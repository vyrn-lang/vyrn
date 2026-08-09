//! Where the emitted IR meets the toolchain that turns it into a binary.
//!
//! The C shim is the portable half of what this crate emits — `stdout`/`stderr`
//! are macros with no linkable symbol, so the IR calls shim functions instead —
//! and clang plus the wasi sysroot are what that IR is fed to. All four pieces
//! sit here rather than in the driver because RFC-0076's wasm generation engine
//! is an EXCLUDED crate the driver may only depend on optionally, so it cannot
//! reach back into the driver for them, and the driver cannot reach into it.
//! This crate is the nearest place both already depend on.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The portable half of the runtime: `stderr`/`stdout` are C macros with no
/// linkable symbol, so the emitted IR calls these two functions instead. The
/// shim is compiled by clang next to the IR on every target — MSVC, glibc,
/// and wasi-libc alike.
pub const RUNTIME_SHIM: &str = r#"
/* MSVC's UCRT deprecates fopen in favor of fopen_s; the portable spelling is
   intentional (glibc and wasi-libc have no fopen_s), so silence the advisory. */
#define _CRT_SECURE_NO_WARNINGS
/* rand_s (a UCRT CSPRNG) needs this defined before <stdlib.h> on MSVC/UCRT; it
   is the native Windows seed source (RFC-0043). Harmless elsewhere. */
#define _CRT_RAND_S
#include <errno.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#if !defined(_WIN32)
/* getentropy: the POSIX/wasi seed CSPRNG (glibc >= 2.25 and wasi-libc). */
#include <unistd.h>
#include <sys/random.h>
#endif
#if defined(_WIN32)
/* _commit / _fileno for fsync (RFC-0044). MoveFileExA gives the atomic overwrite
   the C `rename` refuses on Windows (it fails when the target exists); declared
   here (not via the heavy <windows.h>, which would leak min/max macros into the
   codec below) and satisfied from kernel32. */
#include <io.h>
__declspec(dllimport) int __stdcall MoveFileExA(const char*, const char*, unsigned long);
__declspec(dllimport) unsigned long __stdcall GetLastError(void);
#pragma comment(lib, "kernel32")
#define VYRN_MOVEFILE_REPLACE_EXISTING 0x1u
#define VYRN_ERROR_NOT_SAME_DEVICE 17u
#endif

void* __vyrn_stderr(void) { return stderr; }
void* __vyrn_stdout(void) { return stdout; }

/* size_t-clean wrappers: the IR always passes/returns 64-bit sizes, so these
   adapt on ILP32 targets (wasm32) and are transparent on LP64/LLP64. */
unsigned long long __vyrn_strlen(const char* s) { return (unsigned long long)strlen(s); }

/* lineAt/colAt (1-based) over a byte buffer. The interpreter memoizes a
   line-start table per buffer because a scanner asks once per node and counting
   from byte 0 each time is quadratic; natively there is no such cache, so these
   count directly. Same answer either way — which is all parity requires — and a
   native program calling them in a loop pays what the naive loop would have. */
long long __vyrn_line_at(const unsigned char* d, long long len, long long off) {
    long long line = 1;
    if (off > len) off = len;
    for (long long i = 0; i < off; i++) {
        if (d[i] == 10) line++;
    }
    return line;
}

long long __vyrn_col_at(const unsigned char* d, long long len, long long off) {
    long long col = 1;
    if (off > len) off = len;
    for (long long i = off; i > 0 && d[i - 1] != 10; i--) {
        col++;
    }
    return col;
}

/* (`__vyrn_charcount` was here. RFC-0078's census found `charCount` the one
   builtin with no justification for being one — no primitive, no trap, no
   consteval fold, one caller — and it is `std/text`'s `charCountV` now, the same
   non-continuation-byte scan written in Vyrn. This shim went 47 exports to 46.) */

/* Allocation failure is a trap, not a null dereference: the emitted IR never
   null-checks (every alloc site would need a branch), so the single choke
   point checks instead. The size guard matters on ILP32 (wasm32): without it
   a 64-bit request silently truncates in the (size_t) cast, and a huge size
   could wrap to a tiny allocation - a buffer overflow, not an error. */
static void* __vyrn_alloc_check(void* p, unsigned long long n) {
    if (p == NULL && n > 0) {
        fputs("error: out of memory\n", stderr);
        exit(1);
    }
    return p;
}
void* __vyrn_malloc(unsigned long long n) {
    if (n > (unsigned long long)(size_t)-1) {
        fputs("error: out of memory\n", stderr);
        exit(1);
    }
    return __vyrn_alloc_check(malloc((size_t)n), n);
}
void* __vyrn_realloc(void* p, unsigned long long n) {
    if (n > (unsigned long long)(size_t)-1) {
        fputs("error: out of memory\n", stderr);
        exit(1);
    }
    return __vyrn_alloc_check(realloc(p, (size_t)n), n);
}
/* ---- String header (RFC-0089 M1a) --------------------------------------- */
/* A Vyrn String is still a NUL-terminated `char*`, so every C sink here keeps
   working. What is new is the sixteen bytes in FRONT of it: { long long len,
   long long cap }. `cap == 0` means static: never realloc'd, never freed. The
   IR carries the public accessors (`@__vyrn_str_new` and friends); these three
   are private to the shim so the two definitions cannot collide at link. */
#define VSTR_HDR 16
static char* vstr_new(unsigned long long len, unsigned long long cap) {
    char* base = (char*)__vyrn_malloc(cap + VSTR_HDR + 1);
    ((long long*)base)[0] = (long long)len;
    ((long long*)base)[1] = (long long)cap;
    base[VSTR_HDR + len] = 0;
    return base + VSTR_HDR;
}
static char* vstr_grow(char* s, unsigned long long cap) {
    char* base = (char*)__vyrn_realloc(s - VSTR_HDR, cap + VSTR_HDR + 1);
    ((long long*)base)[1] = (long long)cap;
    return base + VSTR_HDR;
}
static void vstr_setlen(char* s, unsigned long long n) {
    ((long long*)(s - VSTR_HDR))[0] = (long long)n;
    s[n] = 0;
}

/* (`__vyrn_strncmp` lived here for `startsWith`/`endsWith`, which are `std/strpred`
   since RFC-0078 M4c. Nothing else called it, so it is the first shim function a
   milestone of this RFC has retired since M2b.) */

/* ---- Map<String, V> runtime (RFC-0028) ---------------------------------- */
/* A Map lowers to { char** keys, char* vals, i64 len, i64 cap } — two parallel
   growable buffers sharing one length/capacity, in first-insertion order. The
   value buffer is raw bytes with a per-entry stride `esz` (the value type's
   size, passed by the caller). Keys are stored by pointer (no copy — matching
   the array element-store convention). Lookup is a linear strcmp scan. */
typedef struct { char** keys; char* vals; long long len, cap; } VMap;
/* Index of `key`, or -1. Operates on a raw keys buffer so read paths (`at`,
   `has`) can call it with values extracted from an SSA aggregate. */
long long __vyrn_map_find(char** keys, long long len, const char* key) {
    long long i;
    for (i = 0; i < len; i++) if (strcmp(keys[i], key) == 0) return i;
    return -1;
}
/* Ensure room for one more entry, growing both buffers (cap 0 -> 4, else 2x). */
void __vyrn_map_reserve(VMap* m, long long esz) {
    if (m->len + 1 > m->cap) {
        m->cap = m->cap ? m->cap * 2 : 4;
        m->keys = (char**)__vyrn_realloc(m->keys, (unsigned long long)m->cap * sizeof(char*));
        m->vals = (char*)__vyrn_realloc(m->vals, (unsigned long long)m->cap * (unsigned long long)esz);
    }
}
/* Remove entry `i`, shifting later entries down so first-insertion order is
   preserved for the survivors (remove-then-insert therefore moves a key end). */
void __vyrn_map_remove_at(VMap* m, long long i, long long esz) {
    long long rest = m->len - i - 1;
    if (rest > 0) {
        memmove(m->keys + i, m->keys + i + 1, (size_t)(rest * (long long)sizeof(char*)));
        memmove(m->vals + i * esz, m->vals + (i + 1) * esz, (size_t)(rest * esz));
    }
    m->len--;
}
/* A snapshot copy of the key pointers (for `keys()`), owned by the fresh
   Array<String>; the map may then be mutated without disturbing the snapshot. */
char** __vyrn_map_keys_copy(char** keys, long long len) {
    char** r = (char**)__vyrn_malloc((unsigned long long)(len ? len : 1) * sizeof(char*));
    long long i;
    for (i = 0; i < len; i++) r[i] = keys[i];
    return r;
}
int __vyrn_snprintf(char* buf, unsigned long long n, const char* fmt, ...) {
    va_list ap;
    int r;
    va_start(ap, fmt);
    r = vsnprintf(buf, (size_t)n, fmt, ap);
    va_end(ap);
    return r;
}

/* ---- input I/O (RFC-0014) ----------------------------------------------- */
/* argv is stashed by `main` and served to `args()` as argv[1..]. wasi-libc
   populates argv identically on the wasm target (the host provides args_get). */
static int __vyrn_argc = 0;
static char** __vyrn_argv = 0;
long long __vyrn_args_count(void) {
    return (long long)(__vyrn_argc > 1 ? __vyrn_argc - 1 : 0);
}
const char* __vyrn_args_get(long long i) { return __vyrn_argv[i + 1]; }

/* readLine: one line from stdin as a malloc'd, NUL-terminated buffer with its
   trailing \r?\n stripped; *outlen is its byte length. Returns NULL at EOF (no
   bytes) and also for a line containing an embedded NUL byte, which cannot live
   in a NUL-terminated Vyrn String (the parity-safe rule, RFC-0014). The codegen
   validates UTF-8 (via the shared DFA); an invalid line reads as None too. */
char* __vyrn_read_line(unsigned long long* outlen) {
    int c = getchar();
    if (c == EOF) return 0;
    unsigned long long cap = 64, len = 0;
    char* buf = vstr_new(0, cap);
    int had_nul = 0;
    while (c != EOF && c != '\n') {
        if (c == 0) had_nul = 1;
        if (len + 2 >= cap) { cap *= 2; buf = vstr_grow(buf, cap); }
        buf[len++] = (char)c;
        c = getchar();
    }
    if (len > 0 && buf[len - 1] == '\r') len--;
    vstr_setlen(buf, len);
    if (had_nul) { free(buf - VSTR_HDR); return 0; }
    *outlen = len;
    return buf;
}

/* readFile: whole file into a malloc'd, NUL-terminated buffer (*out, *outlen).
   Status: 0 ok, 1 io-error (missing/permission/directory/read error), 3 the
   file contains an embedded NUL byte. UTF-8 validation (status 2) is done by
   the codegen after this returns, reusing the shared DFA. A read loop (not
   fseek/ftell) keeps it portable across regular files, pipes, and wasi-libc. */
int __vyrn_read_file(const char* path, char** out, unsigned long long* outlen) {
    FILE* f = fopen(path, "rb");
    if (f == 0) return 1;
    unsigned long long cap = 1024, len = 0;
    char* buf = vstr_new(0, cap);
    for (;;) {
        if (len + 1 >= cap) { cap *= 2; buf = vstr_grow(buf, cap); }
        size_t got = fread(buf + len, 1, (size_t)(cap - len - 1), f);
        len += (unsigned long long)got;
        if (got == 0) break;
    }
    int bad = ferror(f);
    fclose(f);
    if (bad) { free(buf - VSTR_HDR); return 1; }
    vstr_setlen(buf, len);
    for (unsigned long long k = 0; k < len; k++) {
        if (buf[k] == 0) { free(buf - VSTR_HDR); return 3; }
    }
    *out = buf;
    *outlen = len;
    return 0;
}

/* readFileBytes (M2): binary read, no UTF-8/NUL checks. Status 0 ok / 1 io. */
int __vyrn_read_file_bytes(const char* path, char** out, unsigned long long* outlen) {
    FILE* f = fopen(path, "rb");
    if (f == 0) return 1;
    unsigned long long cap = 1024, len = 0;
    char* buf = (char*)__vyrn_malloc(cap);
    for (;;) {
        if (len + 1 >= cap) { cap *= 2; buf = (char*)__vyrn_realloc(buf, cap); }
        size_t got = fread(buf + len, 1, (size_t)(cap - len), f);
        len += (unsigned long long)got;
        if (got == 0) break;
    }
    int bad = ferror(f);
    fclose(f);
    if (bad) { free(buf); return 1; }
    *out = buf;
    *outlen = len;
    return 0;
}

/* writeFile: create/truncate + write all bytes. Status 0 ok / 1 io-error. A
   Vyrn String is NUL-terminated and never contains a NUL, so strlen is its
   full length. */
int __vyrn_write_file(const char* path, const char* contents) {
    FILE* f = fopen(path, "wb");
    if (f == 0) return 1;
    size_t n = strlen(contents);
    size_t wrote = fwrite(contents, 1, n, f);
    int bad = (wrote != n);
    if (fclose(f) != 0) bad = 1;
    return bad ? 1 : 0;
}

/* renameFile: atomically move `from` over `to` (RFC-0044). Status 0 ok / 1 io /
   2 cross-device. POSIX/wasi `rename` replaces atomically and reports EXDEV;
   Windows C `rename` refuses an existing target, so MoveFileExA(REPLACE_EXISTING)
   is used and ERROR_NOT_SAME_DEVICE maps to the cross-device status. */
int __vyrn_rename_file(const char* from, const char* to) {
#if defined(_WIN32)
    if (MoveFileExA(from, to, VYRN_MOVEFILE_REPLACE_EXISTING) != 0) return 0;
    return GetLastError() == VYRN_ERROR_NOT_SAME_DEVICE ? 2 : 1;
#else
    if (rename(from, to) == 0) return 0;
    return errno == EXDEV ? 2 : 1;
#endif
}

/* fsyncFile: flush a file's data to stable storage (RFC-0044, the optional
   power-durability step). Open, sync the descriptor, close. Status 0 ok / 1 io.
   wasi-libc lowers fsync to fd_sync. */
int __vyrn_fsync_file(const char* path) {
    /* read+write (not "rb"): flushing buffers needs write access on Windows
       (_commit → FlushFileBuffers); "rb+" opens an existing file without
       truncating it. */
    FILE* f = fopen(path, "rb+");
    if (f == 0) return 1;
    int rc = 0;
#if defined(_WIN32)
    if (_commit(_fileno(f)) != 0) rc = 1;
#else
    if (fsync(fileno(f)) != 0) rc = 1;
#endif
    fclose(f);
    return rc;
}

/* ---- time & randomness at the host boundary (RFC-0043) ------------------ */
/* now()/monotonic()/randomSeed() are host INPUTS, not part of the deterministic
   core. Each honors an injected value (VYRN_FIXED_TIME / VYRN_FIXED_SEED) so the
   parity harness can fix the clock and seed identically in every backend; the
   interpreter reads the same env. Absent the env vars they read the real host.
   These symbols are compiled on EVERY target (native + wasi), so a clock/random
   program links and runs under wasmtime with no `vyrn` host page: timespec_get /
   clock_gettime / getentropy lower to WASI clock_time_get / random_get. */

/* Wall clock, epoch milliseconds (UTC). timespec_get(TIME_UTC) is the portable
   spelling across UCRT, glibc, and wasi-libc. */
long long __vyrn_now_millis(void) {
    const char* e = getenv("VYRN_FIXED_TIME");
    if (e && e[0]) return strtoll(e, 0, 10);
    struct timespec ts;
    if (timespec_get(&ts, TIME_UTC) == 0) return 0;
    return (long long)ts.tv_sec * 1000 + (long long)(ts.tv_nsec / 1000000);
}

/* Monotonic nanoseconds. Under a fixed clock: a fixed base plus a deterministic
   per-call increment, so successive calls are byte-identical across backends
   (the interpreter mirrors this base/step exactly: 1e9 + n*1e6). */
static long long __vyrn_mono_ctr = 0;
long long __vyrn_monotonic_nanos(void) {
    const char* e = getenv("VYRN_FIXED_TIME");
    if (e && e[0]) {
        long long v = 1000000000LL + __vyrn_mono_ctr * 1000000LL;
        __vyrn_mono_ctr++;
        return v;
    }
#if defined(_WIN32)
    /* UCRT has no clock_gettime(CLOCK_MONOTONIC); the wall clock in ns is an
       adequate elapsed source (never exercised under the fixed-clock harness). */
    struct timespec ts;
    timespec_get(&ts, TIME_UTC);
    return (long long)ts.tv_sec * 1000000000LL + (long long)ts.tv_nsec;
#else
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000000000LL + (long long)ts.tv_nsec;
#endif
}

/* An unpredictable Int64 seed from the host CSPRNG. */
long long __vyrn_random_seed(void) {
    const char* e = getenv("VYRN_FIXED_SEED");
    if (e && e[0]) return strtoll(e, 0, 10);
#if defined(_WIN32)
    unsigned int a = 0, b = 0;
    rand_s(&a);
    rand_s(&b);
    return (long long)(((unsigned long long)a << 32) ^ (unsigned long long)b);
#else
    unsigned long long v = 0;
    if (getentropy(&v, sizeof v) != 0) v = 0;
    return (long long)v;
#endif
}

/* ---- worker threads (RFC-0025) ------------------------------------------ */
/* `spawn f(args)` lowers to __vyrn_spawn(thunk, frame): the IR packs the
   already-evaluated arguments (behind a leading result slot) into a heap frame
   and passes a per-callee thunk that loads them, calls the isolated task
   function, and stores the result back into the frame. The task is isolated
   (checker-enforced, transitively: no I/O, no module state, no shared cells,
   no `drop`), so ANY schedule produces byte-identical program output — the
   threads below are pure wall-clock optimization. `t.join()` lowers to
   __vyrn_join: block until completion, return the frame (the IR loads the
   result from its leading slot).

   One shared IR, three behaviors, all byte-identical:
     - native: a real OS thread per task (Win32 / pthreads);
     - VYRN_SEQUENTIAL_SPAWN=1 (native): the thunk runs inline at the spawn
       point — the old eager path, a debugging escape hatch;
     - wasm (__wasi__): no threads exist; the thunk always runs inline.

   Locked trap protocol: a trapping task performs the standard trap protocol
   itself (one fputs of the canonical `error: ...` line to stderr, then
   exit(1)) from whichever thread it runs on — same wording, same exit code,
   printed once; exit() flushes stdout so no output is lost. Tasks that were
   never joined are joined at process exit (below, in spawn order): the eager
   semantics ran every task, so a trap in a leaked task must not be lost.

   Ownership: a Task<T> is LINEAR (RFC-0095 M1). It is discharged by exactly
   one of two constructs, and both wait: `t.join()` takes the result, and
   `drop t` releases the result by its type. Each then calls
   __vyrn_task_release, which frees the frame, frees the record and closes the
   event object. That closes RFC-0087 §10 — 81 bytes and one operating-system
   handle per spawn, both linear in the spawn count.

   The rule this replaces was "task records and frames are never freed, because
   a task may be joined more than once". __vyrn_join hands the frame POINTER
   back and the caller loads the result off it, so a free at the first join gave
   the second join a dangling read. What closed it was knowing that no further
   join can happen, which is ownership of the Task value, so a second
   `t.join()` is now a compile error and there is only ever one join.

   Two invariants the release depends on, in the order they are needed:

     - __vyrn_task_release WAITS first. `drop t` reaches it without a join, and
       freeing the frame while the worker still writes the result into it would
       corrupt the heap. The wait is also what keeps the trap protocol: a
       dropped task that traps still prints its line and exits 1.
     - The registry entry goes with the record. The exit walk below reads
       `next`, so a freed record left on the list would be a use-after-free in
       __vyrn_join_all. The list is doubly linked for that, and `listed` makes
       the unlink idempotent — the exit walk detaches what it takes.

   The registry itself is kept, and is empty in every program the checker
   accepts: linearity discharges every task before its owner's scope ends. It
   stays as the net under a hole in that proof, because what it protects is a
   trap that would otherwise be lost. */
#if defined(__wasi__)
typedef struct VTask { void* frame; } VTask;
void* __vyrn_spawn(void (*thunk)(void*), void* frame) {
    VTask* t = (VTask*)__vyrn_malloc(sizeof(VTask));
    t->frame = frame;
    thunk(frame); /* eager: single-threaded target */
    return t;
}
void* __vyrn_join(void* task) { return ((VTask*)task)->frame; }
/* No threads, so the wait is nothing and the release is two frees. */
void __vyrn_task_release(void* task) {
    VTask* t = (VTask*)task;
    free(t->frame);
    free(t);
}
static void __vyrn_join_all(void) {}
#else
#ifdef _WIN32
#include <windows.h>
typedef struct VTask {
    void (*thunk)(void*);
    void* frame;
    HANDLE done; /* manual-reset event, signaled when the task completed */
    struct VTask* prev;
    struct VTask* next;
    int listed;
} VTask;
static DWORD WINAPI __vyrn_task_main(LPVOID p) {
    VTask* t = (VTask*)p;
    t->thunk(t->frame);
    SetEvent(t->done);
    return 0;
}
static SRWLOCK __vyrn_task_lock = SRWLOCK_INIT;
static void __vyrn_tasks_acquire(void) { AcquireSRWLockExclusive(&__vyrn_task_lock); }
static void __vyrn_tasks_release(void) { ReleaseSRWLockExclusive(&__vyrn_task_lock); }
static void __vyrn_task_wait(VTask* t) { WaitForSingleObject(t->done, INFINITE); }
#else
#include <pthread.h>
typedef struct VTask {
    void (*thunk)(void*);
    void* frame;
    pthread_mutex_t mu;
    pthread_cond_t cv;
    int done;
    struct VTask* prev;
    struct VTask* next;
    int listed;
} VTask;
static void* __vyrn_task_main(void* p) {
    VTask* t = (VTask*)p;
    t->thunk(t->frame);
    pthread_mutex_lock(&t->mu);
    t->done = 1;
    pthread_cond_broadcast(&t->cv);
    pthread_mutex_unlock(&t->mu);
    return 0;
}
static pthread_mutex_t __vyrn_task_lock = PTHREAD_MUTEX_INITIALIZER;
static void __vyrn_tasks_acquire(void) { pthread_mutex_lock(&__vyrn_task_lock); }
static void __vyrn_tasks_release(void) { pthread_mutex_unlock(&__vyrn_task_lock); }
static void __vyrn_task_wait(VTask* t) {
    pthread_mutex_lock(&t->mu);
    while (!t->done) pthread_cond_wait(&t->cv, &t->mu);
    pthread_mutex_unlock(&t->mu);
}
#endif
/* Registry of every spawned task that is still outstanding, appended in spawn
   order (a task may itself spawn — the list is edited under the lock, so the
   exit-time walk below observes children its waits allowed to be registered). */
static VTask* __vyrn_task_head = 0;
static VTask* __vyrn_task_tail = 0;

/* Take `t` off the registry. Under the lock, and idempotent: __vyrn_join_all
   detaches what it takes, so a record may reach here already off the list. */
static void __vyrn_task_unlist(VTask* t) {
    __vyrn_tasks_acquire();
    if (t->listed) {
        if (t->prev) t->prev->next = t->next; else __vyrn_task_head = t->next;
        if (t->next) t->next->prev = t->prev; else __vyrn_task_tail = t->prev;
        t->listed = 0;
    }
    __vyrn_tasks_release();
}

void* __vyrn_spawn(void (*thunk)(void*), void* frame) {
    int started = 0;
    VTask* t = (VTask*)__vyrn_malloc(sizeof(VTask));
    t->thunk = thunk;
    t->frame = frame;
    t->prev = 0;
    t->next = 0;
    t->listed = 0;
#ifdef _WIN32
    t->done = CreateEvent(0, TRUE, FALSE, 0);
#else
    pthread_mutex_init(&t->mu, 0);
    pthread_cond_init(&t->cv, 0);
    t->done = 0;
#endif
    {
        const char* seq = getenv("VYRN_SEQUENTIAL_SPAWN");
        if (!(seq && seq[0] == '1' && seq[1] == 0)) {
#ifdef _WIN32
            HANDLE th = CreateThread(0, 0, __vyrn_task_main, t, 0, 0);
            if (th != 0) { CloseHandle(th); started = 1; } /* completion is t->done */
#else
            pthread_t th;
            pthread_attr_t at;
            pthread_attr_init(&at);
            pthread_attr_setdetachstate(&at, PTHREAD_CREATE_DETACHED);
            started = (pthread_create(&th, &at, __vyrn_task_main, t) == 0);
            pthread_attr_destroy(&at);
#endif
        }
    }
    if (!started) {
        /* sequential mode, or thread creation failed: the eager path (run at
           the spawn point, on this thread) — the same bytes, by isolation. */
        __vyrn_task_main(t);
    }
    __vyrn_tasks_acquire();
    t->prev = __vyrn_task_tail;
    if (__vyrn_task_tail) __vyrn_task_tail->next = t; else __vyrn_task_head = t;
    __vyrn_task_tail = t;
    t->listed = 1;
    __vyrn_tasks_release();
    return t;
}

void* __vyrn_join(void* task) {
    VTask* t = (VTask*)task;
    __vyrn_task_wait(t); /* idempotent; safe from any number of joiners */
    return t->frame;
}

/* RFC-0095 M1: give back everything one task owns. The caller has already taken
   or released the RESULT — this cannot, because the shim's ABI does not know
   the result's type — so what is left is the frame, the record and the event.

   The wait comes first and is not optional. `drop t` arrives here without a
   join, and the worker may still be storing the result into the frame. */
void __vyrn_task_release(void* task) {
    VTask* t = (VTask*)task;
    __vyrn_task_wait(t);
    __vyrn_task_unlist(t);
#ifdef _WIN32
    CloseHandle(t->done);
#else
    pthread_cond_destroy(&t->cv);
    pthread_mutex_destroy(&t->mu);
#endif
    free(t->frame);
    free(t);
}

/* Join every task that is still outstanding when the program returns from
   `main` — under eager semantics every spawned task ran, so a leaked task's
   work (and, if it traps, its canonical trap + exit(1)) must still happen.

   Since RFC-0095 M1 a task is linear, so an accepted program leaves this list
   empty and this walk does nothing. It is kept as the net under a hole in that
   proof, because what it protects is a trap that would otherwise be lost. Each
   record is DETACHED before the wait, so a release running concurrently on
   another thread cannot free the pointer this walk is holding; the detached
   record is then left alone, which is a bounded leak on a path no accepted
   program reaches. */
static void __vyrn_join_all(void) {
    for (;;) {
        VTask* t;
        __vyrn_tasks_acquire();
        t = __vyrn_task_head;
        if (t) {
            __vyrn_task_head = t->next;
            if (t->next) t->next->prev = 0; else __vyrn_task_tail = 0;
            t->prev = 0;
            t->next = 0;
            t->listed = 0;
        }
        __vyrn_tasks_release();
        if (!t) return;
        __vyrn_task_wait(t);
    }
}
#endif

#ifdef VYRN_GEN_SHIM
/* ---- standalone-shim build (RFC-0076 M6) --------------------------------
   Here this file is its own wasm module, which every generated module imports
   instead of embedding a private copy of. There is no `main`: `vyrn_entry`
   lives in the OTHER module, and importing it back would make the two modules
   an instantiation cycle. So the host drives, in order, the three things crt1
   would have driven — capture argv, call the generated module's entry, flush.

   Only reached with --target=wasm32-wasip1, which is why the WASI calls are
   spelled as imports rather than through <wasi/api.h>. */
__attribute__((import_module("wasi_snapshot_preview1"), import_name("args_sizes_get")))
extern int __vyrn_wasi_args_sizes(unsigned*, unsigned*);
__attribute__((import_module("wasi_snapshot_preview1"), import_name("args_get")))
extern int __vyrn_wasi_args(char**, char*);

void __vyrn_gen_init(void) {
    unsigned n = 0, sz = 0;
    char** argv;
    char* buf;
    if (__vyrn_wasi_args_sizes(&n, &sz) != 0) return;
    argv = (char**)__vyrn_malloc((n + 1) * sizeof(char*));
    buf = (char*)__vyrn_malloc(sz ? sz : 1);
    if (__vyrn_wasi_args(argv, buf) != 0) return;
    argv[n] = 0;
    __vyrn_argc = (int)n;
    __vyrn_argv = argv;
}

/* What crt1's `__wasm_call_dtors` did on the way out of `main`: a generator's
   whole output is buffered stdout, so nothing may be left in the FILE. */
void __vyrn_gen_fini(void) { fflush(0); }

/* wasm-ld pulls an archive member only when something references it, and the
   generated module's references are IMPORTS — invisible to the link of this
   module. So the libc entry points the emitted IR can call are named here, or
   they would not be in this module for it to import. The list is exactly what
   `vyrn-codegen` declares beyond `__vyrn_*`, plus the four `mem*` LLVM lowers
   intrinsics to; anything missed is caught by the export check on the other
   side and costs a fallback, not a miscompile. */
static void* const __vyrn_gen_libc[] = {
    (void*)printf, (void*)fprintf, (void*)fputs, (void*)fopen, (void*)fclose,
    (void*)exit,   (void*)free,    (void*)strcpy, (void*)strcat, (void*)strcmp,
    (void*)memcpy, (void*)memmove, (void*)memset,  (void*)memcmp,
};
void* __vyrn_gen_libc_keep(int i) { return __vyrn_gen_libc[i]; }
#else
/* The real C entry point: every target's crt (MSVC, glibc, wasi-libc) knows
   how to call a plain C main; the IR only exports vyrn_entry. argv is stashed
   for `args()` (RFC-0014). Outstanding tasks are joined before the exit code
   is returned (RFC-0025). */
extern int vyrn_entry(void);
int main(int argc, char** argv) {
    __vyrn_argc = argc;
    __vyrn_argv = argv;
    int code = vyrn_entry();
    __vyrn_join_all();
    return code;
}
#endif
"#;

/// The dev-tree wasi sysroot, if one exists: the first `tools/wasi-sysroot-*`
/// directory found walking up from `start` (sorted, so the pick is
/// deterministic when several versions are unpacked side by side).
pub fn tools_wasi_sysroot_from(start: &Path) -> Option<std::path::PathBuf> {
    for dir in start.ancestors() {
        let tools = dir.join("tools");
        if !tools.is_dir() {
            continue;
        }
        let mut hits: Vec<std::path::PathBuf> = std::fs::read_dir(&tools)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("wasi-sysroot"))
            })
            .collect();
        hits.sort();
        if let Some(hit) = hits.into_iter().next() {
            return Some(hit);
        }
    }
    None
}

/// Auto-discovered wasi sysroot for the running exe (see
/// [`tools_wasi_sysroot_from`]); `None` when no `tools/` convention applies.
pub fn discovered_wasi_sysroot() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    tools_wasi_sysroot_from(exe.parent()?)
}

/// A wasmtime executable to run a module with: `$VYRN_WASMTIME`, else the first
/// `tools/wasmtime-*` directory found walking up from `start`.
///
/// The same lookup the parity harness does, in the crate both it and RFC-0077's
/// tests can see — nothing here needs wasmtime to BUILD, only to check its own
/// output, so a machine without it skips loudly rather than failing.
pub fn find_wasmtime_from(start: &Path) -> Option<PathBuf> {
    if let Some(p) = std::env::var("VYRN_WASMTIME")
        .map(PathBuf::from)
        .ok()
        .filter(|p| p.exists())
    {
        return Some(p);
    }
    let exe = if cfg!(windows) {
        "wasmtime.exe"
    } else {
        "wasmtime"
    };
    for dir in start.ancestors() {
        let tools = dir.join("tools");
        if !tools.is_dir() {
            continue;
        }
        let mut hits: Vec<PathBuf> = std::fs::read_dir(&tools)
            .ok()?
            .flatten()
            .map(|e| e.path().join(exe))
            .filter(|p| p.exists())
            .collect();
        hits.sort();
        if let Some(hit) = hits.into_iter().next() {
            return Some(hit);
        }
    }
    None
}

/// `libclang_rt.builtins-wasm32.a` from a `libclang_rt.builtins-wasm32-wasi-*`
/// directory next to the sysroot (the wasi-sdk release-artifact layout),
/// version-agnostic and deterministic (sorted).
pub fn builtins_near_sysroot(sysroot: &Path) -> Option<std::path::PathBuf> {
    let parent = sysroot.parent()?;
    let mut hits: Vec<std::path::PathBuf> = std::fs::read_dir(parent)
        .ok()?
        .flatten()
        .map(|e| e.path().join("libclang_rt.builtins-wasm32.a"))
        .filter(|p| {
            p.exists()
                && p.parent()
                    .and_then(|d| d.file_name())
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("libclang_rt.builtins-wasm32"))
        })
        .collect();
    hits.sort();
    hits.into_iter().next()
}

/// Locate a clang executable: `$CLANG`, then PATH, then the default Windows
/// install location.
pub fn find_clang() -> Option<PathBuf> {
    if let Ok(c) = std::env::var("CLANG") {
        let p = PathBuf::from(c);
        if p.exists() {
            return Some(p);
        }
    }
    // Trust PATH: if `clang --version` runs, use the bare name.
    if Command::new("clang")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return Some(PathBuf::from("clang"));
    }
    if cfg!(windows) {
        let default = PathBuf::from(r"C:\Program Files\LLVM\bin\clang.exe");
        if default.exists() {
            return Some(default);
        }
    }
    None
}

/// The runtime shim compiled to a wasm module of its OWN, for a build that links
/// two modules instead of embedding one (RFC-0076 M6, RFC-0077 M2i).
///
/// This is the clang half of what `vyrn-genwasm::build_shim` used to do alone.
/// It moved here when the direct wasm backend became a second consumer, because
/// the flags are the layout contract — `--global-base`/`-z stack-size` at
/// [`crate::wasm::SHIM_BASE`] is what keeps the shim's downward-growing frames
/// away from the generated module's statics — and two copies of a memory map are
/// a memory map that can disagree with itself. `vyrn-genwasm` adds the cranelift
/// half on top and keeps its own artifact cache for that.
///
/// Cached on disk keyed by the shim source and the base address, because it is
/// byte-identical for every consumer and costs ~600 ms to produce. `None` is
/// always "no split build available", never an error: a missing toolchain, or a
/// shim that will not compile, means the caller falls back to the shape it had.
/// The `gen_host` variant is gone with RFC-0076 M7: the generation engine emits
/// its own module through the direct backend, so nothing links this shim against
/// the `vyrn_gen` namespace and nothing defines `-DVYRN_GEN_HOST`.
pub fn shim_wasm() -> Option<PathBuf> {
    // Keyed on what was compiled and where it was told to put it. Not on the
    // compiler build: unlike a cranelift artifact these are plain wasm bytes,
    // and the shim source hash is what decides them.
    let key = format!(
        "shim-{}-{}.wasm",
        vyrn_frontend::hash::sha256_hex(RUNTIME_SHIM.as_bytes()),
        crate::wasm::SHIM_BASE,
    );
    let dir = shim_cache_dir();
    let out = dir.join(&key);
    if out.exists() {
        return Some(out);
    }

    let clang = find_clang()?;
    let sysroot = match std::env::var("WASI_SYSROOT") {
        Ok(s) if Path::new(&s).exists() => PathBuf::from(s),
        _ => discovered_wasi_sysroot()?,
    };
    let builtins = match std::env::var("WASI_BUILTINS") {
        Ok(b) if Path::new(&b).exists() => PathBuf::from(b),
        _ => builtins_near_sysroot(&sysroot)?,
    };
    std::fs::create_dir_all(&dir).ok()?;
    let src = dir.join(format!("{key}.c"));
    std::fs::write(&src, RUNTIME_SHIM).ok()?;
    // Per-process, then renamed: the LSP and a build share this directory, and a
    // reader must never see half a module.
    let tmp = dir.join(format!("{key}.{}.tmp", std::process::id()));

    let st = Command::new(&clang)
        .arg(&src)
        .arg("-o")
        .arg(&tmp)
        // `VYRN_GEN_SHIM` is not about generators here: it is what drops `main`
        // (the other module owns the entry point, and importing it back would
        // make the two an instantiation cycle) and keeps the libc entry points
        // the other module imports from being garbage-collected away.
        .arg("-DVYRN_GEN_SHIM")
        .arg("--target=wasm32-wasip1")
        // No `main`, so no `_start`: the host, or the other module, calls in.
        .arg("-mexec-model=reactor")
        .arg(format!("--sysroot={}", sysroot.display()))
        .arg("-nodefaultlibs")
        .arg(&builtins)
        .arg("-lc")
        // Nothing here is reachable from this module's own entry points, so
        // without this wasm-ld would garbage-collect the whole runtime away.
        .arg("-Wl,--export-all")
        .arg("-Wl,--export-memory")
        // Data above the line, stack growing down TO the line.
        .arg(format!("-Wl,--global-base={}", crate::wasm::SHIM_BASE))
        .arg(format!("-Wl,-z,stack-size={}", crate::wasm::SHIM_BASE))
        .output()
        .ok()?;
    if !st.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    if std::fs::rename(&tmp, &out).is_err() {
        let _ = std::fs::remove_file(&tmp);
        // Lost a race with another process writing the same bytes; theirs will do.
        return out.exists().then_some(out);
    }
    Some(out)
}

fn shim_cache_dir() -> PathBuf {
    match std::env::var("VYRN_GEN_CACHE_DIR") {
        Ok(d) => PathBuf::from(d).join("shim"),
        Err(_) => {
            let home = std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .unwrap_or_else(|_| ".".to_string());
            Path::new(&home).join(".vyrn/cache/shim")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dev-tree toolchain discovery: `tools/wasi-sysroot-*` found from any
    /// ancestor of the starting dir, builtins found version-agnostically next
    /// to the sysroot, and both absent on a layout without the convention.
    #[test]
    fn wasi_toolchain_discovery_walks_the_tools_convention() {
        let root = std::env::temp_dir().join(format!("vyrn_tools_probe_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let sysroot = root.join("tools/wasi-sysroot-25.0");
        let builtins_dir = root.join("tools/libclang_rt.builtins-wasm32-wasi-25.0");
        let deep = root.join("compiler/target/release");
        std::fs::create_dir_all(&sysroot).unwrap();
        std::fs::create_dir_all(&builtins_dir).unwrap();
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(builtins_dir.join("libclang_rt.builtins-wasm32.a"), b"x").unwrap();

        let found = tools_wasi_sysroot_from(&deep).expect("sysroot discovered from exe dir");
        assert_eq!(found, sysroot);
        let b = builtins_near_sysroot(&found).expect("builtins discovered next to sysroot");
        assert!(b.ends_with("libclang_rt.builtins-wasm32.a"));

        // No convention → no discovery (never invent a path).
        let bare = root.join("elsewhere/deeper");
        std::fs::create_dir_all(&bare).unwrap();
        let _ = std::fs::remove_dir_all(root.join("tools"));
        assert!(tools_wasi_sysroot_from(&bare).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
