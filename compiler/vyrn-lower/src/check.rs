//! Every runtime check as a core row (obligation O1 of the check-elision
//! research).
//!
//! [`state`] inserts a [`St::Check`] before each row whose operation can trap:
//! an element place on an array or a String, an integer `/`, `%`, `<<` or
//! `>>`, `@swapRemove`, a SIMD span load or store, and `bytes(s, a, b)`. The
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
use vyrn_frontend::trap::Rule;

use crate::core::{Arg, Body, Op, Place, Rhs, St, Val};

/// One runtime check: the trap it raises, what it compares, where, and
/// whether the emitter runs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub rule: Rule,
    pub guard: Guard,
    pub site: Site,
    pub verdict: Verdict,
}

/// What a pass decided about a check. [`state`] states every row
/// [`Verdict::Kept`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The emitter runs the check.
    Kept,
    /// A pass proved the check cannot fail, and the emitter runs nothing. A
    /// proved [`Guard::Range`] is refused: `std/runtime`'s `bytesOf` checks
    /// its range inside and has no unchecked form.
    Proved,
}

/// What a check compares. The check traps when the condition fails.
#[derive(Debug, Clone, PartialEq)]
pub enum Guard {
    /// `0 <= i < length(base)`.
    Index(Place, Val),
    /// `0 <= i` and `i + span <= length(base)`: a SIMD load or store of
    /// `span` elements.
    Span(Place, Val, i64),
    /// `0 <= from <= to <= length(s)`: `bytes(s, from, to)`, checked by
    /// `std/runtime`'s `bytesOf`.
    Range(Val, Val, Val),
    /// The divisor is not zero.
    NonZero(Val),
    /// Not the minimum of a signed width over `-1`: the dividend, the
    /// divisor, and the width in bits.
    NoOverflow(Val, Val, u8),
    /// `0 <= k < bits`: a shift amount.
    Shift(Val, u8),
}

/// Where a check stands in the source: the line of the row it guards and the
/// check's position among the checks of that line in its body, from 0.
///
/// The pair names the same check in every compile of the same source, and
/// with the body's file and name it names one check in the program. The AST
/// states no column for an operator or a call, so the ordinal stands in for
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Site {
    pub line: usize,
    pub ordinal: u32,
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
        let line = match &s {
            St::Let(n, rhs) => {
                rhs_guards(body, tys, rhs, &mut guards);
                if let Rhs::Take(p) = rhs {
                    taken.push(p.clone());
                }
                body.names[*n as usize].line
            }
            St::Do { rhs, line, .. } => {
                rhs_guards(body, tys, rhs, &mut guards);
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
    let span = match callee {
        "@f32x4Load" | "@f32x4Store" | "@i32x4Load" | "@i32x4Store" => Some(4),
        "@f64x2Load" | "@f64x2Store" => Some(2),
        _ => None,
    };
    let stores = callee.ends_with("Store");
    match (callee, args) {
        ("@swapRemove", [(r, _), (i, _)]) => {
            if let (Some(p), Some(i)) = (base(r), val(i)) {
                out.push((Rule::ArrayIndex, Guard::Index(p, i)));
            }
        }
        ("bytes", [(s, _), (a, _), (b, _)]) => {
            if let (Some(s), Some(a), Some(b)) = (val(s), val(a), val(b)) {
                out.push((Rule::StringIndex, Guard::Range(s, a, b)));
            }
        }
        (_, [(r, _), (i, _), rest @ ..]) if span.is_some() && rest.len() == usize::from(stores) => {
            if let (Some(p), Some(i), Some(n)) = (base(r), val(i), span) {
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
    match op {
        BinOp::Div => {
            out.push((Rule::DivZero, Guard::NonZero(r.clone())));
            if signed {
                out.push((
                    Rule::DivOverflow,
                    Guard::NoOverflow(l.clone(), r.clone(), bits),
                ));
            }
        }
        BinOp::Rem => out.push((Rule::RemZero, Guard::NonZero(r.clone()))),
        BinOp::Shl | BinOp::Shr => out.push((Rule::ShiftRange, Guard::Shift(r.clone(), bits))),
        _ => {}
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
                Some(Type::Array(_) | Type::ArrayN(..) | Type::SmallArray(..)) => Rule::ArrayIndex,
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
        Place::Name(n) => body.names[*n as usize].ty.clone(),
        Place::Global(g) => (tys.global)(g)?,
        Place::Field(b, f) => match place_ty(body, tys, b)? {
            Type::Record(fs) => fs.into_iter().find(|x| &x.name == f)?.ty,
            _ => return None,
        },
        Place::Elem(b, _) => match place_ty(body, tys, b)? {
            Type::Array(e) | Type::ArrayN(e, _) | Type::SmallArray(e, _) | Type::Stream(e) => *e,
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
