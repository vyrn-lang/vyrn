//! Reads a `.syntax` file into a [`Grammar`].
//!
//! A definition starts in column 0 and an indented line continues it; `//`
//! outside a literal or a class starts a comment. Directives: `syntax Name`,
//! `keyword Name 'w'`, `token Name 'p'`, `opener 'p'..`, `nest N ^Row`,
//! `expect ^Row(..)` and `row Name`. Every other definition is a rule,
//! `[nest] Name = expression [fail ^Row]`. The loader refuses a reference to an
//! undefined rule and a left-recursive rule, so every rule call either consumes
//! input or descends the acyclic graph of calls at one position.

use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use crate::{
    is_word, At, Builtin, ByteSet, Embed, Grammar, HoleSrc, Level, Lit, Recover, Rule, RuleId,
    Throw, E,
};

/// Reads the grammar `src`, or says which definition is wrong and why.
pub fn load(src: &str) -> Result<Grammar, String> {
    let defs = definitions(src)?;
    let mut tokens: Vec<(String, String, bool)> = Vec::new();
    let mut builder: BTreeSet<Rc<str>> = BTreeSet::new();
    let mut by_name: HashMap<String, RuleId> = HashMap::new();
    // The directives and the rule names first, so a reference may precede its
    // rule. A throw waits for the second pass, which knows every name.
    let mut heads: Vec<(usize, String, &str, bool)> = Vec::new();
    let mut throws: Vec<(usize, Option<u32>, &str)> = Vec::new();
    let mut openers: Vec<String> = Vec::new();
    for (line, d) in &defs {
        let words: Vec<&str> = d.split_whitespace().collect();
        let err = |what: &str| format!("line {line}: {what}");
        match words.as_slice() {
            ["syntax", _] => {}
            [head @ ("keyword" | "token"), name, lit] if *name != "=" => {
                let spelling = unquote(lit).ok_or_else(|| err("a token spelling is a literal"))?;
                tokens.push((name.to_string(), spelling, *head == "keyword"));
            }
            ["opener", lits @ ..] if lits.first() != Some(&"=") => {
                for l in lits {
                    openers.push(unquote(l).ok_or_else(|| err("an opener is a literal"))?);
                }
            }
            ["row", name] => {
                builder.insert(Rc::from(*name));
            }
            ["nest", n, ..] if n.parse::<u32>().is_ok() => {
                throws.push((
                    *line,
                    n.parse().ok(),
                    d.split_once(n).map_or("", |(_, r)| r),
                ));
            }
            ["expect", ..] if words.get(1) != Some(&"=") => {
                throws.push((*line, None, d.split_once("expect").map_or("", |(_, r)| r)));
            }
            _ => {
                let (lhs, rhs) = d
                    .split_once('=')
                    .ok_or_else(|| err("expected `name = ..`"))?;
                let (nest, name) = match lhs.split_whitespace().collect::<Vec<_>>().as_slice() {
                    [name] => (false, name.to_string()),
                    ["nest", name] => (true, name.to_string()),
                    _ => return Err(err("a rule is `[nest] Name = ..`")),
                };
                if by_name
                    .insert(name.clone(), RuleId(heads.len() as u32))
                    .is_some()
                {
                    return Err(err(&format!("rule `{name}` is defined twice")));
                }
                heads.push((*line, name, rhs, nest));
            }
        }
    }
    let spellings: Vec<Rc<str>> = (tokens.iter().map(|t| t.1.as_str()))
        .chain(openers.iter().map(String::as_str))
        .map(Rc::from)
        .collect();
    let mut m = Meta {
        s: b"",
        i: 0,
        names: &by_name,
        flags: Vec::new(),
        labels: Vec::new(),
        rows: BTreeSet::new(),
    };
    let mut rules = Vec::new();
    for (line, name, rhs, nest) in &heads {
        let at = |e: String| format!("line {line}: rule {name}: {e}");
        m.start(rhs);
        let mut body = m.alt().map_err(at)?;
        let fail = match m.word_is("fail") {
            true => {
                m.eat(b'^').map_err(at)?;
                Some(Rc::new(m.throw("").map_err(at)?))
            }
            false => None,
        };
        m.end().map_err(at)?;
        let syntactic = name.starts_with(|c: char| c.is_ascii_uppercase());
        if syntactic {
            munch(&mut body, &spellings);
        }
        rules.push(Rule {
            name: name.clone(),
            syntactic,
            body,
            nest: *nest,
            fail,
        });
    }
    // A `nest N ^Row` line carries its limit; an `expect ^Row` line none.
    let (mut nest_limit, mut expect) = (None, None);
    for (line, limit, text) in throws {
        let at = |e: String| format!("line {line}: {e}");
        m.start(text);
        m.eat(b'^').map_err(at)?;
        let t = Rc::new(
            m.throw(if limit.is_some() { "" } else { "token" })
                .map_err(at)?,
        );
        m.end().map_err(at)?;
        match limit {
            Some(n) => nest_limit = Some((n, t)),
            None => expect = Some(t),
        }
    }
    if let Some(r) = m.rows.intersection(&builder).next() {
        return Err(format!("row `{r}` is both stated and a builder's"));
    }
    let rule = |n: &str| by_name.get(n).copied();
    let mut g = Grammar {
        trivia: rule("trivia"),
        token_rule: rule("token"),
        scan_rule: rule("scan"),
        reserved: tokens.iter().filter(|t| t.2).map(|t| t.1.clone()).collect(),
        tokens,
        flags: m.flags,
        nest_limit,
        expect: expect.ok_or("a grammar names its expect row: `expect ^Row(..)`")?,
        stated: m.rows,
        builder,
        rules,
        by_name,
    };
    refuse_left_recursion(&g)?;
    let bodies: Vec<E> = g.rules.iter().map(|r| guard(&g, &r.body)).collect();
    for (r, b) in g.rules.iter_mut().zip(bodies) {
        r.body = b;
    }
    Ok(g)
}

/// Splits `src` into definitions, each with its first line's number.
fn definitions(src: &str) -> Result<Vec<(usize, String)>, String> {
    let mut defs: Vec<(usize, String)> = Vec::new();
    for (n, line) in src.lines().enumerate() {
        let code = strip_comment(line);
        if code.trim().is_empty() {
            continue;
        }
        if !line.starts_with([' ', '\t']) {
            defs.push((n + 1, code.to_string()));
        } else if let Some(last) = defs.last_mut() {
            last.1.push('\n');
            last.1.push_str(code);
        } else {
            return Err(format!(
                "line {}: a continuation before any definition",
                n + 1
            ));
        }
    }
    Ok(defs)
}

/// Gives every punctuation literal its longer token spellings (maximal munch).
fn munch(e: &mut E, spellings: &[Rc<str>]) {
    let lit = |l: &mut Lit| {
        if !l.word {
            let longer: Vec<Rc<str>> = (spellings.iter())
                .filter(|t| t.len() > l.s.len() && t.starts_with(&*l.s))
                .cloned()
                .collect();
            l.longer = longer.into();
        }
    };
    match e {
        E::Lit(l) => lit(l),
        E::Climb(x, levels) => {
            munch(x, spellings);
            let mut lv: Vec<Level> = Vec::new();
            for l in levels.iter() {
                let mut ops = l.ops.clone();
                ops.iter_mut().for_each(lit);
                lv.push(Level {
                    right: l.right,
                    ops,
                });
            }
            *levels = lv.into();
        }
        E::Many(a, b, r) => {
            munch(a, spellings);
            munch(b, spellings);
            if let Recover::Decl(p) = r {
                munch(p, spellings);
            }
        }
        _ => children_mut(e)
            .into_iter()
            .for_each(|x| munch(x, spellings)),
    }
}

/// The sub-expressions of `e` that run at its own position or after it.
fn children_mut(e: &mut E) -> Vec<&mut E> {
    match e {
        E::Seq(v) | E::Alt(v) => v.iter_mut().collect(),
        E::Star(x) | E::Plus(x) | E::Opt(x) | E::And(x) | E::Not(x) | E::Try(x) => vec![&mut **x],
        E::Field(_, x) | E::SetFlag(_, _, x) | E::Climb(x, _) | E::Guard(_, x) => vec![&mut **x],
        E::Many(a, b, _) => vec![&mut **a, &mut **b],
        E::List(a, b, c) => vec![&mut **a, &mut **b, &mut **c],
        _ => Vec::new(),
    }
}

/// Refuses a rule that can call itself before it consumes a character, so the
/// interpreter's recursion has a measure: the text left, then the depth in the
/// acyclic graph of calls at one position.
fn refuse_left_recursion(g: &Grammar) -> Result<(), String> {
    // Nullability is a least fixpoint; each round sets one more rule or stops,
    // so there are at most `rules + 1` rounds.
    let mut nullable = vec![false; g.rules.len()];
    for _ in 0..=g.rules.len() {
        let next: Vec<bool> = g.rules.iter().map(|r| empty(&r.body, &nullable)).collect();
        if next == nullable {
            break;
        }
        nullable = next;
    }
    let calls: Vec<Vec<RuleId>> = (g.rules.iter())
        .map(|r| {
            let mut out = Vec::new();
            left_calls(&r.body, &nullable, &mut out);
            out
        })
        .collect();
    // Depth-first search for a cycle: 0 unseen, 1 on the path, 2 done. Each
    // rule is entered once, so the search ends.
    fn visit(r: usize, calls: &[Vec<RuleId>], state: &mut [u8], path: &mut Vec<usize>) -> bool {
        match state[r] {
            1 => {
                path.push(r);
                return true;
            }
            2 => return false,
            _ => {}
        }
        state[r] = 1;
        path.push(r);
        for c in &calls[r] {
            if visit(c.0 as usize, calls, state, path) {
                return true;
            }
        }
        path.pop();
        state[r] = 2;
        false
    }
    let mut state = vec![0u8; g.rules.len()];
    for r in 0..g.rules.len() {
        let mut path = Vec::new();
        if visit(r, &calls, &mut state, &mut path) {
            let names: Vec<&str> = path.iter().map(|&i| g.rules[i].name.as_str()).collect();
            return Err(format!("left recursion: {}", names.join(" -> ")));
        }
    }
    Ok(())
}

/// Whether `e` can succeed without consuming a character, given the rules'
/// nullability so far.
fn empty(e: &E, nullable: &[bool]) -> bool {
    match e {
        E::Seq(v) => v.iter().all(|x| empty(x, nullable)),
        E::Alt(v) => v.iter().any(|x| empty(x, nullable)),
        E::Star(_) | E::Opt(_) | E::And(_) | E::Not(_) | E::TestFlag(_) => true,
        E::Many(..) | E::List(..) => true,
        E::Builtin(b) => matches!(b, Builtin::Eof | Builtin::NoNl),
        E::Plus(x) | E::Try(x) | E::Field(_, x) | E::SetFlag(_, _, x) | E::Guard(_, x) => {
            empty(x, nullable)
        }
        E::Climb(x, _) => empty(x, nullable),
        E::Embed(em) => empty(&em.extent, nullable),
        E::Ref(r) => nullable[r.0 as usize],
        // A skip at the end of the text consumes nothing.
        E::Skip(_) => true,
        E::Lit(_) | E::Split(_) | E::Class(..) | E::Any | E::Throw(_) => false,
    }
}

/// The rules `e` can call before it consumes a character.
fn left_calls(e: &E, nullable: &[bool], out: &mut Vec<RuleId>) {
    match e {
        E::Ref(r) => out.push(*r),
        E::Seq(v) => {
            for x in v {
                left_calls(x, nullable, out);
                if !empty(x, nullable) {
                    break;
                }
            }
        }
        E::Alt(v) => v.iter().for_each(|x| left_calls(x, nullable, out)),
        E::Star(x) | E::Plus(x) | E::Opt(x) | E::And(x) | E::Not(x) | E::Try(x) => {
            left_calls(x, nullable, out)
        }
        E::Field(_, x) | E::SetFlag(_, _, x) | E::Guard(_, x) | E::Climb(x, _) => {
            left_calls(x, nullable, out)
        }
        E::Many(x, c, _) => {
            left_calls(c, nullable, out);
            left_calls(x, nullable, out);
        }
        E::List(x, s, c) => {
            left_calls(c, nullable, out);
            left_calls(x, nullable, out);
            if empty(x, nullable) {
                left_calls(s, nullable, out);
            }
        }
        // The island's rule runs at the extent's start.
        E::Embed(em) => {
            left_calls(&em.extent, nullable, out);
            out.push(em.inner);
        }
        E::Lit(_) | E::Split(_) | E::Class(..) | E::Any | E::Builtin(_) => {}
        E::Throw(_) | E::Skip(_) | E::TestFlag(_) => {}
    }
}

fn set_byte(s: &mut ByteSet, b: u8) {
    s[(b >> 6) as usize] |= 1 << (b & 63);
}

/// The bytes `e` can start with after trivia, or `None` when it may match empty
/// or the set is not worth stating. `seen` holds the rules on the path, so a
/// rule is entered once per path.
fn first(g: &Grammar, e: &E, seen: &mut Vec<RuleId>) -> Option<ByteSet> {
    let mut s = [0u64; 4];
    match e {
        E::Lit(l) => set_byte(&mut s, *l.s.as_bytes().first()?),
        E::Split(x) => set_byte(&mut s, *x.as_bytes().first()?),
        E::Class(ranges, false) => {
            for &(a, b) in ranges.iter() {
                if (b as u32) >= 128 {
                    return None;
                }
                (a as u8..=b as u8).for_each(|c| set_byte(&mut s, c));
            }
        }
        E::Builtin(b) => {
            let mut add = |r: std::ops::RangeInclusive<u8>| r.for_each(|c| set_byte(&mut s, c));
            match b {
                Builtin::Digit => add(b'0'..=b'9'),
                Builtin::Hex => {
                    add(b'0'..=b'9');
                    add(b'a'..=b'f');
                    add(b'A'..=b'F');
                }
                Builtin::Alpha | Builtin::Alnum | Builtin::Reserved => {
                    add(b'a'..=b'z');
                    add(b'A'..=b'Z');
                    add(0x80..=0xFF);
                    if *b == Builtin::Alnum {
                        add(b'0'..=b'9');
                    }
                }
                _ => return None,
            }
        }
        E::Ref(r) => {
            if seen.contains(r) {
                return None;
            }
            seen.push(*r);
            let f = first(g, &g.body(*r).body, seen);
            seen.pop();
            return f;
        }
        E::Seq(v) => {
            for x in v {
                match x {
                    E::And(_) | E::Not(_) | E::TestFlag(_) | E::Builtin(Builtin::NoNl) => continue,
                    E::Field(_, y) if matches!(**y, E::Opt(_) | E::Star(_)) => return None,
                    E::Star(_) | E::Opt(_) => return None,
                    _ => return first(g, x, seen),
                }
            }
            return None;
        }
        E::Alt(v) => {
            for x in v {
                let f = first(g, x, seen)?;
                s.iter_mut().zip(f).for_each(|(a, b)| *a |= b);
            }
        }
        E::Plus(x) | E::Field(_, x) | E::Try(x) | E::SetFlag(_, _, x) | E::Climb(x, _) => {
            return first(g, x, seen)
        }
        _ => return None,
    }
    Some(s)
}

/// Wraps each alternative whose first bytes are known in a [`E::Guard`].
fn guard(g: &Grammar, e: &E) -> E {
    let w = |x: &E| Box::new(guard(g, x));
    match e {
        E::Alt(v) => E::Alt(
            (v.iter())
                .map(|x| match first(g, x, &mut Vec::new()) {
                    Some(set) => E::Guard(Rc::new(set), Box::new(guard(g, x))),
                    None => guard(g, x),
                })
                .collect(),
        ),
        E::Seq(v) => E::Seq(v.iter().map(|x| guard(g, x)).collect()),
        E::Star(x) => E::Star(w(x)),
        E::Plus(x) => E::Plus(w(x)),
        E::Opt(x) => E::Opt(w(x)),
        E::And(x) => E::And(w(x)),
        E::Not(x) => E::Not(w(x)),
        E::Try(x) => E::Try(w(x)),
        E::Field(l, x) => E::Field(l.clone(), w(x)),
        E::SetFlag(f, v, x) => E::SetFlag(*f, *v, w(x)),
        E::Climb(x, l) => E::Climb(w(x), l.clone()),
        E::Many(a, b, r) => E::Many(w(a), w(b), r.clone()),
        E::List(a, b, c) => E::List(w(a), w(b), w(c)),
        other => other.clone(),
    }
}

/// `l` without a `//` comment outside a literal or a class.
fn strip_comment(l: &str) -> &str {
    let b = l.as_bytes();
    let (mut quoted, mut class) = (false, false);
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' if quoted || class => i += 1,
            b'\'' if !class => quoted = !quoted,
            b'[' if !quoted => class = true,
            b']' if class => class = false,
            b'/' if !quoted && !class && b.get(i + 1) == Some(&b'/') => return &l[..i],
            _ => {}
        }
        i += 1;
    }
    l
}

/// The text of a quoted literal, or `None` when `lit` is not one.
fn unquote(lit: &str) -> Option<String> {
    let inner = lit.strip_prefix('\'')?.strip_suffix('\'')?;
    let mut out = String::new();
    let mut cs = inner.chars();
    while let Some(c) = cs.next() {
        out.push(match c {
            '\\' => escaped(cs.next()?),
            c => c,
        });
    }
    Some(out)
}

/// The character `\c` stands for.
fn escaped(c: char) -> char {
    match c {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        '0' => '\0',
        o => o,
    }
}

/// The reader of one definition's expression text.
struct Meta<'a> {
    s: &'a [u8],
    i: usize,
    names: &'a HashMap<String, RuleId>,
    flags: Vec<String>,
    /// The labels of the rule being read, by first appearance.
    labels: Vec<Rc<str>>,
    rows: BTreeSet<Rc<str>>,
}

impl<'a> Meta<'a> {
    fn start(&mut self, text: &'a str) {
        self.s = text.as_bytes();
        self.i = 0;
        self.labels.clear();
    }

    fn end(&mut self) -> Result<(), String> {
        match self.peek() {
            None => Ok(()),
            Some(_) => Err(format!("unexpected `{}`", self.rest())),
        }
    }

    fn at_byte(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn ws(&mut self) {
        while self.at_byte().is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.at_byte()
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        match self.peek() == Some(c) {
            true => {
                self.i += 1;
                Ok(())
            }
            false => Err(format!("expected `{}` at `{}`", c as char, self.rest())),
        }
    }

    fn rest(&self) -> String {
        let tail = &self.s[self.i.min(self.s.len())..];
        String::from_utf8_lossy(tail).chars().take(30).collect()
    }

    fn name(&mut self) -> Option<String> {
        self.ws();
        let st = self.i;
        while let Some(c) = self.at_byte() {
            if !(c.is_ascii_alphanumeric() || c == b'_' || c == b'.' && self.i > st) {
                break;
            }
            self.i += 1;
        }
        (self.i > st).then(|| String::from_utf8_lossy(&self.s[st..self.i]).to_string())
    }

    fn word_is(&mut self, w: &str) -> bool {
        self.ws();
        let b = w.as_bytes();
        let after = self.s.get(self.i + b.len());
        let ok = self.s[self.i..].starts_with(b)
            && !after.is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_');
        if ok {
            self.i += b.len();
        }
        ok
    }

    fn alt(&mut self) -> Result<E, String> {
        let mut v = vec![self.seq()?];
        while self.peek() == Some(b'/') {
            self.i += 1;
            v.push(self.seq()?);
        }
        Ok(match v.len() {
            1 => v.swap_remove(0),
            _ => E::Alt(v),
        })
    }

    fn seq(&mut self) -> Result<E, String> {
        let mut v = Vec::new();
        // Each item consumes at least one byte, or `prefix` errs.
        loop {
            match self.peek() {
                None | Some(b'/' | b')' | b',' | b'}' | b';') => break,
                Some(_) => {}
            }
            let save = self.i;
            if ["until", "sep", "recover", "fail"]
                .iter()
                .any(|w| self.word_is(w))
            {
                self.i = save;
                break;
            }
            v.push(self.prefix()?);
        }
        Ok(match v.len() {
            1 => v.swap_remove(0),
            _ => E::Seq(v),
        })
    }

    fn prefix(&mut self) -> Result<E, String> {
        let wrap = match self.peek() {
            Some(b'&') => E::And,
            Some(b'!') => E::Not,
            Some(b'@') => E::Try,
            _ => {
                let save = self.i;
                if let Some(n) = self.name() {
                    if self.at_byte() == Some(b':') {
                        self.i += 1;
                        let l: Rc<str> = Rc::from(n.as_str());
                        self.labels.push(l.clone());
                        return Ok(E::Field(l, Box::new(self.prefix()?)));
                    }
                }
                self.i = save;
                return self.suffix();
            }
        };
        self.i += 1;
        Ok(wrap(Box::new(self.prefix()?)))
    }

    fn suffix(&mut self) -> Result<E, String> {
        let mut e = self.primary()?;
        // A suffix is glued to its operand.
        loop {
            let wrap = match self.at_byte() {
                Some(b'*') => E::Star,
                Some(b'+') => E::Plus,
                Some(b'?') => E::Opt,
                _ => break,
            };
            e = wrap(Box::new(e));
            self.i += 1;
        }
        Ok(e)
    }

    fn lit(&mut self) -> Result<String, String> {
        self.ws();
        let st = self.i;
        if self.at_byte() != Some(b'\'') {
            return Err(format!("expected a literal at `{}`", self.rest()));
        }
        self.i += 1;
        loop {
            match self.at_byte() {
                None => return Err("an unclosed literal".into()),
                Some(b'\'') => break,
                Some(b'\\') => self.i += 2,
                Some(_) => self.i += 1,
            }
        }
        self.i += 1;
        let text = std::str::from_utf8(&self.s[st..self.i]).map_err(|e| e.to_string())?;
        match unquote(text) {
            Some(s) if !s.is_empty() => Ok(s),
            _ => Err(format!("a literal is not empty: {text}")),
        }
    }

    fn mklit(s: String) -> Lit {
        Lit {
            word: s.chars().last().is_some_and(is_word),
            s: Rc::from(s.as_str()),
            longer: Rc::from(Vec::new()),
        }
    }

    fn flag(&mut self) -> Result<usize, String> {
        let n = self.name().ok_or("a flag name")?;
        Ok(match self.flags.iter().position(|f| *f == n) {
            Some(i) => i,
            None => {
                self.flags.push(n);
                self.flags.len() - 1
            }
        })
    }

    fn class(&mut self) -> Result<E, String> {
        let neg = self.at_byte() == Some(b'^');
        if neg {
            self.i += 1;
        }
        let mut chars: Vec<char> = Vec::new();
        loop {
            match self.at_byte() {
                None => return Err("an unclosed class".into()),
                Some(b']') => break,
                Some(b'\\') => {
                    let c = self.s.get(self.i + 1).ok_or("an unclosed class")?;
                    chars.push(escaped(*c as char));
                    self.i += 2;
                }
                Some(c) => {
                    chars.push(c as char);
                    self.i += 1;
                }
            }
        }
        self.i += 1;
        let mut ranges = Vec::new();
        let mut k = 0;
        while k < chars.len() {
            if k + 2 < chars.len() && chars[k + 1] == '-' {
                ranges.push((chars[k], chars[k + 2]));
                k += 3;
            } else {
                ranges.push((chars[k], chars[k]));
                k += 1;
            }
        }
        Ok(E::Class(ranges.into(), neg))
    }

    fn primary(&mut self) -> Result<E, String> {
        let c = self.peek().ok_or("an expression")?;
        match c {
            b'(' => {
                self.i += 1;
                let e = self.alt()?;
                self.eat(b')')?;
                Ok(e)
            }
            b'\'' => Ok(E::Lit(Self::mklit(self.lit()?))),
            b'.' => {
                self.i += 1;
                Ok(E::Any)
            }
            b'[' => {
                self.i += 1;
                self.class()
            }
            b'^' => {
                self.i += 1;
                Ok(E::Throw(Rc::new(self.throw("")?)))
            }
            b'~' => {
                self.i += 1;
                self.eat(b'^')?;
                Ok(E::Skip(Rc::new(self.throw("")?)))
            }
            b'+' | b'-' => {
                self.i += 1;
                let f = self.flag()?;
                self.eat(b'(')?;
                let e = self.alt()?;
                self.eat(b')')?;
                Ok(E::SetFlag(f, c == b'+', Box::new(e)))
            }
            b'?' => {
                self.i += 1;
                Ok(E::TestFlag(self.flag()?))
            }
            _ => {
                let n = self
                    .name()
                    .ok_or_else(|| format!("unexpected `{}`", self.rest()))?;
                self.word(&n)
            }
        }
    }

    /// The expression a bare word starts: a form, a builtin or a rule.
    fn word(&mut self, n: &str) -> Result<E, String> {
        let builtin = |b| Ok(E::Builtin(b));
        match n {
            "split" => Ok(E::Split(Rc::from(self.lit()?.as_str()))),
            "many" => {
                let x = self.prefix()?;
                if !self.word_is("until") {
                    return Err("many X until C".into());
                }
                let until = self.prefix()?;
                let rec = match self.word_is("recover") {
                    false => Recover::None,
                    true if self.word_is("stmt") => Recover::Stmt,
                    true if self.word_is("decl") => Recover::Decl(Box::new(self.prefix()?)),
                    true => return Err("recover stmt | recover decl P".into()),
                };
                Ok(E::Many(Box::new(x), Box::new(until), rec))
            }
            "list" => {
                let x = self.prefix()?;
                if !self.word_is("sep") {
                    return Err("list X sep S until C".into());
                }
                let sep = self.prefix()?;
                if !self.word_is("until") {
                    return Err("list X sep S until C".into());
                }
                let until = self.prefix()?;
                Ok(E::List(Box::new(x), Box::new(sep), Box::new(until)))
            }
            "climb" => {
                let operand = self.prefix()?;
                self.eat(b'{')?;
                let mut levels = Vec::new();
                // Each level reads its `left` or `right` word, or errs.
                while self.peek() != Some(b'}') {
                    let right = match () {
                        _ if self.word_is("right") => true,
                        _ if self.word_is("left") => false,
                        _ => return Err(format!("left | right at `{}`", self.rest())),
                    };
                    let mut ops = Vec::new();
                    while self.peek() == Some(b'\'') {
                        ops.push(Self::mklit(self.lit()?));
                    }
                    if self.peek() == Some(b';') {
                        self.i += 1;
                    }
                    levels.push(Level { right, ops });
                }
                self.i += 1;
                Ok(E::Climb(Box::new(operand), levels.into()))
            }
            "embed" => {
                self.eat(b'(')?;
                let extent = self.alt()?;
                self.eat(b',')?;
                let inner = self.name().ok_or("the embedded rule")?;
                let inner = self.refer(&inner)?;
                let (mut wrap, mut trailing) = (None, None);
                while self.peek() == Some(b',') {
                    self.i += 1;
                    let (slot, site) = match self.name().as_deref() {
                        Some("wrap") => (&mut wrap, "inner"),
                        Some("trailing") => (&mut trailing, "extent"),
                        _ => return Err("embed takes wrap= and trailing=".into()),
                    };
                    self.eat(b'=')?;
                    self.eat(b'^')?;
                    *slot = Some(Rc::new(self.throw(site)?));
                }
                self.eat(b')')?;
                Ok(E::Embed(Rc::new(Embed {
                    extent,
                    inner,
                    wrap,
                    trailing,
                })))
            }
            "eof" => builtin(Builtin::Eof),
            "nonl" => builtin(Builtin::NoNl),
            "alpha" => builtin(Builtin::Alpha),
            "alnum" => builtin(Builtin::Alnum),
            "digit" => builtin(Builtin::Digit),
            "hex" => builtin(Builtin::Hex),
            "ws" => builtin(Builtin::Ws),
            "reserved" => builtin(Builtin::Reserved),
            "punct" => builtin(Builtin::Punct),
            _ => Ok(E::Ref(self.refer(n)?)),
        }
    }

    fn refer(&self, n: &str) -> Result<RuleId, String> {
        self.names
            .get(n)
            .copied()
            .ok_or_else(|| format!("undefined rule `{n}`"))
    }

    fn at(&mut self) -> Result<At, String> {
        let n = self.name().ok_or("an anchor")?;
        Ok(match n.as_str() {
            "here" => At::Here,
            _ => At::Label(self.label_index(&n)),
        })
    }

    fn label_index(&mut self, l: &str) -> usize {
        match self.labels.iter().position(|x| &**x == l) {
            Some(i) => i,
            None => {
                self.labels.push(Rc::from(l));
                self.labels.len() - 1
            }
        }
    }

    /// Reads `Row(hole=source, ..)@anchor`. `site` is the one site-bound hole
    /// source the row may use: `token` in the `expect` row, `inner` in an
    /// `embed`'s wrap row, `extent` in its trailing row, or none.
    fn throw(&mut self, site: &str) -> Result<Throw, String> {
        let row = self.name().ok_or("a row name")?;
        let mut holes = Vec::new();
        if self.at_byte() == Some(b'(') {
            self.i += 1;
            // Each hole reads a name or errs.
            loop {
                let h = self.name().ok_or("a hole name")?;
                self.eat(b'=')?;
                let src = match self.peek() {
                    Some(b'\'') => HoleSrc::Text(Rc::from(self.lit()?.as_str())),
                    Some(b'.') => {
                        self.i += 1;
                        let debug = self.s[self.i..].starts_with(b":?");
                        if debug {
                            self.i += 2;
                        }
                        HoleSrc::Char { debug }
                    }
                    _ => match self.name().ok_or("a hole source")?.as_str() {
                        w @ ("token" | "inner" | "extent") if w != site => {
                            return Err(format!("`{w}` fills no hole of this row"))
                        }
                        "found" => HoleSrc::Found,
                        "token" => HoleSrc::Token,
                        "inner" => HoleSrc::Inner,
                        "extent" => HoleSrc::Extent,
                        label => HoleSrc::Label(Rc::from(label)),
                    },
                };
                holes.push((Rc::from(h.as_str()), src));
                match self.peek() {
                    Some(b',') => self.i += 1,
                    _ => break,
                }
            }
            self.eat(b')')?;
        }
        let (mut line, mut col) = (At::Here, At::Here);
        if self.at_byte() == Some(b'@') {
            self.i += 1;
            if self.peek() == Some(b'(') {
                self.i += 1;
                line = self.at()?;
                self.eat(b',')?;
                col = self.at()?;
                self.eat(b')')?;
            } else {
                line = self.at()?;
                col = line;
            }
        }
        let row: Rc<str> = Rc::from(row.as_str());
        self.rows.insert(row.clone());
        Ok(Throw {
            row,
            holes,
            line,
            col,
            labels: self.labels.clone(),
        })
    }
}
