//! The grammar gate: `compiler/vyrn-syntax/vyrn.syntax` specifies the lexer and
//! the hand parser, and these tests hold them equal to the grammar's interpreter
//! over every corpus root, the `.vyrn` files under `examples/`, `std/`, `site/`
//! and `compiler/vyrn-cli/tests/`. The lexer's scanner is written from the
//! grammar, so its decoder and the generator are what the lexer side compares.
//!
//! - Diagnostics: the grammar's list equals the hand list, line, column and
//!   sentence. A hand list that holds a builder row (a `row` line of the grammar)
//!   is compared up to that row, because the grammar does not state it and the
//!   parser's recovery differs after it.
//! - Shape: a file the hand parser accepts, the grammar accepts, and each
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
use vyrn_frontend::rules::Rule;
use vyrn_frontend::{lexer, parser};
use vyrn_syntax::{Diag, Grammar, HoleValue, NodeKind, Stage};

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

/// The hand lexer's `Debug` of the first token of `text`: what `found` prints.
fn found(text: &str) -> String {
    match lexer::lex(text) {
        Ok(toks) => toks
            .first()
            .map_or_else(String::new, |t| format!("{:?}", t.tok)),
        Err(_) => "?".into(),
    }
}

/// The sentence of a grammar refusal, rendered by its row of `rules.rs`.
fn render(d: &Diag) -> String {
    let hole = |name: &str| {
        let (_, v) = d.holes.iter().find(|h| &*h.0 == name)?;
        Some(match v {
            HoleValue::Text(t) => t.clone(),
            // A hole re-lexes before it parses; the lexer's sentence names its line.
            HoleValue::Diag(inner) if inner.stage == Stage::Scan => {
                format!("line {}: {}", inner.line, render(inner))
            }
            HoleValue::Diag(inner) => render(inner),
        })
    };
    match Rule::from_text(&d.row, hole) {
        Some(rule) => rule.render(),
        None => format!("<row {} with holes {:?}>", d.row, d.holes),
    }
}

type Said = (usize, usize, String);

fn hand_list(src: &str) -> Vec<Diagnostic> {
    match lexer::lex(src) {
        Err(d) => vec![d],
        Ok(toks) => parser::parse_accum(toks).1,
    }
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
        let module = g.rule("Module").expect("vyrn.syntax names Module");
        let builder: BTreeSet<&str> = g.builder_rows().collect();
        let corpus = corpus();
        let mut diffs = Vec::new();
        for (name, src) in &corpus {
            let hand = hand_list(src);
            let cut = (hand.iter())
                .position(|d| d.rule.as_ref().is_some_and(|r| builder.contains(r.name())));
            let hand: Vec<Said> = hand
                .into_iter()
                .map(|d| (d.line, d.col, d.message))
                .collect();
            let errors = vyrn_syntax::parse(&g, module, src, &found).errors;
            let said: Vec<Said> = errors.iter().map(|d| (d.line, d.col, render(d))).collect();
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
                    "{name} ({} hand, {} grammar)\n  hand    {}\n  grammar {}",
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

/// The declarations of a file the hand parser accepts, by grammar rule name
/// and line, sorted. The prelude's types and the declarations the parser
/// makes up (a `$` or `.` in the name) are not the file's.
fn hand_decls(src: &str, prelude: &BTreeSet<String>) -> Option<Vec<(&'static str, usize)>> {
    let (p, errors) = parser::parse_accum(lexer::lex(src).ok()?);
    if !errors.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for f in p.functions.iter().filter(|f| !f.name.contains('$')) {
        out.push((
            if f.is_extern || f.is_export_extern {
                "Extern"
            } else {
                "FnDecl"
            },
            f.line,
        ));
    }
    for t in &p.type_decls {
        if !t.name.contains('.') && !prelude.contains(&t.name) {
            out.push(("TypeDecl", t.line));
        }
    }
    out.extend(p.globals.iter().map(|x| ("Global", x.line)));
    out.extend(p.imports.iter().map(|x| ("Import", x.line)));
    out.extend(p.protocols.iter().map(|x| ("Protocol", x.line)));
    out.extend(p.contracts.iter().map(|x| ("Contract", x.line)));
    out.extend(p.tests.iter().map(|x| ("Test", x.line)));
    out.extend(p.benches.iter().map(|x| ("Bench", x.line)));
    out.sort();
    Some(out)
}

#[test]
#[ignore]
fn the_grammar_accepts_what_the_parser_accepts_in_its_shape() {
    deep(|| {
        let g = grammar();
        let module = g.rule("Module").expect("vyrn.syntax names Module");
        let kinds = [
            "FnDecl", "Extern", "TypeDecl", "Global", "Import", "Protocol", "Contract", "Test",
            "Bench",
        ];
        let empty = lexer::lex("").expect("an empty file lexes");
        let prelude: BTreeSet<String> = (parser::parse_accum(empty).0.type_decls.into_iter())
            .map(|t| t.name)
            .collect();
        let (mut files, mut decls, mut diffs) = (0, 0, Vec::new());
        for (name, src) in &corpus() {
            let Some(hand) = hand_decls(src, &prelude) else {
                continue;
            };
            files += 1;
            decls += hand.len();
            let parse = vyrn_syntax::parse(&g, module, src, &found);
            let mut tree: Vec<(&'static str, usize)> = Vec::new();
            for n in &parse.nodes {
                let NodeKind::Rule(r) = n.kind else { continue };
                if let Some(k) = kinds.iter().find(|k| **k == g.rule_name(r)) {
                    tree.push((k, parse.line(n.start)));
                }
            }
            tree.sort();
            if !parse.errors.is_empty() || hand != tree {
                let first = hand.iter().zip(&tree).find(|(a, b)| a != b);
                diffs.push(format!(
                    "{name}: {} errors, {} hand, {} grammar, first {first:?}",
                    parse.errors.len(),
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
            seen.extend(
                hand_list(src)
                    .iter()
                    .filter_map(|d| d.rule.as_ref().map(Rule::name)),
            );
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
