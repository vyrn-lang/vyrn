//! Writes `vyrn-frontend`'s token table and scanner from a grammar.
//!
//! [`lexer`] writes `Tok` and its spellings from the `keyword` and `token` lines
//! and the contextual words from the syntactic rules, and it lowers each lexical
//! rule that `scan` reaches to one method of `Scanner`. A rule that
//! matches calls the host's hook of the same name with its extent and with the
//! labels it reads from its callers; the host builds the tokens and decodes
//! their values. The output is committed as
//! `compiler/vyrn-frontend/src/lexer/grammar.rs`, and the test
//! `the_lexer_is_fresh` fails when that file differs from a fresh run.
//!
//! The lowering refuses what it cannot keep exact: a syntactic form in a lexical
//! rule, a rule call or a refusal inside a predicate, a label no caller binds,
//! and a hook that a later failure could undo, because a hook cannot be taken
//! back.

use std::collections::BTreeSet;
use std::fmt::Write;
use std::rc::Rc;

use crate::{At, Builtin, Grammar, HoleSrc, Lit, Recover, RuleId, Throw, E};

/// The scanner's own names, which no rule or label may take.
const TAKEN: &[&str] = &[
    "new",
    "skip_to",
    "eat_while",
    "eat_byte",
    "eat_lit",
    "eat_one",
    "peek_byte",
    "at_eof",
    "char_text",
    "punct",
    "hooks",
    "src",
    "pos",
    "start",
    "p",
    "p0",
    "m",
    "rule",
];

/// Returns the Rust source of `g`'s token table and of the scanner for its `scan`
/// rule, or why the grammar's lexical rules do not lower.
pub fn lexer(g: &Grammar) -> Result<String, String> {
    let scan = g.scan_rule.ok_or("the grammar has no `scan` rule")?;
    let rules = reachable(g, scan)?;
    let mut l = Lower {
        g,
        fallible: vec![false; g.rules.len()],
        free: vec![BTreeSet::new(); g.rules.len()],
    };
    l.solve(&rules)?;
    for &r in &rules {
        let rule = g.body(r);
        if rule.fail.is_some() || (rule.nest && g.nest_limit.is_none()) {
            return Err(format!(
                "lexical rule `{}` has `fail`, or `nest` and no limit",
                rule.name
            ));
        }
        if TAKEN.contains(&snake(&rule.name).as_str()) {
            return Err(format!(
                "rule `{}` takes a name the scanner uses",
                rule.name
            ));
        }
        l.undo_free(&rule.body)
            .map_err(|e| format!("rule `{}`: {e}", rule.name))?;
    }
    if l.fallible[scan.0 as usize] || !l.free[scan.0 as usize].is_empty() {
        return Err("`scan` must end in a refusal or eof and read no label".into());
    }
    Ok(l.emit(&rules, scan))
}

/// The lexical rules `scan` reaches, in file order. An `embed` reaches its
/// extent only, because the scan matches the island without parsing it.
fn reachable(g: &Grammar, scan: RuleId) -> Result<Vec<RuleId>, String> {
    let mut seen = vec![false; g.rules.len()];
    let mut todo = vec![scan];
    while let Some(r) = todo.pop() {
        if std::mem::replace(&mut seen[r.0 as usize], true) {
            continue;
        }
        if g.body(r).syntactic {
            return Err(format!(
                "`scan` reaches syntactic rule `{}`",
                g.body(r).name
            ));
        }
        walk(&g.body(r).body, &mut |e| {
            if let E::Ref(callee) = e {
                todo.push(*callee);
            }
        });
    }
    Ok((0..g.rules.len())
        .filter(|&i| seen[i])
        .map(|i| RuleId(i as u32))
        .collect())
}

/// Calls `f` on `e` and on every expression inside it, outermost first. The rule
/// an `embed` parses its island with is not inside it.
pub(crate) fn walk(e: &E, f: &mut dyn FnMut(&E)) {
    f(e);
    match e {
        E::Seq(v) | E::Alt(v) => v.iter().for_each(|x| walk(x, f)),
        E::Star(x) | E::Plus(x) | E::Opt(x) | E::And(x) | E::Not(x) | E::Try(x) => walk(x, f),
        E::Field(_, x) | E::Guard(_, x) | E::SetFlag(_, _, x) | E::Climb(x, _) => walk(x, f),
        E::Embed(em) => walk(&em.extent, f),
        E::Many(a, b, recover) => {
            walk(a, f);
            walk(b, f);
            if let Recover::Decl(c) = recover {
                walk(c, f);
            }
        }
        E::List(a, b, c) => [a, b, c].iter().for_each(|x| walk(x, f)),
        _ => {}
    }
}

/// Whether `e` or an expression inside it satisfies `p`.
fn any(e: &E, p: &dyn Fn(&E) -> bool) -> bool {
    let mut hit = false;
    walk(e, &mut |x| hit |= p(x));
    hit
}

/// The contextual words: the words of syntactic rules that no `keyword` line
/// reserves, in the order the grammar first names them.
fn contextual(g: &Grammar) -> Vec<Rc<str>> {
    let mut words: Vec<Rc<str>> = Vec::new();
    let mut add = |l: &Lit| {
        if l.word && !g.reserved.contains(&*l.s) && !words.contains(&l.s) {
            words.push(l.s.clone());
        }
    };
    for rule in g.rules.iter().filter(|r| r.syntactic) {
        walk(&rule.body, &mut |e| match e {
            E::Lit(l) => add(l),
            E::Climb(_, levels) => levels
                .iter()
                .flat_map(|level| &level.ops)
                .for_each(&mut add),
            _ => {}
        });
    }
    words
}

/// The punctuation lines of `g`, longest spelling first, in file order otherwise.
fn punct_lines(g: &Grammar) -> Vec<&(String, String, bool)> {
    let mut punct: Vec<_> = g.tokens.iter().filter(|t| !t.2).collect();
    punct.sort_by_key(|t| std::cmp::Reverse(t.1.len()));
    punct
}

/// `strItem` as `str_item`.
pub(crate) fn snake(name: &str) -> String {
    let mut s = String::new();
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            s.push('_');
        }
        s.push(c.to_ascii_lowercase());
    }
    s
}

/// What the lowering knows of each rule, by rule index.
struct Lower<'g> {
    g: &'g Grammar,
    /// Whether the rule can fail without a refusal.
    fallible: Vec<bool>,
    /// The labels the rule reads that it does not bind: its parameters.
    free: Vec<BTreeSet<Rc<str>>>,
}

impl Lower<'_> {
    /// Computes `fallible` and `free` by iteration to their least fixed point.
    /// Both only grow, and each is bounded, so the loop ends.
    fn solve(&mut self, rules: &[RuleId]) -> Result<(), String> {
        loop {
            let mut moved = false;
            for &r in rules {
                let rule = self.g.body(r);
                let f = self.can_fail(&rule.body);
                let mut free = BTreeSet::new();
                self.reads(&rule.body, &mut Vec::new(), &mut free)
                    .map_err(|e| format!("rule `{}`: {e}", rule.name))?;
                let i = r.0 as usize;
                moved |= f != self.fallible[i] || free != self.free[i];
                self.fallible[i] = f;
                self.free[i] = free;
            }
            if !moved {
                return Ok(());
            }
        }
    }

    /// Whether `e` can fail without a refusal.
    fn can_fail(&self, e: &E) -> bool {
        match e {
            E::Star(_) | E::Opt(_) | E::Throw(_) => false,
            E::Seq(v) => v.iter().any(|x| self.can_fail(x)),
            E::Alt(v) => v.iter().all(|x| self.can_fail(x)),
            E::Plus(x) | E::Field(_, x) => self.can_fail(x),
            E::Embed(em) => self.can_fail(&em.extent),
            E::Ref(r) => self.fallible[r.0 as usize],
            _ => true,
        }
    }

    /// Adds to `out` each label `e` reads that `bound` does not hold. A label
    /// binds the rest of the sequence it sits in.
    fn reads(
        &self,
        e: &E,
        bound: &mut Vec<Rc<str>>,
        out: &mut BTreeSet<Rc<str>>,
    ) -> Result<(), String> {
        let read = |l: &Rc<str>, bound: &[Rc<str>], out: &mut BTreeSet<Rc<str>>| {
            if !bound.contains(l) {
                out.insert(l.clone());
            }
        };
        match e {
            E::Seq(v) => {
                let depth = bound.len();
                for x in v {
                    self.reads(x, bound, out)?;
                    if let E::Field(l, y) = x {
                        let mut inner = BTreeSet::new();
                        self.reads(y, &mut Vec::new(), &mut inner)?;
                        if inner.contains(l) {
                            return Err(format!("label `{l}` is read inside its own capture"));
                        }
                        if TAKEN.contains(&&**l) {
                            return Err(format!("label `{l}` takes a name the scanner uses"));
                        }
                        bound.push(l.clone());
                    }
                }
                bound.truncate(depth);
            }
            E::Alt(v) => v.iter().try_for_each(|x| self.reads(x, bound, out))?,
            E::Star(x)
            | E::Plus(x)
            | E::Opt(x)
            | E::And(x)
            | E::Not(x)
            | E::Guard(_, x)
            | E::Field(_, x) => self.reads(x, bound, out)?,
            E::Embed(em) => self.reads(&em.extent, bound, out)?,
            E::Ref(r) => self.free[r.0 as usize]
                .iter()
                .for_each(|l| read(l, bound, out)),
            E::Throw(t) => {
                if t.line != t.col {
                    return Err(format!("`{}` points its line and column apart", t.row));
                }
                if let At::Label(i) = t.line {
                    read(&t.labels[i], bound, out);
                }
                for (h, src) in &t.holes {
                    if !matches!(src, HoleSrc::Char { .. } | HoleSrc::Text(_)) {
                        return Err(format!("hole `{h}` of `{}` has no lexical source", t.row));
                    }
                }
            }
            E::Builtin(b @ (Builtin::NoNl | Builtin::Reserved)) => {
                return Err(format!("builtin {b:?} in a lexical rule"));
            }
            E::Lit(_) | E::Class(..) | E::Any | E::Builtin(_) => {}
            E::Split(_)
            | E::Try(_)
            | E::Skip(_)
            | E::Climb(..)
            | E::SetFlag(..)
            | E::TestFlag(_)
            | E::Many(..)
            | E::List(..) => {
                return Err("a syntactic form in a lexical rule".into());
            }
        }
        Ok(())
    }

    /// Whether matching `e` calls a hook.
    fn hooks(e: &E) -> bool {
        any(e, &|x| matches!(x, E::Ref(_) | E::Builtin(Builtin::Punct)))
    }

    /// Refuses `e` if a failure could undo a hook already called: a hook in a
    /// sequence followed by an element that can fail, or a hook or a refusal
    /// inside a predicate.
    fn undo_free(&self, e: &E) -> Result<(), String> {
        match e {
            E::Seq(v) => {
                for (i, x) in v.iter().enumerate() {
                    self.undo_free(x)?;
                    if Self::hooks(x) && v[i + 1..].iter().any(|y| self.can_fail(y)) {
                        return Err("a hook is followed by an element that can fail".into());
                    }
                }
            }
            E::And(x) | E::Not(x) => {
                if Self::hooks(x) || any(x, &|y| matches!(y, E::Throw(_))) {
                    return Err("a predicate calls a rule or refuses".into());
                }
            }
            E::Alt(v) => v.iter().try_for_each(|x| self.undo_free(x))?,
            E::Star(x) | E::Plus(x) | E::Opt(x) | E::Field(_, x) | E::Guard(_, x) => {
                self.undo_free(x)?
            }
            E::Embed(em) => self.undo_free(&em.extent)?,
            _ => {}
        }
        Ok(())
    }

    fn emit(&self, rules: &[RuleId], scan: RuleId) -> String {
        let mut o = String::new();
        o.push_str(PRELUDE);
        self.tokens(&mut o);
        o.push_str(HOOKS_HEAD);
        for &r in rules {
            let name = snake(&self.g.body(r).name);
            let params = self.params(r, ": usize");
            let _ = writeln!(
                o,
                "    fn {name}(&mut self, start: usize, end: usize{params}) -> Result<(), Box<Diagnostic>> {{\n        Ok(())\n    }}"
            );
        }
        o.push_str("}\n");
        o.push_str(SCANNER);
        self.punct(&mut o);
        for &r in rules {
            self.rule(&mut o, r, r == scan);
        }
        o.push_str("}\n");
        o
    }

    /// `, q: usize` for each free label of `r`, in name order.
    fn params(&self, r: RuleId, ty: &str) -> String {
        self.free[r.0 as usize]
            .iter()
            .map(|l| format!(", {l}{ty}"))
            .collect()
    }

    /// The token table: `Tok`, each direction of lookup on it, and the word lists.
    fn tokens(&self, o: &mut String) {
        let tokens = &self.g.tokens;
        let words: Vec<_> = tokens.iter().filter(|t| t.2).collect();
        let punct = punct_lines(self.g);
        let name_of = |t: &&(String, String, bool)| format!("Self::{} => {:?}", t.0, t.1);
        let tok_of = |t: &&(String, String, bool)| format!("{:?} => Tok::{}", t.1, t.0);
        o.push_str(
            "/// A lexical token kind.\n#[derive(Debug, Clone, PartialEq)]\npub enum Tok {\n",
        );
        o.push_str(UNSPELLED);
        for (name, _, _) in tokens {
            let _ = writeln!(o, "    {name},");
        }
        o.push_str("}\n\nimpl Tok {\n");
        lookup(
            o,
            "    ",
            "Returns the spelling of a reserved word, or `None`.",
            "pub(crate) fn keyword_text(&self) -> Option<&'static str>",
            "self",
            words.iter().map(name_of),
        );
        o.push('\n');
        lookup(
            o,
            "    ",
            "Returns the spelling of a punctuation token, or `None`.",
            "pub(super) fn punct_text(&self) -> Option<&'static str>",
            "self",
            punct.iter().map(name_of),
        );
        o.push_str("}\n\n");
        lookup(
            o,
            "",
            "Returns the reserved word `word` spells, or `None`.",
            "pub(super) fn keyword(word: &str) -> Option<Tok>",
            "word",
            words.iter().map(tok_of),
        );
        o.push('\n');
        lookup(
            o,
            "",
            "Returns the punctuation token a spelling names, or `None`.",
            "pub(crate) fn punct_tok(text: &str) -> Option<Tok>",
            "text",
            punct.iter().map(tok_of),
        );
        o.push_str("\n/// Every reserved word, in table order.\n");
        spellings(o, "KEYWORDS", words.iter().map(|t| t.1.as_str()));
        o.push_str("/// Every punctuation spelling, longest first.\n");
        spellings(o, "PUNCT_SPELLINGS", punct.iter().map(|t| t.1.as_str()));
        o.push_str(
            "/// The words the lexer hands back as identifiers and a syntactic rule reads by\n",
        );
        o.push_str("/// position, in the order the grammar first names them.\n");
        let contextual = contextual(self.g);
        spellings(o, "CONTEXTUAL", contextual.iter().map(|w| &**w));
    }

    /// The `punct` builtin: the punctuation table, longest spelling first.
    fn punct(&self, o: &mut String) {
        o.push_str("    /// Matches the longest punctuation spelling at the cursor.\n");
        o.push_str("    fn punct(&mut self) -> Result<bool, Box<Diagnostic>> {\n");
        o.push_str("        let (tok, n) = match &self.src.as_bytes()[self.pos..] {\n");
        for (name, s, _) in punct_lines(self.g) {
            let bytes: Vec<String> = s
                .bytes()
                .map(|b| format!("b'{}'", b.escape_ascii()))
                .collect();
            let _ = writeln!(
                o,
                "            [{}, ..] => (Tok::{name}, {}),",
                bytes.join(", "),
                s.len()
            );
        }
        o.push_str("            _ => return Ok(false),\n        };\n");
        o.push_str("        let start = self.pos;\n        self.pos += n;\n");
        o.push_str("        self.hooks.punct(start, self.pos, tok)?;\n        Ok(true)\n    }\n");
    }

    fn rule(&self, o: &mut String, r: RuleId, root: bool) {
        let rule = self.g.body(r);
        let name = snake(&rule.name);
        let params = self.params(r, ": usize");
        let args = self.params(r, "");
        let vis = if root { "pub(super) " } else { "" };
        let fallible = self.fallible[r.0 as usize];
        let ret = if fallible { "bool" } else { "()" };
        let _ = writeln!(
            o,
            "\n    {vis}fn {name}(&mut self{params}) -> Result<{ret}, Box<Diagnostic>> {{"
        );
        o.push_str("        let start = self.pos;\n");
        // A `nest` rule counts toward the limit while its body runs.
        let (enter, leave) = match (&self.g.nest_limit, rule.nest) {
            (Some((max, t)), true) => (
                format!(
                    "        if self.depth >= {max} {}\n        self.depth += 1;\n",
                    self.throw(t)
                ),
                "        self.depth -= 1;\n",
            ),
            _ => (String::new(), ""),
        };
        o.push_str(&enter);
        if fallible {
            let mut body = self.bool_expr(&rule.body, 2);
            // A rule's alternatives need no parentheses of their own.
            if let E::Alt(_) = rule.body {
                body = body[1..body.len() - 1].trim().to_string();
            }
            let _ = writeln!(o, "        let hit = {body};\n{leave}        if !hit {{\n            return Ok(false);\n        }}");
            let _ = writeln!(
                o,
                "        self.hooks.{name}(start, self.pos{args})?;\n        Ok(true)\n    }}"
            );
        } else {
            let body = self.unit_expr(&rule.body, 2);
            let _ = writeln!(
                o,
                "        {body}\n{leave}        self.hooks.{name}(start, self.pos{args})\n    }}"
            );
        }
    }

    /// `e` as a Rust expression of type `bool` that leaves the cursor where it
    /// was when it is false. `?` in it returns a refusal.
    fn bool_expr(&self, e: &E, ind: usize) -> String {
        if !self.can_fail(e) && !matches!(e, E::Throw(_)) {
            return format!("{{ {} true }}", self.unit_stmt(e, ind));
        }
        let nl = |ind: usize| format!("\n{}", "    ".repeat(ind + 1));
        // A one-byte literal stays a byte test, guarded or not.
        let byte = |x: &E| matches!(x, E::Lit(l) if l.s.len() == 1);
        let guarded_byte = matches!(e, E::Guard(_, x) if byte(x));
        if let (Some(p), false) = (self.pred(e), byte(e) || guarded_byte) {
            return format!("self.eat_one({})", closure(&p));
        }
        match e {
            E::Lit(l) if l.s.len() == 1 => {
                format!("self.eat_byte(b'{}')", l.s.as_bytes()[0].escape_ascii())
            }
            E::Lit(l) => format!("self.eat_lit(b\"{}\")", l.s.as_bytes().escape_ascii()),
            E::Builtin(b) => match b {
                Builtin::Eof => "self.at_eof()".into(),
                Builtin::Punct => "self.punct()?".into(),
                // `pred` lowered the character builtins, and `reads` refuses the
                // rest in a lexical rule.
                _ => unreachable!("lowered by Lower::pred or refused by Lower::reads"),
            },
            E::Ref(r) => format!("self.{}({})?", snake(&self.g.body(*r).name), self.args(*r)),
            E::Throw(t) => self.throw(t),
            E::Seq(v) => format!(
                "{{{}let p0 = self.pos;{}({}){}|| {{ self.pos = p0; false }}{}}}",
                nl(ind),
                nl(ind),
                self.chain(v, ind + 1),
                nl(ind),
                nl(ind - 1)
            ),
            E::Alt(v) => {
                let alts: Vec<String> = v.iter().map(|x| self.bool_expr(x, ind + 1)).collect();
                format!(
                    "({}{}{})",
                    nl(ind),
                    alts.join(&format!("{}|| ", nl(ind))),
                    nl(ind - 1)
                )
            }
            E::Plus(x) => format!(
                "({} && {{ {} true }})",
                self.bool_expr(x, ind),
                self.star(x, ind)
            ),
            E::And(x) => format!(
                "{{ let p = self.pos; let m = {}; self.pos = p; m }}",
                self.bool_expr(x, ind)
            ),
            E::Not(x) => format!(
                "!{{ let p = self.pos; let m = {}; self.pos = p; m }}",
                self.bool_expr(x, ind)
            ),
            E::Field(_, x) => self.bool_expr(x, ind),
            E::Embed(em) => self.bool_expr(&em.extent, ind),
            E::Guard(set, x) => format!(
                "(matches!(self.peek_byte(), Some({})) && {})",
                byte_pattern(set),
                self.bool_expr(x, ind)
            ),
            // `can_fail` is false for these, so the first line returned.
            E::Star(_) | E::Opt(_) => unreachable!("infallible"),
            // `reads` refuses the syntactic forms in a lexical rule.
            _ => unreachable!("refused by Lower::reads"),
        }
    }

    /// The elements of a sequence joined by `&&`; a label's `let` opens a
    /// block over the rest of the sequence, so only those elements can read it.
    fn chain(&self, v: &[E], ind: usize) -> String {
        let Some((x, rest)) = v.split_first() else {
            return "true".into();
        };
        match x {
            E::Field(l, y) if !rest.is_empty() => format!(
                "{{ let {l} = self.pos; {} && {} }}",
                self.bool_expr(y, ind),
                self.chain(rest, ind)
            ),
            _ if rest.is_empty() => self.bool_expr(x, ind),
            _ => format!("{} && {}", self.bool_expr(x, ind), self.chain(rest, ind)),
        }
    }

    /// `e`, which cannot fail, as a Rust expression of type `()`.
    fn unit_expr(&self, e: &E, ind: usize) -> String {
        format!("{{ {} }}", self.unit_stmt(e, ind))
    }

    /// `e`, which cannot fail, as Rust statements.
    fn unit_stmt(&self, e: &E, ind: usize) -> String {
        match e {
            E::Star(x) => self.star(x, ind),
            E::Opt(x) => format!("let _ = {};", self.bool_expr(x, ind)),
            E::Ref(r) => format!("self.{}({})?;", snake(&self.g.body(*r).name), self.args(*r)),
            E::Throw(t) => self.throw(t),
            E::Plus(x) | E::Field(_, x) => self.unit_stmt(x, ind),
            E::Embed(em) => self.unit_stmt(&em.extent, ind),
            E::Seq(v) => {
                let mut s = String::new();
                for (i, x) in v.iter().enumerate() {
                    match x {
                        E::Field(l, y) if i + 1 < v.len() => {
                            let _ = write!(s, "let {l} = self.pos; {} ", self.unit_stmt(y, ind));
                        }
                        _ => {
                            let _ = write!(s, "{} ", self.unit_stmt(x, ind));
                        }
                    }
                }
                s
            }
            E::Alt(v) => {
                // Alternatives after the first one that cannot fail never run.
                let k = v
                    .iter()
                    .position(|x| !self.can_fail(x))
                    .unwrap_or(v.len() - 1);
                let tried: Vec<String> =
                    v[..k].iter().map(|x| self.bool_expr(x, ind + 1)).collect();
                let last = self.unit_stmt(&v[k], ind + 1);
                match tried.is_empty() {
                    true => last,
                    false => format!("if !({}) {{ {last} }}", tried.join(" || ")),
                }
            }
            _ => unreachable!("only an infallible expression is a statement"),
        }
    }

    /// `e` as a test of one character `c`, if `e` matches exactly one
    /// character and calls nothing: ordered choice among such tests is their
    /// union.
    fn pred(&self, e: &E) -> Option<String> {
        Some(match e {
            E::Class(ranges, neg) => {
                let pats: Vec<String> = (ranges.iter())
                    .map(|&(a, b)| match a == b {
                        true => format!("{a:?}"),
                        false => format!("{a:?}..={b:?}"),
                    })
                    .collect();
                let not = if *neg { "!" } else { "" };
                format!("{not}matches!(c, {})", pats.join(" | "))
            }
            E::Any => "true".into(),
            E::Lit(l) if l.s.chars().count() == 1 => format!("c == {:?}", l.s.chars().next()?),
            E::Builtin(Builtin::Alpha) => "c.is_alphabetic()".into(),
            E::Builtin(Builtin::Alnum) => "c.is_alphanumeric()".into(),
            E::Builtin(Builtin::Digit) => "c.is_ascii_digit()".into(),
            E::Builtin(Builtin::Hex) => "c.is_ascii_hexdigit()".into(),
            E::Builtin(Builtin::Ws) => "c.is_whitespace()".into(),
            E::Guard(_, x) => return self.pred(x),
            E::Alt(v) => (v.iter().map(|x| self.pred(x)))
                .collect::<Option<Vec<String>>>()?
                .join(" || "),
            _ => return None,
        })
    }

    /// `e*`: stops when `e` fails or matches empty. `(!'c' .)*` for an ASCII
    /// `c` is a search for the byte `c`, because no other character's UTF-8
    /// holds that byte.
    fn star(&self, x: &E, ind: usize) -> String {
        if let E::Seq(v) = x {
            if let [E::Not(n), E::Any] = v.as_slice() {
                if let E::Lit(l) = &**n {
                    if let [c] = l.s.as_bytes() {
                        if c.is_ascii() {
                            return format!("self.skip_to(b'{}');", c.escape_ascii());
                        }
                    }
                }
            }
        }
        if let Some(p) = self.pred(x) {
            return format!("self.eat_while({});", closure(&p));
        }
        format!(
            "loop {{ let p = self.pos; if !{} || self.pos == p {{ break; }} }}",
            self.bool_expr(x, ind)
        )
    }

    fn args(&self, r: RuleId) -> String {
        let a: Vec<&str> = self.free[r.0 as usize].iter().map(|l| &**l).collect();
        a.join(", ")
    }

    /// A refusal: a block of type `!` that returns the host's error.
    fn throw(&self, t: &Throw) -> String {
        let at = match t.line {
            At::Here => "self.pos".to_string(),
            At::Label(i) => t.labels[i].to_string(),
        };
        let mut holes = String::new();
        for (h, src) in &t.holes {
            let v = match src {
                HoleSrc::Char { debug } => format!("self.char_text({debug})"),
                HoleSrc::Text(x) => format!("{x:?}"),
                _ => unreachable!("refused by Lower::reads"),
            };
            let _ = write!(holes, ", {h} = {v}");
        }
        format!(
            "{{ let rule = rule!({}{holes}); return Err(self.hooks.refuse({at}, rule)); }}",
            t.row
        )
    }
}

/// A predicate on `c` as a closure.
/// Writes a function indented by `ind` that returns the arm of `arms` matching
/// `subject`, or `None`.
fn lookup(
    o: &mut String,
    ind: &str,
    doc: &str,
    sign: &str,
    subject: &str,
    arms: impl Iterator<Item = String>,
) {
    let _ = writeln!(o, "{ind}/// {doc}\n{ind}{sign} {{");
    let _ = writeln!(o, "{ind}    Some(match {subject} {{");
    for arm in arms {
        let _ = writeln!(o, "{ind}        {arm},");
    }
    let _ = writeln!(o, "{ind}        _ => return None,\n{ind}    }})\n{ind}}}");
}

/// Writes `pub const name: &[&str]` with one spelling per line.
fn spellings<'a>(o: &mut String, name: &str, words: impl Iterator<Item = &'a str>) {
    let _ = writeln!(o, "pub const {name}: &[&str] = &[");
    for w in words {
        let _ = writeln!(o, "    {w:?},");
    }
    o.push_str("];\n\n");
}

fn closure(p: &str) -> String {
    match p {
        "true" => "|_| true".into(),
        _ => format!("|c| {p}"),
    }
}

/// A byte set as a Rust pattern of ranges.
fn byte_pattern(set: &[u64; 4]) -> String {
    let has = |b: usize| set[b >> 6] & (1 << (b & 63)) != 0;
    let mut pats = Vec::new();
    let mut b = 0;
    while b < 256 {
        if !has(b) {
            b += 1;
            continue;
        }
        let a = b;
        while b + 1 < 256 && has(b + 1) {
            b += 1;
        }
        let lit = |x: usize| format!("b'{}'", (x as u8).escape_ascii());
        pats.push(match a == b {
            true => lit(a),
            false => format!("{}..={}", lit(a), lit(b)),
        });
        b += 1;
    }
    pats.join(" | ")
}

const PRELUDE: &str = "\
// Written by vyrn_syntax::lexer from compiler/vyrn-syntax/vyrn.syntax. Do not
// edit: the test the_lexer_is_fresh fails when this file differs from a fresh
// run, and VYRN_PIN=write rewrites it.

//! The token table and the lexical rules of `vyrn.syntax` as Rust. [`Tok`] and
//! its lookups come from the `keyword` and `token` lines. [`Scanner`] has one
//! method per rule that `scan` reaches. A method returns `Ok(false)` with the
//! cursor unmoved when its rule does not match, and the host's error when the
//! rule refuses. The host's [`Hooks`] build the tokens.

use super::Hole;
use crate::diagnostics::Diagnostic;
use crate::rules::{rule, Rule};

";

/// The variants of `Tok` that have no spelling, so no line of a grammar states
/// them. Rust cannot split one enum across two files, and a nested enum would
/// wrap every token a pass matches, so this one list is the hand half of `Tok`.
const UNSPELLED: &str = "    Int(i64),
    /// A byte literal `'c'`: one ASCII byte, an integer literal the checker
    /// defaults to `UInt8`.
    Byte(u8),
    Float(f64),
    /// A string literal with its escapes decoded.
    Str(String),
    /// An interpolated string `\"a\\{e}b\"`. `parts` are the decoded fragments,
    /// always `exprs.len() + 1` of them; the parser parses `exprs`.
    TemplateStr {
        parts: Vec<String>,
        exprs: Vec<Hole>,
    },
    /// A `///` doc line as markdown, one leading space stripped. The parser attaches
    /// it to the next declaration.
    Doc(String),
    Ident(String),
    Eof,
";

const HOOKS_HEAD: &str = "\
/// What the host does when a lexical rule matches. A rule calls its hook once
/// it has matched `start..end`, a byte range of the source, with the labels it
/// reads from its callers, each the byte where that label matched. The
/// scanner never undoes a match it has reported, and it reports the matches
/// inside a rule before the rule's own.
#[allow(unused_variables)]
pub(super) trait Hooks {
    /// The error that states `rule` at byte `at`.
    fn refuse(&mut self, at: usize, rule: Rule) -> Box<Diagnostic>;
    /// The longest punctuation spelling of the token table matched `start..end`.
    fn punct(&mut self, start: usize, end: usize, tok: Tok) -> Result<(), Box<Diagnostic>> {
        Ok(())
    }
";

const SCANNER: &str = "
/// A cursor over the source and the hooks it reports to.
pub(super) struct Scanner<'s, H> {
    src: &'s str,
    /// A byte offset on a character boundary.
    pos: usize,
    /// The `nest` rules running.
    depth: u32,
    pub(super) hooks: H,
}

impl<'s, H: Hooks> Scanner<'s, H> {
    pub(super) fn new(src: &'s str, hooks: H) -> Self {
        Scanner { src, pos: 0, depth: 0, hooks }
    }

    fn peek_byte(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos).copied()
    }

    fn at_eof(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn eat_byte(&mut self, b: u8) -> bool {
        let hit = self.peek_byte() == Some(b);
        self.pos += hit as usize;
        hit
    }

    fn eat_lit(&mut self, s: &[u8]) -> bool {
        let hit = self.src.as_bytes()[self.pos..].starts_with(s);
        if hit {
            self.pos += s.len();
        }
        hit
    }

    /// Consumes one character that satisfies `ok`.
    fn eat_one(&mut self, ok: impl Fn(char) -> bool) -> bool {
        let c = match self.peek_byte() {
            None => return false,
            Some(b) if b < 0x80 => b as char,
            Some(_) => match self.src[self.pos..].chars().next() {
                Some(c) => c,
                None => return false,
            },
        };
        let hit = ok(c);
        if hit {
            self.pos += c.len_utf8();
        }
        hit
    }

    /// Consumes characters while they satisfy `ok`.
    fn eat_while(&mut self, ok: impl Fn(char) -> bool) {
        while self.eat_one(&ok) {}
    }

    /// Moves the cursor to the next byte `b`, or to the end.
    fn skip_to(&mut self, b: u8) {
        let rest = &self.src.as_bytes()[self.pos..];
        self.pos += rest.iter().position(|&c| c == b).unwrap_or(rest.len());
    }

    /// The character at the cursor, as Rust's `Debug` spells it if `debug`,
    /// or empty at the end.
    fn char_text(&self, debug: bool) -> String {
        match self.src[self.pos..].chars().next() {
            Some(c) if debug => format!(\"{c:?}\"),
            Some(c) => c.to_string(),
            None => String::new(),
        }
    }

";

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn a_contextual_word_is_a_syntactic_word_no_keyword_reserves() {
        let grammar = "keyword Fn 'fn'\nexpect ^E\nA = 'fn' 'as' '-' b 'as' 'to'\nb = 'lexical'\n";
        let g = crate::load(grammar).expect("the grammar loads");
        let words = super::contextual(&g);
        assert_eq!(words.iter().map(|w| &**w).collect::<Vec<_>>(), ["as", "to"]);
    }

    #[test]
    fn the_lexer_is_fresh() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(dir.join("vyrn.syntax")).expect("vyrn.syntax reads");
        let g = crate::load(&text).expect("vyrn.syntax loads");
        let fresh = super::lexer(&g).expect("vyrn.syntax's lexical rules lower");
        let file = dir.join("../vyrn-frontend/src/lexer/grammar.rs");
        if std::env::var("VYRN_PIN").as_deref() == Ok("write") {
            std::fs::write(&file, &fresh).expect("write the scanner");
            return;
        }
        let committed = std::fs::read_to_string(&file).unwrap_or_default();
        let first = (committed.lines().zip(fresh.lines()).enumerate())
            .find(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| format!("line {}:\n  committed {a}\n  fresh     {b}", i + 1));
        assert!(
            committed == fresh,
            "lexer/grammar.rs differs from vyrn.syntax; rewrite it with VYRN_PIN=write\n{}",
            first.unwrap_or_else(|| "the files differ in length".into())
        );
    }
}
