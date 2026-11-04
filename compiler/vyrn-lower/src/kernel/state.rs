//! The kernel's state at one point of its walk, and the fork a branch makes
//! of it.
//!
//! A [`State`] is a stack of layers: its own entries over the state it forked
//! from. An `if` or a `match` shares its entry ([`Fork::new`]) and walks each
//! arm in a child of it ([`Fork::arm`]), so an arm costs what it changes, not
//! the names in scope. The join's result is a child of the entry too, and
//! [`Fork::join`] writes it back into the entry. A read looks a name up layer
//! by layer, so it costs the forks open around the walk. Two states that
//! share a layer can differ only in the layers above it ([`State::differs`]).

use std::rc::Rc;

use vyrn_frontend::core::Name;
use vyrn_frontend::diagnostics::Fix;

use super::{Alias, By, Own, Taker};

/// What consumed a name, for a refusal's wording only: the line, the taker,
/// and the edit that copies the name there.
pub(super) type Taken = (usize, By, Taker, Option<Fix>);

/// Where a hole was taken: `(name, path, line, the edits that copy the path
/// in the take's place)`.
pub(super) type HoleTaken = (Name, String, usize, Vec<Fix>);

/// One layer's entries of a table, ascending by name: `None` removes the
/// parent's entry. The root layer holds no `None`.
#[derive(Clone, Debug)]
struct Table<V>(Vec<(Name, Option<V>)>);

impl<V> Default for Table<V> {
    fn default() -> Self {
        Table(Vec::new())
    }
}

impl<V> Table<V> {
    fn get(&self, n: Name) -> Option<&Option<V>> {
        let i = self.0.binary_search_by_key(&n, |e| e.0).ok()?;
        Some(&self.0[i].1)
    }

    /// Sets `n`'s entry; `None` removes it. The root has no parent to
    /// shadow, so a removal there drops the entry.
    fn put(&mut self, root: bool, n: Name, v: Option<V>) {
        match (self.0.binary_search_by_key(&n, |e| e.0), v) {
            (Ok(i), None) if root => {
                self.0.remove(i);
            }
            (Ok(i), v) => self.0[i].1 = v,
            (Err(_), None) if root => {}
            (Err(i), v) => self.0.insert(i, (n, v)),
        }
    }

    fn keys(&self) -> impl Iterator<Item = Name> + '_ {
        self.0.iter().map(|e| e.0)
    }
}

/// The facts at one point of the walk: every owned name's [`Own`], what took
/// it, its holes, the aliases and the dead aliases, plus whether the path has
/// ended.
#[derive(Clone, Debug, Default)]
pub(super) struct State {
    /// The state this one forked from; `None` at the root.
    parent: Option<Rc<State>>,
    /// Every name not [`Own::Gone`]: gone is absence, static is
    /// [`Own::Static`].
    held: Table<Own>,
    /// What consumed each name.
    taker: Table<Taken>,
    /// The sub-places taken out of each held name, sorted.
    holes: Table<Vec<String>>,
    /// Where each hole was taken, appended in this layer. Wording only.
    taken_at: Vec<HoleTaken>,
    /// For an alias whose place was written: the line and the place. A later
    /// read is refused.
    dead: Table<(usize, String)>,
    /// What each alias reads, set by the `let` that reads the place or the
    /// store that rebinds a borrow's binding.
    alias: Table<Alias>,
    /// The path returned, broke, continued or trapped: it reaches no join.
    pub(super) ended: bool,
}

/// The state at a branch, shared by the states its arms walk.
pub(super) struct Fork(Rc<State>);

impl Fork {
    /// Takes `st` as the entry of a branch; `st` is empty until
    /// [`Fork::join`] or [`Fork::undo`] gives it back.
    pub(super) fn new(st: &mut State) -> Fork {
        Fork(Rc::new(std::mem::take(st)))
    }

    pub(super) fn entry(&self) -> &State {
        &self.0
    }

    /// The state an arm starts from: a child of the entry.
    pub(super) fn arm(&self) -> State {
        State {
            parent: Some(Rc::clone(&self.0)),
            ended: self.0.ended,
            ..State::default()
        }
    }

    /// The state after the branch, from the join of its arms' states.
    pub(super) fn join(self, joined: State) -> State {
        drop(self.0);
        joined.fold()
    }

    /// The entry as it was at the fork, for a branch an arm refused.
    pub(super) fn undo(self) -> State {
        unshare(self.0)
    }
}

impl State {
    /// Writes a child of an entry back into the entry: the result has the
    /// entry's layers and this state's facts. The entry is copied only if a
    /// state the walk kept (a `break`'s) still shares it.
    fn fold(mut self) -> State {
        let Some(parent) = self.parent.take() else {
            return self;
        };
        let mut entry = unshare(parent);
        entry.absorb(self);
        entry
    }

    /// Folds layers until this state's parent is `base`'s, or it is the
    /// root: a state built from states a loop kept is as deep as the loop's
    /// entry again.
    pub(super) fn settle(mut self, base: &State) -> State {
        while self.parent.is_some() && !same(&self.parent, &base.parent) {
            self = self.fold();
        }
        self
    }

    /// Applies `layer`'s entries over this state's own, for [`State::fold`].
    fn absorb(&mut self, layer: State) {
        let root = self.parent.is_none();
        for (n, v) in layer.held.0 {
            self.held.put(root, n, v);
        }
        for (n, v) in layer.taker.0 {
            self.taker.put(root, n, v);
        }
        for (n, v) in layer.holes.0 {
            self.holes.put(root, n, v);
        }
        for (n, v) in layer.dead.0 {
            self.dead.put(root, n, v);
        }
        for (n, v) in layer.alias.0 {
            self.alias.put(root, n, v);
        }
        self.taken_at.extend(layer.taken_at);
        self.ended = layer.ended;
    }

    /// This state and the states it forked from, closest first.
    fn layers(&self) -> impl Iterator<Item = &State> {
        std::iter::successors(Some(self), |s| s.parent.as_deref())
    }

    /// The entry for `n` in the table `of` reads, from the closest layer that
    /// has one.
    fn get<V>(&self, of: fn(&State) -> &Table<V>, n: Name) -> Option<&V> {
        let layer = self.layers().find_map(|s| of(s).get(n))?;
        layer.as_ref()
    }

    /// The layers of `self` above the closest layer it shares with `other`.
    fn above<'s>(&'s self, other: &State) -> impl Iterator<Item = &'s State> {
        let depth = |s: &State| s.layers().count();
        let (mine, theirs) = (depth(self), depth(other));
        let (mut a, mut b) = (self.layers(), other.layers());
        a.by_ref().take(mine.saturating_sub(theirs)).for_each(drop);
        b.by_ref().take(theirs.saturating_sub(mine)).for_each(drop);
        let shared = a.zip(b).find(|(x, y)| std::ptr::eq(*x, *y)).map(|(x, _)| x);
        self.layers()
            .take_while(move |s| !shared.is_some_and(|x| std::ptr::eq(*s, x)))
    }

    /// Every name `keys` lists from a layer of `self` or `other` above the one
    /// they share, ascending: any other name reads alike in both.
    fn touched(&self, other: &State, keys: impl Fn(&State, &mut Vec<Name>)) -> Vec<Name> {
        let mut out = Vec::new();
        for s in self.above(other).chain(other.above(self)) {
            keys(s, &mut out);
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    pub(super) fn own(&self, n: Name) -> Own {
        self.get(|s| &s.held, n).copied().unwrap_or(Own::Gone)
    }

    pub(super) fn set_own(&mut self, n: Name, o: Own) {
        if self.own(n) != o {
            let root = self.parent.is_none();
            self.held.put(root, n, (o != Own::Gone).then_some(o));
        }
    }

    /// The names not gone, ascending, which is creation order.
    pub(super) fn live(&self) -> Vec<Name> {
        let mut out: Vec<Name> = (self.layers()).flat_map(|s| s.held.keys()).collect();
        out.sort_unstable();
        out.dedup();
        out.retain(|n| self.own(*n) != Own::Gone);
        out
    }

    pub(super) fn taker(&self, n: Name) -> Option<&Taken> {
        self.get(|s| &s.taker, n)
    }

    pub(super) fn set_taker(&mut self, n: Name, t: Option<Taken>) {
        if t.is_some() || self.taker(n).is_some() {
            self.taker.put(self.parent.is_none(), n, t);
        }
    }

    /// The holes taken out of `n`, sorted.
    pub(super) fn holes(&self, n: Name) -> &[String] {
        self.get(|s| &s.holes, n).map_or(&[], Vec::as_slice)
    }

    pub(super) fn add_hole(&mut self, n: Name, path: String) {
        let mut hs = self.holes(n).to_vec();
        hs.push(path);
        hs.sort();
        self.holes.put(self.parent.is_none(), n, Some(hs));
    }

    /// Keeps the holes of `n` that `keep` accepts.
    pub(super) fn keep_holes(&mut self, n: Name, keep: impl Fn(&str) -> bool) {
        if self.holes(n).iter().all(|h| keep(h)) {
            return;
        }
        let hs: Vec<String> = (self.holes(n).iter())
            .filter(|h| keep(h))
            .cloned()
            .collect();
        let root = self.parent.is_none();
        self.holes.put(root, n, (!hs.is_empty()).then_some(hs));
    }

    pub(super) fn hole_taken(&mut self, at: HoleTaken) {
        self.taken_at.push(at);
    }

    /// The last take of the hole `path` in `n`.
    pub(super) fn hole_take(&self, n: Name, path: &str) -> Option<&HoleTaken> {
        (self.layers())
            .find_map(|s| (s.taken_at.iter().rev()).find(|(h, p, ..)| *h == n && p == path))
    }

    pub(super) fn dead(&self, n: Name) -> Option<&(usize, String)> {
        self.get(|s| &s.dead, n)
    }

    pub(super) fn set_dead(&mut self, n: Name, d: Option<(usize, String)>) {
        if d.is_some() || self.dead(n).is_some() {
            self.dead.put(self.parent.is_none(), n, d);
        }
    }

    pub(super) fn alias(&self, n: Name) -> Option<&Alias> {
        self.get(|s| &s.alias, n)
    }

    pub(super) fn set_alias(&mut self, n: Name, a: Option<Alias>) {
        if a.is_some() || self.alias(n).is_some() {
            self.alias.put(self.parent.is_none(), n, a);
        }
    }

    /// Every alias, in no fixed order: each layer's entries that no closer
    /// layer shadows.
    pub(super) fn aliases(&self) -> impl Iterator<Item = (Name, &Alias)> {
        (self.layers().enumerate()).flat_map(move |(i, s)| {
            (s.alias.0.iter()).filter_map(move |(n, a)| {
                let shadowed = self.layers().take(i).any(|c| c.alias.get(*n).is_some());
                Some((*n, a.as_ref().filter(|_| !shadowed)?))
            })
        })
    }

    /// Every name whose facts differ between `self` and `other`, ascending:
    /// its [`Own`], whether a taker took it, or its holes. A name both states
    /// have alike agrees at a join and a back edge, and changes nothing at a
    /// join or an edge.
    pub(super) fn differs(&self, other: &State) -> Vec<Name> {
        let mut out = self.touched(other, |s, out| {
            out.extend(s.held.keys());
            out.extend(s.taker.keys());
            out.extend(s.holes.keys());
        });
        out.retain(|n| {
            self.own(*n) != other.own(*n)
                || self.taker(*n).is_some() != other.taker(*n).is_some()
                || self.holes(*n) != other.holes(*n)
        });
        out
    }

    /// Adds every alias and dead alias `other` has and `self` lacks: an alias
    /// ended or bound on any edge of a join is so after it.
    pub(super) fn merge_aliases(&mut self, other: &State) {
        for n in self.touched(other, |s, out| out.extend(s.dead.keys())) {
            if let (None, Some(d)) = (self.dead(n), other.dead(n)) {
                let d = d.clone();
                self.set_dead(n, Some(d));
            }
        }
        for n in self.touched(other, |s, out| out.extend(s.alias.keys())) {
            if let (None, Some(a)) = (self.alias(n), other.alias(n)) {
                let a = a.clone();
                self.set_alias(n, Some(a));
            }
        }
    }

    /// Widens a loop's entry by the back edge `at`: `Static` to `Held`, and
    /// an alias ended there stays ended. Returns whether anything changed.
    pub(super) fn widen(&mut self, at: &State) -> bool {
        let mut changed = false;
        for n in self.touched(at, |s, out| out.extend(s.held.keys())) {
            if self.own(n) == Own::Static && at.own(n) == Own::Held {
                self.set_own(n, Own::Held);
                changed = true;
            }
        }
        for n in self.touched(at, |s, out| out.extend(s.dead.keys())) {
            if let (None, Some(d)) = (self.dead(n), at.dead(n)) {
                let d = d.clone();
                self.set_dead(n, Some(d));
                changed = true;
            }
        }
        changed
    }
}

/// The state behind `st`, copied only if another state still shares it.
fn unshare(st: Rc<State>) -> State {
    Rc::try_unwrap(st).unwrap_or_else(|shared| (*shared).clone())
}

/// Whether two parents are the same layer.
fn same(a: &Option<Rc<State>>, b: &Option<Rc<State>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Rc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}
