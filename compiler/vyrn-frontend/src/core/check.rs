//! The runtime checks a row can carry ([`super::St::Check`]).

use super::{Lit, Name, Place, Val};
use crate::ast::{BinOp, Capability, Expr, FnId, Function, Type, UnOp};
use crate::prim::Cmp;
use crate::rules::rule;
use crate::trap::Rule;
use crate::types::Decls;

/// One runtime check: the trap it raises, what it compares, where, and
/// whether the emitter runs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub rule: Raises,
    pub guard: Guard,
    pub site: Site,
    pub verdict: Verdict,
    /// Why a [`Verdict::Kept`] row stays, written with the verdict
    /// (`vyrn_lower::elide`); [`Why::Unsaid`] for a proved row.
    pub why: Why,
}

/// The reason a pass could not prove a check, for `vyrn why --cost` and the
/// editor. Each variant names the name whose origin decided it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Why {
    /// No pass gave a reason: the row is proved, or no pass ran.
    #[default]
    Unsaid,
    /// The name's value comes from outside the program.
    Input(Name),
    /// The goal needs a fact about a call's result: the name it binds and
    /// the function, when the call is to a declared one.
    Callee(Name, Option<FnId>),
    /// The goal needs a fact about a parameter: the parameter, and the goal
    /// over source names.
    Caller(Name, std::sync::Arc<str>),
    /// A kind of gap the research names by its move number: 2 a length a
    /// call may have changed, 3 a field, element or global read, 5 a goal
    /// over one counter and names the loop leaves alone.
    Move(u8, Option<Name>),
    /// No rule applies; the text is the goal the prover could not show.
    Unproved(std::sync::Arc<str>),
}

impl Why {
    /// The reason in two words, for the editor's end-of-line hint.
    pub fn short(&self) -> &'static str {
        match self {
            Why::Unsaid => "",
            Why::Input(_) => "input",
            Why::Callee(..) => "callee fact",
            Why::Caller(..) => "caller fact",
            Why::Move(2, _) => "move 2",
            Why::Move(3, _) => "move 3",
            Why::Move(..) => "move 5",
            Why::Unproved(_) => "unproved",
        }
    }
}

/// What a failed check raises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Raises {
    /// A row of the trap table.
    Row(Rule),
    /// The `where` failure of the checked record's type, which its
    /// constructor raises ([`crate::trap::validation`]).
    Where,
}

impl Raises {
    /// The census name, for a diagnostic and the records.
    pub fn census(self) -> &'static str {
        match self {
            Raises::Row(r) => r.census(),
            Raises::Where => "where",
        }
    }
}

/// What a pass decided about a check. `vyrn_lower::check::state` states every row
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
    /// The record name satisfies its type's `where` rule: the row after the
    /// check calls the type's constructor on it.
    Rule(Name),
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

/// One comparison a parameter's `where` clause states, `l cmp r`, both
/// operands of the integer type `ty`.
#[derive(Debug, Clone, PartialEq)]
pub struct Atom {
    pub l: Operand,
    pub cmp: Cmp,
    pub r: Operand,
    pub ty: Type,
}

/// An operand of an [`Atom`]: the value `of`, or the `part` of it a read
/// gives: `length`, `byteLength` or an integer field.
#[derive(Debug, Clone, PartialEq)]
pub struct Operand {
    pub of: Val,
    pub part: Option<String>,
}

impl Atom {
    /// The atom with each `Val::Name(Name(k))` of [`clauses`] replaced by
    /// `by[k]`: a call's argument or a body's parameter. `None` when `by`
    /// has no entry `k`.
    pub fn with(&self, by: &[Val]) -> Option<Atom> {
        let put = |o: &Operand| {
            let of = match &o.of {
                Val::Name(k) => by.get(k.index())?.clone(),
                lit => lit.clone(),
            };
            Some(Operand {
                of,
                part: o.part.clone(),
            })
        };
        Some(Atom {
            l: put(&self.l)?,
            cmp: self.cmp,
            r: put(&self.r)?,
            ty: self.ty.clone(),
        })
    }
}

/// The comparisons the `where` clause of each parameter of `f` states, by
/// parameter index, over `Val::Name(Name(k))` for parameter `k`
/// ([`Atom::with`]). The clause holds for the whole call: it reads only
/// `read` parameters, which the body cannot assign and no write reaches
/// while the call runs. The error is the line and the rule the checker
/// refuses the first bad clause with.
pub fn clauses(
    f: &Function,
    decls: &dyn Decls,
) -> Result<Vec<(usize, Vec<Atom>)>, (usize, crate::rules::Rule)> {
    let mut out = Vec::new();
    for (k, p) in f.params.iter().enumerate() {
        let Some(e) = &p.clause else { continue };
        let name = crate::rules::DeclName(&f.name);
        let entry = |why: &str| (p.line, rule!(ClauseEntry, name, why));
        if f.is_gen || f.is_export_extern {
            return Err(entry("the host calls it by name and checks no clause"));
        }
        let fn_typed =
            |q: &crate::ast::Param| matches!(crate::types::resolve(&q.ty, decls), Type::Fn(..));
        if f.params.iter().any(fn_typed) {
            return Err(entry(
                "it takes a `fn` parameter, which a call binds apart from its arguments",
            ));
        }
        if p.capability != Capability::Read {
            let (param, cap) = (p.name.as_str(), p.capability.word());
            return Err((p.line, rule!(ClauseChanges, param, name = param, cap)));
        }
        let mut atoms = Vec::new();
        conjuncts(f, k, e, decls, &mut atoms).map_err(|r| (p.line, r))?;
        out.push((k, atoms));
    }
    Ok(out)
}

/// Appends the comparisons of `e`, the clause of parameter `k` of `f` or one
/// of its `&&` operands.
fn conjuncts(
    f: &Function,
    k: usize,
    e: &Expr,
    decls: &dyn Decls,
    out: &mut Vec<Atom>,
) -> Result<(), crate::rules::Rule> {
    let form = || rule!(ClauseForm, param = f.params[k].name.as_str());
    let Expr::Binary { op, lhs, rhs, .. } = e else {
        return Err(form());
    };
    if *op == BinOp::And {
        conjuncts(f, k, lhs, decls, out)?;
        return conjuncts(f, k, rhs, decls, out);
    }
    let cmp = op.compare().ok_or_else(form)?;
    let (l, lt) = operand(f, k, lhs, decls)?;
    let (r, rt) = operand(f, k, rhs, decls)?;
    let ty = match (lt, rt) {
        (Some(a), Some(b)) if a == b => a,
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => Type::Int,
        _ => return Err(form()),
    };
    out.push(Atom { l, cmp, r, ty });
    Ok(())
}

/// One side of a comparison in the clause of parameter `k` of `f`, with its
/// integer type; `None` for a literal, which takes the other side's.
fn operand(
    f: &Function,
    k: usize,
    e: &Expr,
    decls: &dyn Decls,
) -> Result<(Operand, Option<Type>), crate::rules::Rule> {
    let form = || rule!(ClauseForm, param = f.params[k].name.as_str());
    let lit = |n: Option<i64>| {
        let of = Val::Lit(Lit::Int(n.ok_or_else(form)?));
        Ok((Operand { of, part: None }, None))
    };
    let integer = |t: Type| match crate::validate::width(&t) {
        Some(_) => Ok(t),
        None => Err(form()),
    };
    let (name, part) = match e {
        Expr::Int(n, _) => return lit(Some(*n)),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
            ..
        } => match &**expr {
            Expr::Int(n, _) => return lit(n.checked_neg()),
            _ => return Err(form()),
        },
        Expr::Var { name, .. } => (name, None),
        Expr::Field { expr, field, .. } => match &**expr {
            Expr::Var { name, .. } => (name, Some(field)),
            _ => return Err(form()),
        },
        _ => return Err(form()),
    };
    let j = sibling(f, k, name)?;
    let base = crate::types::resolve(&f.params[j].ty, decls);
    let ty = match (&base, part.map(String::as_str)) {
        (_, None) => integer(base)?,
        (t, Some("length")) if t.is_seq() => Type::Int,
        (Type::Str, Some("byteLength")) => Type::Int,
        (_, Some(field)) => {
            let fs = crate::types::record_fields(&base, decls).ok_or_else(form)?;
            let t = &fs.iter().find(|x| x.name == field).ok_or_else(form)?.ty;
            integer(crate::types::resolve(t, decls))?
        }
    };
    let (of, part) = (Val::Name(Name(j as u32)), part.cloned());
    Ok((Operand { of, part }, Some(ty)))
}

/// The parameter `name` names in the clause of parameter `k` of `f`: `value`
/// is `k` itself, any other name an earlier `read` parameter.
fn sibling(f: &Function, k: usize, name: &str) -> Result<usize, crate::rules::Rule> {
    if name == "value" {
        return Ok(k);
    }
    let param = f.params[k].name.as_str();
    match f.params[..k].iter().position(|p| p.name == name) {
        None => Err(rule!(ClauseLater, param, name)),
        Some(j) if f.params[j].capability != Capability::Read => {
            let cap = f.params[j].capability.word();
            Err(rule!(ClauseChanges, param, name, cap))
        }
        Some(j) => Ok(j),
    }
}
