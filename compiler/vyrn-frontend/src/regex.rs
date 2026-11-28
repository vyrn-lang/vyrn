//! A regular-expression engine for `String` refinements (`value =~ "..."`).
//!
//! The compiler builds a DFA once; the code generator emits its transition
//! table and a fixed runner, so nothing parses a pattern at run time.
//!
//! The language is `std/regex.syntax`, which `std/regex` reads too, and the
//! pattern is matched whole. [`grammar`] is its parser, and [`Build`] makes the
//! tree from its matches. Counted repetition expands structurally with bounds
//! capped at 255. A pattern with a backreference is no regular language: it has
//! no DFA, and [`compile`] answers [`Refusal::NotRegular`]. Every construct means what
//! ECMA-262 means on ASCII (`.` excludes `\n` and `\r`), so a pattern round-trips
//! to a JSON Schema `pattern`. A literal, a class member, `.` and a negated class
//! are code points, and the DFA reads their UTF-8 bytes.
//!
//! The DFA is exact for lookahead and lookbehind too, nested to any depth, so a
//! finite string type over a pattern with an assertion is proved by containment
//! like any other: [`Terms`] builds it by derivatives, and an anchor or a word
//! boundary is a lookaround over one byte ([`anchor`]).

/// A set of bytes: one bit per byte value.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct ByteClass([u64; 4]);

impl ByteClass {
    fn empty() -> Self {
        ByteClass([0; 4])
    }
    fn set(&mut self, b: u8) {
        self.0[(b >> 6) as usize] |= 1u64 << (b & 63);
    }
    fn add_range(&mut self, lo: u8, hi: u8) {
        for b in lo..=hi {
            self.set(b);
        }
    }
    fn contains(&self, b: u8) -> bool {
        self.0[(b >> 6) as usize] & (1u64 << (b & 63)) != 0
    }
    fn range(lo: u8, hi: u8) -> Self {
        let mut cl = ByteClass::empty();
        cl.add_range(lo, hi);
        cl
    }
}

/// The last scalar value.
const MAX_CHAR: u32 = 0x10_FFFF;

/// A set of code points: sorted inclusive ranges of scalar values, apart and not
/// adjacent, so no range holds a surrogate.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Chars(Vec<(u32, u32)>);

impl Chars {
    /// The set of the code points `ranges` hold, less the surrogates.
    fn new(mut ranges: Vec<(u32, u32)>) -> Chars {
        ranges.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::new();
        for (lo, hi) in ranges {
            match merged.last_mut() {
                Some(last) if lo <= last.1.saturating_add(1) => last.1 = last.1.max(hi),
                _ => merged.push((lo, hi)),
            }
        }
        let mut out = Vec::new();
        for (lo, hi) in merged {
            out.extend([(lo, hi.min(0xD7FF)), (lo.max(0xE000), hi)]);
        }
        out.retain(|r| r.0 <= r.1);
        Chars(out)
    }
    fn of(ranges: &[(u32, u32)]) -> Chars {
        Chars::new(ranges.to_vec())
    }
    fn complement(&self) -> Chars {
        let mut out = Vec::new();
        let mut next = 0;
        for &(lo, hi) in &self.0 {
            if lo > next {
                out.push((next, lo - 1));
            }
            next = hi + 1;
        }
        if next <= MAX_CHAR {
            out.push((next, MAX_CHAR));
        }
        Chars::new(out)
    }
    fn union(&self, o: &Chars) -> Chars {
        Chars::new([&self.0[..], &o.0[..]].concat())
    }
    fn intersect(&self, o: &Chars) -> Chars {
        self.complement().union(&o.complement()).complement()
    }
    fn minus(&self, o: &Chars) -> Chars {
        self.intersect(&o.complement())
    }

    /// The set's UTF-8 bytes as a term: one class of its ASCII members, then one
    /// sequence of byte ranges for each run of code points that one prefix shape
    /// encodes, as Rust regex-syntax's `Utf8Sequences` splits them. `std/regex`
    /// `fragChars` builds the same terms.
    fn re(&self) -> Re {
        let mut alts = Vec::new();
        let ascii: Vec<(u32, u32)> = (self.0.iter())
            .filter(|r| r.0 < 0x80)
            .map(|&(lo, hi)| (lo, hi.min(0x7F)))
            .collect();
        if !ascii.is_empty() || self.0.is_empty() {
            let mut cl = ByteClass::empty();
            for (lo, hi) in ascii {
                cl.add_range(lo as u8, hi as u8);
            }
            alts.push(Re::Class(cl));
        }
        for seq in utf8_sequences(&self.0) {
            alts.push(concat(
                seq.into_iter()
                    .map(|(a, b)| Re::Class(ByteClass::range(a, b)))
                    .collect(),
            ));
        }
        match alts.len() {
            1 => alts.pop().expect("one alternative"),
            _ => Re::Alt(alts),
        }
    }
}

/// The byte ranges of the members of `set` from 0x80 up: a sequence of one range per
/// byte for each run that encodes with one prefix shape.
fn utf8_sequences(set: &[(u32, u32)]) -> Vec<Vec<(u8, u8)>> {
    let mut out = Vec::new();
    let mut todo: Vec<(u32, u32)> = (set.iter().rev())
        .filter(|r| r.1 >= 0x80)
        .map(|&(lo, hi)| (lo.max(0x80), hi))
        .collect();
    // Each split leaves a shorter `(s, e)` and pushes the rest, so the loop ends.
    while let Some((s, mut e)) = todo.pop() {
        if let Some(&cut) = [0x7FF, 0xFFFF].iter().find(|&&c| s <= c && c < e) {
            todo.push((cut + 1, e));
            todo.push((s, cut));
            continue;
        }
        let split = (1..4).find_map(|i| {
            let m = (1u32 << (6 * i)) - 1;
            if s & !m == e & !m {
                None
            } else if s & m != 0 {
                Some(s | m)
            } else if e & m != m {
                Some((e & !m) - 1)
            } else {
                None
            }
        });
        if let Some(mid) = split {
            todo.push((mid + 1, e));
            e = mid;
            todo.push((s, e));
            continue;
        }
        let (a, b) = (utf8(s), utf8(e));
        out.push(a.into_iter().zip(b).collect());
    }
    out
}

#[derive(Clone)]
enum Re {
    /// The empty string: an empty group or branch.
    Empty,
    Class(ByteClass),
    Concat(Vec<Re>),
    Alt(Vec<Re>),
    Star(Box<Re>),
    Plus(Box<Re>),
    Opt(Box<Re>),
    /// An assertion: the body matches, or with `negate` does not, from here on
    /// when `ahead`, or else up to here.
    Look {
        ahead: bool,
        negate: bool,
        body: Box<Re>,
    },
}

#[rustfmt::skip]
#[allow(unused_variables, unused_parens, dead_code)]
mod grammar;

use grammar::{Hooks, Rule};

/// Budget for nodes copied by counted repetition. Nested counts multiply
/// (`(a{99}){99}` is about 10^4 nodes), so a pathological pattern is a compile
/// error, not an out-of-memory hang.
const EXPANSION_BUDGET: usize = 16_384;

/// Node count of `re`: the unit [`EXPANSION_BUDGET`] charges.
fn re_size(re: &Re) -> usize {
    match re {
        // A body is one term however often a count copies it: equal bodies are one.
        Re::Empty | Re::Class(_) | Re::Look { .. } => 1,
        Re::Concat(ps) | Re::Alt(ps) => ps.iter().map(re_size).sum::<usize>(),
        Re::Star(r) | Re::Plus(r) | Re::Opt(r) => re_size(r),
    }
}

/// The tree of a pattern, built from its rules' matches. A rule's match closes after
/// the matches inside it, and spans nest, so a branch or an alternation takes off
/// the stack exactly the terms that started inside its own span.
struct Build<'a> {
    b: &'a [u8],
    /// Each term with the byte its match starts at, in source order.
    terms: Vec<(usize, Re)>,
    /// Each set of a class being read with the byte its match starts at: a member,
    /// a range, a POSIX class, a union, or a nested class.
    sets: Vec<(usize, Chars)>,
    /// The first byte of each `&&`, `--` or `~~` of a class being read, at its offset.
    ops: Vec<(usize, u8)>,
    /// The counts of the counted repetition being read.
    counts: Vec<u32>,
    /// Nodes copied by counted repetition so far.
    expanded: usize,
    /// Whether the scan stopped at a row of the grammar.
    in_grammar: bool,
    /// The flags of each group being read, the pattern's first.
    flags: Vec<Flags>,
    /// The names of the named groups read so far.
    names: Vec<Vec<u8>>,
    /// The lookarounds open around the term being read.
    looks: usize,
    /// The capturing groups opened so far.
    groups: usize,
    /// The group each backreference names, by number or by name: the builder checks
    /// them once the pattern has numbered every group.
    refs: Vec<Backref>,
}

/// The group a backreference names.
enum Backref {
    Number(usize),
    Name(Vec<u8>),
}

/// The flags that change what an atom means: `i` folds ASCII case, `s` lets `.`
/// take `\n` and `\r`, and `m` makes `^` and `$` a line's ends.
#[derive(Clone, Copy, Default)]
struct Flags {
    fold: bool,
    dot_all: bool,
    lines: bool,
}

impl Flags {
    /// The flags after the letters `t` of `(?ims-ims` up to `:` or `)`.
    fn with(mut self, t: &[u8]) -> Flags {
        let mut on = true;
        for &c in t {
            match c {
                b'-' => on = false,
                b'i' => self.fold = on,
                b's' => self.dot_all = on,
                b'm' => self.lines = on,
                _ => {}
            }
        }
        self
    }
}

/// `set` with the other case of each ASCII letter it holds.
fn fold(set: &Chars) -> Chars {
    let shift = |lo: u32, hi: u32, by: i32| -> Vec<(u32, u32)> {
        let part = set.intersect(&Chars::of(&[(lo, hi)]));
        let moved = |c: u32| c.wrapping_add_signed(by);
        part.0.iter().map(|&(a, b)| (moved(a), moved(b))).collect()
    };
    let mut all = set.0.clone();
    all.extend(shift(0x61, 0x7a, -32));
    all.extend(shift(0x41, 0x5a, 32));
    Chars::new(all)
}

/// The entries of `stack` that start at `start` or later, taken off it.
fn since<T>(stack: &mut Vec<(usize, T)>, start: usize) -> Vec<T> {
    let k = stack.partition_point(|t| t.0 < start);
    stack.drain(k..).map(|t| t.1).collect()
}

impl Build<'_> {
    /// The code point the UTF-8 text from `at` starts with.
    fn char_at(&self, at: usize) -> u32 {
        let text = std::str::from_utf8(&self.b[at..]).expect("a pattern is UTF-8");
        u32::from(text.chars().next().expect("a character"))
    }

    /// The last term, which a repeat applies to. A repeat follows an atom, and
    /// every atom pushes at least one term.
    fn last(&mut self) -> (usize, Re) {
        self.terms.pop().expect("a repeat follows an atom")
    }

    /// The one set a class's rule pushed since `start`: a bound pushes one.
    fn one_set(&mut self) -> Chars {
        self.sets.pop().expect("a bound pushes its set").1
    }

    fn flags(&self) -> Flags {
        *self.flags.last().expect("the pattern's flags stay")
    }

    /// The code point `c` as a term, both cases of an ASCII letter under `i`.
    fn char_term(&self, c: u32) -> Re {
        match self.flags().fold && char::from_u32(c).is_some_and(|c| c.is_ascii_alphabetic()) {
            true => fold(&Chars::of(&[(c, c)])).re(),
            false => char_re(c),
        }
    }
}

impl Hooks for Build<'_> {
    fn refuse(&mut self, _at: usize, rule: Rule) -> String {
        self.in_grammar = true;
        let (row, holes) = rule.row();
        say(row, &holes)
    }

    fn _regex_literal(&mut self, start: usize, _end: usize) -> Result<(), String> {
        self.terms
            .push((start, self.char_term(self.char_at(start))));
        Ok(())
    }

    /// A group's flags hold until its `)`; a name names one group.
    fn regex_group_open(&mut self, start: usize, end: usize) -> Result<(), String> {
        let t = &self.b[start..end];
        let name = match t.get(2) {
            Some(b'P') => Some(&t[4..t.len() - 1]),
            Some(b'<' | b'\'') => Some(&t[3..t.len() - 1]),
            _ => None,
        };
        let flags = match (name, t.len()) {
            (None, 3..) => self.flags().with(&t[2..t.len() - 1]),
            _ => self.flags(),
        };
        if self.looks > 0 && (name.is_some() || t.len() == 1) {
            return Err(say("LookCapture", &[]));
        }
        if let Some(name) = name {
            if self.names.iter().any(|n| n == name) {
                return Err(say("NameTwice", &[]));
            }
            self.names.push(name.to_vec());
        }
        if name.is_some() || t.len() == 1 {
            self.groups += 1;
        }
        self.flags.push(flags);
        Ok(())
    }

    fn _regex_subpattern(&mut self, _start: usize, _end: usize) -> Result<(), String> {
        self.flags.pop();
        Ok(())
    }

    /// `(?i)` sets the flags for the rest of its group and matches the empty string.
    /// A lookaround's flags hold until its `)`, as a group's do.
    fn regex_lookaround_open(&mut self, _start: usize, _end: usize) -> Result<(), String> {
        self.looks += 1;
        self.flags.push(self.flags());
        Ok(())
    }

    fn _regex_look(&mut self, start: usize, _end: usize) -> Result<(), String> {
        self.looks -= 1;
        self.flags.pop();
        let body = Box::new(self.last().1);
        // `(?=`, `(?!`, `(?<=` or `(?<!`, and a `)` after it.
        let (ahead, negate) = match (self.b[start + 2], self.b[start + 3]) {
            (b'=', _) => (true, false),
            (b'!', _) => (true, true),
            (_, b'=') => (false, false),
            _ => (false, true),
        };
        self.terms.push((
            start,
            Re::Look {
                ahead,
                negate,
                body,
            },
        ));
        Ok(())
    }

    /// A backreference is no term of a regular language: it stands as the empty
    /// string, so the counts charge it one term, and marks the pattern.
    fn _regex_backref(&mut self, start: usize, end: usize) -> Result<(), String> {
        let t = &self.b[start..end];
        let digits = |t: &[u8]| String::from_utf8_lossy(t).parse::<usize>().unwrap_or(0);
        let r = match (t[1], t.get(2)) {
            (b'?', _) => Backref::Name(t[4..t.len() - 1].to_vec()),
            (b'g', Some(b'{')) if t[3] == b'-' => {
                let back = digits(&t[4..t.len() - 1]);
                Backref::Number((self.groups + 1).saturating_sub(back))
            }
            (b'g', Some(b'{')) => Backref::Number(digits(&t[3..t.len() - 1])),
            (b'g', _) => Backref::Number(digits(&t[2..])),
            (b'k', _) => Backref::Name(t[3..t.len() - 1].to_vec()),
            _ => Backref::Number(digits(&t[1..])),
        };
        self.refs.push(r);
        self.terms.push((start, Re::Empty));
        Ok(())
    }

    fn _regex_anchor(&mut self, start: usize, end: usize) -> Result<(), String> {
        let t = &self.b[start..end];
        let re = anchor(t.strip_prefix(b"\\").unwrap_or(t), self.flags().lines);
        self.terms.push((start, re));
        Ok(())
    }

    fn _regex_flags(&mut self, start: usize, end: usize) -> Result<(), String> {
        let flags = self.flags().with(&self.b[start + 2..end - 1]);
        *self.flags.last_mut().expect("the pattern's flags stay") = flags;
        self.terms.push((start, Re::Empty));
        Ok(())
    }

    fn _regex_comment(&mut self, start: usize, _end: usize) -> Result<(), String> {
        self.terms.push((start, Re::Empty));
        Ok(())
    }

    /// A quote's characters are terms of their own, so a repeat takes the last, as
    /// in PCRE.
    fn _regex_escape(&mut self, start: usize, end: usize) -> Result<(), String> {
        let t = &self.b[start + 1..end];
        let re = match t[0] {
            b'Q' => {
                let q = &t[1..];
                let q = q.strip_suffix(b"\\E").unwrap_or(q);
                if q.is_empty() {
                    Re::Empty
                } else {
                    let text = std::str::from_utf8(q).expect("a pattern is UTF-8");
                    for c in text.chars() {
                        self.terms.push((start, self.char_term(u32::from(c))));
                    }
                    return Ok(());
                }
            }
            // `\R` is a line break: CR LF, or one of LF, VT, FF and CR.
            b'R' => Re::Alt(vec![
                concat(vec![char_re(13), char_re(10)]),
                Chars::of(&[(0x0a, 0x0d)]).re(),
            ]),
            c if SHORTHANDS.contains(&c) => shorthand(c).re(),
            _ => self.char_term(escape_char(t)?),
        };
        self.terms.push((start, re));
        Ok(())
    }

    // `.` excludes LF and CR, as in ECMA-262, but under `s`.
    fn _regex_class_dot(&mut self, start: usize, _end: usize) -> Result<(), String> {
        let breaks = match self.flags().dot_all {
            true => Chars(Vec::new()),
            false => Chars::of(&[(0x0a, 0x0a), (0x0d, 0x0d)]),
        };
        let dot = breaks.complement();
        self.terms.push((start, dot.re()));
        Ok(())
    }

    fn _regex_single(&mut self, start: usize, _end: usize) -> Result<(), String> {
        let c = self.char_at(start);
        self.sets.push((start, Chars::of(&[(c, c)])));
        Ok(())
    }

    fn _regex_escape_in_class(&mut self, start: usize, end: usize) -> Result<(), String> {
        let t = &self.b[start + 1..end];
        let set = match SHORTHANDS.contains(&t[0]) {
            true => shorthand(t[0]),
            false => {
                let c = escape_char(t)?;
                Chars::of(&[(c, c)])
            }
        };
        self.sets.push((start, set));
        Ok(())
    }

    /// A range runs between the code points of its two bounds, which the grammar
    /// admits only as one character each.
    fn _regex_range(&mut self, start: usize, _end: usize) -> Result<(), String> {
        let hi = self.one_set();
        let lo = self.one_set();
        let (lo, hi) = (lo.0[0].0, hi.0[0].0);
        if lo > hi {
            return Err(say("Backwards", &[]));
        }
        self.sets.push((start, Chars::of(&[(lo, hi)])));
        Ok(())
    }

    fn _regex_member(&mut self, start: usize, _end: usize) -> Result<(), String> {
        let set = self.one_set();
        self.sets.push((start, set));
        Ok(())
    }

    fn _regex_class_posix(&mut self, start: usize, end: usize) -> Result<(), String> {
        let negated = self.b[start + 2] == b'^';
        let name = &self.b[start + 2 + usize::from(negated)..end - 2];
        let set = posix(name);
        self.sets
            .push((start, if negated { set.complement() } else { set }));
        Ok(())
    }

    fn _regex_set(&mut self, start: usize, _end: usize) -> Result<(), String> {
        let union = since(&mut self.sets, start)
            .iter()
            .fold(Chars(Vec::new()), |a, b| a.union(b));
        self.sets.push((start, union));
        Ok(())
    }

    fn regex_set_op(&mut self, start: usize, _end: usize) -> Result<(), String> {
        self.ops.push((start, self.b[start]));
        Ok(())
    }

    /// The operators combine the unions left to right; `i` folds before `^` negates.
    fn _regex_class(&mut self, start: usize, _end: usize) -> Result<(), String> {
        let ops = since(&mut self.ops, start);
        let mut sets = since(&mut self.sets, start).into_iter();
        let first = sets.next().expect("a class holds a union");
        let set = ops
            .into_iter()
            .zip(sets)
            .fold(first, |a, (op, b)| match op {
                b'&' => a.intersect(&b),
                b'-' => a.minus(&b),
                _ => a.minus(&b).union(&b.minus(&a)),
            });
        let set = if self.flags().fold { fold(&set) } else { set };
        let set = if self.b[start + 1] == b'^' {
            set.complement()
        } else {
            set
        };
        self.sets.push((start, set));
        Ok(())
    }

    /// A class that is an atom, not an item of another class, becomes a term.
    fn _regex_atom(&mut self, start: usize, _end: usize) -> Result<(), String> {
        if self.b[start] == b'[' {
            let set = self.one_set();
            self.terms.push((start, set.re()));
        }
        Ok(())
    }

    fn regex_quantifier(&mut self, start: usize, _end: usize) -> Result<(), String> {
        let (at, re) = self.last();
        let re = Box::new(re);
        let re = match self.b[start] {
            b'*' => Re::Star(re),
            b'+' => Re::Plus(re),
            _ => Re::Opt(re),
        };
        self.terms.push((at, re));
        Ok(())
    }

    /// A count, at most 255.
    fn regex_number(&mut self, start: usize, end: usize) -> Result<(), String> {
        let v = String::from_utf8_lossy(&self.b[start..end])
            .parse::<u32>()
            .ok()
            .filter(|&v| v <= 255)
            .ok_or_else(|| say("CountLarge", &[]))?;
        self.counts.push(v);
        Ok(())
    }

    /// Expands `{m}`, `{m,}` or `{m,n}`: `m` copies of the atom, then `n - m`
    /// optional copies, or one `*` for an open bound.
    fn _regex_quantifier_count(&mut self, _start: usize, end: usize) -> Result<(), String> {
        let counts = std::mem::take(&mut self.counts);
        let m = counts[0];
        let n = match counts.get(1) {
            Some(&n) => Some(n),
            None if self.b[end - 2] == b',' => None,
            None => Some(m),
        };
        if let Some(n) = n {
            if n < m {
                return Err(say("CountOrder", &[]));
            }
        }
        let (at, atom) = self.last();
        // Charge the budget before cloning: nesting multiplies the copies.
        let copies = m as usize
            + match n {
                None => 1, // `{m,}` adds one `Star` over the atom
                Some(n) => (n - m) as usize,
            };
        let cost = copies.saturating_mul(re_size(&atom));
        if self.expanded + cost > EXPANSION_BUDGET {
            return Err(say("CountBudget", &[]));
        }
        self.expanded += cost;
        let mut parts: Vec<Re> = std::iter::repeat_with(|| atom.clone())
            .take(m as usize)
            .collect();
        match n {
            None => parts.push(Re::Star(Box::new(atom))),
            Some(n) => {
                for _ in m..n {
                    parts.push(Re::Opt(Box::new(atom.clone())));
                }
            }
        }
        self.terms.push((at, concat(parts)));
        Ok(())
    }

    fn _regex_branch(&mut self, start: usize, _end: usize) -> Result<(), String> {
        let parts = since(&mut self.terms, start);
        self.terms.push((start, concat(parts)));
        Ok(())
    }

    fn _regex_alt(&mut self, start: usize, _end: usize) -> Result<(), String> {
        let mut branches = since(&mut self.terms, start);
        let re = match branches.len() {
            1 => branches.pop().expect("one branch"),
            _ => Re::Alt(branches),
        };
        self.terms.push((start, re));
        Ok(())
    }
}

fn concat(mut parts: Vec<Re>) -> Re {
    match parts.len() {
        0 => Re::Empty,
        1 => parts.pop().expect("one part"),
        _ => Re::Concat(parts),
    }
}

/// The sentence of `row` in `std/regex.errors/en.json`, which `std/regex` states too,
/// with each `{$hole}` filled from `holes`. A backslash takes the character after it.
fn say(row: &str, holes: &[(&str, &str)]) -> String {
    let catalogue = crate::schema::parse_json(include_str!("../../../std/regex.errors/en.json"))
        .expect("std/regex.errors/en.json is JSON");
    let key = row[..1].to_ascii_lowercase() + &row[1..];
    let text = catalogue.get(&key).and_then(crate::schema::Json::as_str);
    let mut chars = text.expect("every row has a sentence").chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            '{' => {
                let hole: String = chars.by_ref().take_while(|&c| c != '}').collect();
                let name = hole.trim_start_matches('$');
                out.extend(holes.iter().filter(|h| h.0 == name).map(|h| h.1));
            }
            c => out.push(c),
        }
    }
    out
}

/// Why a pattern has no DFA: a row the grammar refuses with, which the island check
/// of a `=~` literal states, or one the builder raises from the tree, each in the
/// catalogue's sentence; or a pattern that is no regular language, because it holds
/// a backreference, which only `std/regex`'s backtracking path matches. The builder
/// alone states its DFA state limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    Grammar(String),
    Builder(String),
    NotRegular,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Grammar(s) | Refusal::Builder(s) => f.write_str(s),
            Refusal::NotRegular => f.write_str("a backreference, which no DFA matches"),
        }
    }
}

/// Parses `pattern` by `std/regex.syntax` into its tree.
fn parse(pattern: &str) -> Result<Re, Refusal> {
    let build = Build {
        b: pattern.as_bytes(),
        terms: Vec::new(),
        sets: Vec::new(),
        ops: Vec::new(),
        counts: Vec::new(),
        expanded: 0,
        in_grammar: false,
        flags: vec![Flags::default()],
        names: Vec::new(),
        looks: 0,
        groups: 0,
        refs: Vec::new(),
    };
    let mut scanner = grammar::Scanner::new(pattern, build);
    if let Err(why) = scanner._regex_pattern() {
        return Err(match scanner.hooks.in_grammar {
            true => Refusal::Grammar(why),
            false => Refusal::Builder(why),
        });
    }
    let b = &scanner.hooks;
    let missing = |r: &Backref| match r {
        Backref::Number(k) => *k == 0 || *k > b.groups,
        Backref::Name(n) => !b.names.contains(n),
    };
    if b.refs.iter().any(missing) {
        return Err(Refusal::Builder(say("BackrefMissing", &[])));
    }
    if !b.refs.is_empty() {
        return Err(Refusal::NotRegular);
    }
    // The pattern is one alternation, which leaves one term.
    Ok(scanner.hooks.terms.pop().map_or(Re::Empty, |t| t.1))
}

/// The assertion `t`: `^` or `$`, or the text after the backslash of `\b`, `\B`,
/// `\A`, `\z`, `\Z`, `\<`, `\>` or a `\b{..}`, which the grammar admits. Each is a
/// lookaround over one byte, which is a character's own byte for `\n` and the ASCII
/// of `\w`: `^` is `(?<![\s\S])`, or `(?<![^\n])` on `lines`, and a word boundary is
/// a word character on one side and none on the other.
fn anchor(t: &[u8], lines: bool) -> Re {
    let look = |ahead, negate, body: Re| Re::Look {
        ahead,
        negate,
        body: Box::new(body),
    };
    let any = || Re::Class(ByteClass::range(0, 255));
    let edge = || match lines {
        true => Chars::of(&[(0x0a, 0x0a)]).complement().re(),
        false => any(),
    };
    let w = |ahead, negate| look(ahead, negate, shorthand(b'w').re());
    let start = || concat(vec![w(false, true), w(true, false)]);
    let end = || concat(vec![w(false, false), w(true, true)]);
    match t {
        b"^" => look(false, true, edge()),
        b"$" => look(true, true, edge()),
        b"A" => look(false, true, any()),
        b"z" => look(true, true, any()),
        b"Z" => look(
            true,
            false,
            concat(vec![
                Re::Opt(Box::new(char_re(10))),
                look(true, true, any()),
            ]),
        ),
        b"b" => Re::Alt(vec![end(), start()]),
        b"B" => Re::Alt(vec![
            concat(vec![w(false, false), w(true, false)]),
            concat(vec![w(false, true), w(true, true)]),
        ]),
        b"<" | b"b{start}" => start(),
        b">" | b"b{end}" => end(),
        b"b{start-half}" => w(false, true),
        _ => w(true, true),
    }
}

/// The escapes that stand for a set: `\d`, `\w`, `\s`, `\h`, their capitals, which
/// negate, and `\N`, any character but LF, which the grammar admits outside a class.
const SHORTHANDS: &[u8] = b"dDwWsShHN";

/// The set of the shorthand `\c`.
fn shorthand(c: u8) -> Chars {
    let set = Chars::of(match c.to_ascii_lowercase() {
        b'd' => &[(0x30, 0x39)],
        b'w' => &[(0x30, 0x39), (0x41, 0x5a), (0x5f, 0x5f), (0x61, 0x7a)],
        b's' => &[(0x09, 0x0d), (0x20, 0x20)],
        b'h' => &[(0x09, 0x09), (0x20, 0x20)],
        _ => &[(0x0a, 0x0a)],
    });
    match c.is_ascii_uppercase() {
        true => set.complement(),
        false => set,
    }
}

/// The set of the POSIX class `name`, which the grammar admits.
fn posix(name: &[u8]) -> Chars {
    Chars::of(match name {
        b"alnum" => &[(0x30, 0x39), (0x41, 0x5a), (0x61, 0x7a)],
        b"alpha" => &[(0x41, 0x5a), (0x61, 0x7a)],
        b"blank" => &[(0x09, 0x09), (0x20, 0x20)],
        b"cntrl" => &[(0x00, 0x1f), (0x7f, 0x7f)],
        b"digit" => &[(0x30, 0x39)],
        b"graph" => &[(0x21, 0x7e)],
        b"lower" => &[(0x61, 0x7a)],
        b"print" => &[(0x20, 0x7e)],
        b"punct" => &[(0x21, 0x2f), (0x3a, 0x40), (0x5b, 0x60), (0x7b, 0x7e)],
        b"space" => &[(0x09, 0x0d), (0x20, 0x20)],
        b"upper" => &[(0x41, 0x5a)],
        _ => &[(0x30, 0x39), (0x41, 0x46), (0x61, 0x66)],
    })
}

/// The code point of the escape `t`, the text after its backslash, which the grammar
/// admits as one character: a control, a code point or ASCII punctuation. `\b` reaches
/// here only from a class, where it is backspace.
fn escape_char(t: &[u8]) -> Result<u32, String> {
    Ok(match t[0] {
        b'x' | b'u' | b'o' => code_point(t)?,
        b'c' => u32::from(t[1] & 31),
        b't' => 9,
        b'n' => 10,
        b'v' => 11,
        b'f' => 12,
        b'r' => 13,
        b'a' => 7,
        b'e' => 0x1b,
        b'b' => 8,
        c => u32::from(c),
    })
}

/// The code point of `\xHH`, `\x{H..}`, `\uHHHH`, `\u{H..}` or `\o{O..}`, refused
/// above 10FFFF or in the surrogates. The grammar refuses NUL.
fn code_point(t: &[u8]) -> Result<u32, String> {
    let digits = t[1..]
        .strip_prefix(b"{")
        .map_or(&t[1..], |d| &d[..d.len() - 1]);
    let radix = if t[0] == b'o' { 8 } else { 16 };
    let digits = std::str::from_utf8(digits).expect("the grammar admits ASCII digits");
    u32::from_str_radix(digits, radix)
        .ok()
        .filter(|&c| char::from_u32(c).is_some())
        .ok_or_else(|| say("CodepointRange", &[]))
}

/// The UTF-8 bytes of the code point `c`, which [`code_point`] or the grammar bounds.
fn utf8(c: u32) -> Vec<u8> {
    let c = char::from_u32(c).expect("a scalar value");
    c.encode_utf8(&mut [0; 4]).as_bytes().to_vec()
}

/// The code point `c` as one term: the concatenation of its bytes.
fn char_re(c: u32) -> Re {
    let byte = |b: &u8| {
        let mut cl = ByteClass::empty();
        cl.set(*b);
        Re::Class(cl)
    };
    concat(utf8(c).iter().map(byte).collect())
}

/// A term of the derivative construction: an index into [`Terms`]. Equal terms
/// are one index.
///
/// A term matches a span of the text from a position, and it may look at the whole
/// text around the span: `M(t, i, j)`. Its language is the rests it matches whole:
/// a DFA state is a term for the rest of the text, and one component per lookbehind
/// body, which says at each position whether the text so far ends in that body.
type T = u32;

/// The empty language, the empty string and every string: terms 0, 1 and 3, after
/// the class of every byte.
const EMPTY: T = 0;
const EPS: T = 1;
const SIGMA: T = 3;

/// A list of terms, interned: `len` terms of [`Terms::lists`] from `at`, so two
/// equal lists are one.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct List {
    at: u32,
    len: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Node {
    Empty,
    Eps,
    Class(ByteClass),
    /// A concatenation leans right: the left term is never a `Cat`, and neither
    /// side is `Empty` or `Eps`.
    Cat(T, T),
    /// Two or more terms, ascending, none an `Alt` or `Empty`.
    Alt(List),
    /// The body is not `Empty`, `Eps` or a `Star`.
    Star(T),
    /// Two or more terms, ascending, none an `And`, `Empty` or `SIGMA`.
    And(List),
    /// The spans `M` of the body does not match. The body is no `Not`.
    Not(T),
    /// The empty span where the rest of the text is in the body's language: a
    /// lookahead, `(?=A)` as `A SIGMA` and `(?!A)` as `Not(A SIGMA)`.
    Guard(T),
    /// The empty span where the text so far ends in lookbehind body `k`, or does
    /// not when the flag is false.
    Behind(u32, bool),
}

/// The terms built so far, facts about each, and the work already done. The
/// constructors keep the forms [`Node`] states, and those similarity rules keep
/// the derivatives of a term finite (Brzozowski 1964, with the Boolean operators).
struct Terms {
    nodes: Vec<Node>,
    ids: std::collections::HashMap<Node, T>,
    /// The members of every list, and each list by its members.
    lists: Vec<T>,
    list_ids: std::collections::HashMap<Vec<T>, List>,
    /// Emptied buffers to build a list in, so a derivative that finds its list
    /// interned allocates nothing.
    spare: Vec<Vec<T>>,
    /// Whether term `t` holds no `And`, `Not`, `Guard` or `Behind`, so whether it
    /// matches the empty span depends on nothing around it.
    plain: Vec<bool>,
    /// For a plain term, whether it matches the empty span.
    nullable: Vec<bool>,
    /// Each lookbehind body.
    bodies: Vec<T>,
    /// Each context: the components of a state, one per body.
    contexts: Vec<Vec<T>>,
    context_ids: std::collections::HashMap<Vec<T>, u32>,
    /// The derivative of a term by a byte that stands for its run, in a context.
    derived: std::collections::HashMap<(T, u8, u32), T>,
    /// The condition on the rest under which a term matches the empty span here.
    conditions: std::collections::HashMap<(T, u32), T>,
}

impl Terms {
    fn new() -> Terms {
        let mut terms = Terms {
            nodes: Vec::new(),
            ids: Default::default(),
            lists: Vec::new(),
            list_ids: Default::default(),
            spare: Vec::new(),
            plain: Vec::new(),
            nullable: Vec::new(),
            bodies: Vec::new(),
            contexts: Vec::new(),
            context_ids: Default::default(),
            derived: Default::default(),
            conditions: Default::default(),
        };
        terms.node(Node::Empty);
        terms.node(Node::Eps);
        let any = terms.node(Node::Class(ByteClass::range(0, 255)));
        terms.node(Node::Star(any));
        terms
    }

    fn node(&mut self, n: Node) -> T {
        if let Some(&t) = self.ids.get(&n) {
            return t;
        }
        let (p, z) = (&self.plain, &self.nullable);
        let (plain, nullable) = match n {
            Node::Empty | Node::Class(_) => (true, false),
            Node::Eps => (true, true),
            Node::Star(r) => (p[r as usize], true),
            Node::Cat(l, r) => (
                p[l as usize] && p[r as usize],
                z[l as usize] && z[r as usize],
            ),
            Node::Alt(l) => (
                self.items(l).iter().all(|&t| p[t as usize]),
                self.items(l).iter().any(|&t| z[t as usize]),
            ),
            Node::And(_) | Node::Not(_) | Node::Guard(_) | Node::Behind(..) => (false, false),
        };
        let t = self.nodes.len() as T;
        self.nodes.push(n);
        self.plain.push(plain);
        self.nullable.push(nullable);
        self.ids.insert(n, t);
        t
    }

    /// The members of `l`.
    fn items(&self, l: List) -> &[T] {
        &self.lists[l.at as usize..(l.at + l.len) as usize]
    }

    /// Member `i` of the `Alt` or `And` term `t`.
    fn item(&self, t: T, i: u32) -> T {
        match self.nodes[t as usize] {
            Node::Alt(l) | Node::And(l) => self.lists[(l.at + i) as usize],
            _ => unreachable!("only a list term has members"),
        }
    }

    /// An empty buffer, taken from the spares.
    fn buffer(&mut self) -> Vec<T> {
        self.spare.pop().unwrap_or_default()
    }

    fn give(&mut self, mut buffer: Vec<T>) {
        buffer.clear();
        self.spare.push(buffer);
    }

    /// The list of the members of `ts`, which are ascending and two or more.
    fn list(&mut self, ts: &[T]) -> List {
        if let Some(&l) = self.list_ids.get(ts) {
            return l;
        }
        let l = List {
            at: self.lists.len() as u32,
            len: ts.len() as u32,
        };
        self.lists.extend_from_slice(ts);
        self.list_ids.insert(ts.to_vec(), l);
        l
    }

    fn cat(&mut self, a: T, b: T) -> T {
        if a == EMPTY || b == EMPTY {
            return EMPTY;
        }
        let Node::Cat(..) = self.nodes[a as usize] else {
            return match (a, b) {
                (EPS, _) => b,
                (_, EPS) => a,
                _ => self.node(Node::Cat(a, b)),
            };
        };
        let mut spine = self.buffer();
        let mut x = a;
        while let Node::Cat(l, r) = self.nodes[x as usize] {
            spine.push(l);
            x = r;
        }
        spine.push(x);
        let mut out = b;
        for &t in spine.iter().rev() {
            out = match (t, out) {
                (EPS, _) => out,
                (_, EPS) => t,
                _ => self.node(Node::Cat(t, out)),
            };
        }
        self.give(spine);
        out
    }

    /// The `Alt` of the terms `ts`, a buffer it gives back.
    fn alt(&mut self, ts: Vec<T>) -> T {
        let mut all = self.buffer();
        for &t in &ts {
            match self.nodes[t as usize] {
                Node::Alt(l) => all.extend_from_slice(self.items(l)),
                Node::Empty => {}
                _ => all.push(t),
            }
        }
        self.give(ts);
        all.sort_unstable();
        all.dedup();
        let t = match all.len() {
            0 => EMPTY,
            1 => all[0],
            _ => {
                let l = self.list(&all);
                self.node(Node::Alt(l))
            }
        };
        self.give(all);
        t
    }

    /// The `And` of the terms `ts`, a buffer it gives back.
    fn and(&mut self, ts: Vec<T>) -> T {
        let mut all = self.buffer();
        let mut empty = false;
        for &t in &ts {
            match self.nodes[t as usize] {
                Node::And(l) => all.extend_from_slice(self.items(l)),
                Node::Empty => empty = true,
                _ if t == SIGMA => {}
                _ => all.push(t),
            }
        }
        self.give(ts);
        all.sort_unstable();
        all.dedup();
        let t = match (empty, all.len()) {
            (true, _) => EMPTY,
            (_, 0) => SIGMA,
            (_, 1) => all[0],
            _ => {
                let l = self.list(&all);
                self.node(Node::And(l))
            }
        };
        self.give(all);
        t
    }

    /// The `And` of `a` and `b`, with no buffer when either is `SIGMA` or `EMPTY`.
    fn and2(&mut self, a: T, b: T) -> T {
        match (a, b) {
            (EMPTY, _) | (_, EMPTY) => EMPTY,
            (SIGMA, x) | (x, SIGMA) => x,
            _ => {
                let mut ts = self.buffer();
                ts.extend([a, b]);
                self.and(ts)
            }
        }
    }

    fn not(&mut self, a: T) -> T {
        match self.nodes[a as usize] {
            Node::Not(x) => x,
            Node::Empty => SIGMA,
            _ if a == SIGMA => EMPTY,
            _ => self.node(Node::Not(a)),
        }
    }

    fn guard(&mut self, g: T) -> T {
        match g {
            SIGMA => EPS,
            EMPTY => EMPTY,
            _ => self.node(Node::Guard(g)),
        }
    }

    fn star(&mut self, a: T) -> T {
        match self.nodes[a as usize] {
            Node::Empty | Node::Eps => EPS,
            Node::Star(_) => a,
            _ => self.node(Node::Star(a)),
        }
    }

    /// The context of the components `comps`.
    fn context(&mut self, comps: Vec<T>) -> u32 {
        let next = self.contexts.len() as u32;
        let id = *self.context_ids.entry(comps.clone()).or_insert(next);
        if id == next {
            self.contexts.push(comps);
        }
        id
    }

    /// The term of `re`. A lookbehind body is a term of its own, and its component
    /// starts as `SIGMA body`.
    fn of(&mut self, re: &Re) -> T {
        match re {
            Re::Empty => EPS,
            Re::Class(c) => self.node(Node::Class(*c)),
            Re::Concat(ps) => ps.iter().rev().fold(EPS, |out, p| {
                let t = self.of(p);
                self.cat(t, out)
            }),
            Re::Alt(bs) => {
                let ts = bs.iter().map(|b| self.of(b)).collect();
                self.alt(ts)
            }
            Re::Star(r) => {
                let t = self.of(r);
                self.star(t)
            }
            Re::Plus(r) => {
                let t = self.of(r);
                let s = self.star(t);
                self.cat(t, s)
            }
            Re::Opt(r) => {
                let t = self.of(r);
                self.alt(vec![t, EPS])
            }
            Re::Look {
                ahead: true,
                negate,
                body,
            } => {
                let b = self.of(body);
                let g = self.cat(b, SIGMA);
                let g = if *negate { self.not(g) } else { g };
                self.guard(g)
            }
            Re::Look {
                ahead: false,
                negate,
                body,
            } => {
                let b = self.of(body);
                let k = match self.bodies.iter().position(|&x| x == b) {
                    Some(k) => k,
                    None => {
                        self.bodies.push(b);
                        self.bodies.len() - 1
                    }
                };
                self.node(Node::Behind(k as u32, !negate))
            }
        }
    }

    /// The components a text starts with: `SIGMA body` for each body.
    fn first_context(&mut self) -> u32 {
        let comps = (0..self.bodies.len())
            .map(|k| self.cat(SIGMA, self.bodies[k]))
            .collect();
        self.context(comps)
    }

    /// Whether `t` matches the empty span at the end of the text, in context `cx`.
    fn ends(&self, t: T, cx: u32) -> bool {
        if self.plain[t as usize] {
            return self.nullable[t as usize];
        }
        match &self.nodes[t as usize] {
            Node::Empty | Node::Class(_) => false,
            Node::Eps | Node::Star(_) => true,
            Node::Cat(..) => {
                let mut x = t;
                while let Node::Cat(l, r) = self.nodes[x as usize] {
                    if !self.ends(l, cx) {
                        return false;
                    }
                    x = r;
                }
                self.ends(x, cx)
            }
            Node::Alt(l) => self.items(*l).iter().any(|&t| self.ends(t, cx)),
            Node::And(l) => self.items(*l).iter().all(|&t| self.ends(t, cx)),
            Node::Not(x) => !self.ends(*x, cx),
            Node::Guard(g) => self.ends(*g, cx),
            Node::Behind(k, holds) => {
                self.ends(self.contexts[cx as usize][*k as usize], cx) == *holds
            }
        }
    }

    /// The rests under which `t` matches the empty span here, in context `cx`: for a
    /// plain term, every rest or none.
    fn condition(&mut self, t: T, cx: u32) -> T {
        if self.plain[t as usize] {
            return if self.nullable[t as usize] {
                SIGMA
            } else {
                EMPTY
            };
        }
        if let Some(&c) = self.conditions.get(&(t, cx)) {
            return c;
        }
        let c = match self.nodes[t as usize] {
            Node::Empty | Node::Class(_) => EMPTY,
            Node::Eps | Node::Star(_) => SIGMA,
            Node::Cat(..) => {
                let mut parts = self.buffer();
                let mut x = t;
                while let Node::Cat(l, r) = self.nodes[x as usize] {
                    let c = self.condition(l, cx);
                    parts.push(c);
                    x = r;
                }
                let c = self.condition(x, cx);
                parts.push(c);
                self.and(parts)
            }
            Node::Alt(l) => {
                let mut cs = self.buffer();
                for i in 0..l.len {
                    let c = self.condition(self.item(t, i), cx);
                    cs.push(c);
                }
                self.alt(cs)
            }
            Node::And(l) => {
                let mut cs = self.buffer();
                for i in 0..l.len {
                    let c = self.condition(self.item(t, i), cx);
                    cs.push(c);
                }
                self.and(cs)
            }
            Node::Not(x) => {
                let c = self.condition(x, cx);
                self.not(c)
            }
            Node::Guard(g) => g,
            Node::Behind(k, holds) => {
                let c = self.condition(self.contexts[cx as usize][k as usize], cx);
                if holds {
                    c
                } else {
                    self.not(c)
                }
            }
        };
        self.conditions.insert((t, cx), c);
        c
    }

    /// The term `d` with `M(d, i + 1, j) = M(t, i, j)` where the byte at `i` is `b`,
    /// in context `cx`: the strings `s` such that `b` then `s` is in `t`.
    fn derive(&mut self, t: T, b: u8, cx: u32) -> T {
        let cx = if self.plain[t as usize] { 0 } else { cx };
        if let Some(&d) = self.derived.get(&(t, b, cx)) {
            return d;
        }
        let d = match self.nodes[t as usize] {
            Node::Empty | Node::Eps | Node::Guard(_) | Node::Behind(..) => EMPTY,
            Node::Class(c) => match c.contains(b) {
                true => EPS,
                false => EMPTY,
            },
            Node::Alt(l) => {
                let mut ds = self.buffer();
                for i in 0..l.len {
                    let d = self.derive(self.item(t, i), b, cx);
                    ds.push(d);
                }
                self.alt(ds)
            }
            Node::And(l) => {
                let mut ds = self.buffer();
                for i in 0..l.len {
                    let d = self.derive(self.item(t, i), b, cx);
                    ds.push(d);
                }
                self.and(ds)
            }
            Node::Not(x) => {
                let d = self.derive(x, b, cx);
                self.not(d)
            }
            Node::Star(r) => {
                let dr = self.derive(r, b, cx);
                self.cat(dr, t)
            }
            // A part for each left term that takes `b`, under the condition the terms
            // before it put on the rest as they match the empty span. The recursion
            // goes only into a left term, so its depth is the nesting.
            Node::Cat(..) => {
                let mut parts = self.buffer();
                let mut held = SIGMA;
                let mut x = t;
                loop {
                    let (l, r) = match self.nodes[x as usize] {
                        Node::Cat(l, r) => (l, r),
                        _ => (x, EPS),
                    };
                    let dl = self.derive(l, b, cx);
                    let dh = self.derive(held, b, cx);
                    let rest = self.cat(dl, r);
                    let g = self.guard(dh);
                    parts.push(self.cat(g, rest));
                    let c = self.condition(l, cx);
                    held = self.and2(held, c);
                    if held == EMPTY || r == EPS {
                        break;
                    }
                    x = r;
                }
                self.alt(parts)
            }
        };
        self.derived.insert((t, b, cx), d);
        d
    }
}

/// Each byte class of `re`, so a byte's run is the bytes no class tells apart.
fn classes<'a>(re: &'a Re, out: &mut Vec<&'a ByteClass>) {
    match re {
        Re::Empty => {}
        Re::Class(c) => out.push(c),
        Re::Concat(ps) | Re::Alt(ps) => ps.iter().for_each(|p| classes(p, out)),
        Re::Star(r) | Re::Plus(r) | Re::Opt(r) | Re::Look { body: r, .. } => classes(r, out),
    }
}

/// A deterministic matcher. `table[state * 256 + byte]` is the next state; the
/// table is complete, and a dead state absorbs non-matches. `accepting[state]`
/// marks a full match.
#[derive(Clone, Debug)]
pub struct Dfa {
    pub start: u32,
    pub accepting: Vec<bool>,
    pub table: Vec<u32>,
}

impl Dfa {
    fn num_states(&self) -> usize {
        self.accepting.len()
    }

    /// Returns whether the whole of `s` matches.
    pub fn matches(&self, s: &str) -> bool {
        let mut st = self.start as usize;
        for &byte in s.as_bytes() {
            st = self.table[st * 256 + byte as usize] as usize;
        }
        self.accepting[st]
    }

    /// Marks the states reachable from `start`.
    fn reachable_from_start(&self) -> Vec<bool> {
        let n = self.num_states();
        let mut seen = vec![false; n];
        let mut stack = vec![self.start as usize];
        seen[self.start as usize] = true;
        while let Some(s) = stack.pop() {
            for b in 0..256usize {
                let t = self.table[s * 256 + b] as usize;
                if !seen[t] {
                    seen[t] = true;
                    stack.push(t);
                }
            }
        }
        seen
    }

    /// Returns whether the language is empty: no accepting state is reachable.
    pub fn is_empty(&self) -> bool {
        !self.can_reach_accept()[self.start as usize]
    }

    /// Marks the states that can reach an accepting state.
    fn can_reach_accept(&self) -> Vec<bool> {
        let n = self.num_states();
        // `rev[t]` lists the predecessors of `t`.
        let mut rev: Vec<Vec<usize>> = vec![Vec::new(); n];
        for s in 0..n {
            for b in 0..256usize {
                let t = self.table[s * 256 + b] as usize;
                rev[t].push(s);
            }
        }
        let mut seen = vec![false; n];
        let mut stack = Vec::new();
        for (s, &acc) in self.accepting.iter().enumerate() {
            if acc {
                seen[s] = true;
                stack.push(s);
            }
        }
        while let Some(t) = stack.pop() {
            for &s in &rev[t] {
                if !seen[s] {
                    seen[s] = true;
                    stack.push(s);
                }
            }
        }
        seen
    }

    /// Returns whether the language is finite: the live states (reachable from the
    /// start and able to reach an accept) hold no cycle. The empty language is
    /// finite. This is the test of a finite string type.
    pub fn is_finite(&self) -> bool {
        let n = self.num_states();
        let reach = self.reachable_from_start();
        let acc = self.can_reach_accept();
        let live: Vec<bool> = (0..n).map(|i| reach[i] && acc[i]).collect();
        // Three-colour DFS over the live subgraph; a self-loop is a cycle.
        #[derive(Clone, Copy, PartialEq)]
        enum Colour {
            White,
            Grey,
            Black,
        }
        let mut colour = vec![Colour::White; n];
        let succ = |s: usize| -> Vec<usize> {
            let mut out: Vec<usize> = Vec::new();
            for b in 0..256usize {
                let t = self.table[s * 256 + b] as usize;
                if live[t] && !out.contains(&t) {
                    out.push(t);
                }
            }
            out
        };
        for start in 0..n {
            if !live[start] || colour[start] != Colour::White {
                continue;
            }
            let mut stack: Vec<(usize, Vec<usize>, usize)> = vec![(start, succ(start), 0)];
            colour[start] = Colour::Grey;
            while let Some((s, succs, i)) = stack.last_mut() {
                if *i < succs.len() {
                    let t = succs[*i];
                    *i += 1;
                    match colour[t] {
                        Colour::Grey => return false, // a back edge: a cycle, so infinite
                        Colour::White => {
                            colour[t] = Colour::Grey;
                            let ts = succ(t);
                            stack.push((t, ts, 0));
                        }
                        Colour::Black => {}
                    }
                } else {
                    colour[*s] = Colour::Black;
                    stack.pop();
                }
            }
        }
        true
    }

    /// Returns every string in the language in byte order, or `None` if the
    /// language is infinite or has more than `cap` members. A non-UTF-8 member is
    /// rendered by [`escape_bytes`].
    pub fn enumerate(&self, cap: usize) -> Option<Vec<String>> {
        if !self.is_finite() {
            return None;
        }
        let acc = self.can_reach_accept();
        // BFS by length. A finite language repeats no live state on an accepting
        // path, so the state count bounds the length.
        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut frontier: Vec<(usize, Vec<u8>)> = vec![(self.start as usize, Vec::new())];
        let max_len = self.num_states() + 1; // safety bound; never hit when finite
        let mut depth = 0;
        while !frontier.is_empty() && depth <= max_len {
            let mut next: Vec<(usize, Vec<u8>)> = Vec::new();
            for (st, s) in &frontier {
                if self.accepting[*st] {
                    out.push(s.clone());
                    if out.len() > cap {
                        return None;
                    }
                }
                for b in 0..256usize {
                    let t = self.table[*st * 256 + b] as usize;
                    // Descend only into states that can still reach an accept.
                    if acc[t] {
                        let mut ns = s.clone();
                        ns.push(b as u8);
                        next.push((t, ns));
                        // Every prefix here extends to a distinct member, so a
                        // list over the cap settles the answer. Without this, the
                        // frontier of `[0-9a-f]{40}` grows 16x per level.
                        if next.len() > cap {
                            return None;
                        }
                    }
                }
            }
            frontier = next;
            depth += 1;
        }
        out.sort();
        Some(out.into_iter().map(|b| escape_bytes(&b)).collect())
    }

    /// Returns every accepted string that contains no `exclude` byte, in byte
    /// order, or `None` if there are more than `cap`.
    ///
    /// A sequence validated-string type gets its alphabet here:
    /// `token( token)*` is infinite, but its members without the separator are the
    /// single tokens, read from the same DFA the compiler validates against.
    pub fn enumerate_without(&self, exclude: u8, cap: usize) -> Option<Vec<String>> {
        // `acc[s]`: an accept is reachable from `s` without consuming `exclude`. The
        // unrestricted set would admit prefixes that reach an accept only through
        // `exclude`, and break the cap argument below.
        let n = self.num_states();
        let mut rev: Vec<Vec<usize>> = vec![Vec::new(); n];
        for s in 0..n {
            for b in 0..256usize {
                if b as u8 == exclude {
                    continue;
                }
                let t = self.table[s * 256 + b] as usize;
                rev[t].push(s);
            }
        }
        let mut acc = vec![false; n];
        let mut stack = Vec::new();
        for (s, &a) in self.accepting.iter().enumerate() {
            if a {
                acc[s] = true;
                stack.push(s);
            }
        }
        while let Some(t) = stack.pop() {
            for &s in &rev[t] {
                if !acc[s] {
                    acc[s] = true;
                    stack.push(s);
                }
            }
        }
        let mut out: Vec<Vec<u8>> = Vec::new();
        // BFS by length. If a cycle free of `exclude` makes the sublanguage
        // unbounded, the `cap` check returns `None` before the depth bound.
        let mut frontier: Vec<(usize, Vec<u8>)> = vec![(self.start as usize, Vec::new())];
        let max_len = self.num_states() + 1;
        let mut depth = 0;
        while !frontier.is_empty() && depth <= max_len {
            let mut next: Vec<(usize, Vec<u8>)> = Vec::new();
            for (st, s) in &frontier {
                if self.accepting[*st] {
                    out.push(s.clone());
                    if out.len() > cap {
                        return None;
                    }
                }
                for b in 0..256usize {
                    if b as u8 == exclude {
                        continue;
                    }
                    let t = self.table[*st * 256 + b] as usize;
                    if acc[t] {
                        let mut ns = s.clone();
                        ns.push(b as u8);
                        next.push((t, ns));
                        // As in `enumerate`: a successor list over the cap means `None`.
                        if next.len() > cap {
                            return None;
                        }
                    }
                }
            }
            frontier = next;
            depth += 1;
        }
        out.sort();
        out.dedup();
        Some(out.into_iter().map(|b| escape_bytes(&b)).collect())
    }
}

/// Renders bytes for a diagnostic: the UTF-8 string if valid, otherwise
/// printable ASCII verbatim and every other byte as `\xNN`.
fn escape_bytes(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            let mut out = String::new();
            for &b in bytes {
                if b.is_ascii_graphic() || b == b' ' {
                    out.push(b as char);
                } else {
                    out.push_str(&format!("\\x{b:02x}"));
                }
            }
            out
        }
    }
}

/// Returns the product DFA that accepts what both accept: the language of a
/// `value =~ "a" && value =~ "b"` conjunction.
pub fn intersect(a: &Dfa, b: &Dfa) -> Dfa {
    product(a, b, |x, y| x && y)
}

/// Checks that `sub` accepts nothing `sup` rejects. On failure returns a
/// shortest string in `sub \ sup`, found by BFS over the product of `sub` and
/// the complement of `sup` (interpolation containment).
pub fn contains(sup: &Dfa, sub: &Dfa) -> Result<(), String> {
    // A reachable pair accepting in `sub` and rejecting in `sup` is a witness.
    use std::collections::VecDeque;
    let ns = sup.num_states();
    let start = (sub.start as usize, sup.start as usize);
    let idx = |a: usize, c: usize| a * ns + c;
    let total = sub.num_states() * ns;
    let mut seen = vec![false; total];
    let mut prev: Vec<Option<(usize, u8)>> = vec![None; total];
    let mut queue = VecDeque::new();
    seen[idx(start.0, start.1)] = true;
    queue.push_back(start);
    let mut witness_state: Option<(usize, usize)> = None;
    'bfs: while let Some((a, c)) = queue.pop_front() {
        if sub.accepting[a] && !sup.accepting[c] {
            witness_state = Some((a, c));
            break 'bfs;
        }
        for b in 0..256usize {
            let na = sub.table[a * 256 + b] as usize;
            let nc = sup.table[c * 256 + b] as usize;
            let id = idx(na, nc);
            if !seen[id] {
                seen[id] = true;
                prev[id] = Some((idx(a, c), b as u8));
                queue.push_back((na, nc));
            }
        }
    }
    match witness_state {
        None => Ok(()),
        Some((a, c)) => {
            // Walk `prev` back to the start.
            let mut bytes: Vec<u8> = Vec::new();
            let mut cur = idx(a, c);
            let start_id = idx(start.0, start.1);
            while cur != start_id {
                let (p, b) = prev[cur].expect("product BFS parent");
                bytes.push(b);
                cur = p;
            }
            bytes.reverse();
            Err(escape_bytes(&bytes))
        }
    }
}

/// Product of two DFAs; `accept` combines the two acceptance bits. Only
/// reachable pairs become states.
fn product(a: &Dfa, b: &Dfa, accept: impl Fn(bool, bool) -> bool) -> Dfa {
    use std::collections::HashMap;
    let mut index: HashMap<(usize, usize), u32> = HashMap::new();
    let mut work: Vec<(usize, usize)> = Vec::new();
    let start_pair = (a.start as usize, b.start as usize);
    index.insert(start_pair, 0);
    work.push(start_pair);
    let mut table: Vec<u32> = Vec::new();
    let mut accepting: Vec<bool> = Vec::new();
    let mut processed = 0usize;
    while processed < work.len() {
        let (sa, sb) = work[processed];
        if table.len() < (processed + 1) * 256 {
            table.resize((processed + 1) * 256, 0);
        }
        accepting.resize(processed + 1, false);
        accepting[processed] = accept(a.accepting[sa], b.accepting[sb]);
        for byte in 0..256usize {
            let na = a.table[sa * 256 + byte] as usize;
            let nb = b.table[sb * 256 + byte] as usize;
            let next = *index.entry((na, nb)).or_insert_with(|| {
                let id = work.len() as u32;
                work.push((na, nb));
                id
            });
            table[processed * 256 + byte] = next;
        }
        processed += 1;
    }
    table.resize(work.len() * 256, 0);
    accepting.resize(work.len(), false);
    for (i, &(sa, sb)) in work.iter().enumerate() {
        accepting[i] = accept(a.accepting[sa], b.accepting[sb]);
    }
    Dfa {
        start: 0,
        accepting,
        table,
    }
}

/// One piece of an interpolation's concatenation language: a literal byte run
/// (a string constant) or the language of a hole (a finite string type's DFA).
pub enum ConcatPiece<'a> {
    Lit(&'a [u8]),
    Dfa(&'a Dfa),
}

/// An epsilon-NFA with several labelled edges per state, for concatenating
/// literal runs and hole DFAs: a term cannot state an arbitrary hole DFA.
struct GenNfa {
    states: Vec<GenState>,
}

struct GenState {
    eps: Vec<usize>,
    edges: Vec<(ByteClass, usize)>,
}

impl GenNfa {
    fn new() -> GenNfa {
        GenNfa { states: Vec::new() }
    }
    fn add(&mut self) -> usize {
        self.states.push(GenState {
            eps: Vec::new(),
            edges: Vec::new(),
        });
        self.states.len() - 1
    }

    /// Returns an `(in, out)` fragment matching exactly the bytes `s`.
    fn literal(&mut self, s: &[u8]) -> (usize, usize) {
        let start = self.add();
        let mut cur = start;
        for &b in s {
            let nxt = self.add();
            let mut cl = ByteClass::empty();
            cl.set(b);
            self.states[cur].edges.push((cl, nxt));
            cur = nxt;
        }
        (start, cur)
    }

    /// Returns an `(in, out)` fragment matching the language of `dfa`. The dead
    /// sink is dropped so it never enters the assembled automaton.
    fn from_dfa(&mut self, dfa: &Dfa) -> (usize, usize) {
        let dead = dfa.dead_state();
        let n = dfa.num_states();
        let mut map = vec![usize::MAX; n];
        for (s, m) in map.iter_mut().enumerate() {
            if Some(s) != dead {
                *m = self.add();
            }
        }
        let entry = self.add();
        let exit = self.add();
        self.states[entry].eps.push(map[dfa.start as usize]);
        for s in 0..n {
            if Some(s) == dead {
                continue;
            }
            // One class per target state.
            let mut by_target: std::collections::HashMap<usize, ByteClass> =
                std::collections::HashMap::new();
            for b in 0..256usize {
                let t = dfa.table[s * 256 + b] as usize;
                if Some(t) == dead {
                    continue;
                }
                by_target
                    .entry(t)
                    .or_insert_with(ByteClass::empty)
                    .set(b as u8);
            }
            for (t, cl) in by_target {
                self.states[map[s]].edges.push((cl, map[t]));
            }
            if dfa.accepting[s] {
                self.states[map[s]].eps.push(exit);
            }
        }
        (entry, exit)
    }

    fn eps_closure(&self, seed: &[usize]) -> Vec<usize> {
        let mut seen = vec![false; self.states.len()];
        let mut stack: Vec<usize> = seed.to_vec();
        let mut out = Vec::new();
        while let Some(s) = stack.pop() {
            if seen[s] {
                continue;
            }
            seen[s] = true;
            out.push(s);
            for &n in &self.states[s].eps {
                if !seen[n] {
                    stack.push(n);
                }
            }
        }
        out.sort_unstable();
        out
    }

    /// Subset-constructs the DFA of this NFA from `start` to its single `accept`.
    fn to_dfa(&self, start: usize, accept: usize) -> Dfa {
        use std::collections::HashMap;
        let mut index: HashMap<Vec<usize>, u32> = HashMap::new();
        let mut sets: Vec<Vec<usize>> = Vec::new();
        let mut table: Vec<u32> = Vec::new();
        let mut accepting: Vec<bool> = Vec::new();
        let start_set = self.eps_closure(&[start]);
        index.insert(start_set.clone(), 0);
        sets.push(start_set);
        let mut processed = 0usize;
        while processed < sets.len() {
            let cur = sets[processed].clone();
            if table.len() < (processed + 1) * 256 {
                table.resize((processed + 1) * 256, 0);
            }
            accepting.resize(processed + 1, false);
            accepting[processed] = cur.contains(&accept);
            for byte in 0..256usize {
                let mut moved: Vec<usize> = Vec::new();
                for &s in &cur {
                    for (cl, to) in &self.states[s].edges {
                        if cl.contains(byte as u8) {
                            moved.push(*to);
                        }
                    }
                }
                let closure = self.eps_closure(&moved);
                let next = *index.entry(closure.clone()).or_insert_with(|| {
                    let id = sets.len() as u32;
                    sets.push(closure);
                    id
                });
                table[processed * 256 + byte] = next;
            }
            processed += 1;
        }
        table.resize(sets.len() * 256, 0);
        accepting.resize(sets.len(), false);
        for (i, set) in sets.iter().enumerate() {
            accepting[i] = set.contains(&accept);
        }
        Dfa {
            start: 0,
            accepting,
            table,
        }
    }
}

/// Returns the DFA of the concatenation of literal runs and hole DFAs: the
/// language of an interpolation whose holes are finite string types.
/// An empty list denotes `{""}`.
pub fn concat_language(pieces: &[ConcatPiece]) -> Dfa {
    let mut nfa = GenNfa::new();
    // An epsilon-only seed makes an empty list yield `{""}`.
    let start = nfa.add();
    let mut tail = start;
    for piece in pieces {
        let (fin, fout) = match piece {
            ConcatPiece::Lit(s) => nfa.literal(s),
            ConcatPiece::Dfa(d) => nfa.from_dfa(d),
        };
        nfa.states[tail].eps.push(fin);
        tail = fout;
    }
    nfa.to_dfa(start, tail)
}

impl Dfa {
    /// Returns the dead sink: a non-accepting state whose every transition loops
    /// to itself.
    fn dead_state(&self) -> Option<usize> {
        (0..self.num_states()).find(|&s| {
            !self.accepting[s] && (0..256usize).all(|b| self.table[s * 256 + b] as usize == s)
        })
    }
}

/// Compiles a pattern to a DFA, or returns a readable error.
///
/// Memoized, errors included, once per process: the checker asks for the same
/// DFA at every value boundary, on every thread that types bodies, and
/// rebuilding one table measured 96% of checker time. A clone is a ~1 MB
/// memcpy against a ~25 ms rebuild. A thread that asks while another builds
/// the pattern waits for that build.
pub fn compile(pattern: &str) -> Result<Dfa, Refusal> {
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};
    type Slot = Arc<OnceLock<Result<Dfa, Refusal>>>;
    // ponytail: unbounded, but keyed by patterns in the compiled source. Add an
    // LRU if a long-lived process compiles unbounded distinct patterns.
    static MEMO: Mutex<std::collections::BTreeMap<String, Slot>> =
        Mutex::new(std::collections::BTreeMap::new());
    let slot = {
        let mut memo = MEMO.lock().unwrap_or_else(PoisonError::into_inner);
        match memo.get(pattern) {
            Some(slot) => slot.clone(),
            None => memo.entry(pattern.to_string()).or_default().clone(),
        }
    };
    slot.get_or_init(|| compile_uncached(pattern)).clone()
}

/// Ceiling on DFA states. `EXPANSION_BUDGET` bounds the pattern, but its
/// derivatives can still explode (`(a|b)*a(a|b){20}` needs 2^21 states), so a
/// pathological pattern is a compile error, not a hang.
const DFA_STATE_BUDGET: usize = 8_192;

/// The DFA of `pattern` by derivatives: a state is the term of the rest of the text
/// and its components, and the state a run of bytes leads to derives each of them
/// by the run's first byte. A state accepts where its term matches the empty span
/// at the end.
fn compile_uncached(pattern: &str) -> Result<Dfa, Refusal> {
    let re = parse(pattern)?;
    let mut all = Vec::new();
    classes(&re, &mut all);
    // `firsts` holds the first byte of each run of bytes every class holds alike.
    let firsts: Vec<u8> = std::iter::once(0)
        .chain((1..=255u8).filter(|&b| all.iter().any(|c| c.contains(b) != c.contains(b - 1))))
        .collect();
    let mut terms = Terms::new();
    let start = (terms.of(&re), terms.first_context());
    let mut number = std::collections::HashMap::from([(start, 0u32)]);
    let mut states = vec![start];
    let mut table: Vec<u32> = Vec::new();
    let mut accepting = Vec::new();
    let mut k = 0;
    while k < states.len() {
        if k >= DFA_STATE_BUDGET {
            return Err(Refusal::Builder(format!(
                "pattern compiles past the maximum ({DFA_STATE_BUDGET} DFA states)"
            )));
        }
        let (t, cx) = states[k];
        accepting.push(terms.ends(t, cx));
        let mut run = 0;
        for b in 0..=255u8 {
            if firsts.get(run + 1) == Some(&b) {
                run += 1;
            }
            let next = match firsts[run] == b {
                true => {
                    let comps = terms.contexts[cx as usize].clone();
                    let comps = comps.iter().map(|&c| terms.derive(c, b, cx)).collect();
                    let d = (terms.derive(t, b, cx), terms.context(comps));
                    *number.entry(d).or_insert_with(|| {
                        states.push(d);
                        states.len() as u32 - 1
                    })
                }
                false => table[table.len() - 1],
            };
            table.push(next);
        }
        k += 1;
    }
    let dfa = Dfa {
        start: 0,
        accepting,
        table,
    };
    Ok(minimal(&dfa, &firsts))
}

/// The minimal DFA of `dfa`'s language, its states numbered breadth-first from the
/// start in byte order. A minimal DFA is unique up to numbering, so the table depends
/// on the language alone. `firsts` holds the first byte of each run of bytes that
/// every state steps alike, ascending.
///
/// ponytail: Moore's refinement, O(rounds x states x runs) with at most one round per
/// state; Hopcroft's O(runs x states x log states) if a pattern ever needs it.
fn minimal(dfa: &Dfa, firsts: &[u8]) -> Dfa {
    use std::collections::HashMap;
    let n = dfa.num_states();
    let step = |s: usize, b: u8| dfa.table[s * 256 + usize::from(b)] as usize;
    // Each round splits a block or ends: the count of blocks rises to at most `n`.
    let mut block: Vec<u32> = dfa.accepting.iter().map(|&a| u32::from(a)).collect();
    let mut count = 0;
    loop {
        let mut ids: HashMap<Vec<u32>, u32> = HashMap::new();
        let next: Vec<u32> = (0..n)
            .map(|s| {
                let key: Vec<u32> = std::iter::once(block[s])
                    .chain(firsts.iter().map(|&b| block[step(s, b)]))
                    .collect();
                let id = ids.len() as u32;
                *ids.entry(key).or_insert(id)
            })
            .collect();
        block = next;
        if ids.len() == count {
            break;
        }
        count = ids.len();
    }
    // `rep[k]` is a state of block `k`; `order` lists the blocks as the walk meets them.
    let mut rep = vec![usize::MAX; count];
    for s in (0..n).rev() {
        rep[block[s] as usize] = s;
    }
    let mut number = vec![u32::MAX; count];
    let mut order = vec![block[dfa.start as usize]];
    number[order[0] as usize] = 0;
    let mut table = Vec::new();
    let mut k = 0;
    while k < order.len() {
        let s = rep[order[k] as usize];
        for b in 0..=255u8 {
            let t = block[step(s, b)] as usize;
            if number[t] == u32::MAX {
                number[t] = order.len() as u32;
                order.push(t as u32);
            }
            table.push(number[t]);
        }
        k += 1;
    }
    let accepting = order
        .iter()
        .map(|&k| dfa.accepting[rep[k as usize]])
        .collect();
    Dfa {
        start: 0,
        accepting,
        table,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pat: &str, s: &str) -> bool {
        compile(pat).unwrap().matches(s)
    }

    #[test]
    fn literals_full_match() {
        assert!(m("abc", "abc"));
        assert!(!m("abc", "ab"));
        assert!(!m("abc", "abcd"));
    }

    #[test]
    fn char_class_and_plus() {
        assert!(m("[a-z]+", "hello"));
        assert!(!m("[a-z]+", "Hello"));
        assert!(!m("[a-z]+", ""));
    }

    #[test]
    fn digit_shorthand_and_star() {
        assert!(m("\\d*", ""));
        assert!(m("\\d*", "0042"));
        assert!(!m("\\d*", "0a"));
    }

    #[test]
    fn word_class() {
        assert!(m("\\w+", "id_42"));
        assert!(!m("\\w+", "no space"));
    }

    #[test]
    fn dot_and_optional() {
        assert!(m("a.c", "abc"));
        assert!(m("a.c", "axc"));
        assert!(m("colou?r", "color"));
        assert!(m("colou?r", "colour"));
        assert!(!m("colou?r", "colouur"));
    }

    #[test]
    fn negated_class() {
        assert!(m("[^0-9]+", "abc"));
        assert!(!m("[^0-9]+", "ab9"));
    }

    #[test]
    fn escaped_metachars_are_literal() {
        assert!(m("a\\.b", "a.b"));
        assert!(!m("a\\.b", "axb"));
    }

    #[test]
    fn control_escapes_mean_control_characters() {
        // ECMA-262: `\n` matches LF, not the letter `n`; otherwise `[^\n]` inverts.
        assert!(m("\\n", "\n"));
        assert!(!m("\\n", "n"));
        assert!(m("\\t", "\t"));
        assert!(!m("\\t", "t"));
        assert!(m("\\r", "\r"));
        assert!(!m("\\r", "r"));
        assert!(m("\\f", "\u{0c}"));
        assert!(!m("\\f", "f"));
        assert!(m("\\v", "\u{0b}"));
        assert!(!m("\\v", "v"));
        // Inside a class too.
        assert!(m("[\\n\\t]", "\n"));
        assert!(!m("[\\n]", "n"));
        assert!(m("[^\\n]*", "no newline here"));
        assert!(!m("[^\\n]*", "a\nb"));
    }

    #[test]
    fn email_ish() {
        let p = "[a-z0-9_.]+@[a-z0-9]+\\.[a-z]+";
        assert!(m(p, "user.name_1@example.com"));
        assert!(!m(p, "no-at-sign"));
        assert!(!m(p, "user@@x.com"));
    }

    #[test]
    fn alternation() {
        assert!(m("cat|dog", "cat"));
        assert!(m("cat|dog", "dog"));
        assert!(!m("cat|dog", "cow"));
        assert!(m("(yes|no|maybe)", "maybe"));
    }

    #[test]
    fn groups_with_quantifiers() {
        assert!(m("(ab)+", "ababab"));
        assert!(!m("(ab)+", "aba"));
        assert!(m("(ab)*", ""));
        assert!(m("a(bc)?d", "ad"));
        assert!(m("a(bc)?d", "abcd"));
    }

    #[test]
    fn a_lazy_repeat_matches_the_strings_its_greedy_one_does() {
        assert!(m("a+?b*?c??", "aaab"));
        assert!(m("a{2,3}?", "aaa"));
        assert!(!m("a+?", ""));
    }

    #[test]
    fn every_second_char_is_a() {
        // Even length, and every odd-index char is 'a'.
        let p = "(.a)*";
        assert!(m(p, ""));
        assert!(m(p, "xa"));
        assert!(m(p, "xaya1a"));
        assert!(!m(p, "xayb")); // 4th char is not 'a'
        assert!(!m(p, "xay")); // odd length
    }

    #[test]
    fn nested_groups_and_alternation() {
        let p = "(ab|cd)+";
        assert!(m(p, "abcdab"));
        assert!(!m(p, "abc"));
    }

    #[test]
    fn rejects_unsupported_syntax() {
        assert!(compile("*a").is_err()); // nothing to repeat
        assert!(compile("{2,3}").is_err()); // nothing to repeat (counted)
        assert!(compile("[a-z").is_err()); // unterminated class
        assert!(compile("(ab").is_err()); // unclosed group
        assert!(compile("ab)").is_err()); // unmatched close
        assert!(compile("a}").is_err()); // unmatched brace (literal is \})
        assert!(compile("[z-a]").is_err()); // reversed class range
        assert!(compile("a{3,2}").is_err()); // max < min
        assert!(compile("[]").is_err()); // empty class
        assert!(compile("[^]").is_err()); // empty negated class
        assert!(compile("a{999}").is_err()); // over the 255 cap
        assert!(compile("a{2,").is_err()); // unclosed counted repetition
    }

    #[test]
    fn an_escape_is_a_control_a_code_point_a_shorthand_or_punctuation() {
        assert!(m("\\a\\e\\cJ\\cj", "\u{7}\u{1b}\n\n"));
        assert!(m("\\x41\\x{42}\\u0043\\u{1F600}\\o{101}", "ABC\u{1F600}A"));
        assert!(m("\\u{e9}+", "\u{e9}\u{e9}"));
        assert!(m("\\h\\H", "\tx"));
        assert!(!m("\\h", "\n"));
        assert!(m("\\N", "x"));
        assert!(!m("\\N", "\n"));
        assert!(m("a\\Rb", "a\r\nb"));
        assert!(m("a\\Rb", "a\u{0c}b"));
        assert!(m("\\Q.*\\E", ".*"));
        assert!(m("\\Qab*", "ab*"));
        assert!(m("\\Qab\\E*", "abbb"));
        assert!(m("\\Q\\E", ""));
        assert!(m("[\\b\\x41]", "\u{8}"));
        assert!(m("\\-\\.\\\\", "-.\\"));
        assert!(m("[\\<]", "<"));
    }

    #[test]
    fn a_dot_a_negated_class_and_a_literal_are_one_character() {
        assert!(m(".", "\u{e9}"));
        assert!(m(".", "\u{1F600}"));
        assert!(!m(".", "\u{e9}\u{e9}"));
        assert!(m("[^a]", "\u{20AC}"));
        assert!(m("\u{e9}+", "\u{e9}\u{e9}"));
        assert!(!m("[\u{e9}]", "\u{c3}"));
        assert!(m("[\u{e0}-\u{ff}]{2}", "\u{e9}\u{ff}"));
        assert!(m("[\\x{100}-\\x{10FFFF}]", "\u{10FFFF}"));
        assert!(!m("[\\x{100}-\\x{10FFFF}]", "\u{ff}"));
        assert!(m("\\W", "\u{e9}"));
    }

    #[test]
    fn a_class_has_posix_names_nesting_and_set_operations() {
        assert!(m("[[:alpha:][:digit:]_]+", "a1_Z"));
        assert!(m("[[:^alpha:]]", "1"));
        assert!(!m("[[:^alpha:]]", "a"));
        assert!(m("[a-z&&[^aeiou]]+", "xyz"));
        assert!(!m("[a-z&&[^aeiou]]", "a"));
        assert!(m("[\\w--\\d]", "a"));
        assert!(!m("[\\w--\\d]", "1"));
        assert!(m("[a-c~~b-d]+", "ad"));
        assert!(!m("[a-c~~b-d]", "b"));
        assert!(m("[a[xy]]", "y"));
        assert!(m("[\\x41-\\x43]", "B"));
        assert!(m("[a-]", "-"));
        assert!(m("[a&b]", "&"));
        assert!(!m("[a&&b]", "a"));
    }

    #[test]
    fn a_class_form_with_no_meaning_is_refused() {
        let says = |p: &str| compile(p).err().map(|e| e.to_string());
        let says = |p: &str, s: &str| assert_eq!(says(p).as_deref(), Some(s), "{p}");
        says(
            "[\\d-z]",
            "a range (`-`) whose end is a shorthand or a class",
        );
        says(
            "[a-\\w]",
            "a range (`-`) whose end is a shorthand or a class",
        );
        says(
            "[[:alpha:]-z]",
            "a range (`-`) whose end is a shorthand or a class",
        );
        says("[[:alfa:]]", "a POSIX class (`[:..:]`) whose name is none of alnum, alpha, blank, cntrl, digit, graph, lower, print, punct, space, upper and xdigit");
        says(
            "[a&&]",
            "a class set operation (`&&`, `--` or `~~`) with no members on one side",
        );
        says(
            "[&&a]",
            "a class set operation (`&&`, `--` or `~~`) with no members on one side",
        );
        says("[a-", "a character class with no closing `]`");
        says("[\u{e9}-a]", "a character range that runs backwards");
    }

    #[test]
    fn a_group_form_names_folds_or_comments() {
        assert!(m("(?:ab)+", "abab"));
        assert!(m("(?<y>\\d{4})-(?P<m>\\d\\d)-(?'d'\\d\\d)", "2026-11-27"));
        assert!(m("a(?#note)b", "ab"));
        assert!(m("(?i)abc", "aBC"));
        assert!(m("a(?i)b|c", "C"));
        assert!(!m("(?i:a)b", "AB"));
        assert!(m("(?i:a)b", "Ab"));
        assert!(m("(?i)[a-c]+", "ABC"));
        assert!(!m("(?i)[^a]", "A"));
        assert!(m("(?i)\\x41", "a"));
        assert!(!m("(?i)\u{e9}", "\u{c9}"));
        assert!(m("(?s).", "\n"));
        assert!(!m(".", "\n"));
        assert!(m("(?is-i:A)a", "Aa"));
        assert!(!m("(?is-i:A)a", "aa"));
        assert!(m("(?m)a", "a"));
    }

    #[test]
    fn a_group_form_with_no_meaning_is_refused() {
        let says = |p: &str| compile(p).err().map(|e| e.to_string());
        let says = |p: &str, s: &str| assert_eq!(says(p).as_deref(), Some(s), "{p}");
        says("(?x)a", "a flag (`x`) that is none of `i`, `m` and `s`");
        says(
            "(?=a)*",
            "a repeat after an assertion, which matches no character",
        );
        says(
            "a^{2}",
            "a repeat after an assertion, which matches no character",
        );
        says(
            "(?=(a))",
            "a capturing group inside a lookaround: write `(?:..)`",
        );
        says(
            "(?<!(?<n>a))",
            "a capturing group inside a lookaround: write `(?:..)`",
        );
        says(
            "(?>a)",
            "a PCRE control form (`(?>`), which only a backtracking engine has",
        );
        says(
            "(?R)",
            "a PCRE control form (`(?R)`), which only a backtracking engine has",
        );
        says("(?<1a>b)", "a group name that is not a letter or `_` and then letters, digits and `_` up to its closing `>` or `'`");
        says("(?<a>b)(?<a>c)", "a group name that names two groups");
        says(
            "(?~a)",
            "a group form after `(?` that is none of `:`, a name, flags and `#`",
        );
        says("(?#a", "a group with no closing `)`");
        assert!(compile("(?<a>b){2}").is_ok());
    }

    #[test]
    fn a_backreference_is_not_regular() {
        let each = [
            "(a)\\1",
            "(a)\\g1",
            "(a)\\g{1}",
            "(a)(b)\\g{-2}",
            "(?<n>a)\\k<n>",
            "(?<n>a)\\k'n'",
            "(?<n>a)\\k{n}",
            "(?P<n>a)(?P=n)",
            "\\2(a)(b)",
            "\\k<n>(?<n>a)",
            "(a){0}\\1",
        ];
        for p in each {
            assert_eq!(compile(p).err(), Some(Refusal::NotRegular), "{p}");
        }
        let missing = Some(Refusal::Builder(
            "a backreference to a group the pattern does not have".to_string(),
        ));
        let none = [
            "\\1",
            "(a)\\2",
            "(?:a)\\1",
            "(a)\\g{-2}",
            "(a)\\g0",
            "(?<n>a)\\k<m>",
        ];
        for p in none {
            assert_eq!(compile(p).err(), missing, "{p}");
        }
    }

    #[test]
    fn an_escape_with_no_meaning_is_refused() {
        let says = |p: &str| compile(p).err().map(|e| e.to_string());
        let says = |p: &str, s: &str| assert_eq!(says(p).as_deref(), Some(s), "{p}");
        says(
            "\\y",
            "the escape `\\y` means nothing: a backslash makes only ASCII punctuation literal",
        );
        says(
            "\\b+",
            "a repeat after an assertion, which matches no character",
        );
        says(
            "[\\A]",
            "an anchor or a backreference (`\\A`) cannot stand inside a character class",
        );
        says(
            "\\K",
            "a PCRE control form (`\\K`), which only a backtracking engine has",
        );
        says(
            "\\p{L}",
            "a Unicode property class (`\\p{..}`) is not supported",
        );
        says("\\0", "a NUL escape, which no `String` holds");
        says("\\x{000}", "a NUL escape, which no `String` holds");
        says("\\012", "an octal escape (`\\0` and a digit), which PCRE reads as a backreference: write `\\x` and two hex digits");
        says(
            "\\c1",
            "a control escape (`\\c`) with no ASCII letter after it",
        );
        says("\\x4", "a code point escape (`\\x`) with no digits of its form: `\\xHH`, `\\uHHHH`, `\\x{H..}`, `\\u{H..}` or `\\o{O..}`");
        says(
            "\\x{110000}",
            "a code point escape above `10FFFF` or in the surrogates `D800` to `DFFF`",
        );
        says(
            "\\u{D800}",
            "a code point escape above `10FFFF` or in the surrogates `D800` to `DFFF`",
        );
        says(
            "\\\u{e9}",
            "the escape `\\\u{e9}` means nothing: a backslash makes only ASCII punctuation literal",
        );
    }

    #[test]
    fn a_refusal_is_the_catalogues_sentence() {
        assert_eq!(
            compile("[z-a]").err(),
            Some(Refusal::Builder(
                "a character range that runs backwards".to_string()
            ))
        );
        assert_eq!(
            compile("a}").err(),
            Some(Refusal::Grammar(
                "a `}` with no `{` before it (a literal brace is `\\}`)".to_string()
            ))
        );
    }

    #[test]
    fn nested_counted_repetition_hits_the_expansion_budget() {
        // Hash-shaped patterns stay fine.
        assert!(compile("[0-9a-f]{40}").is_ok());
        assert!(compile("([0-9a-f]{2}){10}").is_ok());
        // Nesting multiplies the counts: a compile error, not a hang.
        let e = compile("(a{200}){200}").unwrap_err().to_string();
        assert_eq!(
            e,
            "counted repetition that expands the pattern past 16384 terms"
        );
        // Just inside the budget still compiles.
        assert!(compile("(a{200}){40}").is_ok());
    }

    #[test]
    fn deeply_nested_groups_hit_the_depth_budget() {
        // A hundred nested groups are fine.
        let ok = format!("{}{}", "(".repeat(100), ")".repeat(100));
        assert!(compile(&ok).is_ok());
        // Thousands of parens: a compile error, not a stack overflow.
        let deep = format!("{}{}", "(".repeat(1000), ")".repeat(1000));
        let e = compile(&deep).unwrap_err().to_string();
        assert!(e.contains("deep"), "message: {e}");
    }

    #[test]
    fn the_depth_budget_counts_nesting_not_groups() {
        assert!(compile(&"(a)".repeat(250)).is_ok());
        let past = format!("{}{}", "(".repeat(250), ")".repeat(250));
        assert_eq!(
            compile(&past).err(),
            Some(Refusal::Grammar(
                "a pattern that nests more than 250 levels deep".to_string()
            ))
        );
    }

    #[test]
    fn exponential_subset_construction_hits_the_state_budget() {
        // About 70 NFA states, but the DFA needs 2^21 states: the state ceiling makes
        // it a compile error.
        let e = compile("(a|b)*a(a|b){20}").unwrap_err().to_string();
        assert!(e.contains("DFA states"), "message: {e}");
        // Real-world sized DFAs still compile.
        assert!(compile("[0-9a-f]{40}").is_ok());
        assert!(compile("nav\\.(home|about|settings)\\.label").is_ok());
    }

    #[test]
    fn a_dfa_is_minimal_and_its_table_depends_on_the_language_alone() {
        let (a, b) = (dfa("(a|b)*a|(a|b)*b"), dfa("[ab]+"));
        assert_eq!(
            (a.start, &a.accepting, &a.table),
            (b.start, &b.accepting, &b.table)
        );
        // The start, then the dead state byte 0 reaches, then the accepting loop.
        assert_eq!(dfa("(aa*|a)+").accepting, [false, false, true]);
    }

    #[test]
    fn counted_repetition() {
        assert!(m("a{3}", "aaa"));
        assert!(!m("a{3}", "aa"));
        assert!(!m("a{3}", "aaaa"));
        assert!(m("a{2,4}", "aa"));
        assert!(m("a{2,4}", "aaaa"));
        assert!(!m("a{2,4}", "a"));
        assert!(!m("a{2,4}", "aaaaa"));
        assert!(m("a{2,}", "aaaaaaa"));
        assert!(!m("a{2,}", "a"));
        assert!(m("(ab){2}", "abab"));
        assert!(!m("(ab){2}", "ab"));
        assert!(m("[0-9]{3}-[0-9]{4}", "555-0123"));
        assert!(!m("[0-9]{3}-[0-9]{4}", "55-0123"));
        assert!(m("a{0,2}", "")); // zero minimum matches empty
    }

    #[test]
    fn dot_excludes_line_terminators() {
        // ECMA-262 `.`: any character except a line terminator.
        assert!(m("a.b", "axb"));
        assert!(!m("a.b", "a\nb"));
        assert!(!m("a.b", "a\rb"));
        // An explicit negated class stays byte-wise and CAN match `\n`.
        assert!(m("a[^x]b", "a\nb"));
    }

    fn dfa(pat: &str) -> Dfa {
        compile(pat).unwrap()
    }

    #[test]
    fn finiteness_of_languages() {
        // Finite: literals, alternations of finite branches, counted {m,n}.
        assert!(dfa("abc").is_finite());
        assert!(dfa("cat|dog|maybe").is_finite());
        assert!(dfa("a{2,4}").is_finite());
        assert!(dfa("(home\\.(title|subtitle)|nav\\.(home|about)\\.label)").is_finite());
        assert!(dfa("nav\\.(home|about|settings)\\.label").is_finite());
        // Infinite: any unbounded repetition on an accepting path.
        assert!(!dfa("a+").is_finite());
        assert!(!dfa("a*").is_finite());
        assert!(!dfa("a{2,}").is_finite());
        assert!(!dfa("[a-z]+").is_finite());
        assert!(!dfa("x(ab)*y").is_finite());
        assert!(dfa("ab").is_finite());
    }

    #[test]
    fn enumeration_is_exact_and_ordered() {
        assert_eq!(
            dfa("cat|dog|bird").enumerate(10),
            Some(vec![
                "bird".to_string(),
                "cat".to_string(),
                "dog".to_string()
            ])
        );
        // Lexicographic (byte) order, deterministic.
        assert_eq!(
            dfa("nav\\.(home|about)\\.label").enumerate(10),
            Some(vec![
                "nav.about.label".to_string(),
                "nav.home.label".to_string()
            ])
        );
        // A single literal is a one-element language (`.` escaped to a literal).
        assert_eq!(dfa("home\\.title").enumerate(10).map(|v| v.len()), Some(1));
        // {m,n} enumerates every count.
        assert_eq!(
            dfa("a{1,3}").enumerate(10),
            Some(vec!["a".to_string(), "aa".to_string(), "aaa".to_string()])
        );
    }

    #[test]
    fn enumeration_respects_cap_and_infinity() {
        // Over the cap: None.
        assert_eq!(dfa("cat|dog|bird").enumerate(2), None);
        // An infinite language: None at any cap.
        assert_eq!(dfa("[a-z]+").enumerate(1000), None);
        // Exactly at the cap: Some.
        assert!(dfa("cat|dog|bird").enumerate(3).is_some());
    }

    #[test]
    fn enumeration_terminates_on_wide_counted_repetitions() {
        // 16^40 members, every accept at depth 40: the first over-cap successor list
        // settles it.
        assert_eq!(dfa("[0-9a-f]{40}").enumerate(1000), None);
        // Under the cap: all 256 two-byte members.
        assert_eq!(
            dfa("[0-9a-f]{2}").enumerate(1000).map(|v| v.len()),
            Some(256)
        );
        // The same bound in `enumerate_without`: 8 members fit a cap of 8, not 7.
        assert_eq!(
            dfa("(a|b){3}").enumerate_without(b' ', 8).map(|v| v.len()),
            Some(8)
        );
        assert_eq!(dfa("(a|b){3}").enumerate_without(b' ', 7), None);
    }

    #[test]
    fn containment_holds_and_fails_with_shortest_witness() {
        let keys = dfa("nav\\.(home|about|settings)\\.label");
        let sub = dfa("nav\\.(home|about)\\.label");
        assert_eq!(contains(&keys, &sub), Ok(()));
        // Self-containment.
        assert_eq!(contains(&keys, &keys), Ok(()));
        // Not contained: `settings` is in sub but a narrower sup forbids it.
        let narrow = dfa("nav\\.(home|about)\\.label");
        let wide = dfa("nav\\.(home|about|settings)\\.label");
        match contains(&narrow, &wide) {
            Err(w) => assert_eq!(w, "nav.settings.label"),
            Ok(()) => panic!("expected a witness"),
        }
    }

    #[test]
    fn containment_witness_is_shortest() {
        // `sup`: length at least 3, starting `a`; `sub` accepts `a`.
        let sup = dfa("a.+");
        let sub = dfa("a|abcd");
        match contains(&sup, &sub) {
            Err(w) => assert_eq!(w, "a"), // the shortest member of sub \ sup
            Ok(()) => panic!("expected a witness"),
        }
    }

    #[test]
    fn empty_language_is_contained_in_everything() {
        // `sub` accepting nothing is a subset of any `sup`. Use an intersection
        // of disjoint literals as a guaranteed-empty language.
        let a = dfa("abc");
        let b = dfa("xyz");
        let empty = intersect(&a, &b);
        assert_eq!(empty.enumerate(10), Some(vec![]));
        assert_eq!(contains(&dfa("abc"), &empty), Ok(()));
    }

    #[test]
    fn intersection_is_the_conjunction_language() {
        // Three lowercase letters with `a` in the middle.
        let inter = intersect(&dfa("[a-z]{3}"), &dfa(".a."));
        assert!(inter.matches("cat"));
        assert!(inter.matches("bat"));
        assert!(!inter.matches("cot"));
        assert!(!inter.matches("ca")); // wrong length
    }

    #[test]
    fn concat_of_literals_and_holes() {
        let section = dfa("home|about");
        let l = concat_language(&[
            ConcatPiece::Lit(b"nav."),
            ConcatPiece::Dfa(&section),
            ConcatPiece::Lit(b".label"),
        ]);
        assert!(l.matches("nav.home.label"));
        assert!(l.matches("nav.about.label"));
        assert!(!l.matches("nav.settings.label"));
        assert!(!l.matches("nav.home.label.x"));
        assert_eq!(
            l.enumerate(10),
            Some(vec![
                "nav.about.label".to_string(),
                "nav.home.label".to_string()
            ])
        );
        // Empty piece list denotes {""}.
        assert_eq!(
            concat_language(&[]).enumerate(10),
            Some(vec!["".to_string()])
        );
        // A hole whose language includes the empty string concatenates correctly.
        let opt = dfa("x?");
        let l2 = concat_language(&[
            ConcatPiece::Lit(b"a"),
            ConcatPiece::Dfa(&opt),
            ConcatPiece::Lit(b"b"),
        ]);
        assert!(l2.matches("ab"));
        assert!(l2.matches("axb"));
        assert!(!l2.matches("axxb"));
    }

    #[test]
    fn concat_containment_end_to_end() {
        let trans_key = dfa("(home\\.(title|subtitle)|nav\\.(home|about|settings)\\.label)");
        let section = dfa("home|about|settings");
        let l = concat_language(&[
            ConcatPiece::Lit(b"nav."),
            ConcatPiece::Dfa(&section),
            ConcatPiece::Lit(b".label"),
        ]);
        assert_eq!(contains(&trans_key, &l), Ok(()));
        // A section key TransKey lacks gives a witness.
        let bad_section = dfa("home|about|profile");
        let bad = concat_language(&[
            ConcatPiece::Lit(b"nav."),
            ConcatPiece::Dfa(&bad_section),
            ConcatPiece::Lit(b".label"),
        ]);
        match contains(&trans_key, &bad) {
            Err(w) => assert_eq!(w, "nav.profile.label"),
            Ok(()) => panic!("expected a witness"),
        }
    }

    #[test]
    fn an_assertion_holds_at_a_position_and_takes_no_character() {
        assert!(m("^abc$", "abc"));
        assert_eq!(dfa("a^b").enumerate(1), Some(vec![]));
        assert!(m("(?m)a$\\n^b", "a\nb"));
        assert!(!m("a$\\nb", "a\nb"));
        assert!(m("a\\Z\\n", "a\n"));
        assert!(m("a\\b-", "a-"));
        assert!(m("a\\Bb", "ab") && !m("a\\bb", "ab"));
        assert!(m("(?!\\.)[a-z.]+", "a.b") && !m("(?!\\.)[a-z.]+", ".ab"));
        assert!(m("[a-z]+(?<!x)", "abc") && !m("[a-z]+(?<!x)", "abx"));
        // A lookbehind whose body grows, and one inside a lookahead.
        assert!(m("a+(?<=^a{2,3})b", "aab") && !m("a+(?<=^a{2,3})b", "aaaab"));
        assert!(m("(?=a(?<=^a))a", "a"));
        // No string is wholly `(?<=a)b`: the lookbehind looks before the text.
        assert_eq!(dfa("(?<=a)b").enumerate(1), Some(vec![]));
        assert!(m("\\<a+\\>", "aa") && !m("x\\b{start-half}", "x"));
    }

    #[test]
    fn a_language_with_an_assertion_is_proved_by_containment() {
        // A known result: the toy of the design note is the DFA of `[a-z]+`.
        let (a, b) = (dfa("(?!\\.)[a-z]+"), dfa("[a-z]+"));
        assert_eq!((&a.accepting, &a.table), (&b.accepting, &b.table));
        // The empty language is in every language and finite.
        let empty = dfa("a^b");
        assert!(empty.is_finite() && contains(&dfa("x"), &empty).is_ok());
        // A singleton.
        let one = dfa("^(?=ab)a(?<=a)b$");
        assert_eq!(one.enumerate(4), Some(vec!["ab".to_string()]));
        assert!(contains(&dfa("[ab]+"), &one).is_ok());
        // Adversarial: the witness is longer than any literal of the pattern.
        let sup = dfa("(?:a|b)*(?<!aaa)");
        assert_eq!(contains(&sup, &dfa("a*")), Err("aaa".to_string()));
    }

    /// The ends of the spans `re` matches in `text` from `i`, by the definition of
    /// each form: a test oracle with no derivative in it.
    fn spans(re: &Re, text: &[u8], i: usize) -> std::collections::BTreeSet<usize> {
        use std::collections::BTreeSet;
        let after = |from: &BTreeSet<usize>, r: &Re| -> BTreeSet<usize> {
            from.iter().flat_map(|&j| spans(r, text, j)).collect()
        };
        match re {
            Re::Empty => BTreeSet::from([i]),
            Re::Class(c) => (text.get(i).filter(|&&b| c.contains(b)))
                .map(|_| i + 1)
                .into_iter()
                .collect(),
            Re::Concat(ps) => ps.iter().fold(BTreeSet::from([i]), |at, p| after(&at, p)),
            Re::Alt(bs) => bs.iter().flat_map(|b| spans(b, text, i)).collect(),
            Re::Opt(r) => spans(r, text, i).into_iter().chain([i]).collect(),
            Re::Plus(r) => {
                let first = spans(r, text, i);
                let star = Re::Star(r.clone());
                after(&first, &star)
            }
            Re::Star(r) => {
                let mut reach = BTreeSet::from([i]);
                loop {
                    let more: BTreeSet<usize> = &reach | &after(&reach, r);
                    if more == reach {
                        return reach;
                    }
                    reach = more;
                }
            }
            Re::Look {
                ahead,
                negate,
                body,
            } => {
                let holds = match ahead {
                    true => !spans(body, text, i).is_empty(),
                    false => (0..=i).any(|j| spans(body, text, j).contains(&i)),
                };
                match holds != *negate {
                    true => BTreeSet::from([i]),
                    false => BTreeSet::new(),
                }
            }
        }
    }

    /// A draw below `n` from the generator state `seed`, which it advances.
    fn draw(seed: &mut u64, n: usize) -> usize {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (*seed >> 33) as usize % n
    }

    /// A pattern over the assertions and the atoms they look at.
    fn assertion_pattern(seed: &mut u64, depth: u32) -> String {
        let atoms = [
            "a", "b", "[ab]", "\\w", "\\W", ".", "-", "\\n", "^", "$", "\\b", "\\B", "\\A", "\\z",
            "\\Z", "\\<", "\\>", "(?m)",
        ];
        let opens = ["(?=", "(?!", "(?<=", "(?<!", "(?:"];
        let repeats = ["", "", "", "*", "+", "?", "{1,2}"];
        let mut out = String::new();
        for b in 0..1 + draw(seed, 2) {
            if b > 0 {
                out.push('|');
            }
            for _ in 0..1 + draw(seed, 3) {
                let pick = draw(seed, atoms.len() + 4);
                if pick >= atoms.len() && depth > 0 {
                    let open = opens[draw(seed, opens.len())];
                    let inner = assertion_pattern(seed, depth - 1);
                    out.push_str(&format!("{open}{inner})"));
                    if open == "(?:" {
                        out.push_str(repeats[draw(seed, repeats.len())]);
                    }
                    continue;
                }
                let atom = atoms[pick % atoms.len()];
                out.push_str(atom);
                if pick % atoms.len() < 8 {
                    out.push_str(repeats[draw(seed, repeats.len())]);
                }
            }
        }
        out
    }

    /// The licence of the derivative construction: on every text of up to five bytes
    /// over `a`, `b`, `-` and LF, the DFA of each generated pattern accepts where the
    /// definition matches the whole text.
    #[test]
    fn the_dfa_of_an_assertion_matches_what_the_definition_matches() {
        let alphabet = b"ab-\n";
        let mut texts: Vec<Vec<u8>> = vec![Vec::new()];
        let mut from = 0;
        for _ in 0..5 {
            let to = texts.len();
            for t in from..to {
                for &c in alphabet {
                    let mut x = texts[t].clone();
                    x.push(c);
                    texts.push(x);
                }
            }
            from = to;
        }
        let (mut seed, mut patterns, mut accepted) = (7u64, 0, 0);
        while patterns < 400 {
            let pattern = assertion_pattern(&mut seed, 2);
            let Ok(got) = compile(&pattern) else {
                continue;
            };
            let re = parse(&pattern).expect("it compiled");
            for t in &texts {
                let want = spans(&re, t, 0).contains(&t.len());
                let text = std::str::from_utf8(t).expect("ASCII");
                assert_eq!(got.matches(text), want, "`{pattern}` on {text:?}");
                accepted += usize::from(want);
            }
            patterns += 1;
        }
        assert_eq!((patterns, texts.len()), (400, 1365));
        // The texts each pattern accepts, summed: the agreement is not all on rejection.
        assert_eq!(accepted, 4735);
    }
}
