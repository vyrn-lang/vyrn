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

use vyrn_frontend::ast::{NodeId, Type};

use vyrn_frontend::ast::Capability;
use vyrn_frontend::core::check::Guard;
use vyrn_frontend::core::{
    rows, Arg, Body, Callee, Name, NameInfo, Place, Rhs, Site, St, Use, Val,
};

use vyrn_frontend::rule;
use vyrn_frontend::rules::Rule;

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
                let info = &self.body.names[n.index()];
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
                        Val::Name(n) => Some(self.body.names[n.index()].ty.clone()),
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
                Rhs::Val(Val::Name(n)) => self.body.names[n.index()].source.clone(),
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
            Rhs::Val(Val::Name(n)) => Some(self.body.names[n.index()].ty.clone()),
            Rhs::Read(p) | Rhs::Take(p) => self.place_ty(p),
            Rhs::Call { ret, .. } => ret.clone(),
            Rhs::Prim(_, _, ty) => ty.clone(),
            Rhs::Make(..) | Rhs::Val(Val::Lit(_)) => None,
        }
    }

    fn place_ty(&mut self, p: &Place) -> Option<Type> {
        match p {
            Place::Name(n) => Some(self.body.names[n.index()].ty.clone()),
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
            Place::Name(n) => self.body.names[n.index()].source.clone(),
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
    /// The record type a store along a path of places, taken from a record
    /// name of that type, belongs to a group of ([`groups`]); `None` where
    /// the store is no group's.
    pub grouped: &'a dyn Fn(&Type, &[&Place]) -> Option<String>,
}

/// Every store the reader may not write, as the sentence `vyrn check` gives
/// and its line, one per source statement. A store is a `St::Store`, a
/// module-state place passed to a `modify` argument, or a removal's receiver.
/// A store inside a value whose record type has a `where` rule is refused,
/// since the rule is checked where the value is built, unless it belongs to a
/// group, which [`groups`] judges; so is a store into a name the reader wrote
/// without `mut`. A local name passed to a `modify` argument is not a store
/// here: `check_modify_arg` refuses it, and this pass accepts it. A minted
/// temporary (`@t`) is not the reader's. `seen` holds the statements already
/// refused, so the instances of one generic function refuse a statement once.
pub fn stores(
    body: &Body,
    rules: &StoreRules,
    seen: &mut std::collections::HashSet<NodeId>,
) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    for f in body.frames() {
        let mut judge = |place: &Place, line, site: Option<NodeId>, removal: Option<&str>| {
            let (at, path) = split(place);
            // The first step out of the root decides the words.
            let step = path.first().copied();
            let (source, ty) = match at {
                Place::Name(n) => {
                    let info = &f.names[n.index()];
                    (info.source.as_str(), Some(info.ty.clone()))
                }
                Place::Global(g) => (f.spelled(g), (rules.global_ty)(g)),
                _ => return,
            };
            let ruled = ty.as_ref().and_then(|t| (rules.ruled_within)(t, &path));
            let grouped = matches!(at, Place::Name(_))
                && ty
                    .as_ref()
                    .is_some_and(|t| (rules.grouped)(t, &path).is_some());
            let ruled = ruled.filter(|_| !grouped);
            let (name, elem) = match at {
                _ if ruled.is_some() => (source, false),
                Place::Name(n) => {
                    let info = &f.names[n.index()];
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
            let rule = match (ruled, step, removal) {
                (Some(n), ..) => rule!(StoreRuled, n, name),
                (_, None, Some(op)) => rule!(RemoveNotMut, op = &op[1..], name),
                (_, None, None) => rule!(AssignNotMut, name),
                (_, Some(Place::Field(..)), _) if !elem => rule!(FieldNotMut, name),
                (_, Some(_), _) => rule!(StoreNotMut, name),
            };
            out.push((line, rule.render()));
        };
        rows(&f.stmts).for_each(|(s, _)| row_stores(s, &f.names, &mut judge));
    }
    out
}

/// The root of `place` and the places from the root's first step to `place`
/// itself, outermost first.
pub(crate) fn split(place: &Place) -> (&Place, Vec<&Place>) {
    let mut path = Vec::new();
    let mut at = place;
    while let Place::Field(b, _) | Place::Elem(b, _) | Place::Key(b, _) = at {
        path.push(at);
        at = b;
    }
    path.reverse();
    (at, path)
}

/// Every place the one row `s` stores into, without the rows it holds, with
/// its line, the source statement it is keyed by where the row names one, and
/// the builtin where the store is a removal's receiver.
pub(crate) fn row_stores(
    s: &St,
    names: &[NameInfo],
    f: &mut dyn FnMut(&Place, usize, Option<NodeId>, Option<&str>),
) {
    fn modified(rhs: &Rhs) -> Vec<(Place, Option<&str>)> {
        match rhs {
            Rhs::Call { callee, args, .. } => {
                // A routed row is never a removal, so the host is not read.
                let removal = matches!(
                    crate::core::builtin_row(callee, false),
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
            let key = Some(*site).filter(|k| *k != NodeId::NONE);
            modified(rhs).iter().for_each(|(p, r)| f(p, *line, key, *r))
        }
        St::Let(n, rhs) => {
            let info = &names[n.index()];
            modified(rhs)
                .iter()
                .for_each(|(p, r)| f(p, info.line, info.binding, *r))
        }
        _ => {}
    }
}

/// Every store into a field of a record name with a `where` rule that its
/// group does not license, as the sentence `vyrn check` gives and its line.
///
/// The builder ends each group with the row that checks the rule
/// ([`Rhs::checks_rule`]). On each path from a store to that row no row reads
/// the record whole. When the record is the caller's (a `modify` parameter),
/// no row calls a function and no `return` or `?` leaves. No `break` or
/// `continue` leaves at all, and a loop checks what it stored before it
/// turns. A trap ends the program and a return releases a record the frame
/// owns, so neither shows the record again. The rows after an ended path are
/// judged as a path of their own.
///
/// `refuted` holds, per frame in [`Body::frames`] order, the rule checks
/// [`crate::elide::refuted`] proved to fail, over `body` with its check rows
/// stated; each is refused at its group's first store. Also answers whether a
/// check ends a group, the one case where `refuted` can hold anything.
pub fn groups(
    body: &Body,
    rules: &StoreRules,
    refuted: &[Vec<crate::elide::Refuted>],
) -> (Vec<(usize, String)>, bool) {
    let mut out = Vec::new();
    let mut ended = false;
    for (i, f) in body.frames().into_iter().enumerate() {
        let mut g = Groups {
            f,
            rules,
            refuted: refuted.get(i).map_or(&[], |r| r.as_slice()),
            ended: &mut ended,
            out: &mut out,
        };
        let left = g.walk(&f.stmts, Vec::new());
        g.unchecked(&left);
    }
    (out, ended)
}

/// A group from its first store to its check: the record name, its type, and
/// the line of the first store.
#[derive(Clone)]
struct Open {
    name: Name,
    ty: String,
    line: usize,
}

struct Groups<'b, 'r> {
    f: &'b Body,
    rules: &'b StoreRules<'r>,
    refuted: &'b [crate::elide::Refuted],
    ended: &'b mut bool,
    out: &'b mut Vec<(usize, String)>,
}

impl Groups<'_, '_> {
    /// The groups still open after `ss`, given those open before.
    fn walk(&mut self, ss: &[St], mut open: Vec<Open>) -> Vec<Open> {
        for s in ss {
            match s {
                St::If { then, els, .. } => {
                    self.observe(s, &open);
                    let a = self.walk(then, open.clone());
                    let b = self.walk(els, open.clone());
                    open = union(a, b);
                }
                St::Switch { arms, .. } => {
                    self.observe(s, &open);
                    let ends = arms.iter().map(|a| self.walk(&a.body, open.clone()));
                    open = ends.reduce(union).unwrap_or(open);
                }
                St::Block { body, .. } => open = self.walk(body, open),
                St::Loop { body, .. } => {
                    let end = self.walk(body, open.clone());
                    let turned = end
                        .iter()
                        .filter(|e| !open.iter().any(|o| o.name == e.name));
                    self.unchecked(&turned.cloned().collect::<Vec<_>>());
                }
                St::Break { line, .. } | St::Continue { line, .. } => {
                    let what = if matches!(s, St::Break { .. }) {
                        "break"
                    } else {
                        "continue"
                    };
                    for o in &open {
                        let name = self.src(o.name);
                        self.refuse(*line, rule!(GroupExit, what, name));
                    }
                    open.clear();
                }
                St::Return { is_try, line, .. } => {
                    let what = if *is_try { "?" } else { "return" };
                    let callers: Vec<Name> = (open.iter().map(|o| o.name))
                        .filter(|n| self.callers(*n))
                        .collect();
                    for n in callers {
                        let name = self.src(n);
                        self.refuse(*line, rule!(GroupExit, what, name));
                    }
                    open.clear();
                }
                St::Trap => open.clear(),
                St::Check(c) => {
                    let Guard::Rule(r) = c.guard else { continue };
                    let fails = self.refuted.iter().find(|(at, ..)| *at == c.site);
                    let group = open.iter().find(|o| o.name == r);
                    if let (Some((_, long, short)), Some(o)) = (fails, group) {
                        let (name, k, n) = (self.src(r), c.site.line, &o.ty);
                        self.refuse(o.line, rule!(GroupFalse, name, k, long, short, n));
                    }
                }
                _ => {
                    let checked = match s {
                        St::Do { rhs, .. } => rhs.checks_rule(&self.f.names),
                        _ => None,
                    };
                    if let Some(c) = checked {
                        *self.ended |= open.iter().any(|o| o.name == c);
                        open.retain(|o| o.name != c);
                        continue;
                    }
                    self.observe(s, &open);
                    row_stores(s, &self.f.names, &mut |place, line, _, _| {
                        let (Place::Name(c), path) = split(place) else {
                            return;
                        };
                        let ty = &self.f.names[c.index()].ty;
                        if let Some(ty) = (self.rules.grouped)(ty, &path) {
                            if !open.iter().any(|o| o.name == *c) {
                                open.push(Open { name: *c, ty, line });
                            }
                        }
                    });
                }
            }
        }
        open
    }

    /// Refuses a read of an open record whole in the one row `s`, and a call
    /// to a function while a caller's record is open.
    fn observe(&mut self, s: &St, open: &[Open]) {
        let line = row_line(s, &self.f.names);
        for o in open {
            let name = self.src(o.name);
            if reads_whole(s, o.name) {
                self.refuse(line, rule!(GroupRead, name));
            }
            if let Some(f) = self.called(s).filter(|_| self.callers(o.name)) {
                self.refuse(line, rule!(GroupCall, f, name));
            }
        }
    }

    /// The function the row calls, in the reader's words: a declared
    /// function, a method, a projection, a function value, or a builtin
    /// handed a function value.
    fn called(&self, s: &St) -> Option<String> {
        let (St::Let(
            _,
            Rhs::Call {
                callee, kind, args, ..
            },
        )
        | St::Do {
            rhs: Rhs::Call {
                callee, kind, args, ..
            },
            ..
        }) = s
        else {
            return None;
        };
        let takes_fn = args.iter().any(|(a, _)| {
            matches!(a, Arg::Val(Val::Name(n)) if matches!(self.f.names[n.index()].ty, Type::Fn(..)))
        });
        let user = matches!(
            kind,
            Callee::Fn(_) | Callee::Bound | Callee::Method | Callee::Projection | Callee::Value(_)
        );
        (user || takes_fn).then(|| match kind {
            Callee::Value(n) => self.f.names[n.index()].source.clone(),
            _ => callee.clone(),
        })
    }

    /// Whether `n` names a record the caller holds: a `modify` parameter, or
    /// a second name for one.
    fn callers(&self, n: Name) -> bool {
        let info = &self.f.names[n.index()];
        info.borrow || info.borrow_kind.is_some()
    }

    /// Refuses each group in `open`: its store has no check on its path.
    fn unchecked(&mut self, open: &[Open]) {
        for o in open {
            let (n, name) = (&o.ty, self.src(o.name));
            self.refuse(o.line, rule!(StoreRuled, n, name));
        }
    }

    fn src(&self, n: Name) -> String {
        self.f.names[n.index()].source.clone()
    }

    fn refuse(&mut self, line: usize, rule: Rule) {
        let u = (line, rule.render());
        if !self.out.contains(&u) {
            self.out.push(u);
        }
    }
}

/// Every group open on either of two paths.
fn union(mut a: Vec<Open>, b: Vec<Open>) -> Vec<Open> {
    for o in b {
        if !a.iter().any(|x| x.name == o.name) {
            a.push(o);
        }
    }
    a
}

/// Whether the one row `s` reads `c` whole: as a value, or as a place that is
/// `c` itself. A release is no read: it runs where the path leaves the frame.
fn reads_whole(s: &St, c: Name) -> bool {
    let mut hit = false;
    s.operands(&mut |v, u| {
        hit |= *v == Val::Name(c) && !matches!(u, Use::Root | Use::Release);
    });
    let whole = |p: &Place| *p == Place::Name(c);
    let rhs_whole = |r: &Rhs| match r {
        Rhs::Read(p) | Rhs::Take(p) => whole(p),
        Rhs::Call { args, .. } => args
            .iter()
            .any(|(a, _)| matches!(a, Arg::Place(p) if whole(p))),
        _ => false,
    };
    hit || match s {
        St::Let(_, r) | St::Do { rhs: r, .. } => rhs_whole(r),
        St::Store { place, .. } => whole(place),
        _ => false,
    }
}

/// The source line of the one row `s`; 0 where it states none.
fn row_line(s: &St, names: &[NameInfo]) -> usize {
    match s {
        St::Let(n, _) => names[n.index()].line,
        St::Do { line, .. }
        | St::Store { line, .. }
        | St::Return { line, .. }
        | St::Break { line, .. }
        | St::Continue { line, .. }
        | St::Switch { line, .. } => *line,
        _ => 0,
    }
}

/// Every `break` and `continue` with no loop around it in its own frame, as
/// the sentence `vyrn check` gives and its line. A lambda's body is a frame of
/// its own, so a loop outside the lambda does not count. `seen` is as in
/// [`stores`].
pub fn loops(body: &Body, seen: &mut std::collections::HashSet<NodeId>) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for f in body.frames() {
        for (s, _) in rows(&f.stmts).filter(|(_, depth)| *depth == 0) {
            let (what, site, line) = match s {
                St::Break { site, line } => ("break", site, line),
                St::Continue { site, line } => ("continue", site, line),
                _ => continue,
            };
            if seen.insert(*site) {
                out.push((*line, rule!(OutsideLoop, what).render()));
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
            let global = program.globals.iter().any(|g| &g.name == name);
            let name = f.spelled(name);
            let rule = match global {
                true => rule!(DropModuleState, name),
                false => rule!(DropUnbound, name),
            };
            out.push((*line, rule.render()));
        }
        let written = rows(&f.stmts).filter_map(|(s, _)| match s {
            St::Drop(n, _, line, _) if *line > 0 => Some((*n, *line)),
            _ => None,
        });
        for (n, line) in written {
            let info = &f.names[n.index()];
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
            let (name, param) = (&info.source, matches!(t, Type::Param(_)));
            let t = body.speech().ty(&t).to_string();
            let rule = match param {
                true => rule!(DropTypeParam, name, t),
                false => rule!(DropNotHeap, name, t),
            };
            out.push((line, rule.render()));
        }
    }
    out
}
