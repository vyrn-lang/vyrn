//! The typed judgment's `vyrn check` rules over the named core: [`stores`]
//! into a place not declared `mut` or under a `where` rule, the [`groups`] of
//! stores into a record with a `where` rule, a `break` or `continue` outside
//! a loop ([`loops`]), the rules the builder met at its construct
//! ([`refused`]), and a `drop` that cannot release what it names ([`drops`]).

use std::collections::HashMap;

use vyrn_frontend::ast::{NodeId, Type};

use vyrn_frontend::ast::Capability;
use vyrn_frontend::core::check::Guard;
use vyrn_frontend::core::{
    rows, Arg, Body, Callee, Name, NameInfo, Place, Rhs, Site, St, Use, Val,
};

use vyrn_frontend::rule;
use vyrn_frontend::rules::Rule;

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
