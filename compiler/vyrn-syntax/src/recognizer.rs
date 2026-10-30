//! Writes a grammar's syntactic rules as a recognizer over `vyrn-frontend`'s tokens.
//!
//! [`recognizer`] lowers each syntactic rule that `Module` reaches to one method
//! of the host's `Recognizer`. A literal is a comparison on `Tok`, and a lexical
//! rule is the token its match lexes to ([`TERMINALS`]). A method returns
//! `Ok(false)` with the cursor unmoved when its rule does not match, and `Err`
//! when the rule refuses. The commit, the `expect` row and each `fail` row are the
//! grammar's: a failure after a sequence consumed a token is the refusal
//! [`Lower::err`] states for the element that failed. The output is committed as
//! `compiler/vyrn-frontend/src/recognizer/rules.rs`, and the test
//! `the_recognizer_is_fresh` fails when that file differs from a fresh run.

use std::collections::BTreeSet;
use std::fmt::Write;
use std::rc::Rc;

use crate::gen::{snake, walk};
use crate::{At, Builtin, Embed, Grammar, HoleSrc, Level, Recover, RuleId, Throw, E};

/// The lexical rules a syntactic rule reads, each with the tokens its match lexes
/// to. `string` is the host's, because a string holds holes.
const TERMINALS: &[(&str, &str)] = &[
    ("ident", "Tok::Ident(_)"),
    ("int", "Tok::Int(0..)"),
    ("number", "Tok::Int(_) | Tok::Float(_)"),
    ("plainStr", "Tok::Str(_)"),
    ("byte", "Tok::Byte(_)"),
    ("doc", "Tok::Doc(_)"),
];

/// The rules whose matches the host records as declarations.
const DECLS: &[&str] = &[
    "FnDecl", "Extern", "TypeDecl", "Global", "Import", "Protocol", "Contract", "Test", "Bench",
];

/// The rules whose close the host reads: the names a type reads as bound.
const HOOKS: &[&str] = &["TName", "TypeName", "IBind", "StmtExpr", "RCap", "IMethod"];

/// The rule a file is, and the rule the host reads an `embed` island with.
const ROOT: &str = "Module";

/// Returns the Rust source of `g`'s syntactic rules, or why they do not lower.
pub fn recognizer(g: &Grammar) -> Result<String, String> {
    let root = g.rule(ROOT).ok_or("the grammar has no `Module` rule")?;
    let rules = reachable(g, root);
    let mut l = Lower {
        g,
        free: vec![BTreeSet::new(); g.rules.len()],
    };
    l.solve(&rules);
    l.emit(&rules)
}

/// The syntactic rules `root` reaches, in file order. A lexical rule is a token,
/// so the walk stops there.
fn reachable(g: &Grammar, root: RuleId) -> Vec<RuleId> {
    let mut seen = vec![false; g.rules.len()];
    let mut todo = vec![root];
    while let Some(r) = todo.pop() {
        if !g.body(r).syntactic || std::mem::replace(&mut seen[r.0 as usize], true) {
            continue;
        }
        walk(&g.body(r).body, &mut |e| {
            if let E::Ref(callee) = e {
                todo.push(*callee);
            }
        });
    }
    (0..g.rules.len())
        .filter(|&i| seen[i])
        .map(|i| RuleId(i as u32))
        .collect()
}

/// The grammar's `embed`.
fn embed(g: &Grammar) -> Result<Rc<Embed>, String> {
    let mut found = None;
    for rule in &g.rules {
        walk(&rule.body, &mut |e| {
            if let E::Embed(em) = e {
                found = Some(em.clone());
            }
        });
    }
    found.ok_or_else(|| "the grammar has no `embed`".to_string())
}

struct Lower<'g> {
    g: &'g Grammar,
    /// The labels each rule reads that it does not bind: its parameters.
    free: Vec<BTreeSet<Rc<str>>>,
}

type Scope = Vec<Rc<str>>;

impl Lower<'_> {
    /// Computes `free` by iteration to its least fixed point. It only grows and is
    /// bounded by the grammar's labels, so the loop ends.
    fn solve(&mut self, rules: &[RuleId]) {
        loop {
            let mut moved = false;
            for &r in rules {
                let mut free = BTreeSet::new();
                self.reads(&self.g.body(r).body, &mut Vec::new(), &mut free);
                let i = r.0 as usize;
                moved |= free != self.free[i];
                self.free[i] = free;
            }
            if !moved {
                return;
            }
        }
    }

    /// Adds to `out` each label `e` reads that `bound` does not hold. A label binds
    /// the rest of the sequence it sits in.
    fn reads(&self, e: &E, bound: &mut Scope, out: &mut BTreeSet<Rc<str>>) {
        let mut read = |l: &Rc<str>, bound: &Scope| {
            if !bound.contains(l) {
                out.insert(l.clone());
            }
        };
        match e {
            E::Seq(v) => {
                let depth = bound.len();
                for x in v {
                    self.reads(x, bound, out);
                    if let E::Field(l, _) = x {
                        bound.push(l.clone());
                    }
                }
                bound.truncate(depth);
            }
            E::Alt(v) => v.iter().for_each(|x| self.reads(x, bound, out)),
            E::Star(x) | E::Plus(x) | E::Opt(x) | E::And(x) | E::Not(x) | E::Try(x) => {
                self.reads(x, bound, out)
            }
            E::Field(_, x) | E::SetFlag(_, _, x) | E::Climb(x, _) => self.reads(x, bound, out),
            E::Many(a, b, rec) => {
                self.reads(a, bound, out);
                self.reads(b, bound, out);
                if let Recover::Decl(c) = rec {
                    self.reads(c, bound, out);
                }
            }
            E::List(a, b, c) => [a, b, c].iter().for_each(|x| self.reads(x, bound, out)),
            E::Ref(r) if self.g.body(*r).syntactic => {
                self.free[r.0 as usize].iter().for_each(|l| read(l, bound));
            }
            E::Throw(t) | E::Skip(t) => {
                for at in [t.line, t.col] {
                    if let At::Label(i) = at {
                        read(&t.labels[i], bound);
                    }
                }
                for (_, src) in &t.holes {
                    if let HoleSrc::Label(l) = src {
                        read(l, bound);
                    }
                }
            }
            _ => {}
        }
    }

    fn emit(&self, rules: &[RuleId]) -> Result<String, String> {
        let mut o = String::from(PRELUDE);
        let mut decls: Vec<&str> = Vec::new();
        for &r in rules {
            let name = &*self.g.body(r).name;
            if DECLS.contains(&name) {
                decls.push(name);
            }
        }
        let _ = writeln!(
            o,
            "/// A declaration the recognizer records.\n#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]\npub enum Decl {{"
        );
        for d in decls {
            let _ = writeln!(o, "    {d},");
        }
        o.push_str("}\n\n/// A rule whose close the host reads.\n#[derive(Clone, Copy)]\npub(super) enum Hook {\n");
        for h in HOOKS {
            let _ = writeln!(o, "    {h},");
        }
        o.push_str("}\n\n/// The grammar's context flags.\n#[derive(Clone, Copy, Default)]\npub(super) struct Flags {\n");
        for f in &self.g.flags {
            let _ = writeln!(o, "    pub(super) {f}: bool,");
        }
        o.push_str("}\n\nimpl Recognizer<'_> {\n");
        let expected = self.throw(&self.g.expect, &Vec::new())?;
        let _ = writeln!(
            o,
            "    /// The `expect` row for the token named `token`.
    fn expected(&mut self, token: &str) -> R {{
        let me = self;
        {expected}
    }}\n"
        );
        self.string(&mut o)?;
        for &r in rules {
            self.rule(&mut o, r)?;
        }
        o.push_str("}\n");
        Ok(o)
    }

    /// The `string` token: the host reads each hole with the rule and the two rows
    /// of the grammar's `embed`.
    fn string(&self, o: &mut String) -> Result<(), String> {
        let em = embed(self.g)?;
        let hole_of = |t: &Option<Rc<Throw>>, want: fn(&HoleSrc) -> bool| {
            let t = t.as_ref().ok_or("an `embed` without its two rows")?;
            let (h, _) = (t.holes.iter())
                .find(|h| want(&h.1))
                .ok_or("an `embed` row without its hole")?;
            Ok::<_, String>((t.row.clone(), h.clone()))
        };
        let (wrap, detail) = hole_of(&em.wrap, |s| matches!(s, HoleSrc::Inner))?;
        let (trailing, text) = hole_of(&em.trailing, |s| matches!(s, HoleSrc::Extent))?;
        let inner = snake(&self.g.body(em.inner).name);
        let miss = self.err(&E::Ref(em.inner), &Vec::new())?;
        let _ = writeln!(
            o,
            "    /// The `string` token; the host parses each hole with `{name}`.
    fn string(&mut self, tagged: bool) -> R {{
        self.string_token(
            tagged,
            |me| {{
                if !me.{inner}()? {{
                    {miss}?;
                }}
                Ok(true)
            }},
            |{detail}| rule!({wrap}, {detail}),
            |{text}| rule!({trailing}, {text}),
        )
    }}",
            name = self.g.body(em.inner).name,
        );
        Ok(())
    }

    fn rule(&self, o: &mut String, r: RuleId) -> Result<(), String> {
        let rule = self.g.body(r);
        let name = snake(&rule.name);
        let free = &self.free[r.0 as usize];
        let params: String = free.iter().map(|l| format!(", l_{l}: Capture")).collect();
        let is_root = rule.name == ROOT || r == embed(self.g)?.inner;
        let vis = if is_root { "pub(super) " } else { "" };
        let mut scope: Scope = free.iter().cloned().collect();
        let _ = writeln!(o, "\n    {vis}fn {name}(&mut self{params}) -> R {{");
        if let E::Climb(operand, levels) = &rule.body {
            if !free.is_empty() {
                return Err(format!("climbing rule `{}` reads a label", rule.name));
            }
            return self.climb(o, &name, operand, levels);
        }
        o.push_str("        let me = self;\n");
        let leave = match (&self.g.nest_limit, rule.nest) {
            (Some((max, t)), true) => {
                let refuse = self.throw(t, &scope)?;
                let _ = writeln!(
                    o,
                    "        if me.depth >= {max} {{\n            return {refuse};\n        }}\n        me.depth += 1;"
                );
                "        me.depth -= 1;\n"
            }
            _ => "",
        };
        let decl = DECLS.contains(&&*rule.name);
        let hook = HOOKS.contains(&&*rule.name);
        if decl || hook {
            o.push_str("        let start = me.cur;\n");
        }
        let mut body = self.expr(&rule.body, &mut scope, 2)?;
        // A rule's alternatives need no parentheses of their own.
        if let E::Alt(_) = rule.body {
            body = body[1..body.len() - 1].trim().to_string();
        }
        let _ = writeln!(o, "        let hit = {body};");
        o.push_str(leave);
        if decl {
            let _ = writeln!(
                o,
                "        if hit {{\n            me.decl(Decl::{}, start);\n        }}",
                rule.name
            );
        }
        if hook {
            let _ = writeln!(
                o,
                "        if hit {{\n            me.closed(Hook::{}, start);\n        }}",
                rule.name
            );
        }
        o.push_str("        Ok(hit)\n    }\n");
        Ok(())
    }

    /// A `climb` rule: a method that enters at the loosest level, one that climbs,
    /// and one that reads a token's level. The recursion's measure is the tokens
    /// left, because an operator is consumed before each descent.
    fn climb(
        &self,
        o: &mut String,
        name: &str,
        operand: &E,
        levels: &[Level],
    ) -> Result<(), String> {
        let _ = writeln!(o, "        self.{name}_from(0)\n    }}");
        let mut scope = Vec::new();
        let unary = self.expr(operand, &mut scope, 2)?;
        let miss = self.err(operand, &scope)?;
        let _ = writeln!(
            o,
            "
    fn {name}_from(&mut self, min: usize) -> R {{
        let me = self;
        if !{unary} {{
            return Ok(false);
        }}
        while let Some((level, right)) = Self::{name}_level(me.peek()) {{
            if level < min {{
                break;
            }}
            me.bump();
            if !me.{name}_from(if right {{ level }} else {{ level + 1 }})? {{
                {miss}?;
            }}
        }}
        Ok(true)
    }}

    /// The level of a binary operator, loosest first, and whether it groups to
    /// the right.
    fn {name}_level(tok: &Tok) -> Option<(usize, bool)> {{
        Some(match tok {{"
        );
        for (i, level) in levels.iter().enumerate() {
            for op in &level.ops {
                let tok = self.token(&op.s)?;
                let _ = writeln!(o, "            Tok::{tok} => ({i}, {}),", level.right);
            }
        }
        o.push_str("            _ => return None,\n        })\n    }\n");
        Ok(())
    }

    /// The name of the token `s` spells, or why it has none.
    fn token(&self, s: &str) -> Result<&str, String> {
        (self.g.tokens.iter())
            .find(|t| t.1 == s)
            .map(|t| &*t.0)
            .ok_or_else(|| format!("`{s}` is no token"))
    }

    /// `e` as a Rust expression of type `bool` that leaves the cursor and the marks
    /// where they were when it is false. `?` in it propagates a refusal.
    fn expr(&self, e: &E, scope: &mut Scope, ind: usize) -> Result<String, String> {
        let nl = |ind: usize| format!("\n{}", "    ".repeat(ind + 1));
        Ok(match e {
            E::Lit(l) if l.word && !self.g.reserved.contains(&*l.s) => {
                format!("me.word({:?})", l.s)
            }
            E::Lit(l) => format!("me.eat(&Tok::{})", self.token(&l.s)?),
            E::Split(s) if &**s == ">" => "me.close_gt()".into(),
            E::Ref(r) => self.call(*r)?,
            E::Builtin(Builtin::Eof) => "matches!(me.peek(), Tok::Eof)".into(),
            E::Builtin(Builtin::NoNl) => "me.no_newline()".into(),
            E::Seq(v) if v.is_empty() => "true".into(),
            E::Seq(v) => {
                let depth = scope.len();
                let chain = self.chain(v, true, scope, ind + 1)?;
                scope.truncate(depth);
                format!(
                    "({{{}let p0 = me.save();{}({}){}|| {{ me.back(p0); false }}{}}})",
                    nl(ind),
                    nl(ind),
                    chain,
                    nl(ind),
                    nl(ind - 1)
                )
            }
            E::Alt(v) => {
                let alts = (v.iter())
                    .map(|x| self.expr(x, scope, ind + 1))
                    .collect::<Result<Vec<_>, _>>()?;
                format!(
                    "({}{}{})",
                    nl(ind),
                    alts.join(&format!("{}|| ", nl(ind))),
                    nl(ind - 1)
                )
            }
            E::Star(x) => format!("({{ {} true }})", self.star(x, scope, ind)?),
            E::Plus(x) => format!(
                "({} && ({{ {} true }}))",
                self.expr(x, scope, ind)?,
                self.star(x, scope, ind)?
            ),
            E::Opt(x) => format!("({{ let _ = {}; true }})", self.expr(x, scope, ind)?),
            E::And(x) => format!("me.look(|me| Ok({}))", self.expr(x, scope, ind)?),
            E::Not(x) => format!("!me.look(|me| Ok({}))", self.expr(x, scope, ind)?),
            E::Try(x) => format!("me.attempt(|me| Ok({}))", self.expr(x, scope, ind)?),
            E::Field(_, x) => self.expr(x, scope, ind)?,
            E::Throw(t) => format!("{}?", self.throw(t, scope)?),
            E::Skip(t) => format!("({{ {}; true }})", self.throw_as("me.skip", t, scope)?),
            E::SetFlag(f, v, x) => {
                let flag = &self.g.flags[*f];
                format!(
                    "({{ let old = std::mem::replace(&mut me.flags.{flag}, {v}); let hit = {}; me.flags.{flag} = old; hit }})",
                    self.expr(x, scope, ind)?
                )
            }
            E::TestFlag(f) => format!("me.flags.{}", self.g.flags[*f]),
            E::Many(x, c, Recover::None) => {
                let item = self.expr(x, scope, ind + 1)?;
                let miss = self.err(x, scope)?;
                let until = self.expr(c, scope, ind + 1)?;
                format!(
                    "({{ loop {{ if me.look(|me| Ok({until})) {{ break; }} let p = me.cur; if !{item} {{ {miss}?; }} if me.cur == p {{ break; }} }} true }})"
                )
            }
            E::Many(x, c, rec) => {
                let item = self.expr(x, scope, ind + 1)?;
                let miss = self.err(x, scope)?;
                let until = self.expr(c, scope, ind + 1)?;
                let start = match rec {
                    Recover::Decl(p) => format!(
                        "Some(|me: &mut Self| Ok({}))",
                        self.expr(p, scope, ind + 1)?
                    ),
                    _ => "None::<fn(&mut Self) -> R>".to_string(),
                };
                format!(
                    "me.many_recovering({nl1}|me| Ok({until}),{nl1}|me| Ok({item}),{nl1}|me| {miss},{nl1}{start},{nl0})?",
                    nl1 = nl(ind + 1),
                    nl0 = nl(ind),
                )
            }
            E::List(x, sep, c) => {
                let item = self.expr(x, scope, ind + 1)?;
                let miss = self.err(x, scope)?;
                let until = self.expr(c, scope, ind + 1)?;
                let sep = self.expr(sep, scope, ind + 1)?;
                format!(
                    "({{ loop {{ let p = me.cur; if me.look(|me| Ok({until})) {{ break; }} if !{item} {{ {miss}?; }} if !{sep} || me.cur == p {{ break; }} }} true }})"
                )
            }
            other => {
                return Err(format!(
                    "a form with no token form in a syntactic rule: {other:?}"
                ))
            }
        })
    }

    /// A call of rule `r`: a method for a syntactic rule, the token for a lexical
    /// one.
    fn call(&self, r: RuleId) -> Result<String, String> {
        let rule = self.g.body(r);
        if rule.syntactic {
            let args: Vec<String> = self.free[r.0 as usize]
                .iter()
                .map(|l| format!("l_{l}"))
                .collect();
            return Ok(format!("me.{}({})?", snake(&rule.name), args.join(", ")));
        }
        match &*rule.name {
            "string" => return Ok("me.string(false)?".into()),
            "tagged" => return Ok("me.string(true)?".into()),
            _ => {}
        }
        let (_, tok) = (TERMINALS.iter())
            .find(|t| t.0 == rule.name)
            .ok_or_else(|| format!("lexical rule `{}` names no token", rule.name))?;
        Ok(format!("me.eat_if(matches!(me.peek(), {tok}))"))
    }

    /// The elements of a sequence joined by `&&`. An element that fails after the
    /// sequence moved is a refusal unless a predicate is looking; a label's `let`
    /// opens a block over the rest of the sequence.
    fn chain(&self, v: &[E], first: bool, scope: &mut Scope, ind: usize) -> Result<String, String> {
        let Some((x, rest)) = v.split_first() else {
            return Ok("true".into());
        };
        let inner = match x {
            E::Field(_, y) => &**y,
            _ => x,
        };
        let mut term = self.expr(inner, scope, ind)?;
        if !first && !matches!(inner, E::Star(_) | E::Opt(_)) {
            term = format!(
                "({term} || (me.committed(p0) && {}?))",
                self.err(inner, scope)?
            );
        }
        if rest.is_empty() {
            return Ok(term);
        }
        let nl = format!("\n{}", "    ".repeat(ind + 1));
        if let E::Field(l, _) = x {
            scope.push(l.clone());
            let tail = self.chain(rest, false, scope, ind)?;
            return Ok(format!(
                "({{ let from = me.cur; {term} && ({{ let l_{l} = Capture {{ from, to: me.cur }};{nl}{tail} }}) }})"
            ));
        }
        Ok(format!(
            "{term}{nl}&& {}",
            self.chain(rest, false, scope, ind)?
        ))
    }

    /// `e*`: stops when `e` fails or matches empty.
    fn star(&self, x: &E, scope: &mut Scope, ind: usize) -> Result<String, String> {
        Ok(format!(
            "loop {{ let p = me.cur; if !{} || me.cur == p {{ break; }} }}",
            self.expr(x, scope, ind)?
        ))
    }

    /// The refusal a committed failure of `e` states, as a Rust expression of type
    /// `R`: its rule's `fail` row, its own throw, or the `expect` row for its first
    /// literal. The grammar has no left recursion, so the descent ends.
    fn err(&self, e: &E, scope: &Scope) -> Result<String, String> {
        Ok(match e {
            E::Lit(l) => self.expected(&l.s),
            E::Split(s) => self.expected(s),
            E::Ref(r) => match &self.g.body(*r).fail {
                Some(t) => self.throw(t, scope)?,
                None => self.err(&self.g.body(*r).body, scope)?,
            },
            E::Alt(v) => match v.last() {
                Some(x) => self.err(x, scope)?,
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
                    Some(x) => self.err(x, scope)?,
                    None => self.expected(""),
                }
            }
            E::Field(_, x)
            | E::Plus(x)
            | E::Try(x)
            | E::SetFlag(_, _, x)
            | E::Climb(x, _)
            | E::And(x)
            | E::Guard(_, x) => self.err(x, scope)?,
            E::Throw(t) => self.throw(t, scope)?,
            E::Embed(em) => self.err(&em.extent, scope)?,
            _ => self.expected(""),
        })
    }

    /// The `expect` row for the literal `s`: the token's name, or the spelling
    /// quoted when no token line names it.
    fn expected(&self, s: &str) -> String {
        let name = match self.token(s) {
            Ok(name) => name.to_string(),
            Err(_) => format!("{s:?}"),
        };
        format!("me.expected({name:?})")
    }

    /// A refusal as a Rust expression of type `R`.
    fn throw(&self, t: &Throw, scope: &Scope) -> Result<String, String> {
        let via = if t.past {
            "me.refuse_past"
        } else {
            "me.refuse"
        };
        self.throw_as(via, t, scope)
    }

    /// `t` handed to the host's method `via`, with where it points and how its
    /// holes fill. A label the scope does not hold points here and fills empty, as
    /// a label that never matched does.
    fn throw_as(&self, via: &str, t: &Throw, scope: &Scope) -> Result<String, String> {
        let bound = |l: &Rc<str>| scope.contains(l);
        let mut holes = String::new();
        for (h, src) in &t.holes {
            let v = match src {
                HoleSrc::Text(x) => format!("{x:?}"),
                HoleSrc::Found => "me.found()".to_string(),
                HoleSrc::Token => "token".to_string(),
                HoleSrc::Label(l) if bound(l) => format!("me.text(l_{l})"),
                HoleSrc::Label(_) => "\"\"".to_string(),
                other => return Err(format!("hole source {other:?} in a syntactic refusal")),
            };
            let _ = write!(holes, ", {h} = {v}");
        }
        let line = match t.line {
            At::Label(i) if bound(&t.labels[i]) => format!("me.line_of(l_{})", t.labels[i]),
            _ => "me.here().0".to_string(),
        };
        let col = match t.col {
            At::Label(i) if bound(&t.labels[i]) => format!("me.col_of(l_{})", t.labels[i]),
            _ => "me.here().1".to_string(),
        };
        Ok(format!(
            "{via}(({line}, {col}), |me| rule!({}{holes}))",
            t.row
        ))
    }
}

const PRELUDE: &str = "\
// Written by vyrn_syntax::recognizer from compiler/vyrn-syntax/vyrn.syntax. Do
// not edit: the test the_recognizer_is_fresh fails when this file differs from a
// fresh run, and VYRN_PIN=write rewrites it.

//! The syntactic rules of `vyrn.syntax` as Rust: one method of [`Recognizer`] per
//! rule that `Module` reaches. A method returns `Ok(false)` with the cursor
//! unmoved when its rule does not match, and `Err` when the rule refuses.

use super::{Capture, Recognizer, R};
use crate::lexer::Tok;
use crate::rules::rule;

";

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn the_recognizer_is_fresh() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(dir.join("vyrn.syntax")).expect("vyrn.syntax reads");
        let g = crate::load(&text).expect("vyrn.syntax loads");
        let fresh = super::recognizer(&g).expect("vyrn.syntax's syntactic rules lower");
        let file = dir.join("../vyrn-frontend/src/recognizer/rules.rs");
        if std::env::var("VYRN_PIN").as_deref() == Ok("write") {
            std::fs::create_dir_all(file.parent().expect("the file has a directory"))
                .expect("create the directory");
            std::fs::write(&file, &fresh).expect("write the recognizer");
            return;
        }
        let committed = std::fs::read_to_string(&file).unwrap_or_default();
        let first = (committed.lines().zip(fresh.lines()).enumerate())
            .find(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| format!("line {}:\n  committed {a}\n  fresh     {b}", i + 1));
        assert!(
            committed == fresh,
            "recognizer/rules.rs differs from vyrn.syntax; rewrite it with VYRN_PIN=write\n{}",
            first.unwrap_or_else(|| "the files differ in length".into())
        );
    }
}
