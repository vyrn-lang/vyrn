//! The structural census of `parser.rs` and `lexer.rs`, by the method of
//! `refusals.rs`. A section is an anchor line, from its doc comment, up to the next
//! anchor's, so the sections tile the file. Each has a kind that answers "is the parser
//! the right home for this?", and a count of the `Diagnostic::` sites it holds, because
//! a rule that leaves a file moves both numbers. An anchor is usually an item; one is
//! the `impl` flattening that ends `parse_accum`, because a section is what a reader would
//! delete. `ast.rs` and `fmt.rs` are not tiled: `surface.rs` and `forms.rs` price the
//! AST a constructor at a time, and `fmt.rs` names no form and no keyword.

use std::path::{Path, PathBuf};

/// What a section is.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Kind {
    /// A production: only the parser sees a token. A production that also rewrites
    /// is `Grammar`.
    Grammar,
    /// A section that exists only to state a rewrite. The parser is the wrong home
    /// when another pass restates it, the right one when another pass keeps a special
    /// case for a form already rewritten away.
    Desugar,
    /// A table stated a second time: a deletion candidate.
    Twice,
    /// Error recovery and the diagnostic sentences.
    Recovery,
    /// Shared machinery: the cursor, the state, the entry points.
    Shared,
    /// The file's own unit tests.
    Tests,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Grammar => "the grammar's own arm",
            Kind::Desugar => "a desugar the parser states",
            Kind::Twice => "a table stated a second time",
            Kind::Recovery => "recovery and the diagnostic sentences",
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

/// The sections of `parser.rs`, in file order; the first starts at line 1.
fn parser_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec("pub fn is_member_type_param(name: &str) -> bool {", Grammar),
        sec("fn mark_member_type_params(ty: &mut Type) {", Desugar),
        sec("fn at_contract_decl(tokens: &[Token], pos: usize) -> bool {", Grammar),
        sec("pub const METHOD_BUILTINS: &[(&str, &str)] = &[", Twice),
        sec("pub fn method_surface(internal: &str) -> &str {", Shared),
        sec("pub fn method_builtin(name: &str) -> Option<&'static str> {", Shared),
        sec("fn unshadow_method_builtins(program: &mut Program) {", Desugar),
        sec("pub(crate) fn parse_bare(tokens: Vec<Token>) -> (Program, Vec<Diagnostic>) {", Shared),
        sec("    let mut flat = Vec::new();", Desugar),
        sec("struct Parser {", Shared),
        sec("fn as_fn_body(src: &str) -> String {", Desugar),
        sec("fn is_index_field_chain(e: &Expr) -> bool {", Desugar),
        sec("pub fn place_receiver(", Desugar),
        sec("fn reads_place(e: &Expr) -> bool {", Desugar),
        sec("pub fn hoist_operand(e: Expr, name: String, hoists: &mut Vec<Stmt>, line: usize) -> Expr {", Desugar),
        sec("fn hoist_mutating_receiver(e: &mut Expr, line: usize) -> Option<(Vec<Stmt>, Vec<Stmt>)> {", Desugar),
        sec("pub fn store_stmts(place: &Expr, value: &Expr, line: usize) -> Option<Vec<Stmt>> {", Desugar),
        sec("impl Parser {", Shared),
        sec("fn peek(&self) -> &Tok {", Shared),
        sec("fn take_docs(&mut self) -> Option<String> {", Grammar),
        sec("fn col(&self) -> usize {", Shared),
        sec("fn eat(&mut self, expected: &Tok) -> Result<(), Diagnostic> {", Recovery),
        sec("fn place_root(&mut self) -> Result<String, Diagnostic> {", Grammar),
        sec("fn expect_ident(&mut self) -> Result<String, Diagnostic> {", Recovery),
        sec("fn program_accum(&mut self) -> (Program, Vec<Diagnostic>) {", Grammar),
        sec("fn sync_to_decl(&mut self) {", Recovery),
        sec("fn protocol_decl(&mut self) -> Result<ProtocolDecl, Diagnostic> {", Grammar),
        sec("fn contract_decl(&mut self) -> Result<ContractDecl, Diagnostic> {", Grammar),
        sec("fn contract_member_type(&mut self) -> Result<Type, Diagnostic> {", Grammar),
        sec("fn impl_block(&mut self) -> Result<ImplBlock, Diagnostic> {", Grammar),
        sec("fn parse_self_capability(&mut self) -> Capability {", Grammar),
        sec("fn parse_result_capability(&mut self) -> Result<Option<Capability>, Diagnostic> {", Grammar),
        sec("fn impl_method(", Grammar),
        sec("fn logging_config(&mut self) -> Result<(usize, LogSink), Diagnostic> {", Grammar),
        sec("fn import_decl(&mut self) -> Result<ImportDecl, Diagnostic> {", Grammar),
        sec("fn type_decl(&mut self) -> Result<Vec<TypeDecl>, Diagnostic> {", Grammar),
        sec("fn parse_capability(&mut self) -> Capability {", Grammar),
        sec("fn enum_type(&mut self) -> Result<Type, Diagnostic> {", Grammar),
        sec("fn record_type(&mut self) -> Result<Type, Diagnostic> {", Grammar),
        sec("fn type_param_binder(", Grammar),
        sec("fn function(&mut self, is_gen: bool) -> Result<Function, Diagnostic> {", Grammar),
        sec("fn named_block(&mut self, word: &str) -> Result<NamedBlock, Diagnostic> {", Grammar),
        sec("fn extern_function(&mut self, exported: bool) -> Result<Function, Diagnostic> {", Grammar),
        sec("fn type_(&mut self) -> Result<Type, Diagnostic> {", Grammar),
        sec("const MAX_NEST: u32 = 1024;", Shared),
        sec("fn block(&mut self) -> Result<Block, Diagnostic> {", Grammar),
        sec("fn sync_to_stmt(&mut self) {", Recovery),
        sec("fn global_decl(&mut self) -> Result<GlobalDecl, Diagnostic> {", Grammar),
        sec("fn if_stmt(&mut self, line: usize) -> Result<Stmt, Diagnostic> {", Grammar),
        sec("fn else_tail(&mut self) -> Result<Option<Block>, Diagnostic> {", Desugar),
        sec("fn if_let_stmt(&mut self, line: usize) -> Result<Stmt, Diagnostic> {", Grammar),
        sec("fn spliced(&mut self, mut stmts: Vec<Stmt>) -> Stmt {", Desugar),
        sec("fn refutable_let(&mut self, line: usize, mutable: bool) -> Result<Stmt, Diagnostic> {", Desugar),
        sec("fn stmt(&mut self) -> Result<Stmt, Diagnostic> {", Grammar),
        sec("fn expr(&mut self) -> Result<Expr, Diagnostic> {", Grammar),
        sec("fn binop(tok: &Tok) -> Option<(BinOp, u8)> {", Twice),
        sec("const NULLISH_BP: u8 = 5;", Grammar),
        sec("fn nullish(lhs: Expr, rhs: Expr, line: usize) -> Expr {", Desugar),
        sec("fn unary(&mut self) -> Result<Expr, Diagnostic> {", Grammar),
        sec("fn postfix(&mut self) -> Result<Expr, Diagnostic> {", Grammar),
        sec("fn at_lambda(&self) -> bool {", Grammar),
        sec("fn primary(&mut self) -> Result<Expr, Diagnostic> {", Grammar),
        sec("fn template(", Desugar),
        sec("fn tagged_template(", Desugar),
        sec("fn code_quote(", Desugar),
        sec("fn skeleton_error_detail(&self, skel: &str) -> (String, usize, usize) {", Recovery),
        sec("fn match_expr(&mut self, line: usize) -> Result<Expr, Diagnostic> {", Grammar),
        sec("fn storage_desugar(name: &str, args: &[Expr], line: usize) -> Option<Expr> {", Desugar),
        sec("fn if_expr(&mut self, line: usize) -> Result<Expr, Diagnostic> {", Grammar),
        sec("fn struct_lit(&mut self, name: String, line: usize) -> Result<Expr, Diagnostic> {", Grammar),
        sec("fn pattern(&mut self) -> Result<Pattern, Diagnostic> {", Grammar),
        sec("mod tests {", Tests),
    ]
}

/// The sections of `lexer.rs`, in file order.
fn lexer_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec("pub enum Tok {", Grammar),
        sec("pub struct Token {", Shared),
        sec(
            "pub fn token_name_and_text(tok: &Tok) -> (String, String) {",
            Shared,
        ),
        sec("pub struct Triv {", Shared),
        sec("macro_rules! keywords {", Twice),
        sec("macro_rules! punctuation {", Twice),
        sec("fn single_char_op(c: char) -> Option<Tok> {", Twice),
        sec(
            "pub fn scan(src: &str) -> Result<Scan, Diagnostic> {",
            Grammar,
        ),
        sec("fn parse_unicode_escape(", Shared),
        sec(
            "pub fn lex(src: &str) -> Result<Vec<Token>, Diagnostic> {",
            Shared,
        ),
        sec("mod tests {", Tests),
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
        // An anchor that IS a comment line starts its own section: walking up
        // from it would swallow the run it belongs to.
        anchors.push(if s.at.trim_start().starts_with("//") {
            hits[0]
        } else {
            doc_start(lines, hits[0])
        });
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
    let want = vec![
        ("parser.rs", "the grammar's own arm", 3172, 52),
        ("parser.rs", "a desugar the parser states", 963, 7),
        ("parser.rs", "a table stated a second time", 87, 0),
        ("parser.rs", "recovery and the diagnostic sentences", 152, 2),
        ("parser.rs", "shared machinery", 199, 1),
        ("parser.rs", "tests", 1877, 0),
        ("lexer.rs", "the grammar's own arm", 531, 11),
        ("lexer.rs", "a desugar the parser states", 0, 0),
        ("lexer.rs", "a table stated a second time", 138, 0),
        ("lexer.rs", "recovery and the diagnostic sentences", 0, 0),
        ("lexer.rs", "shared machinery", 191, 2),
        ("lexer.rs", "tests", 234, 0),
    ];
    assert_eq!(got, want, "the parser census has moved");
}
