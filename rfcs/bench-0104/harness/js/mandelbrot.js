// mandelbrot, the JavaScript leg of RFC-0104 M2.
//
// The specification is examples/mandelbrot.vyrn. Same algorithm, same
// expression order, byte-identical output.
//
//   $ node mandelbrot.js         # the census N, a 200x200 image
//   $ node mandelbrot.js 200     # the bench N (the same 200 — see run.py)
//
// The output is a P4 portable bitmap — a text header, then packed pixel
// bytes — written as Buffers through process.stdout.write, which Node keeps
// binary. JS numbers are IEEE doubles evaluated without contraction, so the
// escape decision is the one the other legs make.

"use strict";

// The census N — a 200x200 image.
const ORDER = 200;

function main() {
    const n = process.argv.length > 2 ? parseInt(process.argv[2], 10) : ORDER;

    process.stdout.write("P4\n" + n + " " + n + "\n");

    // One row of packed pixels at a time, exactly as the Vyrn program
    // buffers one row and writes it whole.
    const row = Buffer.alloc(Math.floor((n + 7) / 8));
    for (let y = 0; y < n; y++) {
        const ci = 2.0 * y / n - 1.0;
        let len = 0;
        let bits = 0;
        let nbits = 0;
        for (let x = 0; x < n; x++) {
            const cr = 2.0 * x / n - 1.5;
            let zr = 0.0;
            let zi = 0.0;
            let inside = 1;
            for (let i = 0; i < 50; i++) {
                const nzr = zr * zr - zi * zi + cr;
                zi = 2.0 * zr * zi + ci;
                zr = nzr;
                if (zr * zr + zi * zi > 4.0) {
                    inside = 0;
                    break;
                }
            }
            bits = bits * 2 + inside;
            nbits += 1;
            if (nbits === 8) {
                row[len++] = bits;
                bits = 0;
                nbits = 0;
            }
        }
        // P4 pads a partial byte on the RIGHT — the unused low bits are zero.
        if (nbits > 0) {
            for (let pad = nbits; pad < 8; pad++) bits *= 2;
            row[len++] = bits;
        }
        process.stdout.write(Buffer.from(row.subarray(0, len)));
    }
}

main();
