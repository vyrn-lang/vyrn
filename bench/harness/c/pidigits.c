/* pidigits, from the Computer Language Benchmarks Game.
 *
 * A transcription of examples/pidigits.vyrn. Build with:
 *
 *   clang -O2 -ffp-contract=off -std=c11 -o pidigits.exe pidigits.c
 *
 * N is argv[1]; without it the census N is used.
 *
 * This is the bounded spigot of Rabinowitz and Wagon, the same one the Vyrn
 * program runs -- it holds one small integer per digit column, so 64-bit
 * arithmetic is enough and no big integer is needed.
 */

#include <stdio.h>
#include <stdlib.h>

/* The census N -- 27 digits. */
#define CENSUS_ORDER 27

/* Extra digits computed and thrown away. A bounded spigot's last few columns
 * are the ones that can be wrong, so the bound is set past the answer. */
#define GUARD 10

static void *checkedMalloc(size_t bytes)
{
    void *p = malloc(bytes);

    if (p == NULL) {
        fprintf(stderr, "out of memory\n");
        exit(1);
    }
    return p;
}

/* The first `n` digits of pi, one per entry. */
static long long *piDigits(long long n)
{
    long long total = n + GUARD;
    long long len = 10 * total / 3 + 1;
    long long *a = (long long *)checkedMalloc((size_t)len * sizeof(long long));
    long long *out = (long long *)checkedMalloc((size_t)(total + 2) * sizeof(long long));
    long long *digits = (long long *)checkedMalloc((size_t)(n > 0 ? n : 1) * sizeof(long long));
    long long outLen = 0;
    long long nines = 0;
    long long predigit = 0;
    long long i, j, d;

    for (i = 0; i < len; i++) {
        a[i] = 2;
    }

    for (j = 0; j <= total; j++) {
        long long q = 0;
        long long k = len;
        while (k > 0) {
            long long x = 10 * a[k - 1] + q * k;
            a[k - 1] = x % (2 * k - 1);
            q = x / (2 * k - 1);
            k = k - 1;
        }
        a[0] = q % 10;
        q = q / 10;
        /* A run of nines is held back: the carry out of the next column can turn
         * all of them into zeroes and bump the digit before them. */
        if (q == 9) {
            nines = nines + 1;
        } else if (q == 10) {
            long long z;
            out[outLen++] = predigit + 1;
            for (z = 0; z < nines; z++) {
                out[outLen++] = 0;
            }
            predigit = 0;
            nines = 0;
        } else {
            long long z;
            out[outLen++] = predigit;
            predigit = q;
            for (z = 0; z < nines; z++) {
                out[outLen++] = 9;
            }
            nines = 0;
        }
    }

    /* Entry 0 is the zero that precedes the 3; the guard digits fall off the
     * end. */
    for (d = 1; d <= n; d++) {
        digits[d - 1] = out[d];
    }

    free(a);
    free(out);
    return digits;
}

/* The digits, ten to a line, each line tagged with how many have been printed.
 * A short last line is padded to ten so the tags stay in one column. */
static void run(long long n)
{
    long long *digits = piDigits(n);
    char line[16];
    int len = 0;
    long long i;

    for (i = 0; i < n; i++) {
        line[len++] = (char)('0' + digits[i]);
        if ((i + 1) % 10 == 0) {
            line[len] = '\0';
            printf("%s\t:%lld\n", line, i + 1);
            len = 0;
        }
    }
    if (len > 0) {
        while (len < 10) {
            line[len++] = ' ';
        }
        line[len] = '\0';
        printf("%s\t:%lld\n", line, n);
    }
    free(digits);
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
