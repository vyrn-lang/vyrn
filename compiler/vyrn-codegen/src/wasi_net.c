/* Outbound HTTP for the native host: the requests a module starts through
   `vyrn_wait.fetch`. The driver links this object only into a program whose
   module imports that function; `wasi_host.c` calls it under `VYRN_FETCH`.

   It does what `vyrn-cli/src/fetch.rs` does, byte for byte, because
   `tests/fetch.rs` compares this binary's output with the embedded engine's:
   HTTP/1.1 with one request per connection (`Connection: close`), no redirect
   followed, no compression asked for, and the reply packed as fields (each its
   byte length in decimal, `:`, its bytes) with the headers sorted by name and
   combined. Each request runs on a thread of its own; `vyrn_net_wait` hands
   the replies to the event loop in the order they finished. It knows nothing
   of the program. */
#ifdef _WIN32
#define _CRT_SECURE_NO_WARNINGS
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
typedef SOCKET sock_t;
#define NO_SOCK INVALID_SOCKET
#define sock_close closesocket
#define SHUT_BOTH SD_BOTH
#else
#include <netdb.h>
#include <pthread.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>
typedef int sock_t;
#define NO_SOCK (-1)
#define sock_close close
#define SHUT_BOTH SHUT_RDWR
#endif
/* A write to a peer that left fails instead of raising SIGPIPE. */
#ifdef MSG_NOSIGNAL
#define SEND_FLAGS MSG_NOSIGNAL
#else
#define SEND_FLAGS 0
#endif
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/* A growable byte buffer; `oom` ends the process, as the host's other
   allocations do. */
struct buf {
    uint8_t* p;
    size_t n, cap;
};

static void oom(void) {
    fputs("host: out of memory\n", stderr);
    exit(1);
}

static void put(struct buf* b, const void* p, size_t n) {
    if (b->n + n > b->cap) {
        size_t cap = b->cap ? b->cap : 256;
        while (cap < b->n + n) cap *= 2;
        b->p = realloc(b->p, cap);
        if (!b->p) oom();
        b->cap = cap;
    }
    memcpy(b->p + b->n, p, n);
    b->n += n;
}

static void put_s(struct buf* b, const char* s) {
    put(b, s, strlen(s));
}

/* One field of the host boundary's encoding. */
static void field(struct buf* b, const void* p, size_t n) {
    char len[24];
    snprintf(len, sizeof len, "%zu:", n);
    put_s(b, len);
    put(b, p, n);
}

/* A slice of the request. */
struct span {
    const uint8_t* p;
    size_t n;
};

/* Splits `p` into its fields, in `*out`, which the caller frees; 0 when it
   is not a list of fields. */
static size_t fields(const uint8_t* p, size_t n, struct span** out) {
    size_t k = 0, i = 0;
    *out = NULL;
    while (i < n) {
        size_t len = 0, j = i;
        while (j < n && p[j] >= '0' && p[j] <= '9') {
            if (len > (SIZE_MAX - 9) / 10) return 0;
            len = len * 10 + (size_t)(p[j++] - '0');
        }
        if (j == i || j == n || p[j] != ':' || len > n - j - 1) return 0;
        *out = realloc(*out, (k + 1) * sizeof **out);
        if (!*out) oom();
        (*out)[k].p = p + j + 1;
        (*out)[k++].n = len;
        i = j + 1 + len;
    }
    return k;
}

/* Whether `p` is UTF-8, as Rust's `str::from_utf8` decides it. */
static int utf8(const uint8_t* p, size_t n) {
    size_t i = 0;
    while (i < n) {
        uint8_t c = p[i];
        size_t len = c < 0x80 ? 1 : (c & 0xE0) == 0xC0 ? 2 : (c & 0xF0) == 0xE0 ? 3 : (c & 0xF8) == 0xF0 ? 4 : 0;
        if (!len || i + len > n || (len == 2 && c < 0xC2) || (len == 4 && c > 0xF4)) return 0;
        for (size_t k = 1; k < len; k++)
            if ((p[i + k] & 0xC0) != 0x80) return 0;
        if (len == 3 && ((c == 0xE0 && p[i + 1] < 0xA0) || (c == 0xED && p[i + 1] > 0x9F))) return 0;
        if (len == 4 && ((c == 0xF0 && p[i + 1] < 0x90) || (c == 0xF4 && p[i + 1] > 0x8F))) return 0;
        i += len;
    }
    return 1;
}

/* The outcomes a reply names, as `std/fetch` reads them. */
#define OK 0
#define BAD_URL 1
#define UNREACHABLE 2
#define BAD_RESPONSE 3
static const char* OUTCOME[] = {"ok", "badUrl", "unreachable", "badResponse"};

/* One request: the event loop's id for it, its bytes, the connection a
   cancellation shuts down, and the reply once it finished. After a
   cancellation the thread owns it and frees it. */
struct req {
    int64_t id;
    uint8_t* bytes;
    size_t len;
    sock_t sock;
    int cancelled;
    uint64_t finished; /* 0 while it runs, then its place in finishing order */
    struct buf reply;
    struct req* next;
};

#ifdef _WIN32
static SRWLOCK g_lock = SRWLOCK_INIT;
static CONDITION_VARIABLE g_done = CONDITION_VARIABLE_INIT;
#define LOCK() AcquireSRWLockExclusive(&g_lock)
#define UNLOCK() ReleaseSRWLockExclusive(&g_lock)
#else
static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t g_done = PTHREAD_COND_INITIALIZER;
#define LOCK() pthread_mutex_lock(&g_lock)
#define UNLOCK() pthread_mutex_unlock(&g_lock)
#endif
/* The requests not yet handed to the event loop, in start order. */
static struct req* g_flying;
static uint64_t g_finished;

/* Buffered reads over a connection. */
struct rd {
    sock_t s;
    uint8_t b[16384];
    size_t at, n;
};

/* The next byte, or -1 at the end of the stream, -2 on an error. */
static int rd_byte(struct rd* r) {
    if (r->at == r->n) {
        int got = recv(r->s, (char*)r->b, (int)sizeof r->b, 0);
        if (got <= 0) return got ? -2 : -1;
        r->at = 0;
        r->n = (size_t)got;
    }
    return r->b[r->at++];
}

/* One line without its CRLF or LF, NUL-terminated in `line`. UNREACHABLE when
   the stream ends first, BAD_RESPONSE when it is not UTF-8. */
static int rd_line(struct rd* r, struct buf* line) {
    line->n = 0;
    for (;;) {
        int c = rd_byte(r);
        if (c < 0) return UNREACHABLE;
        if (c == '\n') break;
        uint8_t ch = (uint8_t)c;
        put(line, &ch, 1);
    }
    if (line->n && line->p[line->n - 1] == '\r') line->n--;
    uint8_t nul = 0;
    put(line, &nul, 1);
    line->n--;
    return utf8(line->p, line->n) ? OK : BAD_RESPONSE;
}

/* `n` bytes into `out`; UNREACHABLE when the stream ends first. */
static int rd_exact(struct rd* r, struct buf* out, size_t n) {
    while (n) {
        if (r->at == r->n) {
            int got = recv(r->s, (char*)r->b, (int)sizeof r->b, 0);
            if (got <= 0) return UNREACHABLE;
            r->at = 0;
            r->n = (size_t)got;
        }
        size_t take = r->n - r->at < n ? r->n - r->at : n;
        put(out, r->b + r->at, take);
        r->at += take;
        n -= take;
    }
    return OK;
}

/* A decimal or hexadecimal number of digits only, or -1. */
static int64_t number(const char* s, size_t n, int base) {
    if (!n) return -1;
    int64_t v = 0;
    for (size_t i = 0; i < n; i++) {
        int c = s[i], d = c >= '0' && c <= '9' ? c - '0'
                        : base == 16 && c >= 'a' && c <= 'f' ? c - 'a' + 10
                        : base == 16 && c >= 'A' && c <= 'F' ? c - 'A' + 10
                                                              : -1;
        if (d < 0 || v > (INT64_MAX - d) / base) return -1;
        v = v * base + d;
    }
    return v;
}

/* Whether `a` is `b` in ASCII, case aside. */
static int same_ci(const char* a, const char* b) {
    for (; *a && *b; a++, b++) {
        char x = *a >= 'A' && *a <= 'Z' ? *a + 32 : *a, y = *b >= 'A' && *b <= 'Z' ? *b + 32 : *b;
        if (x != y) return 0;
    }
    return *a == *b;
}

/* A header, its name lower-case. */
struct hdr {
    char* name;
    char* value;
};

static char* dup_n(const char* s, size_t n) {
    char* d = malloc(n + 1);
    if (!d) oom();
    memcpy(d, s, n);
    d[n] = 0;
    return d;
}

static int is_ws(char c) {
    return c == ' ' || c == '\t';
}

/* One response, packed into `out` as a reply: `ok`, the status, the header
   count, each header's name and value, the body. Interim 1xx responses are
   skipped; `head` says the request was HEAD. */
static int read_response(struct rd* r, int head, struct buf* out) {
    struct buf line = {0}, body = {0};
    struct hdr* hs = NULL;
    size_t nh = 0;
    int status = 0, rc = OK;
    for (;;) {
        for (size_t i = 0; i < nh; i++) free(hs[i].name), free(hs[i].value);
        nh = 0;
        if ((rc = rd_line(r, &line))) goto done;
        const char* l = (const char*)line.p;
        const char* sp = strchr(l, ' ');
        if (strncmp(l, "HTTP/1.", 7) || !sp || (int64_t)(status = (int)number(sp + 1, 3, 10)) < 0 ||
            (sp[4] != ' ' && sp[4] != 0)) {
            rc = BAD_RESPONSE;
            goto done;
        }
        for (;;) {
            if ((rc = rd_line(r, &line))) goto done;
            if (!line.n) break;
            const char* h = (const char*)line.p;
            const char* colon = strchr(h, ':');
            if (!colon || colon == h) {
                rc = BAD_RESPONSE;
                goto done;
            }
            for (const char* c = h; c < colon; c++)
                if (is_ws(*c)) {
                    rc = BAD_RESPONSE;
                    goto done;
                }
            const char* v = colon + 1;
            const char* e = h + line.n;
            while (v < e && is_ws(*v)) v++;
            while (e > v && is_ws(e[-1])) e--;
            hs = realloc(hs, (nh + 1) * sizeof *hs);
            if (!hs) oom();
            hs[nh].name = dup_n(h, (size_t)(colon - h));
            for (char* c = hs[nh].name; *c; c++)
                if (*c >= 'A' && *c <= 'Z') *c += 'a' - 'A';
            hs[nh++].value = dup_n(v, (size_t)(e - v));
        }
        if (status < 100 || status >= 200 || status == 101) break;
    }
    const char *te = NULL, *cl = NULL;
    for (size_t i = 0; i < nh; i++) {
        if (!te && !strcmp(hs[i].name, "transfer-encoding")) te = hs[i].value;
        if (!cl && !strcmp(hs[i].name, "content-length")) cl = hs[i].value;
    }
    if (head || status == 204 || status == 304) {
    } else if (te && same_ci(te, "chunked")) {
        for (;;) {
            if ((rc = rd_line(r, &line))) goto done;
            size_t end = 0;
            while (end < line.n && line.p[end] != ';') end++;
            const char* s = (const char*)line.p;
            while (end && is_ws(s[end - 1])) end--;
            while (end && is_ws(*s)) s++, end--;
            int64_t n = number(s, end, 16);
            if (n < 0) {
                rc = BAD_RESPONSE;
                goto done;
            }
            if (n == 0) {
                do {
                    if ((rc = rd_line(r, &line))) goto done;
                } while (line.n);
                break;
            }
            if ((rc = rd_exact(r, &body, (size_t)n))) goto done;
            if ((rc = rd_line(r, &line))) goto done;
            if (line.n) {
                rc = BAD_RESPONSE;
                goto done;
            }
        }
    } else if (cl) {
        int64_t n = number(cl, strlen(cl), 10);
        if (n < 0) {
            rc = BAD_RESPONSE;
            goto done;
        }
        if ((rc = rd_exact(r, &body, (size_t)n))) goto done;
    } else {
        int c;
        while ((c = rd_byte(r)) >= 0) {
            uint8_t ch = (uint8_t)c;
            put(&body, &ch, 1);
        }
        if (c == -2) {
            rc = UNREACHABLE;
            goto done;
        }
    }
    if (!utf8(body.p, body.n)) {
        rc = BAD_RESPONSE;
        goto done;
    }
    /* Sorted by name, stably, then each name's values joined by ", ";
       `set-cookie` is left out. */
    for (size_t i = 1; i < nh; i++)
        for (size_t j = i; j && strcmp(hs[j - 1].name, hs[j].name) > 0; j--) {
            struct hdr t = hs[j];
            hs[j] = hs[j - 1];
            hs[j - 1] = t;
        }
    size_t kept = 0;
    struct buf vals = {0};
    for (size_t i = 0; i < nh; i++)
        if (strcmp(hs[i].name, "set-cookie") && (!i || strcmp(hs[i].name, hs[i - 1].name))) kept++;
    char num[24];
    field(out, "ok", 2);
    snprintf(num, sizeof num, "%d", status);
    field(out, num, strlen(num));
    snprintf(num, sizeof num, "%zu", kept);
    field(out, num, strlen(num));
    for (size_t i = 0; i < nh;) {
        size_t j = i;
        vals.n = 0;
        for (; j < nh && !strcmp(hs[j].name, hs[i].name); j++) {
            if (j > i) put(&vals, ", ", 2);
            put_s(&vals, hs[j].value);
        }
        if (strcmp(hs[i].name, "set-cookie")) {
            field(out, hs[i].name, strlen(hs[i].name));
            field(out, vals.p ? vals.p : (uint8_t*)"", vals.n);
        }
        i = j;
    }
    field(out, body.p ? body.p : (uint8_t*)"", body.n);
    free(vals.p);
done:
    for (size_t i = 0; i < nh; i++) free(hs[i].name), free(hs[i].value);
    free(hs);
    free(line.p);
    free(body.p);
    return rc;
}

/* Where a request goes, as `fetch.rs`'s `parse_url` reads an `http://` URL. */
struct url {
    char host[1024];
    char port[8];
    char authority[1100];
    const char* target;
    size_t target_n;
    int slash;
};

static int parse_url(const char* u, size_t n, struct url* out) {
    if (n < 7 || memcmp(u, "http://", 7)) return BAD_URL;
    const char* rest = u + 7;
    size_t rn = n - 7;
    for (size_t i = 0; i < rn; i++)
        if (rest[i] == '#') rn = i;
    size_t end = 0;
    while (end < rn && rest[end] != '/' && rest[end] != '?') end++;
    const char* auth = rest;
    size_t an = end;
    for (size_t i = 0; i < an; i++)
        if (auth[i] == '@') return BAD_URL;
    size_t colon = an, hn = an;
    for (size_t i = an; i-- > 0;)
        if (auth[i] == ':') {
            colon = i;
            break;
        }
    int bracketed_after = 0;
    for (size_t i = colon; i < an; i++)
        if (auth[i] == ']') bracketed_after = 1;
    long port = 80;
    if (colon < an && !bracketed_after) {
        hn = colon;
        int64_t p = number(auth + colon + 1, an - colon - 1, 10);
        if (p < 0 || p > 65535) return BAD_URL;
        port = (long)p;
    }
    const char* host = auth;
    size_t bn = hn;
    if (bn && host[0] == '[') host++, bn--;
    if (bn && host[bn - 1] == ']') bn--;
    if (!bn || bn + 2 < hn) return BAD_URL;
    if (bn >= sizeof out->host) return UNREACHABLE;
    memcpy(out->host, host, bn);
    out->host[bn] = 0;
    snprintf(out->port, sizeof out->port, "%ld", port);
    if (port == 80)
        snprintf(out->authority, sizeof out->authority, "%.*s", (int)hn, auth);
    else
        snprintf(out->authority, sizeof out->authority, "%.*s:%ld", (int)hn, auth, port);
    out->target = rest + end;
    out->target_n = rn - end;
    out->slash = !(out->target_n && out->target[0] == '/');
    return OK;
}

static int send_on(struct req* q, struct span* f, size_t nf);

/* One request and its response over a connection of its own; the reply goes
   to `q->reply`. */
static int exchange(struct req* q) {
    struct span* f;
    size_t nf = fields(q->bytes, q->len, &f);
    int rc = send_on(q, f, nf);
    free(f);
    return rc;
}

/* [`exchange`] over the request's fields `f`. */
static int send_on(struct req* q, struct span* f, size_t nf) {
    if (nf < 4) return BAD_URL;
    int64_t n = number((const char*)f[2].p, f[2].n, 10);
    if (n < 0 || (size_t)n > (nf - 4) / 2 || nf != 4 + 2 * (size_t)n) return BAD_URL;
    for (size_t i = 0; i + 1 < nf; i++)
        if (!utf8(f[i].p, f[i].n)) return BAD_URL;
    struct url u;
    int rc = parse_url((const char*)f[1].p, f[1].n, &u);
    if (rc) return rc;
    struct addrinfo hints = {0}, *ai = NULL;
    hints.ai_socktype = SOCK_STREAM;
    if (getaddrinfo(u.host, u.port, &hints, &ai) || !ai) return UNREACHABLE;
    sock_t s = NO_SOCK;
    for (struct addrinfo* a = ai; a; a = a->ai_next) {
        s = socket(a->ai_family, a->ai_socktype, a->ai_protocol);
        if (s == NO_SOCK) continue;
        if (!connect(s, a->ai_addr, (int)a->ai_addrlen)) break;
        sock_close(s);
        s = NO_SOCK;
    }
    freeaddrinfo(ai);
    if (s == NO_SOCK) return UNREACHABLE;
    LOCK();
    q->sock = s;
    int cancelled = q->cancelled;
    UNLOCK();
    if (cancelled) return UNREACHABLE;
    struct buf head = {0};
    put(&head, f[0].p, f[0].n);
    put_s(&head, " ");
    if (u.slash) put_s(&head, "/");
    put(&head, u.target, u.target_n);
    put_s(&head, " HTTP/1.1\r\nHost: ");
    put_s(&head, u.authority);
    put_s(&head, "\r\n");
    for (int64_t i = 0; i < n; i++) {
        put(&head, f[3 + 2 * i].p, f[3 + 2 * i].n);
        put_s(&head, ": ");
        put(&head, f[4 + 2 * i].p, f[4 + 2 * i].n);
        put_s(&head, "\r\n");
    }
    struct span body = f[3 + 2 * n];
    if (body.n) {
        char cl[48];
        snprintf(cl, sizeof cl, "Content-Length: %zu\r\n", body.n);
        put_s(&head, cl);
    }
    put_s(&head, "Connection: close\r\n\r\n");
    put(&head, body.p, body.n);
    size_t sent = 0;
    while (sent < head.n) {
        int k = send(s, (const char*)head.p + sent, (int)(head.n - sent), SEND_FLAGS);
        if (k <= 0) break;
        sent += (size_t)k;
    }
    free(head.p);
    if (sent < head.n) return UNREACHABLE;
    struct rd* r = malloc(sizeof *r);
    if (!r) oom();
    r->s = s;
    r->at = r->n = 0;
    rc = read_response(r, f[0].n == 4 && !memcmp(f[0].p, "HEAD", 4), &q->reply);
    free(r);
    return rc;
}

#ifdef _WIN32
static DWORD WINAPI run(LPVOID arg) {
#else
static void* run(void* arg) {
#endif
    struct req* q = arg;
    int rc = exchange(q);
    if (rc) {
        q->reply.n = 0;
        field(&q->reply, OUTCOME[rc], strlen(OUTCOME[rc]));
    }
    LOCK();
    int cancelled = q->cancelled;
    if (q->sock != NO_SOCK) sock_close(q->sock);
    q->sock = NO_SOCK;
    if (!cancelled) {
        q->finished = ++g_finished;
#ifdef _WIN32
        WakeAllConditionVariable(&g_done);
#else
        pthread_cond_broadcast(&g_done);
#endif
    }
    UNLOCK();
    if (cancelled) {
        free(q->bytes);
        free(q->reply.p);
        free(q);
    }
    return 0;
}

/* Sends the packed request `bytes` as operation `id` on a thread of its own. */
void vyrn_net_start(int64_t id, const uint8_t* bytes, size_t len) {
#ifdef _WIN32
    static int started;
    if (!started) {
        WSADATA w;
        WSAStartup(MAKEWORD(2, 2), &w);
        started = 1;
    }
#endif
    struct req* q = calloc(1, sizeof *q);
    if (!q || !(q->bytes = malloc(len ? len : 1))) oom();
    memcpy(q->bytes, bytes, len);
    q->len = len;
    q->id = id;
    q->sock = NO_SOCK;
    LOCK();
    struct req** at = &g_flying;
    while (*at) at = &(*at)->next;
    *at = q;
    UNLOCK();
#ifdef _WIN32
    HANDLE t = CreateThread(NULL, 0, run, q, 0, NULL);
    if (!t) oom();
    CloseHandle(t);
#else
    pthread_t t;
    if (pthread_create(&t, NULL, run, q)) oom();
    pthread_detach(t);
#endif
}

/* Aborts request `id`: its reply is never handed over. Returns whether `id`
   named a request. */
int vyrn_net_cancel(int64_t id) {
    int found = 0;
    LOCK();
    for (struct req** at = &g_flying; *at; at = &(*at)->next) {
        struct req* q = *at;
        if (q->id != id) continue;
        *at = q->next;
        found = 1;
        if (q->finished) {
            free(q->bytes);
            free(q->reply.p);
            free(q);
        } else {
            q->cancelled = 1;
            if (q->sock != NO_SOCK) shutdown(q->sock, SHUT_BOTH);
        }
        break;
    }
    UNLOCK();
    return found;
}

/* The reply that finished first, waited for at most `ms` milliseconds, or
   until one finishes when `ms` is negative. Returns 1 with `*id` and `*reply`,
   which the caller frees; 0 when the time ran out or no request is in
   flight. */
int vyrn_net_wait(int64_t ms, int64_t* id, uint8_t** reply, size_t* len) {
#ifndef _WIN32
    struct timespec until;
    clock_gettime(CLOCK_REALTIME, &until);
    if (ms > 0) {
        until.tv_sec += (time_t)(ms / 1000);
        until.tv_nsec += (long)(ms % 1000) * 1000000L;
        if (until.tv_nsec >= 1000000000L) until.tv_sec++, until.tv_nsec -= 1000000000L;
    }
#else
    ULONGLONG until = GetTickCount64() + (ULONGLONG)(ms > 0 ? ms : 0);
#endif
    LOCK();
    for (;;) {
        if (!g_flying) break;
        struct req** best = NULL;
        for (struct req** at = &g_flying; *at; at = &(*at)->next)
            if ((*at)->finished && (!best || (*at)->finished < (*best)->finished)) best = at;
        if (best) {
            struct req* q = *best;
            *best = q->next;
            UNLOCK();
            *id = q->id;
            *reply = q->reply.p;
            *len = q->reply.n;
            free(q->bytes);
            free(q);
            return 1;
        }
        if (ms == 0) break;
#ifdef _WIN32
        ULONGLONG now = GetTickCount64();
        if (ms > 0 && now >= until) break;
        SleepConditionVariableSRW(&g_done, &g_lock, ms < 0 ? INFINITE : (DWORD)(until - now), 0);
#else
        if (ms < 0)
            pthread_cond_wait(&g_done, &g_lock);
        else if (pthread_cond_timedwait(&g_done, &g_lock, &until))
            break;
#endif
    }
    UNLOCK();
    return 0;
}
