//! Writes a grammar as one Vyrn module: a parser, not an interpreter of a grammar.
//!
//! [`parser`] lowers each rule a start rule reaches to one function over byte
//! offsets, and each start rule to an exported `parse<Start>`. The written module
//! holds one `Parse` record: the cursor, the end of the last token, the end of the
//! text, the refusals, and one field per flag. A rule function returns whether its
//! rule matched. A false answer with `p.refused` unset left the cursor where it was;
//! with `p.refused` set the rule refused, and every caller up to a predicate or a
//! `recover` hands the false on.
//!
//! A syntactic rule keeps the cursor on the first byte of a token: each token it
//! takes also takes the trivia after it, so a failed alternative pays no second skip
//! and the `committed` test is `p.pos` against where the sequence started. A lexical
//! rule reads bytes, and only a syntactic caller skips trivia after it. A character
//! outside ASCII counts as a letter, which is what the loader's first-byte sets
//! assume, so `alpha`, `alnum` and a word's boundary read no Unicode table.
//!
//! The statements each expression writes compute a Vyrn expression with no side
//! effect, which [`Writer::hit`] returns and its caller reads at once.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::rc::Rc;

use crate::gen::walk;
use crate::lower::{free_labels, miss, Miss};
use crate::{At, Builtin, Embed, Grammar, HoleSrc, Level, Lit, Recover, Refused, RuleId, Throw, E};

/// The rules a file may define that no rule has to read: a host's lexer starts at
/// `scan`, and `trivia` and `token` serve the parser itself.
const NOT_STARTS: &[&str] = &["scan", "trivia", "token"];

/// The type names a written module uses for itself, which no rule may take.
const TAKEN: &[&str] = &["Row", "Parse"];

/// Returns the Vyrn source of `g`'s parser, or where the grammar has no such parser
/// and why.
pub fn parser(g: &Grammar) -> Result<String, Refused> {
    refuse_unset_flags(g)?;
    let starts = start_rules(g)?;
    let rules = reached(g, &starts);
    for &r in &rules {
        let rule = g.body(r);
        if !is_name(&rule.name) {
            return Err(Refused::at(
                rule.line,
                1,
                format!("rule `{}` is not one identifier", rule.name),
            ));
        }
        if rule.nest && g.nest_limit.is_none() {
            return Err(Refused::at(
                rule.line,
                1,
                format!(
                    "rule `{}` has `nest`, and no `nest` line sets a limit",
                    rule.name
                ),
            ));
        }
    }
    let rows = rows(g);
    refuse_clashes(g, &starts, &rows)?;
    let mut w = Writer {
        g,
        free: free_labels(g, &rules),
        rows,
        o: String::new(),
        ind: 0,
        fresh: 0,
        at: starts[0],
        kept: BTreeSet::new(),
        scope: Vec::new(),
    };
    for &r in &rules {
        w.rule(r)?;
    }
    for &r in &starts {
        w.entry(r)?;
    }
    Ok(w.finish(&starts))
}

/// Refuses a flag a rule reads and no rule sets: it is a hook of a host, and a
/// written parser has no host.
fn refuse_unset_flags(g: &Grammar) -> Result<(), Refused> {
    let mut set = vec![false; g.flags.len()];
    for rule in &g.rules {
        walk(&rule.body, &mut |e| {
            if let E::SetFlag(f, true, _) = e {
                set[*f] = true;
            }
        });
    }
    for rule in &g.rules {
        let mut read = None;
        walk(&rule.body, &mut |e| match e {
            E::TestFlag(f) if !set[*f] && read.is_none() => read = Some(*f),
            _ => {}
        });
        if let Some(f) = read {
            let sentence = format!(
                "rule `{}` reads flag `{}`, and no rule sets it: only a host could, and a written parser has no host",
                rule.name, g.flags[f]
            );
            return Err(Refused::at(rule.line, 1, sentence));
        }
    }
    Ok(())
}

/// Each rule no other rule reads, in file order. Such a rule cannot read a label,
/// because no caller binds one.
fn start_rules(g: &Grammar) -> Result<Vec<RuleId>, Refused> {
    let mut read = vec![false; g.rules.len()];
    for (i, rule) in g.rules.iter().enumerate() {
        for callee in callees(&rule.body) {
            read[callee.0 as usize] |= callee.0 as usize != i;
        }
    }
    let starts: Vec<RuleId> = (0..g.rules.len())
        .filter(|&i| !read[i] && !NOT_STARTS.contains(&&*g.rules[i].name))
        .map(|i| RuleId(i as u32))
        .collect();
    if starts.is_empty() {
        return Err(Refused::at(
            1,
            1,
            "the grammar has no start rule: every rule is read by another",
        ));
    }
    let free = free_labels(g, &reached(g, &starts));
    for &r in &starts {
        if let Some(l) = free[r.0 as usize].first() {
            let rule = g.body(r);
            let sentence = format!(
                "start rule `{}` reads label `{l}`, and no caller binds it",
                rule.name
            );
            return Err(Refused::at(rule.line, 1, sentence));
        }
    }
    Ok(starts)
}

/// The rules `e` calls, an `embed`'s island rule among them.
fn callees(e: &E) -> Vec<RuleId> {
    let mut out = Vec::new();
    walk(e, &mut |x| match x {
        E::Ref(r) => out.push(*r),
        E::Embed(em) => out.push(em.inner),
        _ => {}
    });
    out
}

/// The rules the written module holds, in file order: what `starts` reach, and
/// `trivia` and `token` when the grammar has them.
fn reached(g: &Grammar, starts: &[RuleId]) -> Vec<RuleId> {
    let mut seen = vec![false; g.rules.len()];
    let mut todo = starts.to_vec();
    todo.extend(g.rule("trivia"));
    todo.extend(g.rule("token"));
    while let Some(r) = todo.pop() {
        if !std::mem::replace(&mut seen[r.0 as usize], true) {
            todo.extend(callees(&g.body(r).body));
        }
    }
    (0..g.rules.len())
        .filter(|&i| seen[i])
        .map(|i| RuleId(i as u32))
        .collect()
}

/// Each row of `g` with its holes, by name. A row's holes are every hole any of its
/// sites fills, in the order the file first names them.
fn rows(g: &Grammar) -> BTreeMap<Rc<str>, Vec<Rc<str>>> {
    let mut rows: BTreeMap<Rc<str>, Vec<Rc<str>>> = BTreeMap::new();
    let mut site = |t: &Throw| {
        let holes = rows.entry(t.row.clone()).or_default();
        for (h, _) in &t.holes {
            if !holes.contains(h) {
                holes.push(h.clone());
            }
        }
    };
    site(&g.expect);
    if let Some((_, t)) = &g.nest_limit {
        site(t);
    }
    for rule in &g.rules {
        if let Some(t) = &rule.fail {
            site(t);
        }
        walk(&rule.body, &mut |e| match e {
            E::Throw(t) | E::Skip(t) => site(t),
            E::Embed(em) => {
                for t in [&em.wrap, &em.trailing].into_iter().flatten() {
                    site(t);
                }
            }
            _ => {}
        });
    }
    for row in &g.builder {
        rows.entry(row.clone()).or_default();
    }
    rows
}

/// Refuses two written names that would clash: a start rule's type or a row on a
/// name the module uses, or on each other.
fn refuse_clashes(
    g: &Grammar,
    starts: &[RuleId],
    rows: &BTreeMap<Rc<str>, Vec<Rc<str>>>,
) -> Result<(), Refused> {
    let mut names: BTreeSet<String> = rows.keys().map(|r| r.to_string()).collect();
    names.extend(TAKEN.iter().map(|n| n.to_string()));
    for &r in starts {
        let rule = g.body(r);
        if !names.insert(type_name(&rule.name)) {
            let sentence = format!(
                "rule `{}` writes the type `{}`, and the module already has that name",
                rule.name,
                type_name(&rule.name)
            );
            return Err(Refused::at(rule.line, 1, sentence));
        }
    }
    Ok(())
}

fn is_name(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `file` as `File`.
fn type_name(rule: &str) -> String {
    let mut cs = rule.chars();
    match cs.next() {
        Some(c) => c.to_ascii_uppercase().to_string() + cs.as_str(),
        None => String::new(),
    }
}

/// A Vyrn string literal that spells `s`.
fn string_lit(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\r' => o.push_str("\\r"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(o, "\\u{{{:x}}}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// A Vyrn byte literal that spells `b`.
fn byte_lit(b: u8) -> String {
    match b {
        b'\n' => "'\\n'".into(),
        b'\t' => "'\\t'".into(),
        b'\r' => "'\\r'".into(),
        0 => "'\\0'".into(),
        b'\\' => "'\\\\'".into(),
        b'\'' => "'\\''".into(),
        b' '..=b'~' => format!("'{}'", b as char),
        _ => format!("'\\x{b:02x}'"),
    }
}

/// `e` as a test of one character, read from the byte `c`, with whether the test can
/// hold for a character outside ASCII, which is wider than its first byte. `None`
/// when `e` is no such test.
fn char_test(e: &E) -> Option<(String, bool)> {
    let range = |a: char, b: char| {
        if a == b {
            format!("c == {}", byte_lit(a as u8))
        } else {
            format!("(c >= {} && c <= {})", byte_lit(a as u8), byte_lit(b as u8))
        }
    };
    let letter = "(c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')";
    let digit = "(c >= '0' && c <= '9')";
    Some(match e {
        E::Class(ranges, neg) => {
            let pats: Vec<String> = ranges.iter().map(|&(a, b)| range(a, b)).collect();
            let any = pats.join(" || ");
            match neg {
                true => (format!("!({any})"), true),
                false => (any, false),
            }
        }
        E::Any => ("true".into(), true),
        E::Lit(l) if l.s.len() == 1 => (format!("c == {}", byte_lit(l.s.as_bytes()[0])), false),
        E::Builtin(Builtin::Alpha) => (format!("{letter} || c >= '\\x80'"), true),
        E::Builtin(Builtin::Alnum) => (format!("{letter} || {digit} || c >= '\\x80'"), true),
        E::Builtin(Builtin::Digit) => (digit.into(), false),
        E::Builtin(Builtin::Hex) => (
            format!("{digit} || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F')"),
            false,
        ),
        E::Builtin(Builtin::Ws) => ("c == ' ' || (c >= '\\t' && c <= '\\r')".into(), false),
        E::Guard(_, x) => return char_test(x),
        E::Alt(v) => {
            let tests = v.iter().map(char_test).collect::<Option<Vec<_>>>()?;
            let wide = tests.iter().any(|t| t.1);
            let any: Vec<String> = tests.into_iter().map(|t| t.0).collect();
            (any.join(" || "), wide)
        }
        E::Seq(v) => {
            let (last, nots) = v.split_last()?;
            let mut terms = Vec::new();
            for x in nots {
                let E::Not(y) = x else { return None };
                terms.push(format!("!({})", char_test(y)?.0));
            }
            let (test, wide) = char_test(last)?;
            if test != "true" {
                terms.push(format!("({test})"));
            }
            (terms.join(" && "), wide)
        }
        _ => return None,
    })
}

/// The tests that `s` stands in the text from `k` bytes past the cursor.
fn bytes_at(s: &[u8], k: usize) -> Vec<String> {
    (s.iter().enumerate())
        .map(|(i, b)| match i + k {
            0 => format!("text[p.pos] == {}", byte_lit(*b)),
            n => format!("text[p.pos + {n}] == {}", byte_lit(*b)),
        })
        .collect()
}

/// Writes one rule's function at a time, and the module's other parts around them.
struct Writer<'g> {
    g: &'g Grammar,
    /// The labels each rule reads that it does not bind: its parameters.
    free: Vec<BTreeSet<Rc<str>>>,
    rows: BTreeMap<Rc<str>, Vec<Rc<str>>>,
    o: String,
    ind: usize,
    /// The number of the next local of the function being written.
    fresh: usize,
    /// The rule being written.
    at: RuleId,
    /// The labels the rule being written reads, each held in two locals.
    kept: BTreeSet<Rc<str>>,
    /// The labels bound where the writer stands.
    scope: Vec<Rc<str>>,
}

impl Writer<'_> {
    fn line(&mut self, text: impl AsRef<str>) {
        for _ in 0..self.ind {
            self.o.push_str("    ");
        }
        self.o.push_str(text.as_ref());
        self.o.push('\n');
    }

    fn open(&mut self, text: impl AsRef<str>) {
        self.line(text);
        self.ind += 1;
    }

    fn close(&mut self) {
        self.ind -= 1;
        self.line("}");
    }

    fn local(&mut self, prefix: &str) -> String {
        self.fresh += 1;
        format!("{prefix}{}", self.fresh)
    }

    fn syntactic(&self) -> bool {
        self.g.body(self.at).syntactic
    }

    fn no(&self, sentence: String) -> Refused {
        let rule = self.g.body(self.at);
        Refused::at(rule.line, 1, format!("rule `{}`: {sentence}", rule.name))
    }

    /// The function that holds rule `r`, by kind so `file` and `File` stay apart.
    fn function(&self, r: RuleId) -> String {
        let rule = self.g.body(r);
        match rule.syntactic {
            true => format!("rule{}", type_name(&rule.name)),
            false => format!("lex{}", type_name(&rule.name)),
        }
    }

    /// `, qFrom, qTo` for each free label of `r`, with `ty` after each name.
    fn labels(&self, r: RuleId, ty: &str) -> String {
        let mut out = String::new();
        for l in &self.free[r.0 as usize] {
            let _ = write!(out, ", {l}From{ty}, {l}To{ty}");
        }
        out
    }

    fn start_rule(&mut self, r: RuleId) {
        self.at = r;
        self.fresh = 0;
        self.ind = 0;
        self.scope = self.free[r.0 as usize].iter().cloned().collect();
        self.kept.clear();
        let mut kept = BTreeSet::new();
        let free = &self.free;
        walk(&self.g.body(r).body, &mut |e| {
            let mut throw = |t: &Throw| {
                for at in [t.line, t.col] {
                    if let At::Label(i) = at {
                        kept.insert(t.labels[i].clone());
                    }
                }
                for (_, src) in &t.holes {
                    if let HoleSrc::Label(l) = src {
                        kept.insert(l.clone());
                    }
                }
            };
            match e {
                E::Throw(t) | E::Skip(t) => throw(t),
                E::Embed(em) => [&em.wrap, &em.trailing]
                    .into_iter()
                    .flatten()
                    .for_each(|t| throw(t)),
                E::Ref(c) => kept.extend(free[c.0 as usize].iter().cloned()),
                _ => {}
            }
        });
        self.kept = kept;
    }

    fn rule(&mut self, r: RuleId) -> Result<(), Refused> {
        self.start_rule(r);
        let rule = self.g.body(r);
        let params = self.labels(r, ": Int64");
        self.open(format!(
            "fn {}(text: String, p: modify Parse{params}) -> Bool {{",
            self.function(r)
        ));
        if let E::Climb(operand, levels) = &rule.body {
            return self.climb(r, operand, levels);
        }
        let nest = match (&self.g.nest_limit, rule.nest) {
            (Some((max, t)), true) => {
                self.open(format!("if p.depth >= {max} {{"));
                self.refuse(t, None);
                self.line("return false");
                self.close();
                self.line("p.depth = p.depth + 1");
                true
            }
            _ => false,
        };
        if nest {
            let h = self.local("h");
            self.put(&format!("let {h} = "), &rule.body)?;
            self.line("p.depth = p.depth - 1");
            self.line(format!("return {h}"));
        } else {
            self.put("return ", &rule.body)?;
        }
        self.close();
        self.o.push('\n');
        Ok(())
    }

    /// A `climb` rule's function, the one that climbs, and the one that reads an
    /// operator. The recursion's measure is the text left, because an operator is
    /// consumed before each descent.
    fn climb(&mut self, r: RuleId, operand: &E, levels: &[Level]) -> Result<(), Refused> {
        let f = self.function(r);
        self.line(format!("return {f}From(text, p, 0)"));
        self.close();
        self.o.push('\n');
        self.open(format!(
            "fn {f}From(text: String, p: modify Parse, min: Int64) -> Bool {{"
        ));
        let hit = self.hit(operand)?;
        self.open(format!("if !{hit} {{"));
        self.line("return false");
        self.close();
        self.open("while true {");
        self.line(format!("let op = {f}Op(text, p)"));
        self.open("if op < 0 || op / 2 < min {");
        self.line("break");
        self.close();
        self.open("if !advance(text, p, p.oplen) {");
        self.line("return false");
        self.close();
        self.open(format!("if !{f}From(text, p, op / 2 + 1 - op % 2) {{"));
        self.open("if !p.refused {");
        self.refuse_miss(operand);
        self.close();
        self.line("return false");
        self.close();
        self.close();
        self.line("return true");
        self.close();
        self.o.push('\n');
        self.open(format!(
            "fn {f}Op(text: String, p: modify Parse) -> Int64 {{"
        ));
        // The longest spelling first, so `<` does not hide `<=`.
        let mut ops: Vec<(&Lit, usize, bool)> = Vec::new();
        for (i, level) in levels.iter().enumerate() {
            ops.extend(level.ops.iter().map(|op| (op, i, level.right)));
        }
        ops.sort_by_key(|op| std::cmp::Reverse(op.0.s.len()));
        for (op, level, right) in ops {
            let test = self.lit_test(op, true);
            self.open(format!("if {test} {{"));
            self.line(format!("p.oplen = {}", op.s.len()));
            self.line(format!("return {}", level * 2 + right as usize));
            self.close();
        }
        self.line("return -1");
        self.close();
        self.o.push('\n');
        Ok(())
    }

    /// The exported function of start rule `r`.
    fn entry(&mut self, r: RuleId) -> Result<(), Refused> {
        self.start_rule(r);
        let rule = self.g.body(r);
        let ty = type_name(&rule.name);
        self.open(format!(
            "export fn parse{ty}(text: String) -> Result<{ty}, Array<SyntaxError<Row>>> {{"
        ));
        let flags: String = (self.g.flags.iter())
            .map(|f| format!(", {f}: false"))
            .collect();
        self.line(format!(
            "let mut p = Parse {{ pos: 0, last: 0, stop: text.byteLength, oplen: 0, quiet: 0, looking: 0, depth: 0, refused: false, errors: []{flags} }}"
        ));
        if rule.syntactic && self.g.rule("trivia").is_some() {
            self.line("lexTrivia(text, p)");
        }
        self.line("let start = p.pos");
        self.line(format!("let hit = {}(text, p)", self.function(r)));
        self.open("if !hit && !p.refused {");
        self.refuse_miss(&E::Ref(r));
        self.close();
        self.open("if hit && !p.refused && p.pos < p.stop {");
        self.refuse_expected("Eof");
        self.close();
        self.open("if p.errors.length > 0 {");
        self.line("return Err(consume p.errors)");
        self.close();
        let end = if rule.syntactic { "p.last" } else { "p.pos" };
        self.line(format!("return Ok({ty} {{ start: start, end: {end} }})"));
        self.close();
        self.o.push('\n');
        Ok(())
    }

    /// The module around the rule functions: the types, then the rules, then the
    /// helpers the text calls.
    fn finish(self, starts: &[RuleId]) -> String {
        let g = self.g;
        let mut head = String::from("import { SyntaxError } from \"std/syntax\"\n\n");
        head.push_str("export type Row = enum {\n");
        for (row, holes) in &self.rows {
            let fields = vec!["String"; holes.len()].join(", ");
            let _ = match holes.is_empty() {
                true => writeln!(head, "    {row},"),
                false => writeln!(head, "    {row}({fields}),"),
            };
        }
        head.push_str("}\n\n");
        for &r in starts {
            let ty = type_name(&g.body(r).name);
            let _ = writeln!(head, "export type {ty} = {{ start: Int64, end: Int64 }}\n");
        }
        head.push_str("type Parse = {\n    pos: Int64,\n    last: Int64,\n    stop: Int64,\n    oplen: Int64,\n    quiet: Int64,\n    looking: Int64,\n    depth: Int64,\n    refused: Bool,\n    errors: Array<SyntaxError<Row>>,\n");
        for f in &g.flags {
            let _ = writeln!(head, "    {f}: Bool,");
        }
        head.push_str("}\n\n");
        let mut text = self.o;
        // Each helper calls only helpers later in the list, so one pass finds them.
        for (name, body) in helpers(g, &self.rows) {
            if text.contains(&format!("{name}(")) {
                text.push_str(&body);
                text.push('\n');
            }
        }
        // The last helper's blank line would end the file with two newlines.
        while text.ends_with("\n\n") {
            text.pop();
        }
        head + &text
    }

    /// Writes `e`, then the line that starts with `head` and ends in whether `e`
    /// matched. A call that is a rule's whole answer needs no local of its own.
    fn put(&mut self, head: &str, e: &E) -> Result<(), Refused> {
        let call = match e {
            E::Ref(r) if self.g.body(*r).syntactic == self.syntactic() => {
                format!("{}(text, p{})", self.function(*r), self.labels(*r, ""))
            }
            _ => self.hit(e)?,
        };
        self.line(format!("{head}{call}"));
        Ok(())
    }

    /// Writes the statements of `e`, and returns an expression with no side effect:
    /// whether `e` matched. When it is false and `p.refused` is unset, the cursor
    /// stands where it stood.
    fn hit(&mut self, e: &E) -> Result<String, Refused> {
        if !self.syntactic() {
            if let Some((test, wide)) = char_test(e) {
                let h = self.local("h");
                self.line(format!("let mut {h} = false"));
                self.open("if p.pos < p.stop {");
                self.line("let c = text[p.pos]");
                let width = if wide { "charWidth(c)" } else { "1" };
                if test == "true" {
                    self.line(format!("p.pos = p.pos + {width}"));
                    self.line(format!("{h} = true"));
                } else {
                    self.open(format!("if {test} {{"));
                    self.line(format!("p.pos = p.pos + {width}"));
                    self.line(format!("{h} = true"));
                    self.close();
                }
                self.close();
                return Ok(h);
            }
        }
        Ok(match e {
            E::Lit(l) => self.literal(l, true),
            E::Split(s) => {
                let l = Lit {
                    s: s.clone(),
                    word: false,
                };
                self.literal(&l, false)
            }
            E::Ref(r) => self.call(*r)?,
            E::Builtin(Builtin::Eof) => "p.pos >= p.stop".into(),
            E::Builtin(Builtin::NoNl) => "noNewline(text, p)".into(),
            E::Builtin(Builtin::Reserved | Builtin::Punct) if self.syntactic() => {
                return Err(self.no(
                    "`reserved` and `punct` read bytes, and a syntactic rule reads tokens".into(),
                ));
            }
            E::Builtin(Builtin::Reserved) => {
                let h = self.local("h");
                self.line(format!("let {h} = lexReserved(text, p)"));
                h
            }
            E::Builtin(Builtin::Punct) => {
                let h = self.local("h");
                self.line(format!("let {h} = lexPunct(text, p)"));
                h
            }
            E::Builtin(_) | E::Class(..) | E::Any => {
                return Err(
                    self.no("a character form in a syntactic rule, which reads tokens".into())
                );
            }
            E::Seq(v) if v.is_empty() => "true".into(),
            E::Seq(v) => self.seq(v)?,
            E::Alt(v) => {
                let h = self.local("h");
                self.put(&format!("let mut {h} = "), &v[0])?;
                for x in &v[1..] {
                    self.open(format!("if !{h} && !p.refused {{"));
                    self.put(&format!("{h} = "), x)?;
                    self.close();
                }
                h
            }
            E::Star(x) => self.star(x)?,
            E::Plus(x) => {
                let h = self.local("h");
                self.put(&format!("let mut {h} = "), x)?;
                self.open(format!("if {h} {{"));
                let rest = self.star(x)?;
                self.line(format!("{h} = {rest}"));
                self.close();
                h
            }
            E::Opt(x) => {
                self.hit(x)?;
                "!p.refused".into()
            }
            E::And(x) => self.look(x)?,
            E::Not(x) => format!("!{}", self.look(x)?),
            E::Try(x) => {
                let (q, l) = self.save();
                let h = self.local("h");
                self.line("p.quiet = p.quiet + 1");
                self.put(&format!("let {h} = "), x)?;
                self.line("p.quiet = p.quiet - 1");
                self.open(format!("if !{h} {{"));
                self.line("p.refused = false");
                self.restore(&q, &l);
                self.close();
                h
            }
            E::Field(_, x) | E::Guard(_, x) => self.hit(x)?,
            E::Throw(t) => {
                self.refuse(t, None);
                "false".into()
            }
            E::Skip(t) => {
                self.record(t, None);
                self.line("bump(text, p)");
                "true".into()
            }
            E::SetFlag(f, v, x) => {
                let flag = &self.g.flags[*f];
                let (old, h) = (self.local("f"), self.local("h"));
                self.line(format!("let {old} = p.{flag}"));
                self.line(format!("p.{flag} = {v}"));
                self.put(&format!("let {h} = "), x)?;
                self.line(format!("p.{flag} = {old}"));
                h
            }
            E::TestFlag(f) => format!("p.{}", self.g.flags[*f]),
            E::Many(x, c, Recover::None) => self.many(x, c, None)?,
            E::Many(x, c, Recover::Nest) => {
                let (max, t) = (self.g.nest_limit.as_ref()).ok_or_else(|| {
                    self.no("`recover nest`, and no `nest` line sets a limit".into())
                })?;
                let t = t.clone();
                self.many(x, c, Some((*max, &t)))?
            }
            E::Many(x, c, rec) => self.many_recovering(x, c, rec)?,
            E::List(x, sep, c) => self.list(x, sep, c)?,
            E::Embed(em) => self.embed(em)?,
            E::Climb(..) => {
                return Err(self.no("`climb` is the whole of a rule".into()));
            }
        })
    }

    /// A literal: one token in a syntactic rule, its bytes in a lexical one. `exact`
    /// says that a syntactic literal holds no longer token.
    fn literal(&mut self, l: &Lit, exact: bool) -> String {
        let h = self.local("h");
        let test = self.lit_test(l, exact);
        let n = l.s.len();
        if self.syntactic() {
            self.line(format!("let mut {h} = {test}"));
            self.open(format!("if {h} {{"));
            self.line(format!("{h} = advance(text, p, {n})"));
        } else {
            self.line(format!("let {h} = {test}"));
            self.open(format!("if {h} {{"));
            self.line(format!("p.pos = p.pos + {n}"));
        }
        self.close();
        h
    }

    /// Whether the literal `l` stands at the cursor. In a syntactic rule a word ends
    /// at a word's boundary and, when `exact`, punctuation yields to a longer token
    /// of the table.
    fn lit_test(&self, l: &Lit, exact: bool) -> String {
        let s = l.s.as_bytes();
        let n = s.len();
        let mut terms = vec![format!("p.pos + {n} <= p.stop")];
        terms.extend(bytes_at(s, 0));
        if self.syntactic() && l.word {
            terms.push(format!(
                "!(p.pos + {n} < p.stop && isWord(text[p.pos + {n}]))"
            ));
        } else if self.syntactic() && exact {
            for (_, longer, _) in &self.g.tokens {
                if longer.len() > n && longer.as_bytes().starts_with(s) {
                    let rest = bytes_at(&longer.as_bytes()[n..], n).join(" && ");
                    terms.push(format!("!(p.pos + {} <= p.stop && {rest})", longer.len()));
                }
            }
        }
        terms.join(" && ")
    }

    /// A call of rule `r`. A syntactic rule takes the trivia after a lexical rule's
    /// match, because that match is its token.
    fn call(&mut self, r: RuleId) -> Result<String, Refused> {
        let callee = self.g.body(r);
        if callee.syntactic && !self.syntactic() {
            return Err(self.no(format!(
                "it reads syntactic rule `{}`, and only `embed` starts one from bytes",
                callee.name
            )));
        }
        let h = self.local("h");
        let args = self.labels(r, "");
        let f = self.function(r);
        if self.syntactic() && !callee.syntactic {
            self.line(format!("let mut {h} = {f}(text, p{args})"));
            self.open(format!("if {h} {{"));
            self.line(format!("{h} = advance(text, p, 0)"));
            self.close();
        } else {
            self.line(format!("let {h} = {f}(text, p{args})"));
        }
        Ok(h)
    }

    /// Saves the cursor, and the end of the last token in a syntactic rule.
    fn save(&mut self) -> (String, String) {
        let (q, l) = (self.local("q"), self.local("l"));
        self.line(format!("let {q} = p.pos"));
        if self.syntactic() {
            self.line(format!("let {l} = p.last"));
        }
        (q, l)
    }

    fn restore(&mut self, q: &str, l: &str) {
        self.line(format!("p.pos = {q}"));
        if self.syntactic() {
            self.line(format!("p.last = {l}"));
        }
    }

    /// A sequence: each element runs while those before it matched. In a syntactic
    /// rule, an element that fails after the sequence moved is a refusal unless a
    /// predicate is looking. A label binds the elements after it.
    fn seq(&mut self, v: &[E]) -> Result<String, Refused> {
        let depth = self.scope.len();
        let (q, l) = self.save();
        let h = self.local("h");
        for x in v {
            if let E::Field(label, _) = x {
                if self.kept.contains(label) {
                    self.line(format!("let mut {label}From = p.pos"));
                    self.line(format!("let mut {label}To = p.pos"));
                }
            }
        }
        for (i, x) in v.iter().enumerate() {
            if i > 0 {
                self.open(format!("if {h} {{"));
            }
            let (inner, label) = match x {
                E::Field(label, y) => (&**y, Some(label)),
                _ => (x, None),
            };
            let label = label.filter(|l| self.kept.contains(*l));
            if let Some(label) = label {
                self.line(format!("{label}From = p.pos"));
            }
            match i {
                0 => self.put(&format!("let mut {h} = "), inner)?,
                _ => self.put(&format!("{h} = "), inner)?,
            }
            if i > 0 && self.syntactic() {
                self.open(format!(
                    "if !{h} && !p.refused && p.looking == 0 && p.pos != {q} {{"
                ));
                self.refuse_miss(inner);
                self.close();
            }
            if let Some(label) = label {
                let end = if self.syntactic() { "p.last" } else { "p.pos" };
                self.line(format!("{label}To = {end}"));
            }
            if let E::Field(label, _) = x {
                self.scope.push(label.clone());
            }
            if i > 0 {
                self.close();
            }
        }
        self.scope.truncate(depth);
        self.open(format!("if !{h} && !p.refused {{"));
        self.restore(&q, &l);
        self.close();
        Ok(h)
    }

    /// `e*`: stops when `e` fails or matches empty.
    fn star(&mut self, x: &E) -> Result<String, Refused> {
        if let (false, Some((test, wide))) = (self.syntactic(), char_test(x)) {
            self.open("while p.pos < p.stop {");
            self.line("let c = text[p.pos]");
            if test != "true" {
                self.open(format!("if !({test}) {{"));
                self.line("break");
                self.close();
            }
            let width = if wide { "charWidth(c)" } else { "1" };
            self.line(format!("p.pos = p.pos + {width}"));
            self.close();
            return Ok("true".into());
        }
        self.open("while true {");
        let q = self.local("q");
        self.line(format!("let {q} = p.pos"));
        let hit = self.hit(x)?;
        self.open(format!("if !{hit} || p.pos == {q} {{"));
        self.line("break");
        self.close();
        self.close();
        Ok("!p.refused".into())
    }

    /// `&e` as an expression: whether `e` matches here, moving nothing and refusing
    /// nothing.
    fn look(&mut self, x: &E) -> Result<String, Refused> {
        if let Some(test) = self.pure_test(x) {
            let h = self.local("h");
            self.line(format!("let {h} = {test}"));
            return Ok(h);
        }
        let (q, l) = self.save();
        let h = self.local("h");
        self.line("p.looking = p.looking + 1");
        self.line("p.quiet = p.quiet + 1");
        self.put(&format!("let {h} = "), x)?;
        self.line("p.looking = p.looking - 1");
        self.line("p.quiet = p.quiet - 1");
        self.line("p.refused = false");
        self.restore(&q, &l);
        Ok(h)
    }

    /// A test of `e` that moves nothing and can refuse nothing, when `e` has one.
    fn pure_test(&self, e: &E) -> Option<String> {
        match e {
            E::Lit(l) => Some(self.lit_test(l, true)),
            E::Builtin(Builtin::Eof) => Some("p.pos >= p.stop".into()),
            E::Builtin(Builtin::NoNl) => Some("noNewline(text, p)".into()),
            E::TestFlag(f) => Some(format!("p.{}", self.g.flags[*f])),
            E::Not(x) => Some(format!("!({})", self.pure_test(x)?)),
            E::Alt(v) => {
                let tests = v.iter().map(|x| self.pure_test(x));
                let tests = tests.collect::<Option<Vec<_>>>()?;
                Some(format!("({})", tests.join(") || (")))
            }
            E::Guard(_, x) => self.pure_test(x),
            _ => None,
        }
    }

    /// `many X until C`: while C does not match, X must. With a nesting limit, each
    /// round counts as one level on top of the depth the loop started at.
    fn many(&mut self, x: &E, c: &E, nest: Option<(u32, &Throw)>) -> Result<String, Refused> {
        let h = self.local("h");
        self.line(format!("let mut {h} = true"));
        let links = self.local("n");
        if nest.is_some() {
            self.line(format!("let mut {links} = 0"));
        }
        self.open("while true {");
        let until = self.look(c)?;
        self.open(format!("if {until} {{"));
        self.line("break");
        self.close();
        if let Some((max, t)) = nest {
            self.line(format!("{links} = {links} + 1"));
            self.open(format!("if p.depth + {links} >= {max} {{"));
            self.refuse(t, None);
            self.line(format!("{h} = false"));
            self.line("break");
            self.close();
        }
        let q = self.local("q");
        self.line(format!("let {q} = p.pos"));
        let item = self.hit(x)?;
        self.open(format!("if !{item} {{"));
        self.open("if !p.refused {");
        self.refuse_miss(x);
        self.close();
        self.line(format!("{h} = false"));
        self.line("break");
        self.close();
        self.open(format!("if p.pos == {q} {{"));
        self.line("break");
        self.close();
        self.close();
        Ok(h)
    }

    /// `many X until C recover ..`: a refused item is recorded and the loop resumes
    /// at the next statement, or at the next token that starts a declaration. A round
    /// moves the cursor or the loop stops: an item that matches empty ends it, and a
    /// recovery skips a token.
    fn many_recovering(&mut self, x: &E, c: &E, rec: &Recover) -> Result<String, Refused> {
        self.open("while true {");
        let until = self.look(c)?;
        self.open(format!("if {until} {{"));
        self.line("break");
        self.close();
        let (q, quiet) = (self.local("q"), self.local("q"));
        self.line(format!("let {q} = p.pos"));
        self.line(format!("let {quiet} = p.quiet"));
        self.open("if p.looking == 0 {");
        self.line("p.quiet = 0");
        self.close();
        let item = self.local("h");
        self.put(&format!("let {item} = "), x)?;
        self.open(format!("if !{item} && !p.refused {{"));
        self.refuse_miss(x);
        self.close();
        self.line(format!("p.quiet = {quiet}"));
        self.open(format!("if {item} {{"));
        self.open(format!("if p.pos == {q} {{"));
        self.line("break");
        self.close();
        self.ind -= 1;
        self.open("} else {");
        self.line("p.refused = false");
        match rec {
            Recover::Decl(starts) => {
                self.line("bump(text, p)");
                let depth = self.local("d");
                self.line(format!("let mut {depth} = 0"));
                self.open("while p.pos < p.stop {");
                self.open(format!("if {depth} == 0 {{"));
                let start = self.look(starts)?;
                self.open(format!("if {start} {{"));
                self.line("break");
                self.close();
                self.close();
                self.open("if text[p.pos] == '{' {");
                self.line(format!("{depth} = {depth} + 1"));
                self.close();
                self.open(format!("if text[p.pos] == '}}' && {depth} > 0 {{"));
                self.line(format!("{depth} = {depth} - 1"));
                self.close();
                self.line("bump(text, p)");
                self.close();
            }
            _ => self.line(format!("syncToStatement(text, p, {q})")),
        }
        self.open(format!("if p.pos <= {q} {{"));
        self.line("break");
        self.close();
        self.close();
        self.close();
        Ok("true".into())
    }

    /// `list X sep S until C`: while C does not match, X must; then S or stop.
    fn list(&mut self, x: &E, sep: &E, c: &E) -> Result<String, Refused> {
        let h = self.local("h");
        self.line(format!("let mut {h} = true"));
        self.open("while true {");
        let q = self.local("q");
        self.line(format!("let {q} = p.pos"));
        let until = self.look(c)?;
        self.open(format!("if {until} {{"));
        self.line("break");
        self.close();
        let item = self.hit(x)?;
        self.open(format!("if !{item} {{"));
        self.open("if !p.refused {");
        self.refuse_miss(x);
        self.close();
        self.line(format!("{h} = false"));
        self.line("break");
        self.close();
        let more = self.hit(sep)?;
        self.open(format!("if !{more} || p.pos == {q} {{"));
        self.line(format!("{h} = !p.refused"));
        self.line("break");
        self.close();
        self.close();
        Ok(h)
    }

    /// `embed(extent, Rule, wrap=^Row, trailing=^Row)`: matches the extent, then
    /// parses exactly that text with `Rule`, unless a predicate is looking.
    fn embed(&mut self, em: &Embed) -> Result<String, Refused> {
        let from = self.local("q");
        self.line(format!("let {from} = p.pos"));
        let hit = self.hit(&em.extent)?;
        let h = self.local("h");
        self.line(format!("let mut {h} = {hit}"));
        self.open(format!("if {h} && p.looking == 0 {{"));
        let (to, stop, last) = (self.local("q"), self.local("q"), self.local("l"));
        self.line(format!("let {to} = p.pos"));
        self.line(format!("let {stop} = p.stop"));
        self.line(format!("let {last} = p.last"));
        self.line(format!("p.pos = {from}"));
        self.line(format!("p.stop = {to}"));
        if self.g.rule("trivia").is_some() {
            self.line("lexTrivia(text, p)");
        }
        let f = self.function(em.inner);
        let args = self.labels(em.inner, "");
        self.line(format!("{h} = {f}(text, p{args})"));
        self.open(format!("if !{h} && !p.refused {{"));
        self.refuse_miss(&E::Ref(em.inner));
        self.close();
        if let Some(t) = &em.wrap {
            self.open("if p.refused && p.quiet == 0 {");
            self.open("if let Some(e) = p.errors.pop() {");
            let (line, col) = self.anchors(t);
            let row = self.row(t, None, Some("rowName(e.row)"), Some((&from, &to)));
            self.line(format!("record(text, p, {row}, {line}, {col})"));
            self.close();
            self.close();
        }
        if let Some(t) = &em.trailing {
            self.open(format!("if {h} && !p.refused && p.pos < {to} {{"));
            self.line(format!("{h} = false"));
            self.refuse_at(t, Some((&from, &to)));
            self.close();
        }
        self.line(format!("p.stop = {stop}"));
        self.open("if !p.refused {");
        self.line(format!("p.pos = {to}"));
        self.line(format!("p.last = {last}"));
        self.close();
        self.close();
        Ok(h)
    }

    /// The offsets a refusal's line and column read.
    fn anchors(&self, t: &Throw) -> (String, String) {
        let at = |a: At| match a {
            At::Label(i) if self.scope.contains(&t.labels[i]) => {
                format!("{}From", t.labels[i])
            }
            _ => "p.pos".to_string(),
        };
        (at(t.line), at(t.col))
    }

    /// The row a refusal states, as a Vyrn expression. A hole this site does not fill
    /// is empty, and so is a label that is not bound here.
    fn row(
        &self,
        t: &Throw,
        token: Option<&str>,
        inner: Option<&str>,
        extent: Option<(&str, &str)>,
    ) -> String {
        let holes = &self.rows[&t.row];
        if holes.is_empty() {
            return t.row.to_string();
        }
        let mut args = Vec::new();
        for hole in holes {
            let site = t.holes.iter().find(|h| h.0 == *hole).map(|h| &h.1);
            args.push(match site {
                Some(HoleSrc::Text(x)) => string_lit(x),
                Some(HoleSrc::Label(l)) if self.scope.contains(l) => {
                    format!("textOf(text, {l}From, {l}To)")
                }
                Some(HoleSrc::Char { debug: false }) => "charHere(text, p)".to_string(),
                Some(HoleSrc::Char { debug: true }) => {
                    "\"'\" + charHere(text, p) + \"'\"".to_string()
                }
                Some(HoleSrc::Found) => "textOf(text, p.pos, tokenEnd(text, p, p.pos))".to_string(),
                Some(HoleSrc::Token) => string_lit(token.unwrap_or("")),
                Some(HoleSrc::Inner) => inner.unwrap_or("\"\"").to_string(),
                Some(HoleSrc::Extent) => match extent {
                    Some((a, b)) => format!("trimmed(text, {a}, {b})"),
                    None => "\"\"".to_string(),
                },
                Some(HoleSrc::Label(_)) | None => "\"\"".to_string(),
            });
        }
        format!("{}({})", t.row, args.join(", "))
    }

    /// Records a refusal, unless a predicate or an `@e` discards it.
    fn record(&mut self, t: &Throw, token: Option<&str>) {
        let (line, col) = self.anchors(t);
        let row = self.row(t, token, None, None);
        self.open("if p.quiet == 0 {");
        self.line(format!("record(text, p, {row}, {line}, {col})"));
        self.close();
    }

    /// Refuses with `t`. A `!` passes the token here before recovery starts.
    fn refuse(&mut self, t: &Throw, token: Option<&str>) {
        self.record(t, token);
        self.line("p.refused = true");
        if t.past {
            self.line("bump(text, p)");
        }
    }

    /// Refuses with an `embed`'s trailing row.
    fn refuse_at(&mut self, t: &Throw, extent: Option<(&str, &str)>) {
        let (line, col) = self.anchors(t);
        let row = self.row(t, None, None, extent);
        self.open("if p.quiet == 0 {");
        self.line(format!("record(text, p, {row}, {line}, {col})"));
        self.close();
        self.line("p.refused = true");
    }

    fn refuse_expected(&mut self, token: &str) {
        let t = self.g.expect.clone();
        self.refuse(&t, Some(token));
    }

    /// Refuses as a committed failure of `e` does.
    fn refuse_miss(&mut self, e: &E) {
        match miss(self.g, e) {
            Miss::Row(t) => self.refuse(t, None),
            Miss::Expected(name) => self.refuse_expected(&name),
        }
    }
}

/// The helpers a written module may call, each before the helpers it calls.
fn helpers(g: &Grammar, rows: &BTreeMap<Rc<str>, Vec<Rc<str>>>) -> Vec<(&'static str, String)> {
    let trivia = match g.rule("trivia").is_some() {
        true => "    lexTrivia(text, p)\n",
        false => "",
    };
    let token_try = match g.rule("token").is_some() {
        true => {
            "    let q = p.pos
    let refused = p.refused
    p.pos = at
    p.quiet = p.quiet + 1
    p.looking = p.looking + 1
    let hit = lexToken(text, p)
    let end = p.pos
    p.quiet = p.quiet - 1
    p.looking = p.looking - 1
    p.refused = refused
    p.pos = q
    if hit && end > at {
        return end
    }
"
        }
        false => "",
    };
    let mut row_arms = String::new();
    for (row, holes) in rows {
        let bind = match holes.len() {
            0 => String::new(),
            n => {
                let names: Vec<String> = (0..n).map(|i| format!("h{i}")).collect();
                format!("({})", names.join(", "))
            }
        };
        let _ = writeln!(row_arms, "        {row}{bind} => {},", string_lit(row));
    }
    let mut punct = String::new();
    let mut spellings: Vec<&(String, String, bool)> = g.tokens.iter().filter(|t| !t.2).collect();
    spellings.sort_by_key(|t| std::cmp::Reverse(t.1.len()));
    for (_, s, _) in spellings {
        let bytes = bytes_at(s.as_bytes(), 0).join(" && ");
        let n = s.len();
        let _ = writeln!(
            punct,
            "    if p.pos + {n} <= p.stop && {bytes} {{\n        p.pos = p.pos + {n}\n        return true\n    }}"
        );
    }
    let mut words = String::new();
    for (_, s, keyword) in &g.tokens {
        if *keyword {
            let n = s.len();
            let bytes = bytes_at(s.as_bytes(), 0).join(" && ");
            let _ = writeln!(
                words,
                "    if n == {n} && {bytes} {{\n        p.pos = p.pos + n\n        return true\n    }}"
            );
        }
    }
    vec![
        (
            "syncToStatement",
            "/// Skips a failed statement's tokens to a token on a new line at this block's
/// depth, past a `;` at that depth, to the block's `}`, or to the end. A statement
/// that consumed nothing loses one token first.
fn syncToStatement(text: String, p: modify Parse, start: Int64) {
    if p.pos == start {
        bump(text, p)
    }
    let mut depth = 0
    while p.pos < p.stop {
        let c = text[p.pos]
        if c == '{' {
            depth = depth + 1
        } else if c == '}' {
            if depth == 0 {
                return
            }
            depth = depth - 1
        } else if c == ';' && depth == 0 {
            bump(text, p)
            return
        } else if depth == 0 && !noNewline(text, p) {
            return
        }
        bump(text, p)
    }
}
"
            .to_string(),
        ),
        (
            "bump",
            "/// Passes the token at the cursor.
fn bump(text: String, p: modify Parse) {
    p.pos = tokenEnd(text, p, p.pos)
    advance(text, p, 0)
}
"
            .to_string(),
        ),
        (
            "advance",
            format!(
                "/// Passes a token of `n` bytes and the trivia after it. False if the trivia refuses.
fn advance(text: String, p: modify Parse, n: Int64) -> Bool {{
    p.pos = p.pos + n
    p.last = p.pos
{trivia}    return !p.refused
}}
"
            ),
        ),
        (
            "noNewline",
            "/// Whether the token at the cursor starts on the line of the token before it.
fn noNewline(text: String, p: Parse) -> Bool {
    let mut i = p.last
    while i < p.pos {
        if text[i] == '\\n' {
            return false
        }
        i = i + 1
    }
    return true
}
"
            .to_string(),
        ),
        (
            "lexReserved",
            format!(
                "/// Passes a word the token table reserves.
fn lexReserved(text: String, p: modify Parse) -> Bool {{
    let mut n = 0
    while p.pos + n < p.stop && isWord(text[p.pos + n]) {{
        n = n + 1
    }}
{words}    return false
}}
"
            ),
        ),
        (
            "lexPunct",
            format!(
                "/// Passes the longest punctuation spelling of the token table.
fn lexPunct(text: String, p: modify Parse) -> Bool {{
{punct}    return false
}}
"
            ),
        ),
        (
            "rowName",
            format!(
                "/// The name of a row, the text of a hole that holds a refusal.
fn rowName(row: Row) -> String {{
    return match row {{
{row_arms}    }}
}}
"
            ),
        ),
        (
            "trimmed",
            "/// The text between two offsets, without the whitespace at its ends.
fn trimmed(text: String, from: Int64, to: Int64) -> String {
    let mut a = from
    let mut b = to
    while a < b && (text[a] == ' ' || (text[a] >= '\\t' && text[a] <= '\\r')) {
        a = a + 1
    }
    while a < b && (text[b - 1] == ' ' || (text[b - 1] >= '\\t' && text[b - 1] <= '\\r')) {
        b = b - 1
    }
    return textOf(text, a, b)
}
"
            .to_string(),
        ),
        (
            "record",
            "/// Records a refusal: its line reads `lineAt`, its column and its end `colAt`.
fn record(text: String, p: modify Parse, row: consume Row, lineAt: Int64, colAt: Int64) {
    let mut line = 1
    let mut i = 0
    while i < lineAt {
        if text[i] == '\\n' {
            line = line + 1
        }
        i = i + 1
    }
    let mut col = 1
    i = colAt
    while i > 0 && text[i - 1] != '\\n' {
        col = col + 1
        i = i - 1
    }
    p.errors.push(SyntaxError { row: row, line: line, col: col, end: tokenEnd(text, p, colAt) })
}
"
            .to_string(),
        ),
        (
            "charHere",
            "/// The character at the cursor, or empty at the end.
fn charHere(text: String, p: Parse) -> String {
    if p.pos >= p.stop {
        return \"\"
    }
    return textOf(text, p.pos, p.pos + charWidth(text[p.pos]))
}
"
            .to_string(),
        ),
        (
            "tokenEnd",
            format!(
                "/// The offset past the token at `at`: what the `token` rule matches, a word, or
/// one character.
fn tokenEnd(text: String, p: modify Parse, at: Int64) -> Int64 {{
    if at >= p.stop {{
        return at
    }}
{token_try}    let mut i = at
    while i < p.stop && isWord(text[i]) {{
        i = i + 1
    }}
    if i > at {{
        return i
    }}
    return at + charWidth(text[at])
}}
"
            ),
        ),
        (
            "textOf",
            "/// The text between two offsets, which stand on character boundaries.
fn textOf(text: String, from: Int64, to: Int64) -> String {
    return stringFromBytes(bytes(text, from, to)) ?? \"\"
}
"
            .to_string(),
        ),
        (
            "isWord",
            "/// Whether `c` continues a word. A byte outside ASCII belongs to a letter.
fn isWord(c: UInt8) -> Bool {
    return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '_' || c >= '\\x80'
}
"
            .to_string(),
        ),
        (
            "charWidth",
            "/// The bytes of the character whose first byte is `c`.
fn charWidth(c: UInt8) -> Int64 {
    if c < '\\x80' {
        return 1
    }
    if c < '\\xe0' {
        return 2
    }
    if c < '\\xf0' {
        return 3
    }
    return 4
}
"
            .to_string(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use crate::{load, parser};

    fn text(file: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
        std::fs::read_to_string(path).expect("the grammar reads")
    }

    #[test]
    fn a_flag_no_rule_sets_is_refused() {
        let g = load(&text("vyrn.syntax")).expect("vyrn.syntax loads");
        let err = parser(&g).err().expect("refused");
        assert!(err.sentence.contains("and no rule sets it"), "{err:?}");
    }

    fn refusal(grammar: &str) -> String {
        let g = load(grammar).expect("the grammar loads");
        parser(&g).err().expect("refused").sentence
    }

    #[test]
    fn a_grammar_with_no_start_rule_is_refused() {
        let sentence = refusal("expect ^E\nA = '(' B ')' / 'x'\nB = '[' A ']'\n");
        assert!(sentence.contains("no start rule"), "{sentence}");
    }

    #[test]
    fn a_start_rule_that_reads_a_label_is_refused() {
        let sentence = refusal("expect ^E\nA = 'x' ^R@q\n");
        assert!(sentence.contains("no caller binds"), "{sentence}");
    }

    #[test]
    fn a_rule_on_a_name_the_module_holds_is_refused() {
        for grammar in ["expect ^A\nA = 'x'\n", "expect ^E\nRow = 'x'\n"] {
            let sentence = refusal(grammar);
            assert!(sentence.contains("already has that name"), "{sentence}");
        }
    }

    #[test]
    fn a_character_form_in_a_syntactic_rule_is_refused() {
        let sentence = refusal("expect ^E\nA = [a-z]\n");
        assert!(sentence.contains("reads tokens"), "{sentence}");
    }

    #[test]
    fn the_format_grammar_writes_a_parser() {
        let g = load(&text("syntax.syntax")).expect("syntax.syntax loads");
        let written = parser(&g).expect("it lowers");
        assert!(
            written.contains("export fn parseFile(text: String)"),
            "{written}"
        );
    }
}
