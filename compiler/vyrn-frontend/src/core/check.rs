//! The runtime checks a row can carry ([`super::St::Check`]).

use std::sync::Arc;

use super::{Arg, Callee, Lit, Name, Place, Rhs, Val};
use crate::ast::{BinOp, Capability, Expr, FnId, Function, Stmt, Type, TypeDecl, UnOp};
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
    /// The goal reads a sum or difference, bound to this name, that the
    /// prover cannot show stays within `-2^62..=2^62`. It may wrap, so the
    /// prover keeps no fact of it.
    Wraps(Name),
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
    /// The failure of a `let`'s `where` clause after a write of its binding
    /// ([`crate::trap::let_clause`]), which [`Guard::Clause`] carries.
    Let,
    /// The measure of the loop on `line`, at least zero after the guard holds
    /// (`start`) or below its value there where a turn ends. [`Guard::Clause`]
    /// carries it as `m >= 0`, with the measure as its `says`. No build runs
    /// it: a program that keeps one is refused.
    Measure { line: usize, start: bool },
    /// The invariant of the loop on `line` where the loop starts (`entry`) or
    /// where a turn ends, which [`Guard::Clause`] carries with the invariant as
    /// its `says`. No build runs it: a program that keeps one is refused.
    Invariant { line: usize, entry: bool },
}

impl Raises {
    /// The census name, for a diagnostic and the records.
    pub fn census(self) -> &'static str {
        match self {
            Raises::Row(r) => r.census(),
            Raises::Where => "where",
            Raises::Clause => "where-arg",
            Raises::Result => "where-result",
            Raises::Let => "where-let",
            Raises::Invariant { .. } => "invariant",
            Raises::Measure { .. } => "measure",
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
    /// `where` clause, the value of the `return` after it satisfies the
    /// function's result clause, or the binding the row before it wrote
    /// satisfies its `let`'s clause: each comparison holds. `says` is the sentence a failure prints.
    /// Unlike every other check, it runs where it stands: its operands are
    /// names and literals the row after it reads.
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
/// comparison operator, and both sides are of the integer type `ty`.
#[derive(Debug, Clone, PartialEq)]
pub struct Atom {
    pub l: Side,
    pub op: BinOp,
    pub r: Side,
    pub ty: Type,
}

/// A side of an [`Atom`]: the sum of its operands, each times its literal
/// factor. A clause states a fact of mathematical integers, so a run-time
/// check of a side that is no lone operand fails where the sum leaves
/// `Int64`, the only type such a side has. A side has at least one operand;
/// a lone operand allocates nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct Side {
    pub first: (i64, Operand),
    pub rest: Vec<(i64, Operand)>,
}

impl Side {
    /// The side's operand when it is one operand with factor 1.
    pub fn alone(&self) -> Option<&Operand> {
        match (&self.first, self.rest.is_empty()) {
            ((1, o), true) => Some(o),
            _ => None,
        }
    }

    /// Each operand with its factor, the first first.
    pub fn terms(&self) -> impl Iterator<Item = &(i64, Operand)> {
        std::iter::once(&self.first).chain(&self.rest)
    }

    fn one(o: Operand) -> Side {
        Side {
            first: (1, o),
            rest: Vec::new(),
        }
    }

    /// Adds `t` to the side `s`, or makes `s` of it alone.
    fn push(s: &mut Option<Side>, t: (i64, Operand)) {
        match s {
            Some(s) => s.rest.push(t),
            None => {
                *s = Some(Side {
                    first: t,
                    rest: Vec::new(),
                })
            }
        }
    }

    /// The side with each name replaced as [`Atom::with`] replaces it.
    pub fn with(&self, by: &dyn Fn(usize) -> Option<Val>) -> Option<Side> {
        let each = |(f, o): &(i64, Operand)| {
            let of = match &o.of {
                Val::Name(k) => by(k.index())?,
                lit => lit.clone(),
            };
            let part = o.part.clone();
            Some((*f, Operand { of, part }))
        };
        Some(Side {
            first: each(&self.first)?,
            rest: self.rest.iter().map(each).collect::<Option<_>>()?,
        })
    }
}

/// An operand of an [`Atom`]: the value `of`, or the `part` of it a read
/// gives: `length`, `byteLength`, an integer field, or the length of an
/// array or String field, spelled `f.length` or `f.byteLength`.
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
        Some(Atom {
            l: self.l.with(by)?,
            op: self.op,
            r: self.r.with(by)?,
            ty: self.ty.clone(),
        })
    }
}

/// The comparisons the `where` clause of each parameter of `f` states, by
/// parameter index, over `Val::Name(Name(k))` for parameter `k`
/// ([`Atom::with`]). The clause holds for the whole call: it reads only
/// `read` parameters, which the body cannot assign and no write reaches
/// while the call runs. The error is the parameter and the rule the checker
/// refuses the first bad clause with.
pub fn clauses<'f>(
    f: &'f Function,
    decls: &dyn Decls,
) -> Result<Vec<(usize, Vec<Atom>)>, (&'f crate::ast::Param, crate::rules::Rule)> {
    let mut out = Vec::new();
    for (k, p) in f.params.iter().enumerate() {
        let Some(e) = &p.clause else { continue };
        let name = crate::rules::DeclName(&f.name);
        let entry = |why: &str| (p, rule!(ClauseEntry, name, why));
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
        conjuncts(Of::Param(f, k), e, decls, &mut atoms, &mut Vec::new()).map_err(|r| (p, r))?;
        // A fact holds of the argument the call proved it of, whatever the
        // body does with it; a comparison must hold for the whole call.
        if atoms.is_empty() {
            continue;
        }
        if p.capability != Capability::Read {
            let (param, cap) = (p.name.as_str(), p.capability.word());
            return Err((p, rule!(ClauseChanges, param, name = param, cap)));
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

/// A `let`'s `where` clause: a promise about the binding that every write
/// of it keeps. Its comparisons are checked after each write unless proven;
/// its facts are never checked at run time, and the fact judgment refuses a
/// write that may break one.
#[derive(Debug, Clone, PartialEq)]
pub struct Promise {
    /// The `let`'s line.
    pub line: usize,
    /// The comparisons, over `Val::Name(Name(0))` for `value` ([`Atom::with`]).
    pub atoms: Vec<Atom>,
    pub facts: Vec<Fact>,
    /// The sentence a failed comparison prints ([`crate::trap::let_clause`]).
    pub says: Arc<str>,
}

impl Promise {
    /// The promise of the `let` `s`, in a body whose type parameters `bounds`
    /// bounds; `None` for a `let` without a clause. The error is the line and
    /// the rule the checker refuses the clause with.
    pub fn of(
        s: &Stmt,
        bounds: Option<&Bounds>,
        decls: &dyn Decls,
        orders: &crate::order::Names,
    ) -> Result<Option<Promise>, (usize, crate::rules::Rule)> {
        let Stmt::Let {
            name,
            ty: Some(ty),
            clause: Some(e),
            line,
            ..
        } = s
        else {
            return Ok(None);
        };
        let of = Of::Let { name, ty, bounds };
        let mut atoms = Vec::new();
        conjuncts(of, e, decls, &mut atoms, &mut Vec::new()).map_err(|r| (*line, r))?;
        let facts = facts(of, e, decls, orders).map_err(|r| (*line, r))?;
        Ok(Some(Promise {
            line: *line,
            atoms,
            facts,
            says: crate::trap::let_clause(name).into(),
        }))
    }
}

/// A `while` loop's invariant: comparisons that hold at the loop's head. The
/// state before the loop proves them, and so does the end of each turn; the
/// head assumes them. Nothing checks them at run time: a program whose proof
/// fails is refused ([`Raises::Invariant`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Invariant {
    /// The comparisons, over the body's names.
    pub atoms: Vec<Atom>,
    /// The invariant in source form.
    pub says: Arc<str>,
    /// `decreases(m)`: the measure `m`, an `Int64` sum that is at least zero
    /// after the guard holds and falls on every turn, so the loop ends; and
    /// `m` in source form.
    pub measure: Option<(Side, Arc<str>)>,
    /// The loop's line.
    pub line: usize,
    /// The line of the body's last statement, where a turn that falls off the
    /// body ends.
    pub end: usize,
}

impl Invariant {
    /// The clause `e` of the `while` on `line`, whose body's last statement
    /// is on `end`, over `Val::Name(Name(j))` for `names[j]`, the names in
    /// scope it reads and their types ([`Atom::with`]). A `decreases(m)`
    /// conjunct is the measure, at most one. The error is the rule the
    /// checker refuses the clause with.
    pub fn of(
        e: &Expr,
        line: usize,
        end: usize,
        names: &[(String, Type)],
        decls: &dyn Decls,
    ) -> Result<Invariant, crate::rules::Rule> {
        let of = Of::Loop { line, names };
        let mut parts = Vec::new();
        split(e, &mut parts);
        let (mut atoms, mut facts, mut measure) = (Vec::new(), Vec::new(), None);
        let mut says = Vec::new();
        for p in parts {
            let m = match p {
                Expr::Call { name, args, .. } if name == "decreases" => args,
                _ => {
                    conjuncts(of, p, decls, &mut atoms, &mut facts)?;
                    says.push(spelled(p));
                    continue;
                }
            };
            let [m] = m.as_slice() else {
                return Err(rule!(MeasureForm, line));
            };
            let (mut terms, mut ty) = (None, None);
            side(of, m, 1, decls, &mut terms, &mut ty)?;
            let (Some(terms), None | Some(Type::Int)) = (terms, ty) else {
                return Err(rule!(MeasureForm, line));
            };
            if measure.is_some() {
                return Err(rule!(MeasureForm, line));
            }
            measure = Some((terms, spelled(m).into()));
        }
        if !facts.is_empty() {
            return Err(of.form());
        }
        Ok(Invariant {
            atoms,
            says: says.join(" && ").into(),
            measure,
            line,
            end,
        })
    }

    /// The invariant with each name replaced as [`Atom::with`] replaces it.
    pub fn with(&self, by: &dyn Fn(usize) -> Option<Val>) -> Option<Invariant> {
        let measure = match &self.measure {
            Some((m, says)) => Some((m.with(by)?, says.clone())),
            None => None,
        };
        Some(Invariant {
            atoms: self
                .atoms
                .iter()
                .map(|a| a.with(by))
                .collect::<Option<_>>()?,
            measure,
            ..self.clone()
        })
    }

    /// `m >= 0` for the measure `m`, the comparison its proof rows carry.
    pub fn nonnegative(&self) -> Option<Atom> {
        let (m, _) = self.measure.as_ref()?;
        let zero = Operand {
            of: Val::Lit(Lit::Int(0)),
            part: None,
        };
        Some(Atom {
            l: m.clone(),
            op: BinOp::GtEq,
            r: Side::one(zero),
            ty: Type::Int,
        })
    }
}

/// Appends to `out` the `&&` operands of `e`, each no `&&` itself.
fn split<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
    match e {
        Expr::Binary {
            op: BinOp::And,
            lhs,
            rhs,
            ..
        } => {
            split(lhs, out);
            split(rhs, out);
        }
        _ => out.push(e),
    }
}

/// `e` in source form, for the operators of a clause.
pub fn spelled(e: &Expr) -> String {
    let rank = |e: &Expr| match e {
        Expr::Binary { op, .. } => match op {
            BinOp::Or => 1,
            BinOp::And => 2,
            BinOp::Add | BinOp::Sub => 4,
            BinOp::Mul | BinOp::Div | BinOp::Rem => 5,
            _ => 3,
        },
        _ => 6,
    };
    match e {
        Expr::Var { name, .. } => name.clone(),
        Expr::Field { expr, field, .. } => format!("{}.{field}", spelled(expr)),
        Expr::Int(n, _) => n.to_string(),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
            ..
        } if rank(expr) == 6 => format!("-{}", spelled(expr)),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
            ..
        } => format!("-({})", spelled(expr)),
        Expr::Binary { op, lhs, rhs, .. } => {
            let at = rank(e);
            let wrap = |x: &Expr, tight: bool| match rank(x) < at || (tight && rank(x) == at) {
                true => format!("({})", spelled(x)),
                false => spelled(x),
            };
            let op = crate::parser::binop_text(*op);
            format!("{} {op} {}", wrap(lhs, false), wrap(rhs, true))
        }
        _ => "..".into(),
    }
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
        for fact in facts(Of::Param(f, k), e, decls, orders).map_err(|r| (p.line, r))? {
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
    let facts = facts(Of::Result(f), e, decls, orders).map_err(|r| (line, r))?;
    Ok(facts.into_iter().map(Guarantee).collect())
}

/// The facts the clause `e` of `of` states, each of an array of its order's
/// element type.
fn facts(
    of: Of,
    e: &Expr,
    decls: &dyn Decls,
    orders: &crate::order::Names,
) -> Result<Vec<Fact>, crate::rules::Rule> {
    let mut calls = Vec::new();
    conjuncts(of, e, decls, &mut Vec::new(), &mut calls)?;
    let subject = of.subject();
    let bound = orders.name(crate::order::Known::Order);
    let resolve = |t: &Type| crate::types::resolve(t, decls);
    (calls.into_iter())
        .map(|(word, order)| {
            let elem = match &order {
                Type::Order(o) => Some(o.elem().clone()),
                Type::Param(p) => (of.bounds().and_then(|bs| bs.get(p)).into_iter())
                    .flatten()
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
        conjuncts(Of::Result(f), e, decls, &mut atoms, &mut Vec::new()).map_err(|r| (line, r))?;
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

/// Whose `where` clause: parameter `k`'s of `f`, whose `value` is
/// `Name(k)`; the result's of `f`, whose `value` is `Name(0)` and whose
/// parameter `k` is `Name(k + 1)`; or the `let` of `name`'s, of type `ty`,
/// in a body whose type parameters `bounds` bounds, whose `value` is
/// `Name(0)`; or the invariant of the `while` on `line`, whose names are
/// `Name(j)` for `names[j]`.
#[derive(Clone, Copy)]
enum Of<'f> {
    Param(&'f Function, usize),
    Result(&'f Function),
    Let {
        name: &'f str,
        ty: &'f Type,
        bounds: Option<&'f Bounds>,
    },
    Loop {
        line: usize,
        names: &'f [(String, Type)],
    },
}

/// The bounds of a function's type parameters ([`Function::type_bounds`]).
type Bounds = std::collections::HashMap<String, Vec<String>>;

impl<'f> Of<'f> {
    /// The rule a clause outside the grammar breaks.
    fn form(self) -> crate::rules::Rule {
        match self {
            Of::Param(f, k) => rule!(ClauseForm, param = f.params[k].name.as_str()),
            Of::Result(f) => rule!(ResultClauseForm, name = crate::rules::DeclName(&f.name)),
            Of::Let { name, .. } => rule!(LetClauseForm, name),
            Of::Loop { line, .. } => rule!(InvariantForm, line),
        }
    }

    /// The type of `value`.
    fn subject(self) -> &'f Type {
        match self {
            Of::Param(f, k) => &f.params[k].ty,
            Of::Result(f) => &f.ret,
            Of::Let { ty, .. } => ty,
            Of::Loop { .. } => &Type::Unit,
        }
    }

    /// The bounds of the type parameters the clause may name.
    fn bounds(self) -> Option<&'f Bounds> {
        match self {
            Of::Param(f, _) | Of::Result(f) => Some(&f.type_bounds),
            Of::Let { bounds, .. } => bounds,
            Of::Loop { .. } => None,
        }
    }
}

/// Appends the comparisons of `e`, the clause `of` states in `f` or one of
/// its `&&` operands, to `out`, and its fact words with their orders to
/// `facts`. A fact word is only ever a conjunct: under `||` or `!` it is no
/// comparison, and the clause is refused.
fn conjuncts(
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
    let form = || match fact_under(e) {
        Some(word) => rule!(FactConjunct, word = word.spelled()),
        None => of.form(),
    };
    let Expr::Binary { op, lhs, rhs, .. } = e else {
        return Err(form());
    };
    if *op == BinOp::And {
        conjuncts(of, lhs, decls, out, facts)?;
        return conjuncts(of, rhs, decls, out, facts);
    }
    op.compare().ok_or_else(form)?;
    let (mut l, mut r, mut ty) = (None, None, None);
    side(of, lhs, 1, decls, &mut l, &mut ty)?;
    side(of, rhs, 1, decls, &mut r, &mut ty)?;
    let ty = ty.unwrap_or(Type::Int);
    let (Some(l), Some(r)) = (l, r) else {
        return Err(of.form());
    };
    let sum = |s: &Side| s.alone().is_none();
    if ty != Type::Int && (sum(&l) || sum(&r)) {
        return Err(of.form());
    }
    out.push(Atom { l, op: *op, r, ty });
    Ok(())
}

/// Appends to `out` the operands of `e`, one side of a comparison in the
/// clause `of` states, each times `factor`. Every operand that is no literal
/// is of the one integer type `ty`, which the first such sets. A side is a sum of differences of terms; a term
/// is an operand, a literal multiple of a term, or a negated term.
fn side(
    of: Of,
    e: &Expr,
    factor: i64,
    decls: &dyn Decls,
    out: &mut Option<Side>,
    ty: &mut Option<Type>,
) -> Result<(), crate::rules::Rule> {
    let lit = |e: &Expr| match e {
        Expr::Int(n, _) => Some(*n),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
            ..
        } => match &**expr {
            Expr::Int(n, _) => n.checked_neg(),
            _ => None,
        },
        _ => None,
    };
    let times = |k: i64| factor.checked_mul(k).ok_or_else(|| of.form());
    match e {
        Expr::Binary { op, lhs, rhs, .. } => match op {
            BinOp::Add => {
                side(of, lhs, factor, decls, out, ty)?;
                side(of, rhs, factor, decls, out, ty)
            }
            BinOp::Sub => {
                side(of, lhs, factor, decls, out, ty)?;
                side(of, rhs, times(-1)?, decls, out, ty)
            }
            BinOp::Mul => match (lit(lhs), lit(rhs)) {
                (Some(k), _) => side(of, rhs, times(k)?, decls, out, ty),
                (None, Some(k)) => side(of, lhs, times(k)?, decls, out, ty),
                (None, None) => {
                    let (a, b) = (spelled(lhs), spelled(rhs));
                    Err(rule!(ClauseProduct, a, b))
                }
            },
            _ => Err(of.form()),
        },
        Expr::Unary {
            op: UnOp::Neg,
            expr,
            ..
        } if lit(e).is_none() => side(of, expr, times(-1)?, decls, out, ty),
        _ => {
            let (o, t) = operand(of, e, decls)?;
            match (t, &*ty) {
                (Some(t), Some(u)) if t != *u => return Err(of.form()),
                (Some(t), None) => *ty = Some(t),
                _ => {}
            }
            Side::push(out, (factor, o));
            Ok(())
        }
    }
}

/// The first fact word `e` calls, at any depth.
fn fact_under(e: &Expr) -> Option<Word> {
    let mut found = None;
    crate::ast::each_expr_in(e, &mut |x| {
        if let Expr::Call { name, .. } = x {
            found = found.or(Word::of(name));
        }
        found.is_none()
    });
    found
}

/// One side of a comparison in the clause `of` states in `f`, with its
/// integer type; `None` for a literal, which takes the other side's.
fn operand(
    of: Of,
    e: &Expr,
    decls: &dyn Decls,
) -> Result<(Operand, Option<Type>), crate::rules::Rule> {
    let form = || of.form();
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
            Expr::Var { name, .. } => (name, Some(field.clone())),
            // A field's length, one level deep: `value.shape.length`.
            Expr::Field { expr, field: f, .. } if field == "length" || field == "byteLength" => {
                match &**expr {
                    Expr::Var { name, .. } => (name, Some(format!("{f}.{field}"))),
                    _ => return Err(form()),
                }
            }
            _ => return Err(form()),
        },
        _ => return Err(form()),
    };
    let (j, ty) = sibling(of, name)?;
    let base = crate::types::resolve(ty, decls);
    let field = |name: &str| {
        let fs = crate::types::record_fields(&base, decls).ok_or_else(form)?;
        let t = &fs.iter().find(|x| x.name == name).ok_or_else(form)?.ty;
        Ok(crate::types::resolve(t, decls))
    };
    let length = |t: &Type, part: &str| match (t, part) {
        (t, "length") if t.is_seq() => true,
        (Type::Str, "byteLength") => true,
        _ => false,
    };
    let ty = match part.as_deref() {
        None => integer(base)?,
        Some(p) if length(&base, p) => Type::Int,
        Some(p) => match p.split_once('.') {
            Some((f, len)) if length(&field(f)?, len) => Type::Int,
            Some(_) => return Err(form()),
            None => integer(field(p)?)?,
        },
    };
    let of = Val::Name(Name(j as u32));
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
                l: Side::one(l),
                op: *op,
                r: Side::one(r),
                ty: ty.clone(),
            });
            true
        }
        _ => false,
    }
}

/// The name `name` names in the clause `of`, as [`Of`] numbers it, and its
/// declared type. `value` is the clause's own subject. A parameter clause
/// may name an earlier `read` parameter, a result clause any `read`
/// parameter, a `let` clause nothing else, and an invariant a name in scope.
fn sibling<'f>(of: Of<'f>, name: &str) -> Result<(usize, &'f Type), crate::rules::Rule> {
    let (f, before, shift) = match of {
        Of::Param(_, k) if name == "value" => return Ok((k, of.subject())),
        Of::Result(_) | Of::Let { .. } if name == "value" => return Ok((0, of.subject())),
        Of::Let { name: binding, .. } => return Err(rule!(LetClauseName, binding, name)),
        Of::Loop { line, names } => {
            let at = names.iter().position(|(n, _)| n == name);
            return match at {
                Some(j) => Ok((j, &names[j].1)),
                None => Err(rule!(InvariantName, line, name)),
            };
        }
        Of::Param(f, k) => (f, k, 0),
        Of::Result(f) => (f, f.params.len(), 1),
    };
    let at = f.params[..before].iter().position(|p| p.name == name);
    let fname = crate::rules::DeclName(&f.name);
    match (at, of) {
        (None, Of::Param(_, k)) => Err(rule!(ClauseLater, param = f.params[k].name.as_str(), name)),
        (None, _) => Err(rule!(ResultClauseName, name = fname, param = name)),
        (Some(j), _) if f.params[j].capability != Capability::Read => {
            let cap = f.params[j].capability.word();
            Err(match of {
                Of::Param(_, k) => {
                    rule!(ClauseChanges, param = f.params[k].name.as_str(), name, cap)
                }
                _ => rule!(ResultClauseChanges, name = fname, param = name, cap),
            })
        }
        (Some(j), _) => Ok((j + shift, &f.params[j].ty)),
    }
}
