//! Every runtime check as a core row (obligation O1 of the check-elision
//! research).
//!
//! [`state`] inserts a [`St::Check`] before each row whose operation can trap:
//! an element place on an array or a String, an integer `/`, `%`, `<<` or
//! `>>`, `@swapRemove`, a SIMD span load or store, `bytes(s, a, b)`, and a
//! record type's constructor run on a name to check its `where` rule. The
//! emitter emits each check from its row and from nowhere else. A check row
//! emits nothing where it stands: the emitter runs it inside the row it
//! guards, where the operands are on hand, so it reads no name and no walker
//! counts it.
//!
//! One runtime check is one row. A store that puts a taken element back into
//! the place it was taken from (`a[i].push(v)`: `take a[i]`, the call, the
//! store) follows the take's row and states none of its own.

use std::collections::{BTreeMap, HashMap};

use vyrn_frontend::ast::{BinOp, Type, TypeDecl};
use vyrn_frontend::prelude::Indexes;
use vyrn_frontend::trap::Rule;

use vyrn_frontend::core::check::{Check, Guard, Raises, Site, Verdict, Why};
use vyrn_frontend::core::{Arg, Body, Op, Place, Rhs, St, Val};

/// What a build does with its check rows, from the environment variable
/// `VYRN_CHECKS`, which this function alone reads.
///
/// Unset, a pass decides the rows and the emitter removes the proved ones.
/// `keep` runs no pass, so every row stays [`Verdict::Kept`]: the build a
/// differential run compares against. A path is the oracle: the rows are
/// decided and removed as unset does, and the emitter also counts each row's
/// executions and fails the run where a proved row would have trapped. The
/// wasm host appends the counts to that file, one row per line.
pub fn mode() -> &'static Mode {
    static MODE: std::sync::OnceLock<Mode> = std::sync::OnceLock::new();
    MODE.get_or_init(|| match std::env::var_os("VYRN_CHECKS") {
        None => Mode::Elide,
        Some(v) if v == "keep" => Mode::Keep,
        Some(v) => Mode::Count(v.into()),
    })
}

/// See [`mode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Elide,
    Keep,
    Count(std::path::PathBuf),
}

/// What [`state`] reads besides the body: the program's type declarations and
/// the declared type of each module-state name.
pub struct Types<'a> {
    pub decls: &'a HashMap<String, TypeDecl>,
    pub global: &'a dyn Fn(&str) -> Option<Type>,
}

/// Inserts the check rows of `body` and of every lambda body it holds.
pub fn state(body: &mut Body, tys: &Types<'_>) {
    let mut stmts = std::mem::take(&mut body.stmts);
    let mut lines = BTreeMap::new();
    list(body, tys, &mut stmts, &mut lines);
    body.stmts = stmts;
    for l in &mut body.lambdas {
        state(l, tys);
    }
}

fn list(body: &Body, tys: &Types<'_>, ss: &mut Vec<St>, lines: &mut BTreeMap<usize, u32>) {
    let mut out = Vec::with_capacity(ss.len());
    let mut taken: Vec<Place> = Vec::new();
    for mut s in std::mem::take(ss) {
        let mut guards = Vec::new();
        let mut ruled = None;
        let line = match &s {
            St::Let(n, rhs) => {
                rhs_guards(body, tys, rhs, &mut guards);
                if let Rhs::Take(p) = rhs {
                    taken.push(p.clone());
                }
                body.names[n.index()].line
            }
            St::Do { rhs, line, .. } => {
                rhs_guards(body, tys, rhs, &mut guards);
                ruled = rhs.checks_rule(&body.names);
                *line
            }
            St::Store { place, line, .. } => {
                if !taken.contains(place) {
                    place_guards(body, tys, place, &mut guards);
                }
                *line
            }
            _ => 0,
        };
        let guards = (guards.into_iter())
            .map(|(r, g)| (Raises::Row(r), g))
            .chain(ruled.map(|n| (Raises::Where, Guard::Rule(n))));
        for (rule, guard) in guards {
            let ordinal = lines.entry(line).or_insert(0);
            let site = Site {
                line,
                ordinal: *ordinal,
            };
            *ordinal += 1;
            out.push(St::Check(Check {
                rule,
                guard,
                site,
                verdict: Verdict::Kept,
                why: Why::Unsaid,
            }));
        }
        match &mut s {
            St::If { then, els, .. } => {
                list(body, tys, then, lines);
                list(body, tys, els, lines);
            }
            St::Loop { body: b, .. } | St::Block { body: b, .. } => list(body, tys, b, lines),
            St::Switch { arms, .. } => {
                for a in arms {
                    list(body, tys, &mut a.body, lines);
                }
            }
            _ => {}
        }
        out.push(s);
    }
    *ss = out;
}

fn rhs_guards(body: &Body, tys: &Types<'_>, rhs: &Rhs, out: &mut Vec<(Rule, Guard)>) {
    match rhs {
        Rhs::Read(p) | Rhs::Take(p) => place_guards(body, tys, p, out),
        Rhs::Call { callee, args, .. } => {
            for (a, _) in args {
                if let Arg::Place(p) = a {
                    place_guards(body, tys, p, out);
                }
            }
            call_guards(callee, args, out);
        }
        Rhs::Prim(Op::Bin(op), vs, ty) => {
            if let ([l, r], Some(ty)) = (vs.as_slice(), ty) {
                prim_guards(
                    *op,
                    l,
                    r,
                    &vyrn_frontend::types::resolve(ty, tys.decls),
                    out,
                );
            }
        }
        Rhs::Val(_) | Rhs::Prim(..) | Rhs::Make(..) => {}
    }
}

/// The checks of a builtin that indexes its receiver.
fn call_guards(
    callee: &str,
    args: &[(Arg, vyrn_frontend::ast::Capability)],
    out: &mut Vec<(Rule, Guard)>,
) {
    let base = |a: &Arg| match a {
        Arg::Place(p) => Some(p.clone()),
        Arg::Val(Val::Name(n)) => Some(Place::Name(*n)),
        Arg::Val(Val::Lit(_)) => None,
    };
    let val = |a: &Arg| a.val().cloned();
    let Some(indexes) = vyrn_frontend::prelude::builtin(callee).and_then(|b| b.indexes) else {
        return;
    };
    match (indexes, args) {
        (Indexes::Element, [(r, _), (i, _)]) => {
            if let (Some(p), Some(i)) = (base(r), val(i)) {
                out.push((Rule::ArrayIndex, Guard::Index(p, i)));
            }
        }
        (Indexes::Bytes, [(s, _), (a, _), (b, _)]) => {
            if let (Some(s), Some(a), Some(b)) = (val(s), val(a), val(b)) {
                out.push((Rule::StringIndex, Guard::Range(s, a, b)));
            }
        }
        (Indexes::Lanes(n), [(r, _), (i, _), ..]) => {
            if let (Some(p), Some(i)) = (base(r), val(i)) {
                out.push((Rule::ArrayIndex, Guard::Span(p, i, n)));
            }
        }
        _ => {}
    }
}

/// The checks of an integer operator at the width `ty`, its result.
fn prim_guards(op: BinOp, l: &Val, r: &Val, ty: &Type, out: &mut Vec<(Rule, Guard)>) {
    let Some((bits, signed)) = vyrn_frontend::validate::width(ty) else {
        return;
    };
    for &rule in op.row().traps {
        let guard = match rule {
            Rule::DivZero | Rule::RemZero => Guard::NonZero(r.clone()),
            // Only a signed type has a minimum whose negation overflows.
            Rule::DivOverflow if signed => Guard::NoOverflow(l.clone(), r.clone(), bits),
            Rule::ShiftRange => Guard::Shift(r.clone(), bits),
            _ => continue,
        };
        out.push((rule, guard));
    }
}

/// The index checks along `p`, outermost first. An element of a stream is
/// the element a pull wrote, and a map's entry is a lookup: neither checks.
fn place_guards(body: &Body, tys: &Types<'_>, p: &Place, out: &mut Vec<(Rule, Guard)>) {
    match p {
        Place::Name(_) | Place::Global(_) => {}
        Place::Field(b, _) | Place::Key(b, _) => place_guards(body, tys, b, out),
        Place::Elem(b, i) => {
            place_guards(body, tys, b, out);
            let rule = match place_ty(body, tys, b) {
                Some(t) if t.is_seq() => Rule::ArrayIndex,
                Some(Type::Str) => Rule::StringIndex,
                _ => return,
            };
            out.push((rule, Guard::Index((**b).clone(), i.clone())));
        }
    }
}

/// The type of the value at `p`, resolved through its declarations.
fn place_ty(body: &Body, tys: &Types<'_>, p: &Place) -> Option<Type> {
    let resolve = |t: &Type| vyrn_frontend::types::resolve(t, tys.decls);
    Some(resolve(&match p {
        Place::Name(n) => body.names[n.index()].ty.clone(),
        Place::Global(g) => (tys.global)(g)?,
        Place::Field(b, f) => match place_ty(body, tys, b)? {
            Type::Record(fs) => fs.into_iter().find(|x| &x.name == f)?.ty,
            _ => return None,
        },
        Place::Elem(b, _) => match place_ty(body, tys, b)? {
            t if t.is_seq() => t.elem()?.clone(),
            Type::Stream(e) => *e,
            Type::Str => Type::IntN {
                bits: 8,
                signed: false,
            },
            _ => return None,
        },
        Place::Key(b, _) => match place_ty(body, tys, b)? {
            Type::Map(_, v) => *v,
            _ => return None,
        },
    }))
}
