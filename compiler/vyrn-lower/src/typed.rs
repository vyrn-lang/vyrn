//! The typed judgment over the named core: a name of a
//! validated type is produced only by that type's constructor, a name already
//! of the type, or a literal the checker proved. Every name is bound once
//! (`St::Let`), so it is a use-def walk: [`judge`] records what bound each
//! name and judges the producer of every store into a place the caller calls
//! validated. Which types carry a rule is [`vyrn_frontend::validate`]'s; the
//! caller asks it. A sized integer is judged by width: a producer of the same
//! width and signedness crosses nothing. The module also holds the must-use
//! [`obligation`] and the `vyrn check` rules [`stores`], [`loops`],
//! [`refused`] and [`drops`].

use std::collections::HashMap;

use vyrn_frontend::ast::Type;

use crate::core::{rows, Arg, Body, Callee, Name, NameInfo, Place, Rhs, Site, St, Val};
use vyrn_frontend::ast::Capability;

/// A step from one type into the type a place holds, for the caller that
/// resolves a place's type. `Global` has no base.
#[derive(Debug, Clone, Copy)]
pub enum Step<'a> {
    Field(&'a str),
    Elem,
    Key,
    Global(&'a str),
}

/// What produced the value a store put into a validated place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum How {
    /// The type's own constructor, `Age(n)`, where the predicate runs. A
    /// record literal of a validated record type is the same answer: every
    /// engine runs the generated constructor at it (a cross-field
    /// `where`).
    Constructor,
    /// A name already of the type.
    ByName,
    /// A literal, or another crossing the checker proved at compile time
    /// ([`Callee::Proven`]).
    Literal,
    /// A primitive over literals only, into a sized integer. The checker
    /// ranges it where the two share a sign (`-200` into an `Int8` is
    /// refused); otherwise it wraps, as `-1` into a `UInt8` is 255.
    Constant,
    /// A raw value reaching a validated slot, by kind: what the judgment
    /// refuses.
    Finding(&'static str),
}

impl How {
    pub fn kind(&self) -> &'static str {
        match self {
            How::Constructor => "by-constructor",
            How::ByName => "by-name",
            How::Literal => "by-literal",
            How::Constant => "by-constant",
            How::Finding(k) => k,
        }
    }

    pub fn is_finding(&self) -> bool {
        matches!(self, How::Finding(_))
    }
}

/// One store the judgment looked at.
#[derive(Debug, Clone)]
pub struct Store {
    /// The body, by index into the slice handed to [`judge`].
    pub body: usize,
    /// The place, as the core spells it.
    pub place: String,
    /// The name of the declaration whose producer must have run.
    pub ty: String,
    /// A callee's name, a place or a name as the core spells it, or `@lit`,
    /// `@prim` or `@make`.
    pub producer: String,
    pub line: usize,
    pub how: How,
}

/// The judgment's answer.
#[derive(Debug, Default)]
pub struct Judged {
    /// Every store into a validated place, in body order.
    pub stores: Vec<Store>,
    /// Stores into a sized integer whose producer has no type the caller
    /// resolves, such as a read of a generic parameter's place. Counted, not
    /// guessed; zero over the corpus.
    pub unjudged: usize,
}

impl Judged {
    pub fn findings(&self) -> impl Iterator<Item = &Store> {
        self.stores.iter().filter(|s| s.how.is_finding())
    }
}

/// Judges every store in `bodies`, each a frame of the core.
///
/// `validated` maps a destination type to the name of the declaration whose
/// producer must have run for it (a named type with a `where`, or a sized
/// integer), or `None` where no rule applies. `step` says what a place holds.
/// Both answer from the program's declarations, which the judgment does not
/// hold.
pub fn judge(
    bodies: &[&Body],
    validated: &mut dyn FnMut(&Type) -> Option<String>,
    step: &mut dyn FnMut(Option<&Type>, Step) -> Option<Type>,
) -> Judged {
    let mut out = Judged::default();
    for (i, b) in bodies.iter().enumerate() {
        let mut w = Walk {
            body: b,
            index: i,
            born: HashMap::new(),
            validated,
            step,
            out: &mut out,
        };
        rows(&b.stmts).for_each(|(s, _)| w.stmt(s));
    }
    out
}

struct Walk<'a, 'b> {
    body: &'a Body,
    index: usize,
    /// What bound each name: the use-def edge.
    born: HashMap<Name, &'a Rhs>,
    validated: &'b mut dyn FnMut(&Type) -> Option<String>,
    step: &'b mut dyn FnMut(Option<&Type>, Step) -> Option<Type>,
    out: &'b mut Judged,
}

impl<'a> Walk<'a, '_> {
    fn stmt(&mut self, s: &'a St) {
        match s {
            St::Let(n, rhs) => {
                self.born.insert(*n, rhs);
                let info = &self.body.names[*n as usize];
                let ty = info.ty.clone();
                self.judge_store(ty.clone(), info.source.clone(), info.line, rhs, Some(ty));
            }
            St::Store {
                place, value, line, ..
            } => {
                if let Some(ty) = self.place_ty(place) {
                    // The producer is the `let` that bound the name in this
                    // frame, or the name itself when a parameter, capture or
                    // arm binder bound it.
                    let outside;
                    let rhs: &Rhs = match value {
                        Val::Name(n) => match self.born.get(n).copied() {
                            Some(r) => r,
                            None => {
                                outside = Rhs::Val(Val::Name(*n));
                                &outside
                            }
                        },
                        Val::Lit(_) => {
                            outside = Rhs::Val(value.clone());
                            &outside
                        }
                    };
                    let place = self.spell(place);
                    let named = match value {
                        Val::Name(n) => Some(self.body.names[*n as usize].ty.clone()),
                        Val::Lit(_) => None,
                    };
                    self.judge_store(ty, place, *line, rhs, named);
                }
            }
            St::If { .. }
            | St::Loop { .. }
            | St::Block { .. }
            | St::Switch { .. }
            | St::Do { .. }
            | St::Drop(..)
            | St::Row { .. }
            | St::Break { .. }
            | St::Continue { .. }
            | St::Return { .. }
            | St::Trap
            | St::Check(_) => {}
        }
    }

    /// `to` is the place's type, `rhs` what the store was given, and `named`
    /// the type of the name `rhs` was bound to.
    ///
    /// A read converts nothing, so where the declarations cannot resolve the
    /// place it reads, `named` answers. A place of a validated type holds only
    /// what this judgment let in, so a read of it is a producer of that type.
    fn judge_store(
        &mut self,
        to: Type,
        place: String,
        line: usize,
        rhs: &Rhs,
        named: Option<Type>,
    ) {
        let from = match rhs {
            Rhs::Read(_) | Rhs::Take(_) => self.rhs_ty(rhs).or(named),
            _ => self.rhs_ty(rhs),
        };
        let ctor = matches!(rhs, Rhs::Call { callee, .. } if last(callee) == spelling(&to));
        // A narrowing is a producer of another width, so a guess would read
        // every untyped integer store as one.
        if from.is_none()
            && matches!(to, Type::IntN { .. })
            && !ctor
            && !matches!(rhs, Rhs::Val(Val::Lit(_)))
        {
            self.out.unjudged += 1;
            return;
        }
        let Some(name) = (self.validated)(&to) else {
            return;
        };
        let how = match rhs {
            Rhs::Call {
                kind: Callee::Proven,
                ..
            } => How::Literal,
            _ if ctor => How::Constructor,
            Rhs::Val(Val::Lit(_)) => How::Literal,
            // Only into a sized integer: a named type's predicate owes a
            // producer whatever the operands are.
            Rhs::Prim(_, vs, _)
                if matches!(to, Type::IntN { .. })
                    && !vs.is_empty()
                    && vs.iter().all(|v| matches!(v, Val::Lit(_))) =>
            {
                How::Constant
            }
            // A producer already of the type, or of the same integer width and
            // signedness (`Int` and `Int64`), crosses nothing
            // (`validate::required`, `validate::narrows`).
            _ if from
                .as_ref()
                .is_some_and(|f| *f == to || same_width(f, &to)) =>
            {
                How::ByName
            }
            Rhs::Make(..) => How::Constructor,
            Rhs::Call { .. } => How::Finding("other-call"),
            Rhs::Prim(..) => How::Finding("primitive"),
            Rhs::Read(_) | Rhs::Take(_) => How::Finding("read-of-place"),
            Rhs::Val(Val::Name(_)) => How::Finding("other-name"),
        };
        self.out.stores.push(Store {
            body: self.index,
            place,
            ty: name,
            producer: match rhs {
                Rhs::Call {
                    kind: Callee::Proven,
                    args,
                    ..
                } if matches!(args.as_slice(), [(Arg::Val(Val::Lit(_)), _)]) => "@lit".into(),
                Rhs::Call { callee, .. } => callee.clone(),
                Rhs::Prim(..) => "@prim".into(),
                Rhs::Make(..) => "@make".into(),
                Rhs::Read(p) | Rhs::Take(p) => self.spell(p),
                Rhs::Val(Val::Name(n)) => self.body.names[*n as usize].source.clone(),
                Rhs::Val(Val::Lit(_)) => "@lit".into(),
            },
            line,
            how,
        });
    }

    /// The type a right-hand side produces, where the core or the
    /// declarations name one. A literal and a record literal have none.
    fn rhs_ty(&mut self, rhs: &Rhs) -> Option<Type> {
        match rhs {
            Rhs::Val(Val::Name(n)) => Some(self.body.names[*n as usize].ty.clone()),
            Rhs::Read(p) | Rhs::Take(p) => self.place_ty(p),
            Rhs::Call { ret, .. } => ret.clone(),
            Rhs::Prim(_, _, ty) => ty.clone(),
            Rhs::Make(..) | Rhs::Val(Val::Lit(_)) => None,
        }
    }

    fn place_ty(&mut self, p: &Place) -> Option<Type> {
        match p {
            Place::Name(n) => Some(self.body.names[*n as usize].ty.clone()),
            Place::Global(g) => (self.step)(None, Step::Global(g)),
            Place::Field(base, f) => {
                let b = self.place_ty(base);
                (self.step)(b.as_ref(), Step::Field(f))
            }
            Place::Elem(base, _) => {
                let b = self.place_ty(base);
                (self.step)(b.as_ref(), Step::Elem)
            }
            Place::Key(base, _) => {
                let b = self.place_ty(base);
                (self.step)(b.as_ref(), Step::Key)
            }
        }
    }

    fn spell(&self, p: &Place) -> String {
        match p {
            Place::Name(n) => self.body.names[*n as usize].source.clone(),
            Place::Global(g) => g.clone(),
            Place::Field(b, f) => format!("{}.{f}", self.spell(b)),
            Place::Elem(b, _) => format!("{}[]", self.spell(b)),
            Place::Key(b, _) => format!("{}{{}}", self.spell(b)),
        }
    }
}

fn same_width(from: &Type, to: &Type) -> bool {
    vyrn_frontend::validate::width(from).is_some()
        && vyrn_frontend::validate::width(to).is_some()
        && !vyrn_frontend::validate::narrows(from, to)
}

/// The name of a type's own producer: a named type's name, else its spelling,
/// which names its conversion (`UInt8`).
fn spelling(t: &Type) -> String {
    match t {
        Type::Named(n) => n.clone(),
        other => other.to_string(),
    }
}

/// The last segment of a callee's spelling: `mod.Age` and `Age` name one
/// declaration.
fn last(callee: &str) -> &str {
    callee
        .rsplit(['.', ':', '/'])
        .next()
        .unwrap_or(callee)
        .trim_start_matches('@')
}

/// The must-use obligation: a value of a linear type is disposed
/// exactly once on every path out of its block. It is a rule about a type, so
/// it belongs to the typed judgment. Which types carry it is a lookup in
/// [`vyrn_frontend::declared::Owned`], so `impl Owned for T` joins with no
/// compiler change; [`vyrn_frontend::own::Linear`] chooses the fix menu's
/// wording.
///
/// It walks the AST, not the core: the rule is about the blocks and paths the
/// reader wrote, and its sentence names the line of the `let`.
pub mod obligation {
    use std::collections::HashMap;

    use vyrn_frontend::ast::*;
    use vyrn_frontend::ast::{paths, stmt_mentions, sub_blocks};
    use vyrn_frontend::declared::Declared;
    use vyrn_frontend::diagnostics::Diagnostic;
    use vyrn_frontend::own::Linear;

    /// One live must-use binding, as a diagnostic about it needs it: the type
    /// spelled the way the program spelled it, and which row obliged it.
    #[derive(Clone)]
    struct Owed {
        ty: String,
        row: Linear,
    }

    /// What a straight-line statement list does to one live binding.
    #[derive(Clone, Copy, Default)]
    struct Scan {
        /// Disposed on every path that FALLS OUT of the list.
        disposed: bool,
        /// Nothing falls out: every path leaves via `return`/`break`/`continue`,
        /// so `disposed` says nothing about what follows.
        diverges: bool,
        /// Some path abandons it: a `return` that does not move it out, or two
        /// branches that disagree about whether it was disposed.
        leaked: bool,
        /// Disposed, then mentioned again on the same path.
        doubled: bool,
    }

    /// Every obligation `program` breaks, for the slot
    /// `vyrn_frontend::own::install_must_use` fills. It builds its own
    /// [`Declared`] because the caller's lives inside the move check's run.
    pub fn judge(program: &Program) -> Vec<Diagnostic> {
        check(program, &Declared::new(program))
    }

    pub fn check(program: &Program, decl: &Declared) -> Vec<Diagnostic> {
        // Functions, seeded builtins included, whose return type carries the
        // obligation, with the spelling the diagnostic quotes.
        let producers: HashMap<&str, Owed> = program
            .functions
            .iter()
            .chain(vyrn_frontend::prelude::all())
            .filter_map(|f| {
                Some((
                    f.name.as_str(),
                    owed(&f.ret, f.type_params.as_slice(), decl)?,
                ))
            })
            .collect();
        let mut out = Vec::new();
        for f in &program.functions {
            // A must-use parameter carries the obligation into the callee: the
            // caller discharged its own by moving it, and `fn sink(s: Stream<T>) {}`
            // must not be the hole that lets it evaporate.
            let mut live: Vec<(String, Owed)> = Vec::new();
            for p in &f.params {
                // A receiver does not: `fn release(self)` IS the disposal.
                // `self` is a keyword, so the name marks an impl receiver.
                if p.name == "self" {
                    continue;
                }
                if let Some(o) = owed(&p.ty, &[], decl) {
                    let s = scan(&f.body.stmts, &p.name, false);
                    report(&mut out, &s, f.line, &p.name, &o, &f.module);
                    live.push((p.name.clone(), o));
                }
            }
            block(&f.body, &mut live, &producers, &f.module, decl, &mut out);
        }
        for t in &program.tests {
            block(
                &t.body,
                &mut Vec::new(),
                &producers,
                &t.module,
                decl,
                &mut out,
            );
        }
        for b in &program.benches {
            block(
                &b.body,
                &mut Vec::new(),
                &producers,
                &b.module,
                decl,
                &mut out,
            );
        }
        out
    }

    /// The obligation `ty` carries, with the spelling a diagnostic quotes it by,
    /// or `None` where it carries none.
    ///
    /// `binders` are the type parameters in scope where `ty` was written. A
    /// spelling that mentions one quotes only the type constructor (`Stream`),
    /// since this pass has no types to say `Stream<Int64>`. The test is on the
    /// spelling because a signature's type parameter is not reliably a
    /// `Type::Param` before the checker runs.
    fn owed(ty: &Type, binders: &[String], decl: &Declared) -> Option<Owed> {
        let row = decl.linear_kind(ty)?;
        let r = ty.to_string();
        let mentions = |p: &String| {
            r.split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|w| w == p.as_str())
        };
        let ty = match binders.iter().any(mentions) {
            true => r.split('<').next().unwrap_or(&r).to_string(),
            false => r,
        };
        Some(Owed { ty, row })
    }

    fn report(
        out: &mut Vec<Diagnostic>,
        s: &Scan,
        line: usize,
        name: &str,
        o: &Owed,
        module: &Option<String>,
    ) {
        let ty = &o.ty;
        let art = match ty.chars().next() {
            Some('A' | 'E' | 'I' | 'O' | 'U' | 'a' | 'e' | 'i' | 'o' | 'u') => "an",
            _ => "a",
        };
        let msg = if s.doubled {
            format!("`{name}` is {art} `{ty}` and is disposed more than once")
        } else if s.leaked || !(s.disposed || s.diverges) {
            format!("`{name}` is {art} `{ty}` and is never disposed")
        } else {
            return;
        };
        let mut d = Diagnostic::error(line, 0, "movecheck", msg);
        // A stream's release is pushed by its own lowering, so `drop` on one
        // reclaims nothing; a declared type has no `close` and is not iterable
        // unless it says so.
        d.note = Some(match &o.row {
            Linear::Stream => format!(
                "a stream must be consumed with `for … in`, forwarded by returning it, \
                 or released with `close({name})` — on every path"
            ),
            Linear::Declared(by) if by == ty => format!(
                "`{ty}` declares `impl MustUse`, so a value of it must be handed on by \
                 name — passed to a call, forwarded by returning it, or released with \
                 `drop {name}` — on every path"
            ),
            // A container: the reader wrote `Array<Txn>` and the row is `Txn`'s,
            // so the note names both.
            Linear::Declared(by) => format!(
                "`{by}` declares `impl MustUse` and a `{ty}` holds one, so the container \
                 must be handed on by name — passed to a call, forwarded by returning it, \
                 or released with `drop {name}`, which releases each element — on every path"
            ),
        });
        d.file = module.clone();
        out.push(d);
    }

    /// Checks that every must-use binding declared in `b` is disposed on every
    /// path out of the rest of `b`. `live` holds the enclosing scopes' obliged
    /// names, so `let t = s` is recognised as a move.
    fn block(
        b: &Block,
        live: &mut Vec<(String, Owed)>,
        producers: &HashMap<&str, Owed>,
        module: &Option<String>,
        decl: &Declared,
        out: &mut Vec<Diagnostic>,
    ) {
        let base = live.len();
        for (i, st) in b.stmts.iter().enumerate() {
            if let Stmt::Let {
                name,
                ty,
                value,
                line,
                ..
            } = st
            {
                if let Some(o) = owed_let(ty.as_ref(), value, live, producers, decl) {
                    // `false`: a `break` in the rest of this block leaves the
                    // declaring block and abandons the value ([`scan`]).
                    let s = scan(&b.stmts[i + 1..], name, false);
                    report(out, &s, *line, name, &o, module);
                    live.push((name.clone(), o));
                }
            }
            for sub in sub_blocks(st) {
                block(sub, live, producers, module, decl, out);
            }
        }
        live.truncate(base);
    }

    /// The obligation this `let` binds, if it binds one.
    fn owed_let(
        ty: Option<&Type>,
        value: &Expr,
        live: &[(String, Owed)],
        producers: &HashMap<&str, Owed>,
        decl: &Declared,
    ) -> Option<Owed> {
        // An alias of a must-use type carries its base's obligation.
        if let Some(o) = ty.and_then(|t| owed(t, &[], decl)) {
            return Some(o);
        }
        match value {
            Expr::Call { name, .. } => producers.get(name.as_str()).cloned(),
            // `let t = s` moves the value; `t` inherits the obligation and the
            // spelling, and the mention of `s` discharges `s`'s.
            Expr::Var { name, .. } => live.iter().find(|(l, _)| l == name).map(|(_, o)| o.clone()),
            // A branch hands on whichever arm ran, so the binding inherits the
            // obligation any arm carries. The checker made the arms agree on
            // the type, so the first such arm's spelling serves.
            Expr::Match { arms, .. } => arms.iter().find_map(|a| {
                // A block arm yields nothing a binding could owe.
                a.body
                    .as_expr()
                    .and_then(|e| owed_let(None, e, live, producers, decl))
            }),
            Expr::IfExpr {
                then_branch,
                else_branch,
                ..
            } => owed_let(None, then_branch, live, producers, decl).or_else(|| {
                else_branch
                    .as_ref()
                    .and_then(|e| owed_let(None, e, live, producers, decl))
            }),
            _ => None,
        }
    }

    /// What `stmts` does to the binding `name`.
    ///
    /// `nested_loop` is whether `stmts` is, transitively, the body of a loop
    /// inside the declaring block. There a `break` returns control to the
    /// declaring block still owning the value; at the declaring block's level
    /// it abandons it.
    fn scan(stmts: &[Stmt], name: &str, nested_loop: bool) -> Scan {
        let mut acc = Scan::default();
        for (i, st) in stmts.iter().enumerate() {
            // Any mention of the binding in a statement's own expressions moves
            // it (`close(s)`, `for x in s`, `sink(s)`, `let t = s`), and a later
            // mention on the same path is a double disposal. `.0` is "some path
            // through this statement disposes it", `.1` "every path does"; they
            // differ only where a `match` or an if-expression branches.
            let none = (false, false);
            let moved = match st {
                // A write back into the binding is not a disposal: the binding
                // holds a value again when the statement ends. `pool.push(t)`
                // parses as `pool = @push(pool, t)` (`hoist_mutating_receiver`),
                // and must not discharge `pool`.
                Stmt::Assign { name: n, value, .. } if n == name => none,
                Stmt::Assign { value, .. }
                | Stmt::Let { value, .. }
                | Stmt::SetField { value, .. }
                | Stmt::Expr(value) => paths(value, name),
                Stmt::IndexSet { index, value, .. } => {
                    let (i, v) = (paths(index, name), paths(value, name));
                    (i.0 || v.0, i.1 || v.1)
                }
                Stmt::If { cond: e, .. }
                | Stmt::While { cond: e, .. }
                | Stmt::IfLet { scrutinee: e, .. } => paths(e, name),
                Stmt::ForIn { iter, .. } => paths(iter, name),
                Stmt::Drop { name: n, .. } => (n == name, n == name),
                Stmt::Return { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => none,
                Stmt::Region { .. } => none,
            };
            // One arm disposes it and another does not: one path is wrong
            // whatever follows, as with two disagreeing `if` blocks below.
            if moved.0 && !moved.1 {
                acc.leaked = true;
                return acc;
            }
            if moved.1 {
                acc.disposed = true;
                // Only the reachable rest: a mention after a diverging statement
                // is unreachable.
                let mut doubled = false;
                for s in &stmts[i + 1..] {
                    if stmt_mentions(s, name) {
                        doubled = true;
                        break;
                    }
                    if diverges(std::slice::from_ref(s)) {
                        break;
                    }
                }
                acc.doubled = doubled;
                // The caller's branch merge needs to know whether anything falls
                // out: `if c { close(s) return 1 }` disposes and diverges.
                acc.diverges = diverges(&stmts[i + 1..]);
                return acc;
            }
            match st {
                Stmt::Return { value, .. } => {
                    // Returning it is a disposal. `paths`, not `mentions`:
                    // `return match p { Some(n) => t, None => 0 }` forwards it on
                    // one path and abandons it on the other.
                    acc.diverges = true;
                    acc.leaked |= !value.as_ref().is_some_and(|e| paths(e, name).1);
                    return acc;
                }
                // At the declaring block's own level, `break`/`continue` abandon
                // an undisposed value as a bare `return` does.
                Stmt::Break { .. } | Stmt::Continue { .. } => {
                    acc.diverges = true;
                    acc.leaked |= !nested_loop;
                    return acc;
                }
                Stmt::If {
                    then_block,
                    else_block,
                    ..
                }
                | Stmt::IfLet {
                    then_block,
                    else_block,
                    ..
                } => {
                    let t = scan(&then_block.stmts, name, nested_loop);
                    let e = match else_block {
                        Some(b) => scan(&b.stmts, name, nested_loop),
                        None => Scan::default(),
                    };
                    acc.leaked |= t.leaked || e.leaked;
                    acc.doubled |= t.doubled || e.doubled;
                    match (t.diverges, e.diverges) {
                        (true, true) => {
                            acc.diverges = true;
                            return acc;
                        }
                        (true, false) => {
                            if e.disposed {
                                acc.disposed = true;
                                return acc;
                            }
                        }
                        (false, true) => {
                            if t.disposed {
                                acc.disposed = true;
                                return acc;
                            }
                        }
                        (false, false) => {
                            if t.disposed && e.disposed {
                                acc.disposed = true;
                                return acc;
                            }
                            // The branches disagree, so one path is wrong whatever
                            // follows: a leak, or a double disposal later. It is
                            // reported once, here.
                            acc.leaked |= t.disposed != e.disposed;
                        }
                    }
                }
                // A loop body may run zero times, and a disposal in it would
                // repeat on the next iteration, so any disposal there leaks.
                Stmt::While { body, .. } | Stmt::ForIn { body, .. } => {
                    let b = scan(&body.stmts, name, true);
                    acc.leaked |= b.leaked || b.disposed;
                    acc.doubled |= b.doubled;
                }
                Stmt::Region { body, .. } => {
                    let b = scan(&body.stmts, name, nested_loop);
                    acc.leaked |= b.leaked;
                    acc.doubled |= b.doubled;
                    if b.disposed || b.diverges {
                        acc.disposed = b.disposed;
                        acc.diverges = b.diverges;
                        return acc;
                    }
                }
                _ => {}
            }
        }
        acc
    }

    /// Whether every path out of `stmts` leaves via `return`, `break`,
    /// `continue` or `panic`. [`scan`] asks because it stops at the disposal
    /// and never reaches the `return` after it.
    fn diverges(stmts: &[Stmt]) -> bool {
        stmts.iter().any(|s| match s {
            Stmt::Return { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => true,
            Stmt::Expr(Expr::Call { name, .. }) => vyrn_frontend::ast::is_panic(name),
            Stmt::If {
                then_block,
                else_block,
                ..
            }
            | Stmt::IfLet {
                then_block,
                else_block,
                ..
            } => {
                diverges(&then_block.stmts)
                    && else_block.as_ref().is_some_and(|b| diverges(&b.stmts))
            }
            Stmt::Region { body, .. } => diverges(&body.stmts),
            _ => false,
        })
    }
}

/// The facts [`stores`] reads about the program's declarations.
pub struct StoreRules<'a> {
    /// Whether module state `g` was declared `mut`.
    pub global_mutable: &'a dyn Fn(&str) -> bool,
    /// Module state `g`'s declared type.
    pub global_ty: &'a dyn Fn(&str) -> Option<Type>,
    /// Whether a projection owns a type's element places, so a store through
    /// such a name is a store into its element whatever field `atSet` yields.
    pub projected: &'a dyn Fn(&Type) -> bool,
    /// The record type with a `where` rule that a path of places, taken from a
    /// value of the given type, passes through; `None` when it passes through
    /// none. The last place is the one stored into, so it does not count.
    pub ruled_within: &'a dyn Fn(&Type, &[&Place]) -> Option<String>,
}

/// Every store the reader may not write, as the sentence `vyrn check` gives
/// and its line, one per source statement. A store is a `St::Store`, a
/// module-state place passed to a `modify` argument, or a removal's receiver.
/// A store inside a value whose record type has a `where` rule is refused,
/// since the rule is checked where the value is built; so is a store into a
/// name the reader wrote without `mut`. A local name passed to a `modify`
/// argument is not a store here: `check_modify_arg` refuses it, and this pass
/// accepts it. A minted temporary (`@t`) is not the reader's. `seen` holds the
/// statements already refused, so the instances of one generic function refuse
/// a statement once.
pub fn stores(
    body: &Body,
    rules: &StoreRules,
    seen: &mut std::collections::HashSet<usize>,
) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    for f in body.frames() {
        each_store(&f.stmts, &f.names, &mut |place, line, site, removal| {
            // The first step out of the root decides the words.
            let mut step = None;
            let mut path = Vec::new();
            let mut at = place;
            while let Place::Field(b, _) | Place::Elem(b, _) | Place::Key(b, _) = at {
                step = Some(at);
                path.push(at);
                at = b;
            }
            path.reverse();
            let (source, ty) = match at {
                Place::Name(n) => {
                    let info = &f.names[*n as usize];
                    (&info.source, Some(info.ty.clone()))
                }
                Place::Global(g) => (g, (rules.global_ty)(g)),
                _ => return,
            };
            let ruled = ty.as_ref().and_then(|t| (rules.ruled_within)(t, &path));
            let (name, elem) = match at {
                _ if ruled.is_some() => (source, false),
                Place::Name(n) => {
                    let info = &f.names[*n as usize];
                    if info.mutable || info.source.starts_with('@') {
                        return;
                    }
                    (source, (rules.projected)(&info.ty))
                }
                Place::Global(g) if !(rules.global_mutable)(g) => (source, false),
                _ => return,
            };
            if site.is_some_and(|k| !seen.insert(k)) {
                return;
            }
            if let Some(n) = ruled {
                out.push((
                    line,
                    format!(
                        "cannot mutate a field of `{n}` in place (its `where` invariant could be \
                         broken mid-update); rebuild it: `{name} = {n} {{ .. }}`"
                    ),
                ));
                return;
            }
            let what = match (step, removal) {
                (None, Some(op)) => format!("cannot `{}` from", &op[1..]),
                (None, None) => "cannot assign to".into(),
                (Some(Place::Field(..)), _) if !elem => "cannot mutate a field of".into(),
                (Some(_), _) => "cannot store into".into(),
            };
            out.push((line, format!("{what} `{name}` (declared without `mut`)")));
        });
    }
    out
}

/// Every place `stmts` stores into, with its line, the source statement it
/// is keyed by where the row names one, and the builtin where the store is a
/// removal's receiver.
fn each_store(
    stmts: &[St],
    names: &[NameInfo],
    f: &mut dyn FnMut(&Place, usize, Option<usize>, Option<&str>),
) {
    fn modified(rhs: &Rhs) -> Vec<(Place, Option<&str>)> {
        match rhs {
            Rhs::Call { callee, args, .. } => {
                let removal = matches!(
                    crate::core::builtin_row(callee),
                    Some(crate::core::Spec::Removes)
                )
                .then_some(callee.as_str());
                args.iter()
                    .filter_map(|a| match a {
                        (Arg::Place(p), Capability::Modify) => Some((p.clone(), removal)),
                        (Arg::Val(Val::Name(n)), Capability::Modify) if removal.is_some() => {
                            Some((Place::Name(*n), removal))
                        }
                        _ => None,
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }
    for (s, _) in rows(stmts) {
        match s {
            St::Store {
                place, line, site, ..
            } => {
                let key = match site {
                    Site::Node(k) => Some(*k),
                    _ => None,
                };
                f(place, *line, key, None)
            }
            St::Do { rhs, line, site } => {
                let key = Some(*site).filter(|k| *k != 0);
                modified(rhs).iter().for_each(|(p, r)| f(p, *line, key, *r))
            }
            St::Let(n, rhs) => {
                let info = &names[*n as usize];
                modified(rhs)
                    .iter()
                    .for_each(|(p, r)| f(p, info.line, info.binding, *r))
            }
            _ => {}
        }
    }
}

/// Every `break` and `continue` with no loop around it in its own frame, as
/// the sentence `vyrn check` gives and its line. A lambda's body is a frame of
/// its own, so a loop outside the lambda does not count. `seen` is as in
/// [`stores`].
pub fn loops(body: &Body, seen: &mut std::collections::HashSet<usize>) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for f in body.frames() {
        for (s, _) in rows(&f.stmts).filter(|(_, depth)| *depth == 0) {
            let (what, site, line) = match s {
                St::Break { site, line } => ("break", site, line),
                St::Continue { site, line } => ("continue", site, line),
                _ => continue,
            };
            if seen.insert(*site) {
                out.push((*line, format!("`{what}` outside a loop")));
            }
        }
    }
    out
}

/// Every rule the builder met at its construct ([`Body::refused`], and
/// [`Body::mistyped`] where the body's types are `as_written`), as the
/// sentence `vyrn check` gives and its line: an unknown name, a condition
/// that is not Bool, a value its slot does not take. The caller states each
/// sentence once per line.
pub fn refused(body: &Body, as_written: bool) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for f in body.frames() {
        let mistyped = f.mistyped.iter().filter(|_| as_written);
        for (line, refusal) in f.refused.iter().chain(mistyped) {
            out.push((*line, refusal.clone()));
        }
    }
    out
}

/// Every `drop` a reader wrote that cannot release what it names, as the
/// sentence `vyrn check` gives and its line. A name no binding answers is module
/// state, which lives for the whole module, or no name at all. A bound name
/// needs a type that owns heap: a String, an array, a map, a built-in sum
/// that carries heap, or a type declaring `impl Owned`. A type parameter is
/// refused, because no instance check runs on the body. The types are read
/// as written, so the caller passes no instance of a generic function.
pub fn drops(
    body: &Body,
    program: &vyrn_frontend::ast::Program,
    decls: &HashMap<String, vyrn_frontend::ast::TypeDecl>,
) -> Vec<(usize, String)> {
    use vyrn_frontend::types;
    let mut out = Vec::new();
    for f in body.frames() {
        for (name, line) in &f.unbound_drops {
            out.push((
                *line,
                if program.globals.iter().any(|g| &g.name == name) {
                    format!(
                        "cannot `drop` module state `{name}` \u{2014} it lives for the whole \
                     module and is reclaimed at process exit"
                    )
                } else {
                    format!("`drop` of unbound variable `{name}`")
                },
            ));
        }
        let mut written = Vec::new();
        each_drop(&f.stmts, &mut written);
        for (n, line) in written {
            let info = &f.names[n as usize];
            let owned = types::type_key(&info.ty).is_some_and(|k| {
                program.impls.iter().any(|i| {
                    i.protocol == types::OWNED && types::type_key(&i.ty).as_ref() == Some(&k)
                })
            });
            let t = types::resolve(&info.ty, decls);
            let heap = matches!(
                t,
                Type::Str | Type::Array(_) | Type::SmallArray(..) | Type::Map(..)
            ) || (types::is_sum_alias(&t)
                && vyrn_frontend::declared::owns_heap(&t, decls));
            // `Err` is a name the checker could not type, and its refusal
            // is the unknown name's (`refused`).
            if owned || heap || t == Type::Err {
                continue;
            }
            let name = &info.source;
            out.push((
                line,
                if matches!(t, Type::Param(_)) {
                    format!(
                        "cannot `drop` `{name}`: its type `{t}` is a type parameter, so this \
                     body cannot know whether the rule below holds for the instance \u{2014} a \
                     plain record would be released here where `drop` on it directly is \
                     refused. Release the value where its concrete type is known, or \
                     `consume` the heap field and `drop` that"
                    )
                } else {
                    format!(
                        "`drop` needs a heap value (a String, an Array, a Map, a Ref, \
                     or an Option/Result carrying one, or a type declaring `impl Owned`), but \
                     `{name}` is {t}"
                    )
                },
            ));
        }
    }
    out
}

fn each_drop(stmts: &[St], out: &mut Vec<(Name, usize)>) {
    for s in stmts {
        match s {
            St::Drop(n, _, line, _) if *line > 0 => out.push((*n, *line)),
            St::If { then, els, .. } => {
                each_drop(then, out);
                each_drop(els, out);
            }
            St::Loop { body, .. } | St::Block { body, .. } => each_drop(body, out),
            St::Switch { arms, .. } => arms.iter().for_each(|a| each_drop(&a.body, out)),
            _ => {}
        }
    }
}
