// spectral-norm, from the Computer Language Benchmarks Game.
//
// The Rust leg of RFC-0104 M2. It computes what `examples/spectralnorm.vyrn`
// computes, in the same expression order, and prints the same bytes.
//
//   $ rustc -C opt-level=3 -o spectralnorm.exe spectralnorm.rs
//   $ ./spectralnorm 100
//
// Safe Rust, std only. Single threaded: the Vyrn program is, and the row is a
// measurement of the numeric path and not of a thread pool.

use std::env;

/// The census N — a 100x100 window of the matrix.
const ORDER: i64 = 100;

/// `v` at nine decimal places, the format the game prints.
fn fixed9(v: f64) -> String {
    let mut a = v;
    let mut sign = "";
    if a < 0.0 {
        sign = "-";
        a = 0.0 - a;
    }
    let scaled = (a * 1000000000.0 + 0.5) as i64;
    format!("{}{}.{:09}", sign, scaled / 1000000000, scaled % 1000000000)
}

/// `A[i][j]` — the matrix is a formula, so no matrix is ever built.
fn cell(i: i64, j: i64) -> f64 {
    1.0 / (((i + j) * (i + j + 1) / 2 + i + 1) as f64)
}

/// `out = A v`.
fn multiply_av(n: i64, v: &[f64], out: &mut [f64]) {
    for i in 0..n {
        let mut sum = 0.0;
        for j in 0..n {
            sum = sum + cell(i, j) * v[j as usize];
        }
        out[i as usize] = sum;
    }
}

/// `out = A-transpose v`, which is `multiply_av` with the indices of `cell` swapped.
fn multiply_atv(n: i64, v: &[f64], out: &mut [f64]) {
    for i in 0..n {
        let mut sum = 0.0;
        for j in 0..n {
            sum = sum + cell(j, i) * v[j as usize];
        }
        out[i as usize] = sum;
    }
}

/// Ten rounds of `u = A-transpose A u`, then the Rayleigh quotient's square root.
fn spectral_norm(n: i64) -> f64 {
    let mut u = vec![1.0; n as usize];
    let mut v = vec![0.0; n as usize];
    let mut w = vec![0.0; n as usize];
    for _ in 0..10 {
        multiply_av(n, &u, &mut w);
        multiply_atv(n, &w, &mut v);
        multiply_av(n, &v, &mut w);
        multiply_atv(n, &w, &mut u);
    }
    let mut vbv = 0.0;
    let mut vv = 0.0;
    for k in 0..n as usize {
        vbv = vbv + u[k] * v[k];
        vv = vv + v[k] * v[k];
    }
    (vbv / vv).sqrt()
}

fn main() {
    let n: i64 = env::args()
        .nth(1)
        .map(|a| a.parse().expect("N must be an integer"))
        .unwrap_or(ORDER);

    println!("{}", fixed9(spectral_norm(n)));
}
