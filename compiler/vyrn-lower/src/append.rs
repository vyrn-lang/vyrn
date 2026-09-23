//! The String accumulators an append may grow in place, stated once for the
//! core's builder and the emitter: [`append_candidates`] for a body's locals,
//! and [`global_append_candidates`] for module state.

use vyrn_frontend::ast::*;

/// If `value` is `name + e1 + e2 + …` — a `+` chain whose left spine bottoms
/// out in a bare `name` — the appended operands in written order. The chain
/// matters: `out + a + ", "` parses as `Add(Add(Var(out), a), ", ")`, so the
/// accumulator sits at the far end of the spine, not under the top `+`.
pub fn self_append_spine<'e>(name: &str, value: &'e Expr) -> Option<Vec<&'e Expr>> {
    let mut parts: Vec<&Expr> = Vec::new();
    let mut cur = value;
    while let Expr::Binary {
        op: BinOp::Add,
        lhs,
        rhs,
        ..
    } = cur
    {
        parts.push(rhs);
        cur = lhs;
    }
    match cur {
        Expr::Var { name: n, .. } if n == name && !parts.is_empty() => {
            parts.reverse();
            Some(parts)
        }
        _ => None,
    }
}

/// The local `String` accumulators of one function body that `s = s + …` may
/// grow IN PLACE (`@strAppend`, [`crate::core::Spec::Rebuilds`]).
///
/// In-place growth reallocates, so every other holder of the old pointer is
/// invalidated — `let copy = out` before an append must keep reading "a". The
/// interpreter is safe here because `Rc::make_mut` clones a shared buffer;
/// native code has no refcount, so eligibility is decided statically and the
/// rule is a WHITELIST: a name qualifies only if every occurrence of it in the
/// function is a use that provably cannot retain the pointer — the root of a
/// self-append, a `.field` read (a String's fields are byte/char counts),
/// an operand of the interpolation desugar (which copies), or a tail
/// `return`. Anything else — another `let`, any user call, a record
/// field, an array element, a lambda body, an unrecognized builtin — bans the
/// name. An unknown callee is therefore ineligible by construction, so a new
/// retaining builtin cannot silently make this unsound.
pub fn append_candidates(body: &Block) -> std::collections::HashSet<String> {
    let mut targets = std::collections::HashSet::new();
    let mut banned = std::collections::HashSet::new();
    scan_append_block(body, &mut targets, &mut banned, false);
    targets.retain(|n| !banned.contains(n));
    targets
}

/// Walk a block collecting append targets and banned names. `strict` marks a
/// lambda body: everything inside one is banned outright, because a capture
/// copies the pointer into a value that outlives the append.
fn scan_append_block(
    b: &Block,
    targets: &mut std::collections::HashSet<String>,
    banned: &mut std::collections::HashSet<String>,
    strict: bool,
) {
    for s in &b.stmts {
        match s {
            Stmt::Let { value, .. } => ban_append_expr(value, banned, strict),
            Stmt::Assign { name, value, .. } => match self_append_spine(name, value) {
                Some(parts) if !strict => {
                    targets.insert(name.clone());
                    // The accumulator may not appear on the right as well:
                    // `out = out + out` would read a buffer the realloc moved.
                    for p in parts {
                        ban_append_expr(p, banned, strict);
                    }
                }
                _ => ban_append_expr(value, banned, strict),
            },
            Stmt::SetField { value, .. } | Stmt::Expr(value) => {
                ban_append_expr(value, banned, strict)
            }
            Stmt::IndexSet { index, value, .. } => {
                ban_append_expr(index, banned, strict);
                ban_append_expr(value, banned, strict);
            }
            // Returning the accumulator hands off the buffer at the point the
            // frame dies — nothing can append after it.
            Stmt::Return { value: Some(e), .. } => ban_append_read(e, banned, strict),
            // `drop s` frees the buffer; leave that path on the general lowering.
            Stmt::Drop { name, .. } => {
                banned.insert(name.clone());
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                ban_append_expr(cond, banned, strict);
                scan_append_block(then_block, targets, banned, strict);
                if let Some(eb) = else_block {
                    scan_append_block(eb, targets, banned, strict);
                }
            }
            Stmt::IfLet {
                scrutinee,
                then_block,
                else_block,
                ..
            } => {
                ban_append_expr(scrutinee, banned, strict);
                scan_append_block(then_block, targets, banned, strict);
                if let Some(eb) = else_block {
                    scan_append_block(eb, targets, banned, strict);
                }
            }
            Stmt::While { cond, body, .. } => {
                ban_append_expr(cond, banned, strict);
                scan_append_block(body, targets, banned, strict);
            }
            Stmt::ForIn { iter, body, .. } => {
                ban_append_expr(iter, banned, strict);
                scan_append_block(body, targets, banned, strict);
            }
            Stmt::Region { body, .. } => scan_append_block(body, targets, banned, strict),
            Stmt::Return { value: None, .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {}
        }
    }
}

/// A position that does not retain its operand: a bare variable there is fine.
fn ban_append_read(e: &Expr, banned: &mut std::collections::HashSet<String>, strict: bool) {
    if strict || !matches!(e, Expr::Var { .. }) {
        ban_append_expr(e, banned, strict);
    }
}

/// Does `op` leave one of its operands holding a String pointer it did not
/// copy? If so, an accumulator there could be left pointing at a buffer a later
/// in-place append `realloc`'d away, and the name must be banned.
///
/// Today the answer is no for all nineteen, so the guard in `ban_append_expr`
/// looks vacuous — it is not, and this function is why it may not be deleted:
/// the match is exhaustive so a new operator cannot be added without deciding,
/// and a String-borrowing operator (a `slice`-like `..`, say) would flip its
/// arm and re-ban its operands with no other change. Getting this wrong is a
/// use-after-free, so the decision is recorded per operator with its lowering:
///
/// - `+` on two Strings — `emit_str_concat`: `malloc(la+lb+1)` then
///   `strcpy`/`strcat`. A fresh buffer, which is exactly why `@concat` (what
///   `"\{out}]"` desugars to) is already whitelisted above. `Code + Code` takes
///   two arena handles, not pointers, and a `Code` name never owns a shadow.
/// - `== != < <= > >=` on two Strings — one `strcmp` and an `icmp`, result `i1`.
/// - `=~` — `@__vyrn_regex_run(ptr s, …)`, result `i1`; the right operand must
///   be a literal pattern, so only the left can even be an accumulator.
/// - `- * / % && || & | ^ << >>` — `binop_type` refuses a `String` operand
///   outright (arithmetic and bitwise need matching numerics, `&&`/`||` need
///   `Bool`), so no String reaches these lowerings at all.
///
/// A `<T: Ord>` operand monomorphized to `String` reaches the same lowerings
/// through the same operators, so the list covers generics too.
fn binop_retains_str(op: BinOp) -> bool {
    match op {
        BinOp::Add
        | BinOp::Eq
        | BinOp::NotEq
        | BinOp::Lt
        | BinOp::LtEq
        | BinOp::Gt
        | BinOp::GtEq
        | BinOp::Match => false,
        BinOp::Sub
        | BinOp::Mul
        | BinOp::Div
        | BinOp::Rem
        | BinOp::And
        | BinOp::Or
        | BinOp::BitAnd
        | BinOp::BitOr
        | BinOp::BitXor
        | BinOp::Shl
        | BinOp::Shr => false,
    }
}

/// Ban every variable `e` mentions in a position that might retain it. The
/// match is exhaustive on purpose: a new `Expr` variant must be classified
/// rather than silently fall into a permissive default.
fn ban_append_expr(e: &Expr, banned: &mut std::collections::HashSet<String>, strict: bool) {
    match e {
        Expr::Var { name, .. } => {
            banned.insert(name.clone());
        }
        // A String's fields are `byteLength`/`charCount` — an Int, not a borrow.
        Expr::Field { expr, .. } => ban_append_read(expr, banned, strict),
        // The two copying builtins the interpolation desugar emits: `@str`
        // strdups its argument, `@concat` builds a fresh buffer from both
        // halves. Only these two, because the lexer cannot produce a leading
        // `@` — no local binding can shadow the name and turn the call into a
        // dispatch through a stored function value that keeps what it is given.
        // (`print` is spellable, and `let print = f` does exactly that.)
        Expr::Call { name, args, .. } if matches!(name.as_str(), "@str" | "@concat") => {
            for a in args {
                ban_append_read(a, banned, strict);
            }
        }
        Expr::Call { args, .. }
        | Expr::TryConstruct { args, .. }
        | Expr::ArrayLit { elems: args, .. } => {
            for a in args {
                ban_append_expr(a, banned, strict);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try { expr, .. } => ban_append_expr(expr, banned, strict),
        // A take hands the stored place the buffer itself, so the root is
        // banned exactly as a bare mention is.
        Expr::Consume { place, .. } => ban_append_expr(place, banned, strict),
        // An operator's operands are a retaining position only if the LOWERING
        // keeps the pointer. `binop_retains_str` is the decision, exhaustive on
        // `BinOp` so a new operator cannot be added without making one.
        //
        // Banning every operand was the whole of `toJson`'s O(N²): `return out +
        // "]"` at the end of `std/json`'s `emitArr` disqualified `out`, so every
        // element re-`malloc`'d and re-copied the entire result so far (and
        // leaked the previous buffer, which is why 50k records OOM'd on 2.5 MB
        // of output). 80k `Int64` natively: 23.5 s before, 12 ms after. Forty
        // more `return acc + "…"` sites across `std/` were in the same trap.
        Expr::Binary { op, lhs, rhs, .. } => {
            if binop_retains_str(*op) {
                ban_append_expr(lhs, banned, strict);
                ban_append_expr(rhs, banned, strict);
            } else {
                ban_append_read(lhs, banned, strict);
                ban_append_read(rhs, banned, strict);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            ban_append_expr(scrutinee, banned, strict);
            for arm in arms {
                match &arm.body {
                    ArmBody::Expr(e) => ban_append_expr(e, banned, strict),
                    // A block arm (RFC-0118): the throwaway target set means a
                    // self-append inside it keeps the copying path — correct,
                    // just not upgraded; the in-place path can follow demand.
                    ArmBody::Block(b) => {
                        scan_append_block(b, &mut std::collections::HashSet::new(), banned, strict)
                    }
                }
            }
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            ban_append_expr(cond, banned, strict);
            ban_append_expr(then_branch, banned, strict);
            if let Some(eb) = else_branch {
                ban_append_expr(eb, banned, strict);
            }
        }
        Expr::StructLit { fields, .. } => {
            for (_, v) in fields {
                ban_append_expr(v, banned, strict);
            }
        }
        Expr::MapLit { entries, .. } => {
            for (k, v) in entries {
                ban_append_expr(k, banned, strict);
                ban_append_expr(v, banned, strict);
            }
        }
        // A capture outlives the append, so nothing a lambda touches is eligible.
        Expr::Lambda { body, .. } => match body {
            LambdaBody::Expr(e) => ban_append_expr(e, banned, true),
            LambdaBody::Block(b) => {
                scan_append_block(b, &mut std::collections::HashSet::new(), banned, true)
            }
        },
        Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {}
    }
}

/// The **module-state** `String` accumulators of a whole program (census P1).
///
/// The same whitelist, read over every body instead of one, because a global is
/// reachable from all of them: a name qualifies when some body grows it with
/// `g = g + …` and NO body puts a pointer to it anywhere that could outlive the
/// grow. `let t = g` is one of the things that bans a name, which is exactly the
/// aliasing guard a global needs and a local already had.
///
/// P1 measured what not having this costs: 4.92 s and 12.2 GB to build a 160 KB
/// string, against 0.095 s for the identical local. The global did not qualify
/// for one reason — the whitelist read one body — and every server that
/// accumulates a response body is a module-state accumulator.
///
/// A body that binds the name LOCALLY votes on neither side, because inside it
/// the name is not the global. Without that filter one `let out` among the
/// hundreds of linked `std/` functions disqualifies a module-state `out`, and the
/// first measurement of this pass hit exactly that.
/// The result is a `BTreeSet` and not a `HashSet` because one caller ITERATES
/// it: the direct backend reserves an ownership word per accumulator, and a
/// reservation is an address baked into every `i32.const` that reads or writes
/// it — and it shifts every later reservation, so the whole static map moves.
/// `RandomState` is seeded per process, so two accumulators were a coin flip and
/// three built six different modules from one source. Sorted here rather than at
/// that loop, because the next caller to iterate it would have to know.
pub fn global_append_candidates(program: &Program) -> std::collections::BTreeSet<String> {
    let mut targets = std::collections::HashSet::new();
    let mut banned = std::collections::HashSet::new();
    let mut one = |body: &Block, params: &[Param]| {
        let (mut t, mut ban) = (
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
        );
        scan_append_block(body, &mut t, &mut ban, false);
        let mut shadowed: std::collections::HashSet<String> =
            params.iter().map(|p| p.name.clone()).collect();
        bound_names(body, &mut shadowed);
        targets.extend(t.into_iter().filter(|n| !shadowed.contains(n)));
        banned.extend(ban.into_iter().filter(|n| !shadowed.contains(n)));
    };
    for f in &program.functions {
        one(&f.body, &f.params);
    }
    for t in &program.tests {
        one(&t.body, &[]);
    }
    for bn in &program.benches {
        one(&bn.body, &[]);
    }
    // A global's own initializer runs once and cannot append, but a name it reads
    // is a name held somewhere this walk should see.
    for g in &program.globals {
        ban_append_expr(&g.init, &mut banned, false);
    }
    targets.retain(|n| !banned.contains(n));
    targets.retain(|n| program.globals.iter().any(|g| &g.name == n));
    targets.into_iter().collect()
}

thread_local! {
    /// [`global_append_candidates`] of the program `core::augment` is
    /// placing, which every body it builds asks; empty outside it.
    static HELD: std::cell::RefCell<std::collections::BTreeSet<String>> =
        const { std::cell::RefCell::new(std::collections::BTreeSet::new()) };
}

/// Holds `program`'s module-state accumulators for the builds of one
/// placement, and lets them go when dropped.
pub(crate) struct Held;

impl Held {
    pub(crate) fn new(program: &Program) -> Held {
        HELD.with(|h| *h.borrow_mut() = global_append_candidates(program));
        Held
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        HELD.with(|h| h.borrow_mut().clear());
    }
}

/// Whether the module-state binding `name` is a String accumulator of the
/// program being placed.
pub(crate) fn global_grows(name: &str) -> bool {
    HELD.with(|h| h.borrow().contains(name))
}

/// The collector's line at each site: a `let`, a loop variable, an `if let` or
/// arm binder, a lambda parameter.
struct BoundNames<'a>(&'a mut std::collections::HashSet<String>);

impl BodyVisit<'_> for BoundNames<'_> {
    // The union of every name bound anywhere, not what is in scope where.
    const SCOPED: bool = false;

    fn stmt(&mut self, s: &Stmt, _: &std::collections::HashSet<String>) {
        match s {
            Stmt::Let { name, .. } => {
                self.0.insert(name.clone());
            }
            Stmt::ForIn { var, .. } => {
                self.0.insert(var.clone());
            }
            Stmt::IfLet { pattern, .. } => self.0.extend(pattern_names(pattern)),
            _ => {}
        }
    }

    fn expr(&mut self, e: &Expr, _: &std::collections::HashSet<String>) -> bool {
        if let Expr::Lambda { params, .. } = e {
            self.0.extend(params.iter().map(|p| p.name.clone()));
        }
        true
    }

    fn arm_pattern(&mut self, p: &Pattern, _: usize, _: &std::collections::HashSet<String>) {
        self.0.extend(pattern_names(p));
    }
}

/// Every name a block binds anywhere inside it — `let`s, loop variables, pattern
/// binders and lambda parameters. Over-collecting is safe here: the only use is
/// to decide that a body is talking about its own name rather than about module
/// state, and an extra name only costs a global the in-place append path.
fn bound_names(b: &Block, out: &mut std::collections::HashSet<String>) {
    let mut locals = std::collections::HashSet::new();
    body_block(b, &mut locals, &mut BoundNames(out));
}

/// The names a refutable pattern binds.
fn pattern_names(p: &Pattern) -> Vec<String> {
    p.bindings().into_iter().map(String::from).collect()
}

vyrn_frontend::body_scope_descent!(BodyVisit, body_block, body_stmt, body_expr);
