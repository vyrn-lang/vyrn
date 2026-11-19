//! Colours text by the grammar that reads it, for an editor with no TextMate engine.
//!
//! [`runs`] reads the text with the grammar's lexical rules, with full PEG
//! backtracking. A rule whose name [`scope_of`] knows colours what it matched, and
//! an inner one outranks an outer one. Outside those, a word literal is a keyword
//! and a punctuation literal that is no bracket or separator is an operator. A
//! refusal, or a form only a syntactic rule holds, ends the reading: the text after
//! it has no colour. [`naming`] is what the TextMate grammar of a `.syntax` file
//! needs beyond `syntax.syntax`, because a regex matches without the context that
//! places a head, a reference, a label or a capture.

use crate::textmate::{is_bracket_or_separator, scope_of, Naming, Scope};
use crate::{Builtin, Grammar, RuleId, E};

/// A coloured byte range of the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub start: usize,
    pub stop: usize,
    pub scope: Scope,
}

/// `syntax.syntax`, the grammar of a `.syntax` file, and its rule `file`, which reads
/// a whole one.
///
/// # Panics
///
/// Never: the test `the_format_grammar_colours_each_part` loads it and finds `file`.
pub fn format() -> (Grammar, RuleId) {
    let g = crate::load(include_str!("../syntax.syntax")).expect("syntax.syntax loads");
    let file = g.rule("file").expect("syntax.syntax has a rule file");
    (g, file)
}

/// Returns the runs of `src` as `g`'s rule `start` reads it, in source order and apart.
pub fn runs(g: &Grammar, start: RuleId, src: &str) -> Vec<Run> {
    let mut r = Reader {
        g,
        src,
        marks: Vec::new(),
    };
    let _ = r.rule(start, 0);
    let mut by_byte: Vec<Option<Scope>> = vec![None; src.len()];
    // A rule's mark follows the marks of the rules it holds, so the stable sort by
    // start, longest first, paints the outer one first. A literal's mark yields to
    // a rule's.
    r.marks
        .sort_by_key(|m| (!m.strong, m.start, std::cmp::Reverse(m.stop)));
    for m in r.marks.iter().rev().filter(|m| !m.strong) {
        by_byte[m.start..m.stop].fill(Some(m.scope));
    }
    for m in r.marks.iter().filter(|m| m.strong) {
        by_byte[m.start..m.stop].fill(Some(m.scope));
    }
    let mut out: Vec<Run> = Vec::new();
    for (i, s) in by_byte.into_iter().enumerate() {
        match (s, out.last_mut()) {
            (None, _) => {}
            (Some(s), Some(last)) if last.stop == i && last.scope == s => last.stop = i + 1,
            (Some(scope), _) => out.push(Run {
                start: i,
                stop: i + 1,
                scope,
            }),
        }
    }
    out
}

/// The TextMate naming of a `.syntax` file: a head at the start of a definition,
/// a label after `^`, a capture before `:`, then the references and the operators.
pub fn naming() -> Naming {
    const NAME: &str = "[A-Za-z_][A-Za-z0-9_.]*";
    Naming {
        before_words: vec![
            (
                format!(r"(?:^|(?<=^nest[ \t])){NAME}(?=[ \t]*=)"),
                Scope::Function,
            ),
            (format!(r"\^{NAME}"), Scope::Label),
            (format!(r"\b{NAME}(?=:)"), Scope::Parameter),
        ],
        after_words: vec![
            (format!(r"\b{NAME}"), Scope::TypeName),
            ("[/*+?!&=.~@-]".to_string(), Scope::Operator),
        ],
    }
}

/// What one reading step did.
enum Step {
    /// Matched up to this byte.
    To(usize),
    Miss,
    /// A refusal or a form this reader has no meaning for: reading ends.
    End,
}

struct Mark {
    start: usize,
    stop: usize,
    scope: Scope,
    /// From a rule the vocabulary names, not from a literal.
    strong: bool,
}

struct Reader<'g> {
    g: &'g Grammar,
    src: &'g str,
    /// The marks of what matched on the path read so far.
    marks: Vec<Mark>,
}

impl Reader<'_> {
    fn rule(&mut self, r: RuleId, at: usize) -> Step {
        let rule = self.g.body(r);
        let step = self.read(&rule.body, at);
        if let (Step::To(stop), Some(scope)) = (&step, scope_of(&rule.name)) {
            self.mark(at, *stop, scope, true);
        }
        step
    }

    fn mark(&mut self, start: usize, stop: usize, scope: Scope, strong: bool) {
        if start < stop {
            self.marks.push(Mark {
                start,
                stop,
                scope,
                strong,
            });
        }
    }

    /// Reads `e` at byte `at`. A miss takes back the marks it made. The loader
    /// refuses left recursion and a repetition stops when it does not move, so
    /// every call either consumes text or descends the acyclic rule graph.
    fn read(&mut self, e: &E, at: usize) -> Step {
        let kept = self.marks.len();
        let step = self.read_fresh(e, at);
        if matches!(step, Step::Miss) {
            self.marks.truncate(kept);
        }
        step
    }

    fn read_fresh(&mut self, e: &E, at: usize) -> Step {
        let rest = &self.src[at..];
        let one = |ok: fn(char) -> bool| match rest.chars().next() {
            Some(c) if ok(c) => Step::To(at + c.len_utf8()),
            _ => Step::Miss,
        };
        match e {
            E::Lit(l) if rest.starts_with(&*l.s) => {
                let stop = at + l.s.len();
                if l.word && l.s.len() > 1 {
                    self.mark(at, stop, Scope::Keyword, false);
                } else if l.s.bytes().all(|b| b.is_ascii_punctuation())
                    && !is_bracket_or_separator(&l.s)
                {
                    self.mark(at, stop, Scope::Operator, false);
                }
                Step::To(stop)
            }
            E::Lit(_) => Step::Miss,
            E::Class(ranges, negated) => match rest.chars().next() {
                Some(c) if ranges.iter().any(|&(a, b)| a <= c && c <= b) != *negated => {
                    Step::To(at + c.len_utf8())
                }
                _ => Step::Miss,
            },
            E::Any => one(|_| true),
            E::Builtin(Builtin::Eof) if rest.is_empty() => Step::To(at),
            E::Builtin(Builtin::Eof) => Step::Miss,
            E::Builtin(Builtin::Alpha) => one(char::is_alphabetic),
            E::Builtin(Builtin::Alnum) => one(char::is_alphanumeric),
            E::Builtin(Builtin::Digit) => one(|c| c.is_ascii_digit()),
            E::Builtin(Builtin::Hex) => one(|c| c.is_ascii_hexdigit()),
            E::Builtin(Builtin::Ws) => one(char::is_whitespace),
            E::Ref(r) => self.rule(*r, at),
            E::Seq(v) => {
                let mut pos = at;
                for x in v {
                    match self.read(x, pos) {
                        Step::To(p) => pos = p,
                        other => return other,
                    }
                }
                Step::To(pos)
            }
            E::Alt(v) => {
                for x in v {
                    match self.read(x, at) {
                        Step::Miss => {}
                        other => return other,
                    }
                }
                Step::Miss
            }
            E::Star(x) => self.repeat(x, at),
            E::Plus(x) => match self.read(x, at) {
                Step::To(p) => self.repeat(x, p),
                other => other,
            },
            E::Opt(x) => match self.read(x, at) {
                Step::Miss => Step::To(at),
                other => other,
            },
            E::And(x) | E::Not(x) => {
                let kept = self.marks.len();
                let step = self.read(x, at);
                self.marks.truncate(kept);
                match (step, matches!(e, E::And(_))) {
                    (Step::End, _) => Step::End,
                    (Step::To(_), true) | (Step::Miss, false) => Step::To(at),
                    _ => Step::Miss,
                }
            }
            E::Try(x) | E::Field(_, x) | E::Guard(_, x) => self.read(x, at),
            _ => Step::End,
        }
    }

    /// Reads `x` while it matches and moves.
    fn repeat(&mut self, x: &E, mut at: usize) -> Step {
        loop {
            match self.read(x, at) {
                Step::To(p) if p > at => at = p,
                Step::To(_) | Step::Miss => return Step::To(at),
                Step::End => return Step::End,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{format, runs, Scope};

    #[test]
    fn the_format_grammar_colours_each_part() {
        let src = include_str!("../../../examples/syntaxjson.syntax");
        let (g, file) = format();
        let got = runs(&g, file, src);
        let comment = src.lines().next().expect("a first line");
        for (around, part, scope) in [
            (comment, comment, Scope::Comment),
            ("syntax Json", "syntax", Scope::Keyword),
            ("\nObj =", "Obj", Scope::Function),
            ("members:Member", "Member", Scope::TypeName),
            ("hex hex", "hex", Scope::SupportFunction),
            ("[1-9]", "[1-9]", Scope::CharClass),
            (r"'\\' (", r"\\", Scope::Escape),
            ("^BadEscape", "^BadEscape", Scope::Label),
            ("members:Member", "members", Scope::Parameter),
            ("Obj / List", "/", Scope::Operator),
            ("'true'", "'true'", Scope::String),
        ] {
            let start = src.find(around).expect(around) + around.find(part).expect(part);
            let run = got.iter().find(|r| r.start <= start && start < r.stop);
            let found = run.map(|r| (r.start, r.stop, r.scope));
            assert_eq!(found, Some((start, start + part.len(), scope)), "{part}");
        }
    }

    #[test]
    fn a_refusal_ends_the_colours() {
        let (g, file) = format();
        let got = runs(&g, file, "a = 'x'\nb = ( 'y\n");
        assert_eq!(got.last().map(|r| r.stop), Some(7), "{got:?}");
    }
}
