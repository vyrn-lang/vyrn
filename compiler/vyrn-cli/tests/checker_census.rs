//! The structural census of `checker.rs` — RFC-0125 §3 M6, the size strand.
//!
//! `compiler/vyrn-frontend/src/checker.rs` is the largest file in the
//! workspace. §2.7 estimates 139,000 lines down to 40,000–45,000, and that
//! estimate cannot be met while this file stands at its present size. Nobody
//! had counted what is in it. `own.rs` was censused by a reader against every
//! part, and `movecheck.rs` by the kind table in `tests/refusals.rs`; this is
//! the same measurement for the checker.
//!
//! # The method, which is `refusals.rs`'s
//!
//! A section is one item — a `fn`, a `struct`, an `enum`, an `impl`, a `mod`,
//! a `const` — together with every item after it up to the next section's
//! anchor. The span runs from the anchor's own doc comment to the line before
//! the next anchor's, so every line of the file belongs to exactly one section
//! and the counts add up to the file. The test computes the spans; the table
//! below records the anchor, the kind and a reader, so an edit to the file
//! moves the numbers and the classification stays where a reader put it.
//!
//! # The extra column, and why
//!
//! `movecheck.rs`'s census has a kind per section and nothing else, because
//! every one of its rules is a section. The checker's are not: 124 of its 422
//! refusal sites are inside `Checker::call`, which is a table with one arm per
//! builtin, and a kind alone would file all 124 under the surface and lose
//! them. (It was 190 of 487 when this census was written; RFC-0125 §3 M6
//! deleted the twenty-seven that restated a seeded row, then seven more when
//! four names that had no row got one, then fifteen with the six `consume`
//! rows, then nine when the ten migration hints became rows of one migration
//! table, then eight with `@reserve` and `@tally`.) So each section also
//! carries the number of `cerr!`/`cerr_at!` sites it holds, and
//! [`the_structural_census_is_what_the_rfc_records`] pins the
//! per-kind refusal tally beside the per-kind line tally. A rule that leaves
//! the checker moves both numbers.

use std::path::{Path, PathBuf};

/// What a section of `checker.rs` is.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Kind {
    /// The typing judgment proper: inference, unification, assignability, the
    /// type of a node, and the [`Recorded`] table every later pass reads.
    /// Nothing replaces this — it is what a checker is.
    Judgment,
    /// A section that exists to state a rule. Its output is a `Diagnostic` and
    /// nothing else reads it. These are the deletion candidates when another
    /// pass states the same rule.
    Refusal,
    /// The checker's part in a rewrite that is stated somewhere else: the arms
    /// that recognise a desugar's output, and the re-typing of AST the parser
    /// synthesized. The rewrite itself is `parser.rs`'s.
    Desugar,
    /// A table with one arm per surface form, per type constructor or per
    /// builtin — the `(surface x types x builtins)` product RFC-0125 §1.1
    /// measures, counted per constructor by RFC-0126 §3 and per form by
    /// RFC-0127 §3.
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

/// One section: the exact source line that starts it, its kind, and what it is.
struct Section {
    at: &'static str,
    kind: Kind,
    what: &'static str,
}

const fn sec(at: &'static str, kind: Kind, what: &'static str) -> Section {
    Section { at, kind, what }
}

/// The sections, in file order. The first one starts at line 1.
fn sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec(
            "macro_rules! cerr {",
            Shared,
            "the module head and the two error macros — every refusal in the \
             file is one of these two spellings",
        ),
        sec(
            "pub fn check_accum_reusing(",
            Shared,
            "the incremental entry point: a module's diagnostics are reused \
             when its hash and the signature fingerprint both hold",
        ),
        sec(
            "pub fn set_gen_host(on: bool) {",
            Shared,
            "the thread-local host flags a generator host and a test host set, \
             and the three atom-stream primitives",
        ),
        sec(
            "fn signature_fingerprint(",
            Shared,
            "the hash over everything a body may refer to by name, which is \
             what makes the reuse above sound",
        ),
        sec(
            "pub fn check_accum_with_let_types(",
            Shared,
            "the entry points a caller uses",
        ),
        sec(
            "pub const RESERVED: &[&str] = &[",
            Surface,
            "the builtin name table and the migration table beside it — one \
             row per builtin, the `builtins` factor of RFC-0125 §1.1. The \
             migration table carries both kinds of gone name since RFC-0125 §3 \
             M6: RFC-0094 M2's eleven that moved to a `std/` module, and the \
             ten removed free-function spellings, which were ten hand-written \
             blocks of `Checker::call` until then",
        ),
        sec(
            "pub fn check_accum_with_json_types(program: &Program) -> (Vec<Diagnostic>, Vec<Type>, Vec<Type>) {",
            Shared,
            "the full-check entry point and the two renderers a conformance \
             refusal quotes an impl head with",
        ),
        sec(
            "fn check_accum_inner(",
            Shared,
            "the driver: it builds every whole-program table — types, \
             variants, signatures, generics, capabilities, the spawn-safety \
             fixpoint, the protocol registries — and then walks the \
             declarations. Its 26 refusals are the declaration-level ones \
             (a name defined twice, an impl that does not conform, `main`'s \
             signature); the tables are what the rest of the file reads",
        ),
        sec(
            "fn check_places(checker: &Checker, program: &Program, out: &mut Vec<Diagnostic>) {",
            Refusal,
            "the shape of a `place` projection body (RFC-0091 M2): one exit, \
             no `?`, a place and not a value, rooted where the access site owns",
        ),
        sec(
            "fn check_optional_place(checker: &Checker, f: &Function, push: &mut impl FnMut(Diagnostic)) {",
            Refusal,
            "the same shape for an optional projection (RFC-0122): one \
             prologue, one decision, `Some` of a place",
        ),
        sec(
            "fn let_borrows_from(e: &Expr, roots: &std::collections::HashSet<String>) -> bool {",
            Desugar,
            "whether a `let`'s initializer borrows from a root — read by the \
             projection shape rules over the refutable-`let` desugar's `match`",
        ),
        sec(
            "fn count_yields(b: &crate::ast::Block) -> usize {",
            Shared,
            "how many `yield`s a projection body has, counting every branch",
        ),
        sec(
            "fn check_named_blocks(",
            Refusal,
            "every `test` body (RFC-0015) and every `bench` body (RFC-0055) is \
             checked as a synthetic Unit function with its host flag set, and a \
             name may not repeat inside one module — one walk, run twice",
        ),
        sec(
            "pub fn check_accum(program: &Program) -> Vec<Diagnostic> {",
            Shared,
            "the two public entry points, one accumulating and one first-error",
        ),
        sec(
            "pub struct Recorded {",
            Judgment,
            "the recorded answers, keyed by AST node address — the type of \
             every expression, the solved substitutions, the `let` types. \
             Since RFC-0125 §3 M3 every later pass reads this table instead of \
             typing the tree again, so it is the judgment's public form",
        ),
        sec(
            "struct Checker<'a> {",
            Shared,
            "the pass's state: the whole-program tables it borrows, the \
             region floor, the scope stacks, the error sink, the collection \
             cells the loader reads back, and the three-way answer a leaf \
             gives the shared type walk",
        ),
        sec(
            "fn base(&self, ty: &Type) -> Type {",
            Judgment,
            "a validated `Named` type decays to its representation",
        ),
        sec(
            "fn check_key_shape(&self, key: &Type, ty: &Type, line: usize) -> Result<(), Diagnostic> {",
            Refusal,
            "RFC-0117 M2: a user `Map` key is heapless all the way down, no \
             float anywhere, no payload-bearing enum",
        ),
        sec(
            "fn refuse_chained_projection(",
            Desugar,
            "the type a chained projection access resolves to, and the \
             refusal when it resolves to nothing an engine can inline \
             (RFC-0123)",
        ),
        sec(
            "fn optional_scrutinee(",
            Desugar,
            "an `if let` whose scrutinee is an optional projection call — the \
             hit is a borrow of a place, so the arm is typed against the \
             projection's result and not against an `Option` value",
        ),
        sec(
            "fn place_result(",
            Desugar,
            "the result capability of a projection call (RFC-0120): `read` or \
             `modify`, and what the access site may do with it",
        ),
        sec(
            "fn solve_head(&self, imp: &crate::ast::ImplBlock, recv: &Type, ty: &Type, line: usize) -> Type {",
            Judgment,
            "solve an impl head against a receiver and read a declared type \
             through the solution",
        ),
        sec(
            "fn declared_owned_in(",
            Surface,
            "the first part of a type that declares `impl Owned` — one arm per \
             type constructor, resolving named types, cycle-guarded",
        ),
        sec(
            "fn enum_type_params(&self, enum_name: &str) -> Vec<String> {",
            Shared,
            "the generic parameters of the enum a variant belongs to",
        ),
        sec(
            "fn reaches(&self, ty: &Type, at: &dyn Fn(&Type) -> Reach) -> bool {",
            Shared,
            "the resolving descent the three questions below share: every part \
             of every container, a named type through its declaration, and a \
             `seen` list of declaration heads so a recursive record \
             terminates. It was written out once per question until RFC-0125 \
             §3 M6",
        ),
        sec(
            "fn contains_stream(&self, ty: &Type) -> bool {",
            Surface,
            "whether a type reaches a `Stream` (RFC-0075) — one arm per type \
             constructor, and nothing else: the descent is the walk's",
        ),
        sec(
            "fn contains_fn(&self, ty: &Type) -> bool {",
            Surface,
            "whether a type reaches a function value (RFC-0037) — the second \
             verdict over the same walk",
        ),
        sec(
            "fn assignable(&self, from: &Type, to: &Type) -> bool {",
            Judgment,
            "structural assignability, with the descent depth in hand",
        ),
        sec(
            "fn mentions_open_param(&self, ty: &Type) -> bool {",
            Judgment,
            "which type parameters nothing has settled, by name and as a \
             predicate",
        ),
        sec(
            "fn coercible(&self, from: &Type, to: &Type) -> bool {",
            Judgment,
            "whether a value may cross into a declared type at a value \
             boundary",
        ),
        sec(
            "fn prove_coercion(&self, expr: &Expr, to: &Type, line: usize) -> Result<(), Diagnostic> {",
            Refusal,
            "a constant that fails its target's predicate is refused before \
             any engine runs — the compile-time half of `where-scalar`, and \
             the coercion census's one checker row",
        ),
        sec(
            "fn prove_string_interpolation(",
            Desugar,
            "RFC-0020 M1: an interpolation whose parts are all proved needs no \
             run-time validation",
        ),
        sec(
            "fn ensure_no_stream(&self, ty: &Type, line: usize, where_: &str) -> Result<(), Diagnostic> {",
            Refusal,
            "a `Stream` in a position that stores it (RFC-0075)",
        ),
        sec(
            "fn ensure_type_exists(&self, ty: &Type, line: usize) -> Result<(), Diagnostic> {",
            Surface,
            "one arm per type constructor: every written type is resolved, its \
             arity checked and its arguments recursed into. The largest single \
             statement of the `types` factor outside `types.rs`",
        ),
        sec(
            "fn ensure_param_type(&self, ty: &Type, line: usize) -> Result<(), Diagnostic> {",
            Refusal,
            "what a parameter's type may be (RFC-0023)",
        ),
        sec(
            "fn check_protocol_decl(&self, p: &ProtocolDecl) -> Vec<Diagnostic> {",
            Refusal,
            "a protocol's method signatures (RFC-0002 §5)",
        ),
        sec(
            "fn check_contract_decl(&self, c: &ContractDecl) -> Vec<Diagnostic> {",
            Refusal,
            "a module contract's members (RFC-0071)",
        ),
        sec(
            "fn check_member_default(",
            Refusal,
            "a contract member's default against the type it stands in for",
        ),
        sec(
            "fn check_type_decl(&self, t: &TypeDecl) -> Result<(), Diagnostic> {",
            Refusal,
            "what a type declaration may say: the `where` predicate's shape, \
             the transformer bases, the record and enum forms",
        ),
        sec(
            "fn param_has_bound(&self, t: &str, bound: &str) -> bool {",
            Judgment,
            "whether the function being checked declared a bound on `t`",
        ),
        sec(
            "fn type_satisfies(&self, ty: &Type, bound: &str) -> bool {",
            Surface,
            "one arm per built-in bound, over one arm per type constructor",
        ),
        sec(
            "fn check_extern_sig(&self, f: &Function) -> Result<(), Diagnostic> {",
            Refusal,
            "the `extern` ABI type domain (RFC-0012)",
        ),
        sec(
            "fn check_globals(&self, program: &Program, out: &mut Vec<Diagnostic>) {",
            Refusal,
            "every module-state binding in declaration order (RFC-0013)",
        ),
        sec(
            "fn function(&self, f: &Function) -> Result<(), Diagnostic> {",
            Shared,
            "one function body: the parameter scope, the bounds, the return \
             check",
        ),
        sec(
            "fn record_desugar(&self, scope: &Scope, run: impl FnOnce(&Self, &mut Scope)) {",
            Desugar,
            "type AST nobody wrote and record its answers only — the entry \
             every synthesized tree is typed through",
        ),
        sec(
            "fn block(&self, block: &Block, ret: &Type, scope: &mut Scope) -> bool {",
            Shared,
            "the statement loop and the error recovery at its boundary",
        ),
        sec(
            "fn stmt(&self, stmt: &Stmt, ret: &Type, scope: &mut Scope) -> Result<bool, Diagnostic> {",
            Judgment,
            "one arm per statement form (RFC-0127 §3's form column), each \
             giving the form its type and its bindings",
        ),
        sec(
            "fn contains_heap(&self, ty: &Type) -> bool {",
            Surface,
            "whether a type carries a heap allocation — the third verdict over \
             that walk",
        ),
        sec(
            "fn region_store_guard(",
            Refusal,
            "the `region` escape rules at a store and at a call boundary",
        ),
        sec(
            "fn expr(",
            Judgment,
            "one arm per expression form: the type of every node, and the \
             recording wrapper the `Recorded` table is filled through. The \
             centre of the judgment",
        ),
        sec(
            "fn check_struct_lit(",
            Judgment,
            "a record literal against its declaration, field by field",
        ),
        sec(
            "fn check_try(",
            Desugar,
            "`expr?` (RFC-0079): the scrutinee is an `Option`/`Result` and the \
             enclosing return agrees. The rewrite is the parser's",
        ),
        sec(
            "fn check_match(",
            Judgment,
            "a `match` over a built-in sum: both variants, once each",
        ),
        sec(
            "fn arm_block(",
            Desugar,
            "a block arm (RFC-0118) is legal in statement position — a fact \
             about the text, which the parser writes on the node and this reads",
        ),
        sec(
            "fn check_match_enum(",
            Judgment,
            "a `match` over a user enum, the arm patterns and their binders, \
             the `if`-expression form and the arm-type fold",
        ),
        sec(
            "fn binop_type(&self, op: BinOp, l: Type, r: Type, line: usize) -> Result<Type, Diagnostic> {",
            Surface,
            "one arm per operator times one arm per operand type — the \
             `surface x types` product at its densest",
        ),
        sec(
            "fn vector_call(",
            Surface,
            "the SIMD builtins (RFC-0075): one arm per lane type per operation",
        ),
        sec(
            "fn show_dispatch(&self, t: &Type) -> Option<String> {",
            Judgment,
            "which `impl Show` a value renders through, and the hint a refusal \
             adds when there is none",
        ),
        sec(
            "fn call(",
            Surface,
            "the builtin table: twenty-three guarded blocks naming a builtin, \
             each giving its arity, its argument types, its result and its \
             refusals, then the fall-through, which since RFC-0125 §3 M6 types \
             a seeded builtin against its row — twenty-eight names have no block \
             at all. The single largest thing in the file and the `builtins` \
             factor written out",
        ),
        sec(
            "fn solve_fn_param(",
            Judgment,
            "solve a `fn` parameter's own parameter type against the value",
        ),
        sec(
            "fn check_fn_arg(",
            Judgment,
            "a `fn`-typed argument (RFC-0023): a lambda, a named function or a \
             binding, monomorphized at the position",
        ),
        sec(
            "fn stored_fn_lambda(",
            Judgment,
            "a function value that is STORED (RFC-0037): the source is \
             collected for defunctionalization and its signature solved",
        ),
        sec(
            "fn storable_named_fn(&self, name: &str, line: usize) -> Result<(), Diagnostic> {",
            Refusal,
            "which named functions may become values (RFC-0037)",
        ),
        sec(
            "fn check_lambda_body_captures(",
            Refusal,
            "a lambda's capture discipline (RFC-0023)",
        ),
        sec(
            "fn captures_block(",
            Surface,
            "one arm per form again, collecting the names a lambda body \
             captures",
        ),
        sec(
            "fn check_modify_arg(",
            Refusal,
            "the call-site discipline for a `modify` parameter",
        ),
        sec(
            "fn unify(",
            Judgment,
            "match a generic parameter type against a concrete argument and \
             extend the substitution",
        ),
        sec(
            "fn check_construction(",
            Judgment,
            "`TypeName(arg)`, and the constant folded through its predicate",
        ),
        sec(
            "fn shadows_here(&self, name: &str) -> bool {",
            Shared,
            "the three scope queries: shadowing, lookup, and whether a name is \
             module state",
        ),
        sec(
            "fn mut_array_receiver(",
            Judgment,
            "the element type of the array a mutating receiver names",
        ),
        sec(
            "pub(crate) fn pred_summary(expr: &Expr) -> String {",
            Shared,
            "a predicate rendered back into one line, for a refusal to quote",
        ),
        sec(
            "fn type_mentions_self(ty: &Type) -> bool {",
            Surface,
            "whether a type names `Self`, one arm per constructor",
        ),
        sec(
            "fn stmt_source_line(s: &Stmt) -> usize {",
            Shared,
            "the line a literal's range error is attributed to",
        ),
        sec(
            "fn int_literal_fits(n: i64, bits: u8, signed: bool) -> bool {",
            Judgment,
            "the sized-integer literal rules: the value a literal denotes, \
             whether it fits, and the name and range a refusal quotes",
        ),
        sec(
            "fn extern_abi_type_ok(ty: &Type, allow_unit: bool) -> bool {",
            Surface,
            "which type constructors may appear in an `extern` signature",
        ),
        sec(
            "fn expr_contains_spawn(e: &Expr) -> bool {",
            Surface,
            "a whole-tree search, one arm per form: does this body `spawn`",
        ),
        sec(
            "fn check_comptime_purity(program: &Program, out: &mut Vec<Diagnostic>) {",
            Refusal,
            "the generation fence over the AST: a `gen fn` body's effects, \
             joined transitively. `vyrn_lower::effects` judges the same \
             lattice over the named core; this copy exists because a `gen fn` \
             no lowering instantiates has no core to judge",
        ),
        sec(
            "pub struct StoredSource {",
            Shared,
            "the stored-function-value facts (RFC-0037) the loader and the \
             effect judgment read back, and whether two signatures could name \
             one value",
        ),
        sec(
            "pub fn module_state_use(",
            Shared,
            "RFC-0025's `--workers` gate: does a root reach module state",
        ),
        sec(
            "fn touches_globals(f: &Function, globals: &std::collections::HashSet<String>) -> bool {",
            Surface,
            "one arm per binding form, collecting what a block binds",
        ),
        sec(
            "fn sum_arm_arity(name: &str, binds: usize, line: usize) -> Result<(), Diagnostic> {",
            Refusal,
            "the payload count a built-in sum's variant carries",
        ),
        sec(
            "fn pattern_binders(p: &Pattern) -> Vec<String> {",
            Shared,
            "what a pattern binds, and one arm's scope",
        ),
        sec(
            "fn global_ref_block(",
            Surface,
            "one arm per form, deciding whether a body reads or writes a global",
        ),
        sec(
            "fn init_restrictions(",
            Refusal,
            "what a module-state initializer may do (RFC-0013, RFC-0029)",
        ),
        sec(
            "pub fn fn_calls(b: &Block) -> std::collections::HashSet<String> {",
            Surface,
            "one arm per form again, collecting every callee name — the call \
             graph the two fixpoints above run over",
        ),
        sec("mod tests {", Tests, "the file's own unit tests"),
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

/// How many refusal sites a span holds.
fn refusals(lines: &[String], a: usize, b: usize) -> usize {
    lines[a - 1..b]
        .iter()
        .filter(|l| l.contains("cerr!(") || l.contains("cerr_at!("))
        .count()
}

/// The sections tile `checker.rs`: every line is in one, in file order.
#[test]
fn the_structural_census_covers_the_file() {
    let lines = checker();
    let spans = spans(&lines);
    let mut next = 1;
    for (_, a, b) in &spans {
        assert_eq!(*a, next, "a gap or an overlap at line {a} of checker.rs");
        next = b + 1;
    }
    assert_eq!(
        next - 1,
        lines.len(),
        "the last section does not reach the end of checker.rs"
    );
}

/// The line count and the refusal count per kind, as RFC-0125 §3 M6 records
/// them. The prose quotes these numbers, so they are asserted rather than
/// described: a change to `checker.rs` moves one, and the RFC's table moves
/// with it.
#[test]
fn the_structural_census_is_what_the_rfc_records() {
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
    let want = vec![
        ("the typing judgment", 3163, 135),
        ("a rule the checker states", 1354, 57),
        ("the checker's part in a rewrite stated elsewhere", 454, 16),
        ("one arm per form, type constructor or builtin", 3773, 182),
        ("shared machinery", 2091, 29),
        ("tests", 4582, 0),
    ];
    assert_eq!(got, want, "the structural census has moved");
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

/// The table for RFC-0125 §3 M6, printed from the sections above:
/// `cargo test -p vyrn-cli --test checker_census -- --ignored --nocapture
/// the_structural_census_as_a_table`.
#[test]
#[ignore]
fn the_structural_census_as_a_table() {
    let lines = checker();
    let secs = sections();
    println!("| section | lines | refusals | kind | what it is |");
    println!("|---|---|---|---|---|");
    for (i, a, b) in spans(&lines) {
        let name = secs[i]
            .at
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .trim_end_matches(" {")
            .trim_end_matches('(')
            .to_string();
        println!(
            "| `{}` | {} | {} | {} | {} |",
            name,
            b - a + 1,
            refusals(&lines, a, b),
            secs[i].kind.label(),
            secs[i].what
        );
    }
}
