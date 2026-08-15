//! The lowered form — RFC-0101 M1, amended by M3.
//!
//! > The checker's answers become a value. One lowering produces it. A backend
//! > reads it and encodes it, and decides nothing the lowering already decided.
//!
//! M1 builds the value and nothing consumes it in anger. What it holds today is
//! the first item on RFC-0101 §2.1's list and the second and the sixth: concrete
//! function bodies one per instantiation, a type on every expression node, and
//! the line each node came from. Release steps, resolved traps and resolved
//! dispatch (items 3, 4 and 5) arrive in M4 and M5.
//!
//! **It borrows.** Open question 6.1 is answered "borrow, during the migration":
//! a lowered node carries the `&Expr` it came from, which is the only thing that
//! lets an engine migrate one arm at a time and fall back to its old walk for
//! the rest. The cost of the owned form against this one is measured in
//! `vyrn-cli/tests/lowered.rs` and written into the RFC.
//!
//! **It derives one thing, and M3 is where that stopped being none.** Every
//! [`Row::ty`] is the checker's own answer, read out of
//! [`vyrn_frontend::checker::record`] and substituted through the instantiation
//! the body is being lowered for — a lowering that re-derived THAT would be a
//! sixth copy of the derivation RFC-0101 §1.2 counts five of. [`Row::has`] is
//! the other question, and the checker holds no answer to it: it types every
//! expression against its destination. So [`has_of`] derives it, in one closed
//! table, which is the derivation `peek` and `static_ty` are — written once
//! below both backends instead of twice inside them.

mod render;
pub use render::render;

use std::collections::{BTreeMap, HashMap, VecDeque};

use vyrn_frontend::ast::{Block, Expr, Function, LambdaBody, Program, Stmt, Type};
use vyrn_frontend::checker;
use vyrn_frontend::types::{
    expanded_size, mentions_param, substitute, type_depth, MONO_DEPTH_LIMIT, MONO_SIZE_LIMIT,
};

/// The text rendering's version line, printed by `vyrn emit-lowered`.
///
/// It promises nothing (RFC-0101 §6.5, following rustc: "subject to change
/// without notice"). Stability is a blessed snapshot, not a contract.
pub const VERSION: &str = "v1";

/// An AST node a lowered row came from. The identity is the address, which is
/// what `own` and `movecheck` key on already (RFC-0101 §2.5).
#[derive(Debug, Clone, Copy)]
pub enum Node<'a> {
    Stmt(&'a Stmt),
    Expr(&'a Expr),
}

impl Node<'_> {
    /// The address this node is keyed by — the same expression both backends
    /// compute for `arg_drops` and `droppable`.
    pub fn id(&self) -> usize {
        match self {
            Node::Stmt(s) => *s as *const Stmt as usize,
            Node::Expr(e) => *e as *const Expr as usize,
        }
    }

    /// What the node is, in one word — the head of a dump line and the axis a
    /// disagreement is classified on, because "the backends disagree about
    /// `call`" is a finding and "they disagree at foo.vyrn:41" is an anecdote.
    pub fn kind(&self) -> &'static str {
        match self {
            Node::Stmt(s) => match s {
                Stmt::Let { .. } => "let",
                Stmt::Assign { .. } => "assign",
                Stmt::SetField { .. } => "setfield",
                Stmt::IndexSet { .. } => "indexset",
                Stmt::Return { .. } => "return",
                Stmt::Break { .. } => "break",
                Stmt::Continue { .. } => "continue",
                Stmt::If { .. } => "if",
                Stmt::IfLet { .. } => "iflet",
                Stmt::While { .. } => "while",
                Stmt::ForIn { .. } => "for",
                Stmt::Drop { .. } => "drop",
                Stmt::Expr(_) => "do",
                Stmt::Region { .. } => "region",
            },
            Node::Expr(e) => match e {
                Expr::Int(_) => "int",
                Expr::Byte(_) => "byte",
                Expr::Float(_) => "float",
                Expr::Bool(_) => "bool",
                Expr::Str(_) => "str",
                Expr::Var { .. } => "var",
                Expr::Unary { .. } => "unary",
                Expr::Binary { .. } => "binary",
                Expr::Call { .. } => "call",
                Expr::Match { .. } => "match",
                Expr::IfExpr { .. } => "ifexpr",
                Expr::Try { .. } => "try",
                Expr::StructLit { .. } => "record",
                Expr::Field { .. } => "field",
                Expr::TryConstruct { .. } => "tryconstruct",
                Expr::ArrayLit { .. } => "array",
                Expr::MapLit { .. } => "map",
                Expr::Spawn { .. } => "spawn",
                Expr::Lambda { .. } => "lambda",
                Expr::Consume { .. } => "consume",
            },
        }
    }
}

/// One decision, one row. Indentation is structure; the position is last.
#[derive(Debug, Clone)]
pub struct Row<'a> {
    pub depth: u16,
    pub line: u32,
    pub node: Node<'a>,
    /// The type the checker gave this expression, substituted for the
    /// instantiation this body is. `None` for a statement row, and for an
    /// expression the checker never routed through `Checker::expr` — 78 over
    /// the whole corpus since M3b, and `vyrn-cli/tests/lowered.rs` names the
    /// class they all belong to.
    ///
    /// This is the type the value must END UP as: the destination the checker
    /// validated the node against. See [`Row::has`] for the other half.
    pub ty: Option<Type>,
    /// The type the value HAS when this node's code has run, before the
    /// `coerce` that follows — RFC-0101 §2.1 item 2, amendment **[A16]**.
    ///
    /// `None` means "the same as [`Row::ty`]", which is the ordinary case: the
    /// two questions have one answer at every node whose own form settles its
    /// type. Where they differ, the difference is the context: `1` under an
    /// `Int32` destination HAS an `Int64` and must END UP an `Int32`, and both
    /// engines are right about a different question. M1 measured that single
    /// difference as 21,140 of its 22,283 disagreements.
    pub has: Option<Type>,
}

impl Row<'_> {
    /// The pair, as a backend reads it: what the value has, and what it must end
    /// up as. Either may be absent for a node the checker never typed.
    pub fn pair(&self) -> (Option<&Type>, Option<&Type>) {
        (self.has.as_ref().or(self.ty.as_ref()), self.ty.as_ref())
    }
}

/// One function, instantiated. Zig's shape (RFC-0101 §2.1 item 1): no type
/// parameter survives, and the identity is the type arguments rather than a
/// mangled string, which is the defect #165 was.
#[derive(Debug, Clone)]
pub struct Instance<'a> {
    pub func: &'a Function,
    /// The type arguments, in the function's own type-parameter order.
    pub type_args: Vec<Type>,
    /// The same thing keyed by name, which is what a substitution needs.
    pub subst: BTreeMap<String, Type>,
    pub rows: Vec<Row<'a>>,
}

impl Instance<'_> {
    /// `map<Int64, String>` — the instantiation spelled, never mangled.
    pub fn spelling(&self) -> String {
        if self.type_args.is_empty() {
            return self.func.name.clone();
        }
        let args: Vec<String> = self.type_args.iter().map(|t| t.to_string()).collect();
        format!("{}<{}>", self.func.name, args.join(", "))
    }

    /// The module this instance's function was declared in; `""` for the root.
    pub fn module(&self) -> &str {
        self.func.module.as_deref().unwrap_or("")
    }
}

/// Why a generic call did not become an instantiation.
///
/// M1 kept one counter for three facts and M2 measured what that cost: the only
/// entry the corpus produces is `PastTheLimit`, on `examples/polyrecursion.vyrn`,
/// which is the bound WORKING. A counter that cannot reach zero without deleting
/// a limit is not a residue counter, so the reason is now a value the gate reads
/// rather than a sentence it prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// The name is not a function of this linked program.
    NotAFunction,
    /// The checker left a type parameter unsolved at the call.
    UnsolvedParameter,
    /// [`MONO_DEPTH_LIMIT`] or [`MONO_SIZE_LIMIT`] refused the instantiation —
    /// the same refusal both backends make, from the same two constants.
    PastTheLimit,
}

impl std::fmt::Display for Why {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Why::NotAFunction => "the callee is not a function of this program",
            Why::UnsolvedParameter => "the checker left a type parameter for a backend to solve",
            Why::PastTheLimit => "the instantiation passes the monomorphization limit",
        })
    }
}

/// A generic call the lowering could not turn into an instantiation.
#[derive(Debug, Clone)]
pub struct Unresolved {
    pub caller: String,
    pub callee: String,
    /// The line the CALLEE is declared on, which is what a refusal names — the
    /// author has to change the generic, not the call.
    pub line: usize,
    /// The type arguments the call solved, where it solved them. Empty when the
    /// checker left a parameter open. A refusal is worded from these, so
    /// `vyrn check` can say what a backend would have said without running one.
    pub args: Vec<Type>,
    pub why: Why,
}

/// The checked program with the answers written on it and the sugar gone.
#[derive(Debug, Clone)]
pub struct Lowered<'a> {
    /// Sorted by module, then name, then rendered type arguments — never
    /// printed from a `HashMap` (RFC-0101 §2.7).
    pub instances: Vec<Instance<'a>>,
    /// The module-state initializers (RFC-0013), in declaration order.
    ///
    /// They are not a function, and both backends emit them into a synthesized
    /// one; M1's worklist rooted at `program.functions` and so never walked them,
    /// which is why `std/stream`'s `let mut cells: Slots<CursorCell>` reached the
    /// native backend as an instantiation the lowering did not have.
    pub globals: Vec<Row<'a>>,
    /// Where the worklist stopped, and why. Every corpus entry is
    /// [`Why::PastTheLimit`] — the bound refusing an instantiation, which is the
    /// bound working rather than a hole.
    pub unresolved: Vec<Unresolved>,
}

impl<'a> Lowered<'a> {
    /// The instances declared in the root module — what `vyrn emit-lowered`
    /// prints by default, following `vyrn why --memory`'s rule: only the file
    /// asked about, because a linked program's imports are another file's answer.
    pub fn root(&self) -> impl Iterator<Item = &Instance<'a>> {
        self.instances.iter().filter(|i| i.func.module.is_none())
    }

    pub fn rows(&self) -> usize {
        self.instances.iter().map(|i| i.rows.len()).sum::<usize>() + self.globals.len()
    }
}

/// Lower a checked program.
///
/// `program` must already be through `check_and_synthesize`: the synthesized
/// JSON codecs are ordinary Vyrn functions and are lowered like any other, which
/// is only true if they are in the program when the checker runs over it here.
pub fn lower(program: &Program) -> Lowered<'_> {
    let recorded = checker::record(program);
    let mut lowered = build(program, &recorded);
    lowered.instances.sort_by(|a, b| {
        (a.module(), &a.func.name, a.spelling()).cmp(&(b.module(), &b.func.name, b.spelling()))
    });
    debug_assert!(
        lint(&lowered).is_empty(),
        "the lowered form failed its own lint:\n  {}",
        lint(&lowered).join("\n  ")
    );
    lowered
}

/// The walk's state: what the checker recorded, where the rows go, and which
/// calls the worklist has to follow out of this body.
struct Walk<'a, 'r> {
    recorded: &'r checker::Recorded,
    /// The program's `impl` blocks, for the one thing the form has to expand
    /// rather than read: a `place at` projection is inlined at its access site,
    /// so the nodes an engine walks there are not the nodes the source wrote.
    /// The lowering asks [`vyrn_frontend::project::site`] for them, and so do
    /// both backends — one expansion, one set of addresses, one row each.
    impls: &'a [vyrn_frontend::ast::ImplBlock],
    rows: Vec<Row<'a>>,
    /// `(callee, its solved type arguments by name)`, already concrete.
    calls: Vec<(&'r str, HashMap<String, Type>)>,
}

/// The substitutions in scope at a node, outermost first.
///
/// A generic call's ARGUMENTS are checked against the callee's parameter types,
/// so the answer the checker wrote on `[]` in `push(xs, [])` is `Array<T>` — the
/// callee's `T`, before unification solved it. The solution is recorded on the
/// call node, so the fix is to apply it to the subtree it governs. Applying the
/// stack in order rather than merging it is what keeps a caller's `T` and a
/// callee's `T` apart: the outer substitution makes the caller's concrete first,
/// and the inner one can then only reach what is left.
type Chain = Vec<HashMap<String, Type>>;

fn apply(ty: &Type, chain: &Chain) -> Type {
    chain.iter().fold(ty.clone(), |t, s| substitute(&t, s))
}

fn build<'a>(program: &'a Program, recorded: &checker::Recorded) -> Lowered<'a> {
    let by_name: HashMap<&str, &Function> = program
        .functions
        .iter()
        .map(|f| (f.name.as_str(), f))
        .collect();
    let decls = vyrn_frontend::types::decl_map(program);

    // The roots are every non-generic function: that is what both backends emit
    // before either worklist turns, and a generic body is reachable only from
    // one of them.
    let mut queue: VecDeque<(&Function, Vec<Type>)> = program
        .functions
        .iter()
        .filter(|f| f.type_params.is_empty() && !f.is_extern)
        .map(|f| (f, Vec::new()))
        .collect();
    let mut seen: Vec<(String, String)> = queue
        .iter()
        .map(|(f, _)| (f.name.clone(), String::new()))
        .collect();

    let mut instances: Vec<Instance<'a>> = Vec::new();
    let mut unresolved: Vec<Unresolved> = Vec::new();

    // The worklist's second root: module state. A `let` at module scope is an
    // ordinary expression the backends run inside a synthesized initializer, and
    // it instantiates generics like any other body — `std/stream`'s
    // `let mut cells: Slots<CursorCell> = newSlots()` is the corpus's proof.
    let mut gw = Walk {
        recorded,
        impls: &program.impls,
        rows: Vec::new(),
        calls: Vec::new(),
    };
    for g in &program.globals {
        let mut chain: Chain = vec![HashMap::new()];
        expr(&g.init, 0, &mut chain, &mut gw);
    }
    let globals = gw.rows;
    follow(
        "<module state>",
        std::mem::take(&mut gw.calls),
        &by_name,
        &decls,
        &mut seen,
        &mut queue,
        &mut unresolved,
    );

    while let Some((func, type_args)) = queue.pop_front() {
        let subst: BTreeMap<String, Type> = func
            .type_params
            .iter()
            .cloned()
            .zip(type_args.iter().cloned())
            .collect();
        let flat: HashMap<String, Type> = subst.clone().into_iter().collect();

        let mut w = Walk {
            recorded,
            impls: &program.impls,
            rows: Vec::new(),
            calls: Vec::new(),
        };
        let mut chain: Chain = vec![flat];
        block(&func.body, 0, &mut chain, &mut w);

        follow(
            &func.name,
            std::mem::take(&mut w.calls),
            &by_name,
            &decls,
            &mut seen,
            &mut queue,
            &mut unresolved,
        );

        instances.push(Instance {
            func,
            type_args,
            subst,
            rows: w.rows,
        });
    }

    Lowered {
        instances,
        globals,
        unresolved,
    }
}

/// Turn the generic calls one body made into instantiations on the worklist.
///
/// One function rather than one per root, because a module-state initializer
/// discovers instantiations exactly the way a function body does and a second
/// copy of this is a second set of rules for the same decision — which is the
/// defect shape RFC-0101 is about.
#[allow(clippy::too_many_arguments)]
fn follow<'a>(
    caller: &str,
    calls: Vec<(&str, HashMap<String, Type>)>,
    by_name: &HashMap<&str, &'a Function>,
    decls: &HashMap<String, vyrn_frontend::ast::TypeDecl>,
    seen: &mut Vec<(String, String)>,
    queue: &mut VecDeque<(&'a Function, Vec<Type>)>,
    unresolved: &mut Vec<Unresolved>,
) {
    for (callee, solved) in calls {
        let mut stop = |why, line, args: Vec<Type>| {
            unresolved.push(Unresolved {
                caller: caller.to_string(),
                callee: callee.to_string(),
                line,
                args,
                why,
            })
        };
        let Some(target) = by_name.get(callee) else {
            stop(Why::NotAFunction, 0, Vec::new());
            continue;
        };
        if target.type_params.iter().any(|p| !solved.contains_key(p)) {
            stop(Why::UnsolvedParameter, target.line, Vec::new());
            continue;
        }
        let next: Vec<Type> = target
            .type_params
            .iter()
            .map(|p| solved[p].clone())
            .collect();
        // The same bound both backends apply, from the same constant.
        // Polymorphic recursion — `f<T>` calling `f<P<T>>` — has no fixed
        // point, so a worklist without this runs until the machine stops;
        // `examples/polyrecursion.vyrn` is the corpus entry that proves it,
        // and it reached 18 GiB here before the bound was wired in.
        if next.iter().any(|t| {
            type_depth(t) > MONO_DEPTH_LIMIT || expanded_size(t, decls, MONO_SIZE_LIMIT).is_none()
        }) {
            stop(Why::PastTheLimit, target.line, next);
            continue;
        }
        let key = (
            target.name.clone(),
            next.iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
                .join(","),
        );
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        queue.push_back((target, next));
    }
}

// ---- the walk ------------------------------------------------------------
//
// Pre-order, statement then its expressions then its nested blocks, so the row
// order IS the reading order and a dump indents by `depth` and nothing else.

fn block<'a>(b: &'a Block, depth: u16, chain: &mut Chain, w: &mut Walk<'a, '_>) {
    for s in &b.stmts {
        stmt(s, depth, chain, w);
    }
}

fn stmt<'a>(s: &'a Stmt, depth: u16, chain: &mut Chain, w: &mut Walk<'a, '_>) {
    let line = stmt_line(s) as u32;
    let here = w.rows.len();
    w.rows.push(Row {
        depth,
        line,
        node: Node::Stmt(s),
        ty: None,
        has: None,
    });
    let d = depth + 1;
    match s {
        Stmt::Let { value, ty, .. } => {
            expr(value, d, chain, w);
            // The binding's type on the binding's line, which is what makes
            // `grep ': Array<'` a whole query (RFC-0101 §2.7). Declared where
            // the source declared one, and otherwise the value's own answer —
            // the same order the checker settles it in.
            w.rows[here].ty = match ty {
                Some(t) => Some(apply(t, chain)),
                None => w.rows.get(here + 1).and_then(|r| r.ty.clone()),
            };
        }
        Stmt::Assign { value, .. } | Stmt::SetField { value, .. } => {
            expr(value, d, chain, w);
        }
        Stmt::IndexSet { index, value, .. } => {
            expr(index, d, chain, w);
            expr(value, d, chain, w);
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                expr(v, d, chain, w);
            }
        }
        Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Drop { .. } => {}
        Stmt::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            expr(cond, d, chain, w);
            block(then_block, d, chain, w);
            if let Some(e) = else_block {
                block(e, d, chain, w);
            }
        }
        Stmt::IfLet {
            scrutinee,
            then_block,
            else_block,
            ..
        } => {
            expr(scrutinee, d, chain, w);
            block(then_block, d, chain, w);
            if let Some(e) = else_block {
                block(e, d, chain, w);
            }
        }
        Stmt::While { cond, body, .. } => {
            expr(cond, d, chain, w);
            block(body, d, chain, w);
        }
        Stmt::ForIn {
            var, iter, body, ..
        } => {
            expr(iter, d, chain, w);
            block(body, d, chain, w);
            // A `for` over a user container is a desugar too: the loop the
            // engines walk is `place nth` inlined per turn around a COPY of the
            // body above. Same expansion, same nodes, one walk each.
            if let Some(blk) = iterate(var, iter, body, chain, w) {
                block(blk, d, chain, w);
            }
        }
        Stmt::Expr(e) => {
            expr(e, d, chain, w);
        }
        Stmt::Region { body, .. } => block(body, d, chain, w),
    }
    // A literal carries no line of its own — `ast::Expr::line` returns 0 for the
    // five of them, and the parser has none to give. Inheriting the enclosing
    // statement's is a decision made ONCE, here, rather than five times in the
    // engines that print a position; §2.1 item 6 promises every node a line.
    for r in &mut w.rows[here..] {
        if r.line == 0 {
            r.line = line;
        }
    }
}

fn expr<'a>(e: &'a Expr, depth: u16, chain: &mut Chain, w: &mut Walk<'a, '_>) -> usize {
    let key = e as *const Expr as usize;
    let ty = w.recorded.node_types.get(&key).map(|t| apply(t, chain));
    let here = w.rows.len();
    w.rows.push(Row {
        depth,
        line: e.line() as u32,
        node: Node::Expr(e),
        ty,
        has: None,
    });
    // A generic call solves its callee's parameters, and the answer governs the
    // subtree it was solved from — see [`Chain`].
    let pushed = match w.recorded.node_substs.get(&key) {
        Some((callee, args)) => {
            let solved: HashMap<String, Type> = args
                .iter()
                .map(|(p, t)| (p.clone(), apply(t, chain)))
                .collect();
            // A record literal solves parameters too, and it is not a call:
            // only a call adds an instance to the worklist.
            if matches!(e, Expr::Call { .. } | Expr::Spawn { .. }) {
                w.calls.push((callee.as_str(), solved.clone()));
            }
            chain.push(solved);
            true
        }
        None => false,
    };
    let d = depth + 1;
    // The direct children's row indices, in written order — what the has-type
    // below is derived from, and the only reason this walk returns an index.
    let mut kids: Vec<usize> = Vec::new();
    match e {
        Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {}
        Expr::Var { .. } => {}
        Expr::Unary { expr: inner, .. }
        | Expr::Try { expr: inner, .. }
        | Expr::Field { expr: inner, .. } => kids.push(expr(inner, d, chain, w)),
        Expr::Consume { place, .. } => kids.push(expr(place, d, chain, w)),
        Expr::Binary { lhs, rhs, .. } => {
            kids.push(expr(lhs, d, chain, w));
            kids.push(expr(rhs, d, chain, w));
        }
        Expr::Call { name, args, .. } if name == vyrn_frontend::project::AT => {
            for a in args {
                kids.push(expr(a, d, chain, w));
            }
            // …and the expansion, which is the rest of what this site MEANS.
            // Not a child: `has_of` derives an `@at`'s has-type from the
            // receiver, and the expansion is the same answer arrived at the
            // long way. The rows are here so a backend walking those nodes is
            // walking nodes this form has answers for.
            desugar(e, args, d, chain, w);
        }
        Expr::Call { args, .. } | Expr::TryConstruct { args, .. } | Expr::Spawn { args, .. } => {
            for a in args {
                kids.push(expr(a, d, chain, w));
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            kids.push(expr(scrutinee, d, chain, w));
            for arm in arms {
                kids.push(expr(&arm.body, d, chain, w));
            }
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            kids.push(expr(cond, d, chain, w));
            kids.push(expr(then_branch, d, chain, w));
            if let Some(b) = else_branch {
                kids.push(expr(b, d, chain, w));
            }
        }
        Expr::StructLit { fields, .. } => {
            for (_, v) in fields {
                kids.push(expr(v, d, chain, w));
            }
        }
        Expr::ArrayLit { elems, .. } => {
            for el in elems {
                kids.push(expr(el, d, chain, w));
            }
        }
        Expr::MapLit { entries, .. } => {
            for (k, v) in entries {
                kids.push(expr(k, d, chain, w));
                kids.push(expr(v, d, chain, w));
            }
        }
        Expr::Lambda { body, .. } => match body {
            LambdaBody::Expr(b) => kids.push(expr(b, d, chain, w)),
            LambdaBody::Block(b) => block(b, d, chain, w),
        },
    }
    let own = has_of(e, &kids, w);
    if own.as_ref() != w.rows[here].ty.as_ref() {
        w.rows[here].has = own;
    }
    if pushed {
        chain.pop();
    }
    here
}

/// The default an unconstrained position gets. Both compiled backends write
/// `Int64` there — the element of an empty container, the unused side of a
/// `Result`, a `None` whose payload nothing names — because an integer is what
/// a machine word is, and nothing else in the program constrains it.
const UNCONSTRAINED: Type = Type::Int;

/// The type this node's own code produces, ignoring the destination — RFC-0101
/// §2.1 item 2 [A16].
///
/// The checker cannot answer this: it types an expression against the type the
/// context wants, and that answer is the OTHER half of the pair. So the form
/// derives this half, from the node's own shape and its children's, in one
/// place. That derivation is the one M3 deletes `peek` and `static_ty` in favour
/// of, which is why it is a closed table here and not a second checker: every
/// arm below is a node whose form settles its type without asking the context,
/// and everything not in it answers `ty`.
fn has_of(e: &Expr, kids: &[usize], w: &Walk<'_, '_>) -> Option<Type> {
    let kid = |i: usize| -> Option<Type> {
        let r = w.rows.get(kids.get(i).copied()?)?;
        r.has.clone().or_else(|| r.ty.clone())
    };
    Some(match e {
        // A numeric literal is its own width, and the destination is a coercion
        // away. `Byte` too: both backends spell `'a'` as an `i64` immediate and
        // narrow at the use, which the checker does not — it answers `UInt8`.
        Expr::Int(_) | Expr::Byte(_) => Type::Int,
        Expr::Float(_) => Type::Float,
        // A pass-through: the node emits its child's value.
        Expr::Consume { .. } | Expr::Unary { .. } => kid(0)?,
        // A join carries the type of a branch, not of its destination. `panic`
        // in the then-branch makes it `Never`, and the else answers.
        Expr::IfExpr { .. } => match kid(1)? {
            Type::Never => kid(2)?,
            t => t,
        },
        // A `match` is typed by its arms, and a `match` whose every arm leaves
        // the function produces no value to have a type — the bottom, which is
        // what both backends answer and the checker cannot, because the checker
        // is answering about the destination.
        Expr::Match { arms, .. } => {
            let mut t = Type::Never;
            for i in 0..arms.len() {
                match kid(i + 1) {
                    Some(Type::Never) => {}
                    Some(x) => {
                        t = x;
                        break;
                    }
                    None => return None,
                }
            }
            t
        }
        // A literal container is its elements' type, and an empty one has no
        // element to be typed by at all. A written array is a FIXED-size one
        // until something stores it somewhere growable.
        Expr::ArrayLit { elems, .. } => {
            if elems.is_empty() {
                Type::Array(Box::new(UNCONSTRAINED))
            } else {
                Type::ArrayN(Box::new(kid(0)?), elems.len())
            }
        }
        Expr::MapLit { entries, .. } => Type::Map(
            Box::new(Type::Str),
            Box::new(if entries.is_empty() {
                UNCONSTRAINED
            } else {
                kid(1)?
            }),
        ),
        // A sum constructor names one side, and the other is unconstrained.
        Expr::Var { name, .. } if name == "None" => Type::Option(Box::new(UNCONSTRAINED)),
        Expr::Call { name, args, .. } if args.len() == 1 => match name.as_str() {
            "Some" => Type::Option(Box::new(kid(0)?)),
            "Ok" => Type::Result(Box::new(kid(0)?), Box::new(UNCONSTRAINED)),
            "Err" => Type::Result(Box::new(UNCONSTRAINED), Box::new(kid(0)?)),
            _ => return None,
        },
        _ => return None,
    })
}

/// Walk the expansion of an `@at` access site, if the receiver has a user
/// `place at`.
///
/// The receiver's type is the checker's own answer at the receiver node, which
/// is what both backends work out for themselves at the same site — one with
/// `static_ty`, the other with `peek`. All three then ask
/// [`vyrn_frontend::project::site`], which expands once and hands the same tree
/// to each of them (RFC-0101's desugar-once milestone).
fn desugar<'a>(e: &'a Expr, args: &'a [Expr], depth: u16, chain: &mut Chain, w: &mut Walk<'a, '_>) {
    if args.len() != 2 || w.impls.is_empty() {
        return;
    }
    let key = &args[0] as *const Expr as usize;
    let Some(recv) = w.recorded.node_types.get(&key).map(|t| apply(t, chain)) else {
        return;
    };
    let Ok(Some(p)) =
        vyrn_frontend::project::site(w.impls, Some(&recv), "at", &args[0], &args[1..], e.line())
    else {
        return;
    };
    for s in &p.prologue {
        stmt(s, depth, chain, w);
    }
    expr(&p.place, depth, chain, w);
}

/// The loop a `for x in c` over a user container expands to, if it does.
fn iterate<'a>(
    var: &str,
    iter: &'a Expr,
    body: &'a Block,
    chain: &mut Chain,
    w: &mut Walk<'a, '_>,
) -> Option<&'static Block> {
    if w.impls.is_empty() {
        return None;
    }
    let key = iter as *const Expr as usize;
    let ty = apply(w.recorded.node_types.get(&key)?, chain);
    let (size_fn, nth) = vyrn_frontend::types::iterate_impl(w.impls, &ty)?;
    vyrn_frontend::project::iterate_loop(&size_fn, nth, var, iter, body, iter.line()).ok()
}

fn stmt_line(s: &Stmt) -> usize {
    match s {
        Stmt::Let { line, .. }
        | Stmt::Assign { line, .. }
        | Stmt::SetField { line, .. }
        | Stmt::IndexSet { line, .. }
        | Stmt::Return { line, .. }
        | Stmt::Break { line }
        | Stmt::Continue { line }
        | Stmt::If { line, .. }
        | Stmt::IfLet { line, .. }
        | Stmt::While { line, .. }
        | Stmt::ForIn { line, .. }
        | Stmt::Drop { line, .. }
        | Stmt::Region { line, .. } => *line,
        Stmt::Expr(e) => e.line(),
    }
}

// ---- the lint ------------------------------------------------------------

/// An independent pass over [`Lowered`], asserting the invariants the form is
/// supposed to have (RFC-0101 §2.6).
///
/// This is GHC's `-dcore-lint` in the small: it runs on the corpus gate and, via
/// the `debug_assert` in [`lower`], on every debug build forever. What it checks
/// is structure — that the answers are there, that they are concrete, that the
/// order is the order a dump can be diffed in. What it does NOT do is re-derive
/// each type from its children: that check needs a second type derivation, which
/// is the thing this RFC exists to have one of.
pub fn lint(l: &Lowered) -> Vec<String> {
    let mut bad = Vec::new();
    let mut prev: Option<(String, String, String)> = None;
    for i in &l.instances {
        let key = (i.module().to_string(), i.func.name.clone(), i.spelling());
        if let Some(p) = &prev {
            if *p > key {
                bad.push(format!(
                    "instances are out of order: `{}` follows `{}`",
                    key.2, p.2
                ));
            }
        }
        prev = Some(key);

        if i.type_args.len() != i.func.type_params.len() {
            bad.push(format!(
                "{}: {} type arguments for {} type parameters",
                i.spelling(),
                i.type_args.len(),
                i.func.type_params.len()
            ));
        }
        for a in &i.type_args {
            if mentions_param(a) {
                bad.push(format!(
                    "{}: type argument `{a}` still names a type parameter — an \
                     instance is concrete or it is not an instance",
                    i.spelling()
                ));
            }
        }
        let mut depth = None;
        for r in &i.rows {
            // Both members of the pair, because a has-type that still names a
            // parameter is the same defect as a destination that does — and the
            // has-type is the half a backend will read.
            for t in [&r.ty, &r.has].into_iter().flatten() {
                if matches!(t, Type::Err) {
                    bad.push(format!(
                        "{} @{}: the {} is typed `<type error>`, and a \
                         program that reaches lowering has none",
                        i.spelling(),
                        r.line,
                        r.node.kind()
                    ));
                }
                if mentions_param(t) {
                    bad.push(format!(
                        "{} @{}: the {} is typed `{t}`, which still names a type \
                         parameter after substitution",
                        i.spelling(),
                        r.line,
                        r.node.kind()
                    ));
                }
            }
            // A `let` is the one statement that names a type — the binding's.
            // Anything else carrying one is a walk that put a row's answer on
            // the wrong row.
            if matches!(r.node, Node::Stmt(s) if !matches!(s, Stmt::Let { .. })) && r.ty.is_some() {
                bad.push(format!(
                    "{} @{}: the {} carries a type, and only a `let` names one",
                    i.spelling(),
                    r.line,
                    r.node.kind()
                ));
            }
            // Indentation is structure, so a row may open one level at a time.
            if let Some(d) = depth {
                if r.depth > d + 1 {
                    bad.push(format!(
                        "{} @{}: the walk jumped from depth {d} to {}",
                        i.spelling(),
                        r.line,
                        r.depth
                    ));
                }
            }
            depth = Some(r.depth);
        }
    }
    bad
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(src: &str) -> Program {
        let mut p = vyrn_frontend::check(src).expect("the fixture checks");
        let diags = vyrn_frontend::check_and_synthesize(&mut p);
        assert!(diags.is_empty(), "{diags:?}");
        p
    }

    /// One instance per instantiation, the type arguments as the identity, and
    /// the checker's answer on every node — §2.1 items 1 and 2 in one fixture.
    #[test]
    fn a_generic_is_lowered_once_per_instantiation_with_concrete_types() {
        let src = "fn id<T>(x: T) -> T {\n    return x\n}\n\nfn main() -> Int64 {\n    \
                   let a: Int64 = id(1)\n    let b: String = id(\"s\")\n    \
                   print(b)\n    return a\n}\n";
        let p = program(src);
        let l = lower(&p);
        let spelled: Vec<String> = l.instances.iter().map(|i| i.spelling()).collect();
        assert_eq!(spelled, vec!["id<Int64>", "id<String>", "main"]);
        assert!(lint(&l).is_empty(), "{:?}", lint(&l));

        // The body is `return x`, and `x` is the parameter — concrete in each
        // instance, which is the whole of what monomorphizing before the split
        // buys a backend.
        for (inst, want) in l.instances.iter().zip([Type::Int, Type::Str]) {
            let tys: Vec<&Type> = inst.rows.iter().filter_map(|r| r.ty.as_ref()).collect();
            assert_eq!(tys, vec![&want], "{}", inst.spelling());
        }
    }

    /// [A16]: a node carries what the value HAS and what it must END UP as, and
    /// the two are one answer everywhere the node's own form settles it. `1`
    /// under an `Int32` destination is the smallest program where they are two.
    #[test]
    fn a_literal_under_a_sized_destination_carries_both_types() {
        let p =
            program("fn main() -> Int64 {\n    let a: Int32 = 1\n    let b = 2\n    return 0\n}\n");
        let l = lower(&p);
        let pairs: Vec<(String, String)> = l.instances[0]
            .rows
            .iter()
            .filter(|r| matches!(r.node, Node::Expr(Expr::Int(_))))
            .map(|r| {
                let (has, ty) = r.pair();
                (
                    has.map(|t| t.to_string()).unwrap_or_default(),
                    ty.map(|t| t.to_string()).unwrap_or_default(),
                )
            })
            .collect();
        // `1` has an Int64 and must end up an Int32; `2` and `0` have and end up
        // the same type, and carry no second answer.
        assert_eq!(
            pairs,
            vec![
                ("Int64".to_string(), "Int32".to_string()),
                ("Int64".to_string(), "Int64".to_string()),
                ("Int64".to_string(), "Int64".to_string()),
            ]
        );
        assert_eq!(
            l.instances[0]
                .rows
                .iter()
                .filter(|r| r.has.is_some())
                .count(),
            1,
            "only the constrained literal carries a second type"
        );
    }

    /// The lint is the gate, so it has to be able to fail. A type argument that
    /// still names a parameter is not an instance, and this is the assertion
    /// that says so.
    #[test]
    fn the_lint_refuses_an_instance_that_is_not_concrete() {
        let p = program("fn main() -> Int64 {\n    return 0\n}\n");
        let mut l = lower(&p);
        l.instances[0].type_args.push(Type::Param("T".into()));
        assert!(!lint(&l).is_empty());
    }
}
