//! The tree builder. [`crate::recognizer`] matches `vyrn.syntax`'s rules over the
//! tokens and records what it matched as [`Event`]s; this module reads a finished
//! run's events into the [`Program`] and states the desugars.
//!
//! The grammar states shape only, so meaning lives here. The builder reads what a
//! rule matched and never decides what a token means: a token it asks about is one
//! the recognizer consumed. Each function here is total over what the grammar lets
//! through, so a mismatch is a defect in this file or in the grammar, and it panics.
//! The checks of a tree a recognizer rule needs ([`store_shape`], [`await_marks`],
//! [`push_has_place`], [`inline_where_outside_record`], [`ref_let_parts`]) are stated
//! here, and the recognizer raises their refusals as the rule closes.

use crate::ast::*;
use crate::diagnostics::Diagnostic;
use crate::lexer::{Hole, Tok, Token};
use crate::recognizer::{self, Cx, Event, HoleRun, Kind, Run, View};
use crate::rules::{rule, Rule};
use std::collections::HashSet;

/// Whether `name` in a contract member's type is an implicit type parameter:
/// one uppercase ASCII letter, optionally followed by digits (`T`,
/// `T1`). The narrow rule keeps a typo such as `Haed` a named type the checker
/// must resolve. `std/contract:isTypeParam` states the same rule in Vyrn.
pub fn is_member_type_param(name: &str) -> bool {
    let mut cs = name.chars();
    match cs.next() {
        Some(c) if c.is_ascii_uppercase() => cs.all(|c| c.is_ascii_digit()),
        _ => false,
    }
}

/// Rewrites every `Named(n)` in `ty` that [`is_member_type_param`] accepts into
/// a [`Type::Param`].
fn mark_member_type_params(ty: &mut Type) {
    crate::types::walk_type_mut(ty, &mut |t| {
        if let Type::Named(n) = t {
            if is_member_type_param(n) {
                *t = Type::Param(std::mem::take(n));
            }
        }
    });
}

/// Parses the grammar alone: no prelude, no module-declared method names, no
/// `impl` flattening. [`crate::prelude::type_decls`] parses `prelude.vyrn` through
/// this, because the prelude is what [`parse_accum`] adds.
pub(crate) fn parse_bare(mut tokens: Vec<Token>) -> (Program, Vec<Diagnostic>) {
    let answers = HashSet::new();
    let mut run = recognizer::file(&tokens, Cx::plain(&answers));
    let (program, _) = build(&mut tokens, &mut run, &answers, false);
    (program, run.errors)
}

/// Parses a token stream into a [`Program`], returning the first parse error.
/// [`parse_accum`] returns all of them.
pub fn parse(tokens: Vec<Token>) -> Result<Program, Diagnostic> {
    let (program, errors) = parse_accum(tokens);
    match errors.into_iter().next() {
        Some(d) => Err(d),
        None => Ok(program),
    }
}

/// Parses a token stream, recovering past bad top-level declarations so every
/// parse error is reported in one pass.
///
/// A failed declaration records its diagnostic and the cursor skips to the next
/// top-level starter. The `Program` holds the declarations that parsed; a caller
/// should not run later checks when the error list is non-empty.
pub fn parse_accum(tokens: Vec<Token>) -> (Program, Vec<Diagnostic>) {
    let (program, errors, _) = parse_with(tokens, false, false);
    (program, errors)
}

/// [`parse_accum`] for an imported module: its `test` and `bench` blocks are
/// skipped unparsed and left out of the program. With `skim`, the body of each
/// top-level function is left unparsed too, in an empty block, and
/// [`Skimmed::body`] parses it when a caller needs it.
pub fn parse_imported(tokens: Vec<Token>, skim: bool) -> (Program, Vec<Diagnostic>, Skimmed) {
    parse_with(tokens, true, skim)
}

/// The top-level function bodies a skimming [`parse_imported`] left unparsed.
pub struct Skimmed {
    /// The module's tokens. The tree holds the payloads of those it read, and a
    /// skimmed body's stay as the lexer wrote them.
    tokens: Vec<Token>,
    /// Each skimmed function's body, as a range of `tokens`, by the line and
    /// column of the function's name.
    bodies: Vec<SkimmedBody>,
    /// The method-form names the module answers to, which a body's parse reads.
    answers: HashSet<String>,
}

impl Skimmed {
    /// The tokens of `f`'s skimmed body, or `None` when the parse did not skim
    /// it.
    pub fn tokens(&self, f: &Function) -> Option<&[Token]> {
        let (_, range) = self.bodies.iter().find(|(at, _)| *at == (f.line, f.col))?;
        Some(&self.tokens[range.clone()])
    }

    /// Whether a skimmed body spells the identifier `name`.
    pub fn mentions(&self, name: &str) -> bool {
        (self.bodies.iter())
            .flat_map(|(_, range)| &self.tokens[range.clone()])
            .any(|t| matches!(&t.tok, Tok::Ident(s) if s == name))
    }

    /// Parses `f`'s skimmed body, as [`parse_accum`] would have, or returns
    /// `None` when the parse did not skim it.
    pub fn body(&self, f: &Function) -> Option<Result<Block, Vec<Diagnostic>>> {
        let mut tokens = self.tokens(f)?.to_vec();
        let close = tokens.last()?;
        let (line, col) = (close.line, close.col);
        tokens.push(Token {
            tok: Tok::Eof,
            line,
            col,
        });
        let scope: Vec<&str> = f.type_params.iter().map(String::as_str).collect();
        Some(recognizer::block(
            &tokens,
            Cx::plain(&self.answers),
            &scope,
            |view| Build::read(view, 0, &self.answers).block(),
        ))
    }
}

fn parse_with(
    mut tokens: Vec<Token>,
    imported: bool,
    skim: bool,
) -> (Program, Vec<Diagnostic>, Skimmed) {
    // A declaration can follow its use, so the names a module answers to are known
    // only after one run. A module that answers to a method-form name runs again, and
    // each `recv.m(..)` and the statement desugar around it are decided once.
    let mut answers = HashSet::new();
    let cx = Cx {
        answers: &answers,
        imported,
        skim,
    };
    let mut run = recognizer::file(&tokens, cx);
    if !run.answered.is_empty() {
        answers = std::mem::take(&mut run.answered).into_iter().collect();
        let cx = Cx {
            answers: &answers,
            imported,
            skim,
        };
        run = recognizer::file(&tokens, cx);
    }
    let (mut program, bodies) = build(&mut tokens, &mut run, &answers, imported);
    let errors = run.errors;
    // The prelude: the declarations every program gets, stated in `prelude.vyrn`.
    // Only the root's parse carries them, so a linked program holds one copy.
    if !imported {
        program
            .type_decls
            .extend(crate::prelude::type_decls().iter().cloned());
    }
    // Flatten each `impl P for T` method into a mangled top-level function
    // (`types::impl_method_name`); protocol-method calls resolve to these names
    // by the receiver's type. Impls on unsupported targets are left for the
    // checker.
    //
    // Two impls for one (protocol, type constructor) mangle to one name, so only
    // the first is kept; the checker refuses the overlap by name and line.
    //
    // This runs before the loader renames anything, so the name holds the
    // protocol and type key as the module spelled them. The loader re-mangles
    // from the renamed ones, because the checker mangles the names it sees
    // (`Copy$json$Json$copy`, not `json$Copy$Json$copy`).
    let mut flat = Vec::new();
    let mut seen: HashSet<String> = Default::default();
    for imp in &program.impls {
        if let Some(key) = crate::types::type_key(&imp.ty) {
            for m in &imp.methods {
                let mut f = m.clone();
                f.name = crate::types::impl_method_name(&imp.protocol, &key, &m.name);
                if !seen.insert(f.name.clone()) {
                    continue;
                }
                f.type_params = imp.type_params.clone();
                f.type_bounds = imp.type_bounds.clone();
                flat.push(f);
            }
        }
    }
    program.functions.extend(flat);
    program.number();
    let skimmed = Skimmed {
        bodies,
        answers,
        tokens,
    };
    (program, errors, skimmed)
}

/// Returns how a binary operator is written, for a diagnostic that quotes one.
///
/// Derived from [`binop`] and `lexer::punct_tok`, so the spelling is stated once.
/// The search over 36 spellings runs only while formatting a message.
pub fn binop_text(op: BinOp) -> &'static str {
    crate::lexer::PUNCT_SPELLINGS
        .iter()
        .find(|s| crate::lexer::punct_tok(s).and_then(|t| binop(&t)) == Some(op))
        .copied()
        .unwrap_or_else(|| unreachable!("every `BinOp` is spelled by one token"))
}

/// The operator a token spells. The grammar's `climb` table orders them.
pub(crate) fn binop(tok: &Tok) -> Option<BinOp> {
    Some(match tok {
        Tok::OrOr => BinOp::Or,
        Tok::AndAnd => BinOp::And,
        Tok::EqEq => BinOp::Eq,
        Tok::NotEq => BinOp::NotEq,
        Tok::TildeMatch => BinOp::Match,
        Tok::Lt => BinOp::Lt,
        Tok::LtEq => BinOp::LtEq,
        Tok::Gt => BinOp::Gt,
        Tok::GtEq => BinOp::GtEq,
        Tok::Pipe => BinOp::BitOr,
        Tok::Caret => BinOp::BitXor,
        Tok::Amp => BinOp::BitAnd,
        Tok::Shl => BinOp::Shl,
        Tok::Shr => BinOp::Shr,
        Tok::Plus => BinOp::Add,
        Tok::Minus => BinOp::Sub,
        Tok::Star => BinOp::Mul,
        Tok::Slash => BinOp::Div,
        Tok::Percent => BinOp::Rem,
        _ => return None,
    })
}

/// `if let P = e { A } else { B }` as the statement `match e { P => { A },
/// _ => { B } }`, which [`Expr::as_if_let`] reads back.
fn if_let(pattern: Pattern, scrutinee: Expr, then: Block, els: Option<Block>, line: usize) -> Stmt {
    let arm = |pattern, b| MatchArm {
        pattern,
        body: ArmBody::Block(b),
    };
    let els = els.unwrap_or(Block {
        id: Id::NEW,
        stmts: Vec::new(),
    });
    let m = Expr::Match {
        id: Id::NEW,
        stmt_pos: true,
        scrutinee: Box::new(scrutinee),
        arms: vec![arm(pattern, then), arm(Pattern::Other, els)],
        line,
    };
    Stmt::expr(m)
}

/// Whether `tokens` are a separator `join` puts between items (`", "`, `" else "`):
/// punctuation and keywords, no bracket, no name and no literal, which no kind of
/// quote holds alone and no unit of code can open or close.
fn is_separator(tokens: &[Token]) -> bool {
    tokens.iter().all(|t| {
        let (kind, text) = crate::lexer::token_name_and_text(&t.tok);
        match kind.as_str() {
            "eof" | "keyword" => true,
            "punct" => !matches!(text.as_str(), "(" | ")" | "{" | "}" | "[" | "]"),
            _ => false,
        }
    })
}

/// The text a splice writes after each newline, for a hole that ends `line`:
/// the line's leading whitespace and, when the hole stands in a `//`, `///` or
/// `//!` comment, that prefix and one space ([`crate::gen::gen_code_splice`]).
fn continuation(line: &str) -> String {
    let ws = &line[..line.len() - line.trim_start().len()];
    match comment_start(line) {
        Some(at) => {
            let c = &line[at..];
            let prefix = if c.starts_with("///") || c.starts_with("//!") {
                &c[..3]
            } else {
                &c[..2]
            };
            format!("{ws}{prefix} ")
        }
        None => ws.to_string(),
    }
}

/// The byte offset of the first `//` in `line` outside a string or character
/// literal.
fn comment_start(line: &str) -> Option<usize> {
    let b = line.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < b.len() {
        match (quote, b[i]) {
            (Some(_), b'\\') => i += 1,
            (Some(q), c) if c == q => quote = None,
            (None, c @ (b'"' | b'\'')) => quote = Some(c),
            (None, b'/') if b.get(i + 1) == Some(&b'/') => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Returns the refusal of a `vyrn"..."` code quote at line `line`, column `col`
/// whose parts form no skeleton, or `None`.
///
/// The lexer has dropped a `vyrn"""` quote's closing indentation
/// (`lexer::dedent`). The parts, joined by a NUL for each hole, form the skeleton,
/// and `vyrn.syntax`'s `Quote` reads it once. A failure on its first line stands at
/// the literal.
pub(crate) fn skeleton_refusal(
    parts: &[String],
    (line, col): (usize, usize),
    answers: &HashSet<String>,
) -> Option<(Rule, (usize, usize))> {
    let Ok(tokens) = crate::lexer::lex_quote(&parts.join("\0")) else {
        return Some((Rule::SkeletonUnparsable {}, (line, col)));
    };
    if parts.len() == 1 && is_separator(&tokens) {
        return None;
    }
    let d = recognizer::quote_refusal(&tokens, answers)?;
    // A sentence holds a hole raw in a name and escaped in a found token.
    let detail = d.message.replace('\0', "\\{}").replace("\\0", "\\{}");
    let rcol = if d.line <= 1 { col } else { d.col };
    Some((rule!(SkeletonDetail, detail), (line + d.line - 1, rcol)))
}

/// Returns the root's id and name, the steps from the root to the place the
/// leaf writes into, and the leaf, of the store target `e`: a field or an
/// element of a place (`a`, `a.f`, `a[i]`, `@slot(a, i)`, to any depth). `None`
/// for a target with no address (a call result, a literal) and for a bare name,
/// which is no [`Stmt::Store`].
///
/// Two callers: [`Build::stmt`] for `a[i] = v` (parsed as `@at(a, i)`), and
/// `project.rs` for the place a projection's `atSet` yields.
pub fn store_target(e: &Expr) -> Option<(Id, String, Vec<Step>, Step)> {
    let (id, name, mut base) = place_steps(e)?;
    let leaf = base.pop()?.to_step();
    Some((
        id,
        name.to_string(),
        base.into_iter().map(At::to_step).collect(),
        leaf,
    ))
}

/// Returns the root's id and name of the place `e`, and the steps from the
/// root to `e`, root first. `None` where `e` is no place.
pub fn place_steps(e: &Expr) -> Option<(Id, &str, Vec<At<'_>>)> {
    match e {
        Expr::Var { name, id, .. } => Some((Id::at(id.col()), name, Vec::new())),
        Expr::Field { expr, field, .. } => {
            let (id, name, mut path) = place_steps(expr)?;
            path.push(At::Field(field));
            Some((id, name, path))
        }
        Expr::Call { name, args, .. }
            if (name == crate::project::AT || name == crate::project::ELEM) && args.len() == 2 =>
        {
            let (id, name, mut path) = place_steps(&args[0])?;
            path.push(At::Index(&args[1]));
            Some((id, name, path))
        }
        _ => None,
    }
}

/// What an expression before a `=` is to a statement.
pub(crate) enum StoreShape {
    /// A place: the `=` stores into it.
    Place,
    /// `f()[0] = v`: an element of something with no address.
    OverIndex,
    /// `f()[0].x = v`: a field of such an element.
    OverField,
    /// Anything else: the statement ends before the `=`.
    Value,
}

/// Reads the expression at event `open` and returns its [`StoreShape`]. The shape
/// is read before the value parses, so an unreachable target gets its own refusal,
/// not an error from the right side.
pub(crate) fn store_shape(view: View<'_>, open: usize, answers: &HashSet<String>) -> StoreShape {
    let e = Build::read(view, open, answers).expr();
    let (mut under, mut fields) = (&e, 0);
    while let Expr::Field { expr, .. } = under {
        (under, fields) = (expr, fields + 1);
    }
    let over_index =
        matches!(under, Expr::Call { name, args, .. } if name == "@at" && args.len() == 2);
    match (store_target(&e).is_some(), over_index, fields) {
        (true, ..) => StoreShape::Place,
        (false, true, 0) => StoreShape::OverIndex,
        (false, true, _) => StoreShape::OverField,
        (false, false, _) => StoreShape::Value,
    }
}

/// Whether the call a statement names by `name` could write its array back. The
/// recognizer asks before it builds the statement's tree.
pub(crate) fn may_rebuild(name: &str, answers: &HashSet<String>) -> bool {
    crate::prelude::rebuilds(name)
        || (!answers.contains(name)
            && crate::prelude::method_builtin(name).is_some_and(crate::prelude::rebuilds))
}

/// Whether a statement's call `e` has a place to write its array back to, or
/// needs none. `sq.push(v)` writes to `sq`, `r.f.push(v)` to `r.f`, `a[i].push(v)`
/// to `a[i]`. Any other receiver (a temporary, `r.a.b.push(v)`) is a refusal, not
/// a silent write to a copy.
pub(crate) fn push_has_place(view: View<'_>, open: usize, answers: &HashSet<String>) -> bool {
    let e = Build::read(view, open, answers).expr();
    let Expr::Call { name, args, .. } = &e else {
        return true;
    };
    if !crate::prelude::rebuilds(name) {
        return true;
    }
    let var = |e: &Expr| matches!(e, Expr::Var { .. });
    match args.first() {
        Some(Expr::Var { .. }) => true,
        Some(Expr::Field { expr, .. }) => var(expr),
        Some(Expr::Call {
            name: at,
            args: iargs,
            ..
        }) => at == "@at" && iargs.len() == 2 && var(&iargs[0]),
        _ => false,
    }
}

/// Whether `await` and the postfix chain at event `open` mark a call, under any
/// `?` after it.
pub(crate) fn await_marks(view: View<'_>, open: usize, answers: &HashSet<String>) -> bool {
    let mut b = Build::read(view, open, answers);
    b.at += 1;
    let e = b.postfix();
    let mut at = &e;
    while let Expr::Try { expr, .. } = at {
        at = expr;
    }
    matches!(at, Expr::Call { .. })
}

/// The variant, the binders and the scrutinee of the refutable `let` at event
/// `open`.
pub(crate) fn ref_let_parts(
    view: View<'_>,
    open: usize,
    answers: &HashSet<String>,
) -> (String, Vec<Binder>, Expr) {
    Build::read(view, open, answers).ref_let_parts()
}

/// The type `tokens` spell, as a refusal prints it.
pub(crate) fn type_text(tokens: &[Token], answers: &HashSet<String>) -> String {
    let mut toks = tokens.to_vec();
    toks.push(Token {
        tok: Tok::Eof,
        line: 0,
        col: 0,
    });
    recognizer::read_type(&toks, answers, |view| {
        Build::read(view, 0, answers).type_().to_string()
    })
    .unwrap_or_default()
}

/// The name of the `type` declaration at event `open` when a refinement it
/// collected sits outside the one record of its base: a `&` wrapped the record in
/// [`Type::Merge`], or an enum payload holds it, so there is no single record to
/// hang `Decl.field` on.
pub(crate) fn inline_where_outside_record(
    view: View<'_>,
    open: usize,
    answers: &HashSet<String>,
) -> Option<String> {
    let t = Build::read(view, open, answers).type_decl_parts();
    (!t.preds.is_empty() && !matches!(t.base, Type::Record(_))).then_some(t.name)
}

/// Reads a finished run into the declarations it closed. The tree takes each token's
/// payload it reads, so a name costs no second allocation; `tokens` holds no payload
/// of what the run read afterwards.
fn build(
    tokens: &mut [Token],
    run: &mut Run,
    answers: &HashSet<String>,
    imported: bool,
) -> (Program, Vec<SkimmedBody>) {
    let mut program = Program {
        imports: Vec::new(),
        type_decls: Vec::new(),
        functions: Vec::new(),
        protocols: Vec::new(),
        contracts: Vec::new(),
        impls: Default::default(),
        globals: Vec::new(),
        tests: Vec::new(),
        benches: Vec::new(),
        log_level: DEFAULT_LOG_LEVEL,
        log_sink: LogSink::Stderr,
        // The loader fills this once every module is linked.
        surface_shadows: std::collections::HashSet::new(),
        host: Host::default(),
        module_hashes: std::collections::BTreeMap::new(),
        units: 0,
        expansions: Default::default(),
        spellings: Default::default(),
        orders: Default::default(),
        prove: None,
        proved: Default::default(),
        session: Default::default(),
    };
    let mut bodies = Vec::new();
    let events = std::mem::take(&mut run.events);
    let mut b = Build {
        src: Src::Take(tokens, &mut run.holes),
        events: &events,
        at: 0,
        in_hole: false,
        answers,
        aliases: Default::default(),
        field_preds: None,
        extra_stmts: Vec::new(),
    };
    while b.at < events.len() {
        b.aliases.clear();
        b.item(&mut program, &mut bodies, imported, &run.skips);
        b.at += 1;
    }
    (program, bodies)
}

/// A body a skim left unread: its function's line and column, and its tokens.
type SkimmedBody = ((usize, usize), std::ops::Range<usize>);

/// Where a read gets its tokens. A check during the run reads them; the one read
/// after the run takes their payloads.
enum Src<'a> {
    Read(&'a [Token], &'a [HoleRun]),
    Take(&'a mut [Token], &'a mut [HoleRun]),
}

/// A read of [`Event`]s into the tree. The cursor is `at`.
struct Build<'a> {
    src: Src<'a>,
    events: &'a [Event],
    at: usize,
    /// Whether this reads an interpolation hole rather than the file: a binder
    /// parsed here carries no position ([`crate::ast::Binder`]).
    in_hole: bool,
    answers: &'a HashSet<String>,
    /// What an `impl` body's `type Output = ..` bound. Resolving here means an impl
    /// method leaves the builder as an ordinary generic function. The cost: a `type`
    /// member must precede the methods that name it.
    aliases: std::collections::HashMap<String, Type>,
    /// Inline `field: T where pred` refinements collected in a `type` declaration's
    /// record, drained into synthetic validated types named `Decl.field`. `None`
    /// outside a type declaration.
    field_preds: Option<Vec<(String, Expr)>>,
    /// Extra statements from a one-statement desugar. `stmt` returns the first and
    /// stashes the rest here; `block` drains them right after.
    extra_stmts: Vec<Stmt>,
}

/// What [`Build::peek`] returns where the next event holds no token.
static NO_TOKEN: Tok = Tok::Eof;
/// What a split leaves of a `>=`.
static REST_EQ: Tok = Tok::Eq;
/// What a split leaves of a `>>`.
static REST_GT: Tok = Tok::Gt;

/// The parts of a `type` declaration, before its refinements become types.
struct TypeParts {
    line: usize,
    name: String,
    type_params: Vec<String>,
    base: Type,
    predicate: Option<Expr>,
    preds: Vec<(String, Expr)>,
}

impl<'a> Build<'a> {
    /// A read of `view` from event `at`, for a check during the run.
    fn read(view: View<'a>, at: usize, answers: &'a HashSet<String>) -> Build<'a> {
        Build {
            src: Src::Read(view.toks, view.holes),
            events: view.events,
            at,
            in_hole: false,
            answers,
            aliases: Default::default(),
            field_preds: None,
            extra_stmts: Vec::new(),
        }
    }

    // The event cursor.

    /// The next event; a `Close` past the end, as a finished run has nothing after it.
    #[inline]
    fn event(&self) -> Event {
        self.events.get(self.at).copied().unwrap_or(Event::Close(0))
    }

    #[inline]
    fn toks(&self) -> &[Token] {
        match &self.src {
            Src::Read(toks, _) => toks,
            Src::Take(toks, _) => toks,
        }
    }

    /// The next event's first token.
    #[inline]
    fn token(&self) -> &Token {
        match self.event() {
            Event::Token(i)
            | Event::Rest(i)
            | Event::Word(i)
            | Event::Bound(i)
            | Event::Open(_, i) => &self.toks()[i as usize],
            other => unreachable!("no token at {other:?}"),
        }
    }

    /// The token the next event consumed, or none where it opens a rule or closes.
    #[inline]
    fn peek(&self) -> &Tok {
        match self.event() {
            Event::Token(i) | Event::Word(i) | Event::Bound(i) => &self.toks()[i as usize].tok,
            Event::Rest(i) => match self.toks()[i as usize].tok {
                Tok::GtEq => &REST_EQ,
                _ => &REST_GT,
            },
            _ => &NO_TOKEN,
        }
    }

    /// Consumes the next event's token with its payload.
    #[inline]
    fn advance(&mut self) -> Tok {
        let (Event::Token(i) | Event::Word(i) | Event::Bound(i)) = self.event() else {
            unreachable!("no token to take at {:?}", self.event())
        };
        self.at += 1;
        match &mut self.src {
            Src::Read(toks, _) => toks[i as usize].tok.clone(),
            Src::Take(toks, _) => std::mem::replace(&mut toks[i as usize].tok, Tok::Eof),
        }
    }

    #[inline]
    fn eat(&mut self, want: &Tok) -> bool {
        let hit = std::mem::discriminant(self.peek()) == std::mem::discriminant(want);
        self.at += hit as usize;
        hit
    }

    fn eat_semi(&mut self) {
        self.eat(&Tok::Semi);
    }

    /// Takes the `>` that closes a generic list, which may be half a `>>` or `>=`.
    fn eat_close(&mut self) -> bool {
        let hit = matches!(self.peek(), Tok::Gt | Tok::Shr | Tok::GtEq);
        self.at += hit as usize;
        hit
    }

    #[inline]
    fn at_rule(&self, kind: Kind) -> bool {
        matches!(self.event(), Event::Open(k, _) if k == kind)
    }

    /// Whether a rule consumed the next token as the word `word`.
    #[inline]
    fn at_word(&self, word: &str) -> bool {
        matches!(self.event(), Event::Word(_)) && matches!(self.peek(), Tok::Ident(w) if w == word)
    }

    /// Takes the word `word`, if a rule consumed it as one.
    fn eat_word(&mut self, word: &str) -> bool {
        let hit = self.at_word(word);
        self.at += hit as usize;
        hit
    }

    fn line(&self) -> usize {
        self.token().line
    }

    fn col(&self) -> usize {
        self.token().col
    }

    fn ident(&mut self) -> String {
        match self.advance() {
            Tok::Ident(name) => name,
            other => unreachable!("the grammar's name is {other:?}"),
        }
    }

    fn plain_str(&mut self) -> String {
        match self.advance() {
            Tok::Str(s) => s,
            other => unreachable!("the grammar's plain string is {other:?}"),
        }
    }

    /// Where the token here is spelled in the file, or `(0, 0)` inside a hole
    /// ([`Build::in_hole`]). Every binder's position comes from here.
    fn binder_pos(&self) -> (usize, usize) {
        if self.in_hole {
            (0, 0)
        } else {
            (self.line(), self.col())
        }
    }

    /// The slot of a node that spells the name here: its column, or none inside a
    /// hole, as [`Build::binder_pos`] places a binder.
    fn spelled(&self) -> Id {
        match self.in_hole {
            true => Id::NEW,
            false => Id::at(self.col()),
        }
    }

    fn binder(&mut self) -> Binder {
        let (line, col) = self.binder_pos();
        let name = self.ident();
        Binder {
            id: Id::NEW,
            name,
            line,
            col,
        }
    }

    /// `( a, b, .. )`, the opening parenthesis consumed, each item read by `item`.
    fn list_to(&mut self, close: &Tok, mut item: impl FnMut(&mut Self)) {
        while !self.eat(close) {
            item(self);
            self.eat(&Tok::Comma);
        }
    }

    fn binders_to_paren(&mut self) -> Vec<Binder> {
        let mut out = Vec::new();
        self.list_to(&Tok::RParen, |b| out.push(b.binder()));
        out
    }

    fn exprs_to_paren(&mut self, out: &mut Vec<Expr>) {
        self.list_to(&Tok::RParen, |b| out.push(b.expr()));
    }

    /// Consumes consecutive `///` tokens and returns them joined by newlines, or
    /// `None`. A blank line detaches an earlier block, and the block that survives
    /// sits directly above the declaration.
    fn take_docs(&mut self) -> Option<String> {
        let mut lines: Vec<String> = Vec::new();
        let mut last_line = 0usize;
        while matches!(self.peek(), Tok::Doc(_)) {
            let line = self.line();
            if !lines.is_empty() && line > last_line + 1 {
                lines.clear();
            }
            if let Tok::Doc(t) = self.advance() {
                lines.push(t);
            }
            last_line = line;
        }
        if lines.is_empty() {
            return None;
        }
        // The token after the last doc, which no event may record: a refused
        // declaration's first token, or `Eof`.
        let next = self.toks()[self.doc_end()].line;
        if next > last_line + 1 {
            return None;
        }
        Some(lines.join("\n"))
    }

    /// The index of the token after the doc event just read.
    fn doc_end(&self) -> usize {
        match self.events[self.at - 1] {
            Event::Token(i) => i as usize + 1,
            other => unreachable!("a doc is a token, not {other:?}"),
        }
    }

    // Declarations.

    /// Reads one item of the file: its docs, `export`, and the declaration.
    fn item(
        &mut self,
        program: &mut Program,
        bodies: &mut Vec<SkimmedBody>,
        imported: bool,
        skips: &[std::ops::Range<usize>],
    ) {
        let doc = self.take_docs();
        let exported = self.eat(&Tok::Export);
        let export_to = self.export_to();
        let is_gen = self.eat_word("gen");
        let is_mut = self.eat(&Tok::Mut);
        match self.peek() {
            Tok::Fn => {
                let mut f = self.function(is_gen, skips, bodies);
                f.doc = doc;
                f.exported = exported;
                f.export_to = export_to;
                f.is_mut = is_mut;
                program.functions.push(f);
            }
            Tok::Import => program.imports.push(self.import_decl()),
            Tok::Type => {
                let mut ds = self.type_decl();
                ds[0].doc = doc;
                ds[0].exported = exported;
                ds[0].export_to = export_to;
                program.type_decls.extend(ds);
            }
            Tok::Protocol => {
                let mut p = self.protocol_decl();
                p.doc = doc;
                p.exported = exported;
                p.export_to = export_to;
                program.protocols.push(p);
            }
            Tok::Impl => program.impls.push(self.impl_block()),
            Tok::Let => {
                let mut g = self.global_decl();
                g.doc = doc;
                program.globals.push(g);
            }
            _ if self.at_word("contract") => {
                let mut c = self.contract_decl();
                c.doc = doc;
                c.exported = exported;
                c.export_to = export_to;
                program.contracts.push(c);
            }
            _ if self.at_word("extern") => {
                let mut f = self.extern_function(exported);
                f.doc = doc;
                program.functions.push(f);
            }
            _ if self.at_word("logging") => self.logging_config(program),
            _ if self.at_word("test") || self.at_word("bench") => {
                let line = self.line();
                let test = self.at_word("test");
                self.at += 1;
                let name = self.plain_str();
                if !imported {
                    let block = NamedBlock {
                        name,
                        body: self.block(),
                        doc,
                        line,
                    };
                    match test {
                        true => program.tests.push(block),
                        false => program.benches.push(block),
                    }
                }
            }
            // `~^TopLevelExpected` consumed the stray token and recorded nothing.
            _ => {}
        }
    }

    /// The paths of an `export to "a", "b"` clause; empty without one.
    fn export_to(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if self.eat_word("to") {
            out.push(self.plain_str());
            while self.eat(&Tok::Comma) {
                out.push(self.plain_str());
            }
        }
        out
    }

    /// `protocol Name { type A  fn m(self, p: T, ..) -> R; .. }`: method
    /// signatures and associated types. The `self` receiver is required
    /// and dropped from the stored parameters. An associated type parses as
    /// [`Type::Param`] and must precede the signatures that name it.
    fn protocol_decl(&mut self) -> ProtocolDecl {
        let line = self.line();
        self.at += 1;
        let name = self.ident();
        self.eat(&Tok::LBrace);
        let mut methods = Vec::new();
        let mut assoc: Vec<String> = Vec::new();
        loop {
            let doc = self.take_docs();
            if self.eat(&Tok::RBrace) {
                break;
            }
            if self.eat(&Tok::Type) {
                assoc.push(self.ident());
                self.eat_semi();
                continue;
            }
            let mline = self.line();
            self.at += 1;
            let mname = self.ident();
            self.eat(&Tok::LParen);
            let recv = self.capability();
            self.eat(&Tok::Vself);
            let mut params = Vec::new();
            let mut param_caps = Vec::new();
            while self.eat(&Tok::Comma) {
                self.at += 1; // The parameter's name is not kept.
                self.eat(&Tok::Colon);
                param_caps.push(self.capability());
                params.push(self.type_());
            }
            self.eat(&Tok::RParen);
            let mut result_cap = None;
            let ret = if self.eat(&Tok::Arrow) {
                result_cap = self.result_capability();
                self.type_()
            } else {
                Type::Unit
            };
            self.eat_semi();
            methods.push(MethodSig {
                name: mname,
                doc,
                recv,
                params,
                param_caps,
                ret,
                result_cap,
                line: mline,
            });
        }
        ProtocolDecl {
            exported: false,
            export_to: Vec::new(),
            module: None,
            name,
            doc: None,
            assoc,
            methods,
            line,
        }
    }

    /// `contract Name { let m: T = d  fn m(a: T) -> R  fn *(a: T) -> R }`: a module
    /// contract, the exports a module may have.
    ///
    /// It differs from [`Self::protocol_decl`] in two ways: member `///` docs are
    /// kept for the LSP, and member type parameters are implicit, recognized by
    /// spelling ([`is_member_type_param`]).
    fn contract_decl(&mut self) -> ContractDecl {
        let line = self.line();
        self.at += 1;
        let name = self.ident();
        self.eat(&Tok::LBrace);
        let mut members: Vec<ContractMember> = Vec::new();
        loop {
            let doc = self.take_docs();
            if self.eat(&Tok::RBrace) {
                break;
            }
            let mline = self.line();
            members.push(match self.eat(&Tok::Let) {
                true => self.contract_value_member(doc, mline),
                false => self.contract_fn_member(doc, mline),
            });
        }
        ContractDecl {
            name,
            exported: false,
            export_to: Vec::new(),
            module: None,
            doc: None,
            members,
            line,
        }
    }

    /// `let name: Type [= default]` in a contract. A default makes the member
    /// optional.
    fn contract_value_member(&mut self, doc: Option<String>, line: usize) -> ContractMember {
        let name = self.ident();
        self.eat(&Tok::Colon);
        let ty = self.contract_member_type();
        let default = self.eat(&Tok::Eq).then(|| Box::new(self.expr()));
        self.eat_semi();
        ContractMember {
            name,
            doc,
            kind: ContractMemberKind::Value { ty, default },
            line,
        }
    }

    /// `fn name(a: T, ..) -> R [= default]`, or the open rule `fn *(a: T, ..) -> R`,
    /// in a contract. Parameter names are not kept: a contract constrains arity and
    /// types. The default is an expression of the return type and makes the member
    /// optional.
    fn contract_fn_member(&mut self, doc: Option<String>, line: usize) -> ContractMember {
        self.at += 1;
        let name = match self.eat(&Tok::Star) {
            true => OPEN_RULE_NAME.to_string(),
            false => self.ident(),
        };
        self.eat(&Tok::LParen);
        // `fn *(..) -> R` constrains the return type only. `..` is two `.` tokens.
        let variadic = self.eat(&Tok::Dot);
        if variadic {
            self.at += 1;
        }
        let mut params = Vec::new();
        self.list_to(&Tok::RParen, |b| {
            b.at += 1;
            b.eat(&Tok::Colon);
            params.push(b.contract_member_type());
        });
        let ret = match self.eat(&Tok::Arrow) {
            true => self.contract_member_type(),
            false => Type::Unit,
        };
        let default = self.eat(&Tok::Eq).then(|| Box::new(self.expr()));
        self.eat_semi();
        ContractMember {
            name,
            doc,
            kind: ContractMemberKind::Fn {
                params,
                ret,
                default,
                variadic,
            },
            line,
        }
    }

    /// A type in a contract member, with its implicit type parameters rewritten to
    /// [`Type::Param`] by spelling ([`is_member_type_param`]).
    fn contract_member_type(&mut self) -> Type {
        let mut ty = self.type_();
        mark_member_type_params(&mut ty);
        ty
    }

    /// `impl P for T { fn m(self, ..) -> R { .. } .. }`: a type's methods for a
    /// protocol, each `self` typed to `T`.
    ///
    /// `impl<T> P for C<T>` binds type variables for the target and every method.
    /// `type Output = T` binds an associated type, substituted as the methods
    /// read, so a method returning `Output` leaves here returning `T`.
    ///
    /// A `type` member must come before the methods that name it. Deferring would
    /// need a substitution pass over every body, where a missed `Type::Param`
    /// lowers to `void` silently; one ordering rule with a diagnostic is cheaper.
    fn impl_block(&mut self) -> ImplBlock {
        let (line, col) = (self.line(), self.col());
        self.at += 1;
        let (type_params, type_bounds) = self.type_param_binder();
        let protocol = self.ident();
        self.eat(&Tok::For);
        let ty = self.type_();
        self.eat(&Tok::LBrace);
        let mut methods = Vec::new();
        let mut places = Vec::new();
        let mut assoc: Vec<String> = Vec::new();
        loop {
            self.take_docs(); // Method-level docs are discarded.
            if self.eat(&Tok::RBrace) {
                break;
            }
            if self.eat(&Tok::Type) {
                let aname = self.ident();
                self.eat(&Tok::Eq);
                let bound = self.type_();
                assoc.push(aname.clone());
                self.aliases.insert(aname, bound);
                self.eat_semi();
                continue;
            }
            let (mut m, place_cap) = self.impl_method(&ty);
            m.type_params = type_params.clone();
            m.type_bounds = type_bounds.clone();
            // A capability result makes a member a projection: it goes in
            // [`ImplBlock::places`], never into the flattened methods.
            if place_cap.is_some() {
                places.push(m);
            } else {
                methods.push(m);
            }
        }
        ImplBlock {
            protocol,
            type_params,
            type_bounds,
            ty,
            assoc,
            methods,
            places,
            line,
            col,
        }
    }

    /// The word a rule consumed before a type or `self`: `read self`, `consume T`;
    /// [`Capability::Read`] without one. `Array` and the other constructors are words
    /// too, and name no capability.
    fn capability(&mut self) -> Capability {
        if let (Event::Word(_), Tok::Ident(id)) = (self.event(), self.peek()) {
            if let Some(c) = Capability::from_word(id) {
                self.at += 1;
                return c;
            }
        }
        Capability::Read
    }

    /// The `read` / `modify` a result may carry: access to a place the receiver
    /// keeps.
    fn result_capability(&mut self) -> Option<Capability> {
        if let (Event::Word(_), Tok::Ident(id)) = (self.event(), self.peek()) {
            let c = Capability::from_word(id);
            self.at += c.is_some() as usize;
            return c;
        }
        None
    }

    /// One `fn m(read|modify|consume self, ..) -> R { .. }` in an `impl`: a
    /// [`Function`] whose first parameter is `self`, typed to the implementing type
    /// and carrying the receiver's capability. A bare `self` is `read`.
    fn impl_method(&mut self, self_ty: &Type) -> (Function, Option<Capability>) {
        let line = self.line();
        self.at += 1;
        let col = self.col();
        let name = self.ident();
        self.eat(&Tok::LParen);
        let capability = self.capability();
        let (self_line, self_col) = self.binder_pos();
        self.eat(&Tok::Vself);
        let mut params = vec![Param {
            id: Id::NEW,
            name: "self".to_string(),
            capability,
            ty: self_ty.clone(),
            line: self_line,
            col: self_col,
            clause: None,
        }];
        while self.eat(&Tok::Comma) {
            let (line, col) = self.binder_pos();
            let pname = self.ident();
            self.eat(&Tok::Colon);
            let capability = self.capability();
            let ty = self.type_();
            params.push(Param {
                id: Id::NEW,
                name: pname,
                capability,
                ty,
                line,
                col,
                clause: None,
            });
        }
        self.eat(&Tok::RParen);
        let (ret, result_cap) = if self.eat(&Tok::Arrow) {
            let rc = self.result_capability();
            (self.type_(), rc)
        } else {
            (Type::Unit, None)
        };
        let body = self.block();
        (
            Function {
                exported: false,
                export_to: Vec::new(),
                module: None,
                name,
                doc: None,
                type_params: Vec::new(),
                type_bounds: Default::default(),
                params,
                ret,
                ret_clause: None,
                body,
                line,
                col,
                is_extern: false,
                is_export_extern: false,
                is_gen: false,
                is_mut: false,
            },
            result_cap,
        )
    }

    /// `logging { level: <name>, sink: <dest> }`, each field optional.
    fn logging_config(&mut self, program: &mut Program) {
        self.at += 1;
        self.eat(&Tok::LBrace);
        while !self.eat(&Tok::RBrace) {
            let key = self.ident();
            self.eat(&Tok::Colon);
            match key.as_str() {
                "level" => {
                    let name = self.ident();
                    program.log_level = log_level_ordinal(&name)
                        .unwrap_or_else(|| unreachable!("the grammar names each level"));
                }
                _ => program.log_sink = self.log_sink(),
            }
            self.eat(&Tok::Comma);
        }
    }

    fn log_sink(&mut self) -> LogSink {
        let name = self.ident();
        match name.as_str() {
            "stderr" => LogSink::Stderr,
            "stdout" => LogSink::Stdout,
            _ => {
                self.eat(&Tok::LParen);
                let path = self.plain_str();
                self.eat(&Tok::RParen);
                LogSink::File(path)
            }
        }
    }

    /// `import { a, b } from "path"`; `from` is contextual. `import type { .. }`
    /// marks a JSON Schema import; the loader dispatches on the path's extension.
    /// `import * as ns from <source>` binds one name and adds none of the target's
    /// exports to the flat namespace.
    fn import_decl(&mut self) -> ImportDecl {
        let (line, col) = (self.line(), self.col());
        self.at += 1;
        let mut names = Vec::new();
        let mut namespace = None;
        if self.eat(&Tok::Star) {
            self.at += 1;
            namespace = Some(self.ident());
        } else {
            self.eat(&Tok::Type);
            self.eat(&Tok::LBrace);
            self.list_to(&Tok::RBrace, |b| {
                let original = b.ident();
                // `as` is contextual: only between an import name and its `,` or `}`.
                let alias = b.eat_word("as").then(|| b.ident());
                names.push(ImportName { original, alias });
            });
        }
        self.at += 1;
        let source_at = (self.line(), self.col());
        let source = self.import_source();
        self.eat_semi();
        ImportDecl {
            names,
            namespace,
            source,
            line,
            col,
            source_at,
        }
    }

    /// The source after `from`: a module path string, or a generator call
    /// `gen(args...)` run at compile time.
    fn import_source(&mut self) -> ImportSource {
        if matches!(self.peek(), Tok::Str(_)) {
            return ImportSource::Path(self.plain_str());
        }
        let line = self.line();
        let name = self.ident();
        self.eat(&Tok::LParen);
        let mut args = Vec::new();
        self.exprs_to_paren(&mut args);
        ImportSource::Generator { name, args, line }
    }

    fn type_decl(&mut self) -> Vec<TypeDecl> {
        let t = self.type_decl_parts();
        let (name, line) = (t.name, t.line);
        // Each inline refinement becomes a synthetic validated type named
        // `Decl.field`; the `.` keeps it out of the user namespace. The field's type is
        // rewritten to it, so validation applies unchanged.
        let mut base = t.base;
        let mut decls = Vec::with_capacity(1 + t.preds.len());
        if !t.preds.is_empty() {
            let Type::Record(fields) = &mut base else {
                unreachable!("InlineWhereBase refused this declaration")
            };
            for (fname, pred) in t.preds {
                let synthetic = format!("{name}.{fname}");
                let field = fields
                    .iter_mut()
                    .find(|f| f.name == fname)
                    .expect("collected predicate names an existing field");
                decls.push(TypeDecl {
                    exported: false,
                    export_to: Vec::new(),
                    module: None,
                    name: synthetic.clone(),
                    doc: None,
                    type_params: Vec::new(),
                    base: std::mem::replace(&mut field.ty, Type::Named(synthetic)),
                    predicate: Some(pred),
                    members: Vec::new(),
                    line,
                });
            }
        }
        decls.insert(
            0,
            TypeDecl {
                name,
                exported: false,
                export_to: Vec::new(),
                module: None,
                doc: None,
                type_params: t.type_params,
                base,
                predicate: t.predicate,
                members: Vec::new(),
                line,
            },
        );
        decls
    }

    fn type_decl_parts(&mut self) -> TypeParts {
        let line = self.line();
        self.at += 1;
        let name = self.ident();
        let mut type_params = Vec::new();
        if self.eat(&Tok::Lt) {
            while !self.eat_close() {
                type_params.push(self.ident());
                self.eat(&Tok::Comma);
            }
        }
        self.eat(&Tok::Eq);
        // The collector is what makes `where` legal in field position.
        self.field_preds = Some(Vec::new());
        let base = match self.eat(&Tok::Enum) {
            true => self.enum_type(),
            false => self.type_(),
        };
        let preds = self.field_preds.take().unwrap_or_default();
        let predicate = self.eat(&Tok::Where).then(|| self.expr());
        self.eat_semi();
        TypeParts {
            line,
            name,
            type_params,
            base,
            predicate,
            preds,
        }
    }

    /// `enum { Variant(Type), Variant, ... }`: an enum. A trailing comma is allowed,
    /// as in a record; the checker refuses an enum with no variant.
    fn enum_type(&mut self) -> Type {
        self.eat(&Tok::LBrace);
        let mut variants = Vec::new();
        self.list_to(&Tok::RBrace, |b| {
            let name = b.ident();
            let mut payload = Vec::new();
            if b.eat(&Tok::LParen) {
                b.list_to(&Tok::RParen, |b| payload.push(b.type_()));
            }
            variants.push(EnumVariant { name, payload });
        });
        Type::Enum(variants)
    }

    /// `{ field: Type, ... }`, the brace consumed: a record type. In a `type`
    /// declaration a field may carry an inline refinement
    /// (`age: Int64 where value >= 18`); the trailing `where` after `}` states the
    /// cross-field invariant.
    fn record_type(&mut self) -> Type {
        // Only the outermost record of a `type` declaration collects refinements:
        // `take()` hides the collector from nested, anonymous records.
        let outer = self.field_preds.take();
        let mut local: Vec<(String, Expr)> = Vec::new();
        let mut fields = Vec::new();
        self.list_to(&Tok::RBrace, |b| {
            let name = b.ident();
            b.eat(&Tok::Colon);
            // `field: lazy T`. `lazy` is contextual: read only where a field's
            // type begins, a position no call occupies, so `std/ui`'s `lazy(..)` keeps its
            // name.
            let lazy = b.eat_word("lazy");
            let ty = b.type_();
            let ty = if lazy { Type::Lazy(Box::new(ty)) } else { ty };
            if b.eat(&Tok::Where) {
                local.push((name.clone(), b.expr()));
            }
            fields.push(Field { name, ty });
        });
        if let Some(mut prev) = outer {
            prev.extend(local);
            self.field_preds = Some(prev);
        }
        Type::Record(fields)
    }

    /// An optional `<T: Bound + Other, U>` binder, empty when no `<` follows.
    /// Shared by `fn` and `impl<..>`, so an impl keeps its bounds.
    fn type_param_binder(
        &mut self,
    ) -> (Vec<String>, std::collections::HashMap<String, Vec<String>>) {
        let mut type_params = Vec::new();
        let mut type_bounds: std::collections::HashMap<String, Vec<String>> = Default::default();
        if self.eat(&Tok::Lt) {
            while !self.eat_close() {
                let tp = self.ident();
                if self.eat(&Tok::Colon) {
                    let mut bounds = vec![self.ident()];
                    while self.eat(&Tok::Plus) {
                        bounds.push(self.ident());
                    }
                    type_bounds.insert(tp.clone(), bounds);
                }
                type_params.push(tp);
                self.eat(&Tok::Comma);
            }
        }
        (type_params, type_bounds)
    }

    /// `(name: [capability] Type [where clause], ..)`: a function's
    /// parameter list, parentheses included.
    fn params(&mut self) -> Vec<Param> {
        self.eat(&Tok::LParen);
        let mut params = Vec::new();
        self.list_to(&Tok::RParen, |b| {
            let (line, col) = b.binder_pos();
            let name = b.ident();
            b.eat(&Tok::Colon);
            let capability = b.capability();
            let ty = b.type_();
            let clause = b.eat(&Tok::Where).then(|| b.expr());
            params.push(Param {
                id: Id::NEW,
                name,
                capability,
                ty,
                line,
                col,
                clause,
            });
        });
        params
    }

    /// `[gen] fn name<...>(params) -> Ret { body }`. `is_gen` says a contextual
    /// `gen` preceded `fn`. With `skipped`, the recognizer skimmed the body.
    fn function(
        &mut self,
        is_gen: bool,
        skips: &[std::ops::Range<usize>],
        bodies: &mut Vec<SkimmedBody>,
    ) -> Function {
        let line = self.line();
        self.at += 1;
        let col = self.col();
        let name = self.ident();
        let (type_params, type_bounds) = self.type_param_binder();
        let params = self.params();
        // No `-> Type` means Unit.
        let ret = match self.eat(&Tok::Arrow) {
            true => self.type_(),
            false => Type::Unit,
        };
        let ret_clause = self.eat(&Tok::Where).then(|| self.expr());
        let body = match self.event() {
            Event::Skipped(k) => {
                self.at += 1;
                bodies.push(((line, col), skips[k as usize].clone()));
                Block {
                    id: Id::NEW,
                    stmts: Vec::new(),
                }
            }
            _ => self.block(),
        };
        Function {
            name,
            exported: false,
            export_to: Vec::new(),
            module: None,
            doc: None,
            type_params,
            type_bounds,
            params,
            ret,
            ret_clause,
            body,
            line,
            col,
            is_extern: false,
            is_export_extern: false,
            is_gen,
            // The caller sets this when `mut` preceded `fn`.
            is_mut: false,
        }
    }

    /// `extern fn name(params) -> Ret`; `exported` says `export` preceded it.
    ///
    /// - `extern fn f(..)` is a JS import: the host supplies the body.
    /// - `export extern fn f(..) { .. }` is a Vyrn function exported to JS. The checker
    ///   enforces the ABI type domain.
    fn extern_function(&mut self, exported: bool) -> Function {
        let line = self.line();
        self.at += 2;
        let col = self.col();
        let name = self.ident();
        // An extern has no generic parameters: the ABI is monomorphic.
        let params = self.params();
        let ret = match self.eat(&Tok::Arrow) {
            true => self.type_(),
            false => Type::Unit,
        };
        let body = match exported {
            true => self.block(),
            false => {
                self.eat_semi();
                Block {
                    id: Id::NEW,
                    stmts: Vec::new(),
                }
            }
        };
        Function {
            name,
            exported,
            export_to: Vec::new(),
            module: None,
            doc: None,
            type_params: Vec::new(),
            type_bounds: Default::default(),
            params,
            ret,
            ret_clause: None,
            body,
            line,
            col,
            is_extern: !exported,
            is_export_extern: exported,
            is_gen: false,
            is_mut: false,
        }
    }

    // Types.

    /// A type, possibly a union `A | B | ...` of intersections. `|` binds
    /// loosest, so `fn() -> A | B` returns the union.
    fn type_(&mut self) -> Type {
        let first = self.intersection();
        if !matches!(self.peek(), Tok::Pipe) {
            return first;
        }
        let mut members = vec![first];
        while self.eat(&Tok::Pipe) {
            members.push(self.intersection());
        }
        Type::Union(members)
    }

    /// A type, possibly an intersection `A & B & ...` (left-associative
    /// `Merge<A, B>`).
    fn intersection(&mut self) -> Type {
        let mut t = self.type_atom();
        while self.eat(&Tok::Amp) {
            let rhs = self.type_atom();
            t = Type::Merge(Box::new(t), Box::new(rhs));
        }
        t
    }

    fn type_atom(&mut self) -> Type {
        // A name the recognizer found bound: an associated type, or a parameter.
        if let Event::Bound(_) = self.event() {
            let name = self.ident();
            return match self.aliases.get(&name) {
                Some(bound) => bound.clone(),
                None => Type::Param(name),
            };
        }
        if self.eat(&Tok::LBrace) {
            return self.record_type();
        }
        // `fn(T, U) -> R`, or `fn(T)` for a Unit return. The checker
        // restricts it to a parameter position.
        if self.eat(&Tok::Fn) {
            self.eat(&Tok::LParen);
            let mut params = Vec::new();
            self.list_to(&Tok::RParen, |b| params.push(b.type_()));
            let ret = match self.eat(&Tok::Arrow) {
                true => self.type_(),
                false => Type::Unit,
            };
            return Type::Fn(params, Box::new(ret));
        }
        // An integer literal type argument, the `8` in
        // `SmallArray<Int64, 8>`. Any other constructor carrying one is refused by the
        // checker, not here.
        if let Tok::Int(n) = *self.peek() {
            self.at += 1;
            return Type::ConstInt(n as u64);
        }
        let word = matches!(self.event(), Event::Word(_));
        let name = self.ident();
        if word {
            return self.named_type(&name);
        }
        // `ns.User` / `ns.Box<T>`: the loader checks `ns` and rewrites the
        // dotted name to the resolved declaration, so no later pass sees a dot.
        let mut full = name;
        while self.eat(&Tok::Dot) {
            full = format!("{full}.{}", self.ident());
        }
        if self.eat(&Tok::Lt) {
            let args = self.types_to_close();
            return Type::App(full, args);
        }
        Type::Named(full)
    }

    fn types_to_close(&mut self) -> Vec<Type> {
        let mut args = Vec::new();
        while !self.eat_close() {
            args.push(self.type_());
            self.eat(&Tok::Comma);
        }
        args
    }

    /// The type a constructor or primitive word spells, the word consumed.
    fn named_type(&mut self, name: &str) -> Type {
        let one = |b: &mut Self| {
            b.eat(&Tok::Lt);
            let inner = b.type_();
            b.eat_close();
            inner
        };
        let two = |b: &mut Self| {
            b.eat(&Tok::Lt);
            let a = b.type_();
            b.eat(&Tok::Comma);
            let c = b.type_();
            b.eat_close();
            (a, c)
        };
        let int = |bits, signed| Type::IntN { bits, signed };
        match name {
            // `Int64` is the default integer, but always written: there is no unsized
            // `Int`.
            "Int64" => Type::Int,
            "Int8" => int(8, true),
            "Int16" => int(16, true),
            "Int32" => int(32, true),
            "UInt8" => int(8, false),
            "UInt16" => int(16, false),
            "UInt32" => int(32, false),
            "UInt64" => int(64, false),
            "Float64" => Type::Float,
            "Float32" => Type::Float32,
            // One `Bool` mask type for both four-lane widths: an `I32x4` and an `F32x4`
            // comparison both yield `<4 x i32>` / `v128`.
            "F32x4" => Type::F32x4,
            "I32x4" => Type::I32x4,
            "Mask32x4" => Type::Mask32x4,
            // The second mask: two 64-bit lanes are a different count and width.
            "F64x2" => Type::F64x2,
            "Mask64x2" => Type::Mask64x2,
            "Bool" => Type::Bool,
            "String" => Type::Str,
            "Unit" => Type::Unit,
            "Logger" => Type::Logger,
            // `Stream<T>`: linear, disposed exactly once; movecheck checks it.
            "Stream" => Type::Stream(Box::new(one(self))),
            "Array" => {
                self.eat(&Tok::Lt);
                let inner = Box::new(self.type_());
                let size = self.eat(&Tok::Comma).then(|| self.size());
                self.eat_close();
                match size {
                    Some(n) => Type::ArrayN(inner, n),
                    None => Type::Array(inner),
                }
            }
            // `SmallArray<T, N>`: `N` inline elements, spilling to the heap
            // past `N`. The checker enforces `1 <= N <= 64`.
            "SmallArray" => {
                self.eat(&Tok::Lt);
                let inner = Box::new(self.type_());
                self.eat(&Tok::Comma);
                let n = self.size();
                self.eat_close();
                Type::SmallArray(inner, n)
            }
            // `Map<K, V>`: insertion-ordered. The checker enforces the key
            // rule, because a validated string type resolves to `String` only there.
            "Map" => {
                let (key, val) = two(self);
                Type::Map(Box::new(key), Box::new(val))
            }
            "Option" => Type::option(one(self)),
            "Result" => {
                let (ok, err) = two(self);
                Type::result(ok, err)
            }
            // Compile-time transformers.
            "Omit" | "Pick" => {
                self.eat(&Tok::Lt);
                let base = Box::new(self.type_());
                let mut keys = Vec::new();
                while self.eat(&Tok::Comma) {
                    keys.push(self.ident());
                }
                self.eat_close();
                if name == "Omit" {
                    Type::Omit(base, keys)
                } else {
                    Type::Pick(base, keys)
                }
            }
            "Merge" => {
                let (a, b) = two(self);
                Type::Merge(Box::new(a), Box::new(b))
            }
            "Partial" => Type::Partial(Box::new(one(self))),
            // Records are immutable, so `Readonly<T>` is `T`.
            "Readonly" => one(self),
            other => unreachable!("the grammar's type word is `{other}`"),
        }
    }

    fn size(&mut self) -> usize {
        match self.advance() {
            Tok::Int(n) => n as usize,
            other => unreachable!("the grammar's size is {other:?}"),
        }
    }

    // Statements.

    fn block(&mut self) -> Block {
        self.eat(&Tok::LBrace);
        let mut stmts = Vec::new();
        loop {
            match self.peek() {
                Tok::RBrace => {
                    self.at += 1;
                    break;
                }
                // A stray `///` inside a body is discarded, and a semicolon is an optional
                // separator.
                Tok::Doc(_) | Tok::Semi => self.at += 1,
                _ => {
                    stmts.push(self.stmt());
                    // Splice in a desugar's follow-on statements (`a[i].f = v`), in order.
                    stmts.append(&mut self.extra_stmts);
                }
            }
        }
        Block { id: Id::NEW, stmts }
    }

    /// Top-level module state: `let [mut] name [: Type] = init`. The
    /// initializer is required: module state has no default value.
    fn global_decl(&mut self) -> GlobalDecl {
        let line = self.line();
        self.at += 1;
        let mutable = self.eat(&Tok::Mut);
        let name = self.ident();
        let ty = self.eat(&Tok::Colon).then(|| self.type_());
        self.eat(&Tok::Eq);
        let init = self.expr();
        self.eat_semi();
        GlobalDecl {
            name,
            mutable,
            ty,
            init,
            doc: None,
            module: None,
            line,
        }
    }

    /// Reads an `if` whose token is current. `else if` nests the
    /// chained `if` as the only statement of an else-block, with its own line.
    fn if_stmt(&mut self, line: usize) -> Stmt {
        self.at += 1;
        // `if let PAT = e { .. }`: the pattern-binding form.
        if self.eat(&Tok::Let) {
            return self.if_let_stmt(line);
        }
        let cond = self.expr();
        let then_block = self.block();
        let else_block = self.else_tail();
        Stmt::If {
            id: Id::NEW,
            cond,
            then_block,
            else_block,
            line,
        }
    }

    /// The optional `else` tail of `if` and `if let`: `else if`, `else if let`, or
    /// `else { .. }`.
    fn else_tail(&mut self) -> Option<Block> {
        if !self.eat(&Tok::Else) {
            return None;
        }
        if !matches!(self.peek(), Tok::If) {
            return Some(self.block());
        }
        let nested = self.if_stmt(self.line());
        Some(Block {
            id: Id::NEW,
            stmts: vec![nested],
        })
    }

    /// `if let PAT = e { .. } [else ..]`; `if let` consumed.
    fn if_let_stmt(&mut self, line: usize) -> Stmt {
        let pattern = self.pattern();
        self.eat(&Tok::Eq);
        let scrutinee = self.expr();
        let then_block = self.block();
        let else_block = self.else_tail();
        if_let(pattern, scrutinee, then_block, else_block, line)
    }

    /// Returns a desugar's first statement and queues the rest in
    /// [`Build::extra_stmts`] for [`Build::block`] to splice in.
    fn spliced(&mut self, mut stmts: Vec<Stmt>) -> Stmt {
        let head = stmts.remove(0);
        self.extra_stmts.extend(stmts);
        head
    }

    fn ref_let_parts(&mut self) -> (String, Vec<Binder>, Expr) {
        self.at += 1;
        let variant = self.ident();
        self.eat(&Tok::LParen);
        let binds = self.binders_to_paren();
        self.eat(&Tok::Eq);
        let scrut = self.expr();
        self.eat_semi();
        (variant, binds, scrut)
    }

    /// `let Variant(a, b) = v`: binds the payloads in the enclosing
    /// scope, or traps. Each binder is a `let` of a `match` whose default arm
    /// ([`Pattern::Other`]) panics with the canonical wording; unused positions get
    /// unspellable `@` binders.
    fn refutable_let(&mut self, line: usize) -> Stmt {
        let (variant, binds, scrut) = self.ref_let_parts();
        self.at += 1;
        let msg = format!("let `{variant}(..)` did not match");
        let mut stmts = Vec::new();
        for (j, b) in binds.iter().enumerate() {
            let arm_binds: Vec<Binder> = binds
                .iter()
                .enumerate()
                .map(|(k, b)| {
                    if k == j {
                        b.clone()
                    } else {
                        Binder::synthetic(format!("@rl{k}"))
                    }
                })
                .collect();
            let m = Expr::Match {
                id: Id::NEW,
                stmt_pos: false,
                scrutinee: Box::new(scrut.clone()),
                arms: vec![
                    MatchArm {
                        pattern: Pattern::Variant(variant.clone(), arm_binds),
                        body: ArmBody::Expr(Expr::var(b.name.clone(), line)),
                    },
                    MatchArm {
                        pattern: Pattern::Other,
                        body: ArmBody::Expr(Expr::call(
                            "panic",
                            vec![Expr::str(msg.clone())],
                            line,
                        )),
                    },
                ],
                line,
            };
            stmts.push(Stmt::Let {
                id: Id::NEW,
                name: b.name.clone(),
                mutable: false,
                ty: None,
                clause: None,
                value: m,
                line,
                col: b.col,
            });
        }
        self.spliced(stmts)
    }

    fn stmt(&mut self) -> Stmt {
        let line = self.line();
        match self.peek() {
            Tok::Let => {
                self.at += 1;
                if self.at_rule(Kind::RefLet) {
                    return self.refutable_let(line);
                }
                let mutable = self.eat(&Tok::Mut);
                let col = self.binder_pos().1;
                let name = self.ident();
                let ty = self.eat(&Tok::Colon).then(|| self.type_());
                let clause = (ty.is_some() && self.eat(&Tok::Where)).then(|| self.expr());
                self.eat(&Tok::Eq);
                let value = self.expr();
                self.eat_semi();
                Stmt::Let {
                    id: Id::NEW,
                    name,
                    mutable,
                    ty,
                    clause,
                    value,
                    line,
                    col,
                }
            }
            Tok::Return => {
                self.at += 1;
                // No value at `;`, `}` or EOF: a bare `return`.
                let value = self.at_rule(Kind::Expr).then(|| self.expr());
                self.eat_semi();
                Stmt::Return {
                    id: Id::NEW,
                    value,
                    line,
                }
            }
            Tok::Break => {
                self.at += 1;
                self.eat_semi();
                Stmt::Break { id: Id::NEW, line }
            }
            Tok::Continue => {
                self.at += 1;
                self.eat_semi();
                Stmt::Continue { id: Id::NEW, line }
            }
            Tok::If => self.if_stmt(line),
            Tok::While => {
                self.at += 1;
                // `while let PAT = e { body }` desugars to
                // `while true { if let PAT = e { body } else { break } }`.
                if self.eat(&Tok::Let) {
                    let pattern = self.pattern();
                    self.eat(&Tok::Eq);
                    let scrutinee = self.expr();
                    let body = self.block();
                    let brk = Block {
                        id: Id::NEW,
                        stmts: vec![Stmt::Break { id: Id::NEW, line }],
                    };
                    return Stmt::While {
                        id: Id::NEW,
                        cond: Expr::Bool(true, Id::NEW),
                        body: Block {
                            id: Id::NEW,
                            stmts: vec![if_let(pattern, scrutinee, body, Some(brk), line)],
                        },
                        line,
                    };
                }
                let cond = self.expr();
                let body = self.block();
                Stmt::While {
                    id: Id::NEW,
                    cond,
                    body,
                    line,
                }
            }
            Tok::For => {
                self.at += 1;
                let col = self.binder_pos().1;
                let var = self.ident();
                self.eat(&Tok::In);
                // `for x in consume xs`: `consume` counts only when an identifier
                // follows, so `for x in consume(y)` stays a call.
                let consuming = self.eat_word("consume");
                let iter = self.expr();
                let body = self.block();
                Stmt::ForIn {
                    id: Id::NEW,
                    var,
                    iter,
                    body,
                    line,
                    consuming,
                    col,
                }
            }
            Tok::Region => {
                self.at += 1;
                let body = self.block();
                Stmt::Region {
                    id: Id::NEW,
                    body,
                    line,
                }
            }
            // `drop name`: reclaim a heap value explicitly. It consumes `name`.
            Tok::Drop => {
                self.at += 1;
                let id = self.spelled();
                let name = self.ident();
                self.eat_semi();
                Stmt::Drop { id, name, line }
            }
            Tok::Ident(_) | Tok::Vself => {
                let id = self.spelled();
                let name = match self.advance() {
                    Tok::Ident(name) => name,
                    _ => "self".to_string(),
                };
                self.eat(&Tok::Eq);
                let value = self.expr();
                self.eat_semi();
                Stmt::Assign {
                    id,
                    name,
                    value,
                    line,
                }
            }
            _ => self.expr_stmt(line),
        }
    }

    /// `a.f = v`, `a[i] = v`, `a.b[i].f = v`: `postfix` read `a[i]` as `@at(a, i)`; a
    /// `=` the grammar let through makes the place a store target. Otherwise a
    /// statement `x.push(v)` writes the grown array back to wherever `x` lives, or the
    /// push lands on a copy: `sq.push(v)` is `sq = @push(sq, v)`, `r.f.push(v)` is
    /// `r.f = @push(r.f, v)`, `a[i].push(v)` is `a[i] = @push(a[i], v)`. `pop`,
    /// `swapRemove` and `remove` return a value and mutate, so they stay calls over
    /// their place.
    fn expr_stmt(&mut self, line: usize) -> Stmt {
        let e = self.expr();
        if self.eat(&Tok::Eq) {
            let Some((id, name, base, leaf)) = store_target(&e) else {
                unreachable!("the recognizer lets `=` through after a place")
            };
            let value = self.expr();
            self.eat_semi();
            let id = if base.is_empty() { id } else { Id::NEW };
            return Stmt::Store {
                id,
                name,
                base,
                leaf,
                value,
                line,
            };
        }
        self.eat_semi();
        if let Expr::Call { name, args, .. } = &e {
            if crate::prelude::rebuilds(name) {
                match args.first() {
                    Some(Expr::Var { name: recv, .. }) => {
                        return Stmt::Assign {
                            id: Id::NEW,
                            name: recv.clone(),
                            value: e,
                            line,
                        };
                    }
                    // Writes back as a field store, under the rules of `r.f = ..`.
                    Some(Expr::Field {
                        expr: base, field, ..
                    }) => {
                        let Expr::Var { name: recv, .. } = base.as_ref() else {
                            unreachable!("PushNoPlace refused this statement")
                        };
                        return Stmt::store(recv.clone(), Step::Field(field.clone()), e, line);
                    }
                    // Writes back as an element store; the index is evaluated
                    // on both the read and the store.
                    Some(Expr::Call { args: iargs, .. }) => {
                        let Expr::Var { name: recv, .. } = &iargs[0] else {
                            unreachable!("PushNoPlace refused this statement")
                        };
                        let recv = recv.clone();
                        let index = iargs[1].clone();
                        return Stmt::store(recv, Step::Index(index), e, line);
                    }
                    _ => unreachable!("PushNoPlace refused this statement"),
                }
            }
        }
        // Statement position: only a `match` here may have block arms. The
        // parser marks the node and the checker reads it.
        let mut e = e;
        if let Expr::Match { stmt_pos, .. } = &mut e {
            *stmt_pos = true;
        }
        Stmt::expr(e)
    }

    // Expressions.

    /// One level of the grammar's `climb`: an operand, then each operator at that
    /// level or tighter with its right side. `a ?? b ?? c` is `a ?? (b ?? c)`,
    /// because the climb recurses at `??`'s own level; the left reading cannot
    /// typecheck, because `a ?? b` yields an unwrapped `T`.
    fn expr(&mut self) -> Expr {
        self.at += 1;
        let mut lhs = self.unary();
        while !matches!(self.event(), Event::Close(_)) {
            let line = self.line();
            let op = binop(self.peek());
            self.at += 1;
            let rhs = self.expr();
            lhs = match op {
                Some(op) => Expr::Binary {
                    id: Id::NEW,
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                    line,
                },
                None => Self::nullish(lhs, rhs, line),
            };
        }
        self.at += 1;
        lhs
    }

    /// Desugars `a ?? b` to a `match` on the two type-agnostic patterns,
    /// so `a` may be an `Option` or a `Result`. The `@` binders cannot collide, and
    /// nesting shadows harmlessly because only the `Success` arm reads one.
    fn nullish(lhs: Expr, rhs: Expr, line: usize) -> Expr {
        Expr::Match {
            id: Id::NEW,
            stmt_pos: false,
            scrutinee: Box::new(lhs),
            arms: vec![
                MatchArm {
                    pattern: Pattern::Success(Binder::synthetic("@v")),
                    body: ArmBody::Expr(Expr::var("@v", line)),
                },
                MatchArm {
                    pattern: Pattern::Failure(Binder::synthetic("@e")),
                    body: ArmBody::Expr(rhs),
                },
            ],
            line,
        }
    }

    fn unary(&mut self) -> Expr {
        let line = self.line();
        // `consume place`: the take. `await f(..)` marks a waiting call, and it marks
        // the call under any `?` after it: `await f(x)?` is `(await f(x))?`.
        if let Event::Word(_) = self.event() {
            if self.at_word("consume") {
                let id = self.spelled();
                self.at += 1;
                return Expr::Consume {
                    id,
                    place: Box::new(self.postfix()),
                    line,
                };
            }
            if self.at_word("await") {
                self.at += 1;
                let mut e = self.postfix();
                let mut at = &mut e;
                while let Expr::Try { expr, .. } = at {
                    at = &mut **expr;
                }
                if let Expr::Call { awaits, .. } = at {
                    *awaits = true;
                }
                return e;
            }
        }
        let op = match self.peek() {
            Tok::Minus => UnOp::Neg,
            Tok::Bang => UnOp::Not,
            Tok::Tilde => UnOp::BitNot,
            _ => return self.postfix(),
        };
        self.at += 1;
        Expr::Unary {
            id: Id::NEW,
            op,
            expr: Box::new(self.unary()),
            line,
        }
    }

    /// Postfix `?`, `.field`, `.name(..)` and `[i]`, binding tighter than unary and
    /// binary operators.
    fn postfix(&mut self) -> Expr {
        let mut e = self.primary();
        loop {
            let line = match self.peek() {
                Tok::Question | Tok::Dot | Tok::LBracket => self.line(),
                _ => return e,
            };
            e = self.postfix_step(e, line);
        }
    }

    /// One postfix link over `e`, its token here, on line `line`.
    fn postfix_step(&mut self, e: Expr, line: usize) -> Expr {
        match self.advance() {
            Tok::Question => Expr::Try {
                id: Id::NEW,
                expr: Box::new(e),
                line,
            },
            Tok::Dot => {
                // A name on another line than its dot has no column a fix can use.
                let id = match self.line() == line {
                    true => self.spelled(),
                    false => Id::NEW,
                };
                let name = self.ident();
                if !self.eat(&Tok::LParen) {
                    return Expr::Field {
                        id,
                        expr: Box::new(e),
                        field: name,
                        line,
                    };
                }
                // `recv.name(args)` is sugar for `name(recv, args)`.
                let mut args = vec![e];
                self.exprs_to_paren(&mut args);
                // A method-form builtin ([`crate::prelude::Builtin::method`]) maps to its
                // internal name unless the module answers to the spelling. Only a call
                // written `recv.m(..)` asks: a node the sugar makes (`a[i]` is `@at`, a hole
                // is `@str`) is the builtin in every module.
                //
                // `wrote` keeps the written name for the type-name arm below, so
                // `F32x4.anyTrue(m)` reports what the program wrote, not `@anyTrue`.
                let wrote = name.clone();
                let name = match crate::prelude::method_builtin(&name) {
                    Some(internal) if !self.answers.contains(&name) => internal.to_string(),
                    _ => name,
                };
                // `F32x4.splat(x)`, `F32x4.load(xs, i)`, `F32x4.min(a, b)`: the receiver is a
                // type name, dropped here, so no later pass sees a bare `F32x4` variable.
                //
                // These live on the type name because a value-receiver method name is a
                // global rename in the table above, and `min`, `max` and `abs` are `std/math`
                // exports: `math.min(a, b)` arrives here in the same shape. One arm covers
                // every width.
                let n = args.len() - 1;
                let name = match args.first() {
                    // One internal prefix per width, assigned here.
                    Some(Expr::Var { name: ty, .. })
                        if ty == "F32x4" || ty == "I32x4" || ty == "F64x2" =>
                    {
                        let pre = match ty.as_str() {
                            "F32x4" => "@f32x4",
                            "I32x4" => "@i32x4",
                            _ => "@f64x2",
                        };
                        let mut it = wrote.chars();
                        let m = match it.next() {
                            Some(c) => c.to_uppercase().collect::<String>() + it.as_str(),
                            None => name.clone(),
                        };
                        args.remove(0);
                        format!("{pre}{m}")
                    }
                    _ => name,
                };
                // Method form takes no explicit type arguments: the receiver is the first one
                // and it is always concrete.
                Expr::Call {
                    id: Id::NEW,
                    dot: args.len() > n,
                    awaits: false,
                    name,
                    args,
                    type_args: Vec::new(),
                    line,
                }
            }
            _ => {
                // `recv[i]` is sugar for the bounds-checked `@at(recv, i)`.
                let idx = self.expr();
                // The `]`'s column: a `.copy()` fix lands after it.
                let id = match self.line() == line {
                    true => self.spelled(),
                    false => Id::NEW,
                };
                self.at += 1;
                Expr::Call {
                    name: "@at".to_string(),
                    args: vec![e, idx],
                    dot: false,
                    awaits: false,
                    type_args: Vec::new(),
                    line,
                    id,
                }
            }
        }
    }

    fn lambda(&mut self, line: usize) -> Expr {
        self.at += 1;
        let col = self.col();
        let params = match self.eat(&Tok::LParen) {
            true => self.binders_to_paren(),
            false => vec![self.binder()],
        };
        self.eat(&Tok::Arrow);
        let body = match self.peek() {
            Tok::LBrace => LambdaBody::Block(self.block()),
            _ => LambdaBody::Expr(Box::new(self.expr())),
        };
        self.at += 1;
        Expr::Lambda {
            id: Id::NEW,
            params,
            body,
            line,
            col,
        }
    }

    fn primary(&mut self) -> Expr {
        let first = self.token();
        let (line, col) = (first.line, first.col);
        let spelled = match self.in_hole {
            true => Id::NEW,
            false => Id::at(col),
        };
        match self.event() {
            Event::Open(Kind::Lambda, _) => return self.lambda(line),
            // `ns.Type { .. }`: a `Field` followed by `{` has no other reading, so the head
            // is a bare namespace identifier. The name rides as `"ns.Type"`; the loader
            // checks `ns` and rewrites it to the resolved declaration.
            Event::Open(Kind::NsStruct, _) => {
                self.at += 1;
                while self.eat(&Tok::LParen) {}
                let ns = match self.advance() {
                    Tok::Ident(name) => name,
                    _ => "self".to_string(),
                };
                while self.eat(&Tok::RParen) {}
                let dot = self.line();
                self.at += 1;
                let name = self.ident();
                let e = self.struct_lit(format!("{ns}.{name}"), dot);
                self.at += 1;
                return e;
            }
            Event::Open(Kind::Tagged, _) => {
                self.at += 1;
                let tag = self.ident();
                let Tok::TemplateStr { parts, exprs } = self.advance() else {
                    unreachable!("a plain string after a tag is TemplateNoHole")
                };
                let e = self.tagged_template(tag, parts, &exprs, line);
                self.at += 1;
                return e;
            }
            _ => {}
        }
        let word = matches!(self.event(), Event::Word(_));
        match self.advance() {
            Tok::Int(v) => Expr::int(v),
            Tok::Byte(v) => Expr::Byte(v, Id::NEW),
            Tok::Float(v) => Expr::Float(v, Id::NEW),
            Tok::Str(s) => Expr::str(s),
            Tok::Vself => Expr::var("self", line),
            Tok::TemplateStr { parts, exprs } => self.template(parts, &exprs, line),
            Tok::True => Expr::Bool(true, Id::NEW),
            Tok::False => Expr::Bool(false, Id::NEW),
            Tok::LParen => {
                let e = self.expr();
                self.at += 1;
                e
            }
            Tok::LBracket => self.array_literal(line),
            Tok::Match => self.match_expr(line),
            // An `if` in expression position; `stmt` reads the statement form earlier.
            Tok::If => self.if_expr(line),
            Tok::Ident(_) if word => match self.advance() {
                Tok::Str(s) => self.code_quote(&[s], &[], line, col),
                Tok::TemplateStr { parts, exprs } => self.code_quote(&parts, &exprs, line, col),
                other => unreachable!("a tag's string is {other:?}"),
            },
            Tok::Ident(name) => self.ident_expr(name, line, spelled),
            other => unreachable!("the grammar's primary is {other:?}"),
        }
    }

    /// An array literal `[a, b, c]`, or a map literal `[:]` / `["k": v]`, the
    /// bracket consumed. A `:` after the first element marks a map.
    fn array_literal(&mut self, line: usize) -> Expr {
        // `[:]`: the empty map; its value type comes from the expected `Map` type.
        if self.eat(&Tok::Colon) {
            self.at += 1;
            return Expr::MapLit {
                id: Id::NEW,
                entries: Vec::new(),
                line,
            };
        }
        let mut elems = Vec::new();
        if !self.eat(&Tok::RBracket) {
            let first = self.expr();
            if self.eat(&Tok::Colon) {
                let first_val = self.expr();
                let mut entries = vec![(first, first_val)];
                while !self.eat(&Tok::RBracket) {
                    self.eat(&Tok::Comma);
                    if self.eat(&Tok::RBracket) {
                        break;
                    }
                    let k = self.expr();
                    self.eat(&Tok::Colon);
                    let v = self.expr();
                    entries.push((k, v));
                }
                return Expr::MapLit {
                    id: Id::NEW,
                    entries,
                    line,
                };
            }
            elems.push(first);
            self.eat(&Tok::Comma);
            self.list_to(&Tok::RBracket, |b| elems.push(b.expr()));
        }
        Expr::ArrayLit {
            id: Id::NEW,
            elems,
            line,
        }
    }

    /// A name and what the grammar matched after it: a `?(`-path, type arguments,
    /// a call, or a struct literal.
    fn ident_expr(&mut self, mut name: String, line: usize, spelled: Id) -> Expr {
        // `Name?(args)`, and `ns.Name?(args)`, whose dotted path folds into `name`
        // like a dotted type.
        while self.at_rule(Kind::Folded) {
            self.at += 2;
            name = format!("{name}.{}", self.ident());
            self.at += 1;
        }
        let fallible = self.at_rule(Kind::Fallible);
        if fallible {
            self.at += 3;
        }
        let mut type_args = Vec::new();
        if self.at_rule(Kind::TypeArgs) {
            self.at += 2;
            loop {
                type_args.push(self.type_());
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.at += 2;
        }
        if self.eat(&Tok::LParen) {
            let mut args = Vec::new();
            self.exprs_to_paren(&mut args);
            if !fallible {
                if let Some(e) = Self::storage_desugar(&name, &args, line) {
                    return e;
                }
            }
            return if fallible {
                Expr::TryConstruct {
                    id: Id::NEW,
                    name,
                    args,
                    line,
                }
            } else {
                Expr::Call {
                    id: spelled,
                    dot: false,
                    awaits: false,
                    name,
                    args,
                    type_args,
                    line,
                }
            };
        }
        if matches!(self.peek(), Tok::LBrace) {
            return self.struct_lit(name, line);
        }
        Expr::Var {
            id: spelled,
            name,
            line,
        }
    }

    /// Reads the next hole: the run the recognizer kept for it, as a hole of this
    /// read. The aliases carry into it, as a hole names the enclosing type names.
    fn hole(&mut self) -> Expr {
        let Event::Hole(k) = self.event() else {
            unreachable!("a hole's run follows its string")
        };
        self.at += 1;
        let k = k as usize;
        match &mut self.src {
            Src::Read(_, holes) => {
                let run = &holes[k];
                let src = Src::Read(&run.toks, &run.holes);
                self.hole_expr(src, &run.events)
            }
            Src::Take(_, holes) => {
                let mut run = std::mem::take(&mut holes[k]);
                let src = Src::Take(&mut run.toks, &mut run.holes);
                self.hole_expr(src, &run.events)
            }
        }
    }

    fn hole_expr(&mut self, src: Src<'_>, events: &[Event]) -> Expr {
        let mut sub = Build {
            src,
            events,
            at: 0,
            in_hole: true,
            answers: self.answers,
            aliases: std::mem::take(&mut self.aliases),
            field_preds: None,
            extra_stmts: Vec::new(),
        };
        let e = sub.expr();
        self.aliases = sub.aliases;
        e
    }

    /// Desugars an untagged interpolated string `"a\{e}b"` into a
    /// `@concat`/`@str` chain. `parts.len() == exprs.len() + 1`.
    fn template(&mut self, mut parts: Vec<String>, exprs: &[Hole], line: usize) -> Expr {
        let mut pieces: Vec<Expr> = Vec::new();
        if !parts[0].is_empty() {
            pieces.push(Expr::str(std::mem::take(&mut parts[0])));
        }
        for k in 0..exprs.len() {
            let e = self.hole();
            // `@str` and `@concat` are unlexable, so a user call to `str` or `concat`
            // gets the migration hint. The loader's `mentions_runtime` reads a
            // template as a mention of `@str`.
            pieces.push(Expr::call("@str", vec![e], line));
            if !parts[k + 1].is_empty() {
                pieces.push(Expr::str(std::mem::take(&mut parts[k + 1])));
            }
        }
        // There is at least one hole, so `pieces` is non-empty; a lone piece is
        // already a `String`.
        let mut iter = pieces.into_iter();
        let first = iter.next();
        let mut acc = first.unwrap_or_else(|| unreachable!("a template has a hole"));
        for p in iter {
            acc = Expr::call("@concat", vec![acc, p], line);
        }
        acc
    }

    /// Desugars a tagged template `tag"a\{e}b"` into
    /// `tag(list([parts..]), list([value(e)..]))`, each value boxed in the built-in
    /// `Value` enum. Needs at least one hole.
    fn tagged_template(
        &mut self,
        tag: String,
        parts: Vec<String>,
        exprs: &[Hole],
        line: usize,
    ) -> Expr {
        let parts_lit = Expr::ArrayLit {
            id: Id::NEW,
            elems: parts.into_iter().map(Expr::str).collect(),
            line,
        };
        let mut values = Vec::new();
        for _ in exprs {
            let e = self.hole();
            values.push(Expr::call("value", vec![e], line));
        }
        let values_lit = Expr::ArrayLit {
            id: Id::NEW,
            elems: values,
            line,
        };
        // `@list` is unlexable, like `@str`.
        let wrap = |e| Expr::call("@list", vec![e], line);
        // The built-in `template` tag yields the `Template` record; any other tag is a
        // call `tag(parts, values)`.
        if tag == "template" {
            return Expr::StructLit {
                id: Id::NEW,
                name: "Template".to_string(),
                fields: vec![
                    ("parts".to_string(), wrap(parts_lit)),
                    ("values".to_string(), wrap(values_lit)),
                ],
                line,
            };
        }
        Expr::call(tag, vec![wrap(parts_lit), wrap(values_lit)], line)
    }

    /// Desugars a `vyrn"..."` code quote into a `Code` expression: one call,
    /// [`crate::ast::QUOTE`]. The parts are a skeleton the recognizer validated, and
    /// the holes are structural splices, not boxed `Value`s.
    fn code_quote(&mut self, parts: &[String], exprs: &[Hole], line: usize, col: usize) -> Expr {
        let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
        // Every part, empty ones too, with where it starts in this file; the
        // loader adds the module's key, the parts' author. The first part
        // starts after `vyrn"`, so a triple quote's first column is two short;
        // each later part starts after its hole's `}`.
        let int = |n: usize| Expr::int(n as i64);
        let mut args = vec![int(parts.len())];
        let mut at = (line, col + 5);
        // The text of the line the next hole sits on, up to the hole; an
        // earlier hole on the line stands as `x`.
        let mut so_far = String::new();
        for (i, part) in parts.iter().enumerate() {
            args.extend([Expr::str(part), int(at.0), int(at.1)]);
            match part.rfind('\n') {
                Some(nl) => so_far = part[nl + 1..].to_string(),
                None => so_far.push_str(part),
            }
            if let Some(hole) = exprs.get(i) {
                let before = part.chars().next_back().is_some_and(word);
                let after = parts[i + 1].chars().next().is_some_and(word);
                let value = self.hole();
                // `\{` before the hole's source and `}` after it, for a refusal.
                let len = hole.src.chars().count();
                args.extend([
                    value,
                    int((before || after) as usize),
                    Expr::str(continuation(&so_far)),
                    int(hole.line),
                    int(hole.col.saturating_sub(2)),
                    int(hole.col + len),
                ]);
                at = hole
                    .src
                    .chars()
                    .fold((hole.line, hole.col), |(l, c), ch| match ch {
                        '\n' => (l + 1, 1),
                        _ => (l, c + 1),
                    });
                at.1 += 1;
                so_far.push('x');
            }
        }
        Expr::call(crate::ast::QUOTE, args, line)
    }

    /// `match scrutinee { pattern => expr, ... }`; `match` consumed.
    fn match_expr(&mut self, line: usize) -> Expr {
        let scrutinee = self.expr();
        self.eat(&Tok::LBrace);
        let mut arms = Vec::new();
        self.list_to(&Tok::RBrace, |b| {
            let pattern = b.pattern();
            b.eat(&Tok::FatArrow);
            // `pat => { stmts }` is a block arm: a bare `{` starts no
            // expression, so the brace decides. The checker allows one only in statement
            // position. A block arm needs no comma.
            let body = match b.peek() {
                Tok::LBrace => ArmBody::Block(b.block()),
                _ => ArmBody::Expr(b.expr()),
            };
            arms.push(MatchArm { pattern, body });
        });
        Expr::Match {
            id: Id::NEW,
            scrutinee: Box::new(scrutinee),
            arms,
            stmt_pos: false,
            line,
        }
    }

    /// Desugars the `std/storage` helpers at the call site, where the
    /// codec has a concrete type, into `match`/`readFile`/`fromJson`/`toJson`/
    /// `writeAtomic`. Returns `None`, a normal call, unless the name and arity
    /// match.
    ///
    ///   save(path, value)         -> writeAtomic(path, toJson(value))
    ///   load(TypeName, path)      -> match readFile(path) {
    ///                                  Ok(t)  => match fromJson(TypeName, t) {
    ///                                    Valid(v)   => Loaded(v),
    ///                                    Invalid(i) => Corrupt(i) },
    ///                                  Err(e) => Missing }
    ///   loadOr(TypeName, path, d) -> ... Valid(v) => v, Invalid(i) => d, Err => d
    ///
    /// A module using `save` must import `writeAtomic` from `std/storage`; the read
    /// helpers need no import. The loader's `mentions_runtime` lists these names,
    /// because a skimmed body mentions `toJson` and `fromJson` only through them.
    fn storage_desugar(name: &str, args: &[Expr], line: usize) -> Option<Expr> {
        let call = |n: &str, a: Vec<Expr>| Expr::call(n, a, line);
        // `load(TypeName, path)` takes a type name as an argument; the desugar turns it
        // into the type argument `fromJson<T>(s)` takes.
        let decode = |t: &Expr, text: Expr| Expr::Call {
            id: Id::NEW,
            dot: false,
            awaits: false,
            name: "fromJson".to_string(),
            args: vec![text],
            type_args: match t {
                Expr::Var { name, .. } => vec![Type::Named(name.clone())],
                _ => Vec::new(),
            },
            line,
        };
        let var = |n: &str| Expr::var(n, line);
        match (name, args.len()) {
            ("save", 2) => Some(call(
                "writeAtomic",
                vec![args[0].clone(), call("toJson", vec![args[1].clone()])],
            )),
            ("load", 2) => {
                let decoded = Expr::Match {
                    id: Id::NEW,
                    stmt_pos: false,
                    scrutinee: Box::new(decode(&args[0], var("@t"))),
                    arms: vec![
                        MatchArm {
                            pattern: Pattern::Variant(
                                "Valid".to_string(),
                                vec![Binder::synthetic("@v")],
                            ),
                            body: ArmBody::Expr(call("Loaded", vec![var("@v")])),
                        },
                        MatchArm {
                            pattern: Pattern::Variant(
                                "Invalid".to_string(),
                                vec![Binder::synthetic("@i")],
                            ),
                            body: ArmBody::Expr(call("Corrupt", vec![var("@i")])),
                        },
                    ],
                    line,
                };
                Some(Expr::Match {
                    id: Id::NEW,
                    stmt_pos: false,
                    scrutinee: Box::new(call("readFile", vec![args[1].clone()])),
                    arms: vec![
                        MatchArm {
                            pattern: Pattern::Variant("Ok".into(), vec![Binder::synthetic("@t")]),
                            body: ArmBody::Expr(decoded),
                        },
                        MatchArm {
                            pattern: Pattern::Variant("Err".into(), vec![Binder::synthetic("@e")]),
                            body: ArmBody::Expr(var("Missing")),
                        },
                    ],
                    line,
                })
            }
            ("loadOr", 3) => {
                let default = args[2].clone();
                let decoded = Expr::Match {
                    id: Id::NEW,
                    stmt_pos: false,
                    scrutinee: Box::new(decode(&args[0], var("@t"))),
                    arms: vec![
                        MatchArm {
                            pattern: Pattern::Variant(
                                "Valid".to_string(),
                                vec![Binder::synthetic("@v")],
                            ),
                            body: ArmBody::Expr(var("@v")),
                        },
                        MatchArm {
                            pattern: Pattern::Variant(
                                "Invalid".to_string(),
                                vec![Binder::synthetic("@i")],
                            ),
                            body: ArmBody::Expr(default.clone()),
                        },
                    ],
                    line,
                };
                Some(Expr::Match {
                    id: Id::NEW,
                    stmt_pos: false,
                    scrutinee: Box::new(call("readFile", vec![args[1].clone()])),
                    arms: vec![
                        MatchArm {
                            pattern: Pattern::Variant("Ok".into(), vec![Binder::synthetic("@t")]),
                            body: ArmBody::Expr(decoded),
                        },
                        MatchArm {
                            pattern: Pattern::Variant("Err".into(), vec![Binder::synthetic("@e")]),
                            body: ArmBody::Expr(default),
                        },
                    ],
                    line,
                })
            }
            _ => None,
        }
    }

    /// `if cond { expr } else { expr }` in expression position; `if` consumed. Each
    /// branch is one expression, and `else if` nests as `else_branch`. A missing
    /// `else` is `None`, which the checker refuses by the totality rule.
    fn if_expr(&mut self, line: usize) -> Expr {
        let cond = self.expr();
        let then_branch = self.if_branch();
        let else_branch = self.eat(&Tok::Else).then(|| {
            // `else if` nests an expression-`if` with its own line, as in `if_stmt`.
            Box::new(match self.peek() {
                Tok::If => {
                    let else_line = self.line();
                    self.at += 1;
                    self.if_expr(else_line)
                }
                _ => self.if_branch(),
            })
        });
        Expr::IfExpr {
            id: Id::NEW,
            cond: Box::new(cond),
            then_branch: Box::new(then_branch),
            else_branch,
            line,
        }
    }

    /// One `{ expr }` branch of an expression-`if`: exactly one expression.
    fn if_branch(&mut self) -> Expr {
        self.at += 1;
        let e = self.expr();
        self.at += 1;
        e
    }

    /// `Name { field: expr, ... }`; the name consumed.
    fn struct_lit(&mut self, name: String, line: usize) -> Expr {
        self.eat(&Tok::LBrace);
        let mut fields = Vec::new();
        self.list_to(&Tok::RBrace, |b| {
            let fname = b.ident();
            b.eat(&Tok::Colon);
            fields.push((fname, b.expr()));
        });
        Expr::StructLit {
            id: Id::NEW,
            name,
            fields,
            line,
        }
    }

    /// Every identifier is a variant name, `Some`, `None`, `Ok` and `Err` included.
    /// `ns.Color.Red`: the loader checks `ns` and reduces the path to the variant name.
    fn pattern(&mut self) -> Pattern {
        let mut name = self.ident();
        while self.eat(&Tok::Dot) {
            name = format!("{name}.{}", self.ident());
        }
        let binds = match self.eat(&Tok::LParen) {
            true => self.binders_to_paren(),
            false => Vec::new(),
        };
        Pattern::Variant(name, binds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    fn parse_src(s: &str) -> Program {
        parse(lex(s).unwrap()).unwrap()
    }

    fn quote_refused(parts: &[&str]) -> bool {
        let parts: Vec<String> = parts.iter().map(|p| p.to_string()).collect();
        skeleton_refusal(&parts, (1, 1), &HashSet::new()).is_some()
    }

    #[test]
    fn a_quote_of_each_kind_parses() {
        let quotes: &[&[&str]] = &[
            &[""],
            &[", "],
            &[" else "],
            &["fn f() {}\nlet a = 1"],
            &["let a = 1\nfn f() {}"],
            &["let a = 1\nreturn a"],
            &["a + 1"],
            &["a().push(1)"],
            &["if a { 1 } else { 2 } + 3"],
            &["Array<Int64>"],
            &["{ a: Int64 }"],
            &["fn(Int64) -> Bool"],
            &["port: 1, host: h"],
            &["n: Array<Int64>, m: read T"],
            &["Some(x) => x"],
            &["A => 1, B => { 2 }"],
        ];
        for parts in quotes {
            assert!(!quote_refused(parts), "{parts:?}");
        }
    }

    #[test]
    fn a_hole_stands_where_the_grammar_says() {
        let quotes: &[&[&str]] = &[
            &["", ""],
            &["get", "(r)"],
            &["", "_", ""],
            &["fn ", "() {}"],
            &["", "\nfn f() {}"],
            &["fn f() {}\n", ""],
            &["", "\nreturn 1"],
            &["fn f() {\n    ", "\n}"],
            &["", "\n(a).push(1)"],
            &["", "\n(a, b)"],
            &["", "\n[i] = 1"],
            &["fn f(", ") {}"],
            &["fn f(\n", "\n) {}"],
            &["P { ", " }"],
            &["type T = { ", " }"],
            &["match x { ", " }"],
            &["impl P for T { ", " }"],
            &["protocol P { ", " }"],
            &["contract C { ", " }"],
            &["contract C { fn m(", ") }"],
            &["logging { ", " }"],
            &["import { ", " } from \"x\""],
            &["import { a } from ", ""],
            &["import * as a from ", "(1)"],
            &["Array<", ">"],
            &["", ": ", ""],
            &["", " => ", ""],
            &["// ", "\nfn f() {}"],
            &["\"\\{", "}\""],
            &["P { ", " }.x"],
            &["fn f<", ">(x: ", "<Int64>) {}"],
            &["contract C { let ", ": Int64\nfn ", "() }"],
        ];
        for parts in quotes {
            assert!(!quote_refused(parts), "{parts:?}");
        }
    }

    #[test]
    fn a_quote_is_refused_where_no_list_or_name_stands() {
        let quotes: &[&[&str]] = &[
            &[") {} fn g("],
            &["} fn g() {"],
            &["fn ("],
            &["foo()\nfn f() {}"],
            &["fn f() {}\nfoo()"],
            &["export ", ""],
            &["fn f(a: Int64, ", ") {}"],
            &["P { a: 1, ", " }"],
            &["match x { ", ", A => 1 }"],
            &["impl P for T { ", "\nfn m(self) {} }"],
            &["let s = \"a", "b\""],
            &["import { a } from", ""],
            &["from ", " \"x\" fn f() {}"],
        ];
        for parts in quotes {
            assert!(quote_refused(parts), "{parts:?}");
        }
    }

    /// Every `#unit.local` a numbered tree's `{:#?}` prints.
    fn ids(dbg: &str) -> Vec<(u32, u32)> {
        dbg.split('#')
            .skip(1)
            .map(|t| {
                let id: String = t
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect();
                let (unit, local) = id.split_once('.').unwrap();
                (unit.parse().unwrap(), local.parse().unwrap())
            })
            .collect()
    }

    #[test]
    fn parse_numbers_each_unit_once_from_one() {
        let p = parse_src(
            r#"type P = { x: Int64 }
impl Show for P { fn show(self) -> String { "p" } }
fn main() -> Int64 {
  let mut a = [1, 2]
  a[0] = a[0] + 1
  let f = (y) -> { return y }
  while let Some(x) = a.pop() { print("{x}") }
  return 0
}"#,
        );
        let mut units: std::collections::HashMap<u32, Vec<u32>> = Default::default();
        for (unit, local) in ids(&format!("{p:#?}")) {
            assert!(unit < p.units, "unit {unit} of {}", p.units);
            units.entry(unit).or_default().push(local);
        }
        for locals in units.values_mut() {
            locals.sort();
            assert_eq!(*locals, (1..=locals.len() as u32).collect::<Vec<_>>());
        }
    }

    #[test]
    fn an_edit_in_one_unit_renumbers_no_other() {
        let src = |extra: &str| {
            parse_src(&format!(
                "fn a() -> Int64 {{ return 1 + 2 }}
fn b() -> Int64 {{ {extra}return 3 }}
fn c(x: Int64) -> Int64 {{ return x }}
test \"t\" {{ assert(c(1) == 1) }}"
            ))
        };
        let (before, after) = (src(""), src("let y = [4, 5]\n"));
        let unit = |p: &Program, i: usize| ids(&format!("{:#?}", p.functions[i]));
        assert_ne!(unit(&before, 1), unit(&after, 1));
        assert_eq!(unit(&before, 0), unit(&after, 0));
        assert_eq!(unit(&before, 2), unit(&after, 2));
        let tests = |p: &Program| ids(&format!("{:#?}", p.tests));
        assert_eq!(tests(&before), tests(&after));
    }

    fn first_call(p: &Program) -> String {
        let main = p.functions.iter().find(|f| f.name == "main").unwrap();
        let mut got = None;
        let mut grab = |e: &mut Expr| {
            if let Expr::Call { name, .. } = e {
                got.get_or_insert(name.clone());
            }
        };
        let mut body = main.body.clone();
        crate::project::walk_block(&mut body, &mut grab);
        got.unwrap_or_default()
    }

    /// The method spellings of `crate::prelude::builtins`.
    fn methods() -> impl Iterator<Item = (&'static str, &'static str)> {
        crate::prelude::builtins()
            .iter()
            .filter_map(|b| Some((b.method?, b.name)))
    }

    /// A method spelling decides from a name, before any type is known. That is
    /// safe only while the name means the builtin alone: it is in
    /// `checker::RESERVED`, or a declaration of the name keeps the written call. `movecheck::every_view_and_sink_name_is_reserved` checks the same
    /// hazard for its list.
    #[test]
    fn every_method_builtin_is_reserved_or_shadowable() {
        for (surface, internal) in methods() {
            if crate::checker::RESERVED.contains(&surface) {
                continue;
            }
            let p = parse_src(&format!(
                "fn {surface}(x: Int64) -> Int64 {{ return x }}\n\
                 fn main() -> Int64 {{ let y = 1\n return y.{surface}() }}"
            ));
            assert_eq!(
                first_call(&p),
                surface,
                "`{surface}` is neither reserved nor given back, so a declaration \
                 of that name is unreachable in method form and `{internal}` \
                 answers instead"
            );
            // An import takes it back too, under its local spelling.
            let p = parse_src(&format!(
                "import {{ {surface} }} from \"lib\"\n\
                 fn main() -> Int64 {{ let y = 1\n return y.{surface}() }}"
            ));
            assert_eq!(first_call(&p), surface, "an import of `{surface}`");
        }
    }

    /// Nothing declares the name, so the builtin keeps it.
    #[test]
    fn a_method_builtin_keeps_its_name_when_nothing_else_claims_it() {
        for (surface, internal) in methods() {
            let p = parse_src(&format!(
                "fn main() -> Int64 {{ let y = 1\n return y.{surface}() }}"
            ));
            assert_eq!(first_call(&p), internal, "`{surface}` with no declaration");
        }
    }

    /// An `impl` method overrides a method-form builtin for its receiver type only;
    /// counting it as a module declaration would take the builtin from every other
    /// receiver (`std/slots`, `examples/container.vyrn`).
    #[test]
    fn an_impl_method_does_not_claim_a_method_builtin_name() {
        let p = parse_src(
            "type Ring = { data: Array<Int64> }\n\
             impl Copy for Ring { fn copy(self) -> Ring { return Ring { data: [] } } }\n\
             fn main() -> Int64 { let s = \"x\"\n let t = s.copy()\n return 0 }",
        );
        let main = p.functions.iter().find(|f| f.name == "main").unwrap();
        let mut names = Vec::new();
        let mut grab = |e: &mut Expr| {
            if let Expr::Call { name, .. } = e {
                names.push(name.clone());
            }
        };
        let mut body = main.body.clone();
        crate::project::walk_block(&mut body, &mut grab);
        assert!(names.contains(&"@copy".to_string()), "got {names:?}");
    }

    // Projections.

    #[test]
    fn place_and_yield_stay_ordinary_identifiers() {
        // The retired spelling's words stay contextual: a program using either as a
        // name keeps working.
        let p = parse_src(
            "fn place(yield: Int64) -> Int64 { let place = yield + 1\n return place }\n\
             fn main() -> Int64 { return place(1) }",
        );
        assert!(p.functions.iter().any(|f| f.name == "place"));
    }

    #[test]
    fn a_projection_carries_its_receiver_capability() {
        let p = parse_src(
            "type Ring = { data: Array<Int64> }\n\
             impl Index for Ring {\n\
                 fn at(read self, i: Int64) -> read Int64 { return self.data[i] }\n\
                 fn atSet(modify self, i: Int64) -> modify Int64 { return self.data[i] }\n\
             }\n\
             fn main() -> Int64 { return 0 }",
        );
        let ps = &p.impls[0].places;
        assert_eq!(ps.len(), 2);
        assert_eq!(ps[0].params[0].capability, Capability::Read);
        assert_eq!(ps[1].params[0].capability, Capability::Modify);
    }

    #[test]
    fn a_protocol_declares_a_projection() {
        // The result capability marks the requirement, under the receiver rule an
        // impl member follows.
        let p = parse_src(
            "protocol Pick { fn at(read self, i: Int64) -> read Int64 }\n\
             fn main() -> Int64 { return 0 }",
        );
        let sig = &p.protocols[0].methods[0];
        assert_eq!(sig.result_cap, Some(Capability::Read));
        let src = "protocol Pick { fn at(self, i: Int64) -> modify Int64 }\n\
                   fn main() -> Int64 { return 0 }";
        let (_, errs) = parse_accum(lex(src).unwrap());
        assert!(
            errs.iter()
                .any(|e| e.message.contains("receiver must be `modify self`")),
            "{errs:?}"
        );
    }

    // The refutable `let`.

    #[test]
    fn a_refutable_let_desugars_to_a_match_with_a_default_arm() {
        let p = parse_src(
            "type Shape = enum { Circle(Int64), Dot }\n\
             fn main() -> Int64 { let c = Circle(7)\n let Circle(r) = c\n return r }",
        );
        let f = p.functions.iter().find(|f| f.name == "main").unwrap();
        let Stmt::Let { name, value, .. } = &f.body.stmts[1] else {
            panic!("expected the desugared let, got {:?}", f.body.stmts[1]);
        };
        assert_eq!(name, "r");
        let Expr::Match { arms, .. } = value else {
            panic!("expected a match initializer, got {value:?}");
        };
        assert_eq!(arms.len(), 2);
        assert!(
            matches!(&arms[0].pattern, Pattern::Variant(v, b) if v == "Circle" && b.len() == 1 && b[0].name == "r")
        );
        assert!(matches!(&arms[1].pattern, Pattern::Other));
        // The trap is an ordinary `panic`: the wording has one source and the loader
        // stamps the site.
        let Some(Expr::Call { name, args, .. }) = arms[1].body.as_expr() else {
            panic!("expected the panic arm");
        };
        assert!(crate::ast::is_panic(name));
        assert!(matches!(&args[0], Expr::Str(s, _) if s == "let `Circle(..)` did not match"));
    }

    #[test]
    fn a_multi_payload_refutable_let_binds_each_position() {
        let p = parse_src(
            "type Pair = enum { Two(Int64, Int64), Zero }\n\
             fn main() -> Int64 { let q = Two(1, 2)\n let Two(a, b) = q\n return a + b }",
        );
        let f = p.functions.iter().find(|f| f.name == "main").unwrap();
        // One `let` per binder; the positions a statement does not keep get
        // `@`-prefixed binders.
        let lets: Vec<&String> = f.body.stmts[1..3]
            .iter()
            .map(|s| match s {
                Stmt::Let { name, .. } => name,
                other => panic!("expected a let, got {other:?}"),
            })
            .collect();
        assert_eq!(lets, ["a", "b"]);
    }

    #[test]
    fn a_type_named_read_still_parses_in_return_position() {
        // The result capability counts only where a type follows, so `-> read {`
        // keeps `read` as a type name and the brace as the body.
        let p = parse_src(
            "type read = { v: Int64 }\n\
             fn f() -> read { return read { v: 1 } }\n\
             fn main() -> Int64 { return 0 }",
        );
        let f = p.functions.iter().find(|f| f.name == "f").unwrap();
        assert_eq!(f.ret, Type::Named("read".into()));
    }

    fn if_let(s: &Stmt) -> (&Pattern, &Expr, &Block, &Block) {
        let Stmt::Expr(m, _) = s else {
            panic!("expected a statement match, found {s:?}");
        };
        m.as_if_let().expect("an if-let match")
    }

    #[test]
    fn if_let_parses_to_a_match_with_a_default_arm() {
        let src = "fn main() -> Int64 {                    if let Some(v) = f() { return v } else { return 0 } }";
        let p = parse_src(src);
        let (pat, _, _, els) = if_let(&p.functions[0].body.stmts[0]);
        assert!(matches!(pat, Pattern::Variant(v, _) if v == "Some"));
        assert!(matches!(els.stmts[0], Stmt::Return { .. }));
    }

    #[test]
    fn while_let_desugars_to_while_true_with_if_let_else_break() {
        let src = "fn main() -> Int64 { while let Some(v) = f() { print(v) } return 0 }";
        let p = parse_src(src);
        let Stmt::While { cond, body, .. } = &p.functions[0].body.stmts[0] else {
            panic!("expected a while loop");
        };
        assert_eq!(*cond, Expr::Bool(true, Id::NEW));
        let (_, _, _, els) = if_let(&body.stmts[0]);
        assert!(matches!(els.stmts[0], Stmt::Break { .. }));
    }

    #[test]
    fn else_if_let_chains() {
        let src = "fn main() -> Int64 {                    if let Some(a) = f() { return a }                    else if let Ok(b) = g() { return b }                    else { return 0 } }";
        let p = parse_src(src);
        let (_, _, _, els) = if_let(&p.functions[0].body.stmts[0]);
        if_let(&els.stmts[0]);
    }

    #[test]
    fn semicolons_are_optional() {
        // No terminators: statements end where an expression cannot extend.
        let src = "fn main() -> Int64 {\n\
                   let a = 1\n\
                   let mut b = 2\n\
                   b = b + a\n\
                   return b\n\
                   }";
        let p = parse_src(src);
        let f = &p.functions[0];
        assert_eq!(f.body.stmts.len(), 4);
        assert!(matches!(f.body.stmts[0], Stmt::Let { .. }));
        assert!(matches!(f.body.stmts[2], Stmt::Assign { .. }));
        assert!(matches!(
            f.body.stmts[3],
            Stmt::Return { value: Some(_), .. }
        ));
    }

    #[test]
    fn else_if_desugars_to_nested_if_with_honest_lines() {
        // `else if` nests: the else-block holds one statement, the chained `if`, with
        // its own line.
        let src = "fn f() -> Int64 {\n\
                   if a == 1 {\n\
                   return 1\n\
                   } else if a == 2 {\n\
                   return 2\n\
                   } else {\n\
                   return 3\n\
                   }\n\
                   }";
        let p = parse_src(src);
        let f = &p.functions[0];
        let Stmt::If {
            else_block: Some(eb),
            ..
        } = &f.body.stmts[0]
        else {
            panic!("expected an if with an else");
        };
        // The else-block holds the chained `if`, at source line 4.
        assert_eq!(eb.stmts.len(), 1);
        let Stmt::If {
            line,
            else_block: Some(inner),
            ..
        } = &eb.stmts[0]
        else {
            panic!("else-block's sole statement is the chained if");
        };
        assert_eq!(*line, 4, "chained if keeps its own line for diagnostics");
        assert_eq!(inner.stmts.len(), 1);
        assert!(matches!(inner.stmts[0], Stmt::Return { .. }));
    }

    #[test]
    fn else_if_equals_the_nested_form() {
        // `else if` and a hand-written `else { if }` give identical trees.
        let sugar = parse_src(
            "fn f() -> Int64 { if a { return 1 } else if b { return 2 } else { return 3 } }",
        );
        let nested = parse_src(
            "fn f() -> Int64 { if a { return 1 } else { if b { return 2 } else { return 3 } } }",
        );
        assert_eq!(sugar.functions[0].body, nested.functions[0].body);
    }

    #[test]
    fn namespace_import_parses() {
        // `import * as ns from <source>` binds a namespace and no flat names.
        let p = parse_src(
            "import * as api from \"./api\" \
             import * as ui from pages(\"./pages\") \
             fn main() -> Int64 { return 0 }",
        );
        assert_eq!(p.imports[0].namespace.as_deref(), Some("api"));
        assert!(p.imports[0].names.is_empty());
        assert!(matches!(&p.imports[0].source, ImportSource::Path(s) if s == "./api"));
        assert_eq!(p.imports[1].namespace.as_deref(), Some("ui"));
        assert!(
            matches!(&p.imports[1].source, ImportSource::Generator { name, .. } if name == "pages")
        );
    }

    #[test]
    fn namespace_qualified_type_and_record_parse() {
        // `ns.User` in a type position and `ns.Req { .. }` record construction.
        let p = parse_src(
            "import * as api from \"./api\" \
             fn main() -> Int64 { let r: api.User = api.Req { id: 1 } return 0 }",
        );
        let Stmt::Let {
            ty: Some(ty),
            value,
            ..
        } = &p.functions[0].body.stmts[0]
        else {
            panic!("let with type")
        };
        assert_eq!(*ty, Type::Named("api.User".into()));
        assert!(matches!(value, Expr::StructLit { name, .. } if name == "api.Req"));
    }

    #[test]
    fn gen_fn_parses_as_a_generator_marked_function() {
        // `gen fn` is an ordinary function with `is_gen` set.
        let p = parse_src(
            "gen fn make(dir: String) -> String { return \"fn x() -> Int64 { return 0 }\" } \
             fn main() -> Int64 { return 0 }",
        );
        let g = p.functions.iter().find(|f| f.name == "make").unwrap();
        assert!(g.is_gen);
        assert!(!g.is_extern);
        let m = p.functions.iter().find(|f| f.name == "main").unwrap();
        assert!(!m.is_gen);
    }

    #[test]
    fn export_to_names_its_paths_and_leaves_to_a_name_elsewhere() {
        let p = parse_src(
            "export to \"./a\", \"std/b\" gen fn g() -> String { return \"\" } \
             export type to = Int64 \
             fn main() -> Int64 { let to = 1 return to }",
        );
        let g = p.functions.iter().find(|f| f.name == "g").unwrap();
        assert!(g.is_gen && g.exported);
        assert_eq!(g.export_to, ["./a", "std/b"]);
        assert!(p
            .type_decls
            .iter()
            .any(|t| t.name == "to" && t.export_to.is_empty()));
    }

    #[test]
    fn export_gen_fn_parses() {
        let p = parse_src(
            "export gen fn g() -> String { return \"\" } fn main() -> Int64 { return 0 }",
        );
        let g = p.functions.iter().find(|f| f.name == "g").unwrap();
        assert!(g.is_gen && g.exported);
    }

    #[test]
    fn mut_fn_parses_and_export_goes_outside() {
        // `mut fn` declares that a procedure changes state; `export` is outermost.
        let p = parse_src(
            "export mut fn create(x: Int64) -> Int64 { return x } \
             mut fn touch() {} \
             fn read() -> Int64 { return 0 } \
             fn main() -> Int64 { let mut n = 0 return n }",
        );
        let c = p.functions.iter().find(|f| f.name == "create").unwrap();
        assert!(c.is_mut && c.exported && !c.is_gen && !c.is_extern);
        assert!(
            p.functions
                .iter()
                .find(|f| f.name == "touch")
                .unwrap()
                .is_mut
        );
        assert!(
            !p.functions
                .iter()
                .find(|f| f.name == "read")
                .unwrap()
                .is_mut
        );
        // A local `let mut` is untouched: `mut` starts a declaration only before `fn`
        // at the top level.
        assert!(
            matches!(&p.functions.iter().find(|f| f.name == "main").unwrap().body.stmts[0],
            Stmt::Let { name, mutable: true, .. } if name == "n")
        );
    }

    #[test]
    fn gen_is_still_an_identifier_elsewhere() {
        // `gen` as a variable name is unharmed.
        let p = parse_src("fn main() -> Int64 { let gen = 3 return gen }");
        let f = &p.functions[0];
        assert!(matches!(&f.body.stmts[0], Stmt::Let { name, .. } if name == "gen"));
    }

    #[test]
    fn import_from_generator_call_parses() {
        // `import { .. } from ident(args)`.
        let p = parse_src(
            "import { t, TransKey } from i18n(\"./locales\", 3) \
             fn main() -> Int64 { return 0 }",
        );
        let imp = &p.imports[0];
        assert_eq!(
            imp.names,
            vec![ImportName::bare("t"), ImportName::bare("TransKey")]
        );
        match &imp.source {
            ImportSource::Generator { name, args, .. } => {
                assert_eq!(name, "i18n");
                assert_eq!(args.len(), 2);
                assert!(matches!(&args[0], Expr::Str(s, _) if s == "./locales"));
                assert!(matches!(&args[1], Expr::Int(3, _)));
            }
            other => panic!("expected a generator source, got {other:?}"),
        }
    }

    #[test]
    fn import_aliasing_parses_original_and_alias() {
        // `original as alias`; a bare name keeps `alias: None`.
        let p = parse_src(
            "import { getUser as fetchUser, User } from \"./api\" \
             fn main() -> Int64 { return 0 }",
        );
        assert_eq!(
            p.imports[0].names,
            vec![
                ImportName {
                    original: "getUser".into(),
                    alias: Some("fetchUser".into())
                },
                ImportName::bare("User"),
            ]
        );
        assert_eq!(p.imports[0].names[0].local(), "fetchUser");
        assert_eq!(p.imports[0].names[1].local(), "User");
    }

    #[test]
    fn as_is_still_an_identifier_elsewhere() {
        // A variable named `as` is unharmed.
        let p = parse_src("fn main() -> Int64 { let as = 3 return as }");
        let f = &p.functions[0];
        assert!(matches!(&f.body.stmts[0], Stmt::Let { name, .. } if name == "as"));
    }

    #[test]
    fn inline_field_where_desugars_to_synthetic_validated_type() {
        let src = "type User = { name: String where value.byteLength >= 3, age: Int64 } \
                   fn main() -> Int64 { return 0 }";
        let p = parse_src(src);
        // The synthetic `User.name` declaration carries the predicate,
        let synth = p
            .type_decls
            .iter()
            .find(|t| t.name == "User.name")
            .expect("synthetic decl");
        assert_eq!(synth.base, Type::Str);
        assert!(synth.predicate.is_some());
        // and the field's type names it.
        let user = p.type_decls.iter().find(|t| t.name == "User").unwrap();
        let Type::Record(fields) = &user.base else {
            panic!("record")
        };
        assert_eq!(fields[0].ty, Type::Named("User.name".into()));
        assert_eq!(fields[1].ty, Type::Int, "unrefined fields untouched");
        assert!(user.predicate.is_none());
    }

    #[test]
    fn inline_field_where_composes_with_cross_field_where() {
        let src = "type R = { a: Int64 where value > 0, b: Int64 } where a < b \
                   fn main() -> Int64 { return 0 }";
        let p = parse_src(src);
        let r = p.type_decls.iter().find(|t| t.name == "R").unwrap();
        assert!(
            r.predicate.is_some(),
            "cross-field where stays on the record"
        );
        assert!(
            p.type_decls.iter().any(|t| t.name == "R.a"),
            "field where desugars"
        );
    }

    #[test]
    fn parses_top_level_let_module_state() {
        // `let [mut] name [: Type] = init` at the top level.
        let src = "let mut hits: Int64 = 0\n\
                   let banner = \"hi\"\n\
                   fn main() -> Int64 { return 0 }";
        let p = parse_src(src);
        assert_eq!(p.globals.len(), 2);
        assert_eq!(p.globals[0].name, "hits");
        assert!(p.globals[0].mutable);
        assert_eq!(p.globals[0].ty, Some(Type::Int));
        assert_eq!(p.globals[1].name, "banner");
        assert!(!p.globals[1].mutable);
        assert_eq!(p.globals[1].ty, None);
    }

    #[test]
    fn top_level_let_doc_comment_attaches() {
        let src = "/// the live counter\nlet mut hits = 0\nfn main() -> Int64 { return 0 }";
        let p = parse_src(src);
        assert_eq!(p.globals[0].doc.as_deref(), Some("the live counter"));
    }

    #[test]
    fn bad_top_level_let_recovers_to_next_decl() {
        // A malformed global must not swallow the next function: recovery skips to
        // the next top-level `fn`.
        let src = "let 123 = 5\nfn main() -> Int64 { return 0 }";
        let (p, errors) = parse_accum(lex(src).unwrap());
        assert!(
            !errors.is_empty(),
            "expected a parse error for the bad global"
        );
        assert!(
            p.functions.iter().any(|f| f.name == "main"),
            "recovered to `main`"
        );
    }

    // Test blocks.

    #[test]
    fn parses_test_declaration() {
        let src = "test \"adds\" {\n\
                   assert(1 + 1 == 2)\n\
                   assertEq(2 + 2, 4)\n\
                   }";
        let p = parse_src(src);
        assert_eq!(p.tests.len(), 1);
        assert_eq!(p.tests[0].name, "adds");
        assert_eq!(p.tests[0].body.stmts.len(), 2);
        assert_eq!(p.tests[0].line, 1);
    }

    #[test]
    fn test_is_only_contextual_before_a_string() {
        // `test` not followed by a string is a plain variable.
        let src = "fn main() -> Int64 { let test = 5 return test }";
        let p = parse_src(src);
        assert!(p.tests.is_empty());
        assert_eq!(p.functions[0].body.stmts.len(), 2);
    }

    #[test]
    fn test_doc_comment_attaches() {
        let src = "/// checks the happy path\ntest \"ok\" { assert(true) }";
        let p = parse_src(src);
        assert_eq!(p.tests[0].doc.as_deref(), Some("checks the happy path"));
    }

    #[test]
    fn bad_test_recovers_to_next_decl() {
        // A malformed test body must not swallow the next function.
        let src = "test \"broken\" { let = }\nfn main() -> Int64 { return 0 }";
        let (p, errors) = parse_accum(lex(src).unwrap());
        assert!(
            !errors.is_empty(),
            "expected a parse error for the bad test"
        );
        assert!(
            p.functions.iter().any(|f| f.name == "main"),
            "recovered to `main`"
        );
    }

    // Bench blocks.

    #[test]
    fn parses_bench_declaration() {
        let src = "bench \"push\" {\n\
                   let mut xs: Array<Int64> = []\n\
                   blackBox(xs.length)\n\
                   }";
        let p = parse_src(src);
        assert_eq!(p.benches.len(), 1);
        assert_eq!(p.benches[0].name, "push");
        assert_eq!(p.benches[0].body.stmts.len(), 2);
        assert_eq!(p.benches[0].line, 1);
        assert!(p.tests.is_empty());
    }

    #[test]
    fn bench_is_only_contextual_before_a_string() {
        // `bench` not followed by a string is a plain variable.
        let src = "fn main() -> Int64 { let bench = 5 return bench }";
        let p = parse_src(src);
        assert!(p.benches.is_empty());
        assert_eq!(p.functions[0].body.stmts.len(), 2);
    }

    #[test]
    fn bench_doc_comment_attaches() {
        let src = "/// times the hot path\nbench \"hot\" { blackBox(1) }";
        let p = parse_src(src);
        assert_eq!(p.benches[0].doc.as_deref(), Some("times the hot path"));
    }

    #[test]
    fn stray_semicolon_after_block_statement_is_tolerated() {
        // A stray `;` after `if { .. }` is skipped.
        let src = "fn main() -> Int64 { if true { print(1) }; return 0 }";
        let p = parse_src(src);
        assert_eq!(p.functions[0].body.stmts.len(), 2);
    }

    #[test]
    fn gteq_splits_when_closing_a_generic() {
        // The lexer max-munches `>=`; the parser splits it.
        let src = "fn main() -> Int64 { let x: Array<Int64>= [] return x.length }";
        let p = parse_src(src);
        assert!(matches!(
            p.functions[0].body.stmts[0],
            Stmt::Let {
                ty: Some(Type::Array(_)),
                ..
            }
        ));
    }

    #[test]
    fn identifier_before_next_lines_string_is_not_a_tagged_template() {
        // Without semicolons, a statement ending in a variable followed by one
        // starting with a string is not a tagged template: adjacency needs one line.
        let src = "fn main() -> Int64 {\n\
                   let y = 1\n\
                   let z = y\n\
                   print(\"done\")\n\
                   return 0\n\
                   }";
        let p = parse_src(src);
        assert_eq!(p.functions[0].body.stmts.len(), 4);
    }

    #[test]
    fn a_node_in_a_hole_names_the_line_and_column_it_sits_on() {
        // Two lambdas in two holes get two keys (#471).
        let mut p = parse_src("fn main() -> Int64 {\n  print(\"\\{f(x -> x)} \\{f(x -> x)}\")\n}");
        let mut at = Vec::new();
        crate::project::walk_block(&mut p.functions[0].body, &mut |e| {
            if let Expr::Lambda { line, col, .. } = e {
                at.push((*line, *col));
            }
        });
        assert_eq!(at, [(2, 14), (2, 27)]);
    }

    #[test]
    fn failed_generic_decl_does_not_leak_type_params() {
        // After a broken `fn bad<T>`, `T` in the next declaration is a named type,
        // not a stale `Type::Param`.
        let src = "fn bad<T>(x: T -> T { return x } \
                   type T = Int64 \
                   fn ok(x: T) -> T { return x } \
                   fn main() -> Int64 { return ok(1) }";
        let toks = lex(src).unwrap();
        let (prog, errors) = parse_bare(toks);
        assert!(!errors.is_empty(), "the broken decl must actually fail");
        let ok = prog
            .functions
            .iter()
            .find(|f| f.name == "ok")
            .expect("ok parsed");
        assert_eq!(
            ok.params[0].ty,
            Type::Named("T".into()),
            "not a stale Param"
        );
    }

    #[test]
    fn bare_return_before_brace_needs_no_semicolon() {
        let src = "fn f(x: Int64) { if x > 0 { return } print(x) } fn main() -> Int64 { return 0 }";
        let p = parse_src(src);
        let f = p.functions.iter().find(|f| f.name == "f").unwrap();
        match &f.body.stmts[0] {
            Stmt::If { then_block, .. } => {
                assert!(matches!(
                    then_block.stmts[0],
                    Stmt::Return { value: None, .. }
                ));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn attaches_doc_comments_to_declarations() {
        let src = "/// first line\n/// second line\nfn f() -> Int64 { return 0; }\n\
                   // not a doc\n/// a type doc\ntype T = Int64;\n\
                   //// four slashes: plain comment\nfn main() -> Int64 { return 0; }";
        let p = parse_src(src);
        let f = p.functions.iter().find(|f| f.name == "f").unwrap();
        assert_eq!(f.doc.as_deref(), Some("first line\nsecond line"));
        let t = p.type_decls.iter().find(|t| t.name == "T").unwrap();
        assert_eq!(t.doc.as_deref(), Some("a type doc"));
        // `//` and `////` are plain comments, so `main` has no doc.
        let m = p.functions.iter().find(|f| f.name == "main").unwrap();
        assert_eq!(m.doc, None);
    }

    /// A `///` block separated from the declaration by a blank line is a file
    /// header, not the declaration's doc (LSP hover and `schemaOf(T).doc` show it).
    #[test]
    fn detached_doc_blocks_do_not_attach() {
        let p = parse_src("/// This file does things.\n/// Extensively.\n\nfn f() -> Int64 { return 0; }\nfn main() -> Int64 { return 0; }");
        let f = p.functions.iter().find(|f| f.name == "f").unwrap();
        assert_eq!(f.doc, None, "a detached header is not the decl's doc");

        // Only the block directly above the declaration attaches.
        let p = parse_src("/// Header.\n\n/// The real doc.\nfn f() -> Int64 { return 0; }\nfn main() -> Int64 { return 0; }");
        let f = p.functions.iter().find(|f| f.name == "f").unwrap();
        assert_eq!(f.doc.as_deref(), Some("The real doc."));
    }

    /// A `///` above a method in a `protocol` body attaches to that method; the
    /// blank-line rule applies here too.
    #[test]
    fn attaches_doc_comments_to_protocol_methods() {
        let p = parse_src(
            "protocol P {\n/// what show does\nfn show(self) -> String;\n\
             /// detached\n\nfn debug(self) -> String;\nfn plain(self) -> String;\n}\n\
             fn main() -> Int64 { return 0; }",
        );
        let proto = p.protocols.iter().find(|p| p.name == "P").unwrap();
        let doc = |n: &str| {
            proto
                .methods
                .iter()
                .find(|m| m.name == n)
                .unwrap()
                .doc
                .clone()
        };
        assert_eq!(doc("show").as_deref(), Some("what show does"));
        assert_eq!(doc("debug"), None, "a blank line detaches the block");
        assert_eq!(doc("plain"), None);
    }

    #[test]
    fn parses_precedence() {
        // 1 + 2 * 3 is Add(1, Mul(2, 3)).
        let p = parse_src("fn main() -> Int64 { return 1 + 2 * 3; }");
        let f = &p.functions[0];
        match &f.body.stmts[0] {
            Stmt::Return {
                value:
                    Some(Expr::Binary {
                        op: BinOp::Add,
                        rhs,
                        ..
                    }),
                ..
            } => {
                assert!(matches!(**rhs, Expr::Binary { op: BinOp::Mul, .. }));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn bitwise_precedence_and_shift_generic_disambiguation() {
        // `x & mask == 0` is `(x & mask) == 0`: bitwise binds tighter than comparison.
        let p = parse_src("fn main() -> Int64 { let b = x & mask == 0; return 0; }");
        let f = &p.functions[0];
        match &f.body.stmts[0] {
            Stmt::Let {
                value: Expr::Binary {
                    op: BinOp::Eq, lhs, ..
                },
                ..
            } => {
                assert!(
                    matches!(
                        **lhs,
                        Expr::Binary {
                            op: BinOp::BitAnd,
                            ..
                        }
                    ),
                    "`x & mask == 0` must be `(x & mask) == 0`, got {lhs:?}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }

        // `a + b << c` is `(a + b) << c`.
        let p = parse_src("fn main() -> Int64 { let s = a + b << c; return 0; }");
        match &p.functions[0].body.stmts[0] {
            Stmt::Let {
                value:
                    Expr::Binary {
                        op: BinOp::Shl,
                        lhs,
                        ..
                    },
                ..
            } => {
                assert!(matches!(**lhs, Expr::Binary { op: BinOp::Add, .. }));
            }
            other => panic!("unexpected: {other:?}"),
        }

        // A two-generic type and a `>>` shift in one program both parse.
        let p = parse_src(
            "fn f(m: Array<Array<Int64>>) -> Int64 { return m.length >> 1; }\n\
             fn main() -> Int64 { return 0; }",
        );
        let f = p.functions.iter().find(|f| f.name == "f").unwrap();
        assert!(
            matches!(f.params[0].ty, Type::Array(_)),
            "nested generic must parse as Array"
        );
        assert!(matches!(
            f.body.stmts[0],
            Stmt::Return {
                value: Some(Expr::Binary { op: BinOp::Shr, .. }),
                ..
            }
        ));
    }

    #[test]
    fn parses_function_with_params() {
        let p = parse_src("fn add(a: Int64, b: Int64) -> Int64 { return a + b; }");
        let f = &p.functions[0];
        assert_eq!(f.name, "add");
        assert_eq!(f.params.len(), 2);
        assert_eq!(f.ret, Type::Int);
    }

    #[test]
    fn parses_region_block() {
        let p = parse_src("fn main() -> Int64 { region { print(1); } return 0; }");
        let f = &p.functions[0];
        match &f.body.stmts[0] {
            Stmt::Region { body, .. } => assert_eq!(body.stmts.len(), 1),
            other => panic!("expected region, got {other:?}"),
        }
    }

    // Statement recovery inside a body.

    #[test]
    fn two_bad_statements_each_report_and_good_ones_survive() {
        let src = "fn main() -> Int64 {\n\
                   let a = ;\n\
                   let good = 1\n\
                   let b = ;\n\
                   return good\n\
                   }";
        let (p, errors) = parse_accum(lex(src).unwrap());
        assert_eq!(
            errors.len(),
            2,
            "one diagnostic per bad statement: {errors:?}"
        );
        assert_eq!(errors[0].line, 2);
        assert_eq!(errors[1].line, 4);
        // The good statements around the bad ones are kept.
        let body = &p.functions[0].body.stmts;
        assert!(body
            .iter()
            .any(|s| matches!(s, Stmt::Let { name, .. } if name == "good")));
        assert!(matches!(body.last(), Some(Stmt::Return { .. })));
    }

    #[test]
    fn body_error_does_not_hide_a_later_bad_declaration() {
        // A body error must not swallow a separate broken declaration after it, and
        // `main` still parses.
        let src = "fn main() -> Int64 {\n\
                   let x = ;\n\
                   return 0\n\
                   }\n\
                   fn bad<T>(x: T -> T { return x }";
        let (p, errors) = parse_accum(lex(src).unwrap());
        assert!(
            errors.len() >= 2,
            "body error AND decl error both reported: {errors:?}"
        );
        assert_eq!(errors[0].line, 2, "body error comes first in source order");
        assert!(
            errors.iter().any(|e| e.line >= 5),
            "the bad decl is also reported: {errors:?}"
        );
        assert!(
            p.functions.iter().any(|f| f.name == "main"),
            "main survives"
        );
    }

    #[test]
    fn recovery_inside_a_nested_block() {
        // A bad statement in an `if` body recovers within that block.
        let src = "fn main() -> Int64 {\n\
                   let mut n = 0\n\
                   if n == 0 {\n\
                   let x = ;\n\
                   n = 1\n\
                   }\n\
                   return n\n\
                   }";
        let (p, errors) = parse_accum(lex(src).unwrap());
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].line, 4);
        let body = &p.functions[0].body.stmts;
        assert!(
            matches!(body.last(), Some(Stmt::Return { .. })),
            "return survives"
        );
        match body.iter().find(|s| matches!(s, Stmt::If { .. })) {
            Some(Stmt::If { then_block, .. }) => {
                // The bad `let x` is dropped; `n = 1` remains.
                assert!(then_block
                    .stmts
                    .iter()
                    .any(|s| matches!(s, Stmt::Assign { .. })));
            }
            _ => panic!("the `if` statement survives"),
        }
    }

    // A store is one statement: its root, the steps to the place, and the leaf.

    fn store(src: &str) -> (String, Vec<String>, String) {
        let body = parse_src(&format!("fn main() -> Int64 {{ {src} return 0 }}")).functions[0]
            .body
            .stmts
            .clone();
        let [Stmt::Store {
            name, base, leaf, ..
        }, _] = &body[..]
        else {
            panic!("expected one store and the return, got {body:?}");
        };
        let step = |s: &Step| match s {
            Step::Field(f) => format!(".{f}"),
            Step::Index(_) => "[]".to_string(),
        };
        (name.clone(), base.iter().map(step).collect(), step(leaf))
    }

    /// A nested store carries its path whole, root first, with each operand once:
    /// no temp, no move-out, no put-back.
    #[test]
    fn a_nested_store_is_one_statement_with_its_path() {
        assert_eq!(
            store("a[0].f = 9"),
            ("a".into(), vec!["[]".into()], ".f".into())
        );
        assert_eq!(
            store("s.xs[0] = 9"),
            ("s".into(), vec![".xs".into()], "[]".into())
        );
        assert_eq!(
            store("o.i.xs[0] = 9"),
            ("o".into(), vec![".i".into(), ".xs".into()], "[]".into())
        );
        let (_, base, leaf) = store("rows[f()][0] = g()");
        assert_eq!((base.len(), leaf.as_str()), (1, "[]"));
        let dump = format!(
            "{:?}",
            parse_src("fn main() -> Int64 { rows[f()][0] = g()  return 0 }").functions[0].body
        );
        assert_eq!(dump.matches(r#"Call { name: "f""#).count(), 1, "{dump}");
    }

    /// `store_target` reads `@at` and `@slot` as an element step, and answers no
    /// target for a bare name or a call result.
    #[test]
    fn store_target_walks_a_place_from_its_root() {
        let place = |src: &str| match &parse_src(&format!("fn main() -> Int64 {{ let _ = {src} }}"))
            .functions[0]
            .body
            .stmts[0]
        {
            Stmt::Let { value, .. } => store_target(value),
            other => panic!("{other:?}"),
        };
        let (_, name, base, leaf) = place("a.b[i].c").unwrap();
        assert_eq!((name.as_str(), base.len()), ("a", 2));
        assert!(matches!(leaf, Step::Field(f) if f == "c"));
        assert!(place("a").is_none(), "a name is no store target");
        assert!(place("f()[0]").is_none(), "a call result has no address");
    }

    /// A target no store reaches is refused by its shape, before the value parses.
    #[test]
    fn a_store_target_with_no_place_is_refused_by_shape() {
        let refused = |target: &str| {
            let src = format!("fn main() -> Int64 {{ {target} = 1  return 0 }}");
            let err = parse(crate::lexer::lex(&src).unwrap()).unwrap_err();
            err.message
        };
        assert_eq!(
            refused("f()[0]"),
            crate::rules::Rule::IndexAssignTarget {}.render()
        );
        assert_eq!(
            refused("f()[0].x"),
            crate::rules::Rule::FieldAssignTarget {}.render()
        );
    }

    #[test]
    fn a_removal_over_a_place_stays_a_call_over_it() {
        // The parser hoists nothing, in any position.
        for src in [
            "fn main() -> Int64 { let mut s: S = S { xs: [1] }  if s.xs.pop() == None { return 1 }  return 0 }",
            "fn main() -> Int64 { let mut s: S = S { xs: [1] }  let x = s.xs.pop()  return 0 }",
        ] {
            let p = parse_src(src);
            assert!(format!("{:?}", p.functions[0].body.stmts)
                .contains(r#"Call { name: "@pop", args: [Field"#));
        }
    }

    // A statement `push` writes back through its receiver place.

    #[test]
    fn push_on_variable_desugars_to_assign() {
        // `sq.push(x)` becomes `sq = @push(sq, x)`.
        let p =
            parse_src("fn main() -> Int64 { let mut sq: Array<Int64> = []  sq.push(1)  return 0 }");
        let stmts = &p.functions[0].body.stmts;
        match &stmts[1] {
            Stmt::Assign {
                name,
                value: Expr::Call { name: c, .. },
                ..
            } => {
                assert_eq!(name, "sq");
                assert_eq!(c, "@push");
            }
            other => panic!("expected `sq = @push(sq, 1)`, got {other:?}"),
        }
    }

    #[test]
    fn push_on_record_field_desugars_to_a_field_store() {
        // `r.xs.push(x)` becomes `r.xs = @push(r.xs, x)`.
        let p =
            parse_src("fn main() -> Int64 { let mut r: R = R { xs: [] }  r.xs.push(1)  return 0 }");
        let stmts = &p.functions[0].body.stmts;
        match &stmts[1] {
            Stmt::Store {
                name,
                leaf: Step::Field(field),
                value: Expr::Call { name: c, .. },
                ..
            } => {
                assert_eq!(name, "r", "writes back through the record binding");
                assert_eq!(field, "xs");
                assert_eq!(c, "@push");
            }
            other => panic!("expected `r.xs = @push(r.xs, 1)`, got {other:?}"),
        }
    }

    #[test]
    fn push_on_array_element_desugars_to_an_element_store() {
        // `a[i].push(x)` becomes `a[i] = @push(a[i], x)`.
        let p = parse_src(
            "fn main() -> Int64 { let mut a: Array<Int64> = []  a[0].push(1)  return 0 }",
        );
        let stmts = &p.functions[0].body.stmts;
        match &stmts[1] {
            Stmt::Store {
                name,
                leaf: Step::Index(_),
                value: Expr::Call { name: c, .. },
                ..
            } => {
                assert_eq!(name, "a", "writes back into the array slot");
                assert_eq!(c, "@push");
            }
            other => panic!("expected `a[0] = @push(a[0], 1)`, got {other:?}"),
        }
    }

    #[test]
    fn push_on_a_deeper_field_chain_is_rejected() {
        // `r.a.b.push(x)` is a parse error, never a silent copy.
        let src =
            "fn main() -> Int64 { let mut r: R = R { a: A { b: [] } }  r.a.b.push(1)  return 0 }";
        let e = parse(lex(src).unwrap()).unwrap_err();
        assert!(
            e.message.contains("no place to write back to"),
            "{}",
            e.message
        );
    }

    // Function values.

    fn names(ps: &[Binder]) -> Vec<&str> {
        ps.iter().map(|b| b.name.as_str()).collect()
    }

    fn only_arg(p: &Program) -> Expr {
        // The one call argument of `f(<arg>)` in `main`'s first statement.
        match &p.functions.last().unwrap().body.stmts[0] {
            Stmt::Let {
                value: Expr::Call { args, .. },
                ..
            } => args[0].clone(),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_fn_type_in_parameter() {
        let p = parse_src("fn f(g: fn(Int64, Bool) -> Int64) -> Int64 { return 0 }");
        assert_eq!(
            p.functions[0].params[0].ty,
            Type::Fn(vec![Type::Int, Type::Bool], Box::new(Type::Int))
        );
        // `fn()` with no arrow returns Unit.
        let q = parse_src("fn f(g: fn()) -> Int64 { return 0 }");
        assert_eq!(
            q.functions[0].params[0].ty,
            Type::Fn(vec![], Box::new(Type::Unit))
        );
    }

    #[test]
    fn parses_expression_lambda() {
        let p = parse_src(
            "fn f(g: fn(Int64) -> Int64) -> Int64 { return 0 }\n\
                           fn main() -> Int64 { let a = f(x -> x * 2)  return 0 }",
        );
        match only_arg(&p) {
            Expr::Lambda { params, body, .. } => {
                assert_eq!(names(&params), vec!["x"]);
                assert!(matches!(body, LambdaBody::Expr(_)));
            }
            other => panic!("expected lambda, got {other:?}"),
        }
    }

    #[test]
    fn parses_block_and_multiparam_and_niladic_lambda() {
        let p = parse_src(
            "fn f(g: fn(Int64, Int64) -> Int64) -> Int64 { return 0 }\n\
                           fn main() -> Int64 { let a = f((x, y) -> { return x + y })  return 0 }",
        );
        match only_arg(&p) {
            Expr::Lambda { params, body, .. } => {
                assert_eq!(names(&params), vec!["x", "y"]);
                assert!(matches!(body, LambdaBody::Block(_)));
            }
            other => panic!("expected lambda, got {other:?}"),
        }
        let q = parse_src(
            "fn f(g: fn() -> Int64) -> Int64 { return 0 }\n\
                           fn main() -> Int64 { let a = f(() -> 7)  return 0 }",
        );
        match only_arg(&q) {
            Expr::Lambda { params, .. } => assert!(params.is_empty()),
            other => panic!("expected niladic lambda, got {other:?}"),
        }
    }

    #[test]
    fn lambda_body_precedence_spans_or() {
        // In `x -> a || b` the body is the whole `a || b`.
        let p = parse_src(
            "fn f(g: fn(Bool) -> Bool) -> Int64 { return 0 }\n\
                           fn main() -> Int64 { let a = f(x -> x || false)  return 0 }",
        );
        match only_arg(&p) {
            Expr::Lambda {
                body: LambdaBody::Expr(e),
                ..
            } => {
                assert!(matches!(*e, Expr::Binary { op: BinOp::Or, .. }));
            }
            other => panic!("expected lambda with Or body, got {other:?}"),
        }
    }

    // `if` as an expression.

    #[test]
    fn if_in_expression_position_parses_to_if_expr() {
        // `let x = if ...` produces `IfExpr` with an `else`; a statement `if` stays
        // `Stmt::If`.
        let p = parse_src(
            "fn main() -> Int64 {\n\
                           let x = if true { 1 } else { 2 }\n\
                           if true { print(x) }\n\
                           return x }",
        );
        let f = &p.functions[0];
        match &f.body.stmts[0] {
            Stmt::Let {
                value:
                    Expr::IfExpr {
                        else_branch: Some(_),
                        ..
                    },
                ..
            } => {}
            other => panic!("expected IfExpr let-init, got {other:?}"),
        }
        assert!(
            matches!(f.body.stmts[1], Stmt::If { .. }),
            "bare `if` stays a statement"
        );
    }

    #[test]
    fn else_if_chain_nests_as_if_expr() {
        let p = parse_src(
            "fn main() -> Int64 {\n\
                           let x = if false { 1 } else if false { 2 } else { 3 }\n\
                           return x }",
        );
        let f = &p.functions[0];
        let Stmt::Let {
            value:
                Expr::IfExpr {
                    else_branch: Some(eb),
                    ..
                },
            ..
        } = &f.body.stmts[0]
        else {
            panic!("expected chained IfExpr");
        };
        assert!(
            matches!(**eb, Expr::IfExpr { .. }),
            "`else if` nests as an IfExpr"
        );
    }

    // Module contracts.

    #[test]
    fn contract_parses_value_fn_and_open_rule_members() {
        let src = "export contract Page {
                   /// The page's data.
                   let data: Array<T>
                   /// Head contributions.
                   let head: String = \"\"
                   /// The loader.
                   fn load(id: Int64) -> String
                   fn *(input: String) -> String
                   }";
        let p = parse_src(src);
        let c = &p.contracts[0];
        assert_eq!(c.name, "Page");
        assert!(c.exported);
        assert_eq!(c.members.len(), 4);

        // A member type parameter is open per member: `T` is a Param, not a named
        // type.
        assert!(matches!(
            &c.members[0].kind,
            ContractMemberKind::Value {
                ty: Type::Array(inner),
                default: None,
            } if **inner == Type::Param("T".to_string())
        ));
        assert_eq!(c.members[0].type_params(), vec!["T".to_string()]);
        assert!(!c.members[0].optional());

        assert!(c.members[1].optional());
        assert_eq!(c.members[1].spelling(), "String");

        assert_eq!(c.members[2].spelling(), "fn(Int64) -> String");
        assert!(!c.members[2].optional());

        assert!(c.members[3].is_open_rule());
        assert!(c.open_rule().is_some());
    }

    #[test]
    fn contract_member_docs_are_retained() {
        // The LSP shows member docs on completion and hover, so they survive parsing.
        let src = "contract P {
                   /// What this page's data is.
                   let data: String
                   }";
        let p = parse_src(src);
        assert_eq!(
            p.contracts[0].members[0].doc.as_deref(),
            Some("What this page's data is.")
        );
    }

    #[test]
    fn contract_is_a_contextual_keyword_not_a_reserved_word() {
        // `std/rpc`, `std/connect`, `std/openapi` and `std/graphql` take a parameter
        // named `contract`, so the word starts a declaration only as
        // `contract <Ident> {`.
        let src = "fn f(contract: String) -> String {
                   let contract2 = contract
                   return contract2
                   }";
        let p = parse_src(src);
        assert_eq!(p.functions[0].params[0].name, "contract");
        assert!(p.contracts.is_empty());
    }

    #[test]
    fn contract_without_an_open_rule_is_closed() {
        let p = parse_src("contract P { let a: String }");
        assert!(p.contracts[0].open_rule().is_none());
    }

    #[test]
    fn a_broken_contract_recovers_to_the_next_declaration() {
        // Recovery must see `contract` as a top-level starter, or a declaration after
        // a broken one is swallowed.
        let (p, errors) = parse_accum(
            lex("fn a() -> Int64 { return 0 }
contract P { let x }
contract Q { let y: String }
")
            .unwrap(),
        );
        assert!(!errors.is_empty());
        assert_eq!(p.contracts.len(), 1);
        assert_eq!(p.contracts[0].name, "Q");
    }

    #[test]
    fn member_type_parameters_are_single_uppercase_letters_only() {
        // The rule that keeps `Query<T>` open while keeping `Haed` a typo.
        assert!(is_member_type_param("T"));
        assert!(is_member_type_param("R"));
        assert!(is_member_type_param("T1"));
        assert!(!is_member_type_param("Head"));
        assert!(!is_member_type_param("Ta"));
        assert!(!is_member_type_param("t"));
        assert!(!is_member_type_param(""));
    }

    /// An associated type is substituted as the method parses, so `type Output`
    /// must come first; the misordered case gets its own diagnostic.
    #[test]
    fn an_associated_type_is_substituted_and_must_precede_the_methods() {
        let (p, errors) = parse_accum(
            lex("protocol Unwrap { type Output  fn get(self) -> Output }
impl Unwrap for Int64 { type Output = Int64  fn get(self) -> Output { return self } }
")
            .unwrap(),
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(p.protocols[0].assoc, vec!["Output".to_string()]);
        // The protocol keeps `Output` as a type variable; the impl has resolved it.
        assert_eq!(
            p.protocols[0].methods[0].ret,
            Type::Param("Output".to_string())
        );
        assert_eq!(p.impls[0].methods[0].ret, Type::Int);
        assert_eq!(p.impls[0].assoc, vec!["Output".to_string()]);

        let (_, late) = parse_accum(
            lex("protocol Unwrap { type Output  fn get(self) -> Output }
impl Unwrap for Int64 { fn get(self) -> Output { return self }  type Output = Int64 }
")
            .unwrap(),
        );
        assert!(
            late.iter()
                .any(|d| d.message.contains("must be declared before the methods")),
            "{late:?}"
        );
    }
    /// `ns.Age?(5)` is one fallible construction of the dotted name, not a `?` on a
    /// field followed by a stray `(5)`.
    #[test]
    fn namespaced_fallible_construction_parses_as_one_tryconstruct() {
        let p = parse_src("fn main() -> Int64 { return ns.Age?(5) }");
        assert_eq!(p.functions[0].body.stmts.len(), 1);
        match &p.functions[0].body.stmts[0] {
            Stmt::Return {
                value: Some(Expr::TryConstruct { name, args, .. }),
                ..
            } => {
                assert_eq!(name, "ns.Age");
                assert_eq!(args.len(), 1);
            }
            other => panic!("expected TryConstruct ns.Age, got {other:?}"),
        }
    }

    /// The dotted fold fires only when the path continues `?(`.
    #[test]
    fn dotted_paths_without_the_question_stay_postfix() {
        let p = parse_src("fn main() -> Int64 { return ns.f(1) }");
        match &p.functions[0].body.stmts[0] {
            Stmt::Return {
                value: Some(Expr::Call { name, args, .. }),
                ..
            } => {
                assert_eq!(name, "f");
                assert_eq!(args.len(), 2);
            }
            other => panic!("expected method-sugar call, got {other:?}"),
        }
        let p = parse_src("fn main() -> Int64 { return ns.f }");
        match &p.functions[0].body.stmts[0] {
            Stmt::Return {
                value: Some(Expr::Field { field, expr, .. }),
                ..
            } => {
                assert_eq!(field, "f");
                assert!(matches!(expr.as_ref(), Expr::Var { name, .. } if name == "ns"));
            }
            other => panic!("expected field read, got {other:?}"),
        }
    }

    // Refinements under `&`, postfix chains, associated-type alias leaks.

    /// Each postfix link counts toward the grammar's `nest` limit, so a long `a.b.b...b`
    /// chain is refused instead of aborting in a later walk.
    #[test]
    fn a_deep_postfix_chain_hits_the_nest_guard_not_the_stack() {
        let body = format!("let x = s{}.b\n return 0", ".b".repeat(1100));
        let src = format!("fn main() -> Int64 {{ {body} }}");
        let e = parse(lex(&src).unwrap()).unwrap_err();
        assert!(e.message.contains("nesting exceeds"), "{}", e.message);
    }

    /// `fn 9` breaks the protocol after `type Output` installed its alias. In the
    /// next declaration `Output` must be a named type again, not a stale
    /// [`Type::Param`].
    #[test]
    fn a_failed_protocol_does_not_leak_associated_types_into_the_next_decl() {
        let src = "protocol P { type Output  fn 9() -> Output; }\n\
                   type Q = Output";
        let (p, errs) = parse_accum(lex(src).unwrap());
        assert!(!errs.is_empty(), "the broken protocol is still an error");
        let q = p.type_decls.iter().find(|d| d.name == "Q").unwrap();
        assert!(
            matches!(&q.base, Type::Named(n) if n.as_str() == "Output"),
            "got {:?}",
            q.base
        );
    }

    /// The same in an `impl`: `-> 9` fails after `type Out` installed its alias.
    #[test]
    fn a_failed_impl_does_not_leak_associated_types_into_the_next_decl() {
        let src = "type R = { x: Int64 }\n\
                   impl P for R { type Out = Int64  fn bad(self) -> 9 }\n\
                   type S = Out";
        let (p, errs) = parse_accum(lex(src).unwrap());
        assert!(!errs.is_empty(), "the broken impl is still an error");
        let s = p.type_decls.iter().find(|d| d.name == "S").unwrap();
        assert!(
            matches!(&s.base, Type::Named(n) if n.as_str() == "Out"),
            "got {:?}",
            s.base
        );
    }
}
