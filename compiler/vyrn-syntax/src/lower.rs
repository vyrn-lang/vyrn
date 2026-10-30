//! What the writers of a grammar's rules share: which labels a rule reads from its
//! callers, and which refusal a committed failure states.

use std::collections::BTreeSet;
use std::rc::Rc;

use crate::{At, Builtin, Grammar, HoleSrc, Recover, RuleId, Throw, E};

/// The labels each of `rules` reads that it does not bind, by rule index: its
/// parameters. A rule outside `rules` reads none. The sets only grow and the
/// grammar's labels bound them, so the iteration to the least fixed point ends.
pub(crate) fn free_labels(g: &Grammar, rules: &[RuleId]) -> Vec<BTreeSet<Rc<str>>> {
    let mut free = vec![BTreeSet::new(); g.rules.len()];
    loop {
        let mut moved = false;
        for &r in rules {
            let mut read = BTreeSet::new();
            reads(&free, &g.body(r).body, &mut Vec::new(), &mut read);
            let i = r.0 as usize;
            moved |= read != free[i];
            free[i] = read;
        }
        if !moved {
            return free;
        }
    }
}

/// Adds to `out` each label `e` reads that `bound` does not hold. A label binds the
/// rest of the sequence it sits in.
fn reads(free: &[BTreeSet<Rc<str>>], e: &E, bound: &mut Vec<Rc<str>>, out: &mut BTreeSet<Rc<str>>) {
    match e {
        E::Seq(v) => {
            let depth = bound.len();
            for x in v {
                reads(free, x, bound, out);
                if let E::Field(l, _) = x {
                    bound.push(l.clone());
                }
            }
            bound.truncate(depth);
        }
        E::Alt(v) => v.iter().for_each(|x| reads(free, x, bound, out)),
        E::Star(x) | E::Plus(x) | E::Opt(x) | E::And(x) | E::Not(x) | E::Try(x) => {
            reads(free, x, bound, out)
        }
        E::Field(_, x) | E::SetFlag(_, _, x) | E::Climb(x, _) | E::Guard(_, x) => {
            reads(free, x, bound, out)
        }
        E::Many(a, b, rec) => {
            reads(free, a, bound, out);
            reads(free, b, bound, out);
            if let Recover::Decl(c) = rec {
                reads(free, c, bound, out);
            }
        }
        E::List(a, b, c) => [a, b, c].iter().for_each(|x| reads(free, x, bound, out)),
        E::Embed(em) => {
            reads(free, &em.extent, bound, out);
            for t in [&em.wrap, &em.trailing].into_iter().flatten() {
                throw_reads(t, bound, out);
            }
        }
        E::Ref(r) => free[r.0 as usize]
            .iter()
            .filter(|l| !bound.contains(l))
            .for_each(|l| {
                out.insert(l.clone());
            }),
        E::Throw(t) | E::Skip(t) => throw_reads(t, bound, out),
        _ => {}
    }
}

fn throw_reads(t: &Throw, bound: &[Rc<str>], out: &mut BTreeSet<Rc<str>>) {
    let mut read = |l: &Rc<str>| {
        if !bound.contains(l) {
            out.insert(l.clone());
        }
    };
    for at in [t.line, t.col] {
        if let At::Label(i) = at {
            read(&t.labels[i]);
        }
    }
    for (_, src) in &t.holes {
        if let HoleSrc::Label(l) = src {
            read(l);
        }
    }
}

/// What a committed failure of an expression states.
pub(crate) enum Miss<'g> {
    /// The `expect` row, with this token's name or quoted spelling.
    Expected(String),
    Row(&'g Throw),
}

/// The refusal a committed failure of `e` states: its rule's `fail` row, its own
/// throw, or the `expect` row for its first literal. The grammar has no left
/// recursion, so the descent ends.
pub(crate) fn miss<'g>(g: &'g Grammar, e: &'g E) -> Miss<'g> {
    match e {
        E::Lit(l) => expected(g, &l.s),
        E::Split(s) => expected(g, s),
        E::Ref(r) => match &g.body(*r).fail {
            Some(t) => Miss::Row(t),
            None => miss(g, &g.body(*r).body),
        },
        E::Alt(v) => match v.last() {
            Some(x) => miss(g, x),
            None => expected(g, ""),
        },
        E::Seq(v) => {
            let required = v.iter().find(|x| {
                !matches!(
                    x,
                    E::Star(_) | E::Opt(_) | E::Not(_) | E::TestFlag(_) | E::Builtin(Builtin::NoNl)
                )
            });
            match required {
                Some(x) => miss(g, x),
                None => expected(g, ""),
            }
        }
        E::Field(_, x)
        | E::Plus(x)
        | E::Try(x)
        | E::SetFlag(_, _, x)
        | E::Climb(x, _)
        | E::And(x)
        | E::Guard(_, x) => miss(g, x),
        E::Throw(t) => Miss::Row(t),
        E::Embed(em) => miss(g, &em.extent),
        _ => expected(g, ""),
    }
}

/// The `expect` row for the literal `s`: the token's name, or the spelling quoted
/// when no token line names it.
fn expected<'g>(g: &'g Grammar, s: &str) -> Miss<'g> {
    Miss::Expected(match g.tokens.iter().find(|t| t.1 == s) {
        Some(t) => t.0.clone(),
        None => format!("{s:?}"),
    })
}
