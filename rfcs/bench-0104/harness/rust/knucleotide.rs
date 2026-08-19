// k-nucleotide, from the Computer Language Benchmarks Game.
//
// The Rust leg of RFC-0104 M2. It computes what `examples/knucleotide.vyrn`
// computes and prints the same bytes.
//
//   $ rustc -C opt-level=3 -o knucleotide.exe knucleotide.rs
//   $ ./knucleotide < ../../fasta-1000.expected
//
// Safe Rust, std only: `std::collections::HashMap` with its default hasher, and
// no external hash crate. There is no N: the input is FASTA on stdin.
//
// The keys are borrowed `&[u8]` windows of the sequence, which is what a Rust
// program would naturally write — `seq.windows(k)` allocates nothing. The Vyrn
// program allocates a `String` per window because `Map` is keyed by `String`.

use std::collections::HashMap;
use std::io::{self, BufRead, BufWriter, Write};

/// The five fragments the game asks for by name, after the two frequency tables.
const NAMED_FRAGMENTS: [&str; 5] = [
    "GGT",
    "GGTA",
    "GGTATT",
    "GGTATTTTAATT",
    "GGTATTTTAATTTATAGT",
];

/// The THREE sequence from FASTA on stdin: uppercased, with the newlines and the
/// other two sequences left out.
fn third_sequence(input: &mut impl BufRead) -> Vec<u8> {
    let mut out = Vec::new();
    let mut in_third = false;
    for line in input.lines() {
        let l = line.unwrap();
        if l.starts_with('>') {
            in_third = l.starts_with(">THREE");
        } else if in_third {
            out.extend(l.bytes().map(|b| b.to_ascii_uppercase()));
        }
    }
    out
}

/// Every window of width `k`, counted.
fn count_kmers(seq: &[u8], k: usize) -> HashMap<&[u8], i64> {
    let mut m: HashMap<&[u8], i64> = HashMap::new();
    for w in seq.windows(k) {
        *m.entry(w).or_insert(0) += 1;
    }
    m
}

/// `x` at three decimal places — the game asks for three.
fn fixed3(x: f64) -> String {
    let scaled = (x * 1000.0 + 0.5) as i64;
    format!("{}.{:03}", scaled / 1000, scaled % 1000)
}

/// Count descending, ties by fragment ascending.
fn ranked(m: HashMap<&[u8], i64>) -> Vec<(&[u8], i64)> {
    let mut es: Vec<(&[u8], i64)> = m.into_iter().collect();
    es.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    es
}

/// One frequency table: every fragment of width `k` as a percentage of the
/// windows there are, then a blank line.
fn report(out: &mut impl Write, seq: &[u8], k: usize) {
    let m = count_kmers(seq, k);
    let total = seq.len() as i64 - k as i64 + 1;
    for (frag, count) in ranked(m) {
        writeln!(
            out,
            "{} {}",
            std::str::from_utf8(frag).unwrap(),
            fixed3(100.0 * count as f64 / total as f64)
        )
        .unwrap();
    }
    writeln!(out).unwrap();
}

/// The count of one named fragment. It builds the whole table for that width and
/// looks the fragment up, because the table IS the benchmark.
fn count_of(seq: &[u8], frag: &str) -> i64 {
    let m = count_kmers(seq, frag.len());
    *m.get(frag.as_bytes()).unwrap_or(&0)
}

fn main() {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());

    let seq = third_sequence(&mut input);
    report(&mut out, &seq, 1);
    report(&mut out, &seq, 2);
    for frag in NAMED_FRAGMENTS {
        writeln!(out, "{}\t{}", count_of(&seq, frag), frag).unwrap();
    }
}
