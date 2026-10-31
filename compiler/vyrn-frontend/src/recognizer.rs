//! The recognizer: `vyrn.syntax`'s syntactic rules over the lexer's tokens, recording
//! what they match as [`Event`]s.
//!
//! [`rules`] is written from the grammar, and this module is the cursor it runs on.
//! [`file`] reads a file into a [`Run`], which [`crate::parser`], the tree builder,
//! reads afterwards; [`recognize`] and [`recognize_hole`] return the refusals and the
//! words a rule consumed, which [`crate::classify`] reads.
//!
//! The grammar states shape only. The rows that need a value, a count or a name
//! comparison are raised here, in [`Recognizer::closed`], as the rule that holds them
//! closes: the cursor then stands where recovery must start, and no later text of
//! that statement or declaration has been refused.
//!
//! Protocol between the two files. A rule's method returns `Ok(false)` with the
//! cursor and the events unmoved, or `Err` with the cursor where the refusal arose.
//! `?` unwinds past the code that restores the flags and the nesting depth, so each
//! catch site, [`Recognizer::look`], [`Recognizer::attempt`] and
//! [`Recognizer::many_recovering`], restores them. The recognizer allocates only its
//! event, refusal and hole buffers, a refusal's text, and the tokens of a hole.

#[rustfmt::skip]
#[allow(unused_variables, unused_parens, clippy::all)]
mod rules;

pub use rules::{Hook, Kind};

use crate::ast::{Capability, OPEN_RULE_NAME};
use crate::diagnostics::Diagnostic;
use crate::lexer::{self, Hole, Tok, Token};
use crate::parser::{self, StoreShape};
use crate::rules::{rule, Rule};
use std::collections::HashSet;

/// What a rule returns: whether it matched. An `Err` is a refusal; the diagnostic
/// is in [`Recognizer::refusal`] unless a catch site will discard it.
type R = Result<bool, Refused>;

struct Refused;

/// What a run records, in the order its rules matched, so in source order. An index
/// is of a token of the run's slice. A predicate records nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A rule of [`Kind`] opens, at this token.
    Open(Kind, u32),
    /// The rule the `Open` at this event index opened closes.
    Close(u32),
    /// A rule consumed this token.
    Token(u32),
    /// A rule consumed what a split left of this `>>` or `>=`: a `>` or an `=`.
    Rest(u32),
    /// A rule consumed this identifier as a word: a contextual word.
    Word(u32),
    /// A rule consumed this identifier as a type name the declaration binds.
    Bound(u32),
    /// The string just consumed has a hole: this index of the run's [`HoleRun`]s.
    Hole(u32),
    /// A skim left a body unread: this index of [`Run::skips`].
    Skipped(u32),
    /// A declaration closed.
    Item,
}

/// A hook's rule that matched, in a reader's run: the token it opened at, the token it
/// stopped before, and whether that token's first `>` has gone.
#[derive(Debug, Clone, Copy)]
pub struct Spanned {
    pub hook: Hook,
    pub from: u32,
    pub to: u32,
    pub split: bool,
    /// The event count as the rule closed: a backtrack below it drops the rule.
    events: u32,
}

/// A run's record, read by [`crate::parser`].
#[derive(Clone, Copy)]
pub struct View<'a> {
    pub toks: &'a [Token],
    pub events: &'a [Event],
    pub holes: &'a [HoleRun],
}

/// The record of one interpolation hole.
#[derive(Default)]
pub struct HoleRun {
    pub toks: Vec<Token>,
    pub events: Vec<Event>,
    pub holes: Vec<HoleRun>,
}

/// What a file's run found.
pub(crate) struct Run {
    pub events: Vec<Event>,
    pub holes: Vec<HoleRun>,
    /// The tokens of each body a skim left unread.
    pub skips: Vec<std::ops::Range<usize>>,
    pub errors: Vec<Diagnostic>,
    /// The method-form builtin spellings the closed declarations answer to.
    pub answered: Vec<String>,
}

/// What a run reads from the module around its tokens.
#[derive(Clone, Copy)]
pub struct Cx<'a> {
    /// The method-form builtin spellings the module answers to ([`Run::answered`]).
    pub answers: &'a HashSet<String>,
    /// An imported module's parse: each `test` and `bench` block is skipped.
    pub imported: bool,
    /// Each top-level function's body is skipped.
    pub skim: bool,
}

impl<'a> Cx<'a> {
    pub fn plain(answers: &'a HashSet<String>) -> Cx<'a> {
        Cx {
            answers,
            imported: false,
            skim: false,
        }
    }
}

/// What a run found, for a reader that builds no tree.
pub struct Recognized {
    /// The refusals, sorted by line and column.
    pub errors: Vec<Diagnostic>,
    /// The tokens a rule consumed as a word, sorted, with those of text a recovery
    /// dropped.
    pub words: Vec<usize>,
}

/// Recognizes a file. `tokens` ends in `Eof`, as [`lexer::lex`] leaves it.
pub fn recognize(tokens: &[Token]) -> Recognized {
    let answers = HashSet::new();
    run(tokens, Cx::plain(&answers), |r| r._module())
}

/// Recognizes one expression, the whole of an interpolation hole. A token after
/// the expression is no refusal here: the file's run states it.
pub fn recognize_hole(tokens: &[Token]) -> Recognized {
    let answers = HashSet::new();
    run(tokens, Cx::plain(&answers), |r| r._expr())
}

fn run<'t>(
    tokens: &'t [Token],
    cx: Cx<'t>,
    rule: impl FnOnce(&mut Recognizer<'t>) -> R,
) -> Recognized {
    let mut r = Recognizer::over(tokens, cx, &[]);
    r.keep_words = true;
    let _ = rule(&mut r);
    let errors = r.sorted_refusals();
    let mut words = std::mem::take(&mut r.dropped_words);
    let run_words = r.events.iter().filter_map(|e| match e {
        Event::Word(i) => Some(*i as usize),
        _ => None,
    });
    words.extend(run_words);
    words.sort_unstable();
    Recognized { errors, words }
}

/// What a reader starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    Module,
    Type,
}

/// Reads `tokens` from `start` and returns each hook's rule that matched, in the order
/// the rules closed. A declaration a recovery dropped has none, and a function's body
/// stays unread, so a mistake in it does not drop the function. A `Type` that does not
/// read has none at all.
pub fn nodes(tokens: &[Token], start: Start) -> Vec<Spanned> {
    let answers = HashSet::new();
    let cx = Cx {
        skim: true,
        ..Cx::plain(&answers)
    };
    let mut r = Recognizer::over(tokens, cx, &[]);
    r.keep_nodes = true;
    let read = match start {
        Start::Module => r._module(),
        // A `type` declaration's record: a field may refine its type.
        Start::Type => {
            r.flags.collect = true;
            r._type()
        }
    };
    if matches!(read, Ok(true)) {
        r.nodes
    } else {
        Vec::new()
    }
}

/// Reads a file.
pub(crate) fn file(tokens: &[Token], cx: Cx<'_>) -> Run {
    let mut r = Recognizer::over(tokens, cx, &[]);
    let _ = r._module();
    let errors = r.sorted_refusals();
    Run {
        events: r.events,
        holes: r.holes,
        skips: r.skips,
        errors,
        answered: r.answered,
    }
}

/// Reads a function body, as [`parser::Skimmed::body`] asks: `read` builds it unless
/// it holds a refusal. The refusals of its statements come first, in the order they
/// arose, and the body's own last.
pub(crate) fn block<T>(
    tokens: &[Token],
    cx: Cx<'_>,
    scope: &[&str],
    read: impl FnOnce(View<'_>) -> T,
) -> Result<T, Vec<Diagnostic>> {
    let mut r = Recognizer::over(tokens, cx, scope);
    let result = r._block();
    let mut errors = std::mem::take(&mut r.errors);
    if !matches!(result, Ok(true)) {
        if result.is_ok() {
            let _ = r.expected("LBrace");
        }
        errors.extend(r.refusal.take().map(|d| *d));
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(read(r.view()))
}

/// The first refusal of `tokens` read as a code quote's text ([`lexer::lex_quote`]).
pub(crate) fn quote_refusal(tokens: &[Token], answers: &HashSet<String>) -> Option<Diagnostic> {
    let mut r = Recognizer::over(tokens, Cx::plain(answers), &[]);
    let _ = r._quote();
    r.sorted_refusals().into_iter().next()
}

/// Reads the one type in `tokens`, or `None`.
pub(crate) fn read_type<T>(
    tokens: &[Token],
    answers: &HashSet<String>,
    read: impl FnOnce(View<'_>) -> T,
) -> Option<T> {
    let mut r = Recognizer::over(tokens, Cx::plain(answers), &[]);
    matches!(r._type(), Ok(true)).then(|| read(r.view()))
}

/// A place in the tokens: the token, and whether its first `>` has gone.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
struct Cursor {
    /// An index below the slice's length: the last token is `Eof`, and no move
    /// passes it.
    at: u32,
    split: bool,
}

/// Where a label matched.
#[derive(Clone, Copy)]
struct Capture {
    from: Cursor,
    to: Cursor,
}

/// What a sequence restores when it fails.
#[derive(Clone, Copy)]
struct Save {
    cur: Cursor,
    events: u32,
}

/// What a catch site restores: the state `?` unwinds past, too.
#[derive(Clone, Copy)]
struct Catch {
    save: Save,
    flags: rules::Flags,
    depth: u32,
}

/// A contract member the running contract has read.
struct Member<'t> {
    name: &'t str,
    value: bool,
    line: usize,
}

struct Recognizer<'t> {
    toks: &'t [Token],
    cx: Cx<'t>,
    cur: Cursor,
    flags: rules::Flags,
    /// The `nest` rules running.
    depth: u32,
    /// The predicates running. Inside one a sequence never commits, no token's holes
    /// are parsed and no event is recorded.
    looking: u32,
    /// The catch sites running that discard a refusal, so none is built.
    quiet: u32,
    /// The refusal `Err` carries, when `quiet` is 0.
    refusal: Option<Box<Diagnostic>>,
    /// The refusals a declaration's recovery recorded. They sort before `errors`
    /// at one token.
    decl_errors: Vec<Diagnostic>,
    /// The refusals a statement's recovery recorded.
    errors: Vec<Diagnostic>,
    events: Vec<Event>,
    holes: Vec<HoleRun>,
    /// Whether a reader wants the words of the text a recovery drops.
    keep_words: bool,
    /// Whether a reader wants [`Recognizer::nodes`].
    keep_nodes: bool,
    nodes: Vec<Spanned>,
    /// The words of the text a recovery dropped from `events`.
    dropped_words: Vec<usize>,
    /// See [`Run::skips`].
    skips: Vec<std::ops::Range<usize>>,
    /// See [`Run::answered`].
    answered: Vec<String>,
    /// The names a type reads as bound here: the type parameters and the associated
    /// types of the declaration running, which a hole inherits.
    scope: Vec<&'t str>,
    /// How many of `scope` are type parameters: the rest are associated types.
    params: usize,
    /// The tokens an `@e` split: they stay split when the `@e` fails, as the first
    /// reading of `sort<Asc<Int64>= 1` leaves an `=` behind it.
    kept_splits: Vec<u32>,
    /// The capability and the name of the method running.
    receiver: Capability,
    method: &'t str,
    /// Whether the method running declared a capability result.
    projection: bool,
    /// The contract running and its members so far.
    contract: &'t str,
    members: Vec<Member<'t>>,
    /// Whether the file has a `logging` block already.
    saw_logging: bool,
}

/// The token a split `>>` leaves.
static GT: Tok = Tok::Gt;
/// The token a split `>=` leaves.
static EQ: Tok = Tok::Eq;

impl<'t> Recognizer<'t> {
    fn over(toks: &'t [Token], cx: Cx<'t>, scope: &[&'t str]) -> Self {
        // Every recovery loop ends at `Eof`, so a slice without it would never end.
        assert!(
            matches!(toks.last(), Some(t) if t.tok == Tok::Eof),
            "the tokens end in Eof"
        );
        Recognizer {
            toks,
            cx,
            cur: Cursor {
                at: 0,
                split: false,
            },
            flags: rules::Flags::default(),
            depth: 0,
            looking: 0,
            quiet: 0,
            refusal: None,
            decl_errors: Vec::new(),
            errors: Vec::new(),
            events: Vec::with_capacity(toks.len()),
            holes: Vec::new(),
            keep_words: false,
            keep_nodes: false,
            nodes: Vec::new(),
            dropped_words: Vec::new(),
            skips: Vec::new(),
            answered: Vec::new(),
            scope: scope.to_vec(),
            params: scope.len(),
            kept_splits: Vec::new(),
            receiver: Capability::Read,
            method: "",
            projection: false,
            contract: "",
            members: Vec::new(),
            saw_logging: false,
        }
    }

    fn view(&self) -> View<'_> {
        View {
            toks: self.toks,
            events: &self.events,
            holes: &self.holes,
        }
    }

    fn sorted_refusals(&mut self) -> Vec<Diagnostic> {
        let mut errors = std::mem::take(&mut self.decl_errors);
        errors.extend(self.refusal.take().map(|d| *d));
        errors.append(&mut self.errors);
        // The sort is stable: two refusals at one token stay in the order above.
        errors.sort_by_key(|d| (d.line, d.col));
        errors
    }

    #[inline]
    fn token(&self) -> &'t Token {
        &self.toks[self.cur.at as usize]
    }

    /// The token at the cursor; what is left of it after a split.
    #[inline]
    fn peek(&self) -> &'t Tok {
        let tok = &self.token().tok;
        match (self.cur.split, tok) {
            (false, _) => tok,
            (true, Tok::GtEq) => &EQ,
            (true, _) => &GT,
        }
    }

    /// Moves past the token at the cursor. At `Eof` the cursor stays.
    #[inline]
    fn bump(&mut self) {
        let at = (self.cur.at + 1).min(self.toks.len() as u32 - 1);
        let split = !self.kept_splits.is_empty() && self.kept_splits.contains(&at);
        self.cur = Cursor { at, split };
    }

    #[inline]
    fn record(&mut self, event: Event) {
        if self.looking == 0 {
            self.events.push(event);
        }
    }

    /// Consumes the token at the cursor and records it.
    #[inline]
    fn take(&mut self) {
        self.record(match self.cur.split {
            true => Event::Rest(self.cur.at),
            false => Event::Token(self.cur.at),
        });
        self.bump();
    }

    /// Consumes the unit token `want`.
    #[inline]
    fn eat(&mut self, want: &Tok) -> bool {
        let hit = std::mem::discriminant(self.peek()) == std::mem::discriminant(want);
        if hit {
            self.take();
        }
        hit
    }

    #[inline]
    fn eat_if(&mut self, hit: bool) -> bool {
        if hit {
            self.take();
        }
        hit
    }

    /// Consumes the identifier here as a word, if `hit`.
    #[inline]
    fn word_if(&mut self, hit: bool) -> bool {
        if hit {
            self.record(Event::Word(self.cur.at));
            self.bump();
        }
        hit
    }

    /// Consumes the identifier `word` and records it as a word.
    #[inline]
    fn word(&mut self, word: &str) -> bool {
        let hit = matches!(self.peek(), Tok::Ident(name) if name == word);
        if hit {
            self.record(Event::Word(self.cur.at));
            self.bump();
        }
        hit
    }

    /// Opens a rule's node and returns its event index.
    #[inline]
    fn open(&mut self, kind: Kind) -> u32 {
        let at = self.events.len() as u32;
        self.record(Event::Open(kind, self.cur.at));
        at
    }

    /// Closes the node `open` opened, or drops it where its rule did not match.
    #[inline]
    fn close(&mut self, open: u32, hit: bool) {
        if !hit {
            self.cut_events(open as usize);
        } else {
            self.record(Event::Close(open));
        }
    }

    /// The event index the next event takes.
    #[inline]
    fn mark(&self) -> u32 {
        self.events.len() as u32
    }

    /// Skips the rule `hook` opens without reading it, where the run skims.
    fn skipped(&mut self, hook: Hook) -> Option<R> {
        let skip = match hook {
            Hook::SkimBody => self.cx.skim,
            Hook::TestBody => self.cx.imported,
            _ => false,
        };
        skip.then(|| {
            let start = self.cur;
            let hit = self.skip_braces();
            if matches!(hit, Ok(true)) {
                self.node(hook, start);
            }
            hit
        })
    }

    /// Records for a reader that the rule of `hook` matched from `start` to the cursor.
    fn node(&mut self, hook: Hook, start: Cursor) {
        if self.keep_nodes && self.looking == 0 {
            self.nodes.push(Spanned {
                hook,
                from: start.at,
                to: self.cur.at,
                split: self.cur.split,
                events: self.events.len() as u32,
            });
        }
    }

    /// Moves past one `{ .. }` group without reading it.
    fn skip_braces(&mut self) -> R {
        if !matches!(self.peek(), Tok::LBrace) {
            return Ok(false);
        }
        let start = self.cur.at as usize;
        self.bump();
        let mut depth = 1usize;
        while depth > 0 {
            match self.peek() {
                Tok::LBrace => depth += 1,
                Tok::RBrace => depth -= 1,
                // The refusal an unclosed block's parse states.
                Tok::Eof => return self.expected("RBrace"),
                _ => {}
            }
            self.bump();
        }
        self.record(Event::Skipped(self.skips.len() as u32));
        self.skips.push(start..self.cur.at as usize);
        Ok(true)
    }

    /// Reads the rule `hook` that matched from `start`, with its events from `open`,
    /// and raises the rows that rule holds.
    fn closed(&mut self, hook: Hook, open: u32, start: Cursor) -> Result<(), Refused> {
        self.node(hook, start);
        let toks = self.toks;
        let quote = self.flags.quote;
        // A word that holds a hole names nothing yet, so it matches no other word.
        let name = |at: u32| match &toks[at as usize].tok {
            Tok::Ident(name) if !(quote && name.contains('\0')) => name.as_str(),
            _ => "",
        };
        let first = &toks[start.at as usize];
        let open = open as usize;
        match hook {
            Hook::TName => {
                self.bind(name(start.at));
                self.params = self.scope.len();
            }
            Hook::IHead => {
                let aname = name(start.at + 1);
                if self.scope[..self.params].contains(&aname) {
                    let at = (first.line, first.col);
                    self.refuse(at, |_| rule!(ImplAssocTypeClash, aname))?;
                }
            }
            Hook::IBind => self.bind(name(start.at + 1)),
            Hook::TypeName => {
                self.flags.scoped = self.scope.contains(&name(start.at));
                if self.flags.scoped && self.looking == 0 {
                    self.events[open] = Event::Bound(start.at);
                }
            }
            Hook::Recv => {
                self.method = name(start.at.saturating_sub(2));
                self.receiver = match self.cur == start {
                    true => Capability::Read,
                    false => Capability::from_word(name(start.at)).unwrap_or(Capability::Read),
                };
            }
            Hook::RCap => {
                let rc = Capability::from_word(name(start.at)).unwrap_or(Capability::Read);
                // A projection's capabilities must agree: `-> read T` needs `read self` and
                // `-> modify T` needs `modify self`. `consume self` pairs with neither,
                // because the receiver must outlive the access.
                if self.receiver != rc {
                    let (at, method, want) = ((first.line, first.col), self.method, rc.word());
                    self.refuse(at, |_| rule!(ProjectionReceiver, name = method, want))?;
                }
                self.projection = true;
            }
            Hook::IMethod => {
                self.flags.methods |= !self.projection;
                self.projection = false;
            }
            Hook::StmtExpr => {
                self.flags.place = false;
                if matches!(self.peek(), Tok::Eq) {
                    let shape = parser::store_shape(self.view(), open, self.cx.answers);
                    let at = (first.line, self.here().1);
                    match shape {
                        StoreShape::Place => self.flags.place = true,
                        StoreShape::OverIndex => {
                            self.refuse(at, |_| Rule::IndexAssignTarget {})?;
                        }
                        StoreShape::OverField => {
                            self.refuse(at, |_| Rule::FieldAssignTarget {})?;
                        }
                        StoreShape::Value => {}
                    }
                }
            }
            Hook::PlainStmt => {
                let Some(&Event::Close(expr)) = self.events.get(open.wrapping_sub(1)) else {
                    return Ok(());
                };
                let semi = matches!(self.events.get(open), Some(Event::Token(_)));
                let stop = self.cur.at as usize - semi as usize;
                let expr = expr as usize;
                let Event::Open(_, from) = self.events[expr] else {
                    unreachable!("a Close names its Open")
                };
                if self.names_a_rebuild(stop, from as usize)
                    && !parser::push_has_place(self.view(), expr, self.cx.answers)
                {
                    let at = (toks[from as usize].line, self.here().1);
                    self.refuse(at, |_| Rule::PushNoPlace {})?;
                }
            }
            Hook::AwaitTail => {
                if !parser::await_marks(self.view(), open, self.cx.answers) {
                    self.refuse((first.line, first.col), |_| Rule::AwaitNotCall {})?;
                }
            }
            Hook::RefLet => {
                let (variant, binds, scrut) =
                    parser::ref_let_parts(self.view(), open, self.cx.answers);
                let at = (toks[start.at as usize - 1].line, first.col);
                if !matches!(scrut, crate::ast::Expr::Var { .. }) {
                    self.refuse(at, |_| rule!(LetPatternScrutinee, variant))?;
                }
                if binds.is_empty() {
                    self.refuse(at, |_| rule!(LetEmptyVariant, variant))?;
                }
            }
            Hook::CName => {
                self.contract = name(start.at);
                self.members.clear();
            }
            Hook::CLet => self.member(name(start.at + 1), true, first.line)?,
            Hook::CFn => {
                let member = match toks[start.at as usize + 1].tok {
                    Tok::Star => OPEN_RULE_NAME,
                    _ => name(start.at + 1),
                };
                self.member(member, false, first.line)?;
            }
            Hook::TypeDecl => {
                let wheres = self.events[open..]
                    .iter()
                    .any(|e| matches!(e, Event::Token(i) if toks[*i as usize].tok == Tok::Where));
                if wheres {
                    let view = self.view();
                    if let Some(name) =
                        parser::inline_where_outside_record(view, open, self.cx.answers)
                    {
                        let at = (first.line, first.col);
                        self.refuse(at, |_| rule!(InlineWhereBase, name))?;
                    }
                }
            }
            Hook::LogHead => {
                if std::mem::replace(&mut self.saw_logging, true) {
                    // The refused word is the declaration's first token: recovery skips it.
                    self.cur = start;
                    self.refuse((first.line, first.col), |_| Rule::DuplicateLogging {})?;
                }
            }
            Hook::DeclName => self.answers_to(name(start.at)),
            Hook::ImportName => {
                let local = match self.cur.at - start.at {
                    1 => start.at,
                    _ => start.at + 2,
                };
                self.answers_to(name(local));
            }
            Hook::SkimBody | Hook::TestBody => {}
            Hook::Module
            | Hook::Item
            | Hook::Export
            | Hook::GenFn
            | Hook::MutFn
            | Hook::Import
            | Hook::Source
            | Hook::FnDecl
            | Hook::Params
            | Hook::Param
            | Hook::Ret
            | Hook::Type
            | Hook::Field => {}
        }
        Ok(())
    }

    /// Adds `name` to the names a type reads as bound. A word that holds a hole binds
    /// nothing.
    fn bind(&mut self, name: &'t str) {
        if !name.is_empty() {
            self.scope.push(name);
        }
    }

    /// Records that the closed declaration's `name` answers to a method-form builtin
    /// spelling, if it spells one ([`crate::prelude::Builtin::method`]): a written
    /// `recv.m(..)` with `m` among these calls the module's `m`, not the builtin.
    ///
    /// The test is scope, not type: the flat namespace is program-wide unique, so
    /// one lookup answers it. A module that declares `remove` and calls `.remove(k)`
    /// on a `Map` gets a type error at the call. Impl methods are absent: `impl Copy
    /// for T { fn copy(..) }` overrides `@copy` for receivers of type `T` only, and
    /// counting it would take the builtin from every other receiver in the module.
    fn answers_to(&mut self, name: &str) {
        if crate::prelude::method_builtin(name).is_some() {
            self.answered.push(name.to_string());
        }
    }

    /// Whether the expression ending before token `stop` ends in a call of a name that
    /// could write an array back from a receiver the tree must read. The tree's own
    /// check runs only then: `x.push(v)` on a lone name has its place.
    fn names_a_rebuild(&self, stop: usize, first: usize) -> bool {
        let toks = &self.toks[..stop];
        if !matches!(toks.last(), Some(t) if t.tok == Tok::RParen) {
            return false;
        }
        let mut depth = 0usize;
        for at in (0..toks.len()).rev() {
            match toks[at].tok {
                Tok::RParen => depth += 1,
                Tok::LParen => {
                    depth -= 1;
                    if depth == 0 {
                        let lone_name = at == first + 3 && matches!(toks[first].tok, Tok::Ident(_));
                        return !lone_name
                            && matches!(at.checked_sub(1).map(|i| &toks[i].tok),
                            Some(Tok::Ident(name)) if parser::may_rebuild(name, self.cx.answers));
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// A name may repeat in a contract: the repeats are alternative signatures, and a
    /// module matches any one (a page's `head` varies). Refused: a second open rule,
    /// and one name as both a `let` and a `fn` member.
    fn member(&mut self, member: &'t str, value: bool, line: usize) -> Result<(), Refused> {
        if member.is_empty() {
            return Ok(());
        }
        let at = (line, self.here().1);
        let name = self.contract;
        if let Some(prev) = self.members.iter().find(|m| m.name == member) {
            let (prev_line, prev_value) = (prev.line, prev.value);
            if member == OPEN_RULE_NAME {
                self.refuse(at, |_| rule!(ContractOpenRuleTwice, name, line = prev_line))?;
            }
            if prev_value != value {
                self.refuse(at, |_| {
                    rule!(ContractMemberFormChange, name, member, line = prev_line)
                })?;
            }
            if value {
                self.refuse(at, |_| {
                    rule!(ContractMemberTwice, name, member, line = prev_line)
                })?;
            }
        }
        self.members.push(Member {
            name: member,
            value,
            line,
        });
        Ok(())
    }

    /// `splice`: passes a hole that stands for items of its list. It records nothing,
    /// so a tree a row reads holds none of them.
    fn splice(&mut self) -> bool {
        let hit = matches!(self.peek(), Tok::Ident(w) if w == "\0");
        if hit {
            self.bump();
        }
        hit
    }

    /// `split '>'`: takes one `>`. A `>>` leaves a `>` and a `>=` leaves an `=`.
    fn close_gt(&mut self) -> bool {
        match self.peek() {
            Tok::Gt => self.take(),
            Tok::Shr | Tok::GtEq => {
                self.record(Event::Token(self.cur.at));
                self.cur.split = true;
                if self.quiet > 0 {
                    self.kept_splits.push(self.cur.at);
                }
            }
            _ => return false,
        }
        true
    }

    /// The token `k` places on from the cursor, or `Eof` past the end.
    #[inline]
    fn ahead(&self, k: usize) -> &'t Tok {
        let last = self.toks.len() - 1;
        &self.toks[(self.cur.at as usize + k).min(last)].tok
    }

    /// Whether the token at the cursor starts on the line of the token before it.
    fn no_newline(&self) -> bool {
        self.no_newline_at(0)
    }

    /// [`Recognizer::no_newline`] for the token `k` places on.
    #[inline]
    fn no_newline_at(&self, k: usize) -> bool {
        let last = self.toks.len() - 1;
        let at = (self.cur.at as usize + k).min(last);
        at == 0 || self.toks[at - 1].line == self.toks[at].line
    }

    #[inline]
    fn save(&self) -> Save {
        Save {
            cur: self.cur,
            events: self.events.len() as u32,
        }
    }

    #[inline]
    fn back(&mut self, to: Save) {
        self.cur = to.cur;
        self.cut_events(to.events as usize);
    }

    /// Takes the events back to `len`, with a reader's nodes that closed past them.
    fn cut_events(&mut self, len: usize) {
        self.events.truncate(len);
        if self.keep_nodes {
            while self.nodes.last().is_some_and(|n| n.events as usize > len) {
                self.nodes.pop();
            }
        }
    }

    /// Whether a sequence that started at `from` has committed.
    #[inline]
    fn committed(&self, from: Save) -> bool {
        self.looking == 0 && self.cur != from.cur
    }

    fn catch(&self) -> Catch {
        Catch {
            save: self.save(),
            flags: self.flags,
            depth: self.depth,
        }
    }

    /// Restores what `?` unwound past. The cursor stays for the caller to decide.
    fn uncatch(&mut self, c: Catch) {
        self.flags = c.flags;
        self.depth = c.depth;
    }

    /// `&e`: whether `e` matches here, moving nothing.
    #[inline]
    fn look(&mut self, e: impl FnOnce(&mut Self) -> R) -> bool {
        let c = self.catch();
        self.looking += 1;
        self.quiet += 1;
        let hit = matches!(e(self), Ok(true));
        self.looking -= 1;
        self.quiet -= 1;
        self.uncatch(c);
        self.back(c.save);
        hit
    }

    /// `@e`: `e`, or no match if it refuses.
    fn attempt(&mut self, e: impl FnOnce(&mut Self) -> R) -> bool {
        let c = self.catch();
        self.quiet += 1;
        let hit = matches!(e(self), Ok(true));
        self.quiet -= 1;
        self.uncatch(c);
        if !hit {
            self.back(c.save);
        }
        hit
    }

    /// `many item until until recover ..`: a refusal of an item is recorded and the
    /// loop resumes at the next statement, or with `decl` at the next token that
    /// starts a declaration outside braces. Each round moves the cursor or the loop
    /// stops: an item that matches empty ends it, and a recovery skips a token. A
    /// refused item's events go.
    fn many_recovering(
        &mut self,
        until: impl Fn(&mut Self) -> bool,
        item: impl Fn(&mut Self) -> R,
        miss: impl Fn(&mut Self) -> R,
        decl: Option<impl Fn(&mut Self) -> R>,
    ) -> R {
        loop {
            if until(self) {
                return Ok(true);
            }
            if decl.is_some() {
                self.scope.clear();
                self.params = 0;
                self.projection = false;
            }
            let c = self.catch();
            let answered = self.answered.len();
            // This site records the refusal, even under an `@e` that discards the
            // refusal around it. The cursor of the `@e` moves back; the record stays.
            let quiet = self.quiet;
            if self.looking == 0 {
                self.quiet = 0;
            }
            let result = item(self);
            let result = match result {
                Ok(false) => miss(self),
                other => other,
            };
            self.quiet = quiet;
            match result {
                Ok(true) if self.cur == c.save.cur => return Ok(true),
                Ok(true) => {
                    if decl.is_some() {
                        self.record(Event::Item);
                    }
                    continue;
                }
                _ => {}
            }
            self.uncatch(c);
            self.drop_events(c.save.events as usize);
            self.answered.truncate(answered);
            let refusal = self.refusal.take().map(|d| *d);
            match &decl {
                Some(starts) => {
                    self.decl_errors.extend(refusal);
                    self.sync_to_decl(starts);
                }
                None => {
                    self.errors.extend(refusal);
                    self.sync_to_stmt(c.save.cur);
                }
            }
            if self.cur <= c.save.cur {
                return Ok(true);
            }
        }
    }

    /// Drops a refused item's events from `from` on, keeping its words for a reader
    /// that colours them.
    fn drop_events(&mut self, from: usize) {
        if self.keep_words {
            let words = self.events[from..].iter().filter_map(|e| match e {
                Event::Word(i) => Some(*i as usize),
                _ => None,
            });
            self.dropped_words.extend(words);
        }
        self.cut_events(from);
    }

    /// Skips one token, then to the next token at brace depth 0 that `starts`
    /// matches, or to `Eof`. Each round moves one token toward `Eof`.
    fn sync_to_decl(&mut self, starts: &impl Fn(&mut Self) -> R) {
        self.bump();
        let mut depth = 0u32;
        loop {
            match self.peek() {
                Tok::Eof => return,
                _ if depth == 0 && self.look(starts) => return,
                Tok::LBrace => depth += 1,
                Tok::RBrace => depth = depth.saturating_sub(1),
                _ => {}
            }
            self.bump();
        }
    }

    /// Skips a failed statement's tokens to a token on a new line at this block's
    /// brace depth, past a `;` at that depth, to the block's `}`, or to `Eof`. A
    /// statement that consumed nothing loses one token first. Each round moves one
    /// token toward `Eof`.
    fn sync_to_stmt(&mut self, start: Cursor) {
        if self.cur == start {
            self.bump();
        }
        let mut depth = 0u32;
        loop {
            match self.peek() {
                Tok::Eof => return,
                Tok::LBrace => depth += 1,
                Tok::RBrace if depth == 0 => return,
                Tok::RBrace => depth -= 1,
                Tok::Semi if depth == 0 => {
                    self.bump();
                    return;
                }
                _ if depth == 0 && !self.no_newline() => return,
                _ => {}
            }
            self.bump();
        }
    }

    /// The line and column of the cursor. A split token stands one column on.
    fn here(&self) -> (usize, usize) {
        let t = self.token();
        (t.line, t.col + self.cur.split as usize)
    }

    fn line_of(&self, c: Capture) -> usize {
        self.toks[c.from.at as usize].line
    }

    fn col_of(&self, c: Capture) -> usize {
        self.toks[c.from.at as usize].col
    }

    /// What a label matched, as a refusal prints it: a name, or the type it spells.
    fn text(&self, c: Capture) -> String {
        match &self.toks[c.from.at as usize..c.to.at as usize] {
            [Token {
                tok: Tok::Ident(name),
                ..
            }] => name.clone(),
            toks => parser::type_text(toks, self.cx.answers),
        }
    }

    /// What the token here prints as in a refusal.
    fn found(&self) -> String {
        format!("{:?}", self.peek())
    }

    /// Refuses with `rule` at `at`, a line and a column.
    fn refuse(&mut self, at: (usize, usize), rule: impl FnOnce(&Self) -> Rule) -> R {
        if self.quiet == 0 {
            let d = Diagnostic::refusal(at.0, at.1, "parse", rule(self));
            self.refusal = Some(Box::new(d));
        }
        Err(Refused)
    }

    /// `^Row!`: refuses as [`Recognizer::refuse`] does, with the token here past the
    /// cursor.
    fn refuse_past(&mut self, at: (usize, usize), rule: impl FnOnce(&Self) -> Rule) -> R {
        let refused = self.refuse(at, rule);
        self.bump();
        refused
    }

    /// `~^Row`: records the refusal, skips one token, and goes on.
    fn skip(&mut self, at: (usize, usize), rule: impl FnOnce(&Self) -> Rule) {
        if self.quiet == 0 {
            let d = Diagnostic::refusal(at.0, at.1, "parse", rule(self));
            self.decl_errors.push(d);
        }
        self.bump();
    }

    /// The `string` token, with each hole read by `inner` unless a predicate is
    /// looking. A hole's refusal points at the token, or at the tag before it. The
    /// `vyrn` tag's skeleton is checked before any hole.
    fn string_token(
        &mut self,
        tagged: bool,
        inner: for<'a, 'b> fn(&'a mut Recognizer<'b>) -> R,
        wrap: fn(String) -> Rule,
        trailing: fn(&str) -> Rule,
    ) -> R {
        let tok = self.peek();
        if !matches!(tok, Tok::Str(_) | Tok::TemplateStr { .. }) {
            return Ok(false);
        }
        let at = match tagged {
            true => {
                let tag = &self.toks[self.cur.at as usize - 1];
                (tag.line, tag.col)
            }
            false => self.here(),
        };
        let quote = tagged
            && matches!(&self.toks[self.cur.at as usize - 1].tok, Tok::Ident(t) if t == "vyrn");
        self.take();
        if self.looking > 0 {
            return Ok(true);
        }
        let (parts, exprs): (&[String], &[Hole]) = match tok {
            Tok::Str(s) => (std::slice::from_ref(s), &[]),
            Tok::TemplateStr { parts, exprs } => (parts, exprs),
            _ => unreachable!("matched above"),
        };
        if quote {
            if let Some((rule, at)) = parser::skeleton_refusal(parts, at, self.cx.answers) {
                return self.refuse(at, |_| rule);
            }
        }
        for hole in exprs {
            match self.read_hole(hole, inner, wrap, trailing) {
                Ok(run) => {
                    self.record(Event::Hole(self.holes.len() as u32));
                    self.holes.push(run);
                }
                Err(rule) => return self.refuse(at, |_| rule),
            }
        }
        Ok(true)
    }

    /// Lexes and reads one hole. Its tokens count from its own line 1, column 1, so
    /// they move to where the hole stands, and a node's line and a lambda's key are its
    /// own. Its recovered refusals are its own and go with it. An interpolation in a
    /// quote's text may hold the quote's own hole.
    fn read_hole(
        &self,
        hole: &Hole,
        inner: for<'a, 'b> fn(&'a mut Recognizer<'b>) -> R,
        wrap: fn(String) -> Rule,
        trailing: fn(&str) -> Rule,
    ) -> Result<HoleRun, Rule> {
        let lex = match self.flags.quote {
            true => lexer::lex_quote,
            false => lexer::lex,
        };
        let mut toks = lex(&hole.src).map_err(|e| wrap(e.render()))?;
        for t in &mut toks {
            if t.line == 1 {
                t.col += hole.col - 1;
            }
            t.line += hole.line - 1;
        }
        let run = {
            let cx = Cx::plain(self.cx.answers);
            let mut sub = Recognizer::over(&toks, cx, &self.scope);
            sub.flags.quote = self.flags.quote;
            if inner(&mut sub).is_err() {
                let detail = sub.refusal.take().map_or_else(String::new, |d| d.message);
                return Err(wrap(detail));
            }
            if !matches!(sub.peek(), Tok::Eof) {
                return Err(trailing(hole.src.trim()));
            }
            (sub.events, sub.holes)
        };
        Ok(HoleRun {
            toks,
            events: run.0,
            holes: run.1,
        })
    }
}
