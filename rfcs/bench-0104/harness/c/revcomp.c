/* reverse-complement, from the Computer Language Benchmarks Game.
 *
 * A transcription of examples/revcomp.vyrn. Build with:
 *
 *   clang -O2 -ffp-contract=off -std=c11 -o revcomp.exe revcomp.c
 *
 * There is no N: it reads FASTA on stdin, exactly as the Vyrn program does.
 *
 *   revcomp.exe < rfcs/bench-0104/fasta-1000.expected
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void oom(void)
{
    fprintf(stderr, "out of memory\n");
    exit(1);
}

/* One line of stdin, without its newline, into a buffer that grows as needed.
 * Returns the length, or -1 at end of input. Vyrn's `readLine` has no length
 * limit, so neither does this. */
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

/* The IUB complement table, 256 entries, indexed by the input byte. Bases that
 * are not IUB codes map to themselves, which is what makes the lookup total. */
static void complementTable(unsigned char *t)
{
    static const char *from = "ACBDGHKMNSRUTWVYacbdghkmnsrutwvy";
    static const char *to = "TGVHCDMKNSYAAWBRTGVHCDMKNSYAAWBR";
    int i;
    size_t k;

    for (i = 0; i < 256; i++) {
        t[i] = (unsigned char)i;
    }
    for (k = 0; k < strlen(from); k++) {
        t[(unsigned char)from[k]] = (unsigned char)to[k];
    }
}

/* `seq` backwards, every base complemented. */
static unsigned char *reverseComplement(const unsigned char *seq, size_t len,
                                        const unsigned char *table)
{
    unsigned char *out = (unsigned char *)malloc(len + 1);
    size_t i;

    if (out == NULL) {
        oom();
    }
    for (i = 0; i < len; i++) {
        out[i] = table[seq[len - 1 - i]];
    }
    return out;
}

/* `bs` printed 60 columns to a line, with a short last line if it does not
 * divide. */
static void writeWrapped(const unsigned char *bs, size_t len)
{
    char w[61];
    size_t n = 0;
    size_t i;

    for (i = 0; i < len; i++) {
        w[n++] = (char)bs[i];
        if (n == 60) {
            w[n] = '\0';
            printf("%s\n", w);
            n = 0;
        }
    }
    if (n > 0) {
        w[n] = '\0';
        printf("%s\n", w);
    }
}

int main(void)
{
    unsigned char table[256];
    char *lineBuf = (char *)malloc(256);
    size_t lineCap = 256;
    char *header = NULL;
    unsigned char *seq = NULL;
    size_t seqLen = 0;
    size_t seqCap = 0;
    long n;

    if (lineBuf == NULL) {
        oom();
    }
    complementTable(table);

    while ((n = readLine(stdin, &lineBuf, &lineCap)) >= 0) {
        if (n > 0 && lineBuf[0] == '>') {
            if (header != NULL) {
                unsigned char *rc = reverseComplement(seq, seqLen, table);
                printf("%s\n", header);
                writeWrapped(rc, seqLen);
                free(rc);
                seqLen = 0;
                free(header);
            }
            header = (char *)malloc((size_t)n + 1);
            if (header == NULL) {
                oom();
            }
            memcpy(header, lineBuf, (size_t)n + 1);
        } else {
            if (seqLen + (size_t)n > seqCap) {
                size_t next = seqCap == 0 ? 4096 : seqCap;
                unsigned char *grown;
                while (next < seqLen + (size_t)n) {
                    next *= 2;
                }
                grown = (unsigned char *)realloc(seq, next);
                if (grown == NULL) {
                    oom();
                }
                seq = grown;
                seqCap = next;
            }
            memcpy(seq + seqLen, lineBuf, (size_t)n);
            seqLen += (size_t)n;
        }
    }

    if (header != NULL) {
        unsigned char *rc = reverseComplement(seq, seqLen, table);
        printf("%s\n", header);
        writeWrapped(rc, seqLen);
        free(rc);
        free(header);
    }

    free(seq);
    free(lineBuf);
    return 0;
}
