//! The grammar gate: `compiler/vyrn-syntax/vyrn.syntax` specifies the hand parser,
//! and these tests hold it equal to the recognizer written from the grammar over
//! every corpus root, the `.vyrn` files under `examples/`, `std/`, `site/` and
//! `compiler/vyrn-cli/tests/`. The lexer's scanner is written from the grammar too,
//! and both read its tokens, so a file that does not lex has one diagnostic and no
//! comparison.
//!
//! - Diagnostics: the recognizer's list equals the hand list, line, column and
//!   sentence. A hand list that holds a builder row (a `row` line of the grammar)
//!   is compared up to that row, because the grammar does not state it and the
//!   parser's recovery differs after it.
//! - Shape: a file the hand parser accepts, the recognizer accepts, and each
//!   declaration sits at the same kind and line in both.
//! - Rows: the rows the grammar names equal the lex and parse rows: the rows of
//!   `rules.rs` that `lexer.rs`, its scanner `lexer/grammar.rs` or `parser.rs`
//!   names outside its tests.
//!
//! A deep program recurses through many rule frames, so each test runs on a
//! thread with a large stack.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use vyrn_frontend::diagnostics::Diagnostic;
use vyrn_frontend::recognizer::{recognize, Decl, Mark};
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

type Said = (usize, usize, String);

/// What the hand parser and the recognizer say over the tokens of `src`, or
/// `None` when it does not lex.
fn both_lists(src: &str) -> Option<(Vec<Diagnostic>, Vec<Diagnostic>)> {
    let toks = lexer::lex(src).ok()?;
    let said = recognize(&toks).errors;
    Some((parser::parse_accum(toks).1, said))
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

/// Fails with the first `shown` lines of `diffs` and their count.
fn verdict(what: &str, diffs: Vec<String>, files: usize) {
    if diffs.is_empty() {
        println!("{what}: {files} files equal");
        return;
    }
    let shown: Vec<&String> = diffs.iter().take(12).collect();
    let lines: Vec<&str> = shown.iter().map(|s| s.as_str()).collect();
    panic!(
        "{what}: {} of {files} files differ\n{}",
        diffs.len(),
        lines.join("\n")
    );
}

#[test]
#[ignore]
fn the_grammar_refuses_as_the_parser_does() {
    deep(|| {
        let g = grammar();
        let builder: BTreeSet<&str> = g.builder_rows().collect();
        let corpus = corpus();
        let mut diffs = Vec::new();
        for (name, src) in &corpus {
            let Some((hand, said)) = both_lists(src) else {
                continue;
            };
            let cut = (hand.iter())
                .position(|d| d.rule.as_ref().is_some_and(|r| builder.contains(r.name())));
            let said_of = |ds: Vec<Diagnostic>| -> Vec<Said> {
                ds.into_iter().map(|d| (d.line, d.col, d.message)).collect()
            };
            let (hand, said) = (said_of(hand), said_of(said));
            let same = match cut {
                Some(k) => said.len() >= k && hand[..k] == said[..k],
                None => hand == said,
            };
            if !same {
                let one = |l: &[Said]| match l.first() {
                    Some((line, col, m)) => {
                        format!("{line}:{col}: {}", m.lines().next().unwrap_or(""))
                    }
                    None => "nothing".into(),
                };
                diffs.push(format!(
                    "{name} ({} hand, {} recognizer)\n  hand       {}\n  recognizer {}",
                    hand.len(),
                    said.len(),
                    one(&hand[..cut.unwrap_or(hand.len())]),
                    one(&said)
                ));
            }
        }
        verdict("diagnostics", diffs, corpus.len());
    })
}

/// The declarations of a file the hand parser accepts, by kind and line, sorted.
/// The prelude's types and the declarations the parser makes up (a `$` or `.` in
/// the name) are not the file's.
fn hand_decls(toks: Vec<lexer::Token>, prelude: &BTreeSet<String>) -> Option<Vec<(Decl, usize)>> {
    let (p, errors) = parser::parse_accum(toks);
    if !errors.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for f in p.functions.iter().filter(|f| !f.name.contains('$')) {
        out.push((
            if f.is_extern || f.is_export_extern {
                Decl::Extern
            } else {
                Decl::FnDecl
            },
            f.line,
        ));
    }
    for t in &p.type_decls {
        if !t.name.contains('.') && !prelude.contains(&t.name) {
            out.push((Decl::TypeDecl, t.line));
        }
    }
    out.extend(p.globals.iter().map(|x| (Decl::Global, x.line)));
    out.extend(p.imports.iter().map(|x| (Decl::Import, x.line)));
    out.extend(p.protocols.iter().map(|x| (Decl::Protocol, x.line)));
    out.extend(p.contracts.iter().map(|x| (Decl::Contract, x.line)));
    out.extend(p.tests.iter().map(|x| (Decl::Test, x.line)));
    out.extend(p.benches.iter().map(|x| (Decl::Bench, x.line)));
    out.sort();
    Some(out)
}

#[test]
#[ignore]
fn the_grammar_accepts_what_the_parser_accepts_in_its_shape() {
    deep(|| {
        let empty = lexer::lex("").expect("an empty file lexes");
        let prelude: BTreeSet<String> = (parser::parse_accum(empty).0.type_decls.into_iter())
            .map(|t| t.name)
            .collect();
        let (mut files, mut decls, mut diffs) = (0, 0, Vec::new());
        for (name, src) in &corpus() {
            let Ok(toks) = lexer::lex(src) else {
                continue;
            };
            let found = recognize(&toks);
            let mut tree: Vec<(Decl, usize)> = (found.marks.iter())
                .filter_map(|m| match m {
                    Mark::Decl(kind, at) => Some((*kind, toks[*at].line)),
                    Mark::Word(_) => None,
                })
                .collect();
            let Some(hand) = hand_decls(toks, &prelude) else {
                continue;
            };
            files += 1;
            decls += hand.len();
            tree.sort();
            if !found.errors.is_empty() || hand != tree {
                let first = hand.iter().zip(&tree).find(|(a, b)| a != b);
                diffs.push(format!(
                    "{name}: {} errors, {} hand, {} recognizer, first {first:?}",
                    found.errors.len(),
                    hand.len(),
                    tree.len()
                ));
            }
        }
        println!("shape: {decls} declarations");
        verdict("shape", diffs, files);
    })
}

#[test]
#[ignore]
fn every_row_the_grammar_names_has_a_witness() {
    deep(|| {
        let g = grammar();
        let mut seen = BTreeSet::new();
        for (_, src) in &corpus() {
            let hand = match lexer::lex(src) {
                Err(d) => vec![d],
                Ok(toks) => parser::parse_accum(toks).1,
            };
            seen.extend(hand.iter().filter_map(|d| d.rule.as_ref().map(Rule::name)));
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
    let mut rows = rows_named_in("lexer.rs");
    rows.extend(rows_named_in("lexer/grammar.rs"));
    rows.extend(rows_named_in("parser.rs"));
    let named: BTreeSet<&str> = g.stated_rows().chain(g.builder_rows()).collect();
    let rows: BTreeSet<&str> = rows.into_iter().collect();
    let missing: Vec<&&str> = rows.difference(&named).collect();
    let extra: Vec<&&str> = named.difference(&rows).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "rows the grammar does not name: {missing:?}; rows it names that the lexer and parser do not: {extra:?}"
    );
    println!(
        "rows: {} stated, {} builder",
        g.stated_rows().count(),
        g.builder_rows().count()
    );
}
