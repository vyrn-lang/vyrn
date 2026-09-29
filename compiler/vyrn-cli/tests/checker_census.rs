//! The structural census of `compiler/vyrn-frontend/src/checker.rs`, the workspace's
//! largest file, by the method of `tests/frontend_census.rs`.
//!
//! A section is one item (`fn`, `struct`, `enum`, `impl`, `mod`, `const`) with every item
//! after it up to the next section's anchor. A span runs from the anchor's doc comment to
//! the line before the next anchor's, so every line belongs to exactly one section and
//! the counts add up to the file. The test computes the spans; the table below records
//! each anchor and its kind, so an edit moves the numbers while the classification stays
//! where a reader put it.
//!
//! Each section also carries its count of `cerr!`/`cerr_at!` sites: most refusal sites sit
//! in `Checker::call`, one arm per builtin, and a kind alone would file them under the
//! surface. [`the_structural_census_matches_its_pin`] pins the per-kind refusal
//! tally beside the per-kind line tally, so a rule that leaves the checker moves both.

mod common;

use std::path::{Path, PathBuf};

/// What a section of `checker.rs` is.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Kind {
    /// The typing judgment proper: inference, unification, assignability, the
    /// type of a node, and the [`Recorded`] table every later pass reads.
    Judgment,
    /// A section that exists to state a rule. Its output is a `Diagnostic` and
    /// nothing else reads it. These are the deletion candidates when another
    /// pass states the same rule.
    Refusal,
    /// The checker's part in a rewrite that is stated somewhere else: the arms
    /// that recognise a desugar's output, and the re-typing of AST the parser
    /// synthesized. The rewrite itself is `parser.rs`'s.
    Desugar,
    /// A table with one arm per surface form, per type constructor or per builtin: the
    /// `(surface x types x builtins)` product.
    Surface,
    /// Shared machinery: the walk, the scope stacks, the whole-program tables,
    /// the entry points, the renderers.
    Shared,
    /// The file's own unit tests.
    Tests,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Judgment => "the typing judgment",
            Kind::Refusal => "a rule the checker states",
            Kind::Desugar => "the checker's part in a rewrite stated elsewhere",
            Kind::Surface => "one arm per form, type constructor or builtin",
            Kind::Shared => "shared machinery",
            Kind::Tests => "tests",
        }
    }
}

/// One section: the head of the item that starts it, and its kind.
struct Section {
    at: &'static str,
    kind: Kind,
}

const fn sec(at: &'static str, kind: Kind) -> Section {
    Section { at, kind }
}

/// The sections, in file order. The first one starts at line 1.
fn sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec("macro_rules! cerr", Shared),
        sec("fn set_gen_host", Shared),
        sec("struct LocalBinding", Shared),
        sec("const RESERVED", Surface),
        sec("fn check_accum_with_sites", Shared),
        sec("fn check_accum_inner", Shared),
        sec("fn check_places", Refusal),
        sec("fn check_optional_place", Refusal),
        sec("fn let_borrows_from", Desugar),
        sec("fn count_yields", Shared),
        sec("fn check_named_blocks", Refusal),
        sec("fn check_accum", Shared),
        sec("struct Recorded", Judgment),
        sec("struct Checker", Shared),
        sec("fn base", Judgment),
        sec("fn check_key_shape", Refusal),
        sec("fn refuse_chained_projection", Desugar),
        sec("fn optional_scrutinee", Desugar),
        sec("fn place_result", Desugar),
        sec("fn solve_head", Judgment),
        sec("fn declared_owned_in", Surface),
        sec("fn enum_type_params", Shared),
        sec("fn reaches", Shared),
        sec("fn contains_stream", Surface),
        sec("fn contains_fn", Surface),
        sec("fn assignable", Judgment),
        sec("fn mentions_open_param", Judgment),
        sec("fn coercible", Judgment),
        sec("fn prove_coercion", Refusal),
        sec("fn prove_string_interpolation", Desugar),
        sec("fn ensure_no_stream", Refusal),
        sec("fn ensure_type_exists", Surface),
        sec("fn check_protocol_decl", Refusal),
        sec("fn check_contract_decl", Refusal),
        sec("fn check_member_default", Refusal),
        sec("fn check_type_decl", Refusal),
        sec("fn param_has_bound", Judgment),
        sec("fn type_satisfies", Surface),
        sec("fn check_extern_sig", Refusal),
        sec("fn check_globals", Refusal),
        sec("fn function", Shared),
        sec("fn record_desugar", Desugar),
        sec("fn block", Shared),
        sec("fn stmt", Judgment),
        sec("fn contains_heap", Surface),
        sec("fn region_store_guard", Refusal),
        sec("fn expr", Judgment),
        sec("fn check_struct_lit", Judgment),
        sec("fn check_try", Desugar),
        sec("fn check_match", Judgment),
        sec("fn arm_block", Desugar),
        sec("fn check_match_enum", Judgment),
        sec("fn binop_type", Surface),
        sec("fn vector_call", Surface),
        sec("fn show_dispatch", Judgment),
        sec("fn call", Surface),
        sec("fn check_declared_call", Judgment),
        sec("fn solve_fn_param", Judgment),
        sec("fn check_fn_arg", Judgment),
        sec("fn stored_fn_lambda", Judgment),
        sec("fn storable_named_fn", Refusal),
        sec("fn check_lambda_body_captures", Refusal),
        sec("fn check_modify_arg", Refusal),
        sec("fn unify", Judgment),
        sec("fn check_construction", Judgment),
        sec("fn shadows_here", Shared),
        sec("fn mut_array_receiver", Judgment),
        sec("fn pred_summary", Shared),
        sec("fn type_mentions_self", Surface),
        sec("fn literal_value", Judgment),
        sec("fn extern_abi_type_ok", Surface),
        sec("fn check_comptime_purity", Refusal),
        sec("struct StoredSource", Shared),
        sec("fn module_state_use", Shared),
        sec("fn touches_globals", Surface),
        sec("struct GlobalRef", Shared),
        sec("struct InitRules", Refusal),
        sec("crate::body_scope_descent!", Shared),
        sec("mod tests", Tests),
    ]
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn checker() -> Vec<String> {
    let p = repo_root().join("compiler/vyrn-frontend/src/checker.rs");
    std::fs::read_to_string(&p)
        .expect("read checker.rs")
        .replace("\r\n", "\n")
        .lines()
        .map(str::to_string)
        .collect()
}

fn spans(lines: &[String]) -> Vec<(usize, usize, usize)> {
    common::census_spans("checker.rs", lines, sections().iter().map(|s| s.at))
}

/// How many refusal sites a span holds: uses of the two macros, not their
/// definitions and not a comment that names them.
fn refusals(lines: &[String], a: usize, b: usize) -> usize {
    let mut in_macro = false;
    lines[a - 1..b]
        .iter()
        .filter(|l| {
            if l.starts_with("macro_rules!") {
                in_macro = true;
            } else if in_macro && l.as_str() == "}" {
                in_macro = false;
            }
            !in_macro && !l.trim_start().starts_with("//")
        })
        .filter(|l| l.contains("cerr!(") || l.contains("cerr_at!("))
        .count()
}

/// The line count and the refusal count per kind, pinned in
/// `tests/pins/checker-census.tsv`; `VYRN_PIN=write` rewrites the file.
#[test]
fn the_structural_census_matches_its_pin() {
    let lines = checker();
    let secs = sections();
    let mut by_kind = std::collections::BTreeMap::new();
    let mut cerr_by_kind = std::collections::BTreeMap::new();
    for (i, a, b) in spans(&lines) {
        *by_kind.entry(secs[i].kind as usize).or_insert(0usize) += b - a + 1;
        *cerr_by_kind.entry(secs[i].kind as usize).or_insert(0usize) += refusals(&lines, a, b);
    }
    let got: Vec<(&'static str, usize, usize)> = [
        Kind::Judgment,
        Kind::Refusal,
        Kind::Desugar,
        Kind::Surface,
        Kind::Shared,
        Kind::Tests,
    ]
    .iter()
    .map(|k| {
        (
            k.label(),
            by_kind.get(&(*k as usize)).copied().unwrap_or(0),
            cerr_by_kind.get(&(*k as usize)).copied().unwrap_or(0),
        )
    })
    .collect();
    common::pin(
        "checker-census",
        "kind\tlines\trefusals",
        got.iter().map(|(k, n, r)| format!("{k}\t{n}\t{r}")),
    );
    assert_eq!(
        got.iter().map(|(_, n, _)| n).sum::<usize>(),
        lines.len(),
        "the kinds do not add up to the file"
    );
    assert_eq!(
        got.iter().map(|(_, _, n)| n).sum::<usize>(),
        refusals(&lines, 1, lines.len()),
        "the refusal counts do not add up to the file"
    );
}

/// Every refusal `Checker::stmt` and `Checker::expr` state, as `vyrn check` prints it
/// over `tests/checker-rules.vyrn`, one body per rule, pinned in
/// `tests/pins/checker-rules.tsv`. A rule that leaves the checker leaves this pin as it
/// is; the pin moves only with a sentence.
#[test]
fn every_rule_of_the_two_walks_says_what_the_pin_records() {
    let out = common::vyrn()
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests"))
        .args(["check", "checker-rules.vyrn"])
        .output()
        .expect("run vyrn check");
    assert_eq!(out.status.code(), Some(1), "the program is refused");
    let stderr = String::from_utf8(out.stderr).expect("stderr is UTF-8");
    common::pin(
        "checker-rules",
        "what vyrn check prints",
        stderr.lines().map(str::to_string),
    );
}
