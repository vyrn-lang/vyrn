/* mandelbrot, from the Computer Language Benchmarks Game.
 *
 * A transcription of examples/mandelbrot.vyrn. Build with:
 *
 *   clang -O2 -ffp-contract=off -std=c11 -o mandelbrot.exe mandelbrot.c
 *
 * N is argv[1] (the width and height of the image); without it the census N
 * is used. The output is a P4 portable bitmap: a short text header, then one
 * bit per pixel packed eight to a byte, high bit first, each row padded on
 * the right to a byte boundary. Those bytes are arbitrary, so stdout must be
 * binary — on Windows the C runtime would otherwise rewrite the pixel byte
 * 0x0A as CRLF in the middle of the image.
 *
 * The kernel is the Vyrn program's, unchanged: 50 iterations, escape at
 * |z|^2 > 4, `-ffp-contract=off` so no fused multiply-add changes the
 * escape decision the other legs make.
 */

#include <stdio.h>
#include <stdlib.h>

#ifdef _WIN32
#include <fcntl.h>
#include <io.h>
#endif

/* The census N -- a 200x200 image. */
static const long ORDER = 200;

int main(int argc, char **argv)
{
    long n = ORDER;
    if (argc > 1)
        n = strtol(argv[1], NULL, 10);

#ifdef _WIN32
    _setmode(_fileno(stdout), _O_BINARY);
#endif

    printf("P4\n%ld %ld\n", n, n);

    /* One row of packed pixels at a time, exactly as the Vyrn program
     * buffers one row and writes it whole. */
    unsigned char *row = malloc((size_t)((n + 7) / 8));
    if (!row) {
        fprintf(stderr, "out of memory\n");
        return 1;
    }

    for (long y = 0; y < n; y++) {
        double ci = 2.0 * (double)y / (double)n - 1.0;
        long len = 0;
        int bits = 0;
        int nbits = 0;
        for (long x = 0; x < n; x++) {
            double cr = 2.0 * (double)x / (double)n - 1.5;
            double zr = 0.0;
            double zi = 0.0;
            int inside = 1;
            for (int i = 0; i < 50; i++) {
                double nzr = zr * zr - zi * zi + cr;
                zi = 2.0 * zr * zi + ci;
                zr = nzr;
                if (zr * zr + zi * zi > 4.0) {
                    inside = 0;
                    break;
                }
            }
            bits = bits * 2 + inside;
            nbits = nbits + 1;
            if (nbits == 8) {
                row[len++] = (unsigned char)bits;
                bits = 0;
                nbits = 0;
            }
        }
        /* P4 pads a partial byte on the RIGHT -- the unused low bits are
         * zero. */
        if (nbits > 0) {
            for (int pad = nbits; pad < 8; pad++)
                bits = bits * 2;
            row[len++] = (unsigned char)bits;
        }
        fwrite(row, 1, (size_t)len, stdout);
    }

    free(row);
    return 0;
}
