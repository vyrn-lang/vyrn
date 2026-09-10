//! The structural census of the parser and the lexer — RFC-0125 §3 M6, the
//! size strand.
//!
//! Five files of `vyrn-frontend` carry a census: the checker
//! (`tests/checker_census.rs`), the ownership pass (`tests/refusals.rs`), the
//! loader, the symbol map and the project pass (`tests/frontend_census.rs`).
//! The emitter and the CLI have their own. **Nobody had counted the parser**,
//! and at this census `parser.rs` was 7,497 lines — the second largest file
//! in the frontend after the checker. With `lexer.rs` (1,453), `ast.rs`
//! (1,950) and `fmt.rs` (857) the front of the compiler was 11,757 lines, and
//! RFC-0125 §2.7's estimate cannot be met without knowing what is in them. The
//! counts this test pins are the current ones, not those.
//!
//! `ast.rs` and `fmt.rs` are not tiled here and the record says why: `ast.rs`
//! is the surface's declaration, which RFC-0126 and RFC-0127 already price a
//! constructor at a time, and `fmt.rs` names no form and no keyword
//! (RFC-0127 §3.3 measures both zeros).
//!
//! # The method, which is `frontend_census.rs`'s, which is `refusals.rs`'s
//!
//! A section is one anchor line together with every line after it up to the
//! next anchor's. The span runs from the anchor's own doc comment, so every
//! line of a file belongs to exactly one section and the counts add up to the
//! file. The test computes the spans; the table below records the anchor, the
//! kind and a reader.
//!
//! An anchor is usually an item. One is not: `parse_accum` ends with a job of
//! its own — the `impl` flattening — and a section is what a reader would
//! delete, not what `rustfmt` indents. The function held a second such anchor
//! until RFC-0125 §3 M6 moved the 535-line prelude out of it.
//!
//! # The kinds, and why these
//!
//! The loader's census asks "is this rule stated anywhere else?". A parser's
//! answer to that is always "the grammar is stated once", so the question that
//! fits here is RFC-0125 §1.1's other one — **is the parser the right home for
//! this?**
//!
//! * [`Kind::Grammar`] — the grammar's own arm. One production, one place, and
//!   nothing else in the compiler can state it: only the parser sees a token.
//! * [`Kind::Desugar`] — a rewrite the parser states. The surface form goes in
//!   and a different tree comes out, and every row names where the checker,
//!   `vyrn-lower` or `symbols.rs` states the same rewrite or a special case of
//!   it. These are the rows a deletion argument is made from, in both
//!   directions: the parser is the wrong home when another pass restates the
//!   rewrite, and the right one when another pass keeps a special case for a
//!   form the parser has already rewritten away.
//! * [`Kind::Twice`] — a table stated a second time. The deletion candidates
//!   proper; each row names the other statement.
//! * [`Kind::Recovery`] — error recovery and the diagnostic sentences. A parse
//!   error's wording is not the grammar, and RFC-0006 is why it is here.
//! * [`Kind::Shared`] — shared machinery: the cursor, the state, the entry
//!   points.
//! * [`Kind::Tests`] — the file's own unit tests.
//!
//! A production that also rewrites is `Grammar`, and its row names the
//! rewrite. `Desugar` is for a section that exists only to state one — the rule
//! keeps the tiling from turning into an opinion.
//!
//! The second column is the number of `Diagnostic::` sites a section holds, for
//! `checker_census.rs`'s reason: a rule that leaves a file moves both numbers.

use std::path::{Path, PathBuf};

/// What a section is.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Kind {
    /// The grammar's own arm. One production, one place.
    Grammar,
    /// A rewrite the parser states, with the pass that restates it or keeps a
    /// special case of it named in the row.
    Desugar,
    /// A table stated a second time. Each row names the other statement.
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

/// One section: the exact source line that starts it, its kind, and what it is.
struct Section {
    at: &'static str,
    kind: Kind,
    what: &'static str,
}

const fn sec(at: &'static str, kind: Kind, what: &'static str) -> Section {
    Section { at, kind, what }
}

/// `parser.rs` — the recursive-descent parser. The sections, in file order; the
/// first starts at line 1.
fn parser_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec(
            "pub fn is_member_type_param(name: &str) -> bool {",
            Grammar,
            "a contract member's implicit type parameter, recognised by its \
             spelling (RFC-0071). There is no declaration site to consult, so \
             the rule IS the spelling and the parser is the only pass that \
             sees one",
        ),
        sec(
            "fn mark_member_type_params(ty: &mut Type) {",
            Twice,
            "a contract member's implicit type parameter turned into a \
             `Type::Param` (RFC-0071). The DESCENT is \
             `loader::type_head_descent!`'s since RFC-0125 §3 M6, where the \
             macro's hook moved from a type's NAME to the node — a `&mut \
             String` cannot replace a `Type::Named` with a `Type::Param`, \
             which is why this stayed the fourth hand-written copy of those \
             arms until then. What is left is the one node it replaces, and \
             the `Twice` is the spelling rule above stated at a second site",
        ),
        sec(
            "fn at_contract_decl(tokens: &[Token], pos: usize) -> bool {",
            Grammar,
            "the contextual `contract Name {` starter — three tokens of \
             lookahead, because four `std/` generators take a parameter called \
             `contract`",
        ),
        sec(
            "pub const METHOD_BUILTINS: &[(&str, &str)] = &[",
            Twice,
            "the 24 builtins written `x.m(..)` and nowhere else: the surface \
             spelling and the unspellable internal name. `symbols.rs`'s \
             `ALL_BUILTIN_METHODS` is a second copy of the left column, with \
             the completion detail against each; its own test asserts every \
             row here has an entry there, which is what a copy costs when it \
             cannot be deleted",
        ),
        sec(
            "pub fn method_surface(internal: &str) -> &str {",
            Shared,
            "the table read backwards, for a diagnostic that must name what \
             the programmer wrote and not `@push`",
        ),
        sec(
            "pub fn method_builtin(name: &str) -> Option<&'static str> {",
            Shared,
            "the table read forwards",
        ),
        sec(
            "fn unshadow_method_builtins(program: &mut Program) {",
            Desugar,
            "the method-form rewrite given BACK wherever the module can \
             resolve the surface name itself. The rewrite happens in \
             `postfix`, before any declaration is in; this is its second half \
             and it is the parser's own — no later pass could undo a rewrite \
             it cannot see",
        ),
        sec(
            "pub(crate) fn parse_bare(tokens: Vec<Token>) -> (Program, Vec<Diagnostic>) {",
            Shared,
            "the three entry points: the grammar alone, the first error, and \
             every error one pass recovered (RFC-0006). `parse_accum` also \
             puts the language's PRELUDE into the program — fifteen type \
             declarations at line 0 — and since RFC-0125 §3 M6 it does that by \
             extending from `prelude::type_decls`, which parses `prelude.vyrn` \
             through the bare entry point. It was 535 lines of Rust building \
             those declarations as AST values, and none of it was a grammar",
        ),
        sec(
            "    let mut flat = Vec::new();",
            Desugar,
            "every `impl P for T` method flattened to a mangled top-level \
             function, so nothing below the parser knows the feature exists. \
             The mangling itself is `types::impl_method_name`, which the \
             checker calls again to resolve a protocol call by the receiver's \
             type — one rule, two callers, which is what a shared mangling is \
             for",
        ),
        sec(
            "struct Parser {",
            Shared,
            "the cursor and the seven pieces of state a production reads: the \
             no-struct flag, the generic parameters in scope, the associated \
             types, the inline field refinements, the desugar's statement \
             queue, the recovered errors and the nesting depth — and `binop_text`, \
             which renders an operator for a diagnostic out of the precedence \
             table and the lexer's spellings",
        ),
        sec(
            "fn as_fn_body(src: &str) -> String {",
            Desugar,
            "a code-quote skeleton wrapped as a function body — the \
             statement-list mode's one statement, which the mode and the \
             error detail both read (RFC-0054). Spelled twice until \
             RFC-0125 §3 M6's desugar slice, and the error's line is the \
             wrapped line minus the wrapper's own",
        ),
        sec(
            "fn is_index_field_chain(e: &Expr) -> bool {",
            Desugar,
            "whether a write target bottoms out in `a[i]`, which is what \
             separates the two-deep write-through the desugar below handles \
             from the three-deep one it refuses",
        ),
        sec(
            "pub fn place_receiver(",
            Desugar,
            "the plain variable an in-place container mutation writes through \
             (RFC-0082 M1). `pub` for `project.rs`, which asks the same \
             question of the same tree when a projection resolves to a place. \
             `movecheck.rs` does NOT call it — it recognises the temporary the \
             parser mints by its name, through `ast::is_place_temp` and \n             `ast::hoisted_value`, which are the one statement of that reading",
        ),
        sec(
            "fn reads_place(e: &Expr) -> bool {",
            Desugar,
            "whether an operand could read a place, and so has to be \
             evaluated before the container moves out (RFC-0082 M2)",
        ),
        sec(
            "pub fn hoist_operand(e: Expr, name: String, hoists: &mut Vec<Stmt>, line: usize) -> Expr {",
            Desugar,
            "one operand bound to a temporary ahead of the move-out. `pub` \
             for `project.rs`, which hoists the same way when it rewrites a \
             loop body",
        ),
        sec(
            "fn hoist_mutating_receiver(e: &mut Expr, line: usize) -> Option<(Vec<Stmt>, Vec<Stmt>)> {",
            Desugar,
            "`r.a.pop()` — the receiver of a mutating method moved into a slot \
             and back around the statement, because the checker and both \
             backends demand a plain variable there",
        ),
        sec(
            "pub fn store_stmts(place: &Expr, value: &Expr, line: usize) -> Option<Vec<Stmt>> {",
            Desugar,
            "the statements a store through a place becomes — the ONE \
             statement of RFC-0082 M1's rewrite. `Parser::stmt` reaches it for \
             `a[i] = v` and `project.rs` for a store through a projection \
             (RFC-0091 M3); the two spelled the same rewrite, down to the \
             temporaries' names, until RFC-0125 §3 M6's desugar slice",
        ),
        sec(
            "impl Parser {",
            Shared,
            "where the productions begin — and the parser's own state, stated \
             once: `over` for an entry point's nine fields and `sub` for a \
             re-lexing desugar's, which adds the enclosing declaration's \
             generic parameters and type aliases to them. Four places built \
             that record by hand",
        ),
        sec(
            "fn peek(&self) -> &Tok {",
            Shared,
            "the cursor: look, look ahead, the current line and column, take \
             one, and the optional `;`",
        ),
        sec(
            "fn take_docs(&mut self) -> Option<String> {",
            Grammar,
            "`///` comments joined and attached to the declaration that \
             follows; discarded anywhere else",
        ),
        sec(
            "fn col(&self) -> usize {",
            Shared,
            "the column, and the token taken",
        ),
        sec(
            "fn eat(&mut self, expected: &Tok) -> Result<(), Diagnostic> {",
            Recovery,
            "the `expected X, found Y` sentence, and the `>>` split that lets \
             `Array<Array<T>>` close two generics with one token",
        ),
        sec(
            "fn place_root(&mut self) -> Result<String, Diagnostic> {",
            Grammar,
            "an assignment target's root name — an identifier or `self`",
        ),
        sec(
            "fn expect_ident(&mut self) -> Result<String, Diagnostic> {",
            Recovery,
            "the `expected an identifier` sentence, which names what was found",
        ),
        sec(
            "fn program_accum(&mut self) -> (Program, Vec<Diagnostic>) {",
            Grammar,
            "the top-level production: which declaration each token starts, \
             including the six contextual starters (`gen`, `extern`, `test`, \
             `bench`, `logging`, `contract`), and the recovery that keeps one \
             bad declaration from hiding the next",
        ),
        sec(
            "fn sync_to_decl(&mut self) {",
            Recovery,
            "where a failed declaration resumes: the next top-level starter at \
             brace depth 0",
        ),
        sec(
            "fn protocol_decl(&mut self) -> Result<ProtocolDecl, Diagnostic> {",
            Grammar,
            "`protocol Name { type A  fn m(self, ..) -> R }` — the method \
             signatures and the associated types (RFC-0080 M2)",
        ),
        sec(
            "fn contract_decl(&mut self) -> Result<ContractDecl, Diagnostic> {",
            Grammar,
            "`contract Name { .. }` — a module contract (RFC-0071), and its \
             two member forms and the open rule `fn *(..)`",
        ),
        sec(
            "fn contract_member_type(&mut self) -> Result<Type, Diagnostic> {",
            Grammar,
            "a contract member's type, with its implicit parameters marked",
        ),
        sec(
            "fn impl_block(&mut self) -> Result<ImplBlock, Diagnostic> {",
            Grammar,
            "`impl P for T { .. }`, its `type` members and its generic binder \
             (RFC-0080 M1)",
        ),
        sec(
            "fn parse_self_capability(&mut self) -> Capability {",
            Twice,
            "`read`/`modify`/`consume` before `self`. The word-to-`Capability` \
             table, first of three statements in this file",
        ),
        sec(
            "fn parse_result_capability(&mut self) -> Result<Option<Capability>, Diagnostic> {",
            Twice,
            "`-> read T` / `-> modify T` (RFC-0120). The same table, second \
             statement, and the refusal of `-> consume T` beside it",
        ),
        sec(
            "fn impl_method(",
            Grammar,
            "one `fn m(read|modify|consume self, ..) -> R { .. }` inside an \
             `impl`, and the projection rule that ties the result's capability \
             to the receiver's",
        ),
        sec(
            "fn logging_config(&mut self) -> Result<(usize, LogSink), Diagnostic> {",
            Grammar,
            "`logging { level: .., sink: .. }` — the declaration with no field \
             on `Program` (RFC-0127 §3.2), and its sink spellings",
        ),
        sec(
            "fn import_decl(&mut self) -> Result<ImportDecl, Diagnostic> {",
            Grammar,
            "`import { a, b as c } from \"path\"`, `import * as ns` \
             (RFC-0027), `import type { .. }`, and the generator call a \
             specifier may be (RFC-0021)",
        ),
        sec(
            "fn type_decl(&mut self) -> Result<Vec<TypeDecl>, Diagnostic> {",
            Grammar,
            "`type Name = Base where p` and `type Name = { .. }`, and the \
             synthetic `Decl.field` types an inline refinement desugars to — \
             which is why it returns a `Vec`",
        ),
        sec(
            "fn parse_capability(&mut self) -> Capability {",
            Twice,
            "a parameter's capability, `share` included. The same table, third \
             statement — and the only one of the three that carries the fourth \
             word",
        ),
        sec(
            "fn enum_type(&mut self) -> Result<Type, Diagnostic> {",
            Grammar,
            "`| Variant(T) | Variant` — the leading `|` is what disambiguates \
             an enum from every other type form",
        ),
        sec(
            "fn record_type(&mut self) -> Result<Type, Diagnostic> {",
            Grammar,
            "`{ field: T, .. }`, the inline `where` on a field, and the \
             record-level one",
        ),
        sec(
            "fn type_param_binder(",
            Grammar,
            "`<T: Bound + Other, U>`, shared by `fn` and by `impl<..>` so the \
             two spell generics identically",
        ),
        sec(
            "fn function(&mut self, is_gen: bool) -> Result<Function, Diagnostic> {",
            Grammar,
            "`[gen] fn name<..>(params) -> Ret { body }`",
        ),
        sec(
            "fn named_block(&mut self, word: &str) -> Result<NamedBlock, Diagnostic> {",
            Grammar,
            "`test \"name\" { .. }` and `bench \"name\" { .. }` — one \
             production, because RFC-0127 §8 made them one declaration",
        ),
        sec(
            "fn extern_function(&mut self, exported: bool) -> Result<Function, Diagnostic> {",
            Grammar,
            "`extern fn` in both directions (RFC-0012)",
        ),
        sec(
            "fn type_(&mut self) -> Result<Type, Diagnostic> {",
            Grammar,
            "a type: the intersection `A & B`, the nesting counter's one \
             entry, and the 33 constructors RFC-0126 prices — the single \
             largest production in the file",
        ),
        sec(
            "const MAX_NEST: u32 = 1024;",
            Shared,
            "the nesting limit, and the counter that turns a stack overflow \
             into a diagnostic",
        ),
        sec(
            "fn block(&mut self) -> Result<Block, Diagnostic> {",
            Grammar,
            "`{ stmt* }`, the desugar queue drained in order, and the \
             within-block recovery RFC-0017's `vyrn fix` reads",
        ),
        sec(
            "fn sync_to_stmt(&mut self) {",
            Recovery,
            "where a failed statement resumes: the next statement boundary at \
             this block's depth",
        ),
        sec(
            "fn global_decl(&mut self) -> Result<GlobalDecl, Diagnostic> {",
            Grammar,
            "`let [mut] name [: T] = e` at the top level (RFC-0013), whose \
             initializer is required",
        ),
        sec(
            "fn if_stmt(&mut self, line: usize) -> Result<Stmt, Diagnostic> {",
            Grammar,
            "`if cond { .. }` in statement position",
        ),
        sec(
            "fn else_tail(&mut self) -> Result<Option<Block>, Diagnostic> {",
            Desugar,
            "`else if` and `else if let` (RFC-0022): the chained `if` becomes \
             the sole statement of a one-statement `else` block, so no pass \
             below the parser has an `else if` arm. Nothing restates it and \
             nothing keeps a special case for it — the cheapest desugar in the \
             file",
        ),
        sec(
            "fn if_let_stmt(&mut self, line: usize) -> Result<Stmt, Diagnostic> {",
            Grammar,
            "`if let PAT = e { .. } else { .. }` (RFC-0060), which is a real \
             node every pass below has an arm for",
        ),
        sec(
            "fn spliced(&mut self, mut stmts: Vec<Stmt>) -> Stmt {",
            Desugar,
            "how a desugar that produces several statements returns one: the \
             first, with the rest queued for `block` to splice",
        ),
        sec(
            "fn refutable_let(&mut self, line: usize, mutable: bool) -> Result<Stmt, Diagnostic> {",
            Desugar,
            "`let Variant(a, b) = v` (RFC-0121) — one `let` per binder, each a \
             `match` whose default arm panics. It mints `Pattern::Other`, \
             which no source can spell, and the canonical trap wording is \
             stated here and matched by `tests/traps.rs`",
        ),
        sec(
            "fn stmt(&mut self) -> Result<Stmt, Diagnostic> {",
            Grammar,
            "the statement production, and three rewrites inside it: `while \
             let` onto `while true { if let .. else break }`, `a[i] = v` onto \
             `@atSet`, and `a[i].f = v` onto the three-statement temporary \
             idiom",
        ),
        sec(
            "fn expr(&mut self) -> Result<Expr, Diagnostic> {",
            Grammar,
            "the expression entry, and the no-struct context an `if`/`while` \
             head parses in",
        ),
        sec(
            "fn binop(tok: &Tok) -> Option<(BinOp, u8)> {",
            Twice,
            "the 19 binary operators with their binding powers. The token is \
             the lexer's, the `BinOp` is the AST's, and the SPELLING was stated \
             a second time by `checker::pred_summary` until RFC-0125 §3 M6's \
             operator slice, which derived that one from this table and the \
             lexer's",
        ),
        sec(
            "const NULLISH_BP: u8 = 5;",
            Grammar,
            "`??`'s binding power, which is not a `BinOp`, and the Pratt loop \
             both it and the table drive",
        ),
        sec(
            "fn nullish(lhs: Expr, rhs: Expr, line: usize) -> Expr {",
            Desugar,
            "`a ?? b` (RFC-0079) onto the `match` that spells it. It is the \
             ONLY writer of `Pattern::Success` and `Pattern::Failure`: \
             `pattern()` has one arm and no source can spell either, so the \
             48 of each in the corpus are this desugar's 48 `??`",
        ),
        sec(
            "fn unary(&mut self) -> Result<Expr, Diagnostic> {",
            Grammar,
            "the three prefix operators, and the one place an expression's \
             nesting is counted",
        ),
        sec(
            "fn postfix(&mut self) -> Result<Expr, Diagnostic> {",
            Grammar,
            "the postfix chain, and the two rewrites on it: `a[i]` becomes \
             `@at(a, i)` and `x.m(..)` becomes the method builtin \
             `METHOD_BUILTINS` names, or `m(x, ..)` when it does not",
        ),
        sec(
            "fn at_lambda(&self) -> bool {",
            Grammar,
            "`x -> e`, `(x, y) -> e`, `() -> e` (RFC-0110). The arrow decides \
             and the scan is bounded by the parameter list",
        ),
        sec(
            "fn primary(&mut self) -> Result<Expr, Diagnostic> {",
            Grammar,
            "the atoms: every literal, the array and map literals, `spawn`, \
             `match`, `if`, a call, a struct literal, and the tag lookahead \
             that sends an identifier with a string against it to one of the \
             three template desugars",
        ),
        sec(
            "fn template(",
            Desugar,
            "`\"a\\{e}b\"` (RFC-0007) onto a `concat`/`str` chain. Nothing \
             below the parser has a template arm; the hole's raw source is \
             re-lexed here",
        ),
        sec(
            "fn tagged_template(",
            Desugar,
            "`tag\"a\\{e}b\"` onto `tag(list([parts]), list([value(e)]))`, \
             with each value boxed into the injected `Value` enum",
        ),
        sec(
            "fn code_quote(",
            Desugar,
            "`vyrn\"…\"` (RFC-0054) onto a `Code` value, and the skeleton \
             validated at the generator's compile time — the four modes, the \
             splice context of each hole, and the sub-parser they run in. \
             Seven sections and 220 lines for 15 quotes in the corpus",
        ),
        sec(
            "fn skeleton_error_detail(&self, skel: &str) -> (String, usize, usize) {",
            Recovery,
            "the message a skeleton that parses in no mode gets, and where in \
             the quote it points",
        ),
        sec(
            "fn match_expr(&mut self, line: usize) -> Result<Expr, Diagnostic> {",
            Grammar,
            "`match e { p => e, p => { .. } }`, expression arms and block arms \
             both (RFC-0118)",
        ),
        sec(
            "fn storage_desugar(name: &str, args: &[Expr], line: usize) -> Option<Expr> {",
            Desugar,
            "`save`/`load`/`loadOr` (RFC-0044) at the call site, because the \
             codec is type-name-directed and cannot be an ordinary generic. \
             Three calls in the whole corpus",
        ),
        sec(
            "fn if_expr(&mut self, line: usize) -> Result<Expr, Diagnostic> {",
            Grammar,
            "`if c { e } else { e }` in expression position (RFC-0030), whose \
             branches are single expressions and whose missing `else` is the \
             checker's diagnostic and not a parse error",
        ),
        sec(
            "fn struct_lit(&mut self, name: String, line: usize) -> Result<Expr, Diagnostic> {",
            Grammar,
            "`Name { field: e, .. }`",
        ),
        sec(
            "fn pattern(&mut self) -> Result<Pattern, Diagnostic> {",
            Grammar,
            "**one arm**: every identifier is a variant name, with or without \
             binders and with or without a namespace path. Three of the four \
             `Pattern` constructors RFC-0127 §3.1 prices have no surface \
             syntax at all",
        ),
        sec("mod tests {", Tests, "the file's own unit tests"),
    ]
}

/// `lexer.rs` — the hand-written scanner. The sections, in file order.
fn lexer_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec(
            "pub enum Tok {",
            Grammar,
            "the token set: 7 literal and identifier forms, the 24 keywords, \
             and the 36 punctuation and operator tokens",
        ),
        sec(
            "pub struct Token {",
            Shared,
            "a token with the line and column a diagnostic points at",
        ),
        sec(
            "pub fn token_name_and_text(tok: &Tok) -> (String, String) {",
            Shared,
            "the canonical `(kind, text)` of a token, which `lex()` hands a \
             generator (RFC-0054). Its 36 punctuation rows went in RFC-0125 \
             §3 M6's operator slice and its 24 keyword rows in the same \
             milestone's keyword slice. It asks `punct_text` and \
             `keyword_text`, and what is left is the literals, `Eof`, and \
             which of the two tables to ask",
        ),
        sec(
            "pub struct Triv {",
            Shared,
            "one lexical item (RFC-0017): the raw text verbatim, where it \
             starts, the lines it spans, and whether anything separated it \
             from the token before. Both readers of the source take these",
        ),
        sec(
            "macro_rules! keywords {",
            Twice,
            "**the one statement of the keyword table** since RFC-0125 §3 M6's              keyword slice: 24 rows, expanded into `keyword_or_ident` and into              the reverse lookup `token_name_and_text` answers with. RFC-0127              §3.4 measures 1 against every keyword now, where it measured 3              and then 2. The invocation is the anchor              `editor/vscode/test/grammar.test.mjs` and `tests/forms.rs` both              read as text, so the rows stay one per line and spelled              `word => Tok::Name`",
        ),
        sec(
            "macro_rules! punctuation {",
            Twice,
            "**the one statement of the punctuation table** since RFC-0125 \
             §3 M6's operator slice: 36 rows, expanded into the two scanners, \
             into the spelling `token_name_and_text` answers with, into the \
             reverse lookup `parser::binop_text` reaches through, and into the \
             list `tests/forms.rs` counts a corpus's operators against. It was \
             three tables and nothing checked that they agreed",
        ),
        sec(
            "fn single_char_op(c: char) -> Option<Tok> {",
            Twice,
            "the second scanner the table expands into, the invocation itself \
             — 36 rows — and the two readers below it",
        ),
        sec(
            "pub fn scan(src: &str) -> Result<Scan, Diagnostic> {",
            Grammar,
            "**the whole lexical grammar, stated once** (RFC-0125 §3 M6): \
             comments, plain and triple-quoted strings, interpolation holes, \
             byte literals, numbers, identifiers and operators, with every \
             literal DECODED where it is found. It was scanned twice until \
             then — once here, keeping raw text for RFC-0017's formatter, and \
             once inside `lex`, decoding as it went — and the two disagreed \
             about what is a legal token, which the older doc comment claimed \
             they could not",
        ),
        sec(
            "fn parse_unicode_escape(",
            Shared,
            "`\\u{..}` decoded, and the byte literal's own escapes and \
             refusals — the two pieces of literal decoding that ARE stated \
             once",
        ),
        sec(
            "pub fn lex(src: &str) -> Result<Vec<Token>, Diagnostic> {",
            Shared,
            "one of the scan's two readers: the comments dropped, a doc line \
             unwrapped into the token that carries its markdown, and the \
             stream closed with `Eof`. The other reader is `lex_with_trivia`, \
             which is the scan and nothing else",
        ),
        sec("mod tests {", Tests, "the file's own unit tests"),
    ]
}

/// The two files, with their sections.
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

/// How many diagnostic sites a span holds.
fn diags(lines: &[String], a: usize, b: usize) -> usize {
    lines[a - 1..b]
        .iter()
        .filter(|l| l.contains("Diagnostic::error(") || l.contains("Diagnostic::warning("))
        .count()
}

/// The sections tile each file: every line is in one, in file order.
#[test]
fn the_parser_census_covers_every_file() {
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
/// M6 records them.
#[test]
fn the_parser_census_is_what_the_rfc_records() {
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
        ("parser.rs", "the grammar's own arm", 3483, 52),
        ("parser.rs", "a desugar the parser states", 1032, 7),
        ("parser.rs", "a table stated a second time", 234, 1),
        ("parser.rs", "recovery and the diagnostic sentences", 174, 2),
        ("parser.rs", "shared machinery", 234, 1),
        ("parser.rs", "tests", 1953, 0),
        ("lexer.rs", "the grammar's own arm", 578, 11),
        ("lexer.rs", "a desugar the parser states", 0, 0),
        ("lexer.rs", "a table stated a second time", 164, 0),
        ("lexer.rs", "recovery and the diagnostic sentences", 0, 0),
        ("lexer.rs", "shared machinery", 221, 2),
        ("lexer.rs", "tests", 239, 0),
    ];
    assert_eq!(got, want, "the parser census has moved");
}

/// The table for RFC-0125 §3 M6, printed from the sections above:
/// `cargo test -p vyrn-cli --test parser_census -- --ignored --nocapture
/// the_parser_census_as_a_table`.
#[test]
#[ignore]
fn the_parser_census_as_a_table() {
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
