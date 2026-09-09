//! The structural census of the rest of `vyrn-frontend` — RFC-0125 §3 M6, the
//! size strand.
//!
//! §2.7 counts the WHOLE compiler toward 40,000–45,000 lines. Four files carry
//! a census already: the checker (`tests/checker_census.rs`), the ownership pass
//! (`tests/refusals.rs`), the emitter (`tests/emitter_census.rs`) and the CLI
//! (`tests/cli_census.rs`). Nobody had counted `vyrn-frontend` outside the
//! checker, and it is 30,000 lines with the checker taken out. This is the same
//! measurement for the three largest of the rest: `loader.rs` (5,363),
//! `symbols.rs` (4,801) and `project.rs` (1,791) — 11,955 lines, a fifth of the
//! crate.
//!
//! # The method, which is `checker_census.rs`'s, which is `refusals.rs`'s
//!
//! A section is one item — a `fn`, a `struct`, an `enum`, an `impl`, a `mod`, a
//! `const` — together with every item after it up to the next section's anchor.
//! The span runs from the anchor's own doc comment to the line before the next
//! anchor's, so every line of a file belongs to exactly one section and the
//! counts add up to the file. The test computes the spans; the table below
//! records the anchor, the kind and a reader.
//!
//! # The kinds, and why these
//!
//! The checker's census asks "is this the judgment, a refusal, a desugar or the
//! surface". Those questions do not fit a loader, a symbol map or an inliner:
//! none of the three states a typing rule. The question that does fit is
//! RFC-0125's own — **is this rule stated anywhere else?** — so the kinds name
//! where a second statement would be:
//!
//! * [`Kind::Job`] — the file's own job. Nothing else states it.
//! * [`Kind::Twice`] — a walk over the AST that another pass, or another
//!   function in the same file, also walks for the same fact. The deletion
//!   candidates, and each row names the other site.
//! * [`Kind::Dead`] — a path only a deleted route reached (the interpreter,
//!   gone at M5; the text-IR route, gone; `Val`, gone).
//! * [`Kind::Copy`] — a copy of a table another module carries.
//! * [`Kind::Shared`] — shared machinery.
//! * [`Kind::Tests`] — the file's own unit tests.
//!
//! The second column is the number of `Diagnostic::` sites a section holds, for
//! the reason the checker's census counts `cerr!`: a rule that leaves a file
//! moves both numbers, and a file whose rules are all somewhere else states no
//! diagnostic of its own.

use std::path::{Path, PathBuf};

/// What a section is.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Kind {
    /// The file's own job: loading and linking modules; the symbol table the
    /// editor reads; the inlining of a place projection. Nothing replaces this.
    Job,
    /// A rule stated a second time — the same walk over the same AST for the
    /// same fact, beside another pass's or another function's. Each row names
    /// the other statement.
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

/// One section: the exact source line that starts it, its kind, and what it is.
struct Section {
    at: &'static str,
    kind: Kind,
    what: &'static str,
}

const fn sec(at: &'static str, kind: Kind, what: &'static str) -> Section {
    Section { at, kind, what }
}

/// `loader.rs` — RFC-0010's module loading and linking. The sections, in file
/// order; the first starts at line 1.
fn loader_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec(
            "pub trait ModuleResolver {",
            Shared,
            "the module head and the one interface a host implements — read a \
             specifier's source, list a directory, read and write the \
             generation cache",
        ),
        sec(
            "fn bump_gen_runs() {",
            Shared,
            "the generation counters and the two budgets a test lowers",
        ),
        sec(
            "pub struct MapResolver(pub HashMap<String, String>);",
            Shared,
            "the three resolvers this crate ships: the in-memory one tests \
             load a corpus through, the filesystem one every program that \
             loads a project off a disk uses (it was written out in the \
             driver, seven CLI suites, this crate's move-check tests, its \
             `lspbench` example, its contracts test and the language server), \
             and the recording one RFC-0031's `moduleInterface` wraps a real \
             resolver in",
        ),
        sec(
            "pub(crate) fn normalize(path: &str) -> String {",
            Job,
            "a path normalised, and the specifier a resolved key would be \
             written as from a given directory (RFC-0031)",
        ),
        sec(
            "fn site_file(key: &str, root_key: &str, std_root: Option<&str>) -> String {",
            Job,
            "the file name a `panic` in this module reports — derived from the \
             project's shape so two machines bake the same bytes",
        ),
        sec(
            "fn stamp_panic_sites(program: &mut Program, file: &str) {",
            Job,
            "every `panic(msg)` becomes `@panicAt(msg, \"file:line\")`. A \
             rewrite the parser could not do — only the loader knows the \
             module's file — and it is the ONE walk in this file that does not \
             restate the walk: it calls `project::walk_program`",
        ),
        sec(
            "fn dir_of(resolved: &str) -> &str {",
            Job,
            "the generated module's banner and the remote key: the directory \
             part, the separator no path can hold, the importer a banner names, \
             and a remote key's immutable base",
        ),
        sec(
            "pub fn builtin_alias_exports(spec: &str) -> Option<&'static [&'static str]> {",
            Copy,
            "the fixed export lists of `std/result` and `std/option` (RFC-0062) \
             — the same six names `symbols::BUILTIN_TYPES_AND_CTORS` carries, \
             split across two module names here. A test compares the two, which \
             is what a copy costs when it cannot be deleted",
        ),
        sec(
            "pub fn resolve_spec(spec: &str, importer: &str, opts: &LoadOptions) -> Result<String, String> {",
            Job,
            "an import specifier written inside a module becomes a module key. \
             Public so the editor reuses the loader's exact resolution rather \
             than drifting from it",
        ),
        sec("pub struct LoadOptions {", Shared, "what a load was asked for"),
        sec(
            "fn audience_objection(",
            Job,
            "the two import fences: RFC-0072's declared audience, which a \
             project opts into, and PLAN-0125-runtime §3's, which is the \
             compiler's own and no manifest widens",
        ),
        sec("struct Module {", Shared, "one parsed module awaiting linking"),
        sec(
            "pub const RT_PREFIX: &str = \"json$\";",
            Job,
            "the runtime-module table (RFC-0078 M2b, RFC-0125 §2.4): one row per \
             module a builtin's implementation lives in, its reserved prefix, \
             the builtins it routes and the ones a desugar spells. M4c made it a \
             table rather than a second copy of itself, which is why adding a \
             builtin costs one entry",
        ),
        sec(
            "pub fn generated_modules(",
            Job,
            "the synthesized source behind `vyrn emit-gen`",
        ),
        sec(
            "pub fn load(",
            Job,
            "the entry points, and what a load hands back besides the program: \
             the origin maps, the warnings and the module graph the symbol \
             indexer would otherwise recompute by loading a second time",
        ),
        sec(
            "fn floor_graph(modules: &mut [Module]) -> crate::floor::Graph {",
            Job,
            "the graphs a load already built, handed to the capability floor \
             (RFC-0103) and to `vyrn deps`",
        ),
        sec(
            "fn load_modules(",
            Job,
            "the worklist: resolve, read, parse, fence, run the generators, \
             inject the runtime modules a mention needs. The largest single \
             thing in the file",
        ),
        sec(
            "const GEN_FUEL: u64 = 20_000_000;",
            Shared,
            "the generator's three guardrails: fuel, output size, nesting depth",
        ),
        sec(
            "fn run_generator(",
            Job,
            "one `gen fn` run in the mediated sandbox (RFC-0021), cache lookup \
             and all",
        ),
        sec(
            "fn generator_cache_key(",
            Job,
            "the generation cache: the lookup key, the recorded inputs a hit \
             re-hashes, the entry format and the per-user tag that tells this \
             compiler's entries from files something else left there",
        ),
        sec(
            "fn is_injected(t: &TypeDecl) -> bool {",
            Shared,
            "which type declarations the parser injects into every file",
        ),
        sec(
            "fn resolve_aliases(modules: &mut [Module], errors: &mut Vec<Diagnostic>, root_key: &str) {",
            Job,
            "RFC-0022's import aliasing resolved into the flat namespace before \
             the register/merge machinery, which is deliberately alias-unaware, \
             and the co-naming rename that frees a foreign name for a local stub",
        ),
        sec(
            "macro_rules! type_head_descent {",
            Shared,
            "the ONE descent over a `Type`, written by a macro so that the \
             collector's shared borrow and the three rewriters' unique ones \
             are spellings of one arm list (RFC-0125 §3 M6). It was four \
             fourteen-arm matches until then, the fourth in `parser.rs`; the \
             hook is on the NODE so that one of them can replace what it \
             visits, and `type_heads`/`type_heads_mut` are the head-name \
             reading the other three want",
        ),
        sec(
            "crate::body_scope_descent!(BodyVisit, body_block, body_stmt, body_expr);",
            Shared,
            "the two expansions of the ONE descent over a `Block`. The arm list \
             itself is `ast::body_scope_descent!` since RFC-0125 §3 M6's second \
             body slice, because seven more readers wanted it and the AST is \
             where it is declared; this file states which two borrows it needs \
             and nothing else. It was three thirty-five-arm walks — `scope_*`, \
             `rewrite_*` and `NsResolver::walk_*` — until the first slice, and \
             the three had already drifted over an `Ok(x) =>` arm's binding",
        ),
        sec(
            "struct NsResolver<'a> {",
            Job,
            "RFC-0027's `ns.member` pass. Both its descents are read, not \
             written, since RFC-0125 §3 M6 — the type one from \
             `type_head_descent`, the body one from `body_scope_descent`. What \
             is left is the pass itself: what a namespace member resolves to, \
             and the receiver argument it deletes at a call",
        ),
        sec(
            "fn link(mut modules: Vec<Module>, root_key: &str) -> Result<Program, Vec<Diagnostic>> {",
            Job,
            "the link: register every module's declarations in one flat \
             namespace, decide visibility, merge, and refuse a name defined \
             twice or referenced without an import",
        ),
        sec(
            "fn with_file(mut d: Diagnostic, m: &Module, root_key: &str) -> Diagnostic {",
            Shared,
            "where a load's diagnostic points: the module's file, the import \
             line a reader has to edit, and the namespace binding a suggestion \
             would spell",
        ),
        sec(
            "fn clash_diagnostics(",
            Job,
            "two linked modules declaring one name — one diagnostic per PAIR, at \
             an import of one of them, rather than one per name at a line the \
             user never wrote",
        ),
        sec(
            "fn fn_body_ref_names(f: &Function) -> Vec<(String, usize)> {",
            Job,
            "every name a body references that could name a declaration, minus \
             the locals in scope — the link-time visibility check's question. \
             The walk is `body_scope_descent`'s since RFC-0125 §3 M6, where it \
             was the FIRST of three copies of it; what stays here is the \
             collector's own line at a site, and the namespace sugar it records \
             under a dotted spelling. The checker's \
             `Scope`/`shadows_here`/`lookup` still states the scope rule a second \
             time for a different reader",
        ),
        sec(
            "fn type_names(ty: &Type) -> Vec<String> {",
            Job,
            "every named or applied type head inside a type. Three lines over \
             the shared descent since RFC-0125 §3 M6, where it was one of three \
             copies of that descent",
        ),
        sec(
            "fn ren<'a>(map: &'a HashMap<String, String>, n: &'a str) -> String {",
            Shared,
            "a name substitution, and the variant names a module declares itself",
        ),
        sec(
            "fn rewrite_type(ty: &mut Type, map: &HashMap<String, String>) {",
            Job,
            "every referenced type name rewritten through a map — the same \
             shared descent, assigning where `type_names` clones",
        ),
        sec(
            "pub(crate) fn rewrite_names(p: &mut Program, map: &HashMap<String, String>) {",
            Job,
            "every reference to a declaration name rewritten through a map. The \
             walk is `body_scope_descent`'s since RFC-0125 §3 M6, where it was \
             the SECOND of three copies of it and its own comment said so — \"the \
             same walk `fn_body_ref_names` uses\". What stays here is the \
             substitution, and the three things it must not fold: a namespace \
             receiver, a local, and the module's own enum constructor",
        ),
        sec(
            "fn program_ref_names(p: &Program) -> HashSet<String> {",
            Job,
            "the program-wide reference sets the alias check and the runtime \
             injection both ask for. It READS the walk above rather than writing \
             a fourth one, which is what the other two rows should do",
        ),
        sec(
            "fn rename_decls_in_module(p: &mut Program, map: &HashMap<String, String>, ns: &HashSet<String>) {",
            Job,
            "every rename a module needs, applied in ONE walk rather than one \
             walk per rename (RFC-0125 §3 M4: `std/runtime` is 1,951 lines in \
             every program, and the per-rename form was quadratic in it)",
        ),
        sec("mod tests {", Tests, "the file's own unit tests"),
    ]
}

/// `symbols.rs` — the symbol table the LSP reads. The sections, in file order.
fn symbols_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec(
            "pub enum SymbolKind {",
            Shared,
            "the module head and the whole data model one keystroke produces: \
             the symbols, the tokens, the local bindings, the memory notes, the \
             namespaces, a resolution and a completion",
        ),
        sec(
            "pub fn analyze(source: &str) -> Analysis {",
            Job,
            "the two entry points, one over a bare file and one over a linked \
             project",
        ),
        sec(
            "fn adopt_foreign(mut d: Diagnostic) -> Diagnostic {",
            Shared,
            "a diagnostic from another file, shown against this one",
        ),
        sec(
            "fn analyze_inner(",
            Job,
            "one keystroke: lex, parse, link, check, and index everything the \
             editor asks for afterwards. Since RFC-0125 §3 M3 it ASKS the \
             checker for the type of every node and the type of every `let` \
             rather than deciding either itself, and it asks `movecheck` for the \
             ownership refusals — so this is the reading, not a second judgment",
        ),
        sec(
            "fn memory_notes(program: &crate::ast::Program) -> Vec<MemoryNote> {",
            Job,
            "the ownership notes a hover shows, read off `own::analyze` — the \
             same table `vyrn why --memory` reports, filtered the same way",
        ),
        sec(
            "fn empty_analysis(diagnostics: Vec<Diagnostic>) -> Analysis {",
            Shared,
            "what a lex or parse failure hands back",
        ),
        sec(
            "fn keyword_text(t: &Tok) -> Option<String> {",
            Job,
            "the source spelling of a keyword or operator token, READ off \
             `lexer::token_name_and_text` since RFC-0125 §3 M6. It was a \
             thirty-four-line copy of twenty-four of that table's eighty arms, \
             and it had already lost `import`, `export`, `break` and `continue`",
        ),
        sec(
            "fn backtick_tokens(msg: &str) -> Vec<&str> {",
            Shared,
            "the text inside each backtick-quoted span of a message",
        ),
        sec(
            "fn pin_diagnostics(",
            Job,
            "a line-only diagnostic pinned to the column of the token it \
             backtick-quotes, so the editor squiggles the name rather than the \
             line",
        ),
        sec(
            "pub fn resolve(analysis: &Analysis, line: usize, col: usize) -> Option<Resolution> {",
            Job,
            "hover and go-to-definition: what the identifier under the cursor \
             names, and where it is declared",
        ),
        sec(
            "static BUILTIN_TYPES_AND_CTORS: &[(&str, SymbolKind, &str)] = &[",
            Copy,
            "the six builtin sum names with their hover text. ONE TABLE inside \
             this file — the completion loop and the colouring list are filters \
             over it — but the same six names are `loader::builtin_alias_exports` \
             too, split across two module names, and a test compares them",
        ),
        sec(
            "fn enclosing_fn_line(analysis: &Analysis, cursor_line: usize) -> Option<usize> {",
            Shared,
            "the function a cursor line falls in, and whether a position is at \
             module scope",
        ),
        sec(
            "pub fn completions(analysis: &Analysis) -> Vec<Completion> {",
            Job,
            "what a bare cursor offers",
        ),
        sec(
            "pub fn member_completions(analysis: &Analysis, line: usize, col: usize) -> Vec<Completion> {",
            Job,
            "what a `.` offers: the receiver's fields, its impl methods, its \
             protocol members, and the builtin methods its type answers to",
        ),
        sec(
            "pub fn string_literal_completions(",
            Job,
            "the completions inside a string literal: a `.vyx` class name, the \
             CSS rule behind it, and the finite string type a position expects",
        ),
        sec(
            "fn receiver_before_dot(analysis: &Analysis, line: usize, col: usize) -> Option<String> {",
            Job,
            "the receiver before the dot, and its type — read off the local \
             index, which is filled from the checker's answers",
        ),
        sec(
            "fn decl_lines(program: &ast::Program) -> Vec<usize> {",
            Shared,
            "the declaration lines a search is bounded by, and the column a name \
             occupies on its line",
        ),
        sec(
            "fn index_symbols(program: &ast::Program, tok_info: &[TokenInfo], lines: &[usize]) -> Vec<Symbol> {",
            Job,
            "the root module's declarations, indexed with their kind, their \
             detail line and their doc",
        ),
        sec(
            "fn index_imported_symbols(",
            Job,
            "the declarations the root imports, indexed from the linked program \
             so hover and go-to-definition cross a file boundary",
        ),
        sec(
            "fn index_namespaces(",
            Job,
            "RFC-0027's namespace bindings and the exports each reaches",
        ),
        sec(
            "pub(crate) struct OriginIndex {",
            Job,
            "RFC-0073 M3's symbol map: a symbol a generator baked in resolves to \
             the DECLARATION it stands for, in its own file, at its own line",
        ),
        sec(
            "fn index_locals(",
            Twice,
            "a second walk over every body, for the binder POSITIONS the checker \
             does not record. The types are the checker's — `let_types` is \
             passed in — and since RFC-0125 §3 M6's third slice the descent is \
             `ast::body_scope_descent!`'s, so what is stated twice is the PASS \
             and the binding forms it knows, beside `checker::Scope` and \
             `pattern_binders`. A checker that recorded a binder's column would \
             delete this",
        ),
        sec(
            "fn with_doc(detail: &str, doc: &Option<String>) -> String {",
            Job,
            "the detail line every hover shows, one renderer per declaration \
             form: a local, a function, a global, a protocol member, a field, a \
             type, a variant",
        ),
        sec(
            "pub fn type_to_string(ty: &Type) -> String {",
            Shared,
            "a type spelled for a reader — the AST's own `Display`, with the \
             richer per-variant arm rendering an enum hover wants",
        ),
        sec(
            "pub struct DocExport {",
            Job,
            "RFC-0065's `vyrn doc` model: an exported declaration's rendered \
             signature and its `///` block, plus the module header doc",
        ),
        sec(
            "pub enum SemKind {",
            Job,
            "the semantic-token model the editor colours by",
        ),
        sec(
            "static MACRO_BUILTINS: &[&str] = &[",
            Copy,
            "thirty builtin free-function names, every one of them a row of \
             `checker::RESERVED`. Its own comment says what it is: \"Kept in \
             sync with the checker's `RESERVED` list.\" It is not the whole of \
             `RESERVED` — the method builtins, the type names, the contextual \
             words and the sum constructors are excluded — so deleting it needs \
             `RESERVED` to carry the column that says which is which",
        ),
        sec(
            "fn is_constructor_builtin(name: &str) -> bool {",
            Job,
            "the semantic tokens themselves: a symbol kind mapped to a colour, \
             and every token classified",
        ),
        sec(
            "pub struct InlayHint {",
            Job,
            "the inferred types an inlay hint shows",
        ),
        sec(
            "pub struct RefRange {",
            Job,
            "find-references and rename: every occurrence of the name under the \
             cursor, locals scoped to their function and declarations to the \
             whole file",
        ),
        sec(
            "fn classify_token(analysis: &Analysis, tok: &TokenInfo) -> Option<(SemKind, SemMods)> {",
            Job,
            "one arm per way a token can be classified, in the order the editor \
             needs them tried",
        ),
        sec(
            "struct BuiltinMethod {",
            Copy,
            "thirty-four builtin METHOD names with their hover text, and the \
             per-receiver-type dispatch that decides which a `.` offers. Its own \
             comment says the dispatch \"mirrors the receiver-type dispatch in \
             the checker's `call()`\", and since RFC-0125 §3 M6 that dispatch is \
             the seeded rows of `prelude.rs` — a row's parameter 0 IS the \
             receiver type. The hover prose is this file's own and has no home \
             on a row yet, which is what the deletion costs",
        ),
        sec("mod tests {", Tests, "the file's own unit tests"),
    ]
}

/// `project.rs` — place projections (RFC-0091 M2). Not the project manifest:
/// the name is `project` as in projection. The sections, in file order.
fn project_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec(
            "pub const ELEM: &str = \"@slot\";",
            Job,
            "the module head and the two unspellable names: `@slot`, the \
             addressing floor, and `@at`, the dispatch site `a[i]` parses to",
        ),
        sec(
            "pub struct Projection {",
            Shared,
            "one access site's lowering: the statements to run first, then the \
             place",
        ),
        sec(
            "pub fn is_builtin_container(ty: &Type) -> bool {",
            Job,
            "does this receiver type declare a projection under this name — the \
             one lookup every engine calls",
        ),
        sec(
            "pub fn site(",
            Job,
            "what an access site becomes, memoized per site",
        ),
        sec(
            "struct OptExpansion {",
            Shared,
            "the memo and its compile-scope guard: an expansion is leaked for \
             the length of one compile and freed with the scope",
        ),
        sec(
            "pub fn store_index(",
            Job,
            "`a[i] = v` through a store projection",
        ),
        sec(
            "pub fn inline(f: &Function, recv: &Expr, args: &[Expr], line: usize) -> Result<Projection, String> {",
            Job,
            "the inliner: a projection body substituted at the access site, so \
             the borrow it yields lives inside the caller's frame from the first \
             instruction to the last and rule 2 of RFC-0089 holds by \
             construction",
        ),
        sec(
            "fn substituted(",
            Job,
            "the substitution itself: which arguments may go in place and which \
             bind a temporary",
        ),
        sec(
            "pub struct OptionalProjection {",
            Job,
            "RFC-0122's optional projection: the prologue, the decision, and the \
             place a hit yields",
        ),
        sec(
            "pub fn store_node(blk: &Block) -> Option<&Stmt> {",
            Job,
            "the node an engine writes through, in an expansion this pass \
             built. The STATEMENTS a store becomes are `parser::store_stmts` \
             since RFC-0125 §3 M6's desugar slice: the parser stated the same \
             rewrite for `a[i] = v`, down to the temporaries' names, and this \
             pass calls it now",
        ),
        sec(
            "pub fn iterate_loop(",
            Job,
            "RFC-0084's `for x in c` over a user container, built once and \
             memoized",
        ),
        sec(
            "fn collect_bindings(b: &mut Block, tag: usize, out: &mut HashMap<String, String>) {",
            Job,
            "the hygiene an inline needs: every binding an inlined body \
             introduces is renamed apart from the caller's",
        ),
        sec(
            "fn count_uses(b: &Block, name: &str) -> usize {",
            Job,
            "how often a parameter is read, whether under a lambda, and whether \
             under a loop — the three questions that decide substitution in \
             place. The first two READ the shared walk below; only the loop \
             question writes its own statement walk, because loop NESTING is \
             what it is asking about",
        ),
        sec(
            "pub fn walk_block(b: &mut Block, f: &mut impl FnMut(&mut Expr)) {",
            Shared,
            "THE shared mutable walk over every expression a body holds, \
             innermost-last. Four readers in this file, plus `loader.rs`'s panic \
             stamping and `vyrn test`'s builtin rewrite and `vyrn-lower`'s \
             effect scan. The descent is `ast::body_scope_descent!`'s since \
             RFC-0125 §3 M6's second body slice, read through its `after_expr` \
             hook; what is left here is the entry point and the one line these \
             readers write",
        ),
        sec(
            "pub fn is_place(e: &Expr) -> bool {",
            Shared,
            "the three predicates a place question needs: is this a place, what \
             is its root, does this body hold a `?`",
        ),
        sec("mod tests {", Tests, "the file's own unit tests"),
    ]
}

/// The three files, with their sections.
fn files() -> Vec<(&'static str, Vec<Section>)> {
    vec![
        ("loader.rs", loader_sections()),
        ("symbols.rs", symbols_sections()),
        ("project.rs", project_sections()),
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

/// The sections, with the span each holds: `(index, first line, last line)`,
/// one-based and inclusive. Every line of the file is in exactly one span.
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

/// How many diagnostic sites a span holds.
fn diags(lines: &[String], a: usize, b: usize) -> usize {
    lines[a - 1..b]
        .iter()
        .filter(|l| l.contains("Diagnostic::error(") || l.contains("Diagnostic::warning("))
        .count()
}

/// The sections tile each file: every line is in one, in file order.
#[test]
fn the_frontend_census_covers_every_file() {
    for (file, secs) in files() {
        let lines = source(file);
        let spans = spans(file, &lines, &secs);
        let mut next = 1;
        for (_, a, b) in &spans {
            assert_eq!(*a, next, "a gap or an overlap at line {a} of {file}");
            next = b + 1;
        }
        assert_eq!(
            next - 1,
            lines.len(),
            "the last section does not reach the end of {file}"
        );
    }
}

/// The line count and the diagnostic count per kind, per file, as RFC-0125 §3
/// M6 records them. The prose quotes these numbers, so they are asserted rather
/// than described: a change to one of the three files moves one, and the RFC's
/// table moves with it.
#[test]
fn the_frontend_census_is_what_the_rfc_records() {
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
    let want = vec![
        ("loader.rs", "the file's own job", 4192, 23),
        ("loader.rs", "a rule stated a second time", 0, 0),
        ("loader.rs", "a path only a deleted route reached", 0, 0),
        (
            "loader.rs",
            "a copy of a table another module carries",
            15,
            0,
        ),
        ("loader.rs", "shared machinery", 543, 0),
        ("loader.rs", "tests", 137, 0),
        ("symbols.rs", "the file's own job", 2897, 0),
        ("symbols.rs", "a rule stated a second time", 272, 0),
        ("symbols.rs", "a path only a deleted route reached", 0, 0),
        (
            "symbols.rs",
            "a copy of a table another module carries",
            228,
            0,
        ),
        ("symbols.rs", "shared machinery", 435, 0),
        ("symbols.rs", "tests", 861, 0),
        ("project.rs", "the file's own job", 1008, 0),
        ("project.rs", "a rule stated a second time", 0, 0),
        ("project.rs", "a path only a deleted route reached", 0, 0),
        (
            "project.rs",
            "a copy of a table another module carries",
            0,
            0,
        ),
        ("project.rs", "shared machinery", 273, 0),
        ("project.rs", "tests", 309, 0),
    ];
    assert_eq!(got, want, "the frontend census has moved");
}

/// The table for RFC-0125 §3 M6, printed from the sections above:
/// `cargo test -p vyrn-cli --test frontend_census -- --ignored --nocapture
/// the_frontend_census_as_a_table`.
#[test]
#[ignore]
fn the_frontend_census_as_a_table() {
    for (file, secs) in files() {
        let lines = source(file);
        println!("\n### `{file}` — {} lines\n", lines.len());
        println!("| section | lines | diagnostics | kind | what it is |");
        println!("|---|---|---|---|---|");
        for (i, a, b) in spans(file, &lines, &secs) {
            let name = secs[i]
                .at
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .trim_end_matches(" {")
                .trim_end_matches(" = &[")
                .trim_end_matches('(')
                .to_string();
            println!(
                "| `{}` | {} | {} | {} | {} |",
                name,
                b - a + 1,
                diags(&lines, a, b),
                secs[i].kind.label(),
                secs[i].what
            );
        }
    }
}
