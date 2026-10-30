//! The structural census of `parser.rs`, the tree builder, and `lexer.rs`, by the method
//! of `refusals.rs`. A section is an anchor line, from its doc comment, up to the next
//! anchor's, so the sections tile the file. Each has a kind that answers "is the parser
//! the right home for this?", and a count of the `Diagnostic::` sites it holds, because
//! a rule that leaves a file moves both numbers. An anchor is usually an item; one is
//! the `impl` flattening in `parse_with`, because a section is what a reader would
//! delete. `ast.rs` and `fmt.rs` are not tiled: `surface.rs` and `forms.rs` price the
//! AST a constructor at a time, and `fmt.rs` names no form and no keyword.

mod common;

use std::path::{Path, PathBuf};

/// What a section is.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Kind {
    /// A production's tree: only the builder reads its events. One that also rewrites
    /// is `Grammar`.
    Grammar,
    /// A section that exists only to state a rewrite. The parser is the wrong home
    /// when another pass restates it, the right one when another pass keeps a special
    /// case for a form already rewritten away.
    Desugar,
    /// A table stated a second time: a deletion candidate.
    Twice,
    /// The checks behind a row the recognizer raises.
    Recovery,
    /// Shared machinery: the cursor, the state, the entry points.
    Shared,
    /// The file's own unit tests.
    Tests,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Grammar => "the tree of a form",
            Kind::Desugar => "a desugar the parser states",
            Kind::Twice => "a table stated a second time",
            Kind::Recovery => "the checks behind a row",
            Kind::Shared => "shared machinery",
            Kind::Tests => "tests",
        }
    }
}

/// The head of the item that starts a section, and its kind.
struct Section {
    at: &'static str,
    kind: Kind,
}

const fn sec(at: &'static str, kind: Kind) -> Section {
    Section { at, kind }
}

/// The sections of `parser.rs`, in file order; the first starts at line 1.
fn parser_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec("fn is_member_type_param", Grammar),
        sec("fn mark_member_type_params", Desugar),
        sec("fn parse_bare", Shared),
        sec("    let mut flat = Vec::new();", Desugar),
        sec("fn binop_text", Shared),
        sec("fn if_let(", Desugar),
        sec("fn is_separator", Recovery),
        sec("fn as_fn_body", Desugar),
        sec("fn skeleton_refusal", Recovery),
        sec("fn store_target", Grammar),
        sec("fn build(", Shared),
        sec("fn take_docs", Grammar),
        sec("fn else_tail", Desugar),
        sec("fn if_let_stmt", Grammar),
        sec("fn spliced", Desugar),
        sec("fn ref_let_parts", Grammar),
        sec("fn refutable_let", Desugar),
        sec("fn stmt", Grammar),
        sec("fn nullish", Desugar),
        sec("fn unary", Grammar),
        sec("fn template", Desugar),
        sec("fn match_expr", Grammar),
        sec("fn storage_desugar", Desugar),
        sec("fn if_expr", Grammar),
        sec("mod tests", Tests),
    ]
}

/// The sections of `lexer.rs`, in file order.
fn lexer_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec("struct Token", Shared),
        sec("fn token_name_and_text", Shared),
        sec("struct Triv", Shared),
        sec("fn scan", Shared),
        sec("fn lex", Shared),
        sec("mod tests", Tests),
    ]
}

fn files() -> Vec<(&'static str, Vec<Section>)> {
    vec![
        ("parser.rs", parser_sections()),
        ("lexer.rs", lexer_sections()),
    ]
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn source(file: &str) -> Vec<String> {
    let p = repo_root().join("compiler/vyrn-frontend/src").join(file);
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("read {file}: {e}"))
        .replace("\r\n", "\n")
        .lines()
        .map(str::to_string)
        .collect()
}

fn spans(file: &str, lines: &[String], secs: &[Section]) -> Vec<(usize, usize, usize)> {
    common::census_spans(file, lines, secs.iter().map(|s| s.at))
}

fn diags(lines: &[String], a: usize, b: usize) -> usize {
    lines[a - 1..b]
        .iter()
        .filter(|l| {
            [
                "Diagnostic::error(",
                "Diagnostic::warning(",
                "Diagnostic::refusal(",
                "refuse!(",
            ]
            .iter()
            .any(|site| l.contains(site))
        })
        .count()
}

#[test]
fn the_parser_census_matches_its_pin() {
    let order = [
        Kind::Grammar,
        Kind::Desugar,
        Kind::Twice,
        Kind::Recovery,
        Kind::Shared,
        Kind::Tests,
    ];
    let mut got: Vec<(&'static str, &'static str, usize, usize)> = Vec::new();
    for (file, secs) in files() {
        let lines = source(file);
        let mut by_kind = std::collections::BTreeMap::new();
        let mut dg_by_kind = std::collections::BTreeMap::new();
        for (i, a, b) in spans(file, &lines, &secs) {
            *by_kind.entry(secs[i].kind as usize).or_insert(0usize) += b - a + 1;
            *dg_by_kind.entry(secs[i].kind as usize).or_insert(0usize) += diags(&lines, a, b);
        }
        for k in order {
            got.push((
                file,
                k.label(),
                by_kind.get(&(k as usize)).copied().unwrap_or(0),
                dg_by_kind.get(&(k as usize)).copied().unwrap_or(0),
            ));
        }
        assert_eq!(
            order
                .iter()
                .map(|k| by_kind.get(&(*k as usize)).copied().unwrap_or(0))
                .sum::<usize>(),
            lines.len(),
            "the kinds do not add up to {file}"
        );
        assert_eq!(
            order
                .iter()
                .map(|k| dg_by_kind.get(&(*k as usize)).copied().unwrap_or(0))
                .sum::<usize>(),
            diags(&lines, 1, lines.len()),
            "the diagnostic counts do not add up to {file}"
        );
    }
    common::pin(
        "parser-census",
        "file\tkind\tlines\tdiagnostics",
        got.iter().map(|(f, k, n, d)| format!("{f}\t{k}\t{n}\t{d}")),
    );
}
