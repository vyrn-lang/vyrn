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
///
/// `$OOM` is a hole rather than a literal: the C source cannot import a Rust
/// constant, so the one trap this file prints was a fourth spelling of
/// [`vyrn_frontend::trap::OUT_OF_MEMORY`] (three of them, in fact — three call
/// sites). [`runtime_shim`] fills it, and its result is what the shim cache
/// hashes, so a reworded trap invalidates the cached object rather than being
/// silently linked out of it.
pub const RUNTIME_SHIM_TEMPLATE: &str = r#"
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
/* RFC-0111: `_setmode`/`_fileno` and the `_O_BINARY` flag, for the binary
   stdout `__vyrn_write_stdout` needs. */
#include <io.h>
#include <fcntl.h>
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
        fputs($OOM, stderr);
        exit(1);
    }
    return p;
}

/* ---- free audit (RFC-0114 SS25, the double-free half) --------------------
   Every free the IR or this shim runs goes through `__vyrn_free`. With
   VYRN_FREE_AUDIT=1 in the environment, the allocator keeps a live-pointer
   table and a free of anything not in it - a double free, or a free of
   memory this program never owned - prints one line and exits 134. The peak
   rows in `memory.rs` see leaks; this sees the class they cannot. Off (the
   default), the cost is one branch per free and one getenv ever.
   The table uses raw malloc/calloc so it never audits itself. */
static int vyrn_audit_state = -1;
typedef struct { void* p; unsigned long long n; } VyrnAuditEnt;
static VyrnAuditEnt* vyrn_audit_tab = 0;
static size_t vyrn_audit_cap = 0, vyrn_audit_len = 0;
#if defined(__wasm__)
static void vyrn_audit_acquire(void) {}
static void vyrn_audit_release(void) {}
#else
static volatile int vyrn_audit_lock = 0;
static void vyrn_audit_acquire(void) { while (__sync_lock_test_and_set(&vyrn_audit_lock, 1)) {} }
static void vyrn_audit_release(void) { __sync_lock_release(&vyrn_audit_lock); }
#endif
static int vyrn_audit_on(void) {
    if (vyrn_audit_state < 0) {
        const char* e = getenv("VYRN_FREE_AUDIT");
        vyrn_audit_state = (e && e[0] && e[0] != '0') ? 1 : 0;
    }
    return vyrn_audit_state;
}
static size_t vyrn_audit_slot(void* p, VyrnAuditEnt* tab, size_t cap) {
    size_t h = (size_t)p;
    h ^= h >> 16;
    h *= (size_t)0x9e3779b1u;
    h ^= h >> 13;
    size_t i = h & (cap - 1);
    while (tab[i].p && tab[i].p != p) i = (i + 1) & (cap - 1);
    return i;
}
/* lock held */
static void vyrn_audit_add(void* p, unsigned long long n) {
    if (vyrn_audit_len * 2 >= vyrn_audit_cap) {
        size_t ncap = vyrn_audit_cap ? vyrn_audit_cap * 2 : 1024;
        VyrnAuditEnt* nt = (VyrnAuditEnt*)calloc(ncap, sizeof(VyrnAuditEnt));
        if (!nt) return; /* the audit degrades; the program continues */
        for (size_t i = 0; i < vyrn_audit_cap; i++)
            if (vyrn_audit_tab[i].p)
                nt[vyrn_audit_slot(vyrn_audit_tab[i].p, nt, ncap)] = vyrn_audit_tab[i];
        free(vyrn_audit_tab);
        vyrn_audit_tab = nt;
        vyrn_audit_cap = ncap;
    }
    size_t i = vyrn_audit_slot(p, vyrn_audit_tab, vyrn_audit_cap);
    if (!vyrn_audit_tab[i].p) {
        vyrn_audit_tab[i].p = p;
        vyrn_audit_tab[i].n = n;
        vyrn_audit_len++;
    }
}
/* lock held; the block's size if the pointer was live, or -1 (as ull) if it
   was not. Removal rehashes the probe cluster after the hole, or a later
   lookup stops early and reports a false double. */
static unsigned long long vyrn_audit_remove(void* p) {
    if (!vyrn_audit_cap) return (unsigned long long)-1;
    size_t i = vyrn_audit_slot(p, vyrn_audit_tab, vyrn_audit_cap);
    if (vyrn_audit_tab[i].p != p) return (unsigned long long)-1;
    unsigned long long n = vyrn_audit_tab[i].n;
    vyrn_audit_tab[i].p = 0;
    vyrn_audit_len--;
    size_t j = (i + 1) & (vyrn_audit_cap - 1);
    while (vyrn_audit_tab[j].p) {
        VyrnAuditEnt q = vyrn_audit_tab[j];
        vyrn_audit_tab[j].p = 0;
        vyrn_audit_len--;
        vyrn_audit_add(q.p, q.n);
        j = (j + 1) & (vyrn_audit_cap - 1);
    }
    return n;
}
static void vyrn_audit_fail(void) {
    fputs("free audit: double or foreign free\n", stderr);
    exit(134);
}
void __vyrn_free(void* p) {
    if (p == NULL) return;
    if (vyrn_audit_on()) {
        vyrn_audit_acquire();
        unsigned long long n = vyrn_audit_remove(p);
        vyrn_audit_release();
        if (n == (unsigned long long)-1) vyrn_audit_fail();
        /* Poison before the block goes back: a dangling read now yields 0xDD
           bytes instead of stale-but-plausible data, so a use-after-free
           becomes a byte diff parity can see rather than a silent maybe. */
        memset(p, 0xDD, (size_t)n);
    }
    free(p);
}
void* __vyrn_malloc(unsigned long long n) {
    if (n > (unsigned long long)(size_t)-1) {
        fputs($OOM, stderr);
        exit(1);
    }
    void* p = __vyrn_alloc_check(malloc((size_t)n), n);
    if (vyrn_audit_on()) {
        vyrn_audit_acquire();
        vyrn_audit_add(p, n);
        vyrn_audit_release();
    }
    return p;
}
void* __vyrn_realloc(void* p, unsigned long long n) {
    if (n > (unsigned long long)(size_t)-1) {
        fputs($OOM, stderr);
        exit(1);
    }
    if (!vyrn_audit_on()) return __vyrn_alloc_check(realloc(p, (size_t)n), n);
    /* a realloc frees its argument, so the audit treats it as free + malloc —
       unpoisoned, because realloc itself keeps the prefix bytes alive */
    vyrn_audit_acquire();
    unsigned long long old = (p == NULL) ? 0 : vyrn_audit_remove(p);
    vyrn_audit_release();
    if (old == (unsigned long long)-1) vyrn_audit_fail();
    void* q = __vyrn_alloc_check(realloc(p, (size_t)n), n);
    vyrn_audit_acquire();
    vyrn_audit_add(q, n);
    vyrn_audit_release();
    return q;
}
/* ---- String header (RFC-0089 M1a) --------------------------------------- */
/* A Vyrn String is still a NUL-terminated `char*`, so every C sink here keeps
   working. What is new is the sixteen bytes in FRONT of it: { long long len,
   long long cap }. A cap of -1 means static: never realloc'd, never freed. The
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
/* A Map lowers to { char** keys, char* vals, i64 len, i64 cap, i64* idx } — two
   parallel growable buffers sharing one length/capacity, in first-insertion
   order, plus a hash INDEX over them. The value buffer is raw bytes with a
   per-entry stride `esz` (the value type's size, passed by the caller). Keys are
   stored by pointer (no copy — matching the array element-store convention).

   The index is what makes a lookup O(1) (RFC-0104's k-nucleotide row): `idx` is
   an open-addressed bucket array of `cap * 2` slots holding an entry's position
   PLUS ONE, so 0 is the empty slot. It indexes the insertion-ordered storage and
   never reorders it — the observable order is still the arrays', which is the
   thing RFC-0028 locked and parity pins.

   Two invariants hold the arithmetic up. `cap` is 0 or a power of two, because
   `reserve` below is the only thing that ever writes it; and `len <= cap` means
   the table is at most half full, so a probe always reaches an empty slot and
   the loops below need no bound of their own. */
typedef struct { char** keys; char* vals; long long len, cap; long long* idx; } VMap;
/* FNV-1a over the key's bytes. The hash is never observable — no two backends
   have to agree on it, only on the insertion order — so this is the cheap one
   rather than a shared one. To the NUL, which is exactly the equality `strcmp`
   below decides by. */
static unsigned long long map_hash(const char* k) {
    unsigned long long h = 14695981039346656037ULL;
    while (*k) { h ^= (unsigned char)*k++; h *= 1099511628211ULL; }
    return h;
}
/* The bucket `key` belongs in: the one that holds it, or the first empty one
   after where it hashes. One probe serves both readers — a lookup asks whether
   the bucket is occupied, an insert writes into it. */
static unsigned long long map_slot(char** keys, long long* idx, long long nb, const char* key) {
    unsigned long long mask = (unsigned long long)nb - 1;
    unsigned long long b = map_hash(key) & mask;
    while (idx[b] && strcmp(keys[idx[b] - 1], key) != 0) b = (b + 1) & mask;
    return b;
}
/* Rebuild the whole index from the entries. Called where positions move: a grow
   (the bucket count changed) and a remove (the survivors shifted down). Both are
   already O(len) for their own reasons, so this adds no order of growth. */
static void map_reindex(VMap* m) {
    long long nb = m->cap * 2, i;
    if (nb <= 0) return;
    memset(m->idx, 0, (size_t)nb * sizeof(long long));
    for (i = 0; i < m->len; i++) m->idx[map_slot(m->keys, m->idx, nb, m->keys[i])] = i + 1;
}
/* Index of the entry whose key equals the RAW BYTES `p[0..blen)`, or -1
   (RFC-0116). The hash is FNV-1a over exactly `blen` bytes, which equals
   `map_hash` of the equal key — a stored key has no interior NUL, so its
   to-the-NUL hash covers the same bytes. A slice that carries a NUL can
   therefore never match, and the miss path's validation is what refuses it. */
long long __vyrn_map_find_bytes(char** keys, long long len, const unsigned char* p, long long blen, long long* idx, long long cap) {
    unsigned long long h = 14695981039346656037ULL, mask, b;
    long long i;
    if (len <= 0 || cap <= 0) return -1;
    for (i = 0; i < blen; i++) {
        h ^= p[i];
        h *= 1099511628211ULL;
    }
    mask = (unsigned long long)(cap * 2) - 1;
    b = h & mask;
    while (idx[b]) {
        const char* k = keys[idx[b] - 1];
        if (memcmp(k, p, (size_t)blen) == 0 && k[blen] == 0) return idx[b] - 1;
        b = (b + 1) & mask;
    }
    return -1;
}
/* Index of `key`, or -1. Operates on raw buffers so read paths (`at`, `has`) can
   call it with values extracted from an SSA aggregate. */
long long __vyrn_map_find(char** keys, long long len, const char* key, long long* idx, long long cap) {
    unsigned long long b;
    /* An empty map has no index yet, and a map with a `cap` has one — the pair
       is `reserve`'s to keep, so a null `idx` under a non-zero `cap` is a bug
       here rather than a case to fall back on. */
    if (len <= 0 || cap <= 0) return -1;
    b = map_slot(keys, idx, cap * 2, key);
    return idx[b] ? idx[b] - 1 : -1;
}
/* Ensure room for one more entry, growing all three buffers (cap 0 -> 4, else
   2x) and rebuilding the index, whose bucket count is a function of `cap`. */
void __vyrn_map_reserve(VMap* m, long long esz) {
    if (m->len + 1 > m->cap) {
        m->cap = m->cap ? m->cap * 2 : 4;
        m->keys = (char**)__vyrn_realloc(m->keys, (unsigned long long)m->cap * sizeof(char*));
        m->vals = (char*)__vyrn_realloc(m->vals, (unsigned long long)m->cap * (unsigned long long)esz);
        m->idx = (long long*)__vyrn_realloc(m->idx, (unsigned long long)m->cap * 2 * sizeof(long long));
        map_reindex(m);
    }
}
/* Record the entry appended at position `i`, whose key is already in `keys[i]`.
   The append itself is codegen's — it stores the key and the value and bumps the
   length — so this is the one line of it the index needs. */
void __vyrn_map_index_add(VMap* m, long long i) {
    m->idx[map_slot(m->keys, m->idx, m->cap * 2, m->keys[i])] = i + 1;
}
/* Remove entry `i`, shifting later entries down so first-insertion order is
   preserved for the survivors (remove-then-insert therefore moves a key end).

   POINTERS ONLY. This shifts bytes and releases nothing: at this ABI the value
   is `esz` anonymous bytes, so there is no type here to release it BY. The
   caller owns that obligation and discharges it before the call — codegen reads
   the key and the value out of their slots and emits the type-correct release
   for each (see `Gen::release_entry`, and `Fn_::map_method` for the wasm
   backend). A caller that only shifts leaks the key String and the value's heap
   once per removal, which is what this comment exists to stop. */
void __vyrn_map_remove_at(VMap* m, long long i, long long esz) {
    long long rest = m->len - i - 1;
    if (rest > 0) {
        memmove(m->keys + i, m->keys + i + 1, (size_t)(rest * (long long)sizeof(char*)));
        memmove(m->vals + i * esz, m->vals + (i + 1) * esz, (size_t)(rest * esz));
    }
    m->len--;
    map_reindex(m);
}
/* A snapshot copy of the key pointers (for `keys()`), owned by the fresh
   Array<String>; the map may then be mutated without disturbing the snapshot. */
char** __vyrn_map_keys_copy(char** keys, long long len) {
    char** r = (char**)__vyrn_malloc((unsigned long long)(len ? len : 1) * sizeof(char*));
    long long i;
    for (i = 0; i < len; i++) r[i] = keys[i];
    return r;
}

/* ---- the Int64-keyed map (RFC-0117 M1) ---------------------------------- */
/* The same shape with the key column holding the values themselves: no dup on
   insert, no free on removal, equality is the bits. Its own struct because the
   key stride is 8 where a pointer may be 4 (wasm32). The hash is SplitMix64's
   finalizer — the same function std/hash's `impl Hashable for Int64` spells —
   and, like the string map's FNV, it is never observable: only insertion order
   is (RFC-0028). */
typedef struct { long long* keys; char* vals; long long len, cap; long long* idx; } VMapI;
static unsigned long long map_hash_i64(long long k) {
    unsigned long long z = (unsigned long long)k;
    z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ULL;
    z = (z ^ (z >> 27)) * 0x94D049BB133111EBULL;
    return z ^ (z >> 31);
}
static unsigned long long map_slot_i64(long long* keys, long long* idx, long long nb, long long key) {
    unsigned long long mask = (unsigned long long)nb - 1;
    unsigned long long b = map_hash_i64(key) & mask;
    while (idx[b] && keys[idx[b] - 1] != key) b = (b + 1) & mask;
    return b;
}
static void map_reindex_i64(VMapI* m) {
    long long nb = m->cap * 2, i;
    if (nb <= 0) return;
    memset(m->idx, 0, (size_t)nb * sizeof(long long));
    for (i = 0; i < m->len; i++) m->idx[map_slot_i64(m->keys, m->idx, nb, m->keys[i])] = i + 1;
}
long long __vyrn_map_find_i64(long long* keys, long long len, long long key, long long* idx, long long cap) { unsigned long long b; if (len <= 0 || cap <= 0) return -1; b = map_slot_i64(keys, idx, cap * 2, key); return idx[b] ? idx[b] - 1 : -1; }
void __vyrn_map_reserve_i64(VMapI* m, long long esz) { if (m->len + 1 > m->cap) { m->cap = m->cap ? m->cap * 2 : 4; m->keys = (long long*)__vyrn_realloc(m->keys, (unsigned long long)m->cap * sizeof(long long)); m->vals = (char*)__vyrn_realloc(m->vals, (unsigned long long)m->cap * (unsigned long long)esz); m->idx = (long long*)__vyrn_realloc(m->idx, (unsigned long long)m->cap * 2 * sizeof(long long)); map_reindex_i64(m); } }
void __vyrn_map_index_add_i64(VMapI* m, long long i) { m->idx[map_slot_i64(m->keys, m->idx, m->cap * 2, m->keys[i])] = i + 1; }
void __vyrn_map_remove_at_i64(VMapI* m, long long i, long long esz) { long long rest = m->len - i - 1; if (rest > 0) { memmove(m->keys + i, m->keys + i + 1, (size_t)(rest * (long long)sizeof(long long))); memmove(m->vals + i * esz, m->vals + (i + 1) * esz, (size_t)(rest * esz)); } m->len--; map_reindex_i64(m); }
long long* __vyrn_map_keys_copy_i64(long long* keys, long long len) { long long* r = (long long*)__vyrn_malloc((unsigned long long)(len ? len : 1) * sizeof(long long)); long long i; for (i = 0; i < len; i++) r[i] = keys[i]; return r; }
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

/* One byte of standard input, or -1 at end. `getchar` is buffered by the C
   library already, but it is still a call per byte and on some libraries a lock
   per byte; reverse complement over a 40 MB sequence makes 40 million of them.
   Reading blocks into our own buffer costs a compare and an index instead, and
   it is the same shape the wasm runtime's `getbyte` uses, so the two engines
   read standard input the same way rather than two ways.

   `read` and not `fread`: asked for 4096 bytes from a PIPE, `fread` blocks
   until it has all 4096 or the writer closes. A program that parks on
   `readLine()` waiting for one line would then never wake — which is exactly
   what `the_spawn_handles_go_back_natively` does, and it caught this. `read`
   hands back whatever has arrived, which is also what the wasm side's `fd_read`
   does.

   `readLine` is the only reader of fd 0 a Vyrn program has -- `readFile` and
   `readFileBytes` both take a path -- so nothing else can hold a position in
   this stream. */
static unsigned char __vyrn_in_buf[4096];
static unsigned long long __vyrn_in_len = 0, __vyrn_in_pos = 0;
static int __vyrn_in_byte(void) {
    if (__vyrn_in_pos >= __vyrn_in_len) {
#if defined(_WIN32)
        int n = _read(0, __vyrn_in_buf, (unsigned)sizeof __vyrn_in_buf);
#else
        long n = (long)read(0, __vyrn_in_buf, sizeof __vyrn_in_buf);
#endif
        if (n <= 0) return -1;
        __vyrn_in_len = (unsigned long long)n;
        __vyrn_in_pos = 0;
    }
    return __vyrn_in_buf[__vyrn_in_pos++];
}

/* readLine: one line from stdin as a malloc'd, NUL-terminated buffer with its
   trailing \r?\n stripped; *outlen is its byte length. Returns NULL at EOF (no
   bytes) and also for a line containing an embedded NUL byte, which cannot live
   in a NUL-terminated Vyrn String (the parity-safe rule, RFC-0014). The codegen
   validates UTF-8 (via the shared DFA); an invalid line reads as None too. */
char* __vyrn_read_line(unsigned long long* outlen) {
    int c = __vyrn_in_byte();
    if (c < 0) return 0;
    unsigned long long cap = 64, len = 0;
    char* buf = vstr_new(0, cap);
    int had_nul = 0;
    while (c >= 0 && c != '\n') {
        if (c == 0) had_nul = 1;
        if (len + 2 >= cap) { cap *= 2; buf = vstr_grow(buf, cap); }
        buf[len++] = (char)c;
        c = __vyrn_in_byte();
    }
    if (len > 0 && buf[len - 1] == '\r') len--;
    vstr_setlen(buf, len);
    if (had_nul) { __vyrn_free(buf - VSTR_HDR); return 0; }
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
    if (bad) { __vyrn_free(buf - VSTR_HDR); return 1; }
    vstr_setlen(buf, len);
    for (unsigned long long k = 0; k < len; k++) {
        if (buf[k] == 0) { __vyrn_free(buf - VSTR_HDR); return 3; }
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
    if (bad) { __vyrn_free(buf); return 1; }
    *out = buf;
    *outlen = len;
    return 0;
}

/* writeFileBytes (RFC-0111): the same write, with the length passed in rather
   than found with strlen -- the buffer may hold NULs, which is the whole point.
   Status 0 ok / 1 io-error. Already "wb", so no newline translation on any
   platform. */
int __vyrn_write_file_bytes(const char* path, const char* data, unsigned long long len) {
    FILE* f = fopen(path, "wb");
    if (f == 0) return 1;
    size_t wrote = fwrite(data, 1, (size_t)len, f);
    int bad = (wrote != (size_t)len);
    if (fclose(f) != 0) bad = 1;
    return bad ? 1 : 0;
}

/* writeStdout (RFC-0111): raw bytes to fd 1, no newline, no formatting.

   THE WINDOWS TRAP. C stdio opens stdout in TEXT mode, where fwrite turns a
   0x0A into 0x0D 0x0A. For `print` that is the platform's own newline and it is
   correct. For a packed pixel row it is corruption that no line-ending
   normalisation can undo, because nothing downstream can tell which 0x0D 0x0A
   was a real pair of pixels. So stdout goes to binary mode for the write and
   back afterwards -- back, because a `print` after a `writeStdout` must still
   get the platform's newline. Every other platform is binary already and the
   guard compiles to nothing. */
void __vyrn_write_stdout(const char* data, unsigned long long len) {
#if defined(_WIN32)
    fflush(stdout);
    int prev = _setmode(_fileno(stdout), _O_BINARY);
#endif
    fwrite(data, 1, (size_t)len, stdout);
    fflush(stdout);
#if defined(_WIN32)
    if (prev != -1) _setmode(_fileno(stdout), prev);
#endif
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
    __vyrn_free(t->frame);
    __vyrn_free(t);
}
static void __vyrn_join_all(void) {}
#else
#ifdef _WIN32
#include <windows.h>
typedef struct VTask {
    void (*thunk)(void*);
    void* frame;
    HANDLE done; /* manual-reset event, signaled when the task completed */
    volatile LONG flag; /* fallback completion flag, used only if `done` is NULL */
    struct VTask* prev;
    struct VTask* next;
    int listed;
} VTask;
static DWORD WINAPI __vyrn_task_main(LPVOID p) {
    VTask* t = (VTask*)p;
    t->thunk(t->frame);
    if (t->done) SetEvent(t->done); else InterlockedExchange(&t->flag, 1);
    return 0;
}
static SRWLOCK __vyrn_task_lock = SRWLOCK_INIT;
static void __vyrn_tasks_acquire(void) { AcquireSRWLockExclusive(&__vyrn_task_lock); }
static void __vyrn_tasks_release(void) { ReleaseSRWLockExclusive(&__vyrn_task_lock); }
static void __vyrn_task_wait(VTask* t) {
    if (t->done) { WaitForSingleObject(t->done, INFINITE); return; }
    /* CreateEvent failed at spawn (handle exhaustion). WaitForSingleObject on
       NULL returns WAIT_FAILED immediately, which reads as "already done" and
       lets release free the frame while the worker is still writing it. Poll
       the flag the worker sets instead — slow, but never wrong. */
    while (!InterlockedCompareExchange(&t->flag, 0, 0)) Sleep(1);
}
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
    t->flag = 0;
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
    if (t->done) CloseHandle(t->done);
#else
    pthread_cond_destroy(&t->cv);
    pthread_mutex_destroy(&t->mu);
#endif
    __vyrn_free(t->frame);
    __vyrn_free(t);
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

/// C trap stubs for a program's `extern` imports (RFC-0012), one per `extern
/// fn`, appended to [`RUNTIME_SHIM`] on the **native** target only. Each defines
/// the import symbol as a function that prints the canonical trap and exits — so
/// a native binary that reaches an `extern` call behaves exactly like the
/// interpreter, rather than failing to link.
///
/// Both halves belong to somebody, and neither is respelled here: the symbol is
/// [`crate::extern_symbol`]'s (this crate emits the call that must resolve to
/// it) and the wording is [`vyrn_frontend::interp::extern_unavailable`]'s (the
/// interpreter raises it, and parity compares the two byte-for-byte). The driver
/// used to write both by hand, third copies of each.
///
/// The declared `(void)` signature is intentional: the stub never returns (it
/// `exit`s), so the caller's argument/return registers are never observed.
/// [`RUNTIME_SHIM_TEMPLATE`] with its one trap wording filled in from
/// [`vyrn_frontend::trap`] — RFC-0101 M5.
pub fn runtime_shim() -> String {
    RUNTIME_SHIM_TEMPLATE.replace(
        "$OOM",
        &format!(
            "{:?}",
            vyrn_frontend::trap::line(vyrn_frontend::trap::OUT_OF_MEMORY)
        ),
    )
}

pub fn extern_trap_stubs(program: &vyrn_frontend::ast::Program) -> String {
    let mut s = String::new();
    for f in program
        .functions
        .iter()
        // RFC-0043 host-boundary externs (time/random) have REAL implementations
        // in RUNTIME_SHIM on every target, so they get no trap stub.
        .filter(|f| f.is_extern && crate::host_boundary_extern(&f.name).is_none())
    {
        // `f.name` is a Vyrn identifier (alphanumeric + `_`), safe to inline
        // into both a C symbol and a C string literal.
        s.push_str(&format!(
            "void {sym}(void) {{ fputs(\"error: {msg}\\n\", stderr); exit(1); }}\n",
            sym = crate::extern_symbol(&f.name),
            msg = vyrn_frontend::interp::extern_unavailable(&f.name),
        ));
    }
    s
}

/// The dev-tree wasi sysroot, if one exists: the first `tools/wasi-sysroot-*`
/// directory found walking up from `start` (sorted, so the pick is
/// deterministic when several versions are unpacked side by side).
pub fn tools_wasi_sysroot_from(start: &Path) -> Option<std::path::PathBuf> {
    for dir in start.ancestors() {
        let tools = dir.join("tools");
        if !tools.is_dir() {
            continue;
        }
        // A `tools/` directory that passes `is_dir()` but cannot be listed (an
        // ACL denial, a race with deletion) is skipped, not fatal: ending the
        // walk here would hide a valid tool installed at a higher ancestor.
        let Ok(entries) = std::fs::read_dir(&tools) else {
            continue;
        };
        let mut hits: Vec<std::path::PathBuf> = entries
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

/// A variable naming a path, honoured only when the path is there — step 1 of
/// the order for every tool.
fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var(var)
        .map(PathBuf::from)
        .ok()
        .filter(|p| p.exists())
}

/// Step 2 of the order for any tool: the unpacked `~/.vyrn/tools/<sha>/` a pin
/// resolves to, or `Ok(None)` when the `vyrn.json` governing `start` pins no
/// such tool. A pin that cannot be resolved is `Err`, never a fall-through.
///
/// The pin is the project's, so it is read the way every other manifest rule is
/// read: by walking up from `start` to the `vyrn.json` that governs it.
fn pinned_tool_dir(start: &Path, tool: &str) -> Result<Option<PathBuf>, String> {
    let Some(m) = vyrn_frontend::manifest::find(start)? else {
        return Ok(None);
    };
    let Some((_, version)) = m.toolchain.iter().find(|(n, _)| n == tool) else {
        return Ok(None);
    };
    let lock = vyrn_frontend::manifest::Lock::in_project(&m.dir)?;
    vyrn_frontend::toolpin::pinned_tool(Some(&m.dir), &lock, tool, version).map(Some)
}

/// A wasmtime executable to run a module with, and WHY that one — the discovery
/// order RFC-0102 M1 defines:
///
///   1. `$VYRN_WASMTIME`, the explicit escape hatch, which reports itself.
///   2. The pin: `toolchain.wasmtime` in `vyrn.json`, resolved through
///      `vyrn.lock` to a hash and through vendor/cache to an unpacked directory.
///      A pinned tool that cannot be resolved is `Err` — never a fall-through to
///      PATH, because the whole value of a pin is that its absence is loud.
///   3. The `tools/` walk, ONLY when the project declares no pin, so a clone of
///      a project that never pinned anything behaves exactly as it did.
///
/// `Ok(None)` is "nothing pinned and nothing found", which stays a SKIP: nothing
/// here needs wasmtime to BUILD, only to check its own output.
pub fn wasmtime_from(start: &Path) -> Result<Option<(PathBuf, &'static str)>, String> {
    if let Some(p) = env_path("VYRN_WASMTIME") {
        return Ok(Some((p, "override: environment")));
    }
    if let Some(dir) = pinned_tool_dir(start, "wasmtime")? {
        let exe = vyrn_frontend::toolpin::tool_binary(&dir, "wasmtime")
            .ok_or_else(|| unpacked_without("wasmtime", &dir, "a wasmtime binary"))?;
        return Ok(Some((exe, "pinned")));
    }
    Ok(discovered_wasmtime_from(start).map(|p| (p, "discovered: tools/")))
}

/// The refusal for a pinned archive that resolved, unpacked, and turned out not
/// to hold what the tool is for. It is an `Err` rather than a fall-through for
/// the same reason every other pin failure is.
fn unpacked_without(tool: &str, dir: &Path, what: &str) -> String {
    format!(
        "the pinned {tool} archive unpacked to {} with no {what} in it",
        dir.display()
    )
}

/// A wasi sysroot directory, and WHY that one — [`wasmtime_from`]'s order, for
/// the tool `--sysroot=` points at (RFC-0102 M2).
///
/// The pinned answer is the directory a consumer actually points clang at, not
/// the `<sha>` above it: `wasi-sysroot-25.0.tar.gz` unpacks to a version-named
/// directory, and `include/` is the marker that finds it either way.
pub fn wasi_sysroot_from(start: &Path) -> Result<Option<(PathBuf, &'static str)>, String> {
    if let Some(p) = env_path("WASI_SYSROOT") {
        return Ok(Some((p, "override: environment")));
    }
    if let Some(dir) = pinned_tool_dir(start, "wasi-sysroot")? {
        let root = vyrn_frontend::toolpin::tool_root(&dir, "include")
            .ok_or_else(|| unpacked_without("wasi-sysroot", &dir, "`include` directory"))?;
        return Ok(Some((root, "pinned")));
    }
    Ok(tools_wasi_sysroot_from(start).map(|p| (p, "discovered: tools/")))
}

/// `libclang_rt.builtins-wasm32.a`, and WHY that one — the same order again.
///
/// Step 3 is the one place this tool differs: with no pin the archive is found
/// *next to* the sysroot, which is the wasi-sdk release layout the `tools/`
/// convention reproduces, so the sysroot already chosen is what the walk starts
/// from.
pub fn wasi_builtins_from(
    start: &Path,
    sysroot: &Path,
) -> Result<Option<(PathBuf, &'static str)>, String> {
    if let Some(p) = env_path("WASI_BUILTINS") {
        // A link line needs the `.a`, but the variable is named for the tool and
        // the tool ships as a directory, so both spellings arrive: CI exports the
        // file, and a developer who exports the unpacked directory used to get
        // `wasm-ld: is a directory` from clang. Same two levels as the pin.
        let lib = p
            .is_dir()
            .then(|| vyrn_frontend::toolpin::tool_file(&p, BUILTINS_A))
            .flatten()
            .unwrap_or(p);
        return Ok(Some((lib, "override: environment")));
    }
    if let Some(dir) = pinned_tool_dir(start, "wasi-builtins")? {
        let lib = vyrn_frontend::toolpin::tool_file(&dir, BUILTINS_A)
            .ok_or_else(|| unpacked_without("wasi-builtins", &dir, BUILTINS_A))?;
        return Ok(Some((lib, "pinned")));
    }
    Ok(builtins_near_sysroot(sysroot).map(|p| (p, "discovered: tools/")))
}

/// [`wasi_sysroot_from`]'s answer for callers that only want the path; panics on
/// an unresolvable pin, for the reason [`find_wasmtime_from`] does.
pub fn find_wasi_sysroot_from(start: &Path) -> Option<PathBuf> {
    match wasi_sysroot_from(start) {
        Ok(found) => found.map(|(p, _)| p),
        Err(e) => panic!("{e}"),
    }
}

/// [`wasi_builtins_from`]'s answer for callers that only want the path.
pub fn find_wasi_builtins_from(start: &Path, sysroot: &Path) -> Option<PathBuf> {
    match wasi_builtins_from(start, sysroot) {
        Ok(found) => found.map(|(p, _)| p),
        Err(e) => panic!("{e}"),
    }
}

/// [`wasmtime_from`]'s answer for the callers that only want the path.
///
/// A pin that cannot be resolved panics rather than reading as "not installed",
/// for the reason [`require_tools`] panics: a run that silently skips the checks
/// a tool exists for is a green run that proves nothing.
pub fn find_wasmtime_from(start: &Path) -> Option<PathBuf> {
    match wasmtime_from(start) {
        Ok(found) => found.map(|(p, _)| p),
        Err(e) => panic!("{e}"),
    }
}

/// The dev-tree wasmtime: the first `tools/wasmtime-*/wasmtime` found walking up
/// from `start` (sorted, so the pick is deterministic when several versions are
/// unpacked side by side). Step 3 of the order — consulted only when nothing is
/// pinned.
fn discovered_wasmtime_from(start: &Path) -> Option<PathBuf> {
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
        // Same rule as the sysroot walk: an unlistable `tools/` is skipped, not
        // fatal — the walk continues at the ancestors above it.
        let Ok(entries) = std::fs::read_dir(&tools) else {
            continue;
        };
        let mut hits: Vec<PathBuf> = entries
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

/// Turn a missing tool from a SKIP into a failure when `VYRN_REQUIRE_TOOLS` is
/// set, and return it unchanged otherwise.
///
/// Every check in this repo that needs an external binary degrades quietly when
/// it is absent — no `wasmtime` and the wasm column disappears with a `NOTE`,
/// and the run still passes with less checked than its name says. That is right
/// on a developer's machine, where the tool is genuinely optional. It is wrong
/// in CI, where the tool is fetched on purpose and a cache that restored an
/// empty directory, a renamed release asset or a typo in an exported path all
/// read as green.
///
/// So the decision is the CALLER's environment, made once: CI exports
/// `VYRN_REQUIRE_TOOLS=1` and a missing tool stops the build, saying which one
/// and which variable points at it. This lives here rather than in a test
/// harness because two harnesses need it — `vyrn-cli/tests/common` and
/// `vyrn-codegen/tests` — and a rule with two copies is a rule with two
/// answers.
pub fn require_tools(what: &str, var: &str, found: Option<PathBuf>) -> Option<PathBuf> {
    if found.is_none() && std::env::var_os("VYRN_REQUIRE_TOOLS").is_some() {
        panic!(
            "VYRN_REQUIRE_TOOLS is set and `{what}` was not found — this run would have \
             silently skipped the checks that need it. Point `{var}` at the binary, or \
             unset VYRN_REQUIRE_TOOLS to allow the skip."
        );
    }
    found
}

/// The one file the builtins archive exists to deliver, named once: the pinned
/// resolver looks for it inside an unpacked blob and the `tools/` walk looks for
/// it beside a sysroot, and two spellings would be two answers.
pub const BUILTINS_A: &str = "libclang_rt.builtins-wasm32.a";

/// `libclang_rt.builtins-wasm32.a` from a `libclang_rt.builtins-wasm32-wasi-*`
/// directory next to the sysroot (the wasi-sdk release-artifact layout),
/// version-agnostic and deterministic (sorted).
pub fn builtins_near_sysroot(sysroot: &Path) -> Option<std::path::PathBuf> {
    let parent = sysroot.parent()?;
    let mut hits: Vec<std::path::PathBuf> = std::fs::read_dir(parent)
        .ok()?
        .flatten()
        .map(|e| e.path().join(BUILTINS_A))
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

/// The Windows last resort, spelled once: the path and the reason that names it
/// are the same string, because a reason that disagrees with the path it
/// explains is worse than no reason at all.
macro_rules! windows_clang {
    () => {
        r"C:\Program Files\LLVM\bin\clang.exe"
    };
}

/// Locate a clang executable: `$CLANG`, then PATH, then the default Windows
/// install location.
pub fn find_clang() -> Option<PathBuf> {
    clang_from().map(|(p, _, _)| p)
}

/// The clang a build will run, the version it reports, and WHY that one —
/// [`wasmtime_from`]'s shape for the one tool RFC-0102 does not pin.
///
/// clang stays discovered because a native clang links against the host's libc,
/// linker and system libraries: there is no portable tarball that produces a
/// working native binary everywhere, and a pin that failed at LINK time instead
/// of at resolve time would be worse than no pin. So it is recorded rather than
/// pinned — the version is captured, it enters [`shim_wasm`]'s cache key, and
/// `vyrn deps` prints all three columns.
///
/// Memoized for the process: `--version` is a spawn, this is called on the shim
/// cache's hit path, and the answer cannot change under a running compiler. It
/// is also strictly cheaper than what it replaces, which spawned the same probe
/// on every call.
pub fn clang_from() -> Option<(PathBuf, String, &'static str)> {
    static FOUND: std::sync::OnceLock<Option<(PathBuf, String, &'static str)>> =
        std::sync::OnceLock::new();
    FOUND.get_or_init(discover_clang).clone()
}

/// The first line of `clang --version`, trimmed, or `unknown` when the probe
/// says nothing. Whatever the vendor prints is the version: Apple, Ubuntu and
/// upstream all word that line differently, and normalizing it here would be
/// this repository inventing a version number for a compiler it did not build.
fn clang_version(exe: &Path) -> String {
    Command::new(exe)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| first_line(&o.stdout))
        .unwrap_or_else(|| UNKNOWN_VERSION.to_string())
}

/// What a version column says when nothing knows the answer. One spelling, used
/// by the probe here and by `vyrn deps` for every tool that reports no version.
pub const UNKNOWN_VERSION: &str = "unknown";

fn first_line(out: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(out);
    let line = text.lines().next()?.trim().to_string();
    (!line.is_empty()).then_some(line)
}

fn discover_clang() -> Option<(PathBuf, String, &'static str)> {
    if let Ok(c) = std::env::var("CLANG") {
        let p = PathBuf::from(c);
        if p.exists() {
            let v = clang_version(&p);
            return Some((p, v, "override: environment"));
        }
    }
    // Trust PATH: if `clang --version` runs, use the bare name. The output was
    // thrown away until RFC-0102 M3; only the pipe is new.
    if let Ok(out) = Command::new("clang").arg("--version").output() {
        if out.status.success() {
            let v = first_line(&out.stdout).unwrap_or_else(|| UNKNOWN_VERSION.to_string());
            return Some((PathBuf::from("clang"), v, "discovered: PATH"));
        }
    }
    if cfg!(windows) {
        let default = PathBuf::from(windows_clang!());
        if default.exists() {
            let v = clang_version(&default);
            return Some((default, v, concat!("discovered: ", windows_clang!())));
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
/// Cached on disk keyed by the shim source, the base address, the compiler and
/// the sysroot/builtins the link reads (see [`shim_key`]), because it is
/// byte-identical for every consumer and costs ~600 ms to produce. `None` is
/// always "no split build available", never an error: a missing toolchain, or a
/// shim that will not compile, means the caller falls back to the shape it had.
/// The `gen_host` variant is gone with RFC-0076 M7: the generation engine emits
/// its own module through the direct backend, so nothing links this shim against
/// the `vyrn_gen` namespace and nothing defines `-DVYRN_GEN_HOST`.
/// The shim's cache-key filename: the shim source, the base address it is told
/// to put its frames at, the compiler that turns the first into a module at the
/// second, and the sysroot and builtins archive that link consumes.
///
/// The last two are there for the same reason the compiler is: RFC-0102 Exhibit
/// 5 says a cache key that omits an input is a cache that serves a stale
/// answer. The compiled shim embeds whatever wasi-libc the sysroot supplied and
/// links the builtins archive, so upgrade the sysroot and a key that omits them
/// still hits — every subsequent split build links a shim the new libc never
/// saw until `~/.vyrn/cache/shim` is cleared by hand.
///
/// Each enters as a hash rather than as itself, because a key is a FILENAME: a
/// clang version line carries spaces, parentheses and — on upstream builds — a
/// URL, and the two paths are long and slash-laden. Sixteen hex digits each,
/// which is what the module cache already spends on a source hash.
fn shim_key(clang_version: &str, sysroot: &Path, builtins: &Path) -> String {
    format!(
        "shim-{}-{}-{}-{}.wasm",
        vyrn_frontend::hash::sha256_hex(runtime_shim().as_bytes()),
        crate::wasm::SHIM_BASE,
        shim_key_clang_component(clang_version),
        shim_key_sysroot_component(sysroot, builtins),
    )
}

/// The component [`shim_key`] gives the resolved sysroot and builtins archive,
/// exposed beside [`shim_key_clang_component`] so a check can say the key
/// CONTAINS them rather than just that it changed.
fn shim_key_sysroot_component(sysroot: &Path, builtins: &Path) -> String {
    let both = format!("{}\u{1}{}", sysroot.display(), builtins.display());
    format!(
        "sysroot{}",
        &vyrn_frontend::hash::sha256_hex(both.as_bytes())[..16]
    )
}

/// The component [`shim_key`] gives a clang version, exposed so a check can say
/// the key CONTAINS the compiler rather than just that it changed.
pub fn shim_key_clang_component(clang_version: &str) -> String {
    format!(
        "clang{}",
        &vyrn_frontend::hash::sha256_hex(clang_version.as_bytes())[..16]
    )
}

pub fn shim_wasm() -> Option<PathBuf> {
    // Every input the key names is read BEFORE the cache is consulted, because
    // a key computed from half its inputs is a cache that serves a stale answer
    // (RFC-0102 M3, Exhibit 5). It costs nothing new: the clang probe is
    // memoized, and a miss needs all of them anyway.
    let (clang, version, _) = clang_from()?;
    let exe = std::env::current_exe().ok()?;
    let start = exe.parent()?;
    let sysroot = find_wasi_sysroot_from(start)?;
    let builtins = find_wasi_builtins_from(start, &sysroot)?;
    let key = shim_key(&version, &sysroot, &builtins);
    let dir = shim_cache_dir();
    let out = dir.join(&key);
    if out.exists() {
        return Some(out);
    }
    std::fs::create_dir_all(&dir).ok()?;
    let src = dir.join(format!("{key}.c"));
    std::fs::write(&src, runtime_shim()).ok()?;
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

    /// The rule two test harnesses depend on, checked rather than assumed: with
    /// `VYRN_REQUIRE_TOOLS` set a missing tool PANICS, and without it the same
    /// call is a quiet `None` the caller may skip on. A `require_tools` that
    /// silently returned `None` under the variable would give every gate that
    /// uses it the failure mode it was written to remove.
    ///
    /// Serial by construction: it is one test, and it puts the variable back.
    #[test]
    fn require_tools_fails_loud_only_when_the_environment_asks() {
        let saved = std::env::var_os("VYRN_REQUIRE_TOOLS");
        std::env::remove_var("VYRN_REQUIRE_TOOLS");
        assert!(require_tools("nothing", "NOTHING", None).is_none());

        std::env::set_var("VYRN_REQUIRE_TOOLS", "1");
        let missing = std::panic::catch_unwind(|| require_tools("nothing", "NOTHING", None));
        assert!(missing.is_err(), "a missing tool must not skip quietly");
        // A tool that IS there is handed back either way.
        let here = PathBuf::from(".");
        assert_eq!(
            require_tools("here", "HERE", Some(here.clone())),
            Some(here)
        );

        match saved {
            Some(v) => std::env::set_var("VYRN_REQUIRE_TOOLS", v),
            None => std::env::remove_var("VYRN_REQUIRE_TOOLS"),
        }
    }

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

    /// RFC-0102 Exhibit 5, applied to the inputs that came after clang: the
    /// compiled shim embeds the sysroot's wasi-libc and links the builtins
    /// archive, so a key that omits them serves a shim the upgraded sysroot
    /// never saw. Repointing either must move the key, exactly as upgrading
    /// clang does.
    #[test]
    fn the_shim_key_moves_when_the_sysroot_or_builtins_move() {
        let v25 = Path::new("/tools/wasi-sysroot-25.0");
        let v26 = Path::new("/tools/wasi-sysroot-26.0");
        let lib = Path::new("/tools/libclang_rt.builtins-wasm32.a");
        let other = Path::new("/tools/other/libclang_rt.builtins-wasm32.a");

        let key = shim_key("clang version 1", v25, lib);
        assert_ne!(
            key,
            shim_key("clang version 1", v26, lib),
            "sysroot moves it"
        );
        assert_ne!(
            key,
            shim_key("clang version 1", v25, other),
            "builtins move it"
        );
        // The components are IN the key, not merely represented by it.
        assert!(key.contains(&shim_key_clang_component("clang version 1")));
        assert!(key.contains(&shim_key_sysroot_component(v25, lib)));
    }

    /// The trap stub is assembled from the two facts' OWNERS — this crate's
    /// symbol scheme and the interpreter's wording — so renaming either moves
    /// the stub with it. The driver used to hand-write both, and parity was the
    /// only thing holding the three spellings together.
    #[test]
    fn the_extern_stub_is_built_from_the_symbol_and_the_trap_it_quotes() {
        let src = "extern fn jsBeep()\nextern fn hostNowMillis() -> Int64\nfn main() -> Int64 { return 0 }\n";
        let toks = vyrn_frontend::lexer::lex(src).expect("lex");
        let prog = vyrn_frontend::parser::parse(toks).expect("parse");
        let stubs = extern_trap_stubs(&prog);

        assert!(
            stubs.contains(&format!("void {}(void)", crate::extern_symbol("jsBeep"))),
            "the stub must define the symbol codegen calls: {stubs}"
        );
        assert!(
            stubs.contains(&vyrn_frontend::interp::extern_unavailable("jsBeep")),
            "the stub must print the interpreter's trap verbatim: {stubs}"
        );
        // RFC-0043 host-boundary externs are implemented by RUNTIME_SHIM on
        // every target, so a stub for one would be a duplicate symbol.
        assert!(
            !stubs.contains("hostNowMillis"),
            "no stub for a host-boundary extern: {stubs}"
        );
    }
}
