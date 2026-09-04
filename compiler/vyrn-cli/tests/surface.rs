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
//!    of `ast.rs` and asserts the RFC's two tables list exactly those, in the
//!    declaration order. A new constructor therefore fails this test until it
//!    has a cost row and a verdict.
//! 2. [`the_surface_census_is_what_the_rfc_records`] recomputes every count by
//!    the method RFC-0126 §2 states and asserts the table's numbers, its `all
//!    seven` column and the total in the prose.
//! 3. [`the_verdicts_are_from_the_closed_set`] holds §4's vocabulary to three
//!    words and asserts the sentence that tallies them.
//!
//! # The method (RFC-0126 §2), stated once and applied here
//!
//! A line whose trimmed text starts with `//` is not code. An item annotated
//! `#[cfg(test)]` is skipped whole. In what is left, `Type::<Name>` counts where
//! the next character is not a letter, digit or underscore — which is what keeps
//! `Type::Int` from counting `Type::IntN`.

use std::path::{Path, PathBuf};

/// The seven files whose columns the census carries, in the RFC's order.
const COLUMNS: &[(&str, &str)] = &[
    ("checker", "vyrn-frontend/src/checker.rs"),
    ("interp", "vyrn-frontend/src/interp.rs"),
    ("native", "vyrn-codegen/src/lib.rs"),
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

/// A count as the RFC's prose writes one: thousands separated by a comma.
fn grouped(n: usize) -> String {
    let d = n.to_string();
    let mut out = String::new();
    for (i, c) in d.chars().enumerate() {
        if i > 0 && (d.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
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

const COST_HEADER: &str =
    "| constructor | checker | interp | native | wasm | types | prelude | editor | all seven |";
const VERDICT_HEADER: &str =
    "| constructor | what it is | RFC | verdict | the desugar, or the reason |";

/// Both tables list every constructor `ast.rs` declares, in that order.
///
/// This is the anti-drift half: a constructor added to the surface has no cost
/// row and no verdict until somebody writes one, and this test says so by name.
#[test]
fn the_census_covers_every_constructor() {
    let text = rfc_text();
    let want: Vec<String> = constructors()
        .iter()
        .map(|c| format!("`Type::{c}`"))
        .collect();
    for (header, which) in [
        (COST_HEADER, "the cost table"),
        (VERDICT_HEADER, "the verdicts"),
    ] {
        let got: Vec<String> = table(&text, header).iter().map(|r| r[0].clone()).collect();
        assert_eq!(
            got, want,
            "RFC-0126 {which} and `ast::Type` list different constructors"
        );
    }
    assert!(
        text.contains(&format!("{} constructors", want.len())),
        "the prose should say {} constructors",
        want.len()
    );
}

/// Every number in the cost table is what the code says today.
#[test]
fn the_surface_census_is_what_the_rfc_records() {
    let text = rfc_text();
    let code: Vec<String> = COLUMNS
        .iter()
        .map(|(_, f)| code_only(&compiler_file(f)))
        .collect();
    let rows = table(&text, COST_HEADER);
    let mut total = 0usize;
    let mut wrong: Vec<String> = Vec::new();
    for row in &rows {
        assert_eq!(row.len(), 9, "a cost row has {} cells: {row:?}", row.len());
        let name = row[0].trim_matches('`').trim_start_matches("Type::");
        let mut sum = 0usize;
        for (k, (col, _)) in COLUMNS.iter().enumerate() {
            let got = mentions(&code[k], name);
            let want: usize = row[k + 1].parse().expect("a count");
            if got != want {
                wrong.push(format!(
                    "Type::{name} in {col}: code says {got}, RFC says {want}"
                ));
            }
            sum += got;
        }
        let want_sum: usize = row[8].parse().expect("the row total");
        if sum != want_sum {
            wrong.push(format!(
                "Type::{name}: the row sums to {sum}, RFC says {want_sum}"
            ));
        }
        total += sum;
    }
    assert!(
        wrong.is_empty(),
        "the surface census has moved:\n  {}",
        wrong.join("\n  ")
    );
    let sentence = format!("{} mentions in seven files", grouped(total));
    assert!(
        text.contains(&sentence),
        "the prose should say {sentence:?}"
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

/// The cost table for RFC-0126 §3, printed from the code:
/// `cargo test -p vyrn-cli --test surface -- --ignored --nocapture
/// the_surface_census_as_a_table`.
#[test]
#[ignore]
fn the_surface_census_as_a_table() {
    let code: Vec<String> = COLUMNS
        .iter()
        .map(|(_, f)| code_only(&compiler_file(f)))
        .collect();
    println!("{COST_HEADER}");
    println!("|---|---|---|---|---|---|---|---|---|");
    let mut total = 0usize;
    for name in constructors() {
        let counts: Vec<usize> = (0..COLUMNS.len())
            .map(|k| mentions(&code[k], &name))
            .collect();
        let sum: usize = counts.iter().sum();
        total += sum;
        let cells: Vec<String> = counts.iter().map(|c| c.to_string()).collect();
        println!("| `Type::{name}` | {} | {sum} |", cells.join(" | "));
    }
    println!("\n{total} mentions in seven files");
}
