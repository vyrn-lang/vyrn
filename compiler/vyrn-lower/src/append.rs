//! The String accumulators an append may grow in place, stated once for the
//! core's builder and the emitter: [`append_candidates`] for a body's locals,
//! and [`global_append_candidates`] for module state.

use vyrn_frontend::ast::*;

/// If `value` is `name + e1 + e2 + ...`, returns the appended operands in
/// written order. `+` is left-associative, so `name` sits at the bottom of the
/// left spine, not under the top `+`.
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

/// The local `String` accumulators of one function body that `s = s + ...` may
/// grow in place (`@strAppend`, [`crate::core::Spec::Rebuilds`]).
///
/// In-place growth reallocates and invalidates every other holder of the old
/// pointer, so the rule is a whitelist: a name qualifies only if every use of
/// it cannot retain the pointer (the root of a self-append, a `.field` read,
/// an operand of the interpolation desugar, a tail `return`). Anything else,
/// including an unknown callee, bans the name.
pub fn append_candidates(body: &Block) -> std::collections::HashSet<String> {
    let mut targets = std::collections::HashSet::new();
    let mut banned = std::collections::HashSet::new();
    scan_append_block(body, &mut targets, &mut banned, false);
    targets.retain(|n| !banned.contains(n));
    targets
}

/// `strict` marks a lambda body: everything inside one is banned, because a
/// capture copies the pointer into a value that outlives the append.
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
                    // `out = out + out` would read a buffer the realloc moved.
                    for p in parts {
                        ban_append_expr(p, banned, strict);
                    }
                }
                _ => ban_append_expr(value, banned, strict),
            },
            Stmt::SetField { value, .. } | Stmt::Expr(value, _) => {
                ban_append_expr(value, banned, strict)
            }
            Stmt::IndexSet { index, value, .. } => {
                ban_append_expr(index, banned, strict);
                ban_append_expr(value, banned, strict);
            }
            // Nothing can append after the frame returns the buffer.
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

/// Whether `op`'s lowering leaves an operand holding a String pointer it did
/// not copy; if so, its operands are banned. Every arm is `false` today, but
/// do not delete the guard: the match is exhaustive, so a new operator must
/// decide, and a wrong answer is a use-after-free.
///
/// - `+` on Strings allocates a fresh buffer. `Code + Code` takes arena handles.
/// - `== != < <= > >=` on Strings return `i1` from a `strcmp`.
/// - `=~` returns `i1`; its right operand is a literal pattern.
/// - `- * / % && || & | ^ << >>`: `binop_type` refuses a `String` operand.
///
/// A generic operand monomorphized to `String` reaches the same lowerings.
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

/// Bans every variable `e` mentions in a position that might retain it. The
/// match is exhaustive so that a new `Expr` variant must be classified.
fn ban_append_expr(e: &Expr, banned: &mut std::collections::HashSet<String>, strict: bool) {
    match e {
        Expr::Var { name, .. } => {
            banned.insert(name.clone());
        }
        // A String's fields are integer counts, not borrows.
        Expr::Field { expr, .. } => ban_append_read(expr, banned, strict),
        // The interpolation desugar's builtins copy their arguments. Only
        // `@`-names are safe: the lexer cannot produce a leading `@`, so no
        // local can shadow them with a function value that retains its input.
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
        // A take hands over the buffer itself.
        Expr::Consume { place, .. } => ban_append_expr(place, banned, strict),
        // Banning every operand would disqualify `return out + "]"`, which
        // makes a builder like `toJson` quadratic.
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
                    // A self-append in a block arm keeps the copying path: its
                    // targets are thrown away.
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
        Expr::Lambda { body, .. } => match body {
            LambdaBody::Expr(e) => ban_append_expr(e, banned, true),
            LambdaBody::Block(b) => {
                scan_append_block(b, &mut std::collections::HashSet::new(), banned, true)
            }
        },
        Expr::Int(_, _)
        | Expr::Byte(_, _)
        | Expr::Float(_, _)
        | Expr::Bool(_, _)
        | Expr::Str(_, _) => {}
    }
}

/// The module-state `String` accumulators of a whole program: the
/// [`append_candidates`] whitelist read over every body, because every body can
/// reach a global. A body that binds the name locally votes on neither side.
///
/// A `BTreeSet`, because the direct backend iterates it to reserve a static
/// address per accumulator; a hash order would make the module differ per run.
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
    // An initializer cannot append, but a name it reads is held.
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

/// Holds `program`'s module-state accumulators for one placement; drop clears
/// them.
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

/// Every name a block binds anywhere inside it. Over-collecting is safe: an
/// extra name only costs a global the in-place append path.
fn bound_names(b: &Block, out: &mut std::collections::HashSet<String>) {
    let mut locals = std::collections::HashSet::new();
    body_block(b, &mut locals, &mut BoundNames(out));
}

fn pattern_names(p: &Pattern) -> Vec<String> {
    p.bindings().into_iter().map(String::from).collect()
}

vyrn_frontend::body_scope_descent!(BodyVisit, body_block, body_stmt, body_expr);
