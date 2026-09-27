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

/// One section: the exact source line that starts it and its kind.
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
        sec("macro_rules! cerr {", Shared),
        sec("pub fn set_gen_host(on: bool) {", Shared),
        sec("pub struct LocalBinding {", Shared),
        sec("pub const RESERVED: &[&str] = &[", Surface),
        sec("pub fn check_accum_with_json_types(program: &Program) -> CheckedJson {", Shared),
        sec("fn check_accum_inner(", Shared),
        sec("fn check_places(checker: &Checker, program: &Program, out: &mut Vec<Diagnostic>) {", Refusal),
        sec("fn check_optional_place(checker: &Checker, f: &Function, push: &mut impl FnMut(Diagnostic)) {", Refusal),
        sec("fn let_borrows_from(e: &Expr, roots: &std::collections::HashSet<String>) -> bool {", Desugar),
        sec("fn count_yields(b: &crate::ast::Block) -> usize {", Shared),
        sec("fn check_named_blocks(", Refusal),
        sec("pub fn check_accum(program: &Program) -> Vec<Diagnostic> {", Shared),
        sec("pub struct Recorded {", Judgment),
        sec("struct Checker<'a> {", Shared),
        sec("fn base(&self, ty: &Type) -> Type {", Judgment),
        sec("fn check_key_shape(&self, key: &Type, ty: &Type, line: usize) -> Result<(), Diagnostic> {", Refusal),
        sec("fn refuse_chained_projection(", Desugar),
        sec("fn optional_scrutinee(", Desugar),
        sec("fn place_result(", Desugar),
        sec("fn solve_head(&self, imp: &crate::ast::ImplBlock, recv: &Type, ty: &Type, line: usize) -> Type {", Judgment),
        sec("fn declared_owned_in(", Surface),
        sec("fn enum_type_params(&self, enum_name: &str) -> Vec<String> {", Shared),
        sec("fn reaches(&self, ty: &Type, at: &dyn Fn(&Type) -> Reach) -> bool {", Shared),
        sec("fn contains_stream(&self, ty: &Type) -> bool {", Surface),
        sec("fn contains_fn(&self, ty: &Type) -> bool {", Surface),
        sec("fn assignable(&self, from: &Type, to: &Type) -> bool {", Judgment),
        sec("fn mentions_open_param(&self, ty: &Type) -> bool {", Judgment),
        sec("fn coercible(&self, from: &Type, to: &Type) -> bool {", Judgment),
        sec("fn prove_coercion(&self, expr: &Expr, to: &Type, line: usize) -> Result<(), Diagnostic> {", Refusal),
        sec("fn prove_string_interpolation(", Desugar),
        sec("fn ensure_no_stream(&self, ty: &Type, line: usize, where_: &str) -> Result<(), Diagnostic> {", Refusal),
        sec("fn ensure_type_exists(&self, ty: &Type, line: usize) -> Result<(), Diagnostic> {", Surface),
        sec("fn check_protocol_decl(&self, p: &ProtocolDecl) -> Vec<Diagnostic> {", Refusal),
        sec("fn check_contract_decl(&self, c: &ContractDecl) -> Vec<Diagnostic> {", Refusal),
        sec("fn check_member_default(", Refusal),
        sec("fn check_type_decl(&self, t: &TypeDecl) -> Result<(), Diagnostic> {", Refusal),
        sec("fn param_has_bound(&self, t: &str, bound: &str) -> bool {", Judgment),
        sec("fn type_satisfies(&self, ty: &Type, bound: &str) -> bool {", Surface),
        sec("fn check_extern_sig(&self, f: &Function) -> Result<(), Diagnostic> {", Refusal),
        sec("fn check_globals(", Refusal),
        sec("fn function(&self, f: &Function) -> Result<(), Diagnostic> {", Shared),
        sec("fn record_desugar(&self, scope: &Scope, run: impl FnOnce(&Self, &mut Scope)) {", Desugar),
        sec("fn block(&self, block: &Block, ret: &Type, scope: &mut Scope) {", Shared),
        sec("fn stmt(&self, stmt: &Stmt, ret: &Type, scope: &mut Scope) -> Result<(), Diagnostic> {", Judgment),
        sec("fn contains_heap(&self, ty: &Type) -> bool {", Surface),
        sec("fn region_store_guard(", Refusal),
        sec("fn expr(", Judgment),
        sec("fn check_struct_lit(", Judgment),
        sec("fn check_try(", Desugar),
        sec("fn check_match(", Judgment),
        sec("fn arm_block(", Desugar),
        sec("fn check_match_enum(", Judgment),
        sec("fn binop_type(&self, op: BinOp, l: Type, r: Type, line: usize) -> Result<Type, Diagnostic> {", Surface),
        sec("fn vector_call(", Surface),
        sec("fn show_dispatch(&self, t: &Type) -> Option<String> {", Judgment),
        sec("fn call(", Surface),
        sec("fn check_declared_call(", Judgment),
        sec("fn solve_fn_param(", Judgment),
        sec("fn check_fn_arg(", Judgment),
        sec("fn stored_fn_lambda(", Judgment),
        sec("fn storable_named_fn(&self, name: &str, line: usize) -> Result<(), Diagnostic> {", Refusal),
        sec("fn check_lambda_body_captures(", Refusal),
        sec("fn check_modify_arg(", Refusal),
        sec("fn unify(", Judgment),
        sec("fn check_construction(", Judgment),
        sec("fn shadows_here(&self, name: &str) -> bool {", Shared),
        sec("fn mut_array_receiver(", Judgment),
        sec("pub(crate) fn pred_summary(expr: &Expr) -> String {", Shared),
        sec("fn type_mentions_self(ty: &Type) -> bool {", Surface),
        sec("pub fn literal_value(n: i64) -> i128 {", Judgment),
        sec("fn extern_abi_type_ok(ty: &Type, allow_unit: bool) -> bool {", Surface),
        sec("fn check_comptime_purity(program: &Program, out: &mut Vec<Diagnostic>) {", Refusal),
        sec("pub struct StoredSource {", Shared),
        sec("pub fn module_state_use(", Shared),
        sec("fn touches_globals(f: &Function, globals: &std::collections::HashSet<String>) -> bool {", Surface),
        sec("fn sum_arm_arity(name: &str, binds: usize, line: usize) -> Result<(), Diagnostic> {", Refusal),
        sec("struct GlobalRef<'a> {", Shared),
        sec("struct InitRules<'a> {", Refusal),
        sec("crate::body_scope_descent!(BodyVisit, body_block, body_stmt, body_expr);", Shared),
        sec("mod tests {", Tests),
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

/// Where a section's doc comment starts: the run of comment and attribute lines
/// straight above the anchor.
fn doc_start(lines: &[String], anchor: usize) -> usize {
    let mut i = anchor;
    while i > 0 {
        let t = lines[i - 1].trim_start();
        if t.starts_with("//") || t.starts_with("#[") {
            i -= 1;
        } else {
            break;
        }
    }
    i
}

/// The sections, with the span each holds: `(index, first line, last line)`,
/// one-based and inclusive. Every line of the file is in exactly one span.
fn spans(lines: &[String]) -> Vec<(usize, usize, usize)> {
    let secs = sections();
    let mut anchors = Vec::new();
    for s in &secs {
        let want: String = s.at.split_whitespace().collect::<Vec<_>>().join(" ");
        let hits: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.split_whitespace().collect::<Vec<_>>().join(" ") == want)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "the anchor `{}` names {} lines of checker.rs; a section's anchor must name one",
            s.at,
            hits.len()
        );
        anchors.push(doc_start(lines, hits[0]));
    }
    let mut out = Vec::new();
    for i in 0..secs.len() {
        let first = if i == 0 { 0 } else { anchors[i] };
        let last = if i + 1 == secs.len() {
            lines.len()
        } else {
            anchors[i + 1]
        };
        assert!(
            first < last,
            "section `{}` of checker.rs is empty or out of order",
            secs[i].at
        );
        out.push((i, first + 1, last));
    }
    out
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
