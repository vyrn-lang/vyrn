/* k-nucleotide, from the Computer Language Benchmarks Game.
 *
 * A transcription of examples/knucleotide.vyrn. Build with:
 *
 *   clang -O2 -ffp-contract=off -std=c11 -o knucleotide.exe knucleotide.c
 *
 * There is no N: it reads FASTA on stdin, exactly as the Vyrn program does.
 *
 *   knucleotide.exe < rfcs/bench-0104/fasta-1000.expected
 *
 * C has no hash map, so this file carries one: plain open addressing with
 * linear probing, which is what a C book writes. That is the recorded
 * asymmetry with the Vyrn program, which uses `Map<String, Int64>`.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* The longest fragment the game asks for by name is 18 bases. */
#define MAX_K 24

static void oom(void)
{
    fprintf(stderr, "out of memory\n");
    exit(1);
}

/* One line of stdin, without its newline, into a buffer that grows as needed.
 * Returns the length, or -1 at end of input. */
static long readLine(FILE *f, char **buf, size_t *cap)
{
    size_t len = 0;

    for (;;) {
        if (*cap - len < 2) {
            size_t next = *cap * 2;
            char *grown = (char *)realloc(*buf, next);
            if (grown == NULL) {
                oom();
            }
            *buf = grown;
            *cap = next;
        }
        if (fgets(*buf + len, (int)(*cap - len), f) == NULL) {
            if (len == 0) {
                return -1;
            }
            (*buf)[len] = '\0';
            return (long)len;
        }
        len += strlen(*buf + len);
        if (len > 0 && (*buf)[len - 1] == '\n') {
            len--;
            if (len > 0 && (*buf)[len - 1] == '\r') {
                len--;
            }
            (*buf)[len] = '\0';
            return (long)len;
        }
    }
}

/* ---- the hash table ---------------------------------------------------- */

typedef struct {
    char key[MAX_K];
    long long count;
} Slot;

typedef struct {
    Slot *slots;
    size_t cap;
    size_t used;
} Table;

/* ponytail: the table is sized once, to four times the number of windows, so it
 * never rehashes. Add growth if a caller ever inserts more than it was sized
 * for. */
static void tableInit(Table *t, size_t hint)
{
    size_t cap = 16;

    while (cap < hint * 4) {
        cap *= 2;
    }
    t->slots = (Slot *)calloc(cap, sizeof(Slot));
    if (t->slots == NULL) {
        oom();
    }
    t->cap = cap;
    t->used = 0;
}

static void tableFree(Table *t)
{
    free(t->slots);
    t->slots = NULL;
    t->cap = 0;
    t->used = 0;
}

/* FNV-1a over the key bytes. */
static size_t hashKey(const char *key, size_t k)
{
    size_t h = (size_t)1469598103934665603ULL;
    size_t i;

    for (i = 0; i < k; i++) {
        h ^= (unsigned char)key[i];
        h *= (size_t)1099511628211ULL;
    }
    return h;
}

/* Add one to the count for `key`, inserting it if it is new. */
static void tableBump(Table *t, const char *key, size_t k)
{
    size_t mask = t->cap - 1;
    size_t i = hashKey(key, k) & mask;

    for (;;) {
        Slot *s = &t->slots[i];
        if (s->key[0] == '\0') {
            memcpy(s->key, key, k);
            s->key[k] = '\0';
            s->count = 1;
            t->used++;
            return;
        }
        if (memcmp(s->key, key, k) == 0 && s->key[k] == '\0') {
            s->count++;
            return;
        }
        i = (i + 1) & mask;
    }
}

/* The count for `key`, or zero. */
static long long tableGet(const Table *t, const char *key, size_t k)
{
    size_t mask = t->cap - 1;
    size_t i = hashKey(key, k) & mask;

    for (;;) {
        const Slot *s = &t->slots[i];
        if (s->key[0] == '\0') {
            return 0;
        }
        if (memcmp(s->key, key, k) == 0 && s->key[k] == '\0') {
            return s->count;
        }
        i = (i + 1) & mask;
    }
}

/* ---- the benchmark ------------------------------------------------------ */

/* Every window of width `k`, counted. */
static void countKmers(const char *seq, size_t seqLen, size_t k, Table *out)
{
    size_t windows = seqLen >= k ? seqLen - k + 1 : 0;
    size_t i;

    tableInit(out, windows);
    for (i = 0; i + k <= seqLen; i++) {
        tableBump(out, seq + i, k);
    }
}

typedef struct {
    const char *frag;
    long long count;
} Entry;

/* Count descending, ties by fragment ascending. */
static int compareEntries(const void *a, const void *b)
{
    const Entry *x = (const Entry *)a;
    const Entry *y = (const Entry *)b;

    if (x->count != y->count) {
        return x->count > y->count ? -1 : 1;
    }
    return strcmp(x->frag, y->frag);
}

/* One frequency table: every fragment of width `k` as a percentage of the
 * windows there are, then a blank line. */
static void report(const char *seq, size_t seqLen, size_t k)
{
    Table m;
    Entry *es;
    size_t n = 0;
    size_t i;
    long long total;

    countKmers(seq, seqLen, k, &m);
    total = (long long)seqLen - (long long)k + 1;

    es = (Entry *)malloc((m.used > 0 ? m.used : 1) * sizeof(Entry));
    if (es == NULL) {
        oom();
    }
    for (i = 0; i < m.cap; i++) {
        if (m.slots[i].key[0] != '\0') {
            es[n].frag = m.slots[i].key;
            es[n].count = m.slots[i].count;
            n++;
        }
    }
    qsort(es, n, sizeof(Entry), compareEntries);

    for (i = 0; i < n; i++) {
        /* `x` at three decimal places -- the game asks for three. */
        double x = 100.0 * (double)es[i].count / (double)total;
        long long scaled = (long long)(x * 1000.0 + 0.5);
        printf("%s %lld.%03lld\n", es[i].frag, scaled / 1000, scaled % 1000);
    }
    printf("\n");

    free(es);
    tableFree(&m);
}

/* The count of one named fragment. It builds the whole table for that width and
 * looks the fragment up, rather than scanning for the one string -- because the
 * table IS the benchmark. */
static long long countOf(const char *seq, size_t seqLen, const char *frag)
{
    Table m;
    long long c;

    countKmers(seq, seqLen, strlen(frag), &m);
    c = tableGet(&m, frag, strlen(frag));
    tableFree(&m);
    return c;
}

/* The THREE sequence from FASTA on stdin: uppercased, with the newlines and the
 * other two sequences left out. */
static char *thirdSequence(size_t *lenOut)
{
    char *lineBuf = (char *)malloc(256);
    size_t lineCap = 256;
    char *seq = NULL;
    size_t seqLen = 0;
    size_t seqCap = 0;
    int inThird = 0;
    long n;

    if (lineBuf == NULL) {
        oom();
    }
    while ((n = readLine(stdin, &lineBuf, &lineCap)) >= 0) {
        long i;
        if (n > 0 && lineBuf[0] == '>') {
            inThird = strncmp(lineBuf, ">THREE", 6) == 0;
            continue;
        }
        if (!inThird) {
            continue;
        }
        if (seqLen + (size_t)n + 1 > seqCap) {
            size_t next = seqCap == 0 ? 4096 : seqCap;
            char *grown;
            while (next < seqLen + (size_t)n + 1) {
                next *= 2;
            }
            grown = (char *)realloc(seq, next);
            if (grown == NULL) {
                oom();
            }
            seq = grown;
            seqCap = next;
        }
        for (i = 0; i < n; i++) {
            char c = lineBuf[i];
            if (c >= 'a' && c <= 'z') {
                c = (char)(c - 'a' + 'A');
            }
            seq[seqLen++] = c;
        }
    }
    if (seq == NULL) {
        seq = (char *)malloc(1);
        if (seq == NULL) {
            oom();
        }
    }
    seq[seqLen] = '\0';
    free(lineBuf);
    *lenOut = seqLen;
    return seq;
}

int main(void)
{
    /* The five fragments the game asks for by name. */
    static const char *namedFragments[5] = {
        "GGT", "GGTA", "GGTATT", "GGTATTTTAATT", "GGTATTTTAATTTATAGT",
    };
    size_t seqLen = 0;
    char *seq = thirdSequence(&seqLen);
    int i;

    report(seq, seqLen, 1);
    report(seq, seqLen, 2);
    for (i = 0; i < 5; i++) {
        printf("%lld\t%s\n", countOf(seq, seqLen, namedFragments[i]), namedFragments[i]);
    }
    free(seq);
    return 0;
}
