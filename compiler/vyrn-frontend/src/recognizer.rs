//! The recognizer: `vyrn.syntax`'s syntactic rules over the lexer's tokens. It
//! builds no tree.
//!
//! [`rules`] is written from the grammar, and this module is the cursor it runs
//! on. [`recognize`] reads a file and [`recognize_hole`] an interpolation hole's
//! expression. Each returns the refusals, as the parser states them, and the
//! [`Mark`]s its readers read: the `syntaxgate` test reads the declarations, and
//! [`crate::classify`] reads the words a rule consumed. The parser builds the tree.
//!
//! Protocol between the two files. A rule's method returns `Ok(false)` with the
//! cursor and the marks unmoved, or `Err` with the cursor where the refusal arose.
//! `?` unwinds past the code that restores the flags and the nesting depth, so each
//! catch site, [`Recognizer::look`], [`Recognizer::attempt`] and
//! [`Recognizer::many_recovering`], restores them. The recognizer allocates only
//! its mark and refusal buffers, a refusal's text, and the tokens of a hole.

#[rustfmt::skip]
#[allow(unused_variables, unused_parens, clippy::all)]
mod rules;

pub use rules::Decl;
use rules::Hook;

use crate::diagnostics::Diagnostic;
use crate::lexer::{self, Hole, Tok, Token};
use crate::rules::Rule;

/// What a rule returns: whether it matched. An `Err` is a refusal; the diagnostic
/// is in [`Recognizer::refusal`] unless a catch site will discard it.
type R = Result<bool, Refused>;

struct Refused;

/// What a run records, in the order its rules matched, so in source order. A mark
/// holds the index of a token of the run's slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// A rule consumed this identifier as a word: a contextual word.
    Word(usize),
    /// This token starts a declaration.
    Decl(Decl, usize),
}

/// What a run found.
pub struct Recognized {
    /// The refusals, sorted by line and column.
    pub errors: Vec<Diagnostic>,
    pub marks: Vec<Mark>,
}

/// Recognizes a file. `tokens` ends in `Eof`, as [`lexer::lex`] leaves it.
pub fn recognize(tokens: &[Token]) -> Recognized {
    run(tokens, |r| r._module())
}

/// Recognizes one expression, the whole of an interpolation hole. A token after
/// the expression is no refusal here: the file's run states it.
pub fn recognize_hole(tokens: &[Token]) -> Recognized {
    run(tokens, |r| r._expr())
}

fn run<'t>(tokens: &'t [Token], rule: impl FnOnce(&mut Recognizer<'t>) -> R) -> Recognized {
    // Every recovery loop ends at `Eof`, so a slice without it would never end.
    assert!(
        matches!(tokens.last(), Some(t) if t.tok == Tok::Eof),
        "the tokens end in Eof"
    );
    let mut r = Recognizer::over(tokens);
    let _ = rule(&mut r);
    let mut errors = r.decl_errors;
    errors.extend(r.refusal.take().map(|d| *d));
    errors.append(&mut r.errors);
    // The sort is stable: two refusals at one token stay in the order above.
    errors.sort_by_key(|d| (d.line, d.col));
    Recognized {
        errors,
        marks: r.marks,
    }
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
    marks: u32,
}

/// What a catch site restores: the state `?` unwinds past, too.
#[derive(Clone, Copy)]
struct Catch {
    save: Save,
    flags: rules::Flags,
    depth: u32,
}

struct Recognizer<'t> {
    toks: &'t [Token],
    cur: Cursor,
    flags: rules::Flags,
    /// The `nest` rules running.
    depth: u32,
    /// The predicates running. Inside one a sequence never commits and no token's
    /// holes are parsed.
    looking: u32,
    /// The catch sites running that discard a refusal, so none is built.
    quiet: u32,
    /// The refusal `Err` carries, when `quiet` is 0.
    refusal: Option<Box<Diagnostic>>,
    /// The refusals a declaration's recovery recorded. They sort before `errors`
    /// at one token, as the two lists of the parser do.
    decl_errors: Vec<Diagnostic>,
    /// The refusals a statement's recovery recorded.
    errors: Vec<Diagnostic>,
    marks: Vec<Mark>,
    /// The names a type reads as bound here: the type parameters and the associated
    /// types of the declaration running, which a hole inherits.
    scope: Vec<&'t str>,
    /// The tokens an `@e` split: they stay split when the `@e` fails, as the parser
    /// splits a token in place and rewinds only its cursor.
    kept_splits: Vec<u32>,
    /// Whether the method running declared a capability result.
    projection: bool,
}

/// The token a split `>>` leaves.
static GT: Tok = Tok::Gt;
/// The token a split `>=` leaves.
static EQ: Tok = Tok::Eq;

impl<'t> Recognizer<'t> {
    fn over(toks: &'t [Token]) -> Self {
        Recognizer {
            toks,
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
            marks: Vec::new(),
            scope: Vec::new(),
            kept_splits: Vec::new(),
            projection: false,
        }
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

    /// Consumes the unit token `want`.
    #[inline]
    fn eat(&mut self, want: &Tok) -> bool {
        let hit = std::mem::discriminant(self.peek()) == std::mem::discriminant(want);
        if hit {
            self.bump();
        }
        hit
    }

    #[inline]
    fn eat_if(&mut self, hit: bool) -> bool {
        if hit {
            self.bump();
        }
        hit
    }

    /// Consumes the identifier `word` and marks it.
    #[inline]
    fn word(&mut self, word: &str) -> bool {
        let hit = matches!(self.peek(), Tok::Ident(name) if name == word);
        if hit {
            if self.looking == 0 {
                self.marks.push(Mark::Word(self.cur.at as usize));
            }
            self.bump();
        }
        hit
    }

    fn decl(&mut self, decl: Decl, start: Cursor) {
        self.marks.push(Mark::Decl(decl, start.at as usize));
    }

    /// Reads the rule `hook` that matched from `start`.
    fn closed(&mut self, hook: Hook, start: Cursor) {
        let name = |at: u32| match &self.toks[at as usize].tok {
            Tok::Ident(name) => Some(name.as_str()),
            _ => None,
        };
        match hook {
            Hook::TName => self.scope.extend(name(start.at)),
            Hook::IBind => self.scope.extend(name(start.at + 1)),
            Hook::TypeName => {
                self.flags.scoped = name(start.at).is_some_and(|n| self.scope.contains(&n));
            }
            Hook::RCap => self.projection = true,
            Hook::IMethod => {
                self.flags.methods |= !self.projection;
                self.projection = false;
            }
            Hook::StmtExpr => {
                self.flags.place = matches!(self.peek(), Tok::Eq)
                    && crate::parser::is_store_target(
                        &self.toks[start.at as usize..self.cur.at as usize],
                    );
            }
        }
    }

    /// `split '>'`: takes one `>`. A `>>` leaves a `>` and a `>=` leaves an `=`, as
    /// [`crate::parser`] does closing a generic list.
    fn close_gt(&mut self) -> bool {
        match self.peek() {
            Tok::Gt => self.bump(),
            Tok::Shr | Tok::GtEq => {
                self.cur.split = true;
                if self.quiet > 0 {
                    self.kept_splits.push(self.cur.at);
                }
            }
            _ => return false,
        }
        true
    }

    /// Whether the token at the cursor starts on the line of the token before it.
    fn no_newline(&self) -> bool {
        let at = self.cur.at as usize;
        at == 0 || self.toks[at - 1].line == self.toks[at].line
    }

    #[inline]
    fn save(&self) -> Save {
        Save {
            cur: self.cur,
            marks: self.marks.len() as u32,
        }
    }

    #[inline]
    fn back(&mut self, to: Save) {
        self.cur = to.cur;
        self.marks.truncate(to.marks as usize);
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
    /// stops: an item that matches empty ends it, and a recovery skips a token.
    fn many_recovering(
        &mut self,
        until: impl Fn(&mut Self) -> R,
        item: impl Fn(&mut Self) -> R,
        miss: impl Fn(&mut Self) -> R,
        decl: Option<impl Fn(&mut Self) -> R>,
    ) -> R {
        loop {
            if self.look(&until) {
                return Ok(true);
            }
            if decl.is_some() {
                self.scope.clear();
                self.projection = false;
            }
            let c = self.catch();
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
                Ok(true) => continue,
                _ => {}
            }
            self.uncatch(c);
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
            toks => crate::parser::type_text(toks),
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
    /// looking. A hole's refusal points at the token, or at the tag before it.
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
        self.bump();
        if let (Tok::TemplateStr { exprs, .. }, 0) = (tok, self.looking) {
            for hole in exprs {
                if let Err(rule) = read_hole(hole, inner, wrap, trailing) {
                    return self.refuse(at, |_| rule);
                }
            }
        }
        Ok(true)
    }
}

/// Lexes and reads one hole, as [`crate::parser`] does. Its recovered refusals are
/// its own and go with it.
fn read_hole(
    hole: &Hole,
    inner: for<'a, 'b> fn(&'a mut Recognizer<'b>) -> R,
    wrap: fn(String) -> Rule,
    trailing: fn(&str) -> Rule,
) -> Result<(), Rule> {
    let toks = lexer::lex(&hole.src).map_err(|e| wrap(e.render()))?;
    let mut sub = Recognizer::over(&toks);
    if inner(&mut sub).is_err() {
        let detail = sub.refusal.take().map_or_else(String::new, |d| d.message);
        return Err(wrap(detail));
    }
    match sub.peek() {
        Tok::Eof => Ok(()),
        _ => Err(trailing(hole.src.trim())),
    }
}
