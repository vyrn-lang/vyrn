/* spectral-norm, from the Computer Language Benchmarks Game.
 *
 * A transcription of examples/spectralnorm.vyrn. Build with:
 *
 *   clang -O2 -ffp-contract=off -std=c11 -o spectralnorm.exe spectralnorm.c
 *
 * N is argv[1] (the order of the matrix window); without it the census N is
 * used.
 */

#include <stdio.h>
#include <stdlib.h>
#include <math.h>

/* The census N -- a 100x100 window of the matrix. */
#define CENSUS_ORDER 100

/* The Vyrn program takes one lane of F64x2.sqrt, which is IEEE sqrt. */
static double sqrtF(double v)
{
    return sqrt(v);
}

/* `v` at nine decimal places, the format the game prints. */
static void printFixed9(double v)
{
    const char *sign = "";
    double a = v;
    long long scaled;

    if (a < 0.0) {
        sign = "-";
        a = 0.0 - a;
    }
    scaled = (long long)(a * 1000000000.0 + 0.5);
    printf("%s%lld.%09lld\n", sign, scaled / 1000000000, scaled % 1000000000);
}

/* `A[i][j]` -- the matrix is a formula, so no matrix is ever built. */
static double cell(long long i, long long j)
{
    return 1.0 / (double)((i + j) * (i + j + 1) / 2 + i + 1);
}

/* `w = A v`. */
static void multiplyAv(long long n, const double *v, double *w)
{
    long long i, j;

    for (i = 0; i < n; i++) {
        double sum = 0.0;
        for (j = 0; j < n; j++) {
            sum = sum + cell(i, j) * v[j];
        }
        w[i] = sum;
    }
}

/* `w = A-transpose v`, which is multiplyAv with the indices of cell swapped. */
static void multiplyAtv(long long n, const double *v, double *w)
{
    long long i, j;

    for (i = 0; i < n; i++) {
        double sum = 0.0;
        for (j = 0; j < n; j++) {
            sum = sum + cell(j, i) * v[j];
        }
        w[i] = sum;
    }
}

/* `n` copies of `x`, the shape the working vectors start in. */
static double *filled(long long n, double x)
{
    double *out = (double *)malloc((size_t)n * sizeof(double));
    long long i;

    if (out == NULL) {
        fprintf(stderr, "out of memory\n");
        exit(1);
    }
    for (i = 0; i < n; i++) {
        out[i] = x;
    }
    return out;
}

/* Ten rounds of `u = A-transpose A u`, then the Rayleigh quotient's square
 * root. */
static double spectralNorm(long long n)
{
    double *u = filled(n, 1.0);
    double *v = filled(n, 0.0);
    double *w = filled(n, 0.0);
    double vbv = 0.0;
    double vv = 0.0;
    long long round, k;
    double result;

    for (round = 0; round < 10; round++) {
        multiplyAv(n, u, w);
        multiplyAtv(n, w, v);
        multiplyAv(n, v, w);
        multiplyAtv(n, w, u);
    }
    for (k = 0; k < n; k++) {
        vbv = vbv + u[k] * v[k];
        vv = vv + v[k] * v[k];
    }
    result = sqrtF(vbv / vv);
    free(u);
    free(v);
    free(w);
    return result;
}

int main(int argc, char **argv)
{
    long long order = CENSUS_ORDER;

    if (argc > 1) {
        order = strtoll(argv[1], NULL, 10);
    }
    printFixed9(spectralNorm(order));
    return 0;
}
