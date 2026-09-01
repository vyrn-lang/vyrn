// mandelbrot, from the Computer Language Benchmarks Game.
//
// The Rust leg of RFC-0104 M2. It computes what `examples/mandelbrot.vyrn`
// computes, in the same expression order, and prints the same bytes.
//
//   $ rustc -C opt-level=3 -o mandelbrot.exe mandelbrot.rs
//   $ ./mandelbrot 200
//
// Safe Rust, std only. The output is a P4 portable bitmap — a text header,
// then packed pixel bytes — written through `write_all`, which Rust keeps
// binary on every platform. The kernel is the Vyrn program's: 50 iterations,
// escape at |z|^2 > 4, no fused multiply-add (rustc does not contract).

use std::env;
use std::io::{self, Write};

/// The census N — a 200x200 image.
const ORDER: i64 = 200;

fn main() {
    let n: i64 = env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(ORDER);

    let stdout = io::stdout();
    let mut out = stdout.lock();
    out.write_all(format!("P4\n{} {}\n", n, n).as_bytes()).unwrap();

    // One row of packed pixels at a time, exactly as the Vyrn program
    // buffers one row and writes it whole.
    let mut row: Vec<u8> = Vec::with_capacity(((n + 7) / 8) as usize);
    for y in 0..n {
        let ci = 2.0 * y as f64 / n as f64 - 1.0;
        row.clear();
        let mut bits: i32 = 0;
        let mut nbits: i32 = 0;
        for x in 0..n {
            let cr = 2.0 * x as f64 / n as f64 - 1.5;
            let mut zr = 0.0f64;
            let mut zi = 0.0f64;
            let mut inside = 1;
            for _ in 0..50 {
                let nzr = zr * zr - zi * zi + cr;
                zi = 2.0 * zr * zi + ci;
                zr = nzr;
                if zr * zr + zi * zi > 4.0 {
                    inside = 0;
                    break;
                }
            }
            bits = bits * 2 + inside;
            nbits += 1;
            if nbits == 8 {
                row.push(bits as u8);
                bits = 0;
                nbits = 0;
            }
        }
        // P4 pads a partial byte on the RIGHT — the unused low bits are zero.
        if nbits > 0 {
            for _ in nbits..8 {
                bits *= 2;
            }
            row.push(bits as u8);
        }
        out.write_all(&row).unwrap();
    }
}
