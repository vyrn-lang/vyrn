//! The surface census — RFC-0126, and RFC-0125 §2.8's deferred factor.
//!
//! RFC-0125 §1.1 says the size is `(surface × types × builtins × engines)`. The
//! `types` factor is `ast::Type`'s constructors, and §2.8 named three candidate
//! collapses without measuring them. RFC-0126 measures all of them: one row per
//! constructor, its cost in the seven files that decide, and a verdict.
//!
//! # What this pins, and why a test rather than prose
//!
//! `tests/boundaries.rs`, the coercion census in `tests/lowered.rs` and the
//! structural census in `tests/refusals.rs` each hold their table this way, for
//! the same reason: a table in prose beside a table in code is two tables, and
//! the one in prose is the one that drifts.
//!
//! Three facts are checked:
//!
//! 1. [`the_census_covers_every_constructor`] reads `ast::Type`'s variants out
//!    of `ast.rs` and asserts the RFC's verdict table lists exactly those, in
//!    the declaration order. A new constructor therefore fails this test until
//!    it has a verdict.
//! 2. [`the_surface_census_is_what_the_rfc_records`] recomputes every count by
//!    the method RFC-0126 §2 states and pins the cost table, one row per
//!    constructor and a total, in `tests/pins/surface.tsv`.
//! 3. [`the_verdicts_are_from_the_closed_set`] holds §4's vocabulary to three
//!    words and asserts the sentence that tallies them.
//!
//! # The method (RFC-0126 §2), stated once and applied here
//!
//! A line whose trimmed text starts with `//` is not code. An item annotated
//! `#[cfg(test)]` is skipped whole. In what is left, `Type::<Name>` counts where
//! the next character is not a letter, digit or underscore — which is what keeps
//! `Type::Int` from counting `Type::IntN`.

mod common;

use std::path::{Path, PathBuf};

/// The files whose columns the census carries, in the RFC's order.
///
/// There were seven. `interp` went with `interp.rs` (RFC-0125 §3 M5), and every
/// number in the RFC's cost table moved with it — which is what a census is for:
/// the sentence "the surface costs this much" is smaller by one engine, and the
/// table says by how much.
///
/// `shared` was `native` for the same reason. `vyrn-codegen/src/lib.rs` was the
/// text-IR emitter AND the lowering both emitters call; RFC-0125 §3 M4's fourth
/// slice deleted the emitter and left the lowering, so the column measures the
/// shared statement now and its numbers fell with the route.
const COLUMNS: &[(&str, &str)] = &[
    ("checker", "vyrn-frontend/src/checker.rs"),
    ("shared", "vyrn-codegen/src/lib.rs"),
    ("wasm", "vyrn-codegen/src/direct.rs"),
    ("types", "vyrn-frontend/src/types.rs"),
    ("prelude", "vyrn-frontend/src/prelude.rs"),
    ("editor", "vyrn-frontend/src/symbols.rs"),
];

/// The verdict vocabulary of RFC-0126 §4. Three words, and no fourth.
const VERDICTS: &[&str] = &["stays", "desugar", "decide"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn compiler_file(rel: &str) -> String {
    let p = repo_root().join("compiler").join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn rfc_text() -> String {
    let p = repo_root()
        .join("rfcs")
        .join("RFC-0126-a-type-constructor-is-a-case-in-every-pass.md");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The file with its comment lines and its `#[cfg(test)]` items removed.
///
/// A doc comment that names a constructor is not a case, and a fixture in a test
/// module is not a pass. Both would otherwise inflate the small rows most: the
/// transformers are named more often in prose than in code.
fn code_only(src: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let t = lines[i].trim_start();
        if t.starts_with("#[cfg(test)]") {
            // Skip to the line that closes the first brace opened at or after
            // the attribute — the annotated `mod` or `fn`, whichever it is.
            let mut depth = 0i32;
            let mut open = false;
            let mut j = i;
            while j < lines.len() {
                for ch in lines[j].chars() {
                    if ch == '{' {
                        depth += 1;
                        open = true;
                    } else if ch == '}' {
                        depth -= 1;
                    }
                }
                if open && depth <= 0 {
                    break;
                }
                j += 1;
            }
            i = j + 1;
            continue;
        }
        if !t.starts_with("//") {
            out.push(lines[i]);
        }
        i += 1;
    }
    out.join("\n")
}

/// How many times `code` names `Type::<name>` as that constructor.
///
/// The trailing character must not continue the identifier, or `Type::Int`
/// would count every `Type::IntN` and `Type::Array` every `Type::ArrayN`.
fn mentions(code: &str, name: &str) -> usize {
    let needle = format!("Type::{name}");
    let bytes = code.as_bytes();
    let mut n = 0usize;
    let mut from = 0usize;
    while let Some(at) = code[from..].find(&needle) {
        let end = from + at + needle.len();
        let ok = match bytes.get(end) {
            None => true,
            Some(c) => !(c.is_ascii_alphanumeric() || *c == b'_'),
        };
        if ok {
            n += 1;
        }
        from = end;
    }
    n
}

/// `ast::Type`'s variants, in declaration order, read out of `ast.rs`.
///
/// The names are the four-space-indented capitalised identifiers of the enum
/// body — the same shape every variant in that enum has.
fn constructors() -> Vec<String> {
    let src = compiler_file("vyrn-frontend/src/ast.rs");
    let start = src
        .find("pub enum Type {")
        .expect("`pub enum Type` in ast.rs");
    let body = &src[start..];
    let end = body.find("\n}\n").expect("the end of `pub enum Type`");
    let mut out = Vec::new();
    for line in body[..end].lines() {
        let Some(rest) = line.strip_prefix("    ") else {
            continue;
        };
        if rest.starts_with(' ') || rest.starts_with("//") {
            continue;
        }
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if !name.is_empty() && name.starts_with(|c: char| c.is_ascii_uppercase()) {
            out.push(name);
        }
    }
    assert!(out.len() > 20, "only {} variants found", out.len());
    out
}

/// The rows of a markdown table, found by its header line and read to the first
/// line that is not a row. Each row is its trimmed cells.
fn table(text: &str, header: &str) -> Vec<Vec<String>> {
    let start = text
        .find(header)
        .unwrap_or_else(|| panic!("RFC-0126 has no table with header {header:?}"));
    let mut rows = Vec::new();
    for line in text[start..].lines().skip(2) {
        if !line.starts_with("| `Type::") {
            break;
        }
        rows.push(
            line.trim_matches('|')
                .split('|')
                .map(|c| c.trim().to_string())
                .collect(),
        );
    }
    rows
}

const VERDICT_HEADER: &str =
    "| constructor | what it is | RFC | verdict | the desugar, or the reason |";

/// The verdict table lists every constructor `ast.rs` declares, in that order.
///
/// This is the anti-drift half: a constructor added to the surface has no
/// verdict until somebody writes one, and this test says so by name.
#[test]
fn the_census_covers_every_constructor() {
    let text = rfc_text();
    let want: Vec<String> = constructors()
        .iter()
        .map(|c| format!("`Type::{c}`"))
        .collect();
    let got: Vec<String> = table(&text, VERDICT_HEADER)
        .iter()
        .map(|r| r[0].clone())
        .collect();
    assert_eq!(
        got, want,
        "RFC-0126's verdicts and `ast::Type` list different constructors"
    );
    assert!(
        text.contains(&format!("{} constructors", want.len())),
        "the prose should say {} constructors",
        want.len()
    );
}

/// RFC-0126 §3's cost table, one row per constructor and a total, pinned in
/// `tests/pins/surface.tsv`.
#[test]
fn the_surface_census_is_what_the_rfc_records() {
    let code: Vec<String> = COLUMNS
        .iter()
        .map(|(_, f)| code_only(&compiler_file(f)))
        .collect();
    let mut totals = vec![0usize; COLUMNS.len() + 1];
    let mut rows: Vec<String> = Vec::new();
    for name in constructors() {
        let mut counts: Vec<usize> = code.iter().map(|c| mentions(c, &name)).collect();
        counts.push(counts.iter().sum());
        for (t, n) in totals.iter_mut().zip(&counts) {
            *t += n;
        }
        rows.push(common::pin_row(&format!("Type::{name}"), &counts));
    }
    rows.push(common::pin_row("all", &totals));
    let header: Vec<&str> = COLUMNS.iter().map(|(c, _)| *c).collect();
    common::pin(
        "surface",
        &format!("constructor\t{}\tall", header.join("\t")),
        rows,
    );
}

/// §4's verdict column is three words, and the tally sentence counts them.
#[test]
fn the_verdicts_are_from_the_closed_set() {
    let text = rfc_text();
    let rows = table(&text, VERDICT_HEADER);
    let mut tally = [0usize; 3];
    for row in &rows {
        assert_eq!(
            row.len(),
            5,
            "a verdict row has {} cells: {row:?}",
            row.len()
        );
        let at = VERDICTS
            .iter()
            .position(|v| *v == row[3])
            .unwrap_or_else(|| {
                panic!(
                    "{}: verdict {:?} is not one of {VERDICTS:?}",
                    row[0], row[3]
                )
            });
        tally[at] += 1;
        assert!(
            !row[4].is_empty(),
            "{}: a verdict without a reason is a guess",
            row[0]
        );
    }
    let sentence = format!(
        "Of the {} rows, {} say `desugar`, {} say `decide` and {} say `stays`.",
        rows.len(),
        tally[1],
        tally[2],
        tally[0]
    );
    assert!(
        text.contains(&sentence),
        "the prose should say {sentence:?}"
    );
}
