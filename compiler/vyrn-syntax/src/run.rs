//! Interprets a [`Grammar`] over a source text.
//!
//! The run keeps one cursor, a flat list of nodes in post-order, and the
//! captures of the labelled expressions in the active rule frames. A failed
//! alternative restores all three. A committed failure is an `Err` that carries
//! its refusal up to the nearest `many .. recover`, which records it and skips
//! to its recovery point.
//!
//! Termination: every loop below either consumes a character per iteration or
//! stops, and every rule call either consumes input or descends the acyclic
//! graph of calls at one position, which the loader checks.

use std::rc::Rc;

use crate::{is_word, At, Builtin, Embed, Grammar, HoleSrc, Level, Lit, Recover, RuleId, Throw, E};

/// A refusal the grammar states.
#[derive(Debug, Clone)]
pub struct Diag {
    /// 1-based. In a [`HoleValue::Diag`], counted from the island's first line.
    pub line: usize,
    /// 1-based, in characters.
    pub col: usize,
    pub row: Rc<str>,
    pub holes: Vec<(Rc<str>, HoleValue)>,
    pub stage: Stage,
}

/// The pass that raised a [`Diag`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The `scan` rule, which reads every token before the parse starts.
    Scan,
    Parse,
}

/// What fills one hole of a [`Diag`].
#[derive(Debug, Clone)]
pub enum HoleValue {
    Text(String),
    /// The refusal an `embed` island raised, for its `wrap` row.
    Diag(Box<Diag>),
}

/// What a [`Node`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Rule(RuleId),
    /// A literal of a syntactic rule.
    Token,
    /// Text a recovery skipped.
    Error,
    /// One operator application of a `climb`.
    Binary,
}

/// One node of the tree, by its byte extent.
#[derive(Debug, Clone, Copy)]
pub struct Node {
    pub kind: NodeKind,
    pub start: u32,
    pub end: u32,
}

/// The result of [`parse`].
pub struct Parse {
    /// Every node in post-order: a node follows its children.
    pub nodes: Vec<Node>,
    /// Every refusal, by line and then column.
    pub errors: Vec<Diag>,
    line_starts: Vec<usize>,
}

impl Parse {
    /// Returns the 1-based line of byte offset `off`.
    pub fn line(&self, off: u32) -> usize {
        self.line_starts.partition_point(|&s| s <= off as usize)
    }
}

/// Runs rule `start` over `src`. If the grammar has a `scan` rule, it runs
/// first over the whole text and its first refusal is the only one, as a lexer
/// error stops a file before its parse. `found` spells the token text it is
/// given as the host's diagnostics do; it gets `""` at the end of the text.
pub fn parse(g: &Grammar, start: RuleId, src: &str, found: &dyn Fn(&str) -> String) -> Parse {
    let mut run = Run::new(g, src, found);
    let mut errors = Vec::new();
    if let Some(scan) = g.scan_rule {
        run.mode = Mode::Scan;
        run.build = false;
        if let Err(d) = run.call(scan, false) {
            errors.push(d);
        }
        run.mode = Mode::Parse;
        run.build = true;
        run.pos = 0;
        run.trivia_memo = None;
    }
    if errors.is_empty() {
        match run.call(start, false) {
            Err(d) => errors.push(d),
            Ok(true) => {}
            Ok(false) => errors.push(run.err_of(&E::Ref(start), false)),
        }
        errors.append(&mut run.errors);
    }
    errors.sort_by_key(|d| (d.line, d.col));
    Parse {
        nodes: run.nodes,
        errors,
        line_starts: run.line_starts,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Match extents only: an `embed` does not parse its island.
    Scan,
    Parse,
}

struct Run<'g, 'h> {
    g: &'g Grammar,
    src: &'h str,
    b: &'h [u8],
    pos: usize,
    /// The end of the text the run may read: the island's inside an `embed`.
    end: usize,
    mode: Mode,
    /// Whether matches push nodes.
    build: bool,
    nodes: Vec<Node>,
    flags: Vec<bool>,
    /// Labelled captures of the active rule frames: frame, label, start, end.
    caps: Vec<(u32, Rc<str>, usize, usize)>,
    frame: u32,
    errors: Vec<Diag>,
    found: &'h dyn Fn(&str) -> String,
    /// The last trivia skip: from, to.
    trivia_memo: Option<(usize, usize)>,
    depth: u32,
    line_starts: Vec<usize>,
    /// Inside a predicate a sequence never commits.
    lookahead: u32,
}

type R = Result<bool, Diag>;

/// What a failed alternative restores.
#[derive(Clone, Copy)]
struct St {
    pos: usize,
    nodes: usize,
    caps: usize,
}

impl<'g, 'h> Run<'g, 'h> {
    fn new(g: &'g Grammar, src: &'h str, found: &'h dyn Fn(&str) -> String) -> Self {
        let line_starts = std::iter::once(0)
            .chain(
                src.bytes()
                    .enumerate()
                    .filter(|b| b.1 == b'\n')
                    .map(|b| b.0 + 1),
            )
            .collect();
        Run {
            g,
            src,
            b: src.as_bytes(),
            pos: 0,
            end: src.len(),
            mode: Mode::Parse,
            build: true,
            nodes: Vec::new(),
            flags: vec![false; g.flags.len()],
            caps: Vec::new(),
            frame: 0,
            errors: Vec::new(),
            found,
            trivia_memo: None,
            depth: 0,
            line_starts,
            lookahead: 0,
        }
    }

    fn line_col(&self, off: usize) -> (usize, usize) {
        // `line_starts[0]` is 0, so the line index is at least 0.
        let line = self.line_starts.partition_point(|&s| s <= off) - 1;
        let col = self.src[self.line_starts[line]..off].chars().count() + 1;
        (line + 1, col)
    }

    fn save(&self) -> St {
        St {
            pos: self.pos,
            nodes: self.nodes.len(),
            caps: self.caps.len(),
        }
    }

    fn restore(&mut self, s: St) {
        self.pos = s.pos;
        self.nodes.truncate(s.nodes);
        self.caps.truncate(s.caps);
    }

    fn ch(&self) -> Option<char> {
        self.src[self.pos..self.end].chars().next()
    }

    fn skip_trivia(&mut self) {
        if let Some((from, to)) = self.trivia_memo {
            if from == self.pos {
                self.pos = to;
                return;
            }
        }
        let from = self.pos;
        if let Some(t) = self.g.trivia {
            let st = self.save();
            let b = std::mem::replace(&mut self.build, false);
            // Trivia is lexical and throws nothing a grammar would want reported.
            let _ = self.eval(&self.g.body(t).body, false);
            self.build = b;
            let to = self.pos;
            self.restore(st);
            self.pos = to;
        }
        self.trivia_memo = Some((from, self.pos));
    }

    fn push_node(&mut self, kind: NodeKind, start: usize) {
        if self.build {
            self.nodes.push(Node {
                kind,
                start: start as u32,
                end: self.pos as u32,
            });
        }
    }

    /// Runs `body` with its captures scoped to a new rule frame.
    fn framed(&mut self, body: &E, syn: bool) -> R {
        self.frame += 1;
        let res = self.eval(body, syn);
        self.frame -= 1;
        let frame = self.frame;
        self.caps.retain(|c| c.0 <= frame);
        res
    }

    fn call(&mut self, r: RuleId, from_syntactic: bool) -> R {
        let st = self.save();
        let res = self.call_inner(r, from_syntactic);
        if let Ok(false) = res {
            self.restore(st);
        }
        res
    }

    fn call_inner(&mut self, r: RuleId, from_syntactic: bool) -> R {
        let g = self.g;
        let rule = g.body(r);
        let syn = rule.syntactic;
        if from_syntactic && !syn {
            // A token: skip trivia, match characters, make one node.
            self.skip_trivia();
            let start = self.pos;
            // Inside a token only an `embed` builds nodes.
            let b = std::mem::replace(&mut self.build, false);
            let res = self.framed(&rule.body, false);
            self.build = b;
            match res {
                Ok(true) => self.push_node(NodeKind::Rule(r), start),
                // The parser sees a failed token whole: its cursor moves past the
                // token's extent, as the hand parser's does past a token it read.
                Err(_) if self.mode == Mode::Parse => {
                    self.mode = Mode::Scan;
                    self.pos = start;
                    let b = std::mem::replace(&mut self.build, false);
                    if !matches!(self.eval(&rule.body, false), Ok(true)) {
                        self.pos = start;
                    }
                    self.build = b;
                    self.mode = Mode::Parse;
                }
                _ => {}
            }
            return res;
        }
        if syn {
            self.skip_trivia();
        }
        let start = self.pos;
        let nest = match (&g.nest_limit, rule.nest) {
            (Some((max, t)), true) => {
                if self.depth >= *max {
                    return Err(self.throw_diag(t));
                }
                self.depth += 1;
                true
            }
            _ => false,
        };
        let res = self.framed(&rule.body, syn);
        if nest {
            self.depth -= 1;
        }
        if let Ok(true) = res {
            self.push_node(NodeKind::Rule(r), start);
        }
        res
    }

    fn lit_at(&self, l: &Lit, syn: bool) -> bool {
        let rest = &self.b[self.pos..self.end];
        if !rest.starts_with(l.s.as_bytes()) {
            return false;
        }
        if !syn {
            return true;
        }
        let after = &rest[l.s.len()..];
        match l.word {
            true => !self.src[self.pos + l.s.len()..self.end].starts_with(is_word),
            false => !l
                .longer
                .iter()
                .any(|t| after.starts_with(&t.as_bytes()[l.s.len()..])),
        }
    }

    fn match_lit(&mut self, l: &Lit, syn: bool) -> bool {
        let p0 = self.pos;
        if syn {
            self.skip_trivia();
        }
        if !self.lit_at(l, syn) {
            self.pos = p0;
            return false;
        }
        let st = self.pos;
        self.pos += l.s.len();
        if syn {
            self.push_node(NodeKind::Token, st);
        }
        true
    }

    fn here(&mut self, syn: bool) -> usize {
        if syn {
            self.skip_trivia();
        }
        self.pos
    }

    /// The end of the token at `p` by the grammar's `token` rule, or of one
    /// character when no token matches there.
    fn token_end(&mut self, p: usize) -> usize {
        let st = self.save();
        self.pos = p;
        let b = std::mem::replace(&mut self.build, false);
        let mode = std::mem::replace(&mut self.mode, Mode::Scan);
        let ok = match self.g.token_rule {
            Some(t) => matches!(self.eval(&self.g.body(t).body, false), Ok(true)),
            None => false,
        };
        let q = self.pos;
        self.mode = mode;
        self.build = b;
        self.restore(st);
        match ok && q > p {
            true => q,
            false => {
                p + self.src[p..self.end]
                    .chars()
                    .next()
                    .map_or(1, char::len_utf8)
            }
        }
    }

    /// The host's spelling of the token at `p`.
    fn found_at(&mut self, p: usize) -> String {
        match p >= self.end {
            true => (self.found)(""),
            false => {
                let q = self.token_end(p);
                (self.found)(&self.src[p..q])
            }
        }
    }

    /// Skips trivia and then one token, or nothing at the end of the text.
    fn skip_token(&mut self) {
        self.skip_trivia();
        if self.pos < self.end {
            self.pos = self.token_end(self.pos);
        }
    }

    fn anchor(&mut self, a: At, t: &Throw) -> usize {
        match a {
            At::Here => self.pos,
            At::Label(i) => self.capture(&t.labels[i]).map_or(self.pos, |c| c.0),
        }
    }

    /// The extent the innermost capture of `label` matched.
    fn capture(&self, label: &str) -> Option<(usize, usize)> {
        let c = self.caps.iter().rev().find(|c| &*c.1 == label)?;
        Some((c.2, c.3))
    }

    fn throw_diag(&mut self, t: &Throw) -> Diag {
        self.throw_with(t, &mut |_, _| None)
    }

    /// The refusal `t` states here. `special` fills the hole sources only some
    /// sites have: `token`, `inner` and `extent`.
    fn throw_with(
        &mut self,
        t: &Throw,
        special: &mut dyn FnMut(&mut Self, &HoleSrc) -> Option<HoleValue>,
    ) -> Diag {
        let p = self.pos;
        let lo = self.anchor(t.line, t);
        let co = self.anchor(t.col, t);
        let (line, _) = self.line_col(lo);
        let (_, col) = self.line_col(co);
        let mut holes = Vec::new();
        for (h, s) in &t.holes {
            let v = match s {
                HoleSrc::Text(x) => HoleValue::Text(x.to_string()),
                HoleSrc::Char { debug } => HoleValue::Text(match (self.ch(), debug) {
                    (Some(c), true) => format!("{c:?}"),
                    (Some(c), false) => c.to_string(),
                    (None, _) => String::new(),
                }),
                HoleSrc::Found => HoleValue::Text(self.found_at(p)),
                HoleSrc::Label(l) => {
                    let text = self.capture(l).map(|(a, b)| self.src[a..b].to_string());
                    HoleValue::Text(text.unwrap_or_default())
                }
                // The loader admits each of these only at the site that fills it.
                HoleSrc::Token | HoleSrc::Inner | HoleSrc::Extent => {
                    special(self, s).unwrap_or(HoleValue::Text(String::new()))
                }
            };
            holes.push((h.clone(), v));
        }
        Diag {
            line,
            col,
            row: t.row.clone(),
            holes,
            stage: match self.mode {
                Mode::Scan => Stage::Scan,
                Mode::Parse => Stage::Parse,
            },
        }
    }

    /// The refusal the grammar's `expect` row states for the literal `s` here.
    fn expected(&mut self, s: &str) -> Diag {
        let g = self.g;
        let name =
            (g.tokens.iter().find(|t| t.1 == s)).map_or_else(|| format!("{s:?}"), |t| t.0.clone());
        let mut fill = |_: &mut Self, src: &HoleSrc| match src {
            HoleSrc::Token => Some(HoleValue::Text(name.clone())),
            _ => None,
        };
        self.throw_with(&g.expect, &mut fill)
    }

    /// The refusal a committed failure of `e` states here: its rule's `fail`
    /// row, its own throw, or the `expect` row for its first literal.
    fn err_of(&mut self, e: &E, syn: bool) -> Diag {
        self.here(syn);
        match e {
            E::Lit(l) => self.expected(&l.s.clone()),
            E::Split(s) => self.expected(&s.clone()),
            E::Ref(r) => match &self.g.body(*r).fail {
                Some(t) => self.throw_diag(t),
                None => self.err_of(&self.g.body(*r).body, syn),
            },
            E::Alt(v) => match v.last() {
                Some(x) => self.err_of(x, syn),
                None => self.expected(""),
            },
            E::Seq(v) => {
                let required = v.iter().find(|x| {
                    !matches!(
                        x,
                        E::Star(_)
                            | E::Opt(_)
                            | E::Not(_)
                            | E::TestFlag(_)
                            | E::Builtin(Builtin::NoNl)
                    )
                });
                match required {
                    Some(x) => self.err_of(x, syn),
                    None => self.expected(""),
                }
            }
            E::Field(_, x) | E::Plus(x) | E::Try(x) | E::SetFlag(_, _, x) | E::Climb(x, _) => {
                self.err_of(x, syn)
            }
            E::And(x) | E::Guard(_, x) => self.err_of(x, syn),
            E::Throw(t) => self.throw_diag(t),
            E::Embed(em) => self.err_of(&em.extent, syn),
            // No literal to name: the `expect` row with an empty token.
            _ => self.expected(""),
        }
    }

    fn eval(&mut self, e: &E, syn: bool) -> R {
        let p0 = self.pos;
        let r = self.eval_inner(e, syn);
        if let Ok(false) = r {
            self.pos = p0;
        }
        r
    }

    fn eval_inner(&mut self, e: &E, syn: bool) -> R {
        match e {
            E::Seq(v) => {
                let st = self.save();
                let start = self.pos;
                let mut committed = false;
                for x in v {
                    let before = self.pos;
                    if !self.eval(x, syn)? {
                        if committed {
                            return Err(self.err_of(x, syn));
                        }
                        self.restore(st);
                        return Ok(false);
                    }
                    if syn && self.lookahead == 0 && self.pos > before && self.pos > start {
                        committed = true;
                    }
                }
                Ok(true)
            }
            E::Alt(v) => {
                for x in v {
                    if self.eval(x, syn)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            E::Star(x) => {
                self.repeat(x, syn)?;
                Ok(true)
            }
            E::Plus(x) => {
                if !self.eval(x, syn)? {
                    return Ok(false);
                }
                self.repeat(x, syn)?;
                Ok(true)
            }
            E::Opt(x) => {
                self.eval(x, syn)?;
                Ok(true)
            }
            E::And(x) => Ok(self.peek_match(x, syn)),
            E::Not(x) => Ok(!self.peek_match(x, syn)),
            E::Try(x) => {
                let st = self.save();
                match self.eval(x, syn) {
                    Ok(true) => Ok(true),
                    _ => {
                        self.restore(st);
                        Ok(false)
                    }
                }
            }
            E::Lit(l) => Ok(self.match_lit(l, syn)),
            E::Split(s) => {
                if syn {
                    self.skip_trivia();
                }
                if !self.b[self.pos..self.end].starts_with(s.as_bytes()) {
                    return Ok(false);
                }
                let st = self.pos;
                self.pos += s.len();
                if syn {
                    self.push_node(NodeKind::Token, st);
                }
                Ok(true)
            }
            E::Class(ranges, neg) => {
                if syn {
                    self.skip_trivia();
                }
                Ok(self.one(|c| ranges.iter().any(|&(a, b)| a <= c && c <= b) != *neg))
            }
            E::Any => {
                if syn {
                    self.skip_trivia();
                }
                Ok(self.one(|_| true))
            }
            E::Builtin(b) => Ok(self.builtin(*b, syn)),
            E::Ref(r) => self.call(*r, syn),
            E::Field(l, x) => {
                let p = self.here(syn);
                let ok = self.eval(x, syn)?;
                if ok {
                    self.caps.push((self.frame, l.clone(), p, self.pos));
                }
                Ok(ok)
            }
            E::Throw(t) => {
                if syn {
                    self.skip_trivia();
                }
                Err(self.throw_diag(t))
            }
            E::Skip(t) => {
                if syn {
                    self.skip_trivia();
                }
                let d = self.throw_diag(t);
                self.errors.push(d);
                let st = self.pos;
                self.skip_token();
                self.push_node(NodeKind::Error, st);
                Ok(true)
            }
            E::SetFlag(f, v, x) => {
                let old = std::mem::replace(&mut self.flags[*f], *v);
                let r = self.eval(x, syn);
                self.flags[*f] = old;
                r
            }
            E::TestFlag(f) => Ok(self.flags[*f]),
            E::Climb(operand, levels) => self.climb(operand, levels, 0, syn),
            E::Many(x, c, rec) => self.many(x, c, rec, syn),
            E::List(x, s, c) => {
                // Each round consumes a character or the loop stops.
                loop {
                    let p = self.pos;
                    if self.peek_match(c, syn) {
                        break;
                    }
                    if !self.eval(x, syn)? {
                        return Err(self.err_of(x, syn));
                    }
                    if !self.eval(s, syn)? || self.pos == p {
                        break;
                    }
                }
                Ok(true)
            }
            E::Embed(em) => self.embed(em, syn),
            E::Guard(set, x) => {
                if syn {
                    self.skip_trivia();
                }
                match self.b[self.pos..self.end].first() {
                    Some(&c) if set[(c >> 6) as usize] & (1 << (c & 63)) != 0 => self.eval(x, syn),
                    _ => Ok(false),
                }
            }
        }
    }

    /// Matches `x` until it fails or matches empty.
    fn repeat(&mut self, x: &E, syn: bool) -> Result<(), Diag> {
        // Each round consumes a character or the loop stops.
        loop {
            let p = self.pos;
            if !self.eval(x, syn)? || self.pos == p {
                return Ok(());
            }
        }
    }

    /// Consumes one character that satisfies `ok`.
    fn one(&mut self, ok: impl Fn(char) -> bool) -> bool {
        match self.ch() {
            Some(c) if ok(c) => {
                self.pos += c.len_utf8();
                true
            }
            _ => false,
        }
    }

    fn peek_match(&mut self, c: &E, syn: bool) -> bool {
        let st = self.save();
        let b = std::mem::replace(&mut self.build, false);
        self.lookahead += 1;
        let m = matches!(self.eval(c, syn), Ok(true));
        self.lookahead -= 1;
        self.build = b;
        self.restore(st);
        m
    }

    fn builtin(&mut self, b: Builtin, syn: bool) -> bool {
        if syn && b != Builtin::NoNl {
            self.skip_trivia();
        }
        match b {
            Builtin::Eof => self.pos >= self.end,
            Builtin::NoNl => {
                let p = self.pos;
                self.skip_trivia();
                let nl = self.src[p..self.pos].contains('\n');
                self.pos = p;
                !nl
            }
            Builtin::Alpha => self.one(char::is_alphabetic),
            Builtin::Alnum => self.one(char::is_alphanumeric),
            Builtin::Digit => self.one(|c| c.is_ascii_digit()),
            Builtin::Hex => self.one(|c| c.is_ascii_hexdigit()),
            Builtin::Ws => self.one(char::is_whitespace),
            Builtin::Reserved => {
                let rest = &self.src[self.pos..self.end];
                let n = rest.find(|c: char| !is_word(c)).unwrap_or(rest.len());
                let hit = n > 0 && self.g.reserved.contains(&rest[..n]);
                if hit {
                    self.pos += n;
                }
                hit
            }
            Builtin::Punct => {
                let rest = &self.b[self.pos..self.end];
                let best = (self.g.tokens.iter())
                    .filter(|t| !t.2 && rest.starts_with(t.1.as_bytes()))
                    .map(|t| t.1.len())
                    .max();
                if let Some(n) = best {
                    self.pos += n;
                }
                best.is_some()
            }
        }
    }

    /// Precedence climbing from level `min`; the recursion's measure is the text
    /// left, because an operator is consumed before each descent.
    fn climb(&mut self, operand: &E, levels: &[Level], min: usize, syn: bool) -> R {
        self.skip_trivia();
        let start = self.pos;
        if !self.eval(operand, syn)? {
            return Ok(false);
        }
        loop {
            self.skip_trivia();
            let hit = (levels.iter().enumerate().skip(min)).find_map(|(i, lv)| {
                lv.ops
                    .iter()
                    .find(|op| self.lit_at(op, true))
                    .map(|op| (i, op, lv.right))
            });
            let Some((i, op, right)) = hit else { break };
            let st = self.pos;
            self.pos += op.s.len();
            self.push_node(NodeKind::Token, st);
            let next = if right { i } else { i + 1 };
            if !self.climb(operand, levels, next, syn)? {
                return Err(self.err_of(operand, syn));
            }
            self.push_node(NodeKind::Binary, start);
        }
        Ok(true)
    }

    fn many(&mut self, x: &E, c: &E, rec: &Recover, syn: bool) -> R {
        // Each round consumes a character or the loop stops: an item that
        // matches empty ends it, and a recovery skips at least one token.
        loop {
            if self.peek_match(c, syn) {
                return Ok(true);
            }
            let st = self.save();
            let start = self.here(syn);
            let d = match self.eval(x, syn) {
                Ok(true) if self.pos == st.pos => return Ok(true),
                Ok(true) => continue,
                Ok(false) => self.err_of(x, syn),
                Err(d) => d,
            };
            if matches!(rec, Recover::None) {
                return Err(d);
            }
            self.recover(d, rec, st, start, syn);
            if self.pos <= st.pos {
                return Ok(true);
            }
        }
    }

    fn recover(&mut self, d: Diag, rec: &Recover, st: St, start: usize, syn: bool) {
        self.errors.push(d);
        // The cursor stays where the failure left it: skip from there.
        let at = self.pos;
        self.restore(st);
        self.pos = at.max(start);
        let mut depth = 0u32;
        match rec {
            Recover::Decl(p) => {
                self.skip_token();
                // Each round skips one token or stops at the end.
                loop {
                    self.skip_trivia();
                    if self.pos >= self.end || depth == 0 && self.peek_match(p, syn) {
                        break;
                    }
                    match self.b[self.pos] {
                        b'{' => depth += 1,
                        b'}' => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                    self.skip_token();
                }
            }
            Recover::Stmt => {
                if self.pos <= start {
                    self.skip_token();
                }
                // Each round skips one token or stops.
                loop {
                    let prev_end = self.pos;
                    self.skip_trivia();
                    if self.pos >= self.end {
                        break;
                    }
                    let nl = self.src[prev_end..self.pos].contains('\n');
                    match self.b[self.pos] {
                        b'{' => depth += 1,
                        b'}' if depth == 0 => break,
                        b'}' => depth -= 1,
                        b';' if depth == 0 => {
                            self.pos += 1;
                            break;
                        }
                        _ if depth == 0 && nl => break,
                        _ => {}
                    }
                    self.skip_token();
                }
            }
            Recover::None => {}
        }
        self.push_node(NodeKind::Error, start);
    }

    fn embed(&mut self, em: &Embed, syn: bool) -> R {
        let a = self.here(syn);
        let b0 = std::mem::replace(&mut self.build, false);
        let ok = self.eval(&em.extent, false);
        self.build = b0;
        if !ok? {
            return Ok(false);
        }
        let b = self.pos;
        if self.mode == Mode::Scan {
            return Ok(true);
        }
        let island = self.island(em.inner, a, b);
        let first_line = self.line_col(a).0;
        match island {
            Ok(Ok(())) => {
                self.pos = b;
                Ok(true)
            }
            Ok(Err(())) => {
                let text = self.src[a..b].trim().to_string();
                let mut fill = |_: &mut Self, s: &HoleSrc| match s {
                    HoleSrc::Extent => Some(HoleValue::Text(text.clone())),
                    _ => None,
                };
                match &em.trailing {
                    Some(t) => Err(self.throw_with(t, &mut fill)),
                    None => Ok(false),
                }
            }
            Err(mut d) => match &em.wrap {
                Some(w) => {
                    d.line = d.line + 1 - first_line;
                    let mut inner = Some(d);
                    let mut fill = |_: &mut Self, s: &HoleSrc| match s {
                        HoleSrc::Inner => inner.take().map(|d| HoleValue::Diag(Box::new(d))),
                        _ => None,
                    };
                    Err(self.throw_with(w, &mut fill))
                }
                None => Err(d),
            },
        }
    }

    /// Scans and then parses `src[a..b]` with rule `inner`. `Ok(Err(()))` is
    /// text the rule leaves unparsed.
    fn island(&mut self, inner: RuleId, a: usize, b: usize) -> Result<Result<(), ()>, Diag> {
        let saved_end = std::mem::replace(&mut self.end, b);
        let memo = self.trivia_memo.take();
        self.pos = a;
        let r = self.island_inner(inner, a, b);
        self.end = saved_end;
        self.trivia_memo = memo;
        r
    }

    fn island_inner(&mut self, inner: RuleId, a: usize, b: usize) -> Result<Result<(), ()>, Diag> {
        // The island lexes before it parses, as a re-lexed hole does.
        if let Some(scan) = self.g.scan_rule {
            self.mode = Mode::Scan;
            let bb = std::mem::replace(&mut self.build, false);
            let r = self.call(scan, false);
            self.build = bb;
            self.mode = Mode::Parse;
            self.pos = a;
            self.trivia_memo = None;
            r?;
        }
        // The island builds its nodes even inside a token.
        let bb = std::mem::replace(&mut self.build, true);
        let r = self.call(inner, true);
        self.build = bb;
        if !r? {
            return Err(self.err_of(&E::Ref(inner), true));
        }
        self.skip_trivia();
        Ok(match self.pos < b {
            true => Err(()),
            false => Ok(()),
        })
    }
}
