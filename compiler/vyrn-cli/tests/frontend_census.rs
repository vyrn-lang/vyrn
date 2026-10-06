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

/// The head of the item that starts a section, and its kind.
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
        sec("trait ModuleResolver", Shared),
        sec("fn bump_gen_runs", Shared),
        sec("struct MapResolver", Shared),
        sec("fn normalize", Job),
        sec("fn site_file", Job),
        sec("fn stamp_panic_sites", Job),
        sec("fn dir_of", Job),
        sec("fn builtin_alias_exports", Job),
        sec("fn resolve_spec", Job),
        sec("struct LoadOptions", Shared),
        sec("fn audience_objection", Job),
        sec("struct Module", Shared),
        sec("const RT_PREFIX", Job),
        sec("fn generated_modules", Job),
        sec("fn load", Job),
        sec("fn floor_graph", Job),
        sec("fn load_modules", Job),
        sec("const GEN_FUEL", Shared),
        sec("fn run_generator", Job),
        sec("fn generator_cache_key", Job),
        sec("fn is_injected", Shared),
        sec("enum DeclKind", Shared),
        sec("fn resolve_aliases", Job),
        sec("fn type_heads", Shared),
        sec("crate::body_scope_descent!", Shared),
        sec("struct NsResolver", Job),
        sec("fn link", Job),
        sec("fn in_module", Shared),
        sec("fn clash_diagnostics", Job),
        sec("fn fn_body_ref_names", Job),
        sec("fn type_names", Job),
        sec("fn ren", Shared),
        sec("fn rewrite_type", Job),
        sec("fn rewrite_names", Job),
        sec("fn program_ref_names", Job),
        sec("fn rename_decls_in_module", Job),
        sec("mod tests", Tests),
    ]
}

/// The sections of `symbols.rs`, in file order.
fn symbols_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec("enum SymbolKind", Shared),
        sec("fn analyze", Job),
        sec("fn adopt_foreign", Shared),
        sec("fn analyze_inner", Job),
        sec("fn memory_notes", Job),
        sec("fn empty_analysis", Shared),
        sec("fn keyword_text", Job),
        sec("fn backtick_tokens", Shared),
        sec("fn pin_diagnostics", Job),
        sec("fn resolve", Job),
        sec("static BUILTIN_TYPES_AND_CTORS", Job),
        sec("fn enclosing_fn_line", Shared),
        sec("fn completions", Job),
        sec("fn member_completions", Job),
        sec("fn string_literal_completions", Job),
        sec("fn receiver_before_dot", Job),
        sec("fn decl_lines", Shared),
        sec("fn index_symbols", Job),
        sec("fn index_imported_symbols", Job),
        sec("fn index_namespaces", Job),
        sec("struct OriginIndex", Job),
        sec("fn with_doc", Job),
        sec("fn type_to_string", Shared),
        sec("struct DocExport", Job),
        sec("enum SemKind", Job),
        sec("fn is_macro_builtin", Job),
        sec("fn is_constructor_builtin", Job),
        sec("struct InlayHint", Job),
        sec("struct RefRange", Job),
        sec("fn classify_token", Job),
        sec("struct BuiltinMethod", Job),
        sec("mod tests", Tests),
    ]
}

/// The sections of `project.rs` (place projections, not the project manifest), in
/// file order.
fn project_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec("const ELEM", Job),
        sec("struct Projection", Shared),
        sec("fn is_builtin_container", Job),
        sec("struct Expansions", Shared),
        sec("fn site", Job),
        sec("fn inline", Job),
        sec("fn substituted", Job),
        sec("struct OptionalProjection", Job),
        sec("fn store_node", Job),
        sec("const FOR_RECV", Job),
        sec("fn collect_bindings", Job),
        sec("fn count_uses", Job),
        sec("fn walk_block", Shared),
        sec("fn is_place", Shared),
        sec("fn projection_call", Job),
        sec("mod tests", Tests),
    ]
}

/// The sections of `movecheck.rs`, in file order. It states no refusal: it is a
/// driver, a memo and the screens the core reads at a call.
fn movecheck_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec("struct ArgTemp", Job),
        sec("fn hands_back", Job),
        sec("fn views", Job),
        sec("fn in_source_order", Job),
        sec("type Verdict", Job),
        sec("mod tests", Tests),
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
