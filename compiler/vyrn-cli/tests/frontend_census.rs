//! The structural census of `loader.rs`, `symbols.rs`, `project.rs` and `movecheck.rs`
//! by the method of `checker_census.rs`. A section is an anchor item, from
//! its doc comment, up to the next anchor's, so the sections tile the file. Each has a
//! kind that answers "is this rule stated anywhere else?", and a count of the
//! `Diagnostic::` sites it holds, because a rule that leaves a file moves both numbers.

mod common;

use std::path::{Path, PathBuf};

/// What a section is.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Kind {
    /// The file's own job: loading and linking modules; the symbol table the
    /// editor reads; the inlining of a place projection. Nothing replaces this.
    Job,
    /// A rule stated a second time: the same walk over the AST for the same fact as
    /// another pass or function. A deletion candidate.
    Twice,
    /// A path only a deleted route reached. Proved by grepping every caller.
    Dead,
    /// A copy of a table another module carries.
    Copy,
    /// Shared machinery: the data model, the scope helpers, the renderers.
    Shared,
    /// The file's own unit tests.
    Tests,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Job => "the file's own job",
            Kind::Twice => "a rule stated a second time",
            Kind::Dead => "a path only a deleted route reached",
            Kind::Copy => "a copy of a table another module carries",
            Kind::Shared => "shared machinery",
            Kind::Tests => "tests",
        }
    }
}

/// The exact source line that starts a section, and its kind.
struct Section {
    at: &'static str,
    kind: Kind,
}

const fn sec(at: &'static str, kind: Kind) -> Section {
    Section { at, kind }
}

/// The sections of `loader.rs`, in file order; the first starts at line 1.
fn loader_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec("pub trait ModuleResolver {", Shared),
        sec("fn bump_gen_runs() {", Shared),
        sec("pub struct MapResolver(pub HashMap<String, String>);", Shared),
        sec("pub(crate) fn normalize(path: &str) -> String {", Job),
        sec("fn site_file(key: &str, root_key: &str, std_root: Option<&str>) -> String {", Job),
        sec("fn stamp_panic_sites(program: &mut Program, file: &str) {", Job),
        sec("fn dir_of(resolved: &str) -> &str {", Job),
        sec("pub fn builtin_alias_exports(spec: &str) -> Option<Vec<&'static str>> {", Job),
        sec("pub fn resolve_spec(spec: &str, importer: &str, opts: &LoadOptions) -> Result<String, String> {", Job),
        sec("pub struct LoadOptions {", Shared),
        sec("fn audience_objection(", Job),
        sec("struct Module {", Shared),
        sec("pub const RT_PREFIX: &str = \"json$\";", Job),
        sec("pub fn generated_modules(", Job),
        sec("pub fn load(", Job),
        sec("fn floor_graph(modules: &mut [Module]) -> crate::floor::Graph {", Job),
        sec("fn load_modules(", Job),
        sec("const GEN_FUEL: u64 = 20_000_000;", Shared),
        sec("fn run_generator(", Job),
        sec("fn generator_cache_key(", Job),
        sec("fn is_injected(t: &TypeDecl) -> bool {", Shared),
        sec("enum DeclKind {", Shared),
        sec("fn resolve_aliases(modules: &mut [Module], errors: &mut Vec<Diagnostic>, root_key: &str) {", Job),
        sec("macro_rules! type_head_descent {", Shared),
        sec("crate::body_scope_descent!(BodyVisit, body_block, body_stmt, body_expr);", Shared),
        sec("struct NsResolver<'a> {", Job),
        sec("fn link(mut modules: Vec<Module>, root_key: &str) -> Result<Program, Vec<Diagnostic>> {", Job),
        sec("fn in_module(mut d: Diagnostic, key: &str, root_key: &str) -> Diagnostic {", Shared),
        sec("fn clash_diagnostics(", Job),
        sec("fn fn_body_ref_names(f: &Function) -> Vec<(String, usize)> {", Job),
        sec("fn type_names(ty: &Type) -> Vec<String> {", Job),
        sec("fn ren<'a>(map: &'a HashMap<String, String>, n: &'a str) -> String {", Shared),
        sec("fn rewrite_type(ty: &mut Type, map: &HashMap<String, String>) {", Job),
        sec("pub(crate) fn rewrite_names(p: &mut Program, map: &HashMap<String, String>) {", Job),
        sec("fn program_ref_names(p: &Program) -> HashSet<String> {", Job),
        sec("fn rename_decls_in_module(p: &mut Program, map: &HashMap<String, String>, ns: &HashSet<String>) {", Job),
        sec("mod tests {", Tests),
    ]
}

/// The sections of `symbols.rs`, in file order.
fn symbols_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec("pub enum SymbolKind {", Shared),
        sec("pub fn analyze(source: &str) -> Analysis {", Job),
        sec("fn adopt_foreign(mut d: Diagnostic) -> Diagnostic {", Shared),
        sec("fn analyze_inner(", Job),
        sec("fn memory_notes(program: &crate::ast::Program) -> Vec<MemoryNote> {", Job),
        sec("fn empty_analysis(diagnostics: Vec<Diagnostic>) -> Analysis {", Shared),
        sec("fn keyword_text(t: &Tok) -> Option<String> {", Job),
        sec("fn backtick_tokens(msg: &str) -> Vec<&str> {", Shared),
        sec("fn pin_diagnostics(", Job),
        sec("pub fn resolve(analysis: &Analysis, line: usize, col: usize) -> Option<Resolution> {", Job),
        sec("pub(crate) static BUILTIN_TYPES_AND_CTORS: &[(&str, &str, SymbolKind, &str)] = &[", Job),
        sec("fn enclosing_fn_line(analysis: &Analysis, cursor_line: usize) -> Option<usize> {", Shared),
        sec("pub fn completions(analysis: &Analysis) -> Vec<Completion> {", Job),
        sec("pub fn member_completions(analysis: &Analysis, line: usize, col: usize) -> Vec<Completion> {", Job),
        sec("pub fn string_literal_completions(", Job),
        sec("fn receiver_before_dot(analysis: &Analysis, line: usize, col: usize) -> Option<String> {", Job),
        sec("fn decl_lines(program: &ast::Program) -> Vec<usize> {", Shared),
        sec("fn index_symbols(program: &ast::Program, tok_info: &[TokenInfo], lines: &[usize]) -> Vec<Symbol> {", Job),
        sec("fn index_imported_symbols(", Job),
        sec("fn index_namespaces(", Job),
        sec("pub(crate) struct OriginIndex {", Job),
        sec("fn with_doc(detail: &str, doc: &Option<String>) -> String {", Job),
        sec("pub fn type_to_string(ty: &Type) -> String {", Shared),
        sec("pub struct DocExport {", Job),
        sec("pub enum SemKind {", Job),
        sec("static MACRO_BUILTINS: &[&str] = &[", Copy),
        sec("fn is_constructor_builtin(name: &str) -> bool {", Job),
        sec("pub struct InlayHint {", Job),
        sec("pub struct RefRange {", Job),
        sec("fn classify_token(analysis: &Analysis, tok: &TokenInfo) -> Option<(SemKind, SemMods)> {", Job),
        sec("struct BuiltinMethod {", Copy),
        sec("mod tests {", Tests),
    ]
}

/// The sections of `project.rs` (place projections, not the project manifest), in
/// file order.
fn project_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec("pub const ELEM: &str = \"@slot\";", Job),
        sec("pub struct Projection {", Shared),
        sec("pub fn is_builtin_container(ty: &Type) -> bool {", Job),
        sec("pub fn site(", Job),
        sec("struct OptExpansion {", Shared),
        sec("pub fn store_index(", Job),
        sec("pub fn inline(f: &Function, recv: &Expr, args: &[Expr], line: usize) -> Result<Projection, String> {", Job),
        sec("fn substituted(", Job),
        sec("pub struct OptionalProjection {", Job),
        sec("pub fn store_node(blk: &Block) -> Option<&Stmt> {", Job),
        sec("pub fn iterate_loop(", Job),
        sec("fn collect_bindings(b: &mut Block, tag: usize, out: &mut HashMap<String, String>) {", Job),
        sec("fn count_uses(b: &Block, name: &str) -> usize {", Job),
        sec("pub fn walk_block(b: &mut Block, f: &mut impl FnMut(&mut Expr)) {", Shared),
        sec("pub fn is_place(e: &Expr) -> bool {", Shared),
        sec("pub(crate) fn named_projection(name: &str) -> bool {", Job),
        sec("mod tests {", Tests),
    ]
}

/// The sections of `movecheck.rs`, in file order. It states no refusal: it is a
/// driver, a memo, the rows the core reads and the walk that fills them.
fn movecheck_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec("pub struct ArgTemp {", Job),
        sec("pub struct Facts {", Job),
        sec("fn declarations(program: &Program) -> Declared {", Shared),
        sec("struct Lets<'a> {", Job),
        sec("pub fn fn_sig_key(ps: &[Type], ret: &Type, decls: &HashMap<String, TypeDecl>) -> String {", Job),
        sec("pub fn lets_outputs(program: &Program) -> Vec<String> {", Job),
        sec("pub fn hands_back(name: &str) -> bool {", Job),
        sec("fn views(name: &str) -> bool {", Job),
        sec("fn in_source_order(diags: &mut [Diagnostic]) {", Job),
        sec("pub fn comptime<T>(f: impl FnOnce() -> T) -> T {", Job),
        sec("pub type Verdict = Vec<(Option<String>, usize, String, String)>;", Job),
        sec("fn subject(message: &str) -> Option<&str> {", Shared),
        sec("mod tests {", Tests),
    ]
}

fn files() -> Vec<(&'static str, Vec<Section>)> {
    vec![
        ("loader.rs", loader_sections()),
        ("symbols.rs", symbols_sections()),
        ("project.rs", project_sections()),
        ("movecheck.rs", movecheck_sections()),
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

/// Each section's span as `(index, first line, last line)`, one-based and inclusive.
/// Every line of the file is in exactly one span.
fn spans(file: &str, lines: &[String], secs: &[Section]) -> Vec<(usize, usize, usize)> {
    let mut anchors = Vec::new();
    for s in secs {
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
            "the anchor `{}` names {} lines of {file}; a section's anchor must name one",
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
            "section `{}` of {file} is empty or out of order",
            secs[i].at
        );
        out.push((i, first + 1, last));
    }
    out
}

fn diags(lines: &[String], a: usize, b: usize) -> usize {
    lines[a - 1..b]
        .iter()
        .filter(|l| l.contains("Diagnostic::error(") || l.contains("Diagnostic::warning("))
        .count()
}

/// Pins the line and diagnostic counts per kind in `tests/pins/frontend-census.tsv`.
#[test]
fn the_frontend_census_matches_its_pin() {
    let order = [
        Kind::Job,
        Kind::Twice,
        Kind::Dead,
        Kind::Copy,
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
        "frontend-census",
        "file\tkind\tlines\tdiagnostics",
        got.iter().map(|(f, k, n, d)| format!("{f}\t{k}\t{n}\t{d}")),
    );
}
