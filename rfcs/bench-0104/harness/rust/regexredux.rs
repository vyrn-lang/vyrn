// regex-redux, from the Computer Language Benchmarks Game.
//
// The Rust leg of RFC-0104 M2. It computes what `examples/regexredux.vyrn`
// computes, in the same order, and prints the same bytes.
//
//   $ rustc -C opt-level=3 -o regexredux.exe regexredux.rs
//   $ ./regexredux < rfcs/bench-0104/fasta-1000.expected
//
// Safe Rust, std only — which ships no regex engine, so this leg carries the
// same smallest-that-covers-the-corpus matcher the C leg does: literal bytes
// (with `\x` escapes), character classes with optional negation and no
// ranges, top-level alternation, and a postfix `*` on a class. Every starred
// class in the corpus EXCLUDES the byte that follows it, so maximal munch is
// exact and no backtracking exists to differ from the other legs' engines;
// every same-position branch pair matches at equal length, so
// first-branch-wins and leftmost-longest agree on this input.

use std::io::Read;

/// One element of a branch: a set of admitted bytes, possibly starred.
struct Item {
    admits: [bool; 256],
    star: bool,
}

struct Pattern {
    branches: Vec<Vec<Item>>,
}

/// `\x` outside a class: the escapes the corpus spells.
fn unescape(c: u8) -> u8 {
    if c == b'n' {
        b'\n'
    } else {
        c
    }
}

/// Compile `src`, or panic: a pattern here is a literal of this file, so a
/// failure is a bug in this program and never a bad input.
fn compile(src: &str) -> Pattern {
    let bytes = src.as_bytes();
    let mut branches: Vec<Vec<Item>> = vec![Vec::new()];
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'|' {
            branches.push(Vec::new());
            i += 1;
            continue;
        }
        let mut admits = [false; 256];
        if c == b'[' {
            i += 1;
            let negate = bytes[i] == b'^';
            if negate {
                i += 1;
            }
            while bytes[i] != b']' {
                let mut m = bytes[i];
                if m == b'\\' {
                    i += 1;
                    m = unescape(bytes[i]);
                }
                admits[m as usize] = true;
                i += 1;
            }
            if negate {
                for a in admits.iter_mut() {
                    *a = !*a;
                }
            }
        } else if c == b'\\' {
            i += 1;
            admits[unescape(bytes[i]) as usize] = true;
        } else {
            admits[c as usize] = true;
        }
        i += 1;
        let star = i < bytes.len() && bytes[i] == b'*';
        if star {
            i += 1;
        }
        branches.last_mut().unwrap().push(Item { admits, star });
    }
    Pattern { branches }
}

/// The length of the match at `text[at..]`, or None. Branches in order; a
/// starred item takes everything its class admits (see the header comment
/// for why that is exact here).
fn match_at(p: &Pattern, text: &[u8], at: usize) -> Option<usize> {
    'branches: for b in &p.branches {
        let mut pos = at;
        for it in b {
            if it.star {
                while pos < text.len() && it.admits[text[pos] as usize] {
                    pos += 1;
                }
            } else if pos < text.len() && it.admits[text[pos] as usize] {
                pos += 1;
            } else {
                continue 'branches;
            }
        }
        return Some(pos - at);
    }
    None
}

/// Non-overlapping matches, left to right — the count the game prints.
fn count_matches(p: &Pattern, text: &[u8]) -> usize {
    let mut count = 0;
    let mut i = 0;
    while i < text.len() {
        match match_at(p, text, i) {
            Some(m) if m > 0 => {
                count += 1;
                i += m;
            }
            _ => i += 1,
        }
    }
    count
}

/// Every match replaced with `to`, into a fresh buffer.
fn replace_all(p: &Pattern, text: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        match match_at(p, text, i) {
            Some(m) if m > 0 => {
                out.extend_from_slice(to);
                i += m;
            }
            _ => {
                out.push(text[i]);
                i += 1;
            }
        }
    }
    out
}

fn main() {
    // The whole of standard input, linefeeds and all: the game counts the
    // INPUT length including the description lines. Read as bytes and folded
    // CRLF -> LF by hand, the same fold the harness applies to every leg's
    // output and the one the C runtime's text mode does for the C leg.
    let mut raw = Vec::new();
    std::io::stdin().read_to_end(&mut raw).unwrap();
    let input: Vec<u8> = {
        let mut out = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == b'\r' && i + 1 < raw.len() && raw[i + 1] == b'\n' {
                i += 1;
            }
            out.push(raw[i]);
            i += 1;
        }
        out
    };
    let input_length = input.len();

    // Remove the FASTA descriptions and every linefeed.
    let clean = compile(">[^\\n]*\\n|\\n");
    let sequence = replace_all(&clean, &input, b"");
    let clean_length = sequence.len();

    let variants = [
        "agggtaaa|tttaccct",
        "[cgt]gggtaaa|tttaccc[acg]",
        "a[act]ggtaaa|tttacc[agt]t",
        "ag[act]gtaaa|tttac[agt]ct",
        "agg[act]taaa|ttta[agt]cct",
        "aggg[acg]aaa|ttt[cgt]ccct",
        "agggt[cgt]aa|tt[acg]accct",
        "agggta[cgt]a|t[acg]taccct",
        "agggtaa[cgt]|[acg]ttaccct",
    ];
    let mut lines = String::new();
    for pattern in variants {
        let p = compile(pattern);
        lines.push_str(&format!("{} {}\n", pattern, count_matches(&p, &sequence)));
    }

    // The five rewrites, each over the result of the last.
    let substitutions: [(&str, &[u8]); 5] = [
        ("tHa[Nt]", b"<4>"),
        ("aND|caN|Ha[DS]|WaS", b"<3>"),
        ("a[NSt]|BY", b"<2>"),
        ("<[^>]*>", b"|"),
        ("\\|[^|][^|]*\\|", b"-"),
    ];
    let mut rewritten = sequence;
    for (pat, to) in substitutions {
        let p = compile(pat);
        rewritten = replace_all(&p, &rewritten, to);
    }

    lines.push_str(&format!("\n{}\n{}\n{}\n", input_length, clean_length, rewritten.len()));
    print!("{}", lines);
}
