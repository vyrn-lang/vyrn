/* fannkuch-redux, from the Computer Language Benchmarks Game.
 *
 * A transcription of examples/fannkuch.vyrn. Build with:
 *
 *   clang -O2 -ffp-contract=off -std=c11 -o fannkuch.exe fannkuch.c
 *
 * N is argv[1]; without it the census N is used.
 */

#include <stdio.h>
#include <stdlib.h>

/* The census N. The work is `n!`, so this is not a number to raise casually. */
#define CENSUS_ORDER 7

typedef struct {
    long long checksum;
    long long maxflips;
} Fold;

static long long *allocLongs(long long n)
{
    long long *out = (long long *)malloc((size_t)n * sizeof(long long));

    if (out == NULL) {
        fprintf(stderr, "out of memory\n");
        exit(1);
    }
    return out;
}

/* Reverse `p[0 ..= k]` in place -- the flip the benchmark counts. */
static void flip(long long *a, long long k)
{
    long long i = 0;
    long long j = k;

    while (i < j) {
        long long t = a[i];
        a[i] = a[j];
        a[j] = t;
        i = i + 1;
        j = j - 1;
    }
}

/* How many flips `a` takes to bring a 0 to the front. `a` is the scratch copy,
 * so this destroys it. */
static long long foldCount(long long *a)
{
    long long flips = 0;
    long long k = a[0];

    while (k != 0) {
        flip(a, k);
        flips = flips + 1;
        k = a[0];
    }
    return flips;
}

/* The alternating-sign checksum and the deepest fold, over every permutation of
 * `n` elements in the game's prescribed order. */
static Fold fannkuch(long long n)
{
    long long *perm1 = allocLongs(n);
    long long *count = allocLongs(n);
    long long *scratch = allocLongs(n);
    long long maxflips = 0;
    long long checksum = 0;
    long long permcount = 0;
    long long r = n;
    int done = 0;
    long long i;
    Fold f;

    for (i = 0; i < n; i++) {
        perm1[i] = i;
        count[i] = 0;
    }

    while (!done) {
        long long flips;
        int advanced = 0;

        while (r != 1) {
            count[r - 1] = r;
            r = r - 1;
        }
        for (i = 0; i < n; i++) {
            scratch[i] = perm1[i];
        }
        flips = foldCount(scratch);
        if (flips > maxflips) {
            maxflips = flips;
        }
        if (permcount % 2 == 0) {
            checksum = checksum + flips;
        } else {
            checksum = checksum - flips;
        }
        permcount = permcount + 1;
        /* The next permutation, by rotating the first `r + 1` entries left and
         * carrying into the next position when a rotation runs out. */
        while (!advanced && !done) {
            if (r == n) {
                done = 1;
            } else {
                long long first = perm1[0];
                long long m = 0;
                while (m < r) {
                    perm1[m] = perm1[m + 1];
                    m = m + 1;
                }
                perm1[r] = first;
                count[r] = count[r] - 1;
                if (count[r] > 0) {
                    advanced = 1;
                } else {
                    r = r + 1;
                }
            }
        }
    }

    free(perm1);
    free(count);
    free(scratch);
    f.checksum = checksum;
    f.maxflips = maxflips;
    return f;
}

int main(int argc, char **argv)
{
    long long order = CENSUS_ORDER;
    Fold f;

    if (argc > 1) {
        order = strtoll(argv[1], NULL, 10);
    }
    f = fannkuch(order);
    printf("%lld\n", f.checksum);
    printf("Pfannkuchen(%lld) = %lld\n", order, f.maxflips);
    return 0;
}
