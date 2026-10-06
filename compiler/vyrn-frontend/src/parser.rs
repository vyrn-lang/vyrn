//! Recursive-descent parser with precedence climbing for expressions.

use crate::ast::*;
use crate::diagnostics::Diagnostic;
use crate::lexer::{Hole, Tok, Token};
use crate::rules::{refuse, rule, Rule};
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

/// Whether the cursor sits on a `contract Name {` declaration starter.
///
/// `contract` is contextual: `std/rpc`, `std/connect`, `std/openapi` and
/// `std/graphql` take a parameter named `contract`.
fn at_contract_decl(tokens: &[Token], pos: usize) -> bool {
    let at = |i: usize| tokens.get(i).map(|t| &t.tok);
    matches!(at(pos), Some(Tok::Ident(n)) if n == "contract")
        && matches!(at(pos + 1), Some(Tok::Ident(_)))
        && matches!(at(pos + 2), Some(Tok::LBrace))
}

/// The method-form builtin spellings ([`crate::prelude::Builtin::method`]) that
/// the module answers to itself: its top-level declarations, its imports (under
/// the local spelling), and the methods of its protocols. A written
/// `recv.m(..)` with `m` in this set calls the module's `m`, not the builtin.
///
/// The test is scope, not type: the flat namespace is program-wide unique, so
/// one lookup answers it. A module that declares `remove` and calls
/// `.remove(k)` on a `Map` gets a type error at the call.
fn answered_methods(program: &Program) -> HashSet<String> {
    let protocols = program.protocols.iter();
    let imports = program.imports.iter().flat_map(|i| &i.names);
    // Impl methods are absent: `impl Copy for T { fn copy(..) }` overrides `@copy`
    // for receivers of type `T` only. Counting it here would take the builtin from
    // every other receiver in the module.
    (program.functions.iter().map(|f| &f.name))
        .chain(program.type_decls.iter().map(|t| &t.name))
        .chain(program.globals.iter().map(|g| &g.name))
        .chain(protocols.flat_map(|p| p.methods.iter().map(|m| &m.name).chain([&p.name])))
        .chain(imports.map(|n| n.alias.as_ref().unwrap_or(&n.original)))
        .filter(|n| crate::prelude::method_builtin(n).is_some())
        .cloned()
        .collect()
}

/// Parses the grammar alone: no prelude, no module-declared method names, no
/// `impl` flattening. [`crate::prelude::type_decls`] parses `prelude.vyrn` through
/// this, because the prelude is what [`parse_accum`] adds.
pub(crate) fn parse_bare(tokens: Vec<Token>) -> (Program, Vec<Diagnostic>) {
    Parser::over(tokens).program_accum()
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
    // A declaration can follow its use, so the names are known only after one
    // parse. A module that answers to a method-form name parses again, and each
    // `recv.m(..)` and the statement desugar around it are decided once. The
    // copy is kept because [`Parser::eat`] splits `>>` and `>=` in place.
    let (mut program, mut errors) = Parser::over(tokens.clone()).program_accum();
    let answers = answered_methods(&program);
    if !answers.is_empty() {
        (program, errors) = Parser {
            answers,
            ..Parser::over(tokens)
        }
        .program_accum();
    }
    // The prelude: the declarations every program gets, stated in `prelude.vyrn`.
    program
        .type_decls
        .extend(crate::prelude::type_decls().iter().cloned());
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
    let mut seen: std::collections::HashSet<String> = Default::default();
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
    (program, errors)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// Whether this parser reads an interpolation hole rather than the file.
    ///
    /// A hole is re-lexed as its own source, so its tokens count lines and columns
    /// from the hole; [`Parser::parse_hole`] anchors every diagnostic at the
    /// template, and a binder parsed here carries no position
    /// ([`crate::ast::Binder`]).
    in_hole: bool,
    /// When true, a bare `Ident {` is not a struct literal, so `if x { .. }` parses
    /// `x` as the condition. Reset inside `( .. )`.
    no_struct: bool,
    /// The current function's generic parameters; a matching type name parses as
    /// [`Type::Param`].
    type_params: Vec<String>,
    /// Associated types in scope: `Output` in a `protocol` body maps to
    /// `Type::Param("Output")`, and in an `impl` body to what its `type Output = ..`
    /// bound. Resolving here means an impl method leaves the parser as an ordinary
    /// generic function. The cost: a `type` member must precede the methods that
    /// name it ([`Parser::impl_block`]).
    type_aliases: std::collections::HashMap<String, Type>,
    /// Inline `field: T where pred` refinements collected in a `type` declaration's
    /// record, drained by `type_decl` into synthetic validated types named
    /// `Decl.field`. `None` outside a type declaration, where inline `where` is a
    /// parse error: an anonymous record has no name for the refinement.
    field_preds: Option<Vec<(String, Expr)>>,
    /// Extra statements from a one-statement desugar (`a[i].f = v` becomes three).
    /// `stmt` returns the first and stashes the rest here; `block` drains them
    /// right after. Empty outside one `stmt` call.
    extra_stmts: Vec<Stmt>,
    /// Diagnostics from statement recovery inside a body: [`Parser::block`]
    /// records the error and skips to the next statement. [`Parser::program_accum`]
    /// merges them into the program's errors in source order.
    errors: Vec<Diagnostic>,
    /// How many levels of nested source the parser is inside ([`Parser::MAX_NEST`]).
    /// A stack overflow aborts with no `file:line`, so depth is counted and refused.
    /// Bumped on the three unbounded recursive edges: [`Parser::unary`] (every
    /// expression recursion enters it once), [`Parser::type_`] and [`Parser::block`].
    depth: u32,
    /// See [`answered_methods`]; empty on the first parse.
    answers: HashSet<String>,
}

/// Returns how a binary operator is written, for a diagnostic that quotes one.
///
/// Derived from [`Parser::binop`] and `lexer::punct_text`, so the spelling is
/// stated once. The search over 36 spellings runs only while formatting a
/// message.
pub fn binop_text(op: BinOp) -> &'static str {
    crate::lexer::PUNCT_SPELLINGS
        .iter()
        .find(|s| {
            crate::lexer::punct_tok(s)
                .and_then(|t| Parser::binop(&t))
                .map(|(o, _)| o)
                == Some(op)
        })
        .copied()
        .unwrap_or_else(|| unreachable!("every `BinOp` is spelled by one token"))
}

/// Wraps a skeleton as a function body, for the statement-list mode of a code
/// quote. [`Parser::parses_as_stmts`] and
/// [`Parser::skeleton_error_detail`] share it: the detail's line is the wrapped
/// line minus the wrapper's.
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

fn as_fn_body(src: &str) -> String {
    format!("fn __vyrn_probe__() {{\n{src}\n}}")
}

/// Whether `e` is a field chain rooted at `a[i]` (`a[i].f`, `a[i].f.g`), to tell
/// a too-deep element write (`a[i].f.g = v`, refused) from a nested record-field
/// write (`a.b.c = v`).
fn is_index_field_chain(e: &Expr) -> bool {
    match e {
        Expr::Call { name, args, .. } => name == "@at" && args.len() == 2,
        Expr::Field { expr, .. } => is_index_field_chain(expr),
        _ => false,
    }
}

/// Returns the plain-variable receiver an in-place mutation writes through, for
/// a base that may be a record field or an array element.
///
/// The backends load and store a container's header only in a local binding. A
/// base like `r.a` or `rows[0]` moves out into an unspellable temp, is mutated
/// there, and moves back: O(1) per write, not a copy.
///
/// Returns the receiver and three statement lists. The group runs `hoists`,
/// then `moves`, then the mutation, then `post`; `moves` and `post` nest
/// outermost-first and outermost-last, so `r.inner.a[i] = v` works. Nothing may
/// read the place while it is out, so callers put their operand evaluations in
/// `hoists`, left to right: in `rows[f()][g()] = h()` each call runs once, in
/// source order. A base that is neither (a call result, a temporary) yields
/// `None`.
pub fn place_receiver(
    base: &Expr,
    line: usize,
) -> Option<(String, Vec<Stmt>, Vec<Stmt>, Vec<Stmt>)> {
    match base {
        // Already a slot: nothing to move.
        Expr::Var { name, .. } => Some((name.clone(), Vec::new(), Vec::new(), Vec::new())),
        Expr::Field { expr, field, .. } => {
            let (parent, hoists, mut pre, mut post) = place_receiver(expr, line)?;
            // Unspellable (contains `[`): it cannot collide with an identifier, the
            // symbol index filters it out, and it reads naturally in a diagnostic.
            let tmp = format!("{parent}.{field}[]");
            pre.push(Stmt::Let {
                id: Id::NEW,
                name: tmp.clone(),
                mutable: true,
                ty: None,
                value: Expr::field(Expr::var(parent.clone(), line), field.clone(), line),
                line,
                col: 0,
            });
            post.insert(
                0,
                Stmt::SetField {
                    id: Id::NEW,
                    name: parent,
                    field: field.clone(),
                    value: Expr::var(tmp.clone(), line),
                    line,
                },
            );
            Some((tmp, hoists, pre, post))
        }
        // `rows[i][j] = v`: an element that is itself a container. The index is
        // hoisted because the load and the write-back both need it, evaluated once.
        Expr::Call { name, args, .. } if name == "@at" && args.len() == 2 => {
            let (parent, mut hoists, mut pre, mut post) = place_receiver(&args[0], line)?;
            let tmp = format!("{parent}[]");
            let idx = format!("{tmp}idx");
            hoists.push(Stmt::let_(idx.clone(), args[1].clone(), line));
            let index = Expr::var(idx, line);
            let load = Expr::call(
                "@at",
                vec![Expr::var(parent.clone(), line), index.clone()],
                line,
            );
            pre.push(Stmt::Let {
                id: Id::NEW,
                name: tmp.clone(),
                mutable: true,
                ty: None,
                value: load,
                line,
                col: 0,
            });
            post.insert(
                0,
                Stmt::IndexSet {
                    id: Id::NEW,
                    name: parent,
                    index,
                    value: Expr::var(tmp.clone(), line),
                    line,
                },
            );
            Some((tmp, hoists, pre, post))
        }
        _ => None,
    }
}

/// Whether evaluating `e` can reach a place: a record field or an array element.
///
/// A place desugar takes its container out, so an operand that could read one
/// runs before the move (`u.xs[0] = u.xs[2] + 5`). Literals and variables stay
/// in place: `t.rows[k] = []` hoisted would give an empty literal no type. A
/// call is assumed to reach a place, since it can read a module-level record.
fn reads_place(e: &Expr) -> bool {
    match e {
        Expr::Int(_, _)
        | Expr::Byte(_, _)
        | Expr::Float(_, _)
        | Expr::Bool(_, _)
        | Expr::Str(_, _)
        | Expr::Var { .. } => false,
        Expr::Unary { expr, .. } => reads_place(expr),
        Expr::Binary { lhs, rhs, .. } => reads_place(lhs) || reads_place(rhs),
        Expr::ArrayLit { elems, .. } => elems.iter().any(reads_place),
        Expr::MapLit { entries, .. } => entries
            .iter()
            .any(|(k, v)| reads_place(k) || reads_place(v)),
        _ => true,
    }
}

/// Binds `e` to a temp evaluated before the move-out and returns the expression
/// the mutation uses. An operand that cannot reach a place is returned as is.
pub fn hoist_operand(e: Expr, name: String, hoists: &mut Vec<Stmt>, line: usize) -> Expr {
    if !reads_place(&e) {
        return e;
    }
    hoists.push(Stmt::let_(name.clone(), e, line));
    Expr::var(name, line)
}

/// Rewrites the receiver of a mutating method (`r.a.pop()`,
/// `rows[i].swapRemove(j)`) to a moved-out temp, because the backends store the
/// shrunk header only into a plain variable. Returns the statements that
/// bracket it.
///
/// These are expressions, and the move-back is sound only where nothing
/// observes the container before it. So callers apply this to a whole statement
/// (`r.a.pop()` or `let x = r.a.pop()`) only: in `if r.a.pop() == None { .. r.a
/// .. }` the body would read the field before the move-back.
fn hoist_mutating_receiver(e: &mut Expr, line: usize) -> Option<(Vec<Stmt>, Vec<Stmt>)> {
    let Expr::Call { name, args, .. } = e else {
        return None;
    };
    if !crate::prelude::removes(name) {
        return None;
    }
    let (recv, mut hoists, pre, post) = place_receiver(args.first()?, line)?;
    if pre.is_empty() {
        return None;
    }
    // `r.a.swapRemove(r.a.length - 1)` reads the field the move-out empties, so
    // the other arguments run first.
    for (n, arg) in args.iter_mut().enumerate().skip(1) {
        let tmp = format!("{recv}[]arg{n}");
        let taken = std::mem::replace(arg, Expr::int(0));
        *arg = hoist_operand(taken, tmp, &mut hoists, line);
    }
    args[0] = Expr::var(recv, line);
    hoists.extend(pre);
    Some((hoists, post))
}

/// Returns the statements a store through a non-slot place becomes.
///
/// Two callers: [`Parser::stmt`] for `a[i] = v` (parsed as `@at(a, i)`), and
/// `project.rs` for a store through a projection (`@slot`). Both use
/// [`place_receiver`]'s desugar: move the container into a temp, store, move it
/// back. No backend needs an address-of. The move-out copies a growable
/// container's header and a whole value held inline.
///
/// `None` means no store reaches the target (a call result, a literal, a
/// temporary); the caller keeps its own refusal.
pub fn store_stmts(place: &Expr, value: &Expr, line: usize) -> Option<Vec<Stmt>> {
    match place {
        Expr::Var { name, .. } => Some(vec![Stmt::Assign {
            id: Id::NEW,
            name: name.clone(),
            value: value.clone(),
            line,
        }]),
        Expr::Field { expr, field, .. } => {
            let (recv, mut out, moves, post) = place_receiver(expr, line)?;
            let value = if moves.is_empty() {
                value.clone()
            } else {
                hoist_operand(value.clone(), format!("{recv}#val"), &mut out, line)
            };
            out.extend(moves);
            out.push(Stmt::SetField {
                id: Id::NEW,
                name: recv,
                field: field.clone(),
                value,
                line,
            });
            out.extend(post);
            Some(out)
        }
        // An element of a place: `return self.data[j]`, or the seeded row's
        // `yield @slot(self, i)`.
        Expr::Call { name, args, .. }
            if (name == crate::project::AT || name == crate::project::ELEM) && args.len() == 2 =>
        {
            let (recv, mut out, moves, post) = place_receiver(&args[0], line)?;
            // With a move-out, the index and the value run first, in source order.
            let (index, value) = if moves.is_empty() {
                (args[1].clone(), value.clone())
            } else {
                // `#`, not `[]`: a name spelled `{recv}[]idx` reads as derived from the
                // `{recv}[]` temp under `mentions_place`, which vetoed the store's displaced-
                // element row and left the overwritten element with no owner (std/slots).
                let i = hoist_operand(args[1].clone(), format!("{recv}#idx"), &mut out, line);
                let v = hoist_operand(value.clone(), format!("{recv}#val"), &mut out, line);
                (i, v)
            };
            out.extend(moves);
            out.push(Stmt::IndexSet {
                id: Id::NEW,
                name: recv,
                index,
                value,
                line,
            });
            out.extend(post);
            Some(out)
        }
        _ => None,
    }
}

impl Parser {
    /// A parser over `tokens` with nothing in scope. The only place the fields are
    /// listed.
    fn over(tokens: Vec<Token>) -> Parser {
        Parser {
            tokens,
            pos: 0,
            in_hole: false,
            no_struct: false,
            type_params: Vec::new(),
            type_aliases: Default::default(),
            field_preds: None,
            extra_stmts: Vec::new(),
            errors: Vec::new(),
            depth: 0,
            answers: HashSet::new(),
        }
    }

    /// A sub-parser over `tokens` that inherits the enclosing generic parameters
    /// and type aliases, so a re-lexed hole or quote skeleton that names `T`
    /// parses. The desugar queue, errors and depth stay the sub-parse's own.
    fn sub(&self, tokens: Vec<Token>) -> Parser {
        Parser {
            type_params: self.type_params.clone(),
            type_aliases: self.type_aliases.clone(),
            answers: self.answers.clone(),
            in_hole: true,
            ..Parser::over(tokens)
        }
    }

    // Token cursor.

    fn peek(&self) -> &Tok {
        &self.tokens[self.pos].tok
    }

    /// The token `n` places along; the `Eof` terminator past the end.
    fn tok_at(&self, n: usize) -> &Tok {
        &self.tokens[n.min(self.tokens.len() - 1)].tok
    }

    fn line(&self) -> usize {
        self.tokens[self.pos].line
    }

    /// Consumes an optional `;`. Without one, a statement ends where the greedy
    /// expression grammar cannot extend.
    fn eat_semi(&mut self) {
        if *self.peek() == Tok::Semi {
            self.advance();
        }
    }

    /// Consumes consecutive `///` tokens and returns them joined by newlines, or
    /// `None`. Callers inside bodies discard the result.
    fn take_docs(&mut self) -> Option<String> {
        let mut lines: Vec<String> = Vec::new();
        let mut last_line = 0usize;
        loop {
            let line = self.tokens[self.pos].line;
            match self.peek() {
                Tok::Doc(t) => {
                    // A blank line detaches an earlier block (a file header, say): discard it.
                    if !lines.is_empty() && line > last_line + 1 {
                        lines.clear();
                    }
                    lines.push(t.clone());
                    last_line = line;
                    self.advance();
                }
                _ => {
                    // The surviving block must sit directly above the declaration.
                    if !lines.is_empty() && line > last_line + 1 {
                        lines.clear();
                    }
                    break;
                }
            }
        }
        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }

    fn col(&self) -> usize {
        self.tokens[self.pos].col
    }

    fn advance(&mut self) -> Tok {
        let t = self.tokens[self.pos].tok.clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, expected: &Tok) -> Result<(), Diagnostic> {
        if self.peek() == expected {
            self.advance();
            Ok(())
        } else if *expected == Tok::Gt && *self.peek() == Tok::GtEq {
            // `Array<Int>= []`: the lexer max-munches `>=`; closing a generic argument
            // list takes the `>` and leaves an `=`.
            self.tokens[self.pos].tok = Tok::Eq;
            self.tokens[self.pos].col += 1;
            Ok(())
        } else if *expected == Tok::Gt && *self.peek() == Tok::Shr {
            // `Array<Array<T>>`: the lexer max-munches `>>`. Closing a generic list takes
            // one `>` and leaves one for the enclosing list, so a shift survives only
            // where the expression parser consumes it.
            self.tokens[self.pos].tok = Tok::Gt;
            self.tokens[self.pos].col += 1;
            Ok(())
        } else {
            Err(refuse!(
                "parse",
                self.line(),
                self.col(),
                Expected,
                pty = format!("{:?}", expected),
                aty = format!("{:?}", self.peek())
            ))
        }
    }

    /// The root name of an assignment target: an identifier, or `self`.
    ///
    /// `primary` already parses `self` as a variable; only `self = ..` and
    /// `self.n = ..`, which read the root off the token, need this.
    fn place_root(&mut self) -> Result<String, Diagnostic> {
        if *self.peek() == Tok::Vself {
            self.advance();
            return Ok("self".to_string());
        }
        self.expect_ident()
    }

    /// Where the token under the cursor is spelled in the file, or `(0, 0)` inside
    /// a hole ([`Parser::in_hole`]). Every binder's position comes from here.
    fn binder_pos(&self) -> (usize, usize) {
        if self.in_hole {
            (0, 0)
        } else {
            (self.line(), self.col())
        }
    }

    fn expect_binder(&mut self) -> Result<Binder, Diagnostic> {
        let (line, col) = self.binder_pos();
        let name = self.expect_ident()?;
        Ok(Binder {
            id: Id::NEW,
            name,
            line,
            col,
        })
    }

    fn expect_ident(&mut self) -> Result<String, Diagnostic> {
        match self.advance() {
            Tok::Ident(name) => Ok(name),
            other => Err(refuse!(
                "parse",
                self.line(),
                self.col(),
                ExpectedIdent,
                found = format!("{:?}", other)
            )),
        }
    }

    // Grammar.

    /// Parses every top-level declaration with recovery: a failed declaration
    /// records its diagnostic and the cursor skips to the next starter.
    fn program_accum(&mut self) -> (Program, Vec<Diagnostic>) {
        let mut imports = Vec::new();
        let mut type_decls = Vec::new();
        let mut functions = Vec::new();
        let mut protocols = Vec::new();
        let mut contracts = Vec::new();
        let mut impls = Vec::new();
        let mut globals = Vec::new();
        let mut tests = Vec::new();
        let mut benches = Vec::new();
        let mut log_level = DEFAULT_LOG_LEVEL;
        let mut log_sink = LogSink::Stderr;
        let mut saw_logging = false;
        let mut errors = Vec::new();
        while *self.peek() != Tok::Eof {
            // A failed `fn f<T>` clears its generic params only on success; reset them so
            // a `T` cannot parse as `Type::Param` in the next declaration.
            self.type_params.clear();
            let doc = self.take_docs();
            if *self.peek() == Tok::Eof {
                break;
            }
            // `export` marks the following declaration importable.
            let exported = if *self.peek() == Tok::Export {
                self.advance();
                // `export extern fn`: `extern` is contextual, a starter only before `fn`.
                let is_export_extern = matches!(self.peek(), Tok::Ident(n) if n == "extern")
                    && matches!(self.tokens[self.pos + 1].tok, Tok::Fn);
                // `export gen fn`: `gen` is contextual too.
                let is_export_gen = matches!(self.peek(), Tok::Ident(n) if n == "gen")
                    && matches!(self.tokens[self.pos + 1].tok, Tok::Fn);
                let is_export_contract = at_contract_decl(&self.tokens, self.pos);
                // `export mut fn`: `export` is outermost, as in `export gen fn`.
                let is_export_mut =
                    *self.peek() == Tok::Mut && matches!(self.tokens[self.pos + 1].tok, Tok::Fn);
                if !matches!(self.peek(), Tok::Fn | Tok::Type | Tok::Protocol)
                    && !is_export_extern
                    && !is_export_gen
                    && !is_export_contract
                    && !is_export_mut
                {
                    // Module state is legal in any module but never exported.
                    let rule = if *self.peek() == Tok::Let {
                        Rule::ModuleStateExport {}
                    } else {
                        Rule::ExportNeedsDecl {}
                    };
                    errors.push(Diagnostic::refusal(self.line(), self.col(), "parse", rule));
                    self.sync_to_decl();
                    continue;
                }
                true
            } else {
                false
            };
            match self.peek() {
                Tok::Import => match self.import_decl() {
                    Ok(i) => imports.push(i),
                    Err(d) => {
                        errors.push(d);
                        self.sync_to_decl();
                    }
                },
                Tok::Type => match self.type_decl() {
                    Ok(mut ds) => {
                        ds[0].doc = doc;
                        ds[0].exported = exported;
                        type_decls.extend(ds);
                    }
                    Err(d) => {
                        errors.push(d);
                        self.sync_to_decl();
                    }
                },
                Tok::Fn => match self.function(false) {
                    Ok(mut f) => {
                        f.doc = doc;
                        f.exported = exported;
                        functions.push(f);
                    }
                    Err(d) => {
                        errors.push(d);
                        self.sync_to_decl();
                    }
                },
                // `gen fn`: a compile-time module generator. `gen` is contextual,
                // a starter only before `fn`.
                Tok::Ident(name)
                    if name == "gen" && matches!(self.tokens[self.pos + 1].tok, Tok::Fn) =>
                {
                    self.advance();
                    match self.function(true) {
                        Ok(mut f) => {
                            f.doc = doc;
                            f.exported = exported;
                            functions.push(f);
                        }
                        Err(d) => {
                            errors.push(d);
                            self.sync_to_decl();
                        }
                    }
                }
                // `mut fn`: a procedure that changes state. `mut` is already a
                // keyword, so this reserves nothing.
                Tok::Mut if matches!(self.tokens[self.pos + 1].tok, Tok::Fn) => {
                    self.advance();
                    match self.function(false) {
                        Ok(mut f) => {
                            f.doc = doc;
                            f.exported = exported;
                            f.is_mut = true;
                            functions.push(f);
                        }
                        Err(d) => {
                            errors.push(d);
                            self.sync_to_decl();
                        }
                    }
                }
                Tok::Protocol => match self.protocol_decl() {
                    Ok(mut p) => {
                        p.doc = doc;
                        p.exported = exported;
                        protocols.push(p);
                    }
                    Err(d) => {
                        errors.push(d);
                        self.sync_to_decl();
                    }
                },
                // `contract Name { .. }`, contextual: only `contract <Ident> {`
                // starts one.
                Tok::Ident(_) if at_contract_decl(&self.tokens, self.pos) => {
                    match self.contract_decl() {
                        Ok(mut c) => {
                            c.doc = doc;
                            c.exported = exported;
                            contracts.push(c);
                        }
                        Err(d) => {
                            errors.push(d);
                            self.sync_to_decl();
                        }
                    }
                }
                Tok::Impl => match self.impl_block() {
                    Ok(i) => impls.push(i),
                    Err(d) => {
                        errors.push(d);
                        self.sync_to_decl();
                    }
                },
                // Top-level `let [mut] name [: Type] = init`: module state.
                Tok::Let => match self.global_decl() {
                    Ok(mut g) => {
                        g.doc = doc;
                        globals.push(g);
                    }
                    Err(d) => {
                        errors.push(d);
                        self.sync_to_decl();
                    }
                },
                // `extern fn`: a JS import, contextual like `gen`.
                Tok::Ident(name)
                    if name == "extern" && matches!(self.tokens[self.pos + 1].tok, Tok::Fn) =>
                {
                    // Without `export`, `extern fn` is a body-less JS import; `export extern fn`
                    // with a body is a Vyrn function exported to JS.
                    match self.extern_function(exported) {
                        Ok(mut f) => {
                            f.doc = doc;
                            functions.push(f);
                        }
                        Err(d) => {
                            errors.push(d);
                            self.sync_to_decl();
                        }
                    }
                }
                // `test "name" { body }`: `test` is a starter only before a string
                // literal.
                Tok::Ident(name)
                    if name == "test" && matches!(self.tokens[self.pos + 1].tok, Tok::Str(_)) =>
                {
                    match self.named_block("test") {
                        Ok(mut t) => {
                            t.doc = doc;
                            tests.push(t);
                        }
                        Err(d) => {
                            errors.push(d);
                            self.sync_to_decl();
                        }
                    }
                }
                // `bench "name" { body }`, contextual like `test`.
                Tok::Ident(name)
                    if name == "bench" && matches!(self.tokens[self.pos + 1].tok, Tok::Str(_)) =>
                {
                    match self.named_block("bench") {
                        Ok(mut b) => {
                            b.doc = doc;
                            benches.push(b);
                        }
                        Err(d) => {
                            errors.push(d);
                            self.sync_to_decl();
                        }
                    }
                }
                Tok::Ident(name) if name == "logging" => {
                    let line = self.line();
                    let col = self.col();
                    if saw_logging {
                        errors.push(refuse!("parse", line, col, DuplicateLogging));
                        self.sync_to_decl();
                        continue;
                    }
                    saw_logging = true;
                    match self.logging_config() {
                        Ok((lvl, sink)) => {
                            log_level = lvl;
                            log_sink = sink;
                        }
                        Err(d) => {
                            errors.push(d);
                            self.sync_to_decl();
                        }
                    }
                }
                other => {
                    errors.push(refuse!(
                        "parse",
                        self.line(),
                        self.col(),
                        TopLevelExpected,
                        found = format!("{other:?}")
                    ));
                    self.advance(); // Consume the stray token so the loop makes progress.
                }
            }
        }
        // Merge the in-body recovery diagnostics and sort every error by position.
        let mut errors = errors;
        errors.append(&mut self.errors);
        errors.sort_by_key(|d| (d.line, d.col));
        (
            Program {
                imports,
                type_decls,
                functions,
                protocols,
                contracts,
                impls: impls.into(),
                globals,
                tests,
                benches,
                log_level,
                log_sink,
                // The loader fills this once every module is linked.
                surface_shadows: std::collections::HashSet::new(),
                host: Host::default(),
                module_hashes: std::collections::BTreeMap::new(),
                units: 0,
                expansions: Default::default(),
                spellings: Default::default(),
                session: Default::default(),
            },
            errors,
        )
    }

    /// Advances to a top-level declaration starter at brace depth 0, or `Eof`.
    /// Brace depth keeps a stray `fn` inside an unbalanced body from resuming
    /// mid-declaration.
    fn sync_to_decl(&mut self) {
        // Skip the failing token first: it belongs to the bad declaration, and for a
        // duplicate `logging` it is the unconsumed starter, so this ensures progress.
        self.advance();
        let mut depth = 0i32;
        while *self.peek() != Tok::Eof {
            match self.peek() {
                Tok::LBrace => {
                    depth += 1;
                    self.advance();
                }
                Tok::RBrace => {
                    if depth > 0 {
                        depth -= 1;
                    }
                    self.advance();
                }
                Tok::Fn
                | Tok::Type
                | Tok::Protocol
                | Tok::Impl
                | Tok::Import
                | Tok::Export
                | Tok::Let
                    if depth == 0 =>
                {
                    return
                }
                Tok::Ident(name) if depth == 0 && name == "logging" => return,
                Tok::Mut if depth == 0 && matches!(self.tokens[self.pos + 1].tok, Tok::Fn) => {
                    return
                }
                Tok::Ident(name)
                    if depth == 0
                        && (name == "extern" || name == "gen")
                        && matches!(self.tokens[self.pos + 1].tok, Tok::Fn) =>
                {
                    return
                }
                Tok::Ident(name)
                    if depth == 0
                        && (name == "test" || name == "bench")
                        && matches!(self.tokens[self.pos + 1].tok, Tok::Str(_)) =>
                {
                    return
                }
                Tok::Ident(_) if depth == 0 && at_contract_decl(&self.tokens, self.pos) => return,
                _ => {
                    self.advance();
                }
            }
        }
    }

    /// `protocol Name { type A  fn m(self, p: T, ..) -> R; .. }`: method
    /// signatures and associated types. The `self` receiver is required
    /// and dropped from the stored parameters. An associated type parses as
    /// [`Type::Param`] and must precede the signatures that name it.
    fn protocol_decl(&mut self) -> Result<ProtocolDecl, Diagnostic> {
        // Restore the associated-type aliases on every exit path: after a `?` failure
        // recovery moves on, and a stale alias would rewrite the next declaration's
        // type names (`program_accum` resets `type_params` but not these).
        let outer_aliases = std::mem::take(&mut self.type_aliases);
        let r = self.protocol_decl_inner();
        self.type_aliases = outer_aliases;
        r
    }

    fn protocol_decl_inner(&mut self) -> Result<ProtocolDecl, Diagnostic> {
        let line = self.line();
        self.eat(&Tok::Protocol)?;
        let name = self.expect_ident()?;
        self.eat(&Tok::LBrace)?;
        let mut methods = Vec::new();
        let mut assoc: Vec<String> = Vec::new();
        while *self.peek() != Tok::RBrace {
            let doc = self.take_docs();
            if *self.peek() == Tok::RBrace {
                break;
            }
            if *self.peek() == Tok::Type {
                let (tline, col) = (self.line(), self.col());
                self.advance();
                let aname = self.expect_ident()?;
                if *self.peek() == Tok::Eq {
                    return Err(refuse!("parse", tline, col, AssocTypeRhs, aname));
                }
                if !methods.is_empty() {
                    return Err(refuse!(
                        "parse",
                        tline,
                        col,
                        AssocTypeAfterMethods,
                        aname,
                        name
                    ));
                }
                assoc.push(aname.clone());
                self.type_aliases.insert(aname.clone(), Type::Param(aname));
                self.eat_semi();
                continue;
            }
            let mline = self.line();
            self.eat(&Tok::Fn)?;
            let mname = self.expect_ident()?;
            self.eat(&Tok::LParen)?;
            let recv = self.parse_self_capability();
            self.eat(&Tok::Vself)?;
            let mut params = Vec::new();
            let mut param_caps = Vec::new();
            while *self.peek() == Tok::Comma {
                self.advance();
                let _pname = self.expect_ident()?;
                self.eat(&Tok::Colon)?;
                param_caps.push(self.parse_capability());
                params.push(self.type_()?);
            }
            self.eat(&Tok::RParen)?;
            let mut result_cap = None;
            let ret = if *self.peek() == Tok::Arrow {
                self.advance();
                let (rline, rcol) = (self.line(), self.col());
                result_cap = self.parse_result_capability()?;
                // A protocol may declare a projection under the rule an impl member follows.
                if let Some(rc) = result_cap {
                    if recv != rc {
                        let want = rc.word();
                        return Err(refuse!(
                            "parse",
                            rline,
                            rcol,
                            ProjectionReceiver,
                            name = mname,
                            want
                        ));
                    }
                }
                self.type_()?
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
        self.eat(&Tok::RBrace)?;
        Ok(ProtocolDecl {
            exported: false,
            module: None,
            name,
            doc: None,
            assoc,
            methods,
            line,
        })
    }

    /// `contract Name { let m: T = d  fn m(a: T) -> R  fn *(a: T) -> R }`: a module
    /// contract, the exports a module may have.
    ///
    /// It differs from [`Self::protocol_decl`] in two ways: member `///` docs are
    /// kept for the LSP, and member type parameters are implicit, recognized by
    /// spelling ([`is_member_type_param`]).
    fn contract_decl(&mut self) -> Result<ContractDecl, Diagnostic> {
        let line = self.line();
        self.advance();
        let name = self.expect_ident()?;
        self.eat(&Tok::LBrace)?;
        let mut members: Vec<ContractMember> = Vec::new();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            let doc = self.take_docs();
            if *self.peek() == Tok::RBrace {
                break;
            }
            let mline = self.line();
            let member = match self.peek() {
                Tok::Let => self.contract_value_member(doc, mline)?,
                Tok::Fn => self.contract_fn_member(doc, mline)?,
                other => {
                    return Err(refuse!(
                        "parse",
                        self.line(),
                        self.col(),
                        ContractMemberExpected,
                        name,
                        found = format!("{other:?}")
                    ))
                }
            };
            // A name may repeat: the repeats are alternative signatures, and a module
            // matches any one (a page's `head` varies). Refused: a second open rule, and
            // one name as both a `let` and a `fn` member.
            if let Some(prev) = members.iter().find(|m| m.name == member.name) {
                if member.is_open_rule() {
                    return Err(refuse!(
                        "parse",
                        member.line,
                        self.col(),
                        ContractOpenRuleTwice,
                        name,
                        line = prev.line
                    ));
                }
                let same_form = matches!(
                    (&prev.kind, &member.kind),
                    (
                        ContractMemberKind::Value { .. },
                        ContractMemberKind::Value { .. }
                    ) | (ContractMemberKind::Fn { .. }, ContractMemberKind::Fn { .. })
                );
                if !same_form {
                    return Err(refuse!(
                        "parse",
                        member.line,
                        self.col(),
                        ContractMemberFormChange,
                        name,
                        member = member.name,
                        line = prev.line
                    ));
                }
                if matches!(member.kind, ContractMemberKind::Value { .. }) {
                    return Err(refuse!(
                        "parse",
                        member.line,
                        self.col(),
                        ContractMemberTwice,
                        name,
                        member = member.name,
                        line = prev.line
                    ));
                }
            }
            members.push(member);
        }
        self.eat(&Tok::RBrace)?;
        Ok(ContractDecl {
            name,
            exported: false,
            module: None,
            doc: None,
            members,
            line,
        })
    }

    /// `let name: Type [= default]` in a contract. A default makes the member
    /// optional.
    fn contract_value_member(
        &mut self,
        doc: Option<String>,
        line: usize,
    ) -> Result<ContractMember, Diagnostic> {
        self.eat(&Tok::Let)?;
        let name = self.expect_ident()?;
        if *self.peek() != Tok::Colon {
            return Err(refuse!(
                "parse",
                self.line(),
                self.col(),
                ContractMemberNeedsType,
                name
            ));
        }
        self.eat(&Tok::Colon)?;
        let ty = self.contract_member_type()?;
        let default = if *self.peek() == Tok::Eq {
            self.advance();
            Some(Box::new(self.expr()?))
        } else {
            None
        };
        self.eat_semi();
        Ok(ContractMember {
            name,
            doc,
            kind: ContractMemberKind::Value { ty, default },
            line,
        })
    }

    /// `fn name(a: T, ..) -> R [= default]`, or the open rule `fn *(a: T, ..) -> R`,
    /// in a contract. Parameter names are not kept: a contract constrains arity and
    /// types. The default is an expression of the return type and makes the member
    /// optional.
    fn contract_fn_member(
        &mut self,
        doc: Option<String>,
        line: usize,
    ) -> Result<ContractMember, Diagnostic> {
        self.eat(&Tok::Fn)?;
        let name = if *self.peek() == Tok::Star {
            self.advance();
            OPEN_RULE_NAME.to_string()
        } else {
            self.expect_ident()?
        };
        self.eat(&Tok::LParen)?;
        // `fn *(..) -> R` constrains the return type only. `..` is two `.` tokens.
        let variadic = *self.peek() == Tok::Dot
            && self.tokens.get(self.pos + 1).map(|t| &t.tok) == Some(&Tok::Dot);
        if variadic {
            if name != OPEN_RULE_NAME {
                return Err(refuse!(
                    "parse",
                    self.line(),
                    self.col(),
                    ContractMemberParams,
                    name
                ));
            }
            self.advance();
            self.advance();
        }
        let mut params = Vec::new();
        while !variadic && *self.peek() != Tok::RParen {
            let _pname = self.expect_ident()?;
            self.eat(&Tok::Colon)?;
            params.push(self.contract_member_type()?);
            if *self.peek() == Tok::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.eat(&Tok::RParen)?;
        let ret = if *self.peek() == Tok::Arrow {
            self.advance();
            self.contract_member_type()?
        } else {
            Type::Unit
        };
        let default = if *self.peek() == Tok::Eq {
            if name == OPEN_RULE_NAME {
                // The open rule has no name whose absence a default could stand in for.
                return Err(refuse!(
                    "parse",
                    self.line(),
                    self.col(),
                    ContractOpenRuleDefault
                ));
            }
            self.advance();
            Some(Box::new(self.expr()?))
        } else {
            None
        };
        self.eat_semi();
        Ok(ContractMember {
            name,
            doc,
            kind: ContractMemberKind::Fn {
                params,
                ret,
                default,
                variadic,
            },
            line,
        })
    }

    /// A type in a contract member, with its implicit type parameters rewritten to
    /// [`Type::Param`] by spelling ([`is_member_type_param`]).
    fn contract_member_type(&mut self) -> Result<Type, Diagnostic> {
        let mut ty = self.type_()?;
        mark_member_type_params(&mut ty);
        Ok(ty)
    }

    /// `impl P for T { fn m(self, ..) -> R { .. } .. }`: a type's methods for a
    /// protocol, each `self` typed to `T`.
    ///
    /// `impl<T> P for C<T>` binds type variables for the target and every method.
    /// `type Output = T` binds an associated type, substituted as the methods
    /// parse, so a method returning `Output` leaves here returning `T`.
    ///
    /// A `type` member must come before the methods that name it. Deferring would
    /// need a substitution pass over every body, where a missed `Type::Param`
    /// lowers to `void` silently; one ordering rule with a diagnostic is cheaper.
    fn impl_block(&mut self) -> Result<ImplBlock, Diagnostic> {
        // As in [`Parser::protocol_decl`], the aliases are restored around the whole
        // body, so no `?` exit leaves a stale alias behind.
        let outer_aliases = std::mem::take(&mut self.type_aliases);
        let r = self.impl_block_inner();
        self.type_aliases = outer_aliases;
        r
    }

    fn impl_block_inner(&mut self) -> Result<ImplBlock, Diagnostic> {
        let (line, col) = (self.line(), self.col());
        self.eat(&Tok::Impl)?;
        let (type_params, type_bounds) = self.type_param_binder()?;
        self.type_params = type_params.clone();
        let protocol = self.expect_ident()?;
        self.eat(&Tok::For)?;
        let ty = self.type_()?;
        self.eat(&Tok::LBrace)?;
        let mut methods = Vec::new();
        let mut places = Vec::new();
        let mut assoc: Vec<String> = Vec::new();
        while *self.peek() != Tok::RBrace {
            self.take_docs(); // Method-level docs are discarded.
            if *self.peek() == Tok::RBrace {
                break;
            }
            if *self.peek() == Tok::Type {
                let (tline, col) = (self.line(), self.col());
                self.advance();
                let aname = self.expect_ident()?;
                // Both bail-outs clear the impl's type parameters; [`Parser::impl_block`]
                // restores the aliases.
                if !methods.is_empty() {
                    self.type_params.clear();
                    return Err(refuse!(
                        "parse",
                        tline,
                        col,
                        ImplAssocTypeOrder,
                        aname,
                        protocol,
                        ty
                    ));
                }
                if type_params.contains(&aname) {
                    self.type_params.clear();
                    return Err(refuse!("parse", tline, col, ImplAssocTypeClash, aname));
                }
                self.eat(&Tok::Eq)?;
                let bound = self.type_()?;
                assoc.push(aname.clone());
                self.type_aliases.insert(aname, bound);
                self.eat_semi();
                continue;
            }
            let (mut m, place_cap) = self.impl_method(&ty)?;
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
        self.eat(&Tok::RBrace)?;
        self.type_params.clear();
        Ok(ImplBlock {
            protocol,
            type_params,
            type_bounds,
            ty,
            assoc,
            methods,
            places,
            line,
            col,
        })
    }

    /// Parses `read self` / `modify self` / `consume self`; a bare `self` is `read`.
    fn parse_self_capability(&mut self) -> Capability {
        if let Tok::Ident(id) = self.peek() {
            if let Some(c) = Capability::from_word(id) {
                if self.tokens[self.pos + 1].tok == Tok::Vself {
                    self.advance();
                    return c;
                }
            }
        }
        Capability::Read
    }

    /// Parses the contextual `read` / `modify` a result may carry:
    /// access to a place the receiver keeps. Neither means an owned value, so
    /// `-> consume T` is refused as a second spelling of the default.
    ///
    /// Every signature position calls this; one that cannot carry a capability
    /// refuses a `Some` in its own words, rather than an unknown-type error for
    /// `read` later.
    fn parse_result_capability(&mut self) -> Result<Option<Capability>, Diagnostic> {
        let Tok::Ident(id) = self.peek() else {
            return Ok(None);
        };
        let Some(cap) = Capability::from_word(id) else {
            return Ok(None);
        };
        // The word counts only when a type follows. After `-> read {` the brace is the
        // body and `read` is a type name.
        if !matches!(
            self.tokens[self.pos + 1].tok,
            Tok::Ident(_) | Tok::LParen | Tok::Fn
        ) {
            return Ok(None);
        }
        if cap == Capability::Consume {
            return Err(refuse!("parse", self.line(), self.col(), ConsumeResult));
        }
        self.advance();
        Ok(Some(cap))
    }

    /// One `fn m(read|modify|consume self, ..) -> R { .. }` in an `impl`: a
    /// [`Function`] whose first parameter is `self`, typed to the implementing type
    /// and carrying the receiver's capability. A bare `self` is `read`.
    fn impl_method(
        &mut self,
        self_ty: &Type,
    ) -> Result<(Function, Option<Capability>), Diagnostic> {
        let line = self.line();
        self.eat(&Tok::Fn)?;
        let col = self.col();
        let name = self.expect_ident()?;
        self.eat(&Tok::LParen)?;
        let capability = self.parse_self_capability();
        let (self_line, self_col) = self.binder_pos();
        self.eat(&Tok::Vself)?;
        let mut params = vec![Param {
            id: Id::NEW,
            name: "self".to_string(),
            capability,
            ty: self_ty.clone(),
            line: self_line,
            col: self_col,
        }];
        while *self.peek() == Tok::Comma {
            self.advance();
            let (line, col) = self.binder_pos();
            let pname = self.expect_ident()?;
            self.eat(&Tok::Colon)?;
            let capability = self.parse_capability();
            let ty = self.type_()?;
            params.push(Param {
                id: Id::NEW,
                name: pname,
                capability,
                ty,
                line,
                col,
            });
        }
        self.eat(&Tok::RParen)?;
        let (ret, result_cap) = if *self.peek() == Tok::Arrow {
            self.advance();
            let (rline, rcol) = (self.line(), self.col());
            let rc = self.parse_result_capability()?;
            // A projection's capabilities must agree: `-> read T` needs `read self` and
            // `-> modify T` needs `modify self`. `consume self` pairs with neither,
            // because the receiver must outlive the access.
            if let Some(rc) = rc {
                if capability != rc {
                    let want = rc.word();
                    return Err(refuse!(
                        "parse",
                        rline,
                        rcol,
                        ProjectionReceiver,
                        name,
                        want
                    ));
                }
            }
            (self.type_()?, rc)
        } else {
            (Type::Unit, None)
        };
        let body = self.block()?;
        Ok((
            Function {
                exported: false,
                module: None,
                name,
                doc: None,
                type_params: Vec::new(),
                type_bounds: Default::default(),
                params,
                ret,
                body,
                line,
                col,
                is_extern: false,
                is_export_extern: false,
                is_gen: false,
                is_mut: false,
            },
            result_cap,
        ))
    }

    /// `logging { level: <name>, sink: <dest> }`, each field optional. Returns the
    /// threshold ordinal and the sink.
    fn logging_config(&mut self) -> Result<(usize, LogSink), Diagnostic> {
        self.advance();
        self.eat(&Tok::LBrace)?;
        let mut level = DEFAULT_LOG_LEVEL;
        let mut sink = LogSink::Stderr;
        while *self.peek() != Tok::RBrace {
            let line = self.line();
            let col = self.col();
            let key = self.expect_ident()?;
            self.eat(&Tok::Colon)?;
            match key.as_str() {
                "level" => {
                    let name = self.expect_ident()?;
                    level = log_level_ordinal(&name)
                        .ok_or_else(|| refuse!("parse", line, col, UnknownLogLevel, name))?;
                }
                "sink" => sink = self.log_sink()?,
                other => return Err(refuse!("parse", line, col, UnknownLoggingField, other)),
            }
            if *self.peek() == Tok::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.eat(&Tok::RBrace)?;
        Ok((level, sink))
    }

    fn log_sink(&mut self) -> Result<LogSink, Diagnostic> {
        let line = self.line();
        let col = self.col();
        let name = self.expect_ident()?;
        match name.as_str() {
            "stderr" => Ok(LogSink::Stderr),
            "stdout" => Ok(LogSink::Stdout),
            "file" => {
                self.eat(&Tok::LParen)?;
                let path = match self.advance() {
                    Tok::Str(s) => s,
                    other => {
                        return Err(refuse!(
                            "parse",
                            line,
                            col,
                            FileSinkPath,
                            found = format!("{other:?}")
                        ))
                    }
                };
                self.eat(&Tok::RParen)?;
                Ok(LogSink::File(path))
            }
            other => Err(refuse!("parse", line, col, UnknownSink, other)),
        }
    }

    /// `import { a, b } from "path"`; `from` is contextual. `import type { .. }`
    /// marks a JSON Schema import; the loader dispatches on the path's extension.
    fn import_decl(&mut self) -> Result<ImportDecl, Diagnostic> {
        let line = self.line();
        self.eat(&Tok::Import)?;
        // `import * as ns from <source>` binds one name and adds none of
        // the target's exports to the flat namespace.
        if *self.peek() == Tok::Star {
            self.advance();
            match self.advance() {
                Tok::Ident(kw) if kw == "as" => {}
                other => {
                    return Err(refuse!(
                        "parse",
                        line,
                        self.col(),
                        ImportStarAs,
                        found = format!("{other:?}")
                    ))
                }
            }
            let ns = self.expect_ident()?;
            match self.advance() {
                Tok::Ident(kw) if kw == "from" => {}
                other => {
                    return Err(refuse!(
                        "parse",
                        line,
                        self.col(),
                        ImportStarFrom,
                        ns,
                        found = format!("{other:?}")
                    ))
                }
            }
            let source = self.import_source(line)?;
            self.eat_semi();
            return Ok(ImportDecl {
                names: Vec::new(),
                namespace: Some(ns),
                source,
                line,
            });
        }
        if *self.peek() == Tok::Type {
            self.advance();
        }
        self.eat(&Tok::LBrace)?;
        let mut names = Vec::new();
        while *self.peek() != Tok::RBrace {
            let original = self.expect_ident()?;
            // `as` is contextual: only between an import name and its `,` or `}`.
            let alias = if matches!(self.peek(), Tok::Ident(kw) if kw == "as") {
                self.advance();
                Some(self.expect_ident()?)
            } else {
                None
            };
            names.push(ImportName { original, alias });
            if *self.peek() == Tok::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.eat(&Tok::RBrace)?;
        if names.is_empty() {
            return Err(refuse!("parse", line, self.col(), ImportEmpty));
        }
        match self.advance() {
            Tok::Ident(kw) if kw == "from" => {}
            other => {
                return Err(refuse!(
                    "parse",
                    line,
                    self.col(),
                    ImportFrom,
                    found = format!("{other:?}")
                ))
            }
        }
        let source = self.import_source(line)?;
        self.eat_semi();
        Ok(ImportDecl {
            names,
            namespace: None,
            source,
            line,
        })
    }

    /// The source after `from`: a module path string, or a generator call
    /// `gen(args...)` run at compile time.
    fn import_source(&mut self, line: usize) -> Result<ImportSource, Diagnostic> {
        match self.peek().clone() {
            Tok::Str(p) => {
                self.advance();
                Ok(ImportSource::Path(p))
            }
            Tok::Ident(gen_name) if matches!(self.tokens[self.pos + 1].tok, Tok::LParen) => {
                let call_line = self.line();
                self.advance();
                self.eat(&Tok::LParen)?;
                let mut args = Vec::new();
                while *self.peek() != Tok::RParen {
                    args.push(self.expr()?);
                    if *self.peek() == Tok::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                self.eat(&Tok::RParen)?;
                Ok(ImportSource::Generator {
                    name: gen_name,
                    args,
                    line: call_line,
                })
            }
            other => Err(refuse!(
                "parse",
                line,
                self.col(),
                ImportPathExpected,
                found = format!("{other:?}")
            )),
        }
    }

    fn type_decl(&mut self) -> Result<Vec<TypeDecl>, Diagnostic> {
        let (line, col) = (self.line(), self.col());
        self.eat(&Tok::Type)?;
        let name = self.expect_ident()?;

        let mut type_params = Vec::new();
        if *self.peek() == Tok::Lt {
            self.advance();
            while *self.peek() != Tok::Gt {
                type_params.push(self.expect_ident()?);
                if *self.peek() == Tok::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
            self.eat(&Tok::Gt)?;
        }
        self.type_params = type_params.clone();

        self.eat(&Tok::Eq)?;
        // Collect inline field refinements while the base parses; the collector is
        // what makes `where` legal in field position.
        self.field_preds = Some(Vec::new());
        let base = if *self.peek() == Tok::Pipe {
            self.enum_type()?
        } else {
            self.type_()?
        };
        let field_preds = self.field_preds.take().unwrap_or_default();
        let predicate = if *self.peek() == Tok::Where {
            self.advance();
            Some(self.expr()?)
        } else {
            None
        };
        self.eat_semi();
        self.type_params.clear();

        // Each inline refinement becomes a synthetic validated type named
        // `Decl.field`; the `.` keeps it out of the user namespace. The field's type is
        // rewritten to it, so validation applies unchanged.
        let mut base = base;
        let mut decls = Vec::with_capacity(1 + field_preds.len());
        if !field_preds.is_empty() {
            let Type::Record(fields) = &mut base else {
                // A refinement was collected but the outer `{ .. }` is not the whole base: a
                // `&` wrapped it in [`Type::Merge`], or an enum payload holds it. There is no
                // single record to hang `Decl.field` on.
                return Err(refuse!("parse", line, col, InlineWhereBase, name));
            };
            for (fname, pred) in field_preds {
                let synthetic = format!("{name}.{fname}");
                let field = fields
                    .iter_mut()
                    .find(|f| f.name == fname)
                    .expect("collected predicate names an existing field");
                decls.push(TypeDecl {
                    exported: false,
                    module: None,
                    name: synthetic.clone(),
                    doc: None,
                    type_params: Vec::new(),
                    base: std::mem::replace(&mut field.ty, Type::Named(synthetic)),
                    predicate: Some(pred),
                    line,
                });
            }
        }
        decls.insert(
            0,
            TypeDecl {
                name,
                exported: false,
                module: None,
                doc: None,
                type_params,
                base,
                predicate,
                line,
            },
        );
        Ok(decls)
    }

    /// Parses an optional capability keyword (`read`/`modify`/`consume`/`share`)
    /// before a parameter's type. Contextual: the word counts only when the next
    /// token starts a type, `Tok::Fn` included (`run: consume fn() -> T`). A call
    /// `consume(..)` stays a call, because `LParen` does not start a type.
    fn parse_capability(&mut self) -> Capability {
        if let Tok::Ident(id) = self.peek() {
            if let Some(c) = Capability::from_word(id) {
                if matches!(self.tokens[self.pos + 1].tok, Tok::Ident(_) | Tok::Fn) {
                    self.advance();
                    return c;
                }
            }
        }
        Capability::Read
    }

    /// `| Variant(Type) | Variant | ...`: an enum. The leading `|` is required; it
    /// tells an enum from the other type forms.
    fn enum_type(&mut self) -> Result<Type, Diagnostic> {
        let mut variants = Vec::new();
        while *self.peek() == Tok::Pipe {
            self.advance();
            let name = self.expect_ident()?;
            let mut payload = Vec::new();
            if *self.peek() == Tok::LParen {
                self.advance();
                while *self.peek() != Tok::RParen {
                    payload.push(self.type_()?);
                    if *self.peek() == Tok::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                self.eat(&Tok::RParen)?;
            }
            variants.push(EnumVariant { name, payload });
        }
        Ok(Type::Enum(variants))
    }

    /// `{ field: Type, ... }`: a record type. In a `type` declaration a field may
    /// carry an inline refinement (`age: Int64 where value >= 18`); the trailing
    /// `where` after `}` states the cross-field invariant.
    fn record_type(&mut self) -> Result<Type, Diagnostic> {
        self.eat(&Tok::LBrace)?;
        // Only the outermost record of a `type` declaration collects refinements:
        // `take()` hides the collector from nested, anonymous records.
        let outer = self.field_preds.take();
        let collecting = outer.is_some();
        let mut local: Vec<(String, Expr)> = Vec::new();
        let mut parse = || -> Result<Vec<Field>, Diagnostic> {
            let mut fields = Vec::new();
            while *self.peek() != Tok::RBrace {
                let name = self.expect_ident()?;
                self.eat(&Tok::Colon)?;
                // `field: lazy T`. `lazy` is contextual: read only where a field's
                // type begins, a position no call occupies, so `std/ui`'s `lazy(..)` keeps its
                // name.
                let lazy = matches!(self.peek(), Tok::Ident(w) if w == "lazy");
                if lazy {
                    let line = self.line();
                    let col = self.col();
                    self.advance();
                    // Named record types only: the deferral is a fact about a declared field, and
                    // an anonymous record has no declaration to stamp on a value.
                    if !collecting {
                        return Err(refuse!("parse", line, col, LazyAnonymous));
                    }
                }
                let ty = self.type_()?;
                let ty = if lazy { Type::Lazy(Box::new(ty)) } else { ty };
                if *self.peek() == Tok::Where {
                    let line = self.line();
                    let col = self.col();
                    self.advance();
                    let pred = self.expr()?;
                    if !collecting {
                        return Err(refuse!("parse", line, col, WhereAnonymous));
                    }
                    // An inline `where` would move the field into a synthetic type and hide the
                    // `lazy` marker, so the read would stop being forced.
                    if lazy {
                        return Err(refuse!("parse", line, col, LazyWhere));
                    }
                    local.push((name.clone(), pred));
                }
                fields.push(Field { name, ty });
                if *self.peek() == Tok::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
            self.eat(&Tok::RBrace)?;
            Ok(fields)
        };
        let result = parse();
        // Restore and extend the collector on the error path too, for `type_decl`'s
        // recovery.
        if let Some(mut prev) = outer {
            prev.extend(local);
            self.field_preds = Some(prev);
        }
        Ok(Type::Record(result?))
    }

    /// An optional `<T: Bound + Other, U>` binder, empty when no `<` follows.
    /// Shared by `fn` and `impl<..>`, so an impl keeps its bounds.
    #[allow(clippy::type_complexity)]
    /// The explicit type arguments of a call (`fromJson<Shape>(s)`), or an empty
    /// list.
    ///
    /// `<` after a callee is also less-than, so this parse is speculative: it
    /// accepts a comma-separated type list closed by `>` with `(` right after.
    /// Anything else rewinds the cursor and records no diagnostic. The one program
    /// this reads differently is `a < b > (c)`, which compares a `Bool` with a
    /// value and has no meaning.
    fn call_type_args(&mut self) -> Vec<Type> {
        if *self.peek() != Tok::Lt {
            return Vec::new();
        }
        let saved = self.pos;
        self.advance();
        let mut out = Vec::new();
        loop {
            match self.type_() {
                Ok(t) => out.push(t),
                Err(_) => {
                    self.pos = saved;
                    return Vec::new();
                }
            }
            if *self.peek() == Tok::Comma {
                self.advance();
            } else {
                break;
            }
        }
        let closes = *self.peek() == Tok::Gt
            && matches!(
                self.tokens.get(self.pos + 1).map(|t| &t.tok),
                Some(Tok::LParen)
            );
        if closes && !out.is_empty() {
            self.advance();
            return out;
        }
        self.pos = saved;
        Vec::new()
    }

    fn type_param_binder(
        &mut self,
    ) -> Result<(Vec<String>, std::collections::HashMap<String, Vec<String>>), Diagnostic> {
        let mut type_params = Vec::new();
        let mut type_bounds: std::collections::HashMap<String, Vec<String>> = Default::default();
        if *self.peek() == Tok::Lt {
            self.advance();
            while *self.peek() != Tok::Gt {
                let tp = self.expect_ident()?;
                if *self.peek() == Tok::Colon {
                    self.advance();
                    let mut bounds = Vec::new();
                    loop {
                        bounds.push(self.expect_ident()?);
                        if *self.peek() == Tok::Plus {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    type_bounds.insert(tp.clone(), bounds);
                }
                type_params.push(tp);
                if *self.peek() == Tok::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
            self.eat(&Tok::Gt)?;
        }
        Ok((type_params, type_bounds))
    }

    /// `[gen] fn name<...>(params) -> Ret { body }`. `is_gen` says a contextual
    /// `gen` preceded `fn`.
    fn function(&mut self, is_gen: bool) -> Result<Function, Diagnostic> {
        let line = self.line();
        self.eat(&Tok::Fn)?;
        let col = self.col();
        let name = self.expect_ident()?;

        let (type_params, type_bounds) = self.type_param_binder()?;
        self.type_params = type_params.clone();

        self.eat(&Tok::LParen)?;

        let mut params = Vec::new();
        while *self.peek() != Tok::RParen {
            let (line, col) = self.binder_pos();
            let pname = self.expect_ident()?;
            self.eat(&Tok::Colon)?;
            let capability = self.parse_capability();
            let ty = self.type_()?;
            params.push(Param {
                id: Id::NEW,
                name: pname,
                capability,
                ty,
                line,
                col,
            });
            if *self.peek() == Tok::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.eat(&Tok::RParen)?;

        // No `-> Type` means Unit.
        let ret = if *self.peek() == Tok::Arrow {
            self.advance();
            let (rline, rcol) = (self.line(), self.col());
            if self.parse_result_capability()?.is_some() {
                return Err(refuse!("parse", rline, rcol, FreeFnCapability, name));
            }
            self.type_()?
        } else {
            Type::Unit
        };

        let body = self.block()?;
        self.type_params.clear();
        Ok(Function {
            name,
            exported: false,
            module: None,
            doc: None,
            type_params,
            type_bounds,
            params,
            ret,
            body,
            line,
            col,
            is_extern: false,
            is_export_extern: false,
            is_gen,
            // The caller sets this when `mut` preceded `fn`.
            is_mut: false,
        })
    }

    /// `test "name" { body }` and `bench "name" { body }`: one declaration.
    /// The caller has checked the lookahead, so `word` only words the
    /// refusal. The checker allows `assert`/`assertEq` in a test and `blackBox` in
    /// a bench.
    fn named_block(&mut self, word: &str) -> Result<NamedBlock, Diagnostic> {
        let line = self.line();
        self.advance();
        let name = match self.advance() {
            Tok::Str(s) => s,
            other => {
                return Err(refuse!(
                    "parse",
                    self.line(),
                    self.col(),
                    NameStringExpected,
                    word,
                    found = format!("{other:?}")
                ))
            }
        };
        // A test and a bench are monomorphic.
        self.type_params.clear();
        let body = self.block()?;
        self.type_params.clear();
        Ok(NamedBlock {
            name,
            body,
            doc: None,
            module: None,
            line,
        })
    }

    /// `extern fn name(params) -> Ret`; `exported` says `export`
    /// preceded it.
    ///
    /// - `extern fn f(..)` is a JS import: the host supplies the body, so a body is
    ///   an error.
    /// - `export extern fn f(..) { .. }` is a Vyrn function exported to JS and must
    ///   have a body. The checker enforces the ABI type domain.
    fn extern_function(&mut self, exported: bool) -> Result<Function, Diagnostic> {
        let line = self.line();
        self.advance();
        self.eat(&Tok::Fn)?;
        let col = self.col();
        let name = self.expect_ident()?;
        // An extern has no generic parameters: the ABI is monomorphic.
        self.eat(&Tok::LParen)?;
        let mut params = Vec::new();
        while *self.peek() != Tok::RParen {
            let (line, col) = self.binder_pos();
            let pname = self.expect_ident()?;
            self.eat(&Tok::Colon)?;
            let capability = self.parse_capability();
            let ty = self.type_()?;
            params.push(Param {
                id: Id::NEW,
                name: pname,
                capability,
                ty,
                line,
                col,
            });
            if *self.peek() == Tok::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.eat(&Tok::RParen)?;
        let ret = if *self.peek() == Tok::Arrow {
            self.advance();
            self.type_()?
        } else {
            Type::Unit
        };
        let has_body = *self.peek() == Tok::LBrace;
        if exported {
            if !has_body {
                return Err(refuse!(
                    "parse",
                    self.line(),
                    self.col(),
                    ExportedExternBody
                ));
            }
            self.type_params.clear();
            let body = self.block()?;
            self.type_params.clear();
            return Ok(Function {
                name,
                exported: true,
                module: None,
                doc: None,
                type_params: Vec::new(),
                type_bounds: Default::default(),
                params,
                ret,
                body,
                line,
                col,
                is_extern: false,
                is_export_extern: true,
                is_gen: false,
                is_mut: false,
            });
        }
        if has_body {
            return Err(refuse!("parse", self.line(), self.col(), ExternBody));
        }
        self.eat_semi();
        Ok(Function {
            name,
            exported: false,
            module: None,
            doc: None,
            type_params: Vec::new(),
            type_bounds: Default::default(),
            params,
            ret,
            body: Block {
                id: Id::NEW,
                stmts: Vec::new(),
            },
            line,
            col,
            is_extern: true,
            is_export_extern: false,
            is_gen: false,
            is_mut: false,
        })
    }

    /// A type, possibly an intersection `A & B & ...` (left-associative
    /// `Merge<A, B>`).
    fn type_(&mut self) -> Result<Type, Diagnostic> {
        let mut t = self.type_atom()?;
        while *self.peek() == Tok::Amp {
            self.advance();
            let rhs = self.type_atom()?;
            t = Type::Merge(Box::new(t), Box::new(rhs));
        }
        Ok(t)
    }

    /// Every nested type argument enters here once, so a type's nesting is counted
    /// here ([`Parser::MAX_NEST`]).
    fn type_atom(&mut self) -> Result<Type, Diagnostic> {
        self.nest_enter()?;
        let r = self.type_atom_inner();
        self.depth -= 1;
        r
    }

    fn type_atom_inner(&mut self) -> Result<Type, Diagnostic> {
        if *self.peek() == Tok::LBrace {
            return self.record_type();
        }
        // `fn(T, U) -> R`, or `fn(T)` for a Unit return. The checker
        // restricts it to a parameter position.
        if *self.peek() == Tok::Fn {
            self.advance();
            self.eat(&Tok::LParen)?;
            let mut params = Vec::new();
            while *self.peek() != Tok::RParen {
                params.push(self.type_()?);
                if *self.peek() == Tok::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
            self.eat(&Tok::RParen)?;
            let ret = if *self.peek() == Tok::Arrow {
                self.advance();
                self.type_()?
            } else {
                Type::Unit
            };
            return Ok(Type::Fn(params, Box::new(ret)));
        }
        // An integer literal type argument, the `8` in
        // `SmallArray<Int64, 8>`. Any other constructor carrying one is refused by the
        // checker, not here.
        if let Tok::Int(n) = self.peek() {
            if *n >= 0 {
                let v = *n as u64;
                self.advance();
                return Ok(Type::ConstInt(v));
            }
        }
        let name = self.expect_ident()?;
        // `ns.User` / `ns.Box<T>`: the loader checks `ns` and rewrites the
        // dotted name to the resolved declaration, so no later pass sees a dot.
        if *self.peek() == Tok::Dot {
            let mut full = name;
            while *self.peek() == Tok::Dot {
                self.advance();
                full = format!("{full}.{}", self.expect_ident()?);
            }
            if *self.peek() == Tok::Lt {
                self.advance();
                let mut args = Vec::new();
                while *self.peek() != Tok::Gt {
                    args.push(self.type_()?);
                    if *self.peek() == Tok::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                self.eat(&Tok::Gt)?;
                return Ok(Type::App(full, args));
            }
            return Ok(Type::Named(full));
        }
        Ok(match name.as_str() {
            // `Int64` is the default integer, but always written: there is no unsized
            // `Int`.
            "Int64" => Type::Int,
            "Int8" => Type::IntN {
                bits: 8,
                signed: true,
            },
            "Int16" => Type::IntN {
                bits: 16,
                signed: true,
            },
            "Int32" => Type::IntN {
                bits: 32,
                signed: true,
            },
            "UInt8" => Type::IntN {
                bits: 8,
                signed: false,
            },
            "UInt16" => Type::IntN {
                bits: 16,
                signed: false,
            },
            "UInt32" => Type::IntN {
                bits: 32,
                signed: false,
            },
            "UInt64" => Type::IntN {
                bits: 64,
                signed: false,
            },
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
            "Stream" => {
                self.eat(&Tok::Lt)?;
                let inner = self.type_()?;
                self.eat(&Tok::Gt)?;
                Type::Stream(Box::new(inner))
            }
            "Array" => {
                self.eat(&Tok::Lt)?;
                let inner = self.type_()?;
                if *self.peek() == Tok::Comma {
                    self.advance();
                    let n = match self.peek() {
                        Tok::Int(n) if *n >= 0 => *n as usize,
                        _ => return Err(refuse!("parse", self.line(), self.col(), ArraySize)),
                    };
                    self.advance();
                    self.eat(&Tok::Gt)?;
                    Type::ArrayN(Box::new(inner), n)
                } else {
                    self.eat(&Tok::Gt)?;
                    Type::Array(Box::new(inner))
                }
            }
            // `SmallArray<T, N>`: `N` inline elements, spilling to the heap
            // past `N`. The checker enforces `1 <= N <= 64`.
            "SmallArray" => {
                self.eat(&Tok::Lt)?;
                let inner = self.type_()?;
                if *self.peek() != Tok::Comma {
                    return Err(refuse!(
                        "parse",
                        self.line(),
                        self.col(),
                        SmallArrayNeedsCapacity
                    ));
                }
                self.advance();
                let n = match self.peek() {
                    Tok::Int(n) if *n >= 0 => *n as usize,
                    _ => {
                        return Err(refuse!(
                            "parse",
                            self.line(),
                            self.col(),
                            SmallArrayCapacityType
                        ))
                    }
                };
                self.advance();
                self.eat(&Tok::Gt)?;
                Type::SmallArray(Box::new(inner), n)
            }
            // `Map<K, V>`: insertion-ordered. The checker enforces the key
            // rule, because a validated string type resolves to `String` only there.
            "Map" => {
                self.eat(&Tok::Lt)?;
                let key = self.type_()?;
                self.eat(&Tok::Comma)?;
                let val = self.type_()?;
                self.eat(&Tok::Gt)?;
                Type::Map(Box::new(key), Box::new(val))
            }
            "Option" => {
                self.eat(&Tok::Lt)?;
                let inner = self.type_()?;
                self.eat(&Tok::Gt)?;
                Type::option(inner)
            }
            "Result" => {
                self.eat(&Tok::Lt)?;
                let ok = self.type_()?;
                self.eat(&Tok::Comma)?;
                let err = self.type_()?;
                self.eat(&Tok::Gt)?;
                Type::result(ok, err)
            }
            // Compile-time transformers.
            "Omit" | "Pick" => {
                self.eat(&Tok::Lt)?;
                let base = self.type_()?;
                let mut keys = Vec::new();
                while *self.peek() == Tok::Comma {
                    self.advance();
                    keys.push(self.expect_ident()?);
                }
                self.eat(&Tok::Gt)?;
                if keys.is_empty() {
                    return Err(refuse!("parse", self.line(), self.col(), NeedsField, name));
                }
                if name == "Omit" {
                    Type::Omit(Box::new(base), keys)
                } else {
                    Type::Pick(Box::new(base), keys)
                }
            }
            "Merge" => {
                self.eat(&Tok::Lt)?;
                let a = self.type_()?;
                self.eat(&Tok::Comma)?;
                let b = self.type_()?;
                self.eat(&Tok::Gt)?;
                Type::Merge(Box::new(a), Box::new(b))
            }
            "Partial" => {
                self.eat(&Tok::Lt)?;
                let inner = self.type_()?;
                self.eat(&Tok::Gt)?;
                Type::Partial(Box::new(inner))
            }
            // Records are immutable, so `Readonly<T>` is `T`.
            "Readonly" => {
                self.eat(&Tok::Lt)?;
                let inner = self.type_()?;
                self.eat(&Tok::Gt)?;
                inner
            }
            // An associated type. An impl may not declare an alias named like
            // its own binder, so the two sets are disjoint and the order only picks which
            // diagnostic a bug produces.
            other if self.type_aliases.contains_key(other) => self.type_aliases[other].clone(),
            other if self.type_params.iter().any(|p| p == other) => Type::Param(other.to_string()),
            other if *self.peek() == Tok::Lt => {
                self.advance();
                let mut args = Vec::new();
                while *self.peek() != Tok::Gt {
                    args.push(self.type_()?);
                    if *self.peek() == Tok::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                self.eat(&Tok::Gt)?;
                Type::App(other.to_string(), args)
            }
            // Any other identifier is a named type; the checker resolves it.
            other => Type::Named(other.to_string()),
        })
    }

    /// The most levels of nested source the parser accepts. One parenthesis, one
    /// prefix operator, one type argument and one block are one level each.
    ///
    /// The source may be a fetched module the user did not write. 1024 is far
    /// above any written or generated Vyrn (the corpus peaks in the tens) and far
    /// below what later passes need on their stack.
    const MAX_NEST: u32 = 1024;

    /// Enters one level of nesting, or refuses. Every caller decrements on the way
    /// out, the error path included: recovery starts the next declaration at the
    /// depth this one left.
    fn nest_enter(&mut self) -> Result<(), Diagnostic> {
        if self.depth >= Self::MAX_NEST {
            return Err(refuse!(
                "parse",
                self.line(),
                self.col(),
                NestingTooDeep,
                max = Self::MAX_NEST
            ));
        }
        self.depth += 1;
        Ok(())
    }

    fn block(&mut self) -> Result<Block, Diagnostic> {
        self.nest_enter()?;
        let r = self.block_inner();
        self.depth -= 1;
        r
    }

    fn block_inner(&mut self) -> Result<Block, Diagnostic> {
        self.eat(&Tok::LBrace)?;
        let mut stmts = Vec::new();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            self.take_docs(); // discard any stray `///` inside a body
                              // Semicolons are optional separators: a stray one is skipped.
            if *self.peek() == Tok::Semi {
                self.advance();
                continue;
            }
            if *self.peek() == Tok::RBrace || *self.peek() == Tok::Eof {
                break;
            }
            // Statement-level recovery: a bad statement is recorded and
            // dropped, and parsing resumes at the next statement boundary, so each bad
            // statement gets its own diagnostic. A missing brace still propagates.
            let start = self.pos;
            match self.stmt() {
                Ok(s) => {
                    stmts.push(s);
                    // Splice in a desugar's follow-on statements (`a[i].f = v`), in order.
                    stmts.append(&mut self.extra_stmts);
                }
                Err(d) => {
                    self.errors.push(d);
                    self.extra_stmts.clear(); // drop any partial desugar
                                              // Force progress: a parser that failed
                                              // without consuming would re-error
                                              // forever. One that advanced needs no
                                              // skip, which could eat the `}`.
                    if self.pos == start {
                        self.advance();
                    }
                    self.sync_to_stmt();
                }
            }
        }
        self.eat(&Tok::RBrace)?;
        Ok(Block { id: Id::NEW, stmts })
    }

    /// Skips a failed statement's tokens to the next statement boundary: a token on
    /// a new line at this block's brace depth, a `;` at that depth, the block's
    /// `}`, or `Eof`. Consumes nothing at a boundary; [`Parser::block`] ensures
    /// progress.
    fn sync_to_stmt(&mut self) {
        let mut depth = 0i32;
        while *self.peek() != Tok::Eof {
            match self.peek() {
                Tok::LBrace => {
                    depth += 1;
                    self.advance();
                }
                Tok::RBrace => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                    self.advance();
                }
                Tok::Semi if depth == 0 => {
                    self.advance();
                    return;
                }
                _ if depth == 0 && self.line() > self.tokens[self.pos - 1].line => {
                    return;
                }
                _ => {
                    self.advance();
                }
            }
        }
    }

    /// Top-level module state: `let [mut] name [: Type] = init`. The
    /// initializer is required: module state has no default value.
    fn global_decl(&mut self) -> Result<GlobalDecl, Diagnostic> {
        let line = self.line();
        self.eat(&Tok::Let)?;
        let mutable = if *self.peek() == Tok::Mut {
            self.advance();
            true
        } else {
            false
        };
        let name = self.expect_ident()?;
        let ty = if *self.peek() == Tok::Colon {
            self.advance();
            Some(self.type_()?)
        } else {
            None
        };
        if *self.peek() != Tok::Eq {
            return Err(refuse!(
                "parse",
                self.line(),
                self.col(),
                GlobalNeedsInit,
                name
            ));
        }
        self.eat(&Tok::Eq)?;
        let init = self.expr()?;
        self.eat_semi();
        Ok(GlobalDecl {
            name,
            mutable,
            ty,
            init,
            doc: None,
            module: None,
            line,
        })
    }

    /// Parses an `if` whose token is current. `else if` nests the
    /// chained `if` as the only statement of an else-block, with its own line.
    fn if_stmt(&mut self, line: usize) -> Result<Stmt, Diagnostic> {
        self.advance(); // `if`
                        // `if let PAT = e { .. }`: the pattern-binding form.
        if *self.peek() == Tok::Let {
            return self.if_let_stmt(line);
        }
        let cond = self.cond_expr()?;
        let then_block = self.block()?;
        let else_block = self.else_tail()?;
        Ok(Stmt::If {
            id: Id::NEW,
            cond,
            then_block,
            else_block,
            line,
        })
    }

    /// Parses the optional `else` tail of `if` and `if let`: `else if`,
    /// `else if let`, or `else { .. }`.
    fn else_tail(&mut self) -> Result<Option<Block>, Diagnostic> {
        if *self.peek() != Tok::Else {
            return Ok(None);
        }
        self.advance();
        if *self.peek() == Tok::If {
            let else_line = self.line();
            self.advance(); // `if`
                            // `else if let` chains the pattern form; `else if` the ordinary one.
            let nested = if *self.peek() == Tok::Let {
                self.if_let_stmt(else_line)?
            } else {
                let cond = self.cond_expr()?;
                let then_block = self.block()?;
                let else_block = self.else_tail()?;
                Stmt::If {
                    id: Id::NEW,
                    cond,
                    then_block,
                    else_block,
                    line: else_line,
                }
            };
            Ok(Some(Block {
                id: Id::NEW,
                stmts: vec![nested],
            }))
        } else {
            Ok(Some(self.block()?))
        }
    }

    /// Parses `if let PAT = e { .. } [else ..]`; the caller consumed `if`.
    fn if_let_stmt(&mut self, line: usize) -> Result<Stmt, Diagnostic> {
        self.eat(&Tok::Let)?;
        let pattern = self.pattern()?;
        self.eat(&Tok::Eq)?;
        // No-struct context, so a bare `{` opens the body.
        let scrutinee = self.cond_expr()?;
        let then_block = self.block()?;
        let else_block = self.else_tail()?;
        Ok(if_let(pattern, scrutinee, then_block, else_block, line))
    }

    /// Returns a desugar's first statement and queues the rest in
    /// [`Parser::extra_stmts`] for [`Parser::block`] to splice in.
    fn spliced(&mut self, mut stmts: Vec<Stmt>) -> Stmt {
        let head = stmts.remove(0);
        self.extra_stmts.extend(stmts);
        head
    }

    /// `let Variant(a, b) = v`: binds the payloads in the enclosing
    /// scope, or traps. Each binder is a `let` of a `match` whose default arm
    /// ([`Pattern::Other`]) panics with the canonical wording; unused positions get
    /// unspellable `@` binders.
    fn refutable_let(&mut self, line: usize, mutable: bool) -> Result<Stmt, Diagnostic> {
        let col = self.col();
        let variant = self.expect_ident()?;
        if mutable {
            return Err(refuse!("parse", line, col, LetMutPattern, variant));
        }
        self.eat(&Tok::LParen)?;
        let mut binds: Vec<Binder> = Vec::new();
        while *self.peek() != Tok::RParen {
            binds.push(self.expect_binder()?);
            if *self.peek() == Tok::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.eat(&Tok::RParen)?;
        self.eat(&Tok::Eq)?;
        let scrut = self.expr()?;
        self.eat_semi();
        if !matches!(scrut, Expr::Var { .. }) {
            return Err(refuse!("parse", line, col, LetPatternScrutinee, variant));
        }
        if binds.is_empty() {
            return Err(refuse!("parse", line, col, LetEmptyVariant, variant));
        }
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
                value: m,
                line,
                col: b.col,
            });
        }
        Ok(self.spliced(stmts))
    }

    fn stmt(&mut self) -> Result<Stmt, Diagnostic> {
        let line = self.line();
        match self.peek() {
            Tok::Let => {
                self.advance();
                let mutable = if *self.peek() == Tok::Mut {
                    self.advance();
                    true
                } else {
                    false
                };
                // `let Variant(a, b) = v`: an ordinary `let`'s name is never followed by `(`.
                if matches!(self.peek(), Tok::Ident(_))
                    && self.tokens[self.pos + 1].tok == Tok::LParen
                {
                    return self.refutable_let(line, mutable);
                }
                let col = self.binder_pos().1;
                let name = self.expect_ident()?;
                let ty = if *self.peek() == Tok::Colon {
                    self.advance();
                    Some(self.type_()?)
                } else {
                    None
                };
                self.eat(&Tok::Eq)?;
                let mut value = self.expr()?;
                self.eat_semi();
                // `let x = r.a.pop()`: the receiver moves out and back around this statement,
                // only when the call is the whole initializer, so nothing observes the
                // container in between.
                if let Some((mut pre, post)) = hoist_mutating_receiver(&mut value, line) {
                    pre.push(Stmt::Let {
                        id: Id::NEW,
                        name,
                        mutable,
                        ty,
                        value,
                        line,
                        col,
                    });
                    pre.extend(post);
                    return Ok(self.spliced(pre));
                }
                Ok(Stmt::Let {
                    id: Id::NEW,
                    name,
                    mutable,
                    ty,
                    value,
                    line,
                    col,
                })
            }
            Tok::Return => {
                self.advance();
                // No value at `;`, `}` or EOF: a bare `return`.
                let value = if matches!(self.peek(), Tok::Semi | Tok::RBrace | Tok::Eof) {
                    None
                } else {
                    Some(self.expr()?)
                };
                self.eat_semi();
                Ok(Stmt::Return {
                    id: Id::NEW,
                    value,
                    line,
                })
            }
            Tok::Break => {
                self.advance();
                self.eat_semi();
                Ok(Stmt::Break { id: Id::NEW, line })
            }
            Tok::Continue => {
                self.advance();
                self.eat_semi();
                Ok(Stmt::Continue { id: Id::NEW, line })
            }
            Tok::If => self.if_stmt(line),
            Tok::While => {
                self.advance();
                // `while let PAT = e { body }` desugars to
                // `while true { if let PAT = e { body } else { break } }`.
                if *self.peek() == Tok::Let {
                    self.eat(&Tok::Let)?;
                    let pattern = self.pattern()?;
                    self.eat(&Tok::Eq)?;
                    let scrutinee = self.cond_expr()?;
                    let body = self.block()?;
                    let brk = Block {
                        id: Id::NEW,
                        stmts: vec![Stmt::Break { id: Id::NEW, line }],
                    };
                    return Ok(Stmt::While {
                        id: Id::NEW,
                        cond: Expr::Bool(true, Id::NEW),
                        body: Block {
                            id: Id::NEW,
                            stmts: vec![if_let(pattern, scrutinee, body, Some(brk), line)],
                        },
                        line,
                    });
                }
                let cond = self.cond_expr()?;
                let body = self.block()?;
                Ok(Stmt::While {
                    id: Id::NEW,
                    cond,
                    body,
                    line,
                })
            }
            Tok::For => {
                self.advance();
                let col = self.binder_pos().1;
                let var = self.expect_ident()?;
                self.eat(&Tok::In)?;
                // No-struct context, so a bare `{` opens the loop body.
                // `for x in consume xs`: `consume` counts only when an identifier
                // follows, so `for x in consume(y)` stays a call.
                let consuming = matches!(self.peek(), Tok::Ident(id) if id == "consume")
                    && matches!(self.tokens[self.pos + 1].tok, Tok::Ident(_));
                if consuming {
                    self.advance();
                }
                let iter = self.cond_expr()?;
                let body = self.block()?;
                Ok(Stmt::ForIn {
                    id: Id::NEW,
                    var,
                    iter,
                    body,
                    line,
                    consuming,
                    col,
                })
            }
            Tok::Region => {
                self.advance();
                let body = self.block()?;
                Ok(Stmt::Region {
                    id: Id::NEW,
                    body,
                    line,
                })
            }
            // `drop name`: reclaim a heap value explicitly. It consumes `name`.
            Tok::Drop => {
                self.advance();
                let name = self.expect_ident()?;
                self.eat_semi();
                Ok(Stmt::Drop {
                    id: Id::NEW,
                    name,
                    line,
                })
            }
            Tok::Ident(_) | Tok::Vself if self.tokens[self.pos + 1].tok == Tok::Eq => {
                let name = self.place_root()?;
                self.eat(&Tok::Eq)?;
                let value = self.expr()?;
                self.eat_semi();
                Ok(Stmt::Assign {
                    id: Id::NEW,
                    name,
                    value,
                    line,
                })
            }
            Tok::Ident(_) | Tok::Vself
                if self.tokens[self.pos + 1].tok == Tok::Dot
                    && matches!(self.tokens[self.pos + 2].tok, Tok::Ident(_))
                    && self.tokens[self.pos + 3].tok == Tok::Eq =>
            {
                let name = self.place_root()?;
                self.eat(&Tok::Dot)?;
                let field = self.expect_ident()?;
                self.eat(&Tok::Eq)?;
                let value = self.expr()?;
                self.eat_semi();
                Ok(Stmt::SetField {
                    id: Id::NEW,
                    name,
                    field,
                    value,
                    line,
                })
            }
            _ => {
                let e = self.expr()?;
                // `a[i] = v`: `postfix` parsed `a[i]` as `@at(a, i)`; a trailing `=` makes it
                // a store.
                if *self.peek() == Tok::Eq {
                    if let Expr::Call { name, args, .. } = &e {
                        if name == "@at" && args.len() == 2 {
                            // [`store_stmts`] states the rewrite, shared with a
                            // store through a projection. The shape is checked
                            // before the value parses, so an unreachable target
                            // gets this refusal, not an error from the right side.
                            if place_receiver(&args[0], line).is_some() {
                                self.advance();
                                let value = self.expr()?;
                                self.eat_semi();
                                let Some(stmts) = store_stmts(&e, &value, line) else {
                                    unreachable!("`place_receiver` answered for this place")
                                };
                                return Ok(self.spliced(stmts));
                            }
                            return Err(refuse!("parse", line, self.col(), IndexAssignTarget));
                        }
                    }
                    // `a[i].f = v`: [`store_stmts`] moves the container out, sets the field on the
                    // temp, and moves it back. The shape is checked before the value parses.
                    if let Expr::Field { expr, .. } = &e {
                        if let Expr::Call { name, args, .. } = expr.as_ref() {
                            if name == "@at" && args.len() == 2 {
                                if place_receiver(&args[0], line).is_some() {
                                    self.advance();
                                    let value = self.expr()?;
                                    self.eat_semi();
                                    let Some(stmts) = store_stmts(&e, &value, line) else {
                                        unreachable!("`place_receiver` answered for this place")
                                    };
                                    return Ok(self.spliced(stmts));
                                }
                                return Err(refuse!("parse", line, self.col(), FieldAssignTarget));
                            }
                        }
                        // `a[i].f.g = v` and deeper are refused: one level of field write-through.
                        if is_index_field_chain(expr) {
                            return Err(refuse!("parse", line, self.col(), FieldWriteDepth));
                        }
                    }
                }
                self.eat_semi();
                // A statement `x.push(v)` writes the grown array back to wherever `x` lives,
                // or the push lands on a copy:
                //   `sq.push(v)`   -> `sq = @push(sq, v)`
                //   `r.f.push(v)`  -> `r.f = @push(r.f, v)`
                //   `a[i].push(v)` -> `a[i] = @push(a[i], v)`
                // Any other receiver (a temporary, `r.a.b.push(v)`) is a parse error, not a
                // silent no-op. `pop`/`swapRemove`/`remove` return a value and mutate, so
                // they move the receiver out and back below.
                let mut e = e;
                if let Some((mut pre, post)) = hoist_mutating_receiver(&mut e, line) {
                    pre.push(Stmt::expr(e));
                    pre.extend(post);
                    return Ok(self.spliced(pre));
                }
                if let Expr::Call { name, args, .. } = &e {
                    if crate::prelude::rebuilds(name) {
                        match args.first() {
                            Some(Expr::Var { name: recv, .. }) => {
                                return Ok(Stmt::Assign {
                                    id: Id::NEW,
                                    name: recv.clone(),
                                    value: e,
                                    line,
                                });
                            }
                            // Writes back via `SetField`, under the rules of `r.f = ..`.
                            Some(Expr::Field {
                                expr: base, field, ..
                            }) if matches!(base.as_ref(), Expr::Var { .. }) => {
                                let Expr::Var { name: recv, .. } = base.as_ref() else {
                                    unreachable!()
                                };
                                return Ok(Stmt::SetField {
                                    id: Id::NEW,
                                    name: recv.clone(),
                                    field: field.clone(),
                                    value: e,
                                    line,
                                });
                            }
                            // Writes back via `IndexSet`; the index is evaluated
                            // on both the read and the store.
                            Some(Expr::Call {
                                name: at,
                                args: iargs,
                                ..
                            }) if at == "@at"
                                && iargs.len() == 2
                                && matches!(&iargs[0], Expr::Var { .. }) =>
                            {
                                let Expr::Var { name: recv, .. } = &iargs[0] else {
                                    unreachable!()
                                };
                                let recv = recv.clone();
                                let index = iargs[1].clone();
                                return Ok(Stmt::IndexSet {
                                    id: Id::NEW,
                                    name: recv,
                                    index,
                                    value: e,
                                    line,
                                });
                            }
                            _ => {
                                return Err(refuse!("parse", line, self.col(), PushNoPlace));
                            }
                        }
                    }
                }
                // Statement position: only a `match` here may have block arms. The
                // parser marks the node and the checker reads it.
                let mut e = e;
                if let Expr::Match { stmt_pos, .. } = &mut e {
                    *stmt_pos = true;
                }
                Ok(Stmt::expr(e))
            }
        }
    }

    // Expressions, by precedence climbing.

    fn expr(&mut self) -> Result<Expr, Diagnostic> {
        self.binary(0)
    }

    /// Parses an expression followed by a `{ .. }` block (a condition or a `match`
    /// scrutinee), where `Name {` is a value and then a block, not a struct literal.
    fn cond_expr(&mut self) -> Result<Expr, Diagnostic> {
        let saved = self.no_struct;
        self.no_struct = true;
        let e = self.expr();
        self.no_struct = saved;
        e
    }

    /// Binding powers: higher binds tighter.
    ///
    /// The bitwise family sits between comparison and arithmetic, so
    /// `x & mask == 0` is `(x & mask) == 0` and `a + b << c` is `(a + b) << c`.
    /// Within it, tightest first: `~` (in [`unary`]), `<< >>`, `&`, `^`, `|`.
    fn binop(tok: &Tok) -> Option<(BinOp, u8)> {
        Some(match tok {
            Tok::OrOr => (BinOp::Or, 1),
            Tok::AndAnd => (BinOp::And, 2),
            Tok::EqEq => (BinOp::Eq, 3),
            Tok::NotEq => (BinOp::NotEq, 3),
            Tok::TildeMatch => (BinOp::Match, 3),
            Tok::Lt => (BinOp::Lt, 4),
            Tok::LtEq => (BinOp::LtEq, 4),
            Tok::Gt => (BinOp::Gt, 4),
            Tok::GtEq => (BinOp::GtEq, 4),
            // 5 is `??` (`NULLISH_BP`, not a `BinOp`); the tiers below are shifted up to
            // leave it that slot.
            Tok::Pipe => (BinOp::BitOr, 6),
            Tok::Caret => (BinOp::BitXor, 7),
            Tok::Amp => (BinOp::BitAnd, 8),
            Tok::Shl => (BinOp::Shl, 9),
            Tok::Shr => (BinOp::Shr, 9),
            Tok::Plus => (BinOp::Add, 10),
            Tok::Minus => (BinOp::Sub, 10),
            Tok::Star => (BinOp::Mul, 11),
            Tok::Slash => (BinOp::Div, 11),
            Tok::Percent => (BinOp::Rem, 11),
            _ => return None,
        })
    }

    /// `??`'s binding power: tighter than comparison, looser than bitwise and
    /// arithmetic, as in Swift.
    ///
    /// Looser than arithmetic, so `opt ?? x + 1` is `opt ?? (x + 1)`. Tighter than
    /// comparison, so `flag ?? b == c` is `(flag ?? b) == c`: with
    /// `flag: Option<Bool>` and `b`, `c` both `Bool`, both readings typecheck and
    /// disagree, so a tie would parse wrong silently.
    ///
    /// `??` is parsed by hand in [`binary`] and recurses at its own power for right
    /// associativity. Sharing 6 with `BitOr` would bind asymmetrically against `|`,
    /// because the table's left-associative arm recurses at `bp + 1`.
    const NULLISH_BP: u8 = 5;

    fn binary(&mut self, min_bp: u8) -> Result<Expr, Diagnostic> {
        let mut lhs = self.unary()?;
        loop {
            if *self.peek() == Tok::QuestionQuestion && Self::NULLISH_BP >= min_bp {
                let line = self.line();
                self.advance();
                // Right-associative: `a ?? b ?? c` is `a ?? (b ?? c)`. The left reading cannot
                // typecheck, because `a ?? b` yields an unwrapped `T`.
                let rhs = self.binary(Self::NULLISH_BP)?;
                lhs = Self::nullish(lhs, rhs, line);
                continue;
            }
            let Some((op, bp)) = Self::binop(self.peek()) else {
                break;
            };
            if bp < min_bp {
                break;
            }
            let line = self.line();
            self.advance();
            let rhs = self.binary(bp + 1)?;
            lhs = Expr::Binary {
                id: Id::NEW,
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                line,
            };
        }
        Ok(lhs)
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

    /// Every expression recursion enters here once (`binary` calls it first, a
    /// prefix operator calls it again, `( .. )` returns through `primary`), so an
    /// expression's nesting is counted here ([`Parser::MAX_NEST`]).
    fn unary(&mut self) -> Result<Expr, Diagnostic> {
        self.nest_enter()?;
        let r = self.unary_inner();
        self.depth -= 1;
        r
    }

    fn unary_inner(&mut self) -> Result<Expr, Diagnostic> {
        let line = self.line();
        match self.peek() {
            Tok::Minus => {
                self.advance();
                Ok(Expr::Unary {
                    id: Id::NEW,
                    op: UnOp::Neg,
                    expr: Box::new(self.unary()?),
                    line,
                })
            }
            Tok::Bang => {
                self.advance();
                Ok(Expr::Unary {
                    id: Id::NEW,
                    op: UnOp::Not,
                    expr: Box::new(self.unary()?),
                    line,
                })
            }
            // `~x` binds like the other unary prefixes.
            Tok::Tilde => {
                self.advance();
                Ok(Expr::Unary {
                    id: Id::NEW,
                    op: UnOp::BitNot,
                    expr: Box::new(self.unary()?),
                    line,
                })
            }
            // `consume place`: the take. `consume` counts only when an
            // identifier or `self` follows, so `consume(y)` stays a call. `self` lexes as
            // its own keyword, and `std/slots`' `release(consume self)` needs
            // `consume self.vals`.
            Tok::Ident(id)
                if id == "consume"
                    && matches!(self.tokens[self.pos + 1].tok, Tok::Ident(_) | Tok::Vself) =>
            {
                self.advance();
                Ok(Expr::Consume {
                    id: Id::NEW,
                    place: Box::new(self.postfix()?),
                    line,
                })
            }
            _ => self.postfix(),
        }
    }

    /// Postfix `?`, `.field`, `.name(..)` and `[i]`, binding tighter than unary and
    /// binary operators.
    ///
    /// The chain loop is iterative, but every link adds a node the later walks
    /// recurse into, so each link counts toward [`Parser::MAX_NEST`]; otherwise
    /// `a.b.b...b` grows an unbounded tree and a walk aborts the process.
    fn postfix(&mut self) -> Result<Expr, Diagnostic> {
        // `nest_enter`/leave per link would balance to zero, so count the links
        // locally against the ambient depth; `self.depth` stays untouched for
        // recovery.
        let mut links: u32 = 0;
        let mut e = self.primary()?;
        while matches!(self.peek(), Tok::Question | Tok::Dot | Tok::LBracket) {
            links += 1;
            if self.depth.saturating_add(links) >= Self::MAX_NEST {
                return Err(refuse!(
                    "parse",
                    self.line(),
                    self.col(),
                    NestingTooDeep,
                    max = Self::MAX_NEST
                ));
            }
            let r = self.postfix_step(e);
            e = r?;
        }
        Ok(e)
    }

    /// One postfix link over `e`. The caller checked that a postfix token follows
    /// and counted the level this link adds.
    fn postfix_step(&mut self, e: Expr) -> Result<Expr, Diagnostic> {
        let line = self.line();
        match self.peek() {
            Tok::Question => {
                self.advance();
                return Ok(Expr::Try {
                    id: Id::NEW,
                    expr: Box::new(e),
                    line,
                });
            }
            Tok::Dot => {
                self.advance();
                let name = self.expect_ident()?;
                if *self.peek() == Tok::LParen {
                    // `recv.name(args)` is sugar for `name(recv, args)`.
                    self.advance();
                    let saved = self.no_struct;
                    self.no_struct = false;
                    let mut args = vec![e];
                    while *self.peek() != Tok::RParen {
                        args.push(self.expr()?);
                        if *self.peek() == Tok::Comma {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    self.no_struct = saved;
                    self.eat(&Tok::RParen)?;
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
                    let mut args = args;
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
                    return Ok(Expr::Call {
                        id: Id::NEW,
                        dot: args.len() > n,
                        name,
                        args,
                        type_args: Vec::new(),
                        line,
                    });
                } else if *self.peek() == Tok::LBrace
                    && !self.no_struct
                    && matches!(&e, Expr::Var { .. })
                {
                    // `ns.Type { .. }`: a `Field` followed by `{` has no other reading,
                    // so the head must be a bare namespace identifier. The name rides as
                    // `"ns.Type"`; the loader checks `ns` and rewrites it to the resolved
                    // declaration.
                    let Expr::Var { name: ns, .. } = e else {
                        unreachable!()
                    };
                    return self.struct_lit(format!("{ns}.{name}"), line);
                } else {
                    return Ok(Expr::field(e, name, line));
                }
            }
            Tok::LBracket => {
                // `recv[i]` is sugar for the bounds-checked `@at(recv, i)`.
                self.advance();
                let saved = self.no_struct;
                self.no_struct = false;
                let idx = self.expr()?;
                self.no_struct = saved;
                self.eat(&Tok::RBracket)?;
                return Ok(Expr::call("@at", vec![e, idx], line));
            }
            _ => Ok(e),
        }
    }

    /// Whether an expression starts here with a lambda's parameters: `x -> body`,
    /// `(x, y) -> body` or `() -> body`. The arrow decides; `(x)` alone stays a
    /// parenthesised expression.
    ///
    /// A parameter list holds only names and commas, so the scan stops at the
    /// first other token and a `(` never walks to the end of the stream.
    fn at_lambda(&self) -> bool {
        match self.peek() {
            Tok::Ident(_) => matches!(self.tok_at(self.pos + 1), Tok::Arrow),
            Tok::LParen => {
                let mut k = self.pos + 1;
                if matches!(self.tok_at(k), Tok::RParen) {
                    return matches!(self.tok_at(k + 1), Tok::Arrow);
                }
                loop {
                    if !matches!(self.tok_at(k), Tok::Ident(_)) {
                        return false;
                    }
                    k += 1;
                    match self.tok_at(k) {
                        Tok::Comma => k += 1,
                        Tok::RParen => return matches!(self.tok_at(k + 1), Tok::Arrow),
                        _ => return false,
                    }
                }
            }
            _ => false,
        }
    }

    fn lambda(&mut self, line: usize) -> Result<Expr, Diagnostic> {
        let col = self.col();
        let mut params = Vec::new();
        if *self.peek() == Tok::LParen {
            self.advance();
            while *self.peek() != Tok::RParen {
                params.push(self.expect_binder()?);
                if *self.peek() == Tok::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
            self.eat(&Tok::RParen)?;
        } else {
            params.push(self.expect_binder()?);
        }
        self.eat(&Tok::Arrow)?;
        let body = if *self.peek() == Tok::LBrace {
            LambdaBody::Block(self.block()?)
        } else {
            // A struct literal is legal again inside a lambda's expression body.
            let saved = self.no_struct;
            self.no_struct = false;
            let e = self.expr();
            self.no_struct = saved;
            LambdaBody::Expr(Box::new(e?))
        };
        Ok(Expr::Lambda {
            id: Id::NEW,
            params,
            body,
            line,
            col,
        })
    }

    fn primary(&mut self) -> Result<Expr, Diagnostic> {
        let line = self.line();
        let col = self.col();
        if self.at_lambda() {
            return self.lambda(line);
        }
        // The `|x| e` spelling gets a named refusal: a `|` is infix-only, so it cannot
        // start an expression.
        if matches!(self.peek(), Tok::Pipe | Tok::OrOr) {
            let (form, fix) = match self.peek() {
                Tok::OrOr => ("||", "`() -> ...`"),
                _ => ("|x|", "`x -> ...`, or `(x, y) -> ...` for more than one"),
            };
            return Err(refuse!("parse", line, col, LambdaNotHere, form, fix));
        }
        match self.advance() {
            Tok::Int(v) => Ok(Expr::int(v)),
            Tok::Byte(v) => Ok(Expr::Byte(v, Id::NEW)),
            Tok::Float(v) => Ok(Expr::Float(v, Id::NEW)),
            Tok::Str(s) => Ok(Expr::str(s)),
            Tok::Vself => Ok(Expr::var("self", line)),
            Tok::TemplateStr { parts, exprs } => self.template(parts, exprs, line, col),
            Tok::True => Ok(Expr::Bool(true, Id::NEW)),
            Tok::False => Ok(Expr::Bool(false, Id::NEW)),
            Tok::LParen => {
                let saved = self.no_struct;
                self.no_struct = false;
                let e = self.expr()?;
                self.no_struct = saved;
                self.eat(&Tok::RParen)?;
                Ok(e)
            }
            // An array literal `[a, b, c]`, or a map literal `[:]` / `["k": v]`:
            // a `:` after the first element marks a map.
            Tok::LBracket => {
                let saved = self.no_struct;
                self.no_struct = false;
                // `[:]`: the empty map; its value type comes from the expected `Map` type.
                if *self.peek() == Tok::Colon {
                    self.advance();
                    self.no_struct = saved;
                    self.eat(&Tok::RBracket)?;
                    return Ok(Expr::MapLit {
                        id: Id::NEW,
                        entries: Vec::new(),
                        line,
                    });
                }
                if *self.peek() == Tok::RBracket {
                    self.no_struct = saved;
                    self.advance();
                    return Ok(Expr::ArrayLit {
                        id: Id::NEW,
                        elems: Vec::new(),
                        line,
                    });
                }
                let first = self.expr()?;
                if *self.peek() == Tok::Colon {
                    self.advance();
                    let first_val = self.expr()?;
                    let mut entries = vec![(first, first_val)];
                    while *self.peek() == Tok::Comma {
                        self.advance();
                        if *self.peek() == Tok::RBracket {
                            break;
                        }
                        let k = self.expr()?;
                        self.eat(&Tok::Colon)?;
                        let v = self.expr()?;
                        entries.push((k, v));
                    }
                    self.no_struct = saved;
                    self.eat(&Tok::RBracket)?;
                    return Ok(Expr::MapLit {
                        id: Id::NEW,
                        entries,
                        line,
                    });
                }
                let mut elems = vec![first];
                if *self.peek() == Tok::Comma {
                    self.advance();
                    while *self.peek() != Tok::RBracket {
                        elems.push(self.expr()?);
                        if *self.peek() == Tok::Comma {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                }
                self.no_struct = saved;
                self.eat(&Tok::RBracket)?;
                Ok(Expr::ArrayLit {
                    id: Id::NEW,
                    elems,
                    line,
                })
            }
            Tok::Match => self.match_expr(line),
            // An `if` in expression position; `stmt` dispatches the statement
            // form earlier. The `if` token is consumed.
            Tok::If => self.if_expr(line),
            Tok::Ident(mut name) => {
                // Tagged template `tag"...\{e}..."`: an identifier followed on the
                // same line by an interpolated string. The same-line rule keeps a statement
                // ending in a variable from taking the next statement's string literal.
                let string_adjacent = self.tokens[self.pos].line == line;
                if string_adjacent && matches!(self.peek(), Tok::TemplateStr { .. }) {
                    if let Tok::TemplateStr { parts, exprs } = self.advance() {
                        return self.tagged_template(name, parts, exprs, line, col);
                    }
                }
                if string_adjacent && matches!(self.peek(), Tok::Str(_)) {
                    // `vyrn` is the one tag whose hole-less form is meaningful:
                    // `vyrn"type Query {}"` is a whole skeleton. Any other tag needs a hole.
                    if name == "vyrn" {
                        if let Tok::Str(s) = self.advance() {
                            return self.code_quote(vec![s], Vec::new(), line, col);
                        }
                    }
                    return Err(refuse!("parse", line, col, TemplateNoHole, name));
                }
                // `Name?(args)`, and `ns.Name?(args)`, whose dotted path folds into `name`
                // like a dotted type. Only a path that continues `?(` folds;
                // field reads and method calls keep their shapes.
                while *self.peek() == Tok::Dot
                    && matches!(
                        self.tokens.get(self.pos + 1).map(|t| &t.tok),
                        Some(Tok::Ident(_))
                    )
                    && matches!(
                        self.tokens.get(self.pos + 2).map(|t| &t.tok),
                        Some(Tok::Question)
                    )
                    && matches!(
                        self.tokens.get(self.pos + 3).map(|t| &t.tok),
                        Some(Tok::LParen)
                    )
                {
                    self.advance();
                    name = format!(
                        "{name}.{}",
                        match self.advance() {
                            Tok::Ident(seg) => seg,
                            _ => unreachable!("looked ahead"),
                        }
                    );
                }
                let fallible =
                    *self.peek() == Tok::Question && self.tokens[self.pos + 1].tok == Tok::LParen;
                if fallible {
                    self.advance();
                }
                let type_args = self.call_type_args();
                if *self.peek() == Tok::LParen {
                    self.advance();
                    let saved = self.no_struct;
                    self.no_struct = false;
                    let mut args = Vec::new();
                    while *self.peek() != Tok::RParen {
                        args.push(self.expr()?);
                        if *self.peek() == Tok::Comma {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    self.no_struct = saved;
                    self.eat(&Tok::RParen)?;
                    if !fallible {
                        if let Some(e) = Self::storage_desugar(&name, &args, line) {
                            return Ok(e);
                        }
                    }
                    Ok(if fallible {
                        Expr::TryConstruct {
                            id: Id::NEW,
                            name,
                            args,
                            line,
                        }
                    } else {
                        Expr::Call {
                            id: Id::NEW,
                            dot: false,
                            name,
                            args,
                            type_args,
                            line,
                        }
                    })
                } else if *self.peek() == Tok::LBrace && !self.no_struct {
                    self.struct_lit(name, line)
                } else {
                    Ok(Expr::var(name, line))
                }
            }
            other => Err(refuse!(
                "parse",
                line,
                col,
                UnexpectedToken,
                found = format!("{other:?}")
            )),
        }
    }

    /// Desugars an untagged interpolated string `"a\{e}b"` into a
    /// `@concat`/`@str` chain. Each hole is re-lexed and parsed as an expression.
    /// `parts.len() == exprs.len() + 1`.
    fn template(
        &mut self,
        parts: Vec<String>,
        exprs: Vec<Hole>,
        line: usize,
        col: usize,
    ) -> Result<Expr, Diagnostic> {
        let mut pieces: Vec<Expr> = Vec::new();
        if !parts[0].is_empty() {
            pieces.push(Expr::str(parts[0].clone()));
        }
        for (k, src) in exprs.iter().enumerate() {
            let e = self.parse_hole(src, line, col)?;
            // `@str` and `@concat` are unlexable, so a user call to `str` or `concat`
            // gets the migration hint.
            pieces.push(Expr::call("@str", vec![e], line));
            if !parts[k + 1].is_empty() {
                pieces.push(Expr::str(parts[k + 1].clone()));
            }
        }
        // There is at least one hole, so `pieces` is non-empty; a lone piece is
        // already a `String`.
        let mut iter = pieces.into_iter();
        let mut acc = iter.next().unwrap();
        for p in iter {
            acc = Expr::call("@concat", vec![acc, p], line);
        }
        Ok(acc)
    }

    /// Re-lexes and parses one interpolation hole as an expression, with the
    /// enclosing function's generic parameters.
    fn parse_hole(&self, hole: &Hole, line: usize, col: usize) -> Result<Expr, Diagnostic> {
        let src = hole.src.as_str();
        let toks = crate::lexer::lex(src)
            .map_err(|e| refuse!("parse", line, col, InInterpolation, detail = e.render()))?;
        // The hole's tokens count from its own line 1, column 1; move them to where
        // the hole stands, so a node's line and a lambda's key are its own.
        let toks = toks
            .into_iter()
            .map(|t| Token {
                line: hole.line + t.line - 1,
                col: if t.line == 1 {
                    hole.col + t.col - 1
                } else {
                    t.col
                },
                ..t
            })
            .collect();
        let mut sub = self.sub(toks);
        // A sub-parser diagnostic's line is relative to the hole: anchor it at the
        // template and embed the detail.
        let e = sub
            .expr()
            .map_err(|d| refuse!("parse", line, col, InInterpolation, detail = d.message))?;
        if *sub.peek() != Tok::Eof {
            return Err(refuse!(
                "parse",
                line,
                col,
                InterpolationTrailing,
                src = src.trim()
            ));
        }
        Ok(e)
    }

    /// Desugars a tagged template `tag"a\{e}b"` into
    /// `tag(list([parts..]), list([value(e)..]))`, each value boxed in the built-in
    /// `Value` enum. Needs at least one hole.
    fn tagged_template(
        &self,
        tag: String,
        parts: Vec<String>,
        exprs: Vec<Hole>,
        line: usize,
        col: usize,
    ) -> Result<Expr, Diagnostic> {
        // `vyrn"..."` is a code quote: the parts are a skeleton validated
        // here, and the holes are structural splices, not boxed `Value`s.
        if tag == "vyrn" {
            return self.code_quote(parts, exprs, line, col);
        }
        let parts_lit = Expr::ArrayLit {
            id: Id::NEW,
            elems: parts.into_iter().map(Expr::str).collect(),
            line,
        };
        let mut values = Vec::new();
        for src in &exprs {
            let e = self.parse_hole(src, line, col)?;
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
            return Ok(Expr::StructLit {
                id: Id::NEW,
                name: "Template".to_string(),
                fields: vec![
                    ("parts".to_string(), wrap(parts_lit)),
                    ("values".to_string(), wrap(values_lit)),
                ],
                line,
            });
        }
        Ok(Expr::call(
            tag,
            vec![wrap(parts_lit), wrap(values_lit)],
            line,
        ))
    }

    /// Desugars a `vyrn"..."` code quote into a `Code` expression.
    ///
    /// The parts, with each hole replaced by a `__vyrn_holeN` placeholder, form a
    /// skeleton. It must parse here in one of four modes (declarations, statements,
    /// expression, type); a failure is a parse diagnostic at the literal. Each
    /// hole's context ([`Parser::hole_context`]) becomes the flag argument to
    /// `@codeSplice`, which applies the splice rule at generation time. The quote
    /// lowers to a `@codeText(part) + @codeSplice(hole, flag) + ...` chain.
    fn code_quote(
        &self,
        parts: Vec<String>,
        exprs: Vec<Hole>,
        line: usize,
        col: usize,
    ) -> Result<Expr, Diagnostic> {
        let mut skel = String::new();
        for (i, part) in parts.iter().enumerate() {
            skel.push_str(part);
            if i < exprs.len() {
                skel.push_str(&format!("__vyrn_hole{i}"));
            }
        }
        if !self.skeleton_parses_any(&skel) {
            let (rule, skel_line, skel_col) = self.skeleton_error_detail(&skel);
            let rline = line + skel_line.saturating_sub(1);
            let rcol = if skel_line <= 1 { col } else { skel_col };
            return Err(Diagnostic::refusal(rline, rcol, "parse", rule));
        }
        let mut acc: Option<Expr> = None;
        let add = |acc: &mut Option<Expr>, e: Expr| {
            *acc = Some(match acc.take() {
                None => e,
                Some(prev) => Expr::Binary {
                    id: Id::NEW,
                    op: BinOp::Add,
                    lhs: Box::new(prev),
                    rhs: Box::new(e),
                    line,
                },
            });
        };
        let code_text = |s: &str| Expr::call("@codeText", vec![Expr::str(s)], line);
        for (i, part) in parts.iter().enumerate() {
            if !part.is_empty() {
                add(&mut acc, code_text(part));
            }
            if i < exprs.len() {
                let ctx = self.hole_context(&parts, &exprs, i);
                let value = self.parse_hole(&exprs[i], line, col)?;
                let splice = Expr::call("@codeSplice", vec![value, Expr::int(ctx)], line);
                add(&mut acc, splice);
            }
        }
        // The lexer always yields a part, but an all-empty skeleton still needs a
        // `Code` value.
        Ok(acc.unwrap_or_else(|| code_text("")))
    }

    /// The context of hole `i` in a code quote, the flag `@codeSplice` takes:
    ///   * `1`: identifier fragment, textually adjacent to a word character
    ///     (`route_\{name}`);
    ///   * `2`: identifier or type position, where a string literal in the hole
    ///     fails to parse (a string is valid wherever an expression is);
    ///   * `0`: expression position, where a `String` becomes an escaped literal.
    fn hole_context(&self, parts: &[String], exprs: &[Hole], i: usize) -> i64 {
        let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
        let before = parts[i].chars().next_back().is_some_and(word);
        let after = parts
            .get(i + 1)
            .and_then(|p| p.chars().next())
            .is_some_and(word);
        if before || after {
            return 1;
        }
        // A string literal in hole `i`, placeholders in the rest.
        let mut probe = String::new();
        for (j, part) in parts.iter().enumerate() {
            probe.push_str(part);
            if j < exprs.len() {
                if j == i {
                    probe.push_str("\"__vyrn_probe__\"");
                } else {
                    probe.push_str(&format!("__vyrn_hole{j}"));
                }
            }
        }
        if self.skeleton_parses_any(&probe) {
            0
        } else {
            2
        }
    }

    /// A sub-parser over `src` with the enclosing generic parameters, so a skeleton
    /// naming `T` in a generic `gen fn` parses.
    fn sub_parser(&self, src: &str) -> Option<Parser> {
        Some(self.sub(crate::lexer::lex(src).ok()?))
    }

    fn skeleton_parses_any(&self, src: &str) -> bool {
        self.parses_as_decls(src)
            || self.parses_as_stmts(src)
            || self.parses_as_expr(src)
            || self.parses_as_type(src)
    }

    fn parses_as_decls(&self, src: &str) -> bool {
        let Some(mut p) = self.sub_parser(src) else {
            return false;
        };
        let (_prog, errs) = p.program_accum();
        errs.is_empty() && *p.peek() == Tok::Eof
    }

    /// Statement-list mode: `src` is a function body, wrapped for `program_accum`.
    fn parses_as_stmts(&self, src: &str) -> bool {
        self.parses_as_decls(&as_fn_body(src))
    }

    fn parses_as_expr(&self, src: &str) -> bool {
        let Some(mut p) = self.sub_parser(src) else {
            return false;
        };
        p.expr().is_ok() && *p.peek() == Tok::Eof
    }

    fn parses_as_type(&self, src: &str) -> bool {
        let Some(mut p) = self.sub_parser(src) else {
            return false;
        };
        p.type_().is_ok() && *p.peek() == Tok::Eof
    }

    /// The message and skeleton-relative line and column for a skeleton that parses
    /// in no mode, preferring the statement-mode error.
    fn skeleton_error_detail(&self, skel: &str) -> (Rule, usize, usize) {
        if let Some(mut p) = self.sub_parser(&as_fn_body(skel)) {
            let (_prog, errs) = p.program_accum();
            if let Some(d) = errs.into_iter().next() {
                let sl = d.line.saturating_sub(1).max(1); // undo the wrapper's line
                let detail = d.message;
                return (rule!(SkeletonDetail, detail), sl, d.col);
            }
        }
        (Rule::SkeletonUnparsable {}, 1, 1)
    }

    /// `match scrutinee { pattern => expr, ... }`; the caller consumed `match`.
    fn match_expr(&mut self, line: usize) -> Result<Expr, Diagnostic> {
        let scrutinee = self.cond_expr()?;
        self.eat(&Tok::LBrace)?;
        let mut arms = Vec::new();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            let pattern = self.pattern()?;
            self.eat(&Tok::FatArrow)?;
            // `pat => { stmts }` is a block arm: a bare `{` starts no
            // expression, so the brace decides. The checker allows one only in statement
            // position. A block arm needs no comma.
            let body = if *self.peek() == Tok::LBrace {
                ArmBody::Block(self.block()?)
            } else {
                ArmBody::Expr(self.expr()?)
            };
            let block_arm = matches!(body, ArmBody::Block(_));
            arms.push(MatchArm { pattern, body });
            if *self.peek() == Tok::Comma {
                self.advance();
            } else if !block_arm {
                break;
            }
        }
        self.eat(&Tok::RBrace)?;
        Ok(Expr::Match {
            id: Id::NEW,
            scrutinee: Box::new(scrutinee),
            arms,
            stmt_pos: false,
            line,
        })
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
    /// helpers need no import.
    fn storage_desugar(name: &str, args: &[Expr], line: usize) -> Option<Expr> {
        let call = |n: &str, a: Vec<Expr>| Expr::call(n, a, line);
        // `load(TypeName, path)` takes a type name as an argument; the desugar turns it
        // into the type argument `fromJson<T>(s)` takes.
        let decode = |t: &Expr, text: Expr| Expr::Call {
            id: Id::NEW,
            dot: false,
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

    /// `if cond { expr } else { expr }` in expression position; the
    /// caller consumed `if`. Each branch is one expression, and `else if` nests as
    /// `else_branch`. A missing `else` is `None`, which the checker refuses by the
    /// totality rule.
    fn if_expr(&mut self, line: usize) -> Result<Expr, Diagnostic> {
        // `if let` is a statement only; the refusal suggests `match`.
        if *self.peek() == Tok::Let {
            return Err(refuse!("parse", line, self.col(), IfLetExpr));
        }
        let cond = self.cond_expr()?;
        let then_branch = self.if_branch()?;
        let else_branch = if *self.peek() == Tok::Else {
            self.advance();
            if *self.peek() == Tok::If {
                // `else if` nests an expression-`if` with its own line, as in `if_stmt`.
                let else_line = self.line();
                self.advance(); // `if`
                Some(Box::new(self.if_expr(else_line)?))
            } else {
                Some(Box::new(self.if_branch()?))
            }
        } else {
            None
        };
        Ok(Expr::IfExpr {
            id: Id::NEW,
            cond: Box::new(cond),
            then_branch: Box::new(then_branch),
            else_branch,
            line,
        })
    }

    /// Parses one `{ expr }` branch of an expression-`if`: exactly one expression.
    /// A statement keyword or leftover tokens are refused; a nested `if` is an
    /// expression and allowed.
    fn if_branch(&mut self) -> Result<Expr, Diagnostic> {
        let line = self.line();
        self.eat(&Tok::LBrace)?;
        // A leading statement keyword gets the targeted message before `expr()` gives
        // a generic one.
        if matches!(
            self.peek(),
            Tok::Let
                | Tok::Return
                | Tok::While
                | Tok::For
                | Tok::Region
                | Tok::Drop
                | Tok::Break
                | Tok::Continue
        ) {
            return Err(refuse!("parse", self.line(), self.col(), IfExprStatements));
        }
        // Braces delimit, so a bare `Name { .. }` is a struct literal again.
        let saved = self.no_struct;
        self.no_struct = false;
        let e = self.expr()?;
        self.no_struct = saved;
        if *self.peek() != Tok::RBrace {
            return Err(refuse!("parse", line, self.col(), IfExprStatements));
        }
        self.eat(&Tok::RBrace)?;
        Ok(e)
    }

    /// `Name { field: expr, ... }`; the caller consumed the name.
    fn struct_lit(&mut self, name: String, line: usize) -> Result<Expr, Diagnostic> {
        self.eat(&Tok::LBrace)?;
        let saved = self.no_struct;
        self.no_struct = false;
        let mut fields = Vec::new();
        while *self.peek() != Tok::RBrace {
            let fname = self.expect_ident()?;
            self.eat(&Tok::Colon)?;
            let value = self.expr()?;
            fields.push((fname, value));
            if *self.peek() == Tok::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.no_struct = saved;
        self.eat(&Tok::RBrace)?;
        Ok(Expr::StructLit {
            id: Id::NEW,
            name,
            fields,
            line,
        })
    }

    fn pattern(&mut self) -> Result<Pattern, Diagnostic> {
        let line = self.line();
        let mut name = self.expect_ident()?;
        // `ns.Color.Red`: the loader checks `ns` and reduces the path to
        // the variant name.
        if *self.peek() == Tok::Dot {
            while *self.peek() == Tok::Dot {
                self.advance();
                name = format!("{name}.{}", self.expect_ident()?);
            }
            let mut binds = Vec::new();
            if *self.peek() == Tok::LParen {
                self.advance();
                while *self.peek() != Tok::RParen {
                    binds.push(self.expect_binder()?);
                    if *self.peek() == Tok::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                self.eat(&Tok::RParen)?;
            }
            return Ok(Pattern::Variant(name, binds));
        }
        // Every identifier is a variant name, `Some`, `None`, `Ok` and `Err` included.
        let _ = line;
        let mut binds = Vec::new();
        if *self.peek() == Tok::LParen {
            self.advance();
            while *self.peek() != Tok::RParen {
                binds.push(self.expect_binder()?);
                if *self.peek() == Tok::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
            self.eat(&Tok::RParen)?;
        }
        Ok(Pattern::Variant(name, binds))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    fn parse_src(s: &str) -> Program {
        parse(lex(s).unwrap()).unwrap()
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
            "type Shape = | Circle(Int64) | Dot\n\
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
            "type Pair = | Two(Int64, Int64) | Zero\n\
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
        let mut p = Parser::over(toks);
        let (prog, errors) = p.program_accum();
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

    // `a[i].field = v` write-through.

    #[test]
    fn index_field_assign_desugars_to_load_setfield_store() {
        // `a[i].f = v`: the index binds, the element moves out, the field is set on
        // the temp, and the temp moves back.
        let p =
            parse_src("fn main() -> Int64 { let mut a: Array<Int64> = []  a[0].f = 9  return 0 }");
        let stmts = &p.functions[0].body.stmts;
        // let a | let a[]idx=0 | let mut a[]=a[a[]idx] | a[].f=9 | a[a[]idx]=a[] | return
        assert_eq!(stmts.len(), 6);
        assert!(
            matches!(&stmts[1], Stmt::Let { name, .. } if name == "a[]idx"),
            "the index binds once, so the load and the write-back name one \
              value — and `direct::elem_field_store` can fold the three \
              statements into one store through the element's address"
        );
        match &stmts[2] {
            Stmt::Let {
                name,
                mutable,
                value: Expr::Call { name: c, args, .. },
                ..
            } => {
                assert_eq!(name, "a[]");
                assert!(mutable, "the element copy must be mut so SetField applies");
                assert_eq!(c, "@at");
                assert!(matches!(args[0], Expr::Var { .. }));
            }
            other => panic!("expected `let mut a[] = a[0]`, got {other:?}"),
        }
        match &stmts[3] {
            Stmt::SetField { name, field, .. } => {
                assert_eq!(name, "a[]");
                assert_eq!(field, "f");
            }
            other => panic!("expected SetField on the temp, got {other:?}"),
        }
        match &stmts[4] {
            Stmt::IndexSet {
                name,
                value: Expr::Var { name: v, .. },
                ..
            } => {
                assert_eq!(name, "a", "stores back into the real array binding");
                assert_eq!(v, "a[]");
            }
            other => panic!("expected `a[0] = a[]`, got {other:?}"),
        }
    }

    // Index assignment through a place.

    /// `s.xs[0] = 9` lowers to three statements (move the header out, store, move
    /// it back) and nothing that reads the array elementwise. A copying lowering is
    /// equally correct and quadratic, so only the statement count shows it.
    #[test]
    fn index_assign_through_a_record_field_is_a_move() {
        let p = parse_src(
            "fn main() -> Int64 { let mut s: S = S { xs: [1, 2] }  s.xs[0] = 9  return 0 }",
        );
        let stmts = &p.functions[0].body.stmts;
        // [0] is the `let mut s = ..`; the desugar is [1..=3].
        match &stmts[1] {
            Stmt::Let {
                name,
                mutable: true,
                value: Expr::Field { field, .. },
                ..
            } => {
                assert_eq!(name, "s.xs[]");
                assert_eq!(field, "xs");
            }
            other => panic!("expected `let mut s.xs[] = s.xs`, got {other:?}"),
        }
        match &stmts[2] {
            Stmt::IndexSet { name, .. } => assert_eq!(name, "s.xs[]"),
            other => panic!("expected `s.xs[][0] = 9`, got {other:?}"),
        }
        match &stmts[3] {
            Stmt::SetField {
                name,
                field,
                value: Expr::Var { name: v, .. },
                ..
            } => {
                assert_eq!(
                    (name.as_str(), field.as_str(), v.as_str()),
                    ("s", "xs", "s.xs[]")
                );
            }
            other => panic!("expected `s.xs = s.xs[]`, got {other:?}"),
        }
        assert_eq!(
            stmts.len(),
            5,
            "three statements plus the let and the return"
        );
    }

    #[test]
    fn index_assign_through_a_nested_field_nests_the_move() {
        // Two fields deep: the outermost moves out first and back last.
        let p = parse_src(
            "fn main() -> Int64 { let mut o: O = O { i: I { xs: [1] } }  o.i.xs[0] = 9  return 0 }",
        );
        let names: Vec<String> = p.functions[0]
            .body
            .stmts
            .iter()
            .map(|s| format!("{s:?}"))
            .collect();
        let joined = names.join("\n");
        for needle in [
            r#"Let { name: "o.i[]""#,
            r#"Let { name: "o.i[].xs[]""#,
            r#"IndexSet { name: "o.i[].xs[]""#,
            r#"SetField { name: "o.i[]", field: "xs""#,
            r#"SetField { name: "o", field: "i""#,
        ] {
            assert!(joined.contains(needle), "missing {needle} in\n{joined}");
        }
    }

    /// Anything the mutation's operands could read runs before the move-out, which
    /// is why `t.xs[t.xs.length - 1] = 99` answers `99`. Only the statement order
    /// shows it.
    #[test]
    fn an_index_assign_through_a_field_hoists_its_operands_before_the_move() {
        let p = parse_src(
            "fn main() -> Int64 { let mut s: S = S { xs: [1, 2] }  s.xs[f()] = g()  return 0 }",
        );
        let stmts = &p.functions[0].body.stmts;
        let shape: Vec<String> = stmts[1..=4].iter().map(|s| format!("{s:?}")).collect();
        assert!(
            shape[0].starts_with(r#"Let { name: "s.xs[]#idx""#) && shape[0].contains(r#""f""#),
            "the index runs first, into its own temp: {shape:#?}"
        );
        assert!(
            shape[1].starts_with(r#"Let { name: "s.xs[]#val""#) && shape[1].contains(r#""g""#),
            "then the value, left to right: {shape:#?}"
        );
        assert!(
            shape[2].starts_with(r#"Let { name: "s.xs[]""#),
            "only then is the field moved out: {shape:#?}"
        );
        assert!(
            shape[3].starts_with(r#"IndexSet { name: "s.xs[]""#)
                && !shape[3].contains(r#"name: "f""#)
                && !shape[3].contains(r#"name: "g""#),
            "the store names only temps, so it re-evaluates nothing: {shape:#?}"
        );
    }

    /// The desugar names a temp and [`crate::ast::is_place_temp`] reads the name
    /// back; every pass asks that one predicate, so a rename cannot leave a reader
    /// on the old spelling.
    #[test]
    fn the_desugars_temps_answer_the_one_predicate() {
        let mut minted: Vec<(String, bool)> = Vec::new();
        for src in [
            "fn main() -> Int64 { let mut s: S = S { xs: [1, 2] }  s.xs[f()] = g()  return 0 }",
            "fn main() -> Int64 { let mut ps: Array<S> = []  ps[1].xs = g()  return 0 }",
            "fn main() -> Int64 { let mut s: S = S { xs: [1] }  s.xs.swapRemove(h())  return 0 }",
        ] {
            for st in &parse_src(src).functions[0].body.stmts {
                if let Stmt::Let { name, .. } = st {
                    if name.contains('[') || name.contains('#') {
                        minted.push((name.clone(), crate::ast::is_place_temp(name)));
                    }
                }
            }
        }
        minted.sort();
        minted.dedup();
        assert_eq!(
            minted,
            [
                ("ps[]", true),
                ("ps[]#val", false),
                ("ps[]idx", false),
                ("s.xs[]", true),
                ("s.xs[]#idx", false),
                ("s.xs[]#val", false),
                ("s.xs[][]arg1", false),
            ]
            .map(|(n, p)| (n.to_string(), p))
        );
    }

    /// The index of a nested index assignment gets a temp, because the load and the
    /// write-back both need it: `rows[f()][0] = 1` calls `f` once.
    #[test]
    fn a_nested_index_assign_evaluates_its_index_once() {
        let p = parse_src(
            "fn main() -> Int64 { let mut rows: Array<Array<Int64>> = []  rows[f()][0] = 1  return 0 }",
        );
        let dump = format!("{:?}", p.functions[0].body.stmts);
        assert_eq!(
            dump.matches(r#"Call { name: "f""#).count(),
            1,
            "`f()` must appear once in the desugar:\n{dump}"
        );
        assert!(dump.contains(r#"Let { name: "rows[]idx""#), "{dump}");
    }

    #[test]
    fn pop_through_a_record_field_moves_out_and_back() {
        // `pop` mutates and returns, so it is hoisted around the whole statement.
        let p = parse_src(
            "fn main() -> Int64 { let mut s: S = S { xs: [1, 2] }  let x = s.xs.pop()  return 0 }",
        );
        let stmts = &p.functions[0].body.stmts;
        assert!(matches!(&stmts[1], Stmt::Let { name, .. } if name == "s.xs[]"));
        match &stmts[2] {
            Stmt::Let {
                name,
                value: Expr::Call { name: c, args, .. },
                ..
            } => {
                assert_eq!((name.as_str(), c.as_str()), ("x", "@pop"));
                assert!(matches!(&args[0], Expr::Var { name, .. } if name == "s.xs[]"));
            }
            other => panic!("expected `let x = s.xs[].pop()`, got {other:?}"),
        }
        assert!(
            matches!(&stmts[3], Stmt::SetField { name, field, .. } if name == "s" && field == "xs")
        );
    }

    #[test]
    fn pop_inside_a_branching_statement_is_still_rejected() {
        // No place for the move-back keeps the branch body from reading a stale field,
        // so the checker's error stays.
        let src = "fn main() -> Int64 { let mut s: S = S { xs: [1] }  if s.xs.pop() == None { return 1 }  return 0 }";
        let p = parse_src(src);
        // The receiver is still the field.
        assert!(format!("{:?}", p.functions[0].body.stmts)
            .contains(r#"Call { name: "@pop", args: [Field"#));
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
    fn push_on_record_field_desugars_to_setfield() {
        // `r.xs.push(x)` becomes `r.xs = @push(r.xs, x)`.
        let p =
            parse_src("fn main() -> Int64 { let mut r: R = R { xs: [] }  r.xs.push(1)  return 0 }");
        let stmts = &p.functions[0].body.stmts;
        match &stmts[1] {
            Stmt::SetField {
                name,
                field,
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
    fn push_on_array_element_desugars_to_indexset() {
        // `a[i].push(x)` becomes `a[i] = @push(a[i], x)`.
        let p = parse_src(
            "fn main() -> Int64 { let mut a: Array<Int64> = []  a[0].push(1)  return 0 }",
        );
        let stmts = &p.functions[0].body.stmts;
        match &stmts[1] {
            Stmt::IndexSet {
                name,
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

    /// Each postfix link counts toward [`Parser::MAX_NEST`], so a long `a.b.b...b`
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
