/* fasta, from the Computer Language Benchmarks Game.
 *
 * A transcription of examples/fasta.vyrn. Build with:
 *
 *   clang -O2 -ffp-contract=off -std=c11 -o fasta.exe fasta.c
 *
 * N is argv[1]; without it the census N is used. The output at the census N is
 * rfcs/bench-0104/fasta-1000.expected, which is also the stdin of revcomp and
 * knucleotide.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* The census N. The game's own N is 25,000,000, which is 250 MB of output. */
#define CENSUS_ORDER 1000

/* The 287-base repeat unit of the ONE sequence, as the game publishes it. */
static const char *alu =
    "GGCCGGGCGCGGTGGCTCACGCCTGTAATCCCAGCACTTTGGGAGGCCGAGGCGGGCGGATCACCTGAGGTCAGGAGTT"
    "CGAGACCAGCCTGGCCAACATGGTGAAACCCCGTCTCTACTAAAAATACAAAAATTAGCCGGGCGTGGTGGCGCGCGCC"
    "TGTAATCCCAGCTACTCGGGAGGCTGAGGCAGGAGAATCGCTTGAACCCGGGAGGCGGAGGTTGCAGTGAGCCGAGATC"
    "GCGCCACTGCACTCCAGCCTGGGCGACAGAGCGAGACTCCGTCTCAAAAA";

/* The generator's state. It is file scope and not a parameter because the game
 * specifies one stream shared by both random sequences: THREE continues where
 * TWO left off. */
static long long seed = 42;

/* The game's linear congruential generator. */
static double nextRandom(double max)
{
    seed = (seed * 3877 + 29573) % 139968;
    return max * (double)seed / 139968.0;
}

/* The running totals of `ws` -- the form the weighted pick reads. */
static void cumulative(const double *ws, int n, double *out)
{
    double p = 0.0;
    int i;

    for (i = 0; i < n; i++) {
        p = p + ws[i];
        out[i] = p;
    }
}

/* The first symbol whose running total is above the next random draw. A linear
 * scan, as the game specifies. */
static char pick(const char *syms, const double *cum, int n)
{
    double r = nextRandom(1.0);
    int i;

    for (i = 0; i < n; i++) {
        if (cum[i] > r) {
            return syms[i];
        }
    }
    return syms[n - 1];
}

/* The width of an output line. */
static long long lineWidth(long long todo)
{
    if (todo < 60) {
        return todo;
    }
    return 60;
}

/* `count` bases taken from `src` cyclically -- the ONE sequence. */
static void repeatFasta(const char *header, const char *src, long long count)
{
    long long srcLen = (long long)strlen(src);
    long long k = 0;
    long long todo = count;
    char w[61];

    printf("%s\n", header);
    while (todo > 0) {
        long long m = lineWidth(todo);
        long long i;
        for (i = 0; i < m; i++) {
            w[i] = src[k];
            k = (k + 1) % srcLen;
        }
        w[m] = '\0';
        printf("%s\n", w);
        todo = todo - m;
    }
}

/* `count` bases drawn from the weighted table -- the TWO and THREE sequences. */
static void randomFasta(const char *header, const char *syms, const double *cum, int nsyms,
                        long long count)
{
    long long todo = count;
    char w[61];

    printf("%s\n", header);
    while (todo > 0) {
        long long m = lineWidth(todo);
        long long i;
        for (i = 0; i < m; i++) {
            w[i] = pick(syms, cum, nsyms);
        }
        w[m] = '\0';
        printf("%s\n", w);
        todo = todo - m;
    }
}

/* The IUB ambiguity codes and their published weights. */
static const double iubWeights[15] = {
    0.27, 0.12, 0.12, 0.27, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02,
};

/* The Homo sapiens base frequencies, as the game publishes them. */
static const double humanWeights[4] = {
    0.3029549426680, 0.1979883004921, 0.1975473066391, 0.3015094502008,
};

/* The whole run at `n` -- three sequences in the order the generator's single
 * stream requires. */
static void run(long long n)
{
    double iubCum[15];
    double humanCum[4];

    cumulative(iubWeights, 15, iubCum);
    cumulative(humanWeights, 4, humanCum);

    repeatFasta(">ONE Homo sapiens alu", alu, n * 2);
    randomFasta(">TWO IUB ambiguity codes", "acgtBDHKMNRSVWY", iubCum, 15, n * 3);
    randomFasta(">THREE Homo sapiens frequency", "acgt", humanCum, 4, n * 5);
}

int main(int argc, char **argv)
{
    long long order = CENSUS_ORDER;

    if (argc > 1) {
        order = strtoll(argv[1], NULL, 10);
    }
    run(order);
    return 0;
}
