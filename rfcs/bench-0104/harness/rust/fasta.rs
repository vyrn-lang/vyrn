// fasta, from the Computer Language Benchmarks Game.
//
// The Rust leg of RFC-0104 M2. It computes what `examples/fasta.vyrn` computes
// and prints the same bytes — 10,245 of them at the census N.
//
//   $ rustc -C opt-level=3 -o fasta.exe fasta.rs
//   $ ./fasta 1000
//
// Safe Rust, std only.

use std::env;
use std::io::{self, BufWriter, Write};

/// The census N.
const ORDER: i64 = 1000;

/// The 287-base repeat unit of the ONE sequence, as the game publishes it.
const ALU: &[u8] = b"GGCCGGGCGCGGTGGCTCACGCCTGTAATCCCAGCACTTTGGGAGGCCGAGGCGGGCGGATCACCTGAGGTCAGGAGTTCGAGACCAGCCTGGCCAACATGGTGAAACCCCGTCTCTACTAAAAATACAAAAATTAGCCGGGCGTGGTGGCGCGCGCCTGTAATCCCAGCTACTCGGGAGGCTGAGGCAGGAGAATCGCTTGAACCCGGGAGGCGGAGGTTGCAGTGAGCCGAGATCGCGCCACTGCACTCCAGCCTGGGCGACAGAGCGAGACTCCGTCTCAAAAA";

/// The IUB ambiguity codes and their published weights.
const IUB_SYMS: &[u8] = b"acgtBDHKMNRSVWY";
const IUB_WEIGHTS: [f64; 15] = [
    0.27, 0.12, 0.12, 0.27, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02,
];

/// The Homo sapiens base frequencies, as the game publishes them.
const HUMAN_SYMS: &[u8] = b"acgt";
const HUMAN_WEIGHTS: [f64; 4] = [0.3029549426680, 0.1979883004921, 0.1975473066391, 0.3015094502008];

/// The game's linear congruential generator. One stream is shared by both random
/// sequences: THREE continues where TWO left off.
struct Rand {
    seed: i64,
}

impl Rand {
    fn next(&mut self, max: f64) -> f64 {
        self.seed = (self.seed * 3877 + 29573) % 139968;
        max * (self.seed as f64) / 139968.0
    }
}

/// The running totals of `ws` — the form the weighted pick reads.
fn cumulative(ws: &[f64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(ws.len());
    let mut p = 0.0;
    for w in ws {
        p = p + w;
        out.push(p);
    }
    out
}

/// The first symbol whose running total is above the next random draw. A linear
/// scan, as the game specifies.
fn pick(rng: &mut Rand, syms: &[u8], cum: &[f64]) -> u8 {
    let r = rng.next(1.0);
    for i in 0..cum.len() {
        if cum[i] > r {
            return syms[i];
        }
    }
    syms[syms.len() - 1]
}

/// The width of an output line.
fn line_width(todo: i64) -> i64 {
    if todo < 60 {
        return todo;
    }
    60
}

/// `count` bases taken from `src` cyclically — the ONE sequence.
fn repeat_fasta(out: &mut impl Write, header: &str, src: &[u8], count: i64) {
    writeln!(out, "{}", header).unwrap();
    let mut w: Vec<u8> = Vec::with_capacity(60);
    let mut k = 0;
    let mut todo = count;
    while todo > 0 {
        let m = line_width(todo);
        w.clear();
        for _ in 0..m {
            w.push(src[k]);
            k = (k + 1) % src.len();
        }
        w.push(b'\n');
        out.write_all(&w).unwrap();
        todo = todo - m;
    }
}

/// `count` bases drawn from the weighted table — the TWO and THREE sequences.
fn random_fasta(
    out: &mut impl Write,
    rng: &mut Rand,
    header: &str,
    syms: &[u8],
    cum: &[f64],
    count: i64,
) {
    writeln!(out, "{}", header).unwrap();
    let mut w: Vec<u8> = Vec::with_capacity(60);
    let mut todo = count;
    while todo > 0 {
        let m = line_width(todo);
        w.clear();
        for _ in 0..m {
            w.push(pick(rng, syms, cum));
        }
        w.push(b'\n');
        out.write_all(&w).unwrap();
        todo = todo - m;
    }
}

/// The whole run at `n` — three sequences in the order the generator's single
/// stream requires.
fn run(out: &mut impl Write, n: i64) {
    let mut rng = Rand { seed: 42 };
    repeat_fasta(out, ">ONE Homo sapiens alu", ALU, n * 2);
    random_fasta(
        out,
        &mut rng,
        ">TWO IUB ambiguity codes",
        IUB_SYMS,
        &cumulative(&IUB_WEIGHTS),
        n * 3,
    );
    random_fasta(
        out,
        &mut rng,
        ">THREE Homo sapiens frequency",
        HUMAN_SYMS,
        &cumulative(&HUMAN_WEIGHTS),
        n * 5,
    );
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
