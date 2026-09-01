/* regex-redux, from the Computer Language Benchmarks Game.
 *
 * A transcription of examples/regexredux.vyrn. Build with:
 *
 *   clang -O2 -ffp-contract=off -std=c11 -o regexredux.exe regexredux.c
 *
 * There is no N: it reads FASTA on stdin, exactly as the Vyrn program does.
 *
 *   regexredux.exe < rfcs/bench-0104/fasta-1000.expected
 *
 * C ships no regex engine, so this leg carries the smallest one that covers
 * the corpus — every pattern is a literal of this file, and between them
 * they need exactly: literal bytes (with `\x` escapes), character classes
 * with optional negation and no ranges, top-level alternation, and a postfix
 * `*` on a class. Every starred class in the corpus EXCLUDES the byte that
 * follows it (`[^>]*>`, `[^\n]*\n`, `[^|]*\|`), so maximal munch is exact
 * and no backtracking exists to differ from the other legs' engines. Every
 * same-position branch pair matches at equal length, so first-branch-wins
 * and leftmost-longest agree on this input; the fixture is the proof.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void oom(void)
{
    fprintf(stderr, "out of memory\n");
    exit(1);
}

/* ---- the smallest regex that covers the corpus -------------------------- */

#define MAX_ITEMS 16
#define MAX_BRANCHES 4

/* One element of a branch: a set of admitted bytes, possibly starred. */
typedef struct {
    unsigned char in[256];
    int star;
} Item;

typedef struct {
    Item items[MAX_ITEMS];
    int nitems;
} Branch;

typedef struct {
    Branch branches[MAX_BRANCHES];
    int nbranches;
} Pattern;

/* `\x` outside a class: the escapes the corpus spells. */
static unsigned char unescape(char c)
{
    if (c == 'n')
        return '\n';
    return (unsigned char)c;
}

/* Compile `src`, or exit loudly: a pattern here is a literal of this file,
 * so a failure is a bug in this program and never a bad input. */
static Pattern compile(const char *src)
{
    Pattern p;
    memset(&p, 0, sizeof p);
    Branch *b = &p.branches[p.nbranches++];

    for (const char *s = src; *s != '\0'; s++) {
        if (*s == '|') {
            if (p.nbranches == MAX_BRANCHES) {
                fprintf(stderr, "`%s`: too many branches\n", src);
                exit(1);
            }
            b = &p.branches[p.nbranches++];
            continue;
        }
        if (b->nitems == MAX_ITEMS) {
            fprintf(stderr, "`%s`: too many items\n", src);
            exit(1);
        }
        Item *it = &b->items[b->nitems++];
        if (*s == '[') {
            int negate = 0;
            s++;
            if (*s == '^') {
                negate = 1;
                s++;
            }
            for (; *s != ']' && *s != '\0'; s++) {
                unsigned char c = (unsigned char)*s;
                if (c == '\\') {
                    s++;
                    c = unescape(*s);
                }
                it->in[c] = 1;
            }
            if (*s == '\0') {
                fprintf(stderr, "`%s`: unclosed class\n", src);
                exit(1);
            }
            if (negate)
                for (int c = 0; c < 256; c++)
                    it->in[c] = !it->in[c];
        } else if (*s == '\\') {
            s++;
            it->in[unescape(*s)] = 1;
        } else {
            it->in[(unsigned char)*s] = 1;
        }
        if (s[1] == '*') {
            it->star = 1;
            s++;
        }
    }
    return p;
}

/* The length of the match at `text + at`, or -1. Branches in order; a
 * starred item takes everything its class admits (see the header comment
 * for why that is exact here). */
static long matchAt(const Pattern *p, const unsigned char *text, long len, long at)
{
    for (int bi = 0; bi < p->nbranches; bi++) {
        const Branch *b = &p->branches[bi];
        long pos = at;
        int ok = 1;
        for (int ii = 0; ii < b->nitems; ii++) {
            const Item *it = &b->items[ii];
            if (it->star) {
                while (pos < len && it->in[text[pos]])
                    pos++;
            } else if (pos < len && it->in[text[pos]]) {
                pos++;
            } else {
                ok = 0;
                break;
            }
        }
        if (ok)
            return pos - at;
    }
    return -1;
}

/* Non-overlapping matches, left to right — the count the game prints. */
static long countMatches(const Pattern *p, const unsigned char *text, long len)
{
    long count = 0;
    long i = 0;
    while (i < len) {
        long m = matchAt(p, text, len, i);
        if (m > 0) {
            count++;
            i += m;
        } else {
            i++;
        }
    }
    return count;
}

/* Every match replaced with `to`, into a fresh buffer the caller frees. */
static unsigned char *replaceAll(const Pattern *p, const unsigned char *text,
                                 long len, const char *to, long *outLen)
{
    long toLen = (long)strlen(to);
    size_t cap = (size_t)len + 64;
    unsigned char *out = malloc(cap);
    if (!out)
        oom();
    long w = 0;
    long i = 0;
    while (i < len) {
        long m = matchAt(p, text, len, i);
        long take = m > 0 ? toLen : 1;
        if ((size_t)(w + take) > cap) {
            cap = cap * 2 + (size_t)take;
            out = realloc(out, cap);
            if (!out)
                oom();
        }
        if (m > 0) {
            memcpy(out + w, to, (size_t)toLen);
            w += toLen;
            i += m;
        } else {
            out[w++] = text[i++];
        }
    }
    *outLen = w;
    return out;
}

/* ---- the program -------------------------------------------------------- */

int main(void)
{
    /* The whole of standard input, linefeeds and all: the game counts the
     * INPUT length including the description lines. The C runtime's text
     * mode already folds CRLF to LF on Windows, which is the same fold the
     * harness applies to every leg's output. */
    size_t cap = 1 << 16;
    unsigned char *input = malloc(cap);
    if (!input)
        oom();
    long inputLength = 0;
    for (;;) {
        if ((size_t)inputLength == cap) {
            cap *= 2;
            input = realloc(input, cap);
            if (!input)
                oom();
        }
        size_t got = fread(input + inputLength, 1, cap - (size_t)inputLength, stdin);
        if (got == 0)
            break;
        inputLength += (long)got;
    }

    /* Remove the FASTA descriptions and every linefeed. */
    Pattern clean = compile(">[^\\n]*\\n|\\n");
    long cleanLength = 0;
    unsigned char *sequence = replaceAll(&clean, input, inputLength, "", &cleanLength);

    static const char *variants[] = {
        "agggtaaa|tttaccct",
        "[cgt]gggtaaa|tttaccc[acg]",
        "a[act]ggtaaa|tttacc[agt]t",
        "ag[act]gtaaa|tttac[agt]ct",
        "agg[act]taaa|ttta[agt]cct",
        "aggg[acg]aaa|ttt[cgt]ccct",
        "agggt[cgt]aa|tt[acg]accct",
        "agggta[cgt]a|t[acg]taccct",
        "agggtaa[cgt]|[acg]ttaccct",
    };
    for (int v = 0; v < 9; v++) {
        Pattern p = compile(variants[v]);
        printf("%s %ld\n", variants[v], countMatches(&p, sequence, cleanLength));
    }

    /* The five rewrites, each over the result of the last. */
    static const char *subs[][2] = {
        { "tHa[Nt]", "<4>" },
        { "aND|caN|Ha[DS]|WaS", "<3>" },
        { "a[NSt]|BY", "<2>" },
        { "<[^>]*>", "|" },
        { "\\|[^|][^|]*\\|", "-" },
    };
    unsigned char *rewritten = sequence;
    long rewrittenLength = cleanLength;
    for (int r = 0; r < 5; r++) {
        Pattern p = compile(subs[r][0]);
        long nextLength = 0;
        unsigned char *next = replaceAll(&p, rewritten, rewrittenLength, subs[r][1], &nextLength);
        if (rewritten != sequence)
            free(rewritten);
        rewritten = next;
        rewrittenLength = nextLength;
    }

    printf("\n%ld\n%ld\n%ld\n", inputLength, cleanLength, rewrittenLength);

    if (rewritten != sequence)
        free(rewritten);
    free(sequence);
    free(input);
    return 0;
}
