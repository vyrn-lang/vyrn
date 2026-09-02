/* The WASI host of the wasm2c route (RFC-0125 §2.4 and §2.5; PLAN-0125-runtime
   §6 step 3). `vyrn build --route wasm2c` translates the program's wasm to C with
   wasm2c and links it with this file, wasm-rt and a C compiler. This is the
   two-hundred-line host §2.4 names: the fifteen `wasi_snapshot_preview1` imports
   `vyrn_codegen::direct` declares and nothing else.

   Each import does what `vyrn-cli/src/wasmrun.rs` does, byte for byte, because
   `tests/route.rs` compares this binary's stdout, stderr and exit code with the
   wasm engine's on every corpus program. Where a choice has a reason it is
   written beside the code. `VYRN_W2C_HEADER` is the header wasm2c wrote; the
   driver defines it. The module name is `prog`, fixed by the driver too. */
#ifdef _WIN32
#define _CRT_SECURE_NO_WARNINGS
#define _CRT_NONSTDC_NO_DEPRECATE
#endif
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#ifdef _WIN32
#include <windows.h>
#include <bcrypt.h>
#include <io.h>
#define vh_open _open
#define vh_read _read
#define vh_write _write
#define vh_close _close
#define VH_BINARY _O_BINARY
#else
#include <dirent.h>
#include <unistd.h>
#define vh_open open
#define vh_read read
#define vh_write write
#define vh_close close
#define VH_BINARY 0
#endif
#include "wasm-rt.h"
#include "wasm-rt-impl.h"
#include VYRN_W2C_HEADER

/* WASI preview1 errno values and flag bits, by name, from the witx. */
#define E_SUCCESS 0
#define E_ACCES 2
#define E_BADF 8
#define E_EXIST 20
#define E_IO 29
#define E_ISDIR 31
#define E_NOENT 44
#define E_NOTDIR 54
#define E_NOTCAPABLE 76
#define OFLAGS_CREAT 1
#define OFLAGS_DIRECTORY 2
#define OFLAGS_EXCL 4
#define OFLAGS_TRUNC 8
#define RIGHT_FD_READ (1ull << 1)
#define RIGHT_FD_WRITE (1ull << 6)
#define FDFLAGS_APPEND 1
/* The preopened directory: the working directory, as fd 3, named `.`. */
#define PREOPEN_FD 3
#define FILETYPE_UNKNOWN 0
#define FILETYPE_DIRECTORY 3
#define FILETYPE_REGULAR_FILE 4
#define FILETYPE_SYMBOLIC_LINK 7

struct w2c_wasi__snapshot__preview1 {
    int unused;
};

/* One open descriptor above the preopen: a file, or a directory listing read
   once at the open for `fd_readdir`. Numbers are handed out in order and never
   reused, as the wasm engine's host does. */
struct dent {
    char* name;
    uint8_t kind;
};
struct desc {
    int kind; /* 0 closed, 1 file, 2 directory */
    int fd;
    struct dent* ents;
    uint32_t nents;
};
static struct desc* g_desc;
static uint32_t g_ndesc;
static w2c_prog g_inst;
static int g_argc;
static char** g_argv;
static uint64_t g_started;

static uint64_t mono_ns(void) {
#ifdef _WIN32
    LARGE_INTEGER f, c;
    QueryPerformanceFrequency(&f);
    QueryPerformanceCounter(&c);
    return (uint64_t)((double)c.QuadPart * 1e9 / (double)f.QuadPart);
#else
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
#endif
}

/* A guest pointer checked against the memory's size, or NULL when it is out of
   bounds; every import answers EBADF to NULL, the way the wasm engine's host
   answers a slice it cannot take. */
static void* mem_at(uint32_t off, uint32_t len) {
    wasm_rt_memory_t* m = &g_inst.w2c_memory;
    if ((uint64_t)off + len > m->size) return NULL;
    return m->data + off;
}

static int wr32(uint32_t at, uint32_t v) {
    void* p = mem_at(at, 4);
    if (!p) return 0;
    memcpy(p, &v, 4);
    return 1;
}

static int wasi_errno(int e) {
    switch (e) {
        case ENOENT: return E_NOENT;
        case EACCES: return E_ACCES;
        case EEXIST: return E_EXIST;
        case EISDIR: return E_ISDIR;
        case ENOTDIR: return E_NOTDIR;
        default: return E_IO;
    }
}

static struct desc* desc_of(uint32_t fd) {
    if (fd < PREOPEN_FD + 1 || fd - (PREOPEN_FD + 1) >= g_ndesc) return NULL;
    struct desc* d = &g_desc[fd - (PREOPEN_FD + 1)];
    return d->kind ? d : NULL;
}

static uint32_t desc_new(void) {
    g_desc = realloc(g_desc, (g_ndesc + 1) * sizeof *g_desc);
    if (!g_desc) {
        fputs("host: out of memory\n", stderr);
        exit(1);
    }
    memset(&g_desc[g_ndesc], 0, sizeof *g_desc);
    return PREOPEN_FD + 1 + g_ndesc++;
}

/* A guest path stays under the preopen: not absolute, and never more `..` than
   segments above it. That is the capability rule the wasm engine applies to
   `--dir .`. */
static int under_root(const char* p, uint32_t len) {
    if (len && (p[0] == '/' || p[0] == '\\')) return 0;
#ifdef _WIN32
    if (len >= 2 && p[1] == ':') return 0;
#endif
    int depth = 0;
    uint32_t i = 0;
    while (i <= len) {
        uint32_t j = i;
        while (j < len && p[j] != '/' && p[j] != '\\') j++;
        uint32_t n = j - i;
        if (n == 2 && p[i] == '.' && p[i + 1] == '.') {
            if (--depth < 0) return 0;
        } else if (n > 1 || (n == 1 && p[i] != '.')) {
            depth++;
        }
        i = j + 1;
    }
    return 1;
}

/* The path bytes at `at`, NUL-terminated in a fresh buffer, or NULL when they
   are out of bounds. */
static char* path_at(uint32_t at, uint32_t len) {
    const char* raw = mem_at(at, len);
    if (!raw) return NULL;
    char* s = malloc(len + 1);
    if (!s) return NULL;
    memcpy(s, raw, len);
    s[len] = 0;
    return s;
}

static int is_dir(const char* path) {
    struct stat st;
    return stat(path, &st) == 0 && (st.st_mode & S_IFMT) == S_IFDIR;
}

static void push_ent(struct desc* d, const char* name, uint8_t kind) {
    d->ents = realloc(d->ents, (d->nents + 1) * sizeof *d->ents);
    if (!d->ents) exit(1);
    d->ents[d->nents].name = strdup(name);
    d->ents[d->nents].kind = kind;
    d->nents++;
}

/* The listing the wasm engine's host takes at the open: `.` and `..` first,
   then the entries in the operating system's order; the guest sorts. */
static int list_dir(struct desc* d, const char* path) {
    push_ent(d, ".", FILETYPE_DIRECTORY);
    push_ent(d, "..", FILETYPE_DIRECTORY);
#ifdef _WIN32
    char pat[MAX_PATH + 4];
    snprintf(pat, sizeof pat, "%s\\*", path);
    WIN32_FIND_DATAA fd;
    HANDLE h = FindFirstFileA(pat, &fd);
    if (h == INVALID_HANDLE_VALUE) return is_dir(path) ? E_IO : E_NOENT;
    do {
        if (!strcmp(fd.cFileName, ".") || !strcmp(fd.cFileName, "..")) continue;
        uint8_t kind = (fd.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) ? FILETYPE_SYMBOLIC_LINK
                     : (fd.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY)   ? FILETYPE_DIRECTORY
                                                                          : FILETYPE_REGULAR_FILE;
        push_ent(d, fd.cFileName, kind);
    } while (FindNextFileA(h, &fd));
    FindClose(h);
#else
    DIR* dir = opendir(path);
    if (!dir) return wasi_errno(errno);
    struct dirent* e;
    while ((e = readdir(dir))) {
        if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
        uint8_t kind = e->d_type == DT_DIR ? FILETYPE_DIRECTORY
                     : e->d_type == DT_REG ? FILETYPE_REGULAR_FILE
                     : e->d_type == DT_LNK ? FILETYPE_SYMBOLIC_LINK
                                           : FILETYPE_UNKNOWN;
        push_ent(d, e->d_name, kind);
    }
    closedir(dir);
#endif
    return E_SUCCESS;
}

/* Write all of `len` bytes to an operating-system descriptor. */
static int write_all(int fd, const uint8_t* p, size_t len) {
    while (len) {
        int n = vh_write(fd, p, len > 1 << 30 ? 1 << 30 : (unsigned)len);
        if (n <= 0) return 0;
        p += n;
        len -= (size_t)n;
    }
    return 1;
}

uint32_t w2c_wasi__snapshot__preview1_fd_write(struct w2c_wasi__snapshot__preview1* w,
                                               uint32_t fd, uint32_t iovs, uint32_t n,
                                               uint32_t nwritten) {
    (void)w;
    /* The chunks are gathered before anything is written, so an out-of-bounds
       chunk refuses the whole call and writes nothing, as the engine's host. */
    size_t total = 0;
    for (uint32_t i = 0; i < n; i++) {
        uint32_t* iov = mem_at(iovs + i * 8, 8);
        if (!iov || !mem_at(iov[0], iov[1])) return E_BADF;
        total += iov[1];
    }
    int os = fd == 1 ? 1 : fd == 2 ? 2 : -1;
    if (os < 0) {
        struct desc* d = desc_of(fd);
        if (!d || d->kind != 1) return E_BADF;
        os = d->fd;
    }
    for (uint32_t i = 0; i < n; i++) {
        uint32_t* iov = mem_at(iovs + i * 8, 8);
        if (iov[1] && !write_all(os, mem_at(iov[0], iov[1]), iov[1])) return E_IO;
    }
    return wr32(nwritten, (uint32_t)total) ? E_SUCCESS : E_BADF;
}

uint32_t w2c_wasi__snapshot__preview1_fd_read(struct w2c_wasi__snapshot__preview1* w,
                                              uint32_t fd, uint32_t iovs, uint32_t n,
                                              uint32_t nread) {
    (void)w;
    /* One read into the first buffer with room, as a read syscall does; the
       guest loops until it has what it wants. */
    uint32_t at = 0, len = 0;
    for (uint32_t i = 0; i < n; i++) {
        uint32_t* iov = mem_at(iovs + i * 8, 8);
        if (!iov) return E_BADF;
        if (iov[1]) {
            at = iov[0];
            len = iov[1];
            break;
        }
    }
    if (!len) return wr32(nread, 0) ? E_SUCCESS : E_BADF;
    uint8_t* buf = mem_at(at, len);
    if (!buf) return E_BADF;
    int os = 0;
    if (fd != 0) {
        struct desc* d = desc_of(fd);
        if (!d || d->kind != 1) return E_BADF;
        os = d->fd;
    }
    int got = vh_read(os, buf, len > 1 << 30 ? 1 << 30 : len);
    if (got < 0) return wasi_errno(errno);
    return wr32(nread, (uint32_t)got) ? E_SUCCESS : E_BADF;
}

uint32_t w2c_wasi__snapshot__preview1_fd_close(struct w2c_wasi__snapshot__preview1* w,
                                               uint32_t fd) {
    (void)w;
    if (fd <= PREOPEN_FD) return E_SUCCESS;
    struct desc* d = desc_of(fd);
    if (!d) return E_BADF;
    if (d->kind == 1) vh_close(d->fd);
    d->kind = 0;
    return E_SUCCESS;
}

uint32_t w2c_wasi__snapshot__preview1_fd_readdir(struct w2c_wasi__snapshot__preview1* w,
                                                 uint32_t fd, uint32_t buf, uint32_t buf_len,
                                                 uint64_t cookie, uint32_t bufused) {
    (void)w;
    struct desc* d = desc_of(fd);
    if (!d || d->kind != 2) return E_BADF;
    /* Entries from the cookie on, laid end to end: a dirent header (`d_next:
       u64, d_ino: u64, d_namlen: u32, d_type: u8`, three bytes of padding) and
       the name. The last is cut where the buffer ends, which is how the guest
       learns to ask again from that entry's predecessor. */
    uint8_t* out = malloc(buf_len + 24 + 4096);
    if (!out) return E_IO;
    uint32_t used = 0;
    for (uint64_t i = cookie; i < d->nents && used < buf_len; i++) {
        uint64_t next = i + 1, ino = 0;
        uint32_t namlen = (uint32_t)strlen(d->ents[i].name);
        uint8_t* p = realloc(out, used + 24 + namlen);
        if (!p) {
            free(out);
            return E_IO;
        }
        out = p;
        memcpy(out + used, &next, 8);
        memcpy(out + used + 8, &ino, 8);
        memcpy(out + used + 16, &namlen, 4);
        out[used + 20] = d->ents[i].kind;
        memset(out + used + 21, 0, 3);
        memcpy(out + used + 24, d->ents[i].name, namlen);
        used += 24 + namlen;
    }
    if (used > buf_len) used = buf_len;
    void* slot = mem_at(buf, used);
    if (!slot) {
        free(out);
        return E_BADF;
    }
    memcpy(slot, out, used);
    free(out);
    return wr32(bufused, used) ? E_SUCCESS : E_BADF;
}

uint32_t w2c_wasi__snapshot__preview1_fd_sync(struct w2c_wasi__snapshot__preview1* w,
                                              uint32_t fd) {
    (void)w;
    struct desc* d = desc_of(fd);
    if (!d || d->kind != 1) return E_BADF;
#ifdef _WIN32
    return FlushFileBuffers((HANDLE)_get_osfhandle(d->fd)) ? E_SUCCESS : E_IO;
#else
    return fsync(d->fd) == 0 ? E_SUCCESS : wasi_errno(errno);
#endif
}

void w2c_wasi__snapshot__preview1_proc_exit(struct w2c_wasi__snapshot__preview1* w,
                                            uint32_t code) {
    (void)w;
    exit((int)code);
}

uint32_t w2c_wasi__snapshot__preview1_path_open(struct w2c_wasi__snapshot__preview1* w,
                                                uint32_t dirfd, uint32_t dirflags,
                                                uint32_t path, uint32_t path_len,
                                                uint32_t oflags, uint64_t rights,
                                                uint64_t rights_inheriting, uint32_t fdflags,
                                                uint32_t out) {
    (void)w;
    (void)dirflags;
    (void)rights_inheriting;
    if (dirfd != PREOPEN_FD) return E_BADF;
    char* name = path_at(path, path_len);
    if (!name) return E_BADF;
    if (!under_root(name, path_len)) {
        free(name);
        return E_NOTCAPABLE;
    }
    int rc;
    if (oflags & OFLAGS_DIRECTORY) {
        uint32_t fd = desc_new();
        struct desc* d = &g_desc[fd - (PREOPEN_FD + 1)];
        rc = list_dir(d, name);
        if (rc == E_SUCCESS) {
            d->kind = 2;
            rc = wr32(out, fd) ? E_SUCCESS : E_BADF;
        }
        free(name);
        return rc;
    }
    /* A directory opens on some hosts; the guest asked for a file, and the
       engine's host refuses it with EISDIR. */
    if (is_dir(name)) {
        free(name);
        return E_ISDIR;
    }
    int r = (rights & RIGHT_FD_READ) != 0, wr = (rights & RIGHT_FD_WRITE) != 0;
    int flags = (r && wr) ? O_RDWR : wr ? O_WRONLY : O_RDONLY;
    if (fdflags & FDFLAGS_APPEND) flags |= O_APPEND;
    if (oflags & OFLAGS_CREAT) flags |= O_CREAT;
    if (oflags & OFLAGS_EXCL) flags |= O_CREAT | O_EXCL;
    if (oflags & OFLAGS_TRUNC) flags |= O_TRUNC;
    int fd = vh_open(name, flags | VH_BINARY, 0666);
    free(name);
    if (fd < 0) return wasi_errno(errno);
    uint32_t g = desc_new();
    g_desc[g - (PREOPEN_FD + 1)].kind = 1;
    g_desc[g - (PREOPEN_FD + 1)].fd = fd;
    return wr32(out, g) ? E_SUCCESS : E_BADF;
}

uint32_t w2c_wasi__snapshot__preview1_path_rename(struct w2c_wasi__snapshot__preview1* w,
                                                  uint32_t old_fd, uint32_t old,
                                                  uint32_t old_len, uint32_t new_fd,
                                                  uint32_t new_, uint32_t new_len) {
    (void)w;
    if (old_fd != PREOPEN_FD || new_fd != PREOPEN_FD) return E_BADF;
    char* from = path_at(old, old_len);
    char* to = path_at(new_, new_len);
    int rc;
    if (!from || !to) {
        rc = E_NOENT;
    } else if (!under_root(from, old_len) || !under_root(to, new_len)) {
        rc = E_NOTCAPABLE;
    } else {
#ifdef _WIN32
        /* `rename` refuses an existing target on Windows; the engine's host
           replaces it, as POSIX does. */
        rc = MoveFileExA(from, to, MOVEFILE_REPLACE_EXISTING) ? E_SUCCESS : E_IO;
        if (rc != E_SUCCESS) {
            DWORD e = GetLastError();
            rc = e == ERROR_FILE_NOT_FOUND || e == ERROR_PATH_NOT_FOUND ? E_NOENT
               : e == ERROR_ACCESS_DENIED                               ? E_ACCES
                                                                        : E_IO;
        }
#else
        rc = rename(from, to) == 0 ? E_SUCCESS : wasi_errno(errno);
#endif
    }
    free(from);
    free(to);
    return rc;
}

uint32_t w2c_wasi__snapshot__preview1_fd_prestat_get(struct w2c_wasi__snapshot__preview1* w,
                                                     uint32_t fd, uint32_t buf) {
    (void)w;
    if (fd != PREOPEN_FD) return E_BADF;
    /* prestat { tag: u8 = dir, pr_name_len: u32 }: the name is `.`. */
    return wr32(buf, 0) && wr32(buf + 4, 1) ? E_SUCCESS : E_BADF;
}

/* `args_sizes_get` and `environ_sizes_get`, and `args_get` and `environ_get`:
   how many strings and the bytes they take with their terminators; then the
   strings laid end to end at `buf` with a pointer to each at `ptrs`. */
static uint32_t sizes(char** strings, uint32_t count, uint32_t size) {
    uint32_t n = 0, bytes = 0;
    for (; strings[n]; n++) bytes += (uint32_t)strlen(strings[n]) + 1;
    return wr32(count, n) && wr32(size, bytes) ? E_SUCCESS : E_BADF;
}

static uint32_t fill(char** strings, uint32_t ptrs, uint32_t buf) {
    uint32_t at = buf;
    for (uint32_t i = 0; strings[i]; i++) {
        uint32_t len = (uint32_t)strlen(strings[i]) + 1;
        void* slot = mem_at(at, len);
        if (!slot) return E_BADF;
        memcpy(slot, strings[i], len);
        if (!wr32(ptrs + i * 4, at)) return E_BADF;
        at += len;
    }
    return E_SUCCESS;
}

extern char** environ;

uint32_t w2c_wasi__snapshot__preview1_args_sizes_get(struct w2c_wasi__snapshot__preview1* w,
                                                     uint32_t count, uint32_t size) {
    (void)w;
    return sizes(g_argv, count, size);
}
uint32_t w2c_wasi__snapshot__preview1_args_get(struct w2c_wasi__snapshot__preview1* w,
                                               uint32_t ptrs, uint32_t buf) {
    (void)w;
    return fill(g_argv, ptrs, buf);
}
uint32_t w2c_wasi__snapshot__preview1_environ_sizes_get(struct w2c_wasi__snapshot__preview1* w,
                                                        uint32_t count, uint32_t size) {
    (void)w;
    return sizes(environ, count, size);
}
uint32_t w2c_wasi__snapshot__preview1_environ_get(struct w2c_wasi__snapshot__preview1* w,
                                                  uint32_t ptrs, uint32_t buf) {
    (void)w;
    return fill(environ, ptrs, buf);
}

uint32_t w2c_wasi__snapshot__preview1_clock_time_get(struct w2c_wasi__snapshot__preview1* w,
                                                     uint32_t id, uint64_t precision,
                                                     uint32_t out) {
    (void)w;
    (void)precision;
    uint64_t nanos;
    if (id == 0) {
        struct timespec ts;
        timespec_get(&ts, TIME_UTC);
        nanos = (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
    } else {
        /* monotonic, process_cputime, thread_cputime: one steady clock. */
        nanos = mono_ns() - g_started;
    }
    void* p = mem_at(out, 8);
    if (!p) return E_BADF;
    memcpy(p, &nanos, 8);
    return E_SUCCESS;
}

uint32_t w2c_wasi__snapshot__preview1_random_get(struct w2c_wasi__snapshot__preview1* w,
                                                 uint32_t buf, uint32_t len) {
    (void)w;
    uint8_t* p = mem_at(buf, len);
    if (!p) return E_BADF;
#ifdef _WIN32
    return BCryptGenRandom(NULL, p, len, BCRYPT_USE_SYSTEM_PREFERRED_RNG) == 0 ? E_SUCCESS : E_IO;
#else
    int fd = open("/dev/urandom", O_RDONLY);
    if (fd < 0) return E_IO;
    size_t got = 0;
    while (got < len) {
        int n = read(fd, p + got, len - got);
        if (n <= 0) break;
        got += (size_t)n;
    }
    close(fd);
    return got == len ? E_SUCCESS : E_IO;
#endif
}

int main(int argc, char** argv) {
    /* Bytes pass through unchanged: no CRLF, no ^Z, as under the wasm engine. */
#ifdef _WIN32
    _setmode(0, _O_BINARY);
    _setmode(1, _O_BINARY);
    _setmode(2, _O_BINARY);
#endif
    g_argc = argc;
    g_argv = argv;
    g_started = mono_ns();
    static struct w2c_wasi__snapshot__preview1 wasi;
    wasm_rt_init();
    wasm2c_prog_instantiate(&g_inst, &wasi);
    /* `wasm_rt_impl_try` spelled out, without the exceptions runtime it pulls
       in: the same save and setjmp. */
    WASM_RT_SAVE_STACK_DEPTH();
    wasm_rt_trap_t trap = (wasm_rt_trap_t)WASM_RT_SETJMP(g_wasm_rt_jmp_buf);
    if (trap == WASM_RT_TRAP_NONE) {
        w2c_prog_0x5Fstart(&g_inst);
        /* `_start` ends in `proc_exit`; a plain return is exit 0 too. */
        return 0;
    }
    /* A trap the program did not spell: a wasm `unreachable`, an out-of-bounds
       access. Not program output, so the wording is this host's, in the shape
       the engine's host gives (`error: ..`, exit 1). */
    fprintf(stderr, "error: %s\n", wasm_rt_strerror(trap));
    return 1;
}
