//! The runtime checks a row can carry ([`super::St::Check`]).

use std::sync::Arc;

use super::{Arg, Callee, Lit, Name, Place, Rhs, Val};
use crate::ast::{BinOp, Capability, Expr, FnId, Function, Type, TypeDecl, UnOp};
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
/// editor. A goal is what the check needs, in source form (`` `xs` non-empty ``,
/// `` `i < xs.length` ``), as `vyrn_lower::elide` writes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Why {
    /// No pass gave a reason: the row is proved, or no pass ran.
    #[default]
    Unsaid,
    /// The name's value comes from outside the program.
    Input(Name),
    /// The check needs a fact about a call's result: the name it binds, the
    /// function when the call is to a declared one, and the goal.
    Callee(Name, Option<FnId>, Option<Arc<str>>),
    /// The check needs a fact about a parameter: the parameter, the goal, and
    /// the goal as the parameter's `where` clause when the clause grammar can
    /// state it.
    Caller(Name, Arc<str>, Option<Arc<str>>),
    /// A call in the loop may change the name's length.
    Resized(Name),
    /// The name, or the array the check reads, is read from a field, an
    /// element or module state, which the prover keeps no facts about.
    Place(Option<Name>),
    /// The loop moves only this counter, and the goal is over it and names the
    /// loop leaves alone.
    Counter(Name, Option<Arc<str>>),
    /// No rule applies: the goal the prover could not show.
    Unproved(Arc<str>),
    /// A record `where` rule other than equal lengths.
    Rule,
    /// A guard with no linear form.
    Nonlinear,
}

/// What a failed check raises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Raises {
    /// A row of the trap table.
    Row(Rule),
    /// The `where` failure of the checked record's type, which its
    /// constructor raises ([`crate::trap::validation`]).
    Where,
    /// The failure of a callee's parameter `where` clause
    /// ([`crate::trap::clause`]), which [`Guard::Clause`] carries.
    Clause,
    /// The failure of the function's own result `where` clause
    /// ([`crate::trap::result_clause`]), which [`Guard::Clause`] carries.
    Result,
}

impl Raises {
    /// The census name, for a diagnostic and the records.
    pub fn census(self) -> &'static str {
        match self {
            Raises::Row(r) => r.census(),
            Raises::Where => "where",
            Raises::Clause => "where-arg",
            Raises::Result => "where-result",
        }
    }
}

/// What a pass decided about a check. `vyrn_lower::check::state` states every row
/// [`Verdict::Kept`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The emitter runs the check.
    Kept,
    /// A pass proved the check cannot fail, and the emitter runs nothing.
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
    /// `0 <= from <= to <= length(s)`: `bytes(s, from, to)`.
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
    /// The name satisfies the `where` rule of the validated integer type
    /// named: the row after the check constructs the type from it
    /// ([`scalar_arg`]).
    Scalar(Name, String),
    /// The arguments of the call row after the check satisfy one parameter's
    /// `where` clause, or the value of the `return` after it satisfies the
    /// function's result clause: each comparison holds. `says` is the
    /// sentence a failure prints. Unlike every other check, it runs where it
    /// stands: its operands are names and literals the row after it reads.
    Clause {
        atoms: Vec<Atom>,
        says: std::sync::Arc<str>,
    },
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

/// A function's result `where` clause: a guarantee every return checks and
/// every direct caller assumes. A parameter's clause is a requirement, the
/// other way round, with its own form ([`clauses`]), so neither reads as the
/// other.
#[derive(Debug, Clone, PartialEq)]
pub struct Ensures {
    /// The comparisons, over `Name(0)` for the result and `Name(k + 1)` for
    /// parameter `k`; read only through [`Ensures::over`].
    atoms: Vec<Atom>,
    /// The sentence a failure prints ([`crate::trap::result_clause`]).
    pub says: Arc<str>,
    /// The clause's line, where the check of a body that falls off its end
    /// stands.
    pub line: usize,
}

/// One comparison a parameter's `where` clause states, `l op r`: `op` is a
/// comparison operator, and both operands are of the integer type `ty`.
#[derive(Debug, Clone, PartialEq)]
pub struct Atom {
    pub l: Operand,
    pub op: BinOp,
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
    /// `by(k)`: a call's argument or a body's parameter. `None` when `by`
    /// has none.
    pub fn with(&self, by: &dyn Fn(usize) -> Option<Val>) -> Option<Atom> {
        let put = |o: &Operand| {
            let of = match &o.of {
                Val::Name(k) => by(k.index())?,
                lit => lit.clone(),
            };
            Some(Operand {
                of,
                part: o.part.clone(),
            })
        };
        Some(Atom {
            l: put(&self.l)?,
            op: self.op,
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
            |q: &crate::ast::Param| matches!(&*crate::types::resolved(&q.ty, decls), Type::Fn(..));
        if f.params.iter().any(fn_typed) {
            return Err(entry(
                "it takes a `fn` parameter, which a call binds apart from its arguments",
            ));
        }
        let mut atoms = Vec::new();
        conjuncts(f, Of::Param(k), e, decls, &mut atoms, &mut Vec::new())
            .map_err(|r| (p.line, r))?;
        // A fact holds of the argument the call proved it of, whatever the
        // body does with it; a comparison must hold for the whole call.
        if atoms.is_empty() {
            continue;
        }
        if p.capability != Capability::Read {
            let (param, cap) = (p.name.as_str(), p.capability.word());
            return Err((p.line, rule!(ClauseChanges, param, name = param, cap)));
        }
        out.push((k, atoms));
    }
    Ok(out)
}

/// Which fact a fact clause states of a sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Word {
    /// `sorted<O>(value)`: no value is before the one ahead of it under `O`.
    Sorted,
    /// `strict<O>(value)`: each value is before the next under `O`.
    Strict,
}

impl Word {
    /// The fact word a clause call spells, if its name is one.
    pub(crate) fn of(name: &str) -> Option<Word> {
        match name {
            "sorted" => Some(Word::Sorted),
            "strict" => Some(Word::Strict),
            _ => None,
        }
    }

    pub fn spelled(self) -> &'static str {
        match self {
            Word::Sorted => "sorted",
            Word::Strict => "strict",
        }
    }
}

/// A fact of a sequence under an order: the order as the declaration writes
/// it, a [`Type::Order`] or a type parameter bound by `Order`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Fact {
    pub word: Word,
    pub order: Type,
}

/// What a parameter's fact clause requires of the argument. Every call
/// proves it; nothing checks it at run time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub param: usize,
    pub fact: Fact,
}

/// What a result's fact clause guarantees of the result. The body
/// establishes it at every return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guarantee(pub Fact);

/// The facts the parameters' clauses of `f` require, in parameter order.
/// The error is the line and the rule the checker refuses the first bad
/// fact with.
pub fn requirements(
    f: &Function,
    decls: &dyn Decls,
    orders: &crate::order::Names,
) -> Result<Vec<Requirement>, (usize, crate::rules::Rule)> {
    let mut out = Vec::new();
    for (k, p) in f.params.iter().enumerate() {
        let Some(e) = &p.clause else { continue };
        for fact in facts(f, Of::Param(k), e, decls, orders).map_err(|r| (p.line, r))? {
            out.push(Requirement { param: k, fact });
        }
    }
    Ok(out)
}

/// The facts the result clause of `f` guarantees. The error is as
/// [`requirements`]'s.
pub fn guarantees(
    f: &Function,
    decls: &dyn Decls,
    orders: &crate::order::Names,
) -> Result<Vec<Guarantee>, (usize, crate::rules::Rule)> {
    let Some(e) = &f.ret_clause else {
        return Ok(Vec::new());
    };
    let line = e.line().max(f.line);
    let facts = facts(f, Of::Result, e, decls, orders).map_err(|r| (line, r))?;
    Ok(facts.into_iter().map(Guarantee).collect())
}

/// The facts the clause `e` of `of` states, each of an array of its order's
/// element type.
fn facts(
    f: &Function,
    of: Of,
    e: &Expr,
    decls: &dyn Decls,
    orders: &crate::order::Names,
) -> Result<Vec<Fact>, crate::rules::Rule> {
    let mut calls = Vec::new();
    conjuncts(f, of, e, decls, &mut Vec::new(), &mut calls)?;
    let subject = match of {
        Of::Param(k) => &f.params[k].ty,
        Of::Result => &f.ret,
    };
    let bound = orders.name(crate::order::Known::Order);
    let resolve = |t: &Type| crate::types::resolve(t, decls);
    (calls.into_iter())
        .map(|(word, order)| {
            let elem = match &order {
                Type::Order(o) => Some(o.elem().clone()),
                Type::Param(p) => (f.type_bounds.get(p).into_iter().flatten())
                    .any(|b| Some(b.as_str()) == bound)
                    .then(|| Type::Elem(Box::new(order.clone()))),
                _ => None,
            };
            let elem =
                elem.ok_or_else(|| rule!(FactOrder, word = word.spelled(), ty = order.clone()))?;
            match resolve(subject) {
                Type::Array(e) if resolve(&e) == resolve(&elem) => Ok(Fact { word, order }),
                ty => Err(rule!(
                    FactSubject,
                    word = word.spelled(),
                    ty,
                    order = order.clone(),
                    elem
                )),
            }
        })
        .collect()
}

impl Ensures {
    /// The result clause of `f`, `None` without one. `func` is `f`'s name as
    /// a failure prints it. The clause reads only `read` parameters, so what
    /// it states of them at a return it states of the arguments after the
    /// call. The error is the line and the rule the checker refuses the
    /// clause with.
    pub fn of(
        f: &Function,
        decls: &dyn Decls,
        func: &str,
    ) -> Result<Option<Ensures>, (usize, crate::rules::Rule)> {
        let Some(e) = &f.ret_clause else {
            return Ok(None);
        };
        let line = e.line().max(f.line);
        let mut atoms = Vec::new();
        conjuncts(f, Of::Result, e, decls, &mut atoms, &mut Vec::new()).map_err(|r| (line, r))?;
        if atoms.is_empty() {
            return Ok(None);
        }
        let says = crate::trap::result_clause(func).into();
        Ok(Some(Ensures { atoms, says, line }))
    }

    /// The comparisons over `result`, the value a return hands back or the
    /// name a call binds, and `param(k)`, the value of parameter `k`. `None`
    /// for a comparison with a side that has no value.
    pub fn over<'a>(
        &'a self,
        result: Option<&'a Val>,
        param: &'a dyn Fn(usize) -> Option<Val>,
    ) -> impl Iterator<Item = Option<Atom>> + 'a {
        let by = move |k: usize| match k.checked_sub(1) {
            None => result.cloned(),
            Some(k) => param(k),
        };
        self.atoms.iter().map(move |a| a.with(&by))
    }
}

/// Whose `where` clause: parameter `k`'s, whose `value` is `Name(k)`, or the
/// result's, whose `value` is `Name(0)` and whose parameter `k` is
/// `Name(k + 1)`.
#[derive(Clone, Copy)]
enum Of {
    Param(usize),
    Result,
}

impl Of {
    /// The rule a clause outside the grammar breaks.
    fn form(self, f: &Function) -> crate::rules::Rule {
        match self {
            Of::Param(k) => rule!(ClauseForm, param = f.params[k].name.as_str()),
            Of::Result => rule!(ResultClauseForm, name = crate::rules::DeclName(&f.name)),
        }
    }
}

/// Appends the comparisons of `e`, the clause `of` states in `f` or one of
/// its `&&` operands, to `out`, and its fact words with their orders to
/// `facts`. A fact word is only ever a conjunct: under `||` or `!` it is no
/// comparison, and the clause is refused.
fn conjuncts(
    f: &Function,
    of: Of,
    e: &Expr,
    decls: &dyn Decls,
    out: &mut Vec<Atom>,
    facts: &mut Vec<(Word, Type)>,
) -> Result<(), crate::rules::Rule> {
    if let Expr::Call {
        name,
        type_args,
        args,
        ..
    } = e
    {
        if let Some(word) = Word::of(name) {
            return match (type_args.as_slice(), args.as_slice()) {
                ([o], [Expr::Var { name, .. }]) if name == "value" => {
                    facts.push((word, o.clone()));
                    Ok(())
                }
                _ => Err(rule!(FactForm, word = word.spelled())),
            };
        }
    }
    let Expr::Binary { op, lhs, rhs, .. } = e else {
        return Err(of.form(f));
    };
    if *op == BinOp::And {
        conjuncts(f, of, lhs, decls, out, facts)?;
        return conjuncts(f, of, rhs, decls, out, facts);
    }
    op.compare().ok_or_else(|| of.form(f))?;
    let (l, lt) = operand(f, of, lhs, decls)?;
    let (r, rt) = operand(f, of, rhs, decls)?;
    let ty = match (lt, rt) {
        (Some(a), Some(b)) if a == b => a,
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => Type::Int,
        _ => return Err(of.form(f)),
    };
    out.push(Atom { l, op: *op, r, ty });
    Ok(())
}

/// One side of a comparison in the clause `of` states in `f`, with its
/// integer type; `None` for a literal, which takes the other side's.
fn operand(
    f: &Function,
    of: Of,
    e: &Expr,
    decls: &dyn Decls,
) -> Result<(Operand, Option<Type>), crate::rules::Rule> {
    let form = || of.form(f);
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
    let (j, ty) = sibling(f, of, name)?;
    let base = crate::types::resolve(ty, decls);
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
/// Whether `decl` is a validated integer type: a construction of it from a
/// name is a check row ([`Guard::Scalar`]).
pub fn integer_rule(decl: &TypeDecl) -> bool {
    decl.predicate.is_some()
        && decl.type_params.is_empty()
        && crate::validate::width(&decl.base).is_some()
}

/// The name `rhs` constructs a validated integer type from at run time, and
/// the type: `T(consume n)`. A constant the checker proved is a
/// [`Callee::Proven`] call and has no row.
pub fn scalar_arg(rhs: &Rhs, decls: &dyn Decls) -> Option<(Name, String)> {
    let Rhs::Call {
        callee,
        args,
        kind: Callee::Named,
        ..
    } = rhs
    else {
        return None;
    };
    let [(Arg::Val(Val::Name(n)), _)] = args.as_slice() else {
        return None;
    };
    integer_rule(decls.decl(callee)?).then(|| (*n, callee.clone()))
}

/// The comparisons the rule of the validated integer type `decl` states of
/// every value of it, over `Val::Name(Name(0))` for `value` ([`Atom::with`]),
/// and whether they are the whole rule. Each `&&` operand that compares
/// `value` or an integer literal the base holds is one. Any other operand
/// states none: a `||`, arithmetic, which wraps, `=~` or a call. `None` where
/// [`integer_rule`] is false.
pub fn scalar_rule(decl: &TypeDecl) -> Option<(Vec<Atom>, bool)> {
    if !integer_rule(decl) {
        return None;
    }
    let mut atoms = Vec::new();
    let whole = scalar_conjuncts(decl.predicate.as_ref()?, &decl.base, &mut atoms);
    Some((atoms, whole))
}

/// Appends the comparisons of `e`, one `&&` operand of a rule over `value`
/// of the integer type `ty`, and answers whether they are all of `e`.
fn scalar_conjuncts(e: &Expr, ty: &Type, out: &mut Vec<Atom>) -> bool {
    let Expr::Binary { op, lhs, rhs, .. } = e else {
        return false;
    };
    if *op == BinOp::And {
        let l = scalar_conjuncts(lhs, ty, out);
        return scalar_conjuncts(rhs, ty, out) && l;
    }
    let Some((bits, signed)) = crate::validate::width(ty) else {
        return false;
    };
    let lit = |k: i64| (crate::validate::wrap(k, bits, signed) == k).then(|| Val::Lit(Lit::Int(k)));
    let operand = |e: &Expr| {
        let of = match e {
            Expr::Var { name, .. } if name == "value" => Val::Name(Name(0)),
            Expr::Int(k, _) => lit(*k)?,
            Expr::Unary {
                op: UnOp::Neg,
                expr,
                ..
            } => match &**expr {
                Expr::Int(k, _) => lit(k.checked_neg()?)?,
                _ => return None,
            },
            _ => return None,
        };
        Some(Operand { of, part: None })
    };
    match (op.compare(), operand(lhs), operand(rhs)) {
        (Some(_), Some(l), Some(r)) => {
            out.push(Atom {
                l,
                op: *op,
                r,
                ty: ty.clone(),
            });
            true
        }
        _ => false,
    }
}

/// The name `name` names in the clause `of` states in `f`, as [`Of`]
/// numbers it, and its declared type. `value` is the clause's own subject.
/// A parameter clause may name an earlier `read` parameter, a result clause
/// any `read` parameter.
fn sibling<'f>(
    f: &'f Function,
    of: Of,
    name: &str,
) -> Result<(usize, &'f Type), crate::rules::Rule> {
    let (before, shift) = match of {
        Of::Param(k) if name == "value" => return Ok((k, &f.params[k].ty)),
        Of::Result if name == "value" => return Ok((0, &f.ret)),
        Of::Param(k) => (k, 0),
        Of::Result => (f.params.len(), 1),
    };
    let at = f.params[..before].iter().position(|p| p.name == name);
    let fname = crate::rules::DeclName(&f.name);
    match (at, of) {
        (None, Of::Param(k)) => Err(rule!(ClauseLater, param = f.params[k].name.as_str(), name)),
        (None, Of::Result) => Err(rule!(ResultClauseName, name = fname, param = name)),
        (Some(j), _) if f.params[j].capability != Capability::Read => {
            let cap = f.params[j].capability.word();
            Err(match of {
                Of::Param(k) => rule!(ClauseChanges, param = f.params[k].name.as_str(), name, cap),
                Of::Result => rule!(ResultClauseChanges, name = fname, param = name, cap),
            })
        }
        (Some(j), _) => Ok((j + shift, &f.params[j].ty)),
    }
}
