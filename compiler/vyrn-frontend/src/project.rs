//! Place projections: a `place` member yields a place inside its
//! receiver and is inlined at the access site, so the borrow never leaves the
//! caller's frame. [`Expansions::site`] expands `a[i]` (a call to [`AT`]) and
//! [`Expansions::store_index`] expands `a[i] = v`; a builtin container keeps
//! its own nodes and lowers to [`ELEM`], the unspellable addressing primitive.
//! A shared [`Expansions`] expands each site once, because the checker, the
//! ownership passes and the lowering key side tables by node.

use crate::ast::{
    Block, Expr, Function, Id, ImplBlock, NodeId, Numbering, Program, Stmt, Type, TypeDecl,
};
use crate::types::Impls;
use std::collections::{HashMap, HashSet};

/// The element-place primitive `@slot(container, index)`: the only indexing
/// the backends know by name. Unspellable.
pub const ELEM: &str = "@slot";

/// The element-access dispatch `@at(container, index)` that `a[i]` and
/// `x.at(i)` parse to. Unspellable, so the checker can refuse a free `at(..)`
/// written in source. It dispatches to the method named `at`.
pub const AT: &str = "@at";

/// A field read or an element read rooted in a name: a place, not a value the
/// reader owns. A part of a call's result (`mk().q.s`) is none.
pub fn is_place_read(e: &Expr) -> bool {
    match e {
        Expr::Field { expr, .. } => is_place_read(expr),
        Expr::Call { name, args, .. } => name == AT && args.len() == 2 && is_place_read(&args[0]),
        Expr::Var { .. } => true,
        _ => false,
    }
}

/// One access site's lowering: statements to run first, then the place.
#[derive(Debug, Clone)]
pub struct Projection {
    /// The body's statements before the `yield`, substituted, run in the
    /// caller's frame.
    pub prologue: Vec<Stmt>,
    /// The yielded place, substituted: a variable, a field of one, or
    /// [`ELEM`].
    pub place: Expr,
}

/// Whether `ty` indexes through the seeded row rather than a user `impl`. One
/// pattern match, because every `a[i]` asks it.
pub fn is_builtin_container(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Array(_)
            | Type::ArrayN(..)
            | Type::SmallArray(..)
            | Type::Str
            | Type::Map(..)
            | Type::Err
    )
}

/// The projection expansions of one compile: one tree per access site, which
/// the checker, the ownership passes, the lowering and the emitter all walk.
/// [`crate::loader::LoadOptions`] carries it and the loader stamps it on the
/// program it links. A generator's load gets a table of its own, because a
/// generator program's node ids repeat its importer's. Trees are leaked,
/// because passes key side tables by their node ids.
///
/// A tree's ids and temporary names derive from its site. It is numbered in
/// the site's [`NodeId::expansion_unit`], after the trees made there before
/// it. One thread types a unit's body, in the body's order, so the ids do not
/// depend on the thread count. The lowering's walks and the placer's parallel
/// builds only read trees: they run under [`Expansions::seal`]. An unshared
/// table ([`Expansions::default`], the editor's) keeps no tree: each ask
/// builds one, [`Expansions::schema`] answers none, and the lowering inlines
/// no site.
#[derive(Default)]
pub struct Expansions {
    shared: bool,
    tables: std::sync::RwLock<Tables>,
}

impl Expansions {
    /// A table that keeps every tree for the later asks, as a compile needs.
    pub fn shared() -> std::sync::Arc<Expansions> {
        std::sync::Arc::new(Expansions {
            shared: true,
            tables: Default::default(),
        })
    }

    /// Whether this table keeps its trees ([`Expansions::shared`]).
    pub fn is_shared(&self) -> bool {
        self.shared
    }

    /// Forbids making a tree until the guard drops, for a section that builds
    /// bodies on many threads: there a tree's ids would depend on the thread
    /// order, so a site typing did not expand panics instead. An unshared
    /// table stays open: each ask builds a tree of its own, and no pass
    /// lowers through one, so its ids reach no output.
    pub fn seal(&self) -> Sealed<'_> {
        let was = std::mem::replace(&mut self.write().sealed, self.shared);
        Sealed { table: self, was }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Tables> {
        self.tables
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Tables> {
        self.tables
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Unseals what [`Expansions::seal`] sealed when it drops.
pub struct Sealed<'a> {
    table: &'a Expansions,
    was: bool,
}

impl Drop for Sealed<'_> {
    fn drop(&mut self) {
        self.table.write().sealed = self.was;
    }
}

/// Every program's table answers alike: a program's `Debug` text keys the
/// generator engine's artifacts, and its equality compares source.
impl std::fmt::Debug for Expansions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Expansions")
    }
}

impl PartialEq for Expansions {
    fn eq(&self, _: &Expansions) -> bool {
        true
    }
}

/// An access site: its anchor node, receiver type key, member name. A node
/// names one site because each program has a table of its own.
type Key = (NodeId, String, String);

#[derive(Default)]
struct Tables {
    sites: HashMap<Key, &'static Projection>,
    optional: HashMap<Key, &'static OptionalProjection>,
    /// Store expansions, keyed by the index node: `a[i] = v` has no receiver
    /// node, only the temporary [`Expansions::store_index`] synthesizes.
    stores: HashMap<NodeId, &'static Block>,
    /// The `Schema` literal each `schemaOf<T>()` node stands for, keyed by
    /// the call node. See [`Expansions::schema`].
    schemas: HashMap<NodeId, &'static Expr>,
    /// How many nodes each expansion unit holds.
    used: HashMap<u32, u32>,
    /// Whether [`Expansions::seal`] holds the table: no tree may be made.
    sealed: bool,
}

impl Tables {
    /// Builds a tree anchored at `anchor`, on `line`, and numbers it after
    /// the unit's earlier trees. `build` gets the tag that names the tree's
    /// temporaries, unique in the unit because it names the tree's first node.
    ///
    /// # Panics
    ///
    /// While the table is sealed ([`Expansions::seal`]): two threads that
    /// expand sites of one unit then number them in the order they run.
    fn expand<T>(
        &mut self,
        anchor: NodeId,
        line: usize,
        build: impl FnOnce(&str) -> Result<T, String>,
        number: impl FnOnce(&mut T, &mut Numbering),
    ) -> Result<&'static T, String> {
        assert!(
            !self.sealed,
            "line {line}: the projection site at node {anchor:?} was first expanded while \
             bodies build on many threads; typing must expand every site the lowering reads"
        );
        let unit = anchor.expansion_unit();
        let used = self.used.get(&unit).copied().unwrap_or(0);
        let mut built = build(&format!("{}_{}", unit & !NodeId::EXPANDED, used + 1))?;
        let mut n = Numbering::resume(unit, used);
        number(&mut built, &mut n);
        self.used.insert(unit, n.used());
        Ok(Box::leak(Box::new(built)))
    }
}

/// Answers `key` from its table (`get`, `get_mut`), else builds it on `line`
/// through [`Tables::expand`] and keeps it when `ex` is shared.
fn memo<T>(
    ex: &Expansions,
    get: fn(&Tables) -> &HashMap<Key, &'static T>,
    get_mut: fn(&mut Tables) -> &mut HashMap<Key, &'static T>,
    key: Key,
    line: usize,
    build: impl FnOnce(&str) -> Result<T, String>,
    number: impl FnOnce(&mut T, &mut Numbering),
) -> Result<&'static T, String> {
    let hit = |t: &Tables| get(t).get(&key).copied();
    // A read lock first: after the checker, every ask is a hit.
    if let Some(tree) = hit(&ex.read()).filter(|_| ex.shared) {
        return Ok(tree);
    }
    let mut t = ex.write();
    if let Some(tree) = hit(&t).filter(|_| ex.shared) {
        return Ok(tree);
    }
    let tree = t.expand(key.0, line, build, number)?;
    if ex.shared {
        get_mut(&mut t).insert(key, tree);
    }
    Ok(tree)
}

impl Expansions {
    /// Returns the expansion an access site lowers through. `None` means the site keeps its
    /// own nodes: no user projection answers, the projection is optional
    /// ([`Expansions::optional_site`] expands it), or the seeded expansion is the identity.
    pub fn site(
        &self,
        impls: &Impls,
        recv: Option<&Type>,
        method: &str,
        recv_expr: &Expr,
        args: &[Expr],
        line: usize,
    ) -> Result<Option<&'static Projection>, String> {
        self.site_at(recv_expr.id(), impls, recv, method, recv_expr, args, line)
    }

    /// [`Expansions::site`] anchored at `anchor`, for a receiver the caller
    /// synthesized.
    #[allow(clippy::too_many_arguments)]
    fn site_at(
        &self,
        anchor: NodeId,
        impls: &Impls,
        recv: Option<&Type>,
        method: &str,
        recv_expr: &Expr,
        args: &[Expr],
        line: usize,
    ) -> Result<Option<&'static Projection>, String> {
        let Some((key, f)) = member(impls, recv, method).filter(|(_, f)| !is_optional(f)) else {
            return Ok(None);
        };
        memo(
            self,
            |t| &t.sites,
            |t| &mut t.sites,
            (anchor, key, method.to_string()),
            line,
            |tag| inline(f, recv_expr, args, line, tag),
            |built, n| {
                built.prologue.iter_mut().for_each(|s| n.stmt(s));
                n.expr(&mut built.place);
            },
        )
        .map(Some)
    }

    /// [`Expansions::site`] for an optional projection. `Ok(None)` also
    /// covers a plain member, which the caller's own paths handle.
    pub fn optional_site(
        &self,
        impls: &Impls,
        recv: Option<&Type>,
        method: &str,
        recv_expr: &Expr,
        args: &[Expr],
        line: usize,
    ) -> Result<Option<&'static OptionalProjection>, String> {
        let Some((key, f)) = member(impls, recv, method).filter(|(_, f)| is_optional(f)) else {
            return Ok(None);
        };
        memo(
            self,
            |t| &t.optional,
            |t| &mut t.optional,
            (recv_expr.id(), key, method.to_string()),
            line,
            |tag| optional_inline(f, recv_expr, args, line, tag),
            |built, n| {
                built.prologue.iter_mut().for_each(|s| n.stmt(s));
                n.expr(&mut built.miss);
                built.hit.iter_mut().for_each(|s| n.stmt(s));
                n.expr(&mut built.place);
            },
        )
        .map(Some)
    }

    /// Returns the `Schema` literal `schemaOf<T>()` at `call` stands for,
    /// expanded once, so the checker types the nodes the lowering walks.
    /// `None` in an unshared table.
    pub fn schema(&self, call: &Expr, decl: &TypeDecl) -> Option<&'static Expr> {
        if !self.shared {
            return None;
        }
        let key = call.id();
        let found = |t: &Tables| t.schemas.get(&key).copied();
        if let Some(e) = found(&self.read()) {
            return Some(e);
        }
        let mut t = self.write();
        if let Some(e) = found(&t) {
            return Some(e);
        }
        let e = t
            .expand(
                key,
                call.line(),
                |_| Ok(crate::types::schema_struct_lit(decl)),
                |lit, n| n.expr(lit),
            )
            .ok()?;
        t.schemas.insert(key, e);
        Some(e)
    }

    /// Returns the literal [`Expansions::schema`] expanded for `call`.
    pub fn schema_at(&self, call: &Expr) -> Option<&'static Expr> {
        self.read().schemas.get(&call.id()).copied()
    }

    /// Returns the statements `a[i] = v` lowers as through a user `place
    /// atSet`: the prologue, then the group [`crate::parser::store_stmts`]
    /// builds. `None` is the seeded row, which the caller's element path
    /// writes.
    pub fn store_index(
        &self,
        impls: &Impls,
        name: &str,
        index: &Expr,
        value: &Expr,
        aty: &Type,
    ) -> Result<Option<&'static Block>, String> {
        if let Some(b) = self.stored(index) {
            return Ok(Some(b));
        }
        let line = index.line();
        let recv = Expr::var(name, line);
        let Some(p) = self.site_at(
            index.id(),
            impls,
            Some(aty),
            "atSet",
            &recv,
            std::slice::from_ref(index),
            line,
        )?
        else {
            return Ok(None);
        };
        let Some(store) = crate::parser::store_stmts(&p.place, value, line) else {
            return Err(format!(
                "line {line}: `{name}[..] = v` goes through an `atSet` projection whose \
                  result has no address — a call result or a temporary. A projection \
                  returns a place: a binding, a field of one, or an element of one"
            ));
        };
        let mut out = p.prologue.clone();
        out.extend(store);
        let mut t = self.write();
        let blk = t.expand(
            index.id(),
            line,
            |_| {
                Ok(Block {
                    id: Id::NEW,
                    stmts: out,
                })
            },
            |b, n| n.block(b),
        )?;
        if self.shared {
            t.stores.insert(index.id(), blk);
        }
        Ok(Some(blk))
    }

    /// Returns the store expansion the checker built, for a reader that has
    /// the statement but not the receiver's type (the lowering, `movecheck`).
    pub fn stored(&self, index: &Expr) -> Option<&'static Block> {
        self.read().stores.get(&index.id()).copied()
    }

    /// Returns the element read of `for x in iter` over a user container: its
    /// `place nth` at [`FOR_RECV`] and [`FOR_INDEX`], expanded once per loop
    /// like [`Expansions::site`]. The loop is the index walk every container
    /// takes; only this read is the container's own.
    pub fn for_element(
        &self,
        impls: &Impls,
        ty: &Type,
        iter: &Expr,
        line: usize,
    ) -> Result<Option<&'static Projection>, String> {
        let var = |name: &str| Expr::Var {
            id: Id::of(iter.id()),
            name: name.to_string(),
            line,
        };
        let nth = crate::types::ITERATE_NTH;
        self.site(
            impls,
            Some(ty),
            nth,
            &var(FOR_RECV),
            &[var(FOR_INDEX)],
            line,
        )
    }
}

/// The receiver's type key and its user projection named `method`; `None`
/// for a builtin container, which indexes through the seeded row.
fn member<'a>(
    impls: &'a Impls,
    recv: Option<&Type>,
    method: &str,
) -> Option<(String, &'a Function)> {
    let (_, f) = impls.place(recv?, method)?;
    Some((crate::types::type_key(recv?)?, f))
}

/// Inlines `f` at an access site. An argument used exactly once is substituted
/// in place; any other binds a temporary first, so its side effects happen
/// exactly once.
pub fn inline(
    f: &Function,
    recv: &Expr,
    args: &[Expr],
    line: usize,
    tag: &str,
) -> Result<Projection, String> {
    let (mut prologue, body) = substituted(f, recv, args, line, tag)?;
    let mut stmts = body;
    let Some(Stmt::Return {
        value: Some(place), ..
    }) = stmts.last()
    else {
        return Err(format!(
            "line {line}: projection `{}` has no exit — a projection ends by \
             returning the place it names",
            f.name
        ));
    };
    let place = place.clone();
    stmts.pop();
    prologue.extend(stmts);
    Ok(Projection { prologue, place })
}

/// Renames the body's own bindings, substitutes the receiver and arguments,
/// and returns the argument-temporary prologue and the substituted statements.
fn substituted(
    f: &Function,
    recv: &Expr,
    args: &[Expr],
    line: usize,
    tag: &str,
) -> Result<(Vec<Stmt>, Vec<Stmt>), String> {
    if args.len() + 1 != f.params.len() {
        return Err(format!(
            "line {line}: projection `{}` takes {} argument(s), got {}",
            f.name,
            f.params.len() - 1,
            args.len()
        ));
    }
    let mut body = f.body.clone();
    // One tag per inline: the prologue lands in the caller's block, so
    // `s[j] = s[k]` would otherwise bind one name twice and read the wrong
    // element.
    // A `let n` inside a projection must not capture a caller's `n`, or be
    // captured by it.
    let mut rename: HashMap<String, String> = HashMap::new();
    collect_bindings(&body, tag, &mut rename);
    if !rename.is_empty() {
        rename_uses(&mut body, &rename);
        rename_bindings(&mut body, &rename);
    }

    let mut prologue = Vec::new();
    let mut map: HashMap<String, Expr> = HashMap::new();
    // The receiver is a place: a `let` would copy the container.
    map.insert("self".to_string(), recv.clone());
    for (p, a) in f.params[1..].iter().zip(args) {
        let uses = count_uses(&body, &p.name, true);
        // A use under a lambda or a loop runs once per call or turn, so only an
        // eager use outside both counts as exactly once.
        if uses == 1 && count_uses(&body, &p.name, false) == 1 && !is_under_loop(&body, &p.name) {
            map.insert(p.name.clone(), a.clone());
        } else {
            let tmp = format!("@p{tag}.{}", p.name);
            prologue.push(Stmt::let_(tmp.clone(), a.clone(), line));
            map.insert(p.name.clone(), Expr::var(tmp, line));
        }
    }
    subst_block(&mut body, &map);
    Ok((prologue, body.stmts))
}

/// One optional access site's lowering: the prologue, a miss test,
/// statements that run only on a hit, and the place a hit reads. Its consumer
/// is always an `if let`, so no `Option` is built on either path.
#[derive(Debug, Clone)]
pub struct OptionalProjection {
    pub prologue: Vec<Stmt>,
    pub miss: Expr,
    pub hit: Vec<Stmt>,
    pub place: Expr,
}

/// Whether a projection member declares an `Option<T>` result, the optional
/// kind. The checker enforces the body shape.
pub fn is_optional(f: &Function) -> bool {
    crate::types::option_payload(&f.ret).is_some()
}

/// [`inline`] for an optional projection: splits the body into the prologue,
/// the one `if <miss> { return None }`, the hit statements, and the trailing
/// `return Some(<place>)`. The checker (`check_places`) enforces the shape, so
/// a mismatch here is its defect and errs rather than mis-lowers.
pub fn optional_inline(
    f: &Function,
    recv: &Expr,
    args: &[Expr],
    line: usize,
    tag: &str,
) -> Result<OptionalProjection, String> {
    let (mut prologue, mut stmts) = substituted(f, recv, args, line, tag)?;
    let bad = || {
        format!(
            "line {line}: optional projection `{}` must end with \
             `if <miss> {{ return None }}` then `return Some(<place>)`",
            f.name
        )
    };
    let Some(Stmt::Return { value: Some(v), .. }) = stmts.last() else {
        return Err(bad());
    };
    let Expr::Call { name, args: sa, .. } = v else {
        return Err(bad());
    };
    if name != "Some" || sa.len() != 1 {
        return Err(bad());
    }
    let place = sa[0].clone();
    stmts.pop();
    let Some(at) = stmts.iter().position(is_miss_return) else {
        return Err(bad());
    };
    let Some(Stmt::If { cond, .. }) = stmts.get(at) else {
        unreachable!("positioned just above");
    };
    let miss = cond.clone();
    let hit: Vec<Stmt> = stmts.split_off(at + 1);
    stmts.pop();
    prologue.extend(stmts);
    Ok(OptionalProjection {
        prologue,
        miss,
        hit,
        place,
    })
}

/// Whether `s` is the optional shape's miss exit: `if <cond> { return None }`
/// with no `else`.
pub fn is_miss_return(s: &Stmt) -> bool {
    let Stmt::If {
        then_block,
        else_block: None,
        ..
    } = s
    else {
        return false;
    };
    then_block.stmts.len() == 1
        && matches!(
            &then_block.stmts[0],
            Stmt::Return { value: Some(Expr::Var { name, .. }), .. } if name == "None"
        )
}

/// Returns the store in a [`store_index`] expansion: its first statement that
/// writes a place (the prologue and move-outs before it are `let`s). The
/// emitter maps this node to the source statement the core judged.
pub fn store_node(blk: &Block) -> Option<&Stmt> {
    blk.stmts.iter().find(|s| {
        matches!(
            s,
            Stmt::Assign { .. } | Stmt::SetField { .. } | Stmt::IndexSet { .. }
        )
    })
}

/// The receiver and the counter a `for` over a user container binds for its
/// element read ([`for_element`]). Unspellable, so no source name collides.
pub const FOR_RECV: &str = "@i.c";
pub const FOR_INDEX: &str = "@i.i";

/// Maps every binding a projection body introduces to an unspellable name.
/// Lambda parameters and pattern binders count: [`subst_block`] walks through
/// lambdas, so an unrenamed `|i| i + 1` would have its `i` substituted.
fn collect_bindings(b: &Block, tag: &str, out: &mut HashMap<String, String>) {
    crate::ast::each_binding(b, &mut |n| {
        out.insert(n.to_string(), format!("@b{tag}.{n}"));
    });
}

/// Renames each read and store of a name through `map` where a binding of the
/// body holds the name, so a parameter of the same spelling keeps its name.
/// Runs before [`rename_bindings`], whose scopes it reads by the old names.
fn rename_uses(b: &mut Block, map: &HashMap<String, String>) {
    crate::body_scope_descent!(UseVisit, use_block, use_stmt, use_expr, mut);

    struct Uses<'a>(&'a HashMap<String, String>);

    impl Uses<'_> {
        fn put(&self, n: &mut String, locals: &std::collections::HashSet<String>) {
            if !locals.contains(n.as_str()) {
                return;
            }
            if let Some(r) = self.0.get(n.as_str()) {
                *n = r.clone();
            }
        }
    }

    impl UseVisit for Uses<'_> {
        fn stmt(&mut self, s: &mut Stmt, locals: &std::collections::HashSet<String>) {
            match s {
                Stmt::Assign { name, .. }
                | Stmt::IndexSet { name, .. }
                | Stmt::SetField { name, .. }
                | Stmt::Drop { name, .. } => self.put(name, locals),
                _ => {}
            }
        }

        fn expr(&mut self, e: &mut Expr, locals: &std::collections::HashSet<String>) -> bool {
            if let Expr::Var { name, .. } = e {
                self.put(name, locals);
            }
            true
        }
    }

    use_block(b, &mut std::collections::HashSet::new(), &mut Uses(map));
}

/// Renames the declaration side of each binding through `map`; [`rename_uses`]
/// renames the uses.
fn rename_bindings(b: &mut Block, map: &HashMap<String, String>) {
    crate::body_scope_descent!(RenameVisit, ren_block, ren_stmt, ren_expr, mut);

    /// A name the map does not hold (module state) is left alone.
    struct Rename<'a>(&'a HashMap<String, String>);

    impl Rename<'_> {
        fn put(&self, n: &mut String) {
            if let Some(r) = self.0.get(n.as_str()) {
                *n = r.clone();
            }
        }
    }

    impl RenameVisit for Rename<'_> {
        // `collect_bindings` already tagged every name, so no scope is needed.
        const SCOPED: bool = false;

        fn stmt(&mut self, s: &mut Stmt, _: &std::collections::HashSet<String>) {
            match s {
                Stmt::Let { name, .. } => self.put(name),
                Stmt::ForIn { var, .. } => self.put(var),
                _ => {}
            }
        }

        fn expr(&mut self, e: &mut Expr, _: &std::collections::HashSet<String>) -> bool {
            if let Expr::Lambda { params, .. } = e {
                for p in params.iter_mut() {
                    self.put(&mut p.name);
                }
            }
            true
        }

        fn arm_pattern(
            &mut self,
            p: &mut crate::ast::Pattern,
            _: usize,
            _: &std::collections::HashSet<String>,
        ) {
            for n in p.binders_mut() {
                self.put(&mut n.name);
            }
        }
    }

    ren_block(b, &mut std::collections::HashSet::new(), &mut Rename(map));
}

/// How many times `name` is read in `b`; a read in a lambda body counts only
/// when `through_lambdas` is set.
fn count_uses(b: &Block, name: &str, through_lambdas: bool) -> usize {
    let mut n = 0;
    crate::ast::each_expr(b, &mut |e| {
        match e {
            Expr::Var { name: v, .. } => n += usize::from(v == name),
            Expr::Lambda { .. } => return through_lambdas,
            _ => {}
        }
        true
    });
    n
}

/// Whether `name` is read inside a loop body.
fn is_under_loop(b: &Block, name: &str) -> bool {
    b.stmts.iter().any(|s| match s {
        Stmt::While { body, .. } | Stmt::ForIn { body, .. } => count_uses(body, name, true) > 0,
        _ => crate::ast::sub_blocks(s)
            .into_iter()
            .any(|b| is_under_loop(b, name)),
    })
}

fn subst_block(b: &mut Block, map: &HashMap<String, Expr>) {
    let mut f = |e: &mut Expr| {
        if let Expr::Var { name, .. } = e {
            if let Some(r) = map.get(name) {
                *e = r.clone();
            }
        }
    };
    walk_block(b, &mut f);
}

/// Applies `f` to every expression in `b`, each after its children, so a
/// substituted expression is never re-walked.
pub fn walk_block(b: &mut Block, f: &mut impl FnMut(&mut Expr)) {
    crate::body_scope_descent!(ExprVisit, expr_block, expr_stmt, expr_expr, mut);

    struct Innermost<'f, F: FnMut(&mut Expr)>(&'f mut F);

    impl<F: FnMut(&mut Expr)> ExprVisit for Innermost<'_, F> {
        const SCOPED: bool = false;
        fn after_expr(&mut self, e: &mut Expr, _: &std::collections::HashSet<String>) {
            (self.0)(e)
        }
    }

    expr_block(b, &mut std::collections::HashSet::new(), &mut Innermost(f));
}

/// [`walk_block`] over every expression a program holds: functions, impl
/// methods and `place` members (never in `Program::functions`), tests,
/// benches, module-state initializers and refinement predicates.
pub(crate) fn walk_program(program: &mut Program, f: &mut impl FnMut(&mut Expr)) {
    for fun in &mut program.functions {
        walk_block(&mut fun.body, f);
    }
    for imp in &mut program.impls {
        for m in imp.methods.iter_mut().chain(imp.places.iter_mut()) {
            walk_block(&mut m.body, f);
        }
    }
    for t in &mut program.tests {
        walk_block(&mut t.body, f);
    }
    for b in &mut program.benches {
        walk_block(&mut b.body, f);
    }
    for g in &mut program.globals {
        walk_bare(&mut g.init, f);
    }
    for t in &mut program.type_decls {
        if let Some(p) = &mut t.predicate {
            walk_bare(p, f);
        }
    }
}

/// [`walk_block`] over a bare expression.
pub fn walk_bare(e: &mut Expr, f: &mut impl FnMut(&mut Expr)) {
    let mut b = Block {
        id: Id::NEW,
        stmts: vec![Stmt::expr(std::mem::replace(e, Expr::int(0)))],
    };
    walk_block(&mut b, f);
    let Some(Stmt::Expr(back, _)) = b.stmts.pop() else {
        unreachable!("one statement in, one statement out")
    };
    *e = back;
}

/// The variable a place is rooted at, e.g. `self` for `self.data[i]`. `None`
/// when `e` has no address: a variable, and a field or element of a place, have
/// one.
pub fn place_root(e: &Expr) -> Option<String> {
    match e {
        Expr::Var { name, .. } => Some(name.clone()),
        Expr::Field { expr, .. } => place_root(expr),
        Expr::Call { name, args, .. } if (name == AT || name == ELEM) && args.len() == 2 => {
            place_root(&args[0])
        }
        _ => None,
    }
}

/// Whether `b` uses `?`. A projection may not: inlined, it has no frame to
/// return from.
pub fn has_try(b: &Block) -> bool {
    let mut found = false;
    crate::ast::each_expr(b, &mut |e| {
        found |= matches!(e, Expr::Try { .. });
        !found
    });
    found
}

/// Whether any impl declares a `place` member. Ask it before working to name a
/// receiver's type: the direct backend's type probe takes `&mut self`.
pub fn any(impls: &[ImplBlock]) -> bool {
    impls.iter().any(|i| !i.places.is_empty())
}

pub fn all(p: &Program) -> impl Iterator<Item = (&ImplBlock, &Function)> {
    p.impls
        .iter()
        .flat_map(|i| i.places.iter().map(move |f| (i, f)))
}

/// `@at`, or one of `places`, the program's user projection names
/// ([`crate::own::Ownership::place_names`]): an element read either way. A
/// name is an element read at every site, whatever the receiver: that only
/// widens a borrow verdict.
pub fn projection_call(name: &str, places: &HashSet<String>) -> bool {
    name == AT || places.contains(name)
}

/// Returns the place an element read looks into, as `(root name, quoted
/// path)`: `xs[i].key` is `("xs", "xs[i].key")`. [`crate::ast::place_path`]
/// answers `None` for these, because `xs[i]` is a call to `@at`.
pub fn element_path(e: &Expr, places: &HashSet<String>) -> Option<(String, String)> {
    match e {
        Expr::Call { name, args, .. } if projection_call(name, places) => {
            let a = args.first()?;
            let (root, path) = crate::ast::place_path(a).or_else(|| element_path(a, places))?;
            // Quoted as the reader wrote it: `xs[i]`, or `xs.name(..)`.
            if name == AT {
                Some((root, format!("{path}[{}]", index_text(args.get(1)))))
            } else {
                Some((root, format!("{path}.{name}(..)")))
            }
        }
        // A field of an element, `fs[0].key`, which `place_path` cannot reach.
        Expr::Field { expr, field, .. } => {
            let (root, path) = element_path(expr, places)?;
            Some((root, format!("{path}.{field}")))
        }
        _ => None,
    }
}

/// An index as the reader wrote it: a name or an integer, else `..`, where
/// `vyrn fix` refuses rather than guessing.
fn index_text(e: Option<&Expr>) -> String {
    match e {
        Some(Expr::Var { name, .. }) => name.clone(),
        Some(Expr::Int(n, _)) => n.to_string(),
        _ => "..".to_string(),
    }
}

/// Returns the names a call can read as a user projection: `program`'s
/// projection names that no function has. The checker types `x.f(..)` as an
/// access only where no function is named `f`, so a function always wins.
pub fn place_names(program: &Program) -> HashSet<String> {
    let fns: HashSet<&str> = program.functions.iter().map(|f| f.name.as_str()).collect();
    (all(program).map(|(_, f)| &f.name))
        .filter(|n| !fns.contains(n.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Program {
        crate::parser::parse(crate::lexer::lex(src).unwrap()).unwrap()
    }

    #[test]
    fn a_projection_parses_and_is_not_a_function() {
        let p = parse(
            "type Ring = { data: Array<Int64> }\n\
             impl Index for Ring {\n\
                 fn at(read self, i: Int64) -> read Int64 { return self.data[i] }\n\
             }\n\
             fn main() { print(1) }\n",
        );
        assert_eq!(p.impls[0].places.len(), 1);
        assert_eq!(p.impls[0].methods.len(), 0);
        assert!(!p.functions.iter().any(|f| f.name.contains("__at")));
    }

    /// A ring with a user `at`, and the site `r[0]` on line 1.
    fn ring_site(ex: &Expansions) -> Result<Option<&'static Projection>, String> {
        let p = parse(
            "type Ring = { data: Array<Int64> }
             impl Index for Ring {
                 fn at(read self, i: Int64) -> read Int64 { return self.data[i] }
             }
             fn main() { print(1) }
",
        );
        let recv = Expr::var("r", 1);
        let ring = Type::Named("Ring".into());
        ex.site(&p.impls, Some(&ring), "at", &recv, &[Expr::int(0)], 1)
    }

    #[test]
    #[should_panic(expected = "was first expanded while bodies build on many threads")]
    fn a_sealed_table_makes_no_tree() {
        let ex = Expansions::shared();
        let _sealed = ex.seal();
        let _ = ring_site(&ex);
    }

    #[test]
    fn a_sealed_table_answers_a_site_typing_expanded() {
        let ex = Expansions::shared();
        let typed = ring_site(&ex).unwrap().unwrap();
        let _sealed = ex.seal();
        assert!(std::ptr::eq(ring_site(&ex).unwrap().unwrap(), typed));
    }

    #[test]
    fn a_single_use_argument_substitutes_in_place() {
        let p = parse(
            "type Ring = { data: Array<Int64> }\n\
             impl Index for Ring {\n\
                 fn at(read self, i: Int64) -> read Int64 { return self.data[i] }\n\
             }\n\
             fn main() { print(1) }\n",
        );
        let (_, f) = p.impls.place(&Type::Named("Ring".into()), "at").unwrap();
        let recv = Expr::var("r", 1);
        let idx = Expr::Binary {
            id: Id::NEW,
            op: crate::ast::BinOp::Add,
            lhs: Box::new(Expr::var("k", 1)),
            rhs: Box::new(Expr::int(1)),
            line: 1,
        };
        let pr = inline(f, &recv, std::slice::from_ref(&idx), 1, "t").unwrap();
        assert!(pr.prologue.is_empty(), "no temp for a single use");
        let Expr::Call { name, args, .. } = &pr.place else {
            panic!("expected the yielded place to stay a call")
        };
        assert_eq!(name, "@at");
        assert!(matches!(&args[0], Expr::Field { field, .. } if field == "data"));
        assert_eq!(args[1], idx, "the index substituted in place");
    }

    #[test]
    fn an_argument_used_only_under_a_lambda_binds_a_temporary() {
        let p = parse(
            "type Ring = { data: Array<Int64> }\n\
             impl Index for Ring {\n\
                 fn at(read self, i: Int64) -> read Int64 {\n\
                     let g = k -> self.data[i]\n\
                     return g(0)\n\
                 }\n\
             }\n\
             fn main() { print(1) }\n",
        );
        let (_, f) = p.impls.place(&Type::Named("Ring".into()), "at").unwrap();
        let pr = inline(f, &Expr::var("r", 5), &[Expr::var("side", 5)], 5, "t").unwrap();
        assert!(
            pr.prologue
                .iter()
                .any(|s| matches!(s, Stmt::Let { name, .. } if name.starts_with("@p"))),
            "a lambda-lazy use must hoist the argument into a temporary: {:?}",
            pr.prologue
        );
    }

    #[test]
    fn a_builtin_container_expands_to_nothing() {
        let recv = Expr::var("a", 3);
        let args = [Expr::int(2)];
        let ex = Expansions::shared();
        for ty in [
            Type::Array(Box::new(Type::Int)),
            Type::Str,
            Type::Map(Box::new(Type::Str), Box::new(Type::Int)),
        ] {
            for method in ["at", "atSet"] {
                assert!(
                    (ex.site(&Impls::default(), Some(&ty), method, &recv, &args, 3))
                        .unwrap()
                        .is_none(),
                    "{ty} took an expansion at `{method}`"
                );
            }
        }
        assert!(ex
            .site(&Impls::default(), None, "at", &recv, &args, 3)
            .unwrap()
            .is_none());
    }

    #[test]
    fn iterate_needs_both_halves() {
        let p = parse(
            "type Ring = { data: Array<Int64> }\n\
             impl Iterate for Ring {\n\
                 fn size(self) -> Int64 { return self.data.length }\n\
             }\n\
             fn main() { print(1) }\n",
        );
        assert!(crate::types::iterate_impl(&p.impls, &Type::Named("Ring".into())).is_none());
    }

    #[test]
    fn a_prologue_binding_cannot_capture_a_caller_name() {
        let p = parse(
            "type Ring = { data: Array<Int64> }\n\
             impl Index for Ring {\n\
                 fn at(read self, i: Int64) -> read Int64 {\n\
                     let j = i * 2\n\
                     return self.data[j]\n\
                 }\n\
             }\n\
             fn main() { print(1) }\n",
        );
        let (_, f) = p.impls.place(&Type::Named("Ring".into()), "at").unwrap();
        let pr = inline(f, &Expr::var("r", 1), &[Expr::int(3)], 1, "t").unwrap();
        assert_eq!(pr.prologue.len(), 1);
        assert!(
            matches!(&pr.prologue[0], Stmt::Let { name, .. } if name.starts_with("@b") && name.ends_with(".j"))
        );
    }

    #[test]
    fn a_lambda_parameter_is_renamed_out_of_the_callers_namespace() {
        let p = parse(
            "type Ring = { data: Array<Int64> }\n\
             impl Index for Ring {\n\
                 fn at(read self, i: Int64) -> read Int64 {\n\
                     let g = i -> i + 1\n\
                     return self.data[g(0)]\n\
                 }\n\
             }\n\
             fn main() { print(1) }\n",
        );
        let (_, f) = p.impls.place(&Type::Named("Ring".into()), "at").unwrap();
        let mut pr = inline(f, &Expr::var("r", 1), &[Expr::int(1)], 1, "t").unwrap();
        // After renaming, `i` has no use outside the lambda, so the argument
        // binds a temporary.
        assert!(
            matches!(&pr.place, Expr::Call { name, .. } if name == "@at"),
            "the yielded place survived inlining"
        );
        assert!(
            pr.prologue.iter().any(
                |s| matches!(s, Stmt::Let { name, value: Expr::Int(1, _), .. } if name.starts_with("@p"))
            ),
            "the caller's argument binds a temporary, not a capture"
        );
        let mut seen_lambda = false;
        let mut prologue = Block {
            id: Id::NEW,
            stmts: std::mem::take(&mut pr.prologue),
        };
        {
            walk_block(&mut prologue, &mut |e: &mut Expr| match e {
                Expr::Lambda { params, body, .. } => {
                    seen_lambda = true;
                    assert_eq!(params.len(), 1);
                    assert!(params[0].starts_with("@b"), "binder renamed: {}", params[0]);
                    if let crate::ast::LambdaBody::Expr(inner) = body {
                        assert!(
                            matches!(inner.as_ref(), Expr::Binary { .. }),
                            "the lambda body still computes from its own binder"
                        );
                    }
                }
                Expr::Var { name, .. } => {
                    assert!(name != "i", "a bare lambda-parameter use survived: {name}")
                }
                _ => {}
            });
        }
        pr.prologue = prologue.stmts;
        assert!(seen_lambda, "the lambda should have been walked");
    }
}
