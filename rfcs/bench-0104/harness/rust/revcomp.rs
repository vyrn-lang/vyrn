// reverse-complement, from the Computer Language Benchmarks Game.
//
// The Rust leg of RFC-0104 M2. It computes what `examples/revcomp.vyrn`
// computes and prints the same bytes.
//
//   $ rustc -C opt-level=3 -o revcomp.exe revcomp.rs
//   $ ./revcomp < ../../fasta-1000.expected
//
// Safe Rust, std only. There is no N: the input is FASTA on stdin.

use std::io::{self, BufRead, BufWriter, Write};

/// The IUB complement table, 256 entries, indexed by the input byte. Bases that
/// are not IUB codes map to themselves, which is what makes the lookup total.
fn complement_table() -> [u8; 256] {
    let mut t = [0u8; 256];
    for i in 0..256 {
        t[i] = i as u8;
    }
    let from = b"ACBDGHKMNSRUTWVYacbdghkmnsrutwvy";
    let to = b"TGVHCDMKNSYAAWBRTGVHCDMKNSYAAWBR";
    for k in 0..from.len() {
        t[from[k] as usize] = to[k];
    }
    t
}

/// `seq` backwards, every base complemented.
fn reverse_complement(seq: &[u8], table: &[u8; 256]) -> Vec<u8> {
    seq.iter().rev().map(|&b| table[b as usize]).collect()
}

/// `bs` printed 60 columns to a line, with a short last line if it does not
/// divide.
fn write_wrapped(out: &mut impl Write, bs: &[u8]) {
    for chunk in bs.chunks(60) {
        out.write_all(chunk).unwrap();
        out.write_all(b"\n").unwrap();
    }
}

/// The whole run: one header and one sequence at a time, transformed and
/// written as soon as its last line arrives.
fn run(input: &mut impl BufRead, out: &mut impl Write) {
    let table = complement_table();
    let mut header = String::new();
    let mut seq: Vec<u8> = Vec::new();
    for line in input.lines() {
        let l = line.unwrap();
        if l.as_bytes().first() == Some(&b'>') {
            if !header.is_empty() {
                writeln!(out, "{}", header).unwrap();
                write_wrapped(out, &reverse_complement(&seq, &table));
                seq.clear();
            }
            header = l;
        } else {
            seq.extend_from_slice(l.as_bytes());
        }
    }
    if !header.is_empty() {
        writeln!(out, "{}", header).unwrap();
        write_wrapped(out, &reverse_complement(&seq, &table));
    }
}

fn main() {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    run(&mut input, &mut out);
}
