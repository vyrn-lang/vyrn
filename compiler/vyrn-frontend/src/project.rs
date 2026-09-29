//! Place projections: a `place` member yields a place inside its
//! receiver and is inlined at the access site, so the borrow never leaves the
//! caller's frame. [`site`] expands `a[i]` (a call to [`AT`]) and
//! [`store_index`] expands `a[i] = v`; a builtin container keeps its own nodes
//! and lowers to [`ELEM`], the unspellable addressing primitive. While a
//! [`Memo`] is open each site is expanded once and leaked, because the
//! checker, the ownership passes and the lowering key side tables by node
//! address.

use crate::ast::{
    BinOp, Block, Expr, Function, Id, ImplBlock, LambdaBody, NodeId, Numbering, Program, Stmt,
    Type, TypeDecl,
};
use std::collections::HashMap;

/// The element-place primitive `@slot(container, index)`: the only indexing
/// the backends know by name. Unspellable.
pub const ELEM: &str = "@slot";

/// The element-access dispatch `@at(container, index)` that `a[i]` and
/// `x.at(i)` parse to. Unspellable, so the checker can refuse a free `at(..)`
/// written in source. It dispatches to the method named `at`.
pub const AT: &str = "@at";

/// A field read or an element read: a place, not a value the reader owns.
pub fn is_place_read(e: &Expr) -> bool {
    match e {
        Expr::Field { expr, .. } => {
            matches!(&**expr, Expr::Var { .. } | Expr::Field { .. }) || is_place_read(expr)
        }
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

/// Returns the user `place` member named `method` for a receiver of type `ty`;
/// a builtin container has none.
pub fn lookup<'a>(program: &'a Program, ty: &Type, method: &str) -> Option<&'a Function> {
    lookup_in(&program.impls, ty, method)
}

/// [`lookup`] over an impl list.
pub fn lookup_in<'a>(impls: &'a [ImplBlock], ty: &Type, method: &str) -> Option<&'a Function> {
    if is_builtin_container(ty) {
        return None;
    }
    lookup_by_key(impls, &crate::types::type_key(ty)?, method)
}

/// [`lookup_in`] by type key.
pub fn lookup_by_key<'a>(impls: &'a [ImplBlock], key: &str, method: &str) -> Option<&'a Function> {
    lookup_impl_by_key(impls, key, method).map(|(_, f)| f)
}

/// [`lookup_impl_by_key`] by type; a builtin container skips the scan.
pub fn lookup_impl<'a>(
    impls: &'a [ImplBlock],
    ty: &Type,
    method: &str,
) -> Option<(&'a ImplBlock, &'a Function)> {
    if is_builtin_container(ty) {
        return None;
    }
    lookup_impl_by_key(impls, &crate::types::type_key(ty)?, method)
}

pub fn lookup_impl_by_key<'a>(
    impls: &'a [ImplBlock],
    key: &str,
    method: &str,
) -> Option<(&'a ImplBlock, &'a Function)> {
    for imp in impls {
        if imp.places.is_empty() {
            continue;
        }
        if crate::types::type_key(&imp.ty).as_deref() != Some(key) {
            continue;
        }
        if let Some(f) = imp.places.iter().find(|f| f.name == method) {
            return Some((imp, f));
        }
    }
    None
}

/// Returns the expansion an access site lowers through. `None` means the site
/// keeps its own nodes: no user projection answers, and the seeded expansion
/// would be the identity. `Some` is built once per site while
/// a [`Memo`] is open, so every pass sees the same node addresses.
pub fn site(
    impls: &[ImplBlock],
    recv: Option<&Type>,
    method: &str,
    recv_expr: &Expr,
    args: &[Expr],
    line: usize,
) -> Result<Option<&'static Projection>, String> {
    let Some(t) = recv.filter(|t| !is_builtin_container(t)) else {
        return Ok(None);
    };
    let Some(key) = crate::types::type_key(t) else {
        return Ok(None);
    };
    let Some(f) = lookup_by_key(impls, &key, method) else {
        return Ok(None);
    };
    memo(
        (
            recv_expr as *const Expr as usize,
            line,
            key,
            method.to_string(),
        ),
        recv_expr,
        args,
        || inline(f, recv_expr, args, line),
    )
    .map(Some)
}

/// [`site`] for an optional projection. `Ok(None)` also covers a
/// plain member, which the caller's own paths handle.
pub fn optional_site(
    impls: &[ImplBlock],
    recv: Option<&Type>,
    method: &str,
    recv_expr: &Expr,
    args: &[Expr],
    line: usize,
) -> Result<Option<&'static OptionalProjection>, String> {
    let Some(t) = recv.filter(|t| !is_builtin_container(t)) else {
        return Ok(None);
    };
    let Some(key) = crate::types::type_key(t) else {
        return Ok(None);
    };
    let Some(f) = lookup_by_key(impls, &key, method) else {
        return Ok(None);
    };
    if !is_optional(f) {
        return Ok(None);
    }
    let hit = OPT_MEMO.with(|m| {
        let m = m.borrow();
        let e = m.as_ref()?.get(&(
            recv_expr as *const Expr as usize,
            line,
            key.clone(),
            method.to_string(),
        ))?;
        (e.recv == *recv_expr && e.args == args).then_some(e.tree)
    });
    if let Some(t) = hit {
        return Ok(Some(t));
    }
    let mut built = optional_inline(f, recv_expr, args, line)?;
    numbered(|n| {
        built.prologue.iter_mut().for_each(|s| n.stmt(s));
        n.expr(&mut built.miss);
        built.hit.iter_mut().for_each(|s| n.stmt(s));
        n.expr(&mut built.place);
    });
    let tree: &'static OptionalProjection = Box::leak(Box::new(built));
    OPT_MEMO.with(|m| {
        if let Some(m) = m.borrow_mut().as_mut() {
            m.insert(
                (
                    recv_expr as *const Expr as usize,
                    line,
                    key,
                    method.to_string(),
                ),
                OptExpansion {
                    recv: recv_expr.clone(),
                    args: args.to_vec(),
                    tree,
                },
            );
        }
    });
    Ok(Some(tree))
}

/// [`Expansion`] for the optional kind.
struct OptExpansion {
    recv: Expr,
    args: Vec<Expr>,
    tree: &'static OptionalProjection,
}

/// An access site: receiver node address, line, receiver type key, member
/// name. The line is needed because the memo spans a load, which drops whole
/// generator programs, so an address is reused and two equal sites can differ
/// only in their line.
type Key = (usize, usize, String, String);

/// One expansion and the site inputs it was built from. A hit compares the
/// inputs, because a reused address would otherwise answer with another site's
/// expansion.
struct Expansion {
    recv: Expr,
    args: Vec<Expr>,
    tree: &'static Projection,
}

thread_local! {
    #[allow(clippy::type_complexity)]
    static LOOPS: std::cell::RefCell<
        Option<HashMap<(usize, String, String), (Expr, Block, &'static Block)>>,
    > = const { std::cell::RefCell::new(None) };
    static MEMO: std::cell::RefCell<Option<HashMap<Key, Expansion>>> =
        const { std::cell::RefCell::new(None) };
    /// [`MEMO`] for optional projections.
    static OPT_MEMO: std::cell::RefCell<Option<HashMap<Key, OptExpansion>>> =
        const { std::cell::RefCell::new(None) };
    /// Store expansions, keyed by the index node: `a[i] = v` has no receiver
    /// node, only the temporary [`store_index`] synthesizes.
    #[allow(clippy::type_complexity)]
    static STORES: std::cell::RefCell<
        Option<HashMap<usize, (String, Expr, Expr, &'static Block)>>,
    > = const { std::cell::RefCell::new(None) };
    /// The `Schema` literal each `schemaOf<T>()` node stands for, keyed by
    /// the call node, with the target's name. See [`schema`].
    static SCHEMAS: std::cell::RefCell<Option<HashMap<usize, (String, &'static Expr)>>> =
        const { std::cell::RefCell::new(None) };
}

thread_local! {
    /// The last id [`numbered`] gave.
    static EXPANDED: std::cell::Cell<u32> = const { std::cell::Cell::new(NodeId::EXPANDED) };
}

/// Numbers an expansion's nodes above every program's own ids, so a
/// substituted argument is a node apart from the one it copies.
fn numbered(f: impl FnOnce(&mut Numbering)) {
    EXPANDED.with(|c| {
        let mut n = Numbering(c.get());
        f(&mut n);
        c.set(n.0);
    });
}

/// Shares every expansion built while it is alive, so the checker, the
/// lowering and the emitter walk the same nodes; `direct::compile` takes it as
/// proof (#547). The LSP opens none: it re-checks per keystroke. Expansions are
/// leaked on purpose, because passes key side tables by their addresses; the
/// cost is one tree per user-projection site.
pub struct Memo(());

impl Memo {
    /// Runs `load` under a new memo and returns what it loaded with the
    /// memo, which must outlive every compile of it.
    pub fn load<P, E>(load: impl FnOnce() -> Result<P, E>) -> Result<(P, Self), E> {
        MEMO.with(|m| *m.borrow_mut() = Some(HashMap::new()));
        OPT_MEMO.with(|m| *m.borrow_mut() = Some(HashMap::new()));
        LOOPS.with(|m| *m.borrow_mut() = Some(HashMap::new()));
        STORES.with(|m| *m.borrow_mut() = Some(HashMap::new()));
        SCHEMAS.with(|m| *m.borrow_mut() = Some(HashMap::new()));
        let memo = Memo(());
        Ok((load()?, memo))
    }
}

impl Drop for Memo {
    fn drop(&mut self) {
        MEMO.with(|m| *m.borrow_mut() = None);
        OPT_MEMO.with(|m| *m.borrow_mut() = None);
        LOOPS.with(|m| *m.borrow_mut() = None);
        STORES.with(|m| *m.borrow_mut() = None);
        SCHEMAS.with(|m| *m.borrow_mut() = None);
    }
}

/// Returns the `Schema` literal `schemaOf<T>()` at `call` stands for,
/// expanded once while a [`Memo`] is open so the checker types the nodes the
/// lowering walks. `None` outside a memo.
pub fn schema(call: &Expr, decl: &TypeDecl) -> Option<&'static Expr> {
    SCHEMAS.with(|m| {
        let mut m = m.borrow_mut();
        let m = m.as_mut()?;
        let key = call as *const Expr as usize;
        if let Some((_, e)) = m.get(&key).filter(|(n, _)| *n == decl.name) {
            return Some(*e);
        }
        let mut lit = crate::types::schema_struct_lit(decl);
        numbered(|n| n.expr(&mut lit));
        let e: &'static Expr = Box::leak(Box::new(lit));
        m.insert(key, (decl.name.clone(), e));
        Some(e)
    })
}

/// Returns the literal [`schema`] expanded for `call`.
pub fn schema_at(call: &Expr) -> Option<&'static Expr> {
    let key = call as *const Expr as usize;
    SCHEMAS.with(|m| m.borrow().as_ref()?.get(&key).map(|(_, e)| *e))
}

/// Whether a [`Memo`] is open. A projection store is expanded only then;
/// without one (the LSP) it would leak a tree per keystroke.
pub fn memo_open() -> bool {
    STORES.with(|m| m.borrow().is_some())
}

/// Returns the shared expansion for `key`, or `build`'s, leaked.
fn memo(
    key: Key,
    recv: &Expr,
    args: &[Expr],
    build: impl FnOnce() -> Result<Projection, String>,
) -> Result<&'static Projection, String> {
    let hit = MEMO.with(|m| {
        let m = m.borrow();
        let e = m.as_ref()?.get(&key)?;
        (e.recv == *recv && e.args == args).then_some(e.tree)
    });
    if let Some(t) = hit {
        return Ok(t);
    }
    let mut built = build()?;
    numbered(|n| {
        built.prologue.iter_mut().for_each(|s| n.stmt(s));
        n.expr(&mut built.place);
    });
    let tree: &'static Projection = Box::leak(Box::new(built));
    MEMO.with(|m| {
        if let Some(m) = m.borrow_mut().as_mut() {
            m.insert(
                key,
                Expansion {
                    recv: recv.clone(),
                    args: args.to_vec(),
                    tree,
                },
            );
        }
    });
    Ok(tree)
}

/// Returns the statements `a[i] = v` lowers as through a user `place atSet`:
/// the prologue, then the group [`crate::parser::store_stmts`] builds. `None`
/// is the seeded row, which the caller's element path writes.
pub fn store_index(
    impls: &[ImplBlock],
    name: &str,
    index: &Expr,
    value: &Expr,
    aty: &Type,
) -> Result<Option<&'static Block>, String> {
    if let Some(b) = stored(name, index, value) {
        return Ok(Some(b));
    }
    let line = index.line();
    let recv = Expr::Var {
        id: Id::NEW,
        name: name.to_string(),
        line,
    };
    let Some(p) = site(
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
    let mut built = Block {
        id: Id::NEW,
        stmts: out,
    };
    numbered(|n| n.block(&mut built));
    let blk: &'static Block = Box::leak(Box::new(built));
    STORES.with(|m| {
        if let Some(m) = m.borrow_mut().as_mut() {
            m.insert(
                index as *const Expr as usize,
                (name.to_string(), index.clone(), value.clone(), blk),
            );
        }
    });
    Ok(Some(blk))
}

/// Returns the store expansion the checker built, for a reader that has the
/// statement but not the receiver's type (the lowering, `movecheck`). A hit
/// must match the whole site, as in [`memo`].
pub fn stored(name: &str, index: &Expr, value: &Expr) -> Option<&'static Block> {
    STORES.with(|m| {
        let m = m.borrow();
        let (n, i, v, blk) = m.as_ref()?.get(&(index as *const Expr as usize))?;
        (n == name && i == index && v == value).then_some(*blk)
    })
}

/// Inlines `f` at an access site. An argument used exactly once is substituted
/// in place; any other binds a temporary first, so its side effects happen
/// exactly once.
pub fn inline(f: &Function, recv: &Expr, args: &[Expr], line: usize) -> Result<Projection, String> {
    let (mut prologue, body) = substituted(f, recv, args, line)?;
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
    // One number per inline: the prologue lands in the caller's block, so
    // `s[j] = s[k]` would otherwise bind one name twice and read the wrong
    // element.
    let tag = {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    };
    // A `let n` inside a projection must not capture a caller's `n`, or be
    // captured by it.
    let mut rename: HashMap<String, String> = HashMap::new();
    collect_bindings(&mut body, tag, &mut rename);
    if !rename.is_empty() {
        let renames: HashMap<String, Expr> = rename
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    Expr::Var {
                        id: Id::NEW,
                        name: v.clone(),
                        line,
                    },
                )
            })
            .collect();
        rename_bindings(&mut body, &rename);
        subst_block(&mut body, &renames);
    }

    let mut prologue = Vec::new();
    let mut map: HashMap<String, Expr> = HashMap::new();
    // The receiver is a place: a `let` would copy the container.
    map.insert("self".to_string(), recv.clone());
    for (p, a) in f.params[1..].iter().zip(args) {
        let uses = count_uses(&body, &p.name);
        // A use under a lambda or a loop runs once per call or turn, so only an
        // eager use outside both counts as exactly once.
        if uses == 1 && uses_outside_lambdas(&body, &p.name) == 1 && !is_under_loop(&body, &p.name)
        {
            map.insert(p.name.clone(), a.clone());
        } else {
            let tmp = format!("@p{tag}.{}", p.name);
            prologue.push(Stmt::Let {
                id: Id::NEW,
                name: tmp.clone(),
                mutable: false,
                ty: None,
                value: a.clone(),
                line,
                col: 0,
            });
            map.insert(
                p.name.clone(),
                Expr::Var {
                    id: Id::NEW,
                    name: tmp,
                    line,
                },
            );
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
) -> Result<OptionalProjection, String> {
    let (mut prologue, mut stmts) = substituted(f, recv, args, line)?;
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

/// Returns what `for x in xs` becomes when `xs` is a user container, built
/// from its `size` and `place nth` and expanded once like [`site`]:
///
/// ```text
/// let @i.n = size(xs)
/// let mut @i.i = -1
/// while @i.i + 1 < @i.n {
///     @i.i = @i.i + 1
///     <the projection's prologue>
///     let x = <the place it yields>
///     <the body>
/// }
/// ```
///
/// The increment comes first so a `continue` cannot skip it. The `@` names are
/// unspellable, so no source name collides and a nested loop shadows its outer
/// one.
pub fn iterate_loop(
    size_fn: &str,
    nth: &Function,
    var: &str,
    iter: &Expr,
    body: &Block,
    line: usize,
) -> Result<&'static Block, String> {
    let key = (
        iter as *const Expr as usize,
        size_fn.to_string(),
        var.to_string(),
    );
    let hit = LOOPS.with(|m| {
        let m = m.borrow();
        let (i, b, blk) = m.as_ref()?.get(&key)?;
        (i == iter && b == body).then_some(*blk)
    });
    if let Some(b) = hit {
        return Ok(b);
    }
    let mut built = iterate_loop_build(size_fn, nth, var, iter, body, line)?;
    numbered(|n| n.block(&mut built));
    let blk: &'static Block = Box::leak(Box::new(built));
    LOOPS.with(|m| {
        if let Some(m) = m.borrow_mut().as_mut() {
            m.insert(key, (iter.clone(), body.clone(), blk));
        }
    });
    Ok(blk)
}

fn iterate_loop_build(
    size_fn: &str,
    nth: &Function,
    var: &str,
    iter: &Expr,
    body: &Block,
    line: usize,
) -> Result<Block, String> {
    const IDX: &str = "@i.i";
    const LEN: &str = "@i.n";
    const RECV: &str = "@i.c";
    let var_of = |n: &str| Expr::Var {
        id: Id::NEW,
        name: n.to_string(),
        line,
    };
    let bump = |e: Expr| Expr::Binary {
        id: Id::NEW,
        op: BinOp::Add,
        lhs: Box::new(e),
        rhs: Box::new(Expr::Int(1, Id::NEW)),
        line,
    };

    let mut out = Vec::new();
    // A place is read where it lives, so the loop variable borrows it. Anything
    // else binds once, or its side effects would run n+1 times.
    let recv = if is_place(iter) {
        iter.clone()
    } else {
        out.push(Stmt::Let {
            id: Id::NEW,
            name: RECV.to_string(),
            mutable: false,
            ty: None,
            value: iter.clone(),
            line,
            col: 0,
        });
        var_of(RECV)
    };
    out.push(Stmt::Let {
        id: Id::NEW,
        name: LEN.to_string(),
        mutable: false,
        ty: None,
        value: Expr::Call {
            id: Id::NEW,
            dot: false,
            type_args: Vec::new(),
            name: size_fn.to_string(),
            args: vec![recv.clone()],
            line,
        },
        line,
        col: 0,
    });
    out.push(Stmt::Let {
        id: Id::NEW,
        name: IDX.to_string(),
        mutable: true,
        ty: None,
        value: Expr::Int(-1, Id::NEW),
        line,
        col: 0,
    });

    let mut inner = vec![Stmt::Assign {
        id: Id::NEW,
        name: IDX.to_string(),
        value: bump(var_of(IDX)),
        line,
    }];
    let p = inline(nth, &recv, &[var_of(IDX)], line)?;
    inner.extend(p.prologue);
    inner.push(Stmt::Let {
        id: Id::NEW,
        name: var.to_string(),
        mutable: false,
        ty: None,
        value: p.place,
        line,
        col: 0,
    });
    inner.extend(body.stmts.iter().cloned());
    out.push(Stmt::While {
        id: Id::NEW,
        cond: Expr::Binary {
            id: Id::NEW,
            op: BinOp::Lt,
            lhs: Box::new(bump(var_of(IDX))),
            rhs: Box::new(var_of(LEN)),
            line,
        },
        body: Block {
            id: Id::NEW,
            stmts: inner,
        },
        line,
    });
    Ok(Block {
        id: Id::NEW,
        stmts: out,
    })
}

/// Maps every binding a projection body introduces to an unspellable name.
/// Lambda parameters and pattern binders count: [`subst_block`] walks through
/// lambdas, so an unrenamed `|i| i + 1` would have its `i` substituted.
fn collect_bindings(b: &mut Block, tag: usize, out: &mut HashMap<String, String>) {
    for s in &mut b.stmts {
        match s {
            Stmt::Let { name, .. } => {
                out.insert(name.clone(), format!("@b{tag}.{name}"));
            }
            Stmt::If {
                then_block,
                else_block,
                ..
            } => {
                collect_bindings(then_block, tag, out);
                if let Some(e) = else_block {
                    collect_bindings(e, tag, out);
                }
            }
            Stmt::IfLet {
                pattern,
                then_block,
                else_block,
                ..
            } => {
                for n in pattern.binders() {
                    out.insert(n.name.clone(), format!("@b{tag}.{n}"));
                }
                collect_bindings(then_block, tag, out);
                if let Some(e) = else_block {
                    collect_bindings(e, tag, out);
                }
            }
            Stmt::While { body, .. } | Stmt::Region { body, .. } => {
                collect_bindings(body, tag, out)
            }
            Stmt::ForIn { var, body, .. } => {
                out.insert(var.clone(), format!("@b{tag}.{var}"));
                collect_bindings(body, tag, out);
            }
            _ => {}
        }
    }
    // Lambdas and `match` arm binders live in expressions, which the walk
    // above never enters. Revisiting a sub-block re-inserts the same entries.
    walk_block(b, &mut |e: &mut Expr| {
        collect_lambda(e, tag, out);
        if let Expr::Match { arms, .. } = e {
            for arm in arms {
                for n in arm.pattern.binders() {
                    out.insert(n.name.clone(), format!("@b{tag}.{n}"));
                }
            }
        }
    });
}

fn collect_lambda(e: &mut Expr, tag: usize, out: &mut HashMap<String, String>) {
    let Expr::Lambda { params, body, .. } = e else {
        return;
    };
    for p in params.iter() {
        out.insert(p.name.clone(), format!("@b{tag}.{p}"));
    }
    match body {
        LambdaBody::Expr(inner) => collect_lambda(inner, tag, out),
        LambdaBody::Block(b) => collect_bindings(b, tag, out),
    }
}

/// Renames the declaration side of each binding through `map`; [`subst_block`]
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
                Stmt::Let { name, .. }
                | Stmt::Assign { name, .. }
                | Stmt::IndexSet { name, .. }
                | Stmt::SetField { name, .. }
                | Stmt::Drop { name, .. } => self.put(name),
                Stmt::IfLet { pattern, .. } => {
                    for n in pattern.binders_mut() {
                        self.put(&mut n.name);
                    }
                }
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

fn count_uses(b: &Block, name: &str) -> usize {
    let mut n = 0;
    let mut probe = b.clone();
    let map: HashMap<String, Expr> = HashMap::new();
    count_block(&mut probe, name, &mut n, &map);
    n
}

/// How many times `name` is read outside any lambda body in `b`.
fn uses_outside_lambdas(b: &Block, name: &str) -> usize {
    let mut probe = b.clone();
    walk_block(&mut probe, &mut |e: &mut Expr| {
        if let Expr::Lambda { body, .. } = e {
            *body = LambdaBody::Block(Block {
                id: Id::NEW,
                stmts: Vec::new(),
            });
        }
    });
    count_uses(&probe, name)
}

fn count_block(b: &mut Block, name: &str, n: &mut usize, _m: &HashMap<String, Expr>) {
    let mut counter = |e: &mut Expr| {
        if matches!(e, Expr::Var { name: v, .. } if v == name) {
            *n += 1;
        }
    };
    walk_block(b, &mut counter);
}

/// Whether `name` is read inside a loop body.
fn is_under_loop(b: &Block, name: &str) -> bool {
    fn go(b: &Block, name: &str, in_loop: bool) -> bool {
        for s in &b.stmts {
            match s {
                Stmt::While { body, .. } | Stmt::ForIn { body, .. } => {
                    if count_uses(body, name) > 0 || go(body, name, true) {
                        return true;
                    }
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
                    if go(then_block, name, in_loop) {
                        return true;
                    }
                    if let Some(e) = else_block {
                        if go(e, name, in_loop) {
                            return true;
                        }
                    }
                }
                Stmt::Region { body, .. } => {
                    if go(body, name, in_loop) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }
    go(b, name, false)
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
        stmts: vec![Stmt::Expr(
            std::mem::replace(e, Expr::Int(0, Id::NEW)),
            Id::NEW,
        )],
    };
    walk_block(&mut b, f);
    let Some(Stmt::Expr(back, _)) = b.stmts.pop() else {
        unreachable!("one statement in, one statement out")
    };
    *e = back;
}

/// Whether `e` has an address: a variable, or a field or element of a place.
pub fn is_place(e: &Expr) -> bool {
    match e {
        Expr::Var { .. } => true,
        Expr::Field { expr, .. } => is_place(expr),
        Expr::Call { name, args, .. } if (name == AT || name == ELEM) && args.len() == 2 => {
            is_place(&args[0])
        }
        _ => false,
    }
}

/// The variable a place is rooted at, e.g. `self` for `self.data[i]`.
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
    let mut probe = b.clone();
    walk_block(&mut probe, &mut |e: &mut Expr| {
        if matches!(e, Expr::Try { .. }) {
            found = true;
        }
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

thread_local! {
    /// The user projection names of the program under check, set
    /// by [`note_place_names`]. A name is an element read like `@at` at every
    /// site, whatever the receiver: that only widens a borrow verdict.
    static PLACE_NAMES: std::cell::RefCell<std::collections::HashSet<String>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// Whether `name` is a user projection's name.
pub(crate) fn named_projection(name: &str) -> bool {
    PLACE_NAMES.with(|s| s.borrow().contains(name))
}

/// `@at`, or a user projection's own name: an element read either way.
pub(crate) fn projection_call(name: &str) -> bool {
    name == AT || named_projection(name)
}

/// Returns the place an element read looks into, as `(root name, quoted
/// path)`: `xs[i].key` is `("xs", "xs[i].key")`. [`crate::ast::place_path`]
/// answers `None` for these, because `xs[i]` is a call to `@at`.
pub fn element_path(e: &Expr) -> Option<(String, String)> {
    match e {
        Expr::Call { name, args, .. } if projection_call(name) => {
            let a = args.first()?;
            let (root, path) = crate::ast::place_path(a).or_else(|| element_path(a))?;
            // Quoted as the reader wrote it: `xs[i]`, or `xs.name(..)`.
            if name == AT {
                Some((root, format!("{path}[{}]", index_text(args.get(1)))))
            } else {
                Some((root, format!("{path}.{name}(..)")))
            }
        }
        // A field of an element, `fs[0].key`, which `place_path` cannot reach.
        Expr::Field { expr, field, .. } => {
            let (root, path) = element_path(expr)?;
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

/// Records `program`'s projection names. Call it per analysis, so the language
/// server answers for the program in hand.
pub fn note_place_names(program: &Program) {
    PLACE_NAMES.with(|s| {
        *s.borrow_mut() = program
            .impls
            .iter()
            .flat_map(|i| i.places.iter().map(|p| p.name.clone()))
            .collect();
    });
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

    #[test]
    fn a_single_use_argument_substitutes_in_place() {
        let p = parse(
            "type Ring = { data: Array<Int64> }\n\
             impl Index for Ring {\n\
                 fn at(read self, i: Int64) -> read Int64 { return self.data[i] }\n\
             }\n\
             fn main() { print(1) }\n",
        );
        let f = lookup(&p, &Type::Named("Ring".into()), "at").unwrap();
        let recv = Expr::Var {
            id: Id::NEW,
            name: "r".into(),
            line: 1,
        };
        let idx = Expr::Binary {
            id: Id::NEW,
            op: crate::ast::BinOp::Add,
            lhs: Box::new(Expr::Var {
                id: Id::NEW,
                name: "k".into(),
                line: 1,
            }),
            rhs: Box::new(Expr::Int(1, Id::NEW)),
            line: 1,
        };
        let pr = inline(f, &recv, std::slice::from_ref(&idx), 1).unwrap();
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
        let f = lookup(&p, &Type::Named("Ring".into()), "at").unwrap();
        let pr = inline(
            f,
            &Expr::Var {
                id: Id::NEW,
                name: "r".into(),
                line: 5,
            },
            &[Expr::Var {
                id: Id::NEW,
                name: "side".into(),
                line: 5,
            }],
            5,
        )
        .unwrap();
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
        let recv = Expr::Var {
            id: Id::NEW,
            name: "a".into(),
            line: 3,
        };
        let args = [Expr::Int(2, Id::NEW)];
        for ty in [
            Type::Array(Box::new(Type::Int)),
            Type::Str,
            Type::Map(Box::new(Type::Str), Box::new(Type::Int)),
        ] {
            for method in ["at", "atSet"] {
                assert!(
                    site(&[], Some(&ty), method, &recv, &args, 3)
                        .unwrap()
                        .is_none(),
                    "{ty} took an expansion at `{method}`"
                );
            }
        }
        assert!(site(&[], None, "at", &recv, &args, 3).unwrap().is_none());
    }

    #[test]
    fn an_iterate_loop_increments_before_it_reads() {
        let p = parse(
            "type Ring = { data: Array<Int64> }\n\
             impl Iterate for Ring {\n\
                 fn size(self) -> Int64 { return self.data.length }\n\
                 fn nth(read self, i: Int64) -> read Int64 { return self.data[i] }\n\
             }\n\
             fn main() { print(1) }\n",
        );
        let (size, nth) =
            crate::types::iterate_impl(&p.impls, &Type::Named("Ring".into())).unwrap();
        assert_eq!(size, "Iterate__Ring__size");
        let blk = iterate_loop(
            &size,
            nth,
            "x",
            &Expr::Var {
                id: Id::NEW,
                name: "r".into(),
                line: 9,
            },
            &Block {
                id: Id::NEW,
                stmts: Vec::new(),
            },
            9,
        )
        .unwrap();
        assert_eq!(
            blk.stmts.len(),
            3,
            "size, index, loop — and no receiver copy"
        );
        let Some(Stmt::While { body, .. }) = blk.stmts.last() else {
            panic!("expected a while loop")
        };
        assert!(matches!(&body.stmts[0], Stmt::Assign { name, .. } if name == "@i.i"));
        assert!(matches!(&body.stmts[1], Stmt::Let { name, .. } if name == "x"));
    }

    #[test]
    fn an_iterate_loop_binds_a_temporary_once() {
        let p = parse(
            "type Ring = { data: Array<Int64> }\n\
             impl Iterate for Ring {\n\
                 fn size(self) -> Int64 { return self.data.length }\n\
                 fn nth(read self, i: Int64) -> read Int64 { return self.data[i] }\n\
             }\n\
             fn main() { print(1) }\n",
        );
        let (size, nth) =
            crate::types::iterate_impl(&p.impls, &Type::Named("Ring".into())).unwrap();
        let blk = iterate_loop(
            &size,
            nth,
            "x",
            &Expr::Call {
                id: Id::NEW,
                dot: false,
                type_args: Vec::new(),
                name: "makeRing".into(),
                args: Vec::new(),
                line: 9,
            },
            &Block {
                id: Id::NEW,
                stmts: Vec::new(),
            },
            9,
        )
        .unwrap();
        assert!(matches!(&blk.stmts[0], Stmt::Let { name, .. } if name == "@i.c"));
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
        let f = lookup(&p, &Type::Named("Ring".into()), "at").unwrap();
        let pr = inline(
            f,
            &Expr::Var {
                id: Id::NEW,
                name: "r".into(),
                line: 1,
            },
            &[Expr::Int(3, Id::NEW)],
            1,
        )
        .unwrap();
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
        let f = lookup(&p, &Type::Named("Ring".into()), "at").unwrap();
        let mut pr = inline(
            f,
            &Expr::Var {
                id: Id::NEW,
                name: "r".into(),
                line: 1,
            },
            &[Expr::Int(1, Id::NEW)],
            1,
        )
        .unwrap();
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
                    if let LambdaBody::Expr(inner) = body {
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
