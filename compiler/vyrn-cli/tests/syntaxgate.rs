//! The grammar gate: `compiler/vyrn-syntax/vyrn.syntax` is read by the recognizer
//! written from it, and these tests hold what that says over every corpus root, the
//! `.vyrn` files under `examples/`, `std/`, `site/` and `compiler/vyrn-cli/tests/`.
//!
//! - Pin: `tests/pins/parse-diagnostics.tsv` holds what the parser says over each root
//!   and its mutants, as a count and a hash per root. `VYRN_PIN=write` rewrites it.
//! - Rows: the rows the grammar names equal the lex and parse rows: the rows of
//!   `rules.rs` that the lexer, its scanner, the recognizer or the builder names
//!   outside its tests.
//!
//! A deep program recurses through many rule frames, so each test runs on a
//! thread with a large stack.

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use vyrn_frontend::diagnostics::Diagnostic;
use vyrn_frontend::rules::Rule;
use vyrn_frontend::{lexer, parser};
use vyrn_syntax::Grammar;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn grammar() -> Grammar {
    let path = repo_root().join("compiler/vyrn-syntax/vyrn.syntax");
    let text = std::fs::read_to_string(path).expect("vyrn.syntax reads");
    vyrn_syntax::load(&text).expect("vyrn.syntax loads")
}

/// Every `.vyrn` file under `dir`, recursively, but a build's `target/`.
fn roots_in(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).expect("a corpus directory reads") {
        let p = e.expect("an entry reads").path();
        if p.is_dir() && !p.ends_with("target") {
            roots_in(&p, out);
        } else if p.extension().is_some_and(|x| x == "vyrn") {
            out.push(p);
        }
    }
}

/// The corpus roots, sorted, with their text and their path from the root.
fn corpus() -> Vec<(String, String)> {
    let root = repo_root();
    let mut paths = Vec::new();
    for dir in ["examples", "std", "site", "compiler/vyrn-cli/tests"] {
        roots_in(&root.join(dir), &mut paths);
    }
    paths.sort();
    (paths.into_iter())
        .map(|p| {
            let text = std::fs::read_to_string(&p).expect("a root reads");
            let name = p
                .strip_prefix(&root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            (name, text)
        })
        .collect()
}

/// Runs `f` on a thread with a stack deep enough for the deepest witness.
fn deep<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let child = (std::thread::Builder::new())
        .stack_size(vyrn_frontend::trap::DEEP_STACK_BYTES)
        .spawn(f);
    child
        .expect("the thread starts")
        .join()
        .expect("the thread ends")
}

/// The mutants of each root, a fixed sequence from a fixed seed: one token removed,
/// doubled, replaced by another or moved, or another token's copy put after it.
const MUTANTS_PER_ROOT: usize = 20;

/// A xorshift generator, so a mutant's text depends only on the root and its place
/// in the sequence.
struct Seq(u64);

impl Seq {
    fn next(&mut self) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 as usize
    }
}

/// The mutants of `src` that still lex.
fn mutants(src: &str) -> Vec<String> {
    let Ok(items) = lexer::lex_with_trivia(src) else {
        return Vec::new();
    };
    let spans: Vec<_> = (items.into_iter())
        .filter(|i| !matches!(i.kind, lexer::TrivKind::Comment))
        .map(|i| i.span)
        .collect();
    if spans.is_empty() {
        return Vec::new();
    }
    let mut seq = Seq(0x9E37_79B9_7F4A_7C15);
    (0..MUTANTS_PER_ROOT)
        .map(|n| {
            let at = spans[seq.next() % spans.len()].clone();
            let other = spans[seq.next() % spans.len()].clone();
            let (word, rest) = (&src[at.clone()], &src[other.clone()]);
            match n % 5 {
                0 => format!("{}{}", &src[..at.start], &src[at.end..]),
                1 => format!("{} {word} {}", &src[..at.end], &src[at.end..]),
                2 => format!("{} {rest} {}", &src[..at.start], &src[at.end..]),
                3 => format!("{} {rest} {}", &src[..at.end], &src[at.end..]),
                _ if other.start >= at.end => format!(
                    "{}{} {word} {}",
                    &src[..at.start],
                    &src[at.end..other.start],
                    &src[other.start..]
                ),
                _ => format!(
                    "{} {word} {}{}",
                    &src[..other.start],
                    &src[other.start..at.start],
                    &src[at.end..]
                ),
            }
        })
        .filter(|m| lexer::lex(m).is_ok())
        .collect()
}

/// An FNV-1a hash over the line, column and sentence of each diagnostic given it.
struct Heard(u64);

impl Heard {
    fn new() -> Heard {
        Heard(0xCBF2_9CE4_8422_2325)
    }

    fn hear(&mut self, ds: &[Diagnostic]) {
        for d in ds {
            let said = format!("{}:{}: {}\n", d.line, d.col, d.message);
            for byte in said.bytes() {
                self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01B3);
            }
        }
    }
}

#[test]
#[ignore]
fn the_parser_says_what_the_pin_holds() {
    deep(|| {
        let rows = corpus().into_iter().map(|(name, src)| {
            let own = match lexer::lex(&src) {
                Err(d) => vec![d],
                Ok(toks) => parser::parse_accum(toks).1,
            };
            let mut own_hash = Heard::new();
            own_hash.hear(&own);
            let (mut runs, mut refused, mut hash) = (0, 0, Heard::new());
            for mutant in mutants(&src) {
                let Ok(toks) = lexer::lex(&mutant) else {
                    continue;
                };
                let said = parser::parse_accum(toks).1;
                runs += 1;
                refused += !said.is_empty() as usize;
                hash.hear(&said);
            }
            format!(
                "{name}\t{}\t{:016x}\t{runs}\t{refused}\t{:016x}",
                own.len(),
                own_hash.0,
                hash.0
            )
        });
        common::pin(
            "parse-diagnostics",
            "root\tsaid\thash\tmutants\trefused\thash",
            rows,
        );
    })
}

#[test]
#[ignore]
fn every_row_the_grammar_names_has_a_witness() {
    deep(|| {
        let g = grammar();
        let mut seen = BTreeSet::new();
        for (_, src) in &corpus() {
            let said = match lexer::lex(src) {
                Err(d) => vec![d],
                Ok(toks) => parser::parse_accum(toks).1,
            };
            seen.extend(said.iter().filter_map(|d| d.rule.as_ref().map(Rule::name)));
        }
        let unwitnessed: Vec<&str> = (g.stated_rows().chain(g.builder_rows()))
            .filter(|r| !seen.contains(r))
            .collect();
        assert!(
            unwitnessed.is_empty(),
            "rows no corpus root refuses with: {unwitnessed:?}"
        );
    })
}

/// The rows of `rules.rs` that `file` names before its tests.
fn rows_named_in(file: &str) -> BTreeSet<&'static str> {
    let path = repo_root().join("compiler/vyrn-frontend/src").join(file);
    let text = std::fs::read_to_string(path).expect("the source reads");
    let code = text.split("#[cfg(test)]").next().unwrap_or("");
    let words: BTreeSet<&str> = code
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .collect();
    Rule::ROWS
        .iter()
        .copied()
        .filter(|r| words.contains(r))
        .collect()
}

#[test]
fn the_grammar_names_every_lex_and_parse_row() {
    let g = grammar();
    let mut rows = BTreeSet::new();
    for file in [
        "lexer.rs",
        "lexer/grammar.rs",
        "recognizer.rs",
        "recognizer/rules.rs",
        "parser.rs",
    ] {
        rows.extend(rows_named_in(file));
    }
    let named: BTreeSet<&str> = g.stated_rows().chain(g.builder_rows()).collect();
    let rows: BTreeSet<&str> = rows.into_iter().collect();
    let missing: Vec<&&str> = rows.difference(&named).collect();
    let extra: Vec<&&str> = named.difference(&rows).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "rows the grammar does not name: {missing:?}; rows it names that no reader names: {extra:?}"
    );
    println!(
        "rows: {} stated, {} builder",
        g.stated_rows().count(),
        g.builder_rows().count()
    );
}
