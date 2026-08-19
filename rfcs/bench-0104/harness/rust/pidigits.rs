// pidigits, from the Computer Language Benchmarks Game.
//
// The Rust leg of RFC-0104 M2. It computes what `examples/pidigits.vyrn`
// computes and prints the same bytes.
//
//   $ rustc -C opt-level=3 -o pidigits.exe pidigits.rs
//   $ ./pidigits 27
//
// Safe Rust, std only — and, to stay the same program as the Vyrn one, the same
// bounded spigot of Rabinowitz and Wagon over `i64` rather than an arbitrary-
// precision entry. A big-integer entry is a different program; this row compares
// the two implementations of the same one.

use std::env;
use std::io::{self, BufWriter, Write};

/// The census N — 27 digits.
const ORDER: i64 = 27;

/// Extra digits computed and thrown away. A bounded spigot's last few columns are
/// the ones that can be wrong, so the bound is set past the answer.
const GUARD: i64 = 10;

/// The first `n` digits of pi, one per entry.
fn pi_digits(n: i64) -> Vec<i64> {
    let total = n + GUARD;
    let len = 10 * total / 3 + 1;
    let mut a = vec![2i64; len as usize];
    let mut out: Vec<i64> = Vec::new();
    let mut nines = 0;
    let mut predigit = 0;
    let mut j = 0;
    while j <= total {
        let mut q = 0;
        let mut k = len;
        while k > 0 {
            let x = 10 * a[(k - 1) as usize] + q * k;
            a[(k - 1) as usize] = x % (2 * k - 1);
            q = x / (2 * k - 1);
            k = k - 1;
        }
        a[0] = q % 10;
        q = q / 10;
        // A run of nines is held back: the carry out of the next column can turn
        // all of them into zeroes and bump the digit before them.
        if q == 9 {
            nines = nines + 1;
        } else if q == 10 {
            out.push(predigit + 1);
            for _ in 0..nines {
                out.push(0);
            }
            predigit = 0;
            nines = 0;
        } else {
            out.push(predigit);
            predigit = q;
            for _ in 0..nines {
                out.push(9);
            }
            nines = 0;
        }
        j = j + 1;
    }
    // Entry 0 is the zero that precedes the 3; the guard digits fall off the end.
    let mut digits = Vec::with_capacity(n as usize);
    let mut d = 1;
    while d <= n {
        digits.push(out[d as usize]);
        d = d + 1;
    }
    digits
}

/// The digits, ten to a line, each line tagged with how many have been printed.
/// A short last line is padded to ten so the tags stay in one column.
fn run(out: &mut impl Write, n: i64) {
    let digits = pi_digits(n);
    let mut line = String::new();
    let mut i = 0;
    while i < n {
        line.push_str(&digits[i as usize].to_string());
        if (i + 1) % 10 == 0 {
            writeln!(out, "{}\t:{}", line, i + 1).unwrap();
            line.clear();
        }
        i = i + 1;
    }
    if !line.is_empty() {
        while line.len() < 10 {
            line.push(' ');
        }
        writeln!(out, "{}\t:{}", line, n).unwrap();
    }
}

fn main() {
    let n: i64 = env::args()
        .nth(1)
        .map(|a| a.parse().expect("N must be an integer"))
        .unwrap_or(ORDER);

    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    run(&mut out, n);
}
