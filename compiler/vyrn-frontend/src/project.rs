//! Place projections (RFC-0091 M2) — the `place` member form, and the one
//! mechanism the memory model needed that was genuinely new.
//!
//! A projection yields a *place* inside its receiver instead of returning a
//! value. Rule 3 of RFC-0089 says a return is owned; rule 2 says a borrow may
//! not be returned. A projection is neither: it is **inlined at the access
//! site**, so the borrow it yields lives inside the caller's frame from the
//! first instruction to the last. Rule 2 holds by construction, and no analysis
//! has to prove it.
//!
//! ## What this module does
//!
//! [`lookup`] answers "does this receiver type declare a projection under this
//! name". [`inline`] answers "what does the access site become". Every engine
//! calls exactly those two, at the two sites the source can spell: `a[i]`
//! ([`Expr::Call`] named [`AT`]) and `a[i] = v` ([`Stmt::IndexSet`]).
//!
//! ## The seeded row, and what the dogfood proof really deletes
//!
//! RFC-0091 M2 asks for the hardcoded `Array` indexing to be deleted and
//! re-expressed as the seeded `Index` impl. It cannot be deleted in full, and
//! the RFC does not notice why: it was written before RFC-0080/0081 withdrew
//! raw memory. `Array`'s own `at` has nothing to write its body *with* — there
//! is no way in Vyrn to say "the element at offset i of my buffer".
//!
//! So one primitive stays, under a name no source can spell: [`ELEM`]
//! (`@slot`). It is the addressing floor, exactly as `malloc`/`free`/`memcpy`
//! are the allocation floor RFC-0091 deliberately leaves closed. What the proof
//! does delete is the **dispatch**: `a[i]` no longer means "the compiler knows
//! about arrays". It means "look up a projection for this receiver's type", and
//! `Array` gets there through [`SEEDED`] like anything else. A user container
//! whose `place at` yields `self.data[i]` inlines to the same `@slot` through
//! one more level of the same machinery.

use crate::ast::{
    ArmBody, BinOp, Block, Expr, Function, ImplBlock, LambdaBody, Program, Stmt, Type,
};
use std::collections::HashMap;

/// The element-place primitive: `@slot(container, index)`. Unspellable (no
/// source token lexes to it), and the only indexing the backends still know
/// about by name. [`AT`] — what `a[i]` parses to — is now the *dispatch* site.
pub const ELEM: &str = "@slot";

/// The element-access **dispatch** site: `@at(container, index)`. What `a[i]`
/// and `x.at(i)` parse to.
///
/// Unspellable for the same reason `@str` is: the free verb form `at(xs, i)`
/// was removed, so a source-written `at(..)` must arrive as a DIFFERENT node
/// than the sugar, or the checker cannot tell the two apart and cannot report
/// the one it refuses.
///
/// The **impl method** this dispatches to is still named `at` — that is what a
/// user writes in `place at`, and what [`SEEDED`] declares. Only the call site
/// moved.
pub const AT: &str = "@at";

/// One access site's lowering: statements to run first, then the place.
#[derive(Debug, Clone)]
pub struct Projection {
    /// The projection body's statements before the `yield`, with the receiver
    /// and arguments substituted. Run in the caller's frame, in order.
    pub prologue: Vec<Stmt>,
    /// The yielded place, substituted. A read loads from it; a store writes to
    /// it. It is always a place the backends already address: a variable, a
    /// field of one, or [`ELEM`].
    pub place: Expr,
}

/// Does `ty` index through the seeded row rather than through a user `impl`?
///
/// `pub` because it is the early-out on the hottest path in the checker: every
/// `a[i]` in the program asks whether its receiver projects, and for the
/// overwhelming majority the answer is "it is an Array" — which must cost one
/// pattern match, not a type resolution.
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

/// The `place` member named `method` for a receiver of type `ty`, if any.
///
/// A user `impl` wins over the seeded row: a type key can only be one or the
/// other, since a program may not declare an impl for a builtin container.
pub fn lookup<'a>(program: &'a Program, ty: &Type, method: &str) -> Option<&'a Function> {
    lookup_in(&program.impls, ty, method)
}

/// [`lookup`], over an impl list rather than a whole program — the shape the
/// engines hold.
pub fn lookup_in<'a>(impls: &'a [ImplBlock], ty: &Type, method: &str) -> Option<&'a Function> {
    if is_builtin_container(ty) {
        return None;
    }
    lookup_by_key(impls, &crate::types::type_key(ty)?, method)
}

/// [`lookup_in`], by type key rather than by type. The interpreter reaches this
/// one: a record value carries the name `coerce` stamped on it, which is the
/// key, where its static type may only be the anonymous record it aliases.
pub fn lookup_by_key<'a>(impls: &'a [ImplBlock], key: &str, method: &str) -> Option<&'a Function> {
    lookup_impl_by_key(impls, key, method).map(|(_, f)| f)
}

/// [`lookup_impl_by_key`], by type. The builtin containers short-circuit before
/// the scan: every `a[i]` in the program reaches this, and a builtin container
/// can never have a user `impl`, so the scan would always come back empty.
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

/// The impl and the `place` member named `method` for type key `key`.
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

/// The expansion an access site lowers through, or `None` when the site keeps
/// its own nodes.
///
/// **`None` is the seeded row**, which is every builtin container and every
/// receiver whose type the caller could not name. Its body is
/// `yield @slot(self, i)`, so inlining it substitutes the site's own receiver
/// and index back into an [`ELEM`] of themselves: the expansion is the
/// identity, and every engine then lowers the ORIGINAL nodes rather than the
/// copies. Both compiling backends used to build that expansion in order to
/// discover they did not need it — 20,205 clone-rename-substitute rounds over
/// the corpus, all discarded. The interpreter never did; this is its shape,
/// shared.
///
/// Answering from the LOOKUP also keeps the node ADDRESSES, and this compiler
/// keys side tables by them — `own`'s rows, the elided `get`/`set` generation
/// checks, the lambda monomorphization keys, and since RFC-0101 the lowering's
/// own answers. A copy has an address of its own and loses whatever was
/// recorded against the original.
/// **`Some` is expanded once and shared** while a [`Memo`] is open (RFC-0101's
/// desugar-once milestone). The expansion is a function of the projection, the
/// receiver and the arguments, so building it per engine produced two or three
/// structurally identical trees at three different addresses — trees the
/// checker never saw, the lowering never recorded, and no side table could
/// reach. One tree, one set of addresses, one answer per node.
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
        (recv_expr as *const Expr as usize, key, method.to_string()),
        recv_expr,
        args,
        || inline(f, recv_expr, args, line),
    )
    .map(Some)
}

/// [`site`], for the OPTIONAL kind (RFC-0122). `Ok(None)` means "no optional
/// projection answers here": a builtin receiver, a type this walk cannot
/// name, a member of the PLAIN kind (the caller's own paths handle those),
/// or no member at all. Shares [`Memo`]'s lifetime and discipline — one
/// tree, one set of addresses, one answer per node.
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
            key.clone(),
            method.to_string(),
        ))?;
        (e.recv == *recv_expr && e.args == args).then_some(e.tree)
    });
    if let Some(t) = hit {
        return Ok(Some(t));
    }
    let tree: &'static OptionalProjection =
        Box::leak(Box::new(optional_inline(f, recv_expr, args, line)?));
    OPT_MEMO.with(|m| {
        if let Some(m) = m.borrow_mut().as_mut() {
            m.insert(
                (recv_expr as *const Expr as usize, key, method.to_string()),
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

/// [`Expansion`], for the optional kind — same verification, same reason.
struct OptExpansion {
    recv: Expr,
    args: Vec<Expr>,
    tree: &'static OptionalProjection,
}

// ---------------------------------------------------------------------------
// Desugar once
// ---------------------------------------------------------------------------

/// One expansion, and the site inputs it was built from.
///
/// The inputs are kept so a hit can be VERIFIED. The key holds a node address,
/// a freed node's address is handed out again, and a memo that answers from a
/// dead key is a miscompile rather than a slow path. Comparing the receiver and
/// the arguments costs a walk over two small expressions; being wrong here
/// costs a program that indexes with somebody else's index.
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
    static MEMO: std::cell::RefCell<Option<HashMap<(usize, String, String), Expansion>>> =
        const { std::cell::RefCell::new(None) };
    /// The optional kind's half of [`MEMO`] (RFC-0122), same key, same rules.
    static OPT_MEMO: std::cell::RefCell<Option<HashMap<(usize, String, String), OptExpansion>>> =
        const { std::cell::RefCell::new(None) };
    /// The store half, keyed by the INDEX node rather than by the receiver:
    /// `a[i] = v` has no receiver node — [`store_index`] synthesizes one, and a
    /// stack temporary's address is not an identity. See [`stored`].
    #[allow(clippy::type_complexity)]
    static STORES: std::cell::RefCell<
        Option<HashMap<usize, (String, Expr, Expr, &'static Block)>>,
    > = const { std::cell::RefCell::new(None) };
}

/// Share every desugar built while this value is alive.
///
/// Open one around a compile — the point where a program is lowered and then
/// emitted, once or twice — so the lowering and both backends walk the SAME
/// nodes. Outside a `Memo` every caller expands for itself, which is what the
/// engines did before this existed and what the LSP still wants: it re-checks
/// a program per keystroke, and a memo keyed by node address over programs it
/// throws away is a leak with a verification bill attached.
///
/// An expansion is leaked deliberately. The nodes it holds are keyed by
/// address by `own`, by `movecheck` and by the lowering, so they must outlive
/// every walk that could record against them, and a compile is a process here.
/// The bound is one tree per user-projection site per program, which the corpus
/// measures at 164 trees of about 4,800 nodes for 161 examples.
pub struct Memo(());

impl Memo {
    pub fn open() -> Self {
        MEMO.with(|m| *m.borrow_mut() = Some(HashMap::new()));
        OPT_MEMO.with(|m| *m.borrow_mut() = Some(HashMap::new()));
        LOOPS.with(|m| *m.borrow_mut() = Some(HashMap::new()));
        STORES.with(|m| *m.borrow_mut() = Some(HashMap::new()));
        Memo(())
    }
}

impl Drop for Memo {
    fn drop(&mut self) {
        MEMO.with(|m| *m.borrow_mut() = None);
        OPT_MEMO.with(|m| *m.borrow_mut() = None);
        LOOPS.with(|m| *m.borrow_mut() = None);
        STORES.with(|m| *m.borrow_mut() = None);
    }
}

/// The shared expansion for `key`, or `build`'s, leaked so its addresses outlive
/// every consumer.
fn memo(
    key: (usize, String, String),
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
    let tree: &'static Projection = Box::leak(Box::new(build()?));
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

/// The statements `a[i] = v` lowers as, after `place atSet` has had its say
/// (RFC-0091 M2, finished in M3).
///
/// `None` is [`site`]'s `None`: the seeded row, whose store the caller's own
/// element path writes. `Some` is a user container, whose store becomes the
/// projection's prologue and the move-out/mutate/move-back group
/// [`store_stmts`] builds.
///
/// One function because it was two, byte for byte, in `lib.rs` and
/// `direct.rs` — the shape RFC-0101 §1.1 counts, down to the refusal's wording.
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
    let Some(store) = store_stmts(&p.place, value, line) else {
        return Err(format!(
            "line {line}: `{name}[..] = v` goes through an `atSet` projection whose              result has no address — a call result or a temporary. A projection              returns a place: a binding, a field of one, or an element of one"
        ));
    };
    let mut out = p.prologue.clone();
    out.extend(store);
    let blk: &'static Block = Box::leak(Box::new(Block { stmts: out }));
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

/// The shared expansion of a store site, for a reader that has the statement
/// but not the receiver's TYPE — which is the lowering.
///
/// [`store_index`] needs `aty` to find the `place atSet` at all; the lowering
/// stands at a `Stmt::IndexSet` whose receiver is a NAME and has no scope of
/// binding types to resolve it in. So the anchor is the index node — a node of
/// the program, alive for the whole compile — and the verification is the whole
/// site: the same receiver name, the same index, the same value. Address reuse
/// answering from a dead key is the failure [`memo`] guards against, and this
/// guards against it the same way.
pub fn stored(name: &str, index: &Expr, value: &Expr) -> Option<&'static Block> {
    STORES.with(|m| {
        let m = m.borrow();
        let (n, i, v, blk) = m.as_ref()?.get(&(index as *const Expr as usize))?;
        (n == name && i == index && v == value).then_some(*blk)
    })
}

/// Inline `f` at an access site whose receiver is `recv` and whose arguments
/// are `args`.
///
/// An argument used exactly once is substituted in place, which is what keeps
/// the seeded row's lowering byte-identical to the hardcoded one it replaces.
/// An argument used zero times or more than once binds a temporary first: zero
/// so its side effects still happen, more than once so they happen once.
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

/// The shared front half of every inline: hygiene-rename the body's own
/// bindings, substitute the receiver and the arguments, and hand back the
/// argument-temporary prologue plus the substituted statements. [`inline`]
/// splits one trailing `return` off the result; [`optional_inline`]
/// (RFC-0122) splits a miss test and a `Some` exit.
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
    // One number per inline, so two inlines of one projection in one block bind
    // different names.
    //
    // They land in the CALLER's block: the prologue is statements, not a scope
    // of its own. `s[j] = s[k]` inlines the same body twice, and with a fixed
    // `@b.name` the second `let` shadows the first for everything after it — the
    // store read the wrong element, and only in the two compiling backends,
    // because the interpreter gives each inline a frame. The names never reach
    // the emitted output (a slot is `%tN`), so this costs nothing.
    let tag = {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    };
    // Rename the body's own bindings out of the caller's namespace. A `let n`
    // inside a projection must not capture, or be captured by, a caller's `n`.
    let mut rename: HashMap<String, String> = HashMap::new();
    collect_bindings(&mut body, tag, &mut rename);
    if !rename.is_empty() {
        let renames: HashMap<String, Expr> = rename
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    Expr::Var {
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
    // The receiver is a place, never a value: substituting it directly is the
    // whole point. Hoisting it into a `let` would copy the container.
    map.insert("self".to_string(), recv.clone());
    for (p, a) in f.params[1..].iter().zip(args) {
        let uses = count_uses(&body, &p.name);
        // A use inside a lambda body evaluates lazily — once per INVOCATION.
        // Substituting the caller's expression there re-runs it on every call
        // of the lambda, so only an EAGER use counts as "exactly once".
        if uses == 1 && uses_outside_lambdas(&body, &p.name) == 1 && !is_under_loop(&body, &p.name)
        {
            map.insert(p.name.clone(), a.clone());
        } else {
            let tmp = format!("@p{tag}.{}", p.name);
            prologue.push(Stmt::Let {
                name: tmp.clone(),
                mutable: false,
                ty: None,
                value: a.clone(),
                line,
            });
            map.insert(p.name.clone(), Expr::Var { name: tmp, line });
        }
    }
    subst_block(&mut body, &map);
    Ok((prologue, body.stmts))
}

/// One OPTIONAL access site's lowering (RFC-0122; the hit prologue is
/// RFC-0123 M1): statements, then a miss test, then statements that run
/// only on the hit, then the place the hit reads. The consumer is always an
/// `if let` — the else arm on the miss, the then arm running `hit` and then
/// binding its binder to the place — so no `Option` exists on either path.
#[derive(Debug, Clone)]
pub struct OptionalProjection {
    pub prologue: Vec<Stmt>,
    pub miss: Expr,
    pub hit: Vec<Stmt>,
    pub place: Expr,
}

/// Whether a projection member is the OPTIONAL kind (RFC-0122): its declared
/// result is an `Option<T>`, where the plain kind names the place's type
/// bare. The checker enforces the body shape this classification implies.
pub fn is_optional(f: &Function) -> bool {
    matches!(f.ret, crate::ast::Type::Option(_))
}

/// [`inline`] for an optional projection: split the body into its four
/// parts — prologue, the ONE `if <miss> { return None }`, the hit prologue
/// (RFC-0123 M1), and the trailing `return Some(<place>)`.
/// The shapes are the checker's (`check_places`) — a surprise here is a bug
/// there, and errs rather than mis-lowers.
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
    // The ONE miss exit sits anywhere before the exit (RFC-0123 M1): what
    // precedes it is the prologue, what follows is the HIT prologue —
    // statements that run only when the place exists.
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

/// What a store through a projected place becomes (RFC-0091 M3).
///
/// 7a refused this by name: `a[i] = v` accepted a projection only where the
/// yielded place was the binding's own element, because writing anywhere else
/// "needs an address-of no backend has". **That reading was wrong, and the
/// mechanism was already in the repo.** RFC-0082 M1 met the same problem for
/// `r.a[i] = v` — a container that is not a slot — and answered it without an
/// address-of: move the container out into a temp, mutate the temp, move it
/// back. [`crate::parser::place_receiver`] is that desugar, it is pure AST, and
/// it already handles the three shapes a place can take.
///
/// So a store through a user container is the same three statements the
/// language emits for `r.a[i] = v`, wrapped around the store the projection
/// resolved to. No engine gains an addressing mode.
///
/// The move-out is O(1) for a growable container — a header copy, sharing the
/// buffer — and a whole-value copy for one held inline, which is what
/// `a[i].f = v` has always cost.
///
/// `None` means the projection yields something no store can reach: a call
/// result, a literal, a temporary. The caller keeps its own refusal.
pub fn store_stmts(place: &Expr, value: &Expr, line: usize) -> Option<Vec<Stmt>> {
    match place {
        // The whole receiver: `yield self` and nothing else.
        Expr::Var { name, .. } => Some(vec![Stmt::Assign {
            name: name.clone(),
            value: value.clone(),
            line,
        }]),
        // A field of a place: `return self.count`.
        Expr::Field { expr, field, .. } => {
            let (recv, mut out, moves, post) = crate::parser::place_receiver(expr, line)?;
            let value = if moves.is_empty() {
                value.clone()
            } else {
                crate::parser::hoist_operand(
                    value.clone(),
                    format!("{recv}.{field}=val"),
                    &mut out,
                    line,
                )
            };
            out.extend(moves);
            out.push(Stmt::SetField {
                name: recv,
                field: field.clone(),
                value,
                line,
            });
            out.extend(post);
            Some(out)
        }
        // An element of a place: `return self.data[j]`, and the seeded row's
        // `yield @slot(self, i)`.
        Expr::Call { name, args, .. } if (name == AT || name == ELEM) && args.len() == 2 => {
            let (recv, mut out, moves, post) = crate::parser::place_receiver(&args[0], line)?;
            // With a move-out in play the index and the value run before it, in
            // source order: nothing may read the place while it is out.
            let (index, value) = if moves.is_empty() {
                (args[1].clone(), value.clone())
            } else {
                let i = crate::parser::hoist_operand(
                    args[1].clone(),
                    format!("{recv}[]idx"),
                    &mut out,
                    line,
                );
                let v = crate::parser::hoist_operand(
                    value.clone(),
                    format!("{recv}[]val"),
                    &mut out,
                    line,
                );
                (i, v)
            };
            out.extend(moves);
            out.push(Stmt::IndexSet {
                name: recv,
                index,
                value,
                line,
            });
            out.extend(post);
            Some(out)
        }
        _ => None,
    }
}

/// What `for x in xs` becomes when `xs` is a user container (RFC-0091 M3).
///
/// A builtin container is walked by each backend's own element loop, which is
/// three pointer bumps and a bounds test. A user container has no buffer the
/// compiler can name, so the loop is written in terms of what the container
/// declared: `size` for how many, and the `place nth` projection for where each
/// element is. Both come from the same table `a[i]` reads, so there is no
/// second list.
///
/// The result is ordinary AST, and each engine lowers it with the statements it
/// already has. That is what keeps this one implementation instead of three:
/// the loop is a `while`, the element is a `let` of the yielded place, and the
/// prologue the projection needs runs inside the turn that reads it.
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
/// **The increment is the body's first statement, not its last.** A `continue`
/// jumps to the condition; an increment at the end would be skipped and the
/// loop would spin on one element. Testing `@i.i + 1` rather than `@i.i` is
/// what pays for that — the index names the element the turn is reading, so a
/// `break` out of the body leaves it where a reader expects.
///
/// The three bindings are unspellable (`@` does not lex), so a nested loop's
/// pair shadows its outer pair and no source name can collide with either.
pub fn iterate_loop(
    size_fn: &str,
    nth: &Function,
    var: &str,
    iter: &Expr,
    body: &Block,
    line: usize,
) -> Result<&'static Block, String> {
    // Expanded once and shared, like [`site`] — and this one carries the user's
    // whole loop body, cloned into the shape the projection needs, so an engine
    // that expands for itself makes every node of that body phantom too.
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
    let blk: &'static Block = Box::leak(Box::new(iterate_loop_build(
        size_fn, nth, var, iter, body, line,
    )?));
    // RFC-0114 §26: the clone's nodes carry none of the plan's addresses, so
    // a release planned on the original body would go undischarged — the
    // leak the finish check caught on `slots.vyrn`'s own loop. The body
    // clones VERBATIM as the `while`'s tail, so zipping the two walks pairs
    // every cloned node with its original; the backends register the pairs
    // and the plan resolves queries through them.
    let tail_at = |b: &'static Block| -> &'static [Stmt] {
        let Some(Stmt::While { body: inner, .. }) = b.stmts.last() else {
            return &[];
        };
        let n = inner.stmts.len();
        &inner.stmts[n - body.stmts.len()..]
    };
    let mut orig = Vec::new();
    let mut clone = Vec::new();
    crate::ast::node_addrs(body, &mut orig);
    for s in tail_at(blk) {
        crate::ast::node_addrs_one(s, &mut clone);
    }
    let pairs: &'static [(usize, usize)] = Box::leak(
        clone
            .into_iter()
            .zip(orig)
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );
    ALIASES.with(|m| {
        m.borrow_mut().insert(blk as *const Block as usize, pairs);
    });
    LOOPS.with(|m| {
        if let Some(m) = m.borrow_mut().as_mut() {
            m.insert(key, (iter.clone(), body.clone(), blk));
        }
    });
    Ok(blk)
}

thread_local! {
    /// Per expansion: every cloned node's address paired with its original's
    /// — see the note in [`iterate_loop`]. Keyed by the leaked block's
    /// address, so a memo hit answers with the pairs built the first time.
    static ALIASES: std::cell::RefCell<HashMap<usize, &'static [(usize, usize)]>> =
        std::cell::RefCell::new(HashMap::new());
}

/// The clone→original address pairs for an [`iterate_loop`] expansion, empty
/// for a block this module did not build.
pub fn iterate_aliases(blk: &Block) -> &'static [(usize, usize)] {
    ALIASES.with(|m| {
        m.borrow()
            .get(&(blk as *const Block as usize))
            .copied()
            .unwrap_or(&[])
    })
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
        name: n.to_string(),
        line,
    };
    let bump = |e: Expr| Expr::Binary {
        op: BinOp::Add,
        lhs: Box::new(e),
        rhs: Box::new(Expr::Int(1)),
        line,
    };

    let mut out = Vec::new();
    // A container named by a place is read where it lives, which is what makes
    // the loop variable a borrow of it. Anything else is a temporary and binds
    // once: evaluating it per turn would run its side effects n+1 times.
    let recv = if is_place(iter) {
        iter.clone()
    } else {
        out.push(Stmt::Let {
            name: RECV.to_string(),
            mutable: false,
            ty: None,
            value: iter.clone(),
            line,
        });
        var_of(RECV)
    };
    out.push(Stmt::Let {
        name: LEN.to_string(),
        mutable: false,
        ty: None,
        value: Expr::Call {
            name: size_fn.to_string(),
            args: vec![recv.clone()],
            line,
        },
        line,
    });
    out.push(Stmt::Let {
        name: IDX.to_string(),
        mutable: true,
        ty: None,
        value: Expr::Int(-1),
        line,
    });

    let mut inner = vec![Stmt::Assign {
        name: IDX.to_string(),
        value: bump(var_of(IDX)),
        line,
    }];
    let p = inline(nth, &recv, &[var_of(IDX)], line)?;
    inner.extend(p.prologue);
    inner.push(Stmt::Let {
        name: var.to_string(),
        mutable: false,
        ty: None,
        value: p.place,
        line,
    });
    inner.extend(body.stmts.iter().cloned());
    out.push(Stmt::While {
        cond: Expr::Binary {
            op: BinOp::Lt,
            lhs: Box::new(bump(var_of(IDX))),
            rhs: Box::new(var_of(LEN)),
            line,
        },
        body: Block { stmts: inner },
        line,
    });
    Ok(Block { stmts: out })
}

// ---------------------------------------------------------------------------
// Substitution
// ---------------------------------------------------------------------------

/// Every binding a projection body introduces, mapped to an unspellable name.
///
/// A lambda's parameters count (RFC-0023). Once the body is inlined they share
/// the caller's namespace, and [`subst_block`]'s walk reaches straight through
/// [`Expr::Lambda`] — so a parameter named like a projection parameter was
/// substituted OVER: under a caller's `r[1]`, `let g = |i| i + 1` became
/// `|i| 1 + 1` and read the wrong element. Renaming the binder here (and its
/// uses, through the same map) puts them out of substitution's reach.
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
                // A pattern binder is a binding of the body too (RFC-0121):
                // leaving it un-renamed while `subst_block` rewrites its uses
                // is how an arm came to yield a name nothing bound.
                for n in pattern_binder_names(pattern) {
                    out.insert(n.clone(), format!("@b{tag}.{n}"));
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
    // The statement walk above never enters an expression, and that is where a
    // lambda lives (`let g = |i| ..`) — and, since RFC-0121, a `match` whose
    // arms bind payloads. One full-expression walk per block picks both up,
    // nested ones included; re-visiting a sub-block's expressions from both
    // this walk and the recursion re-inserts the same entries, which is
    // idempotent.
    walk_block(b, &mut |e: &mut Expr| {
        collect_lambda(e, tag, out);
        if let Expr::Match { arms, .. } = e {
            for arm in arms {
                for n in pattern_binder_names(&arm.pattern) {
                    out.insert(n.clone(), format!("@b{tag}.{n}"));
                }
            }
        }
    });
}

/// The names a pattern binds, by reference — the rename walk's view.
fn pattern_binder_names(p: &crate::ast::Pattern) -> Vec<&String> {
    use crate::ast::Pattern;
    match p {
        Pattern::Some(b)
        | Pattern::Ok(b)
        | Pattern::Err(b)
        | Pattern::Success(b)
        | Pattern::Failure(b) => vec![b],
        Pattern::Variant(_, binds) => binds.iter().collect(),
        Pattern::None | Pattern::Other => Vec::new(),
    }
}

/// The same names, mutably — what [`rename_bindings`] rewrites.
fn pattern_binder_names_mut(p: &mut crate::ast::Pattern) -> Vec<&mut String> {
    use crate::ast::Pattern;
    match p {
        Pattern::Some(b)
        | Pattern::Ok(b)
        | Pattern::Err(b)
        | Pattern::Success(b)
        | Pattern::Failure(b) => vec![b],
        Pattern::Variant(_, binds) => binds.iter_mut().collect(),
        Pattern::None | Pattern::Other => Vec::new(),
    }
}

fn collect_lambda(e: &mut Expr, tag: usize, out: &mut HashMap<String, String>) {
    let Expr::Lambda { params, body, .. } = e else {
        return;
    };
    for p in params.iter() {
        out.insert(p.clone(), format!("@b{tag}.{p}"));
    }
    match body {
        LambdaBody::Expr(inner) => collect_lambda(inner, tag, out),
        LambdaBody::Block(b) => collect_bindings(b, tag, out),
    }
}

/// Rewrite the *declaration* side of each binding through `map` — a lambda's
/// parameters are declarations too (see [`collect_bindings`]); their USES go
/// through the same map in [`subst_block`], whose walk reaches the same bodies.
fn rename_bindings(b: &mut Block, map: &HashMap<String, String>) {
    for s in &mut b.stmts {
        match s {
            Stmt::Let { name, .. } => {
                if let Some(n) = map.get(name) {
                    *name = n.clone();
                }
            }
            // A statement that NAMES a binding follows the binding's rename
            // (RFC-0121 — the first projection bodies that mutate a local).
            // A name not in the map is somebody else's (module state) and is
            // left alone.
            Stmt::Assign { name, .. }
            | Stmt::IndexSet { name, .. }
            | Stmt::SetField { name, .. }
            | Stmt::Drop { name, .. } => {
                if let Some(n) = map.get(name) {
                    *name = n.clone();
                }
            }
            Stmt::If {
                then_block,
                else_block,
                ..
            } => {
                rename_bindings(then_block, map);
                if let Some(e) = else_block {
                    rename_bindings(e, map);
                }
            }
            Stmt::IfLet {
                pattern,
                then_block,
                else_block,
                ..
            } => {
                for n in pattern_binder_names_mut(pattern) {
                    if let Some(r) = map.get(n.as_str()) {
                        *n = r.clone();
                    }
                }
                rename_bindings(then_block, map);
                if let Some(e) = else_block {
                    rename_bindings(e, map);
                }
            }
            Stmt::While { body, .. } | Stmt::Region { body, .. } => rename_bindings(body, map),
            Stmt::ForIn { var, body, .. } => {
                if let Some(n) = map.get(var) {
                    *var = n.clone();
                }
                rename_bindings(body, map);
            }
            _ => {}
        }
    }
    walk_block(b, &mut |e: &mut Expr| {
        if let Expr::Match { arms, .. } = e {
            for arm in arms.iter_mut() {
                for n in pattern_binder_names_mut(&mut arm.pattern) {
                    if let Some(r) = map.get(n.as_str()) {
                        *n = r.clone();
                    }
                }
            }
        }
        if let Expr::Lambda { params, .. } = e {
            for p in params.iter_mut() {
                if let Some(n) = map.get(p) {
                    *p = n.clone();
                }
            }
        }
    });
}

/// How many times `name` is read in `b`.
fn count_uses(b: &Block, name: &str) -> usize {
    let mut n = 0;
    let mut probe = b.clone();
    let map: HashMap<String, Expr> = HashMap::new();
    count_block(&mut probe, name, &mut n, &map);
    n
}

/// How many times `name` is read OUTSIDE any lambda body in `b`. A read under
/// a lambda runs once per invocation of that lambda, so it never counts as the
/// single eager use that lets an argument substitute in place — such an
/// argument binds a temporary, exactly like one used twice.
fn uses_outside_lambdas(b: &Block, name: &str) -> usize {
    let mut probe = b.clone();
    walk_block(&mut probe, &mut |e: &mut Expr| {
        if let Expr::Lambda { body, .. } = e {
            // The walk sees a node after its children, so every nested body
            // has already had its reads counted by the time its enclosing
            // lambda is blanked here.
            *body = LambdaBody::Block(Block { stmts: Vec::new() });
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

/// Is `name` read inside a loop body? Substituting there would re-evaluate the
/// argument once per turn, so such a parameter always binds a temporary.
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

/// Apply `f` to every expression node in `b`, innermost-last: `f` sees a node
/// after its children, so a substituted expression is never re-walked.
///
/// `pub(crate)` since census U5: the loader stamps every `panic` with its source
/// site and needs the same complete walk this one already is.
pub(crate) fn walk_block(b: &mut Block, f: &mut impl FnMut(&mut Expr)) {
    for s in &mut b.stmts {
        walk_stmt(s, f);
    }
}

fn walk_stmt(s: &mut Stmt, f: &mut impl FnMut(&mut Expr)) {
    match s {
        Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::SetField { value, .. } => {
            walk_expr(value, f)
        }
        Stmt::IndexSet { index, value, .. } => {
            walk_expr(index, f);
            walk_expr(value, f);
        }
        Stmt::Return { value: Some(e), .. } => walk_expr(e, f),
        Stmt::Return { value: None, .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            walk_expr(cond, f);
            walk_block(then_block, f);
            if let Some(e) = else_block {
                walk_block(e, f);
            }
        }
        Stmt::IfLet {
            scrutinee,
            then_block,
            else_block,
            ..
        } => {
            walk_expr(scrutinee, f);
            walk_block(then_block, f);
            if let Some(e) = else_block {
                walk_block(e, f);
            }
        }
        Stmt::While { cond, body, .. } => {
            walk_expr(cond, f);
            walk_block(body, f);
        }
        Stmt::ForIn { iter, body, .. } => {
            walk_expr(iter, f);
            walk_block(body, f);
        }
        Stmt::Drop { .. } => {}
        Stmt::Expr(e) => walk_expr(e, f),
        Stmt::Region { body, .. } => walk_block(body, f),
    }
}

fn walk_expr(e: &mut Expr, f: &mut impl FnMut(&mut Expr)) {
    match e {
        Expr::Int(_)
        | Expr::Byte(_)
        | Expr::Float(_)
        | Expr::Bool(_)
        | Expr::Str(_)
        | Expr::Var { .. } => {}
        Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
            walk_expr(expr, f)
        }
        Expr::Consume { place, .. } => walk_expr(place, f),
        Expr::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, f);
            walk_expr(rhs, f);
        }
        Expr::Call { args, .. } | Expr::Spawn { args, .. } | Expr::TryConstruct { args, .. } => {
            for a in args {
                walk_expr(a, f);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            walk_expr(scrutinee, f);
            for a in arms {
                match &mut a.body {
                    ArmBody::Expr(e) => walk_expr(e, f),
                    ArmBody::Block(b) => walk_block(b, f),
                }
            }
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            walk_expr(cond, f);
            walk_expr(then_branch, f);
            if let Some(e) = else_branch {
                walk_expr(e, f);
            }
        }
        Expr::StructLit { fields, .. } => {
            for (_, v) in fields {
                walk_expr(v, f);
            }
        }
        Expr::ArrayLit { elems, .. } => {
            for v in elems {
                walk_expr(v, f);
            }
        }
        Expr::MapLit { entries, .. } => {
            for (k, v) in entries {
                walk_expr(k, f);
                walk_expr(v, f);
            }
        }
        Expr::Lambda { body, .. } => match body {
            LambdaBody::Expr(e) => walk_expr(e, f),
            LambdaBody::Block(b) => walk_block(b, f),
        },
    }
    f(e);
}

/// Apply `f` to every expression a whole program can hold, innermost-last.
///
/// Every body and every bare expression: functions, impl methods AND `place`
/// projections (a projection is never flattened into `Program::functions`),
/// tests, benches, module-state initializers and refinement predicates. Two
/// passes want exactly this walk — the loader stamps every `panic` with its
/// source site, and the parser hands a method-form builtin's name back to a
/// declaration that answers to it — so the list of places a body hides in is
/// written once.
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

/// [`walk_program`] over a bare expression — a global's initializer or a
/// refinement predicate, neither of which is a block.
pub(crate) fn walk_bare(e: &mut Expr, f: &mut impl FnMut(&mut Expr)) {
    let mut b = Block {
        stmts: vec![Stmt::Expr(std::mem::replace(e, Expr::Int(0)))],
    };
    walk_block(&mut b, f);
    let Some(Stmt::Expr(back)) = b.stmts.pop() else {
        unreachable!("one statement in, one statement out")
    };
    *e = back;
}

/// Is `e` a place — something with an address the access site can read from or
/// write to, rather than a value it would have to copy?
///
/// A variable, a field of a place, and an element of a place. Nothing else: a
/// call result, a literal and an arithmetic result are all values.
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

/// Does `b` use `?` anywhere? A projection may not: `?` propagates by
/// returning, and an inlined projection has no frame of its own to return from.
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

/// Does this program declare any `place` member at all?
///
/// An engine that must WORK to name a receiver's type asks this first. Where
/// the answer is no, every access site takes the seeded row and the work is
/// wasted — and in the direct backend the type probe is `&mut self`, so the
/// wasted work was also visible in the emitted bytes.
pub fn any(impls: &[ImplBlock]) -> bool {
    impls.iter().any(|i| !i.places.is_empty())
}

/// The `place` members of every impl in `p`, for the checker and the LSP.
pub fn all(p: &Program) -> impl Iterator<Item = (&ImplBlock, &Function)> {
    p.impls
        .iter()
        .flat_map(|i| i.places.iter().map(move |f| (i, f)))
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
        // A projection is never callable: it does not reach `functions`.
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
            name: "r".into(),
            line: 1,
        };
        let idx = Expr::Binary {
            op: crate::ast::BinOp::Add,
            lhs: Box::new(Expr::Var {
                name: "k".into(),
                line: 1,
            }),
            rhs: Box::new(Expr::Int(1)),
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

    /// The once-only contract is about EAGER evaluation. A parameter whose
    /// single use sits INSIDE a lambda body runs once per invocation of that
    /// lambda, so substituting the caller's expression in place would
    /// re-evaluate it on every call — it must bind a temporary like any
    /// multiply-used argument.
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
                name: "r".into(),
                line: 5,
            },
            &[Expr::Var {
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

    /// The load-bearing half of [`site`]: a builtin container has no user
    /// projection, so the site keeps its own nodes and no copy is ever built.
    /// It used to answer this by inlining the seeded row and comparing the
    /// result to what it already had, 20,205 times over the corpus.
    #[test]
    fn a_builtin_container_expands_to_nothing() {
        let recv = Expr::Var {
            name: "a".into(),
            line: 3,
        };
        let args = [Expr::Int(2)];
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
        // …and so does a receiver no engine could name.
        assert!(site(&[], None, "at", &recv, &args, 3).unwrap().is_none());
    }

    /// RFC-0091 M3. The increment is the loop body's FIRST statement, which is
    /// what makes `continue` step the loop instead of spinning on one element,
    /// and the condition tests `i + 1` to pay for it.
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
                name: "r".into(),
                line: 9,
            },
            &Block { stmts: Vec::new() },
            9,
        )
        .unwrap();
        // A place receiver binds no temporary: the loop reads the container
        // where it lives, which is what makes the element a borrow of it.
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

    /// An iterable that is not a place is a temporary, and evaluating it once
    /// per turn would run its side effects `size + 1` times.
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
                name: "makeRing".into(),
                args: Vec::new(),
                line: 9,
            },
            &Block { stmts: Vec::new() },
            9,
        )
        .unwrap();
        assert!(matches!(&blk.stmts[0], Stmt::Let { name, .. } if name == "@i.c"));
    }

    /// Both halves are required. An impl with a `size` and no `place nth` is not
    /// an iterable, and neither is the reverse.
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
                name: "r".into(),
                line: 1,
            },
            &[Expr::Int(3)],
            1,
        )
        .unwrap();
        assert_eq!(pr.prologue.len(), 1);
        assert!(
            matches!(&pr.prologue[0], Stmt::Let { name, .. } if name.starts_with("@b") && name.ends_with(".j"))
        );
    }

    /// A lambda parameter named like the projection parameter must be renamed
    /// out of the caller's namespace before substitution: `count_uses` walks
    /// through lambdas, so without the rename a single-use parameter was
    /// substituted IN PLACE inside the lambda body — `r[1]` on
    /// `let g = |i| i + 1  return self.data[g(0)]` read `data[2]`, not `data[1]`.
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
                name: "r".into(),
                line: 1,
            },
            &[Expr::Int(1)],
            1,
        )
        .unwrap();
        // The lambda owns the only textual use of `i`, so the caller's `1`
        // binds a temporary (the zero-uses path) instead of substituting in
        // place INSIDE the lambda body. The yielded place stays `data[g(0)]`,
        // and `g` still computes from its own argument: `r[1]` reads data[1].
        assert!(
            matches!(&pr.place, Expr::Call { name, .. } if name == "@at"),
            "the yielded place survived inlining"
        );
        assert!(
            pr.prologue.iter().any(
                |s| matches!(s, Stmt::Let { name, value: Expr::Int(1), .. } if name.starts_with("@p"))
            ),
            "the caller's argument binds a temporary, not a capture"
        );
        let mut seen_lambda = false;
        for s in &mut pr.prologue {
            walk_stmt(s, &mut |e: &mut Expr| match e {
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
        assert!(seen_lambda, "the lambda should have been walked");
    }
}
