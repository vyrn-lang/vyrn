//! The lowered form: the checker's answers as a value that one
//! lowering builds and every backend reads without deciding again. It holds one
//! [`Instance`] per instantiation, the checker's types for its expressions
//! ([`NodeTypes`]), and the release steps at every exit of a body. The types
//! are the checker's answers, substituted through the instantiation;
//! [`NodeTypes::produced`] is the one thing the form derives ([`has_of`]),
//! because the checker types every expression against its destination.

pub mod append;
pub mod check;
pub mod core;
pub mod effects;
pub mod elide;
pub mod facts;
mod fixpoint;
pub mod kernel;
mod pipeline;
pub mod rules;
pub mod typed;
mod world;

pub use pipeline::{check_and_synthesize, gen_engine, load, load_warned, refusals, JUDGE};

pub use core::refuses as kernel_refuses;
pub use world::{analyze, FnRow, Fns, World};

use std::collections::{BTreeMap, HashMap, VecDeque};

use vyrn_frontend::ast::{
    Expr, FnId, Function, LambdaBody, NodeId, Program, SourceBody, Stmt, Type,
};
use vyrn_frontend::checker;
use vyrn_frontend::own::DropKind;
use vyrn_frontend::types::{
    expanded_size, mentions_param, substitute, type_depth, MONO_DEPTH_LIMIT, MONO_SIZE_LIMIT,
};

/// The version line `vyrn emit-lowered` prints above the named core. It
/// promises no stability.
pub const VERSION: &str = "v2";

/// What the checker decided about the expressions of one body, under the
/// substitution the body is lowered for. The core builder reads these and
/// derives no type of its own.
#[derive(Debug, Clone, Default)]
pub struct NodeTypes<'a> {
    /// The type each expression must END UP as: the destination the checker
    /// validated it against, or [`NodeTypes::produced`] where it recorded none.
    pub types: HashMap<NodeId, Type>,
    /// The type each expression HAS when its code has run, before the coercion
    /// to its destination.
    pub produced: HashMap<NodeId, Type>,
    /// At a generic call or a `?` on a generic `Fallible`, the type arguments
    /// the checker solved, by parameter name in the callee's order.
    pub solved: HashMap<NodeId, Vec<(String, Type)>>,
    /// Every expression the body holds, expansions included, in reading
    /// order, each with its line. A literal has no line of its own and takes
    /// the line of the statement or binding around it.
    pub exprs: Vec<(&'a Expr, u32)>,
}

/// What an expression is, in one word: the axis a disagreement or a gap is
/// classified on.
pub fn kind(e: &Expr) -> &'static str {
    match e {
        Expr::Int(_, _) => "int",
        Expr::Byte(_, _) => "byte",
        Expr::Float(_, _) => "float",
        Expr::Bool(_, _) => "bool",
        Expr::Str(_, _) => "str",
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
        Expr::Lambda { .. } => "lambda",
        Expr::Consume { .. } => "consume",
    }
}

/// Which exit a release step belongs to. It lives in `own` because the
/// interpreter reports against it and cannot import this crate.
pub use vyrn_frontend::own::Exit;

/// One reclamation the language places at an exit. The placement lives in
/// `own` for the same reason as [`Exit`]; an [`Instance`] carries it
/// substituted ([`Instance::releases`]).
pub use vyrn_frontend::own::Release;

/// One function, instantiated: no type parameter survives, and the identity
/// is the type arguments, never a mangled string (#165).
#[derive(Debug, Clone)]
pub struct Instance<'a> {
    pub func: &'a Function,
    /// `func`'s id, which the plan's rows are keyed by: every instance of a
    /// generic shares it.
    pub func_id: FnId,
    /// The type arguments, in the function's own type-parameter order.
    pub type_args: Vec<Type>,
    pub subst: BTreeMap<String, Type>,
    pub facts: NodeTypes<'a>,
    /// [`vyrn_frontend::own::Ownership::releases`] for this body, in `own`'s
    /// order, with this instance's substitution applied to each step's type.
    ///
    /// A step can still name a type parameter: `own` records a declared type's
    /// base record shape with the declaration's parameters, so
    /// `examples/fnvalarg.vyrn` carries `Deep({ run: fn(P) -> T })` in a
    /// non-generic function. [`lint`] does not check steps for that reason.
    pub releases: Vec<Release>,
}

/// The name an instance of `name` at `type_args` is built and emitted under:
/// `name` alone, or `map<Int64, String>`. It is the key of [`World::body_of`].
pub fn spell(name: &str, type_args: &[Type]) -> String {
    if type_args.is_empty() {
        return name.to_string();
    }
    let args: Vec<String> = type_args.iter().map(|t| t.to_string()).collect();
    format!("{name}<{}>", args.join(", "))
}

impl Instance<'_> {
    pub fn spelling(&self) -> String {
        spell(&self.func.name, &self.type_args)
    }

    /// Whether the kernel judges this body and no emitter reads it: a `gen fn`
    /// in the program that holds it. The generator's own compile clears
    /// `is_gen` and emits the body there.
    pub fn judged_only(&self) -> bool {
        self.func.is_gen
    }

    /// The module this instance's function was declared in; `""` for the root.
    pub fn module(&self) -> &str {
        self.func.module.as_deref().unwrap_or("")
    }
}

/// Why a generic call did not become an instantiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// The name is not a function of this linked program.
    NotAFunction,
    /// The checker left a type parameter unsolved at the call.
    UnsolvedParameter,
    /// [`MONO_DEPTH_LIMIT`] or [`MONO_SIZE_LIMIT`] refused the instantiation.
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
    /// The line the callee is declared on, which a refusal names: the author
    /// changes the generic, not the call.
    pub line: usize,
    /// The type arguments the call solved; a refusal is worded from these.
    /// Empty when the checker left a parameter open.
    pub args: Vec<Type>,
    pub why: Why,
}

/// The checked program with the answers written on it and the sugar gone.
#[derive(Debug, Clone)]
pub struct Lowered<'a> {
    /// Sorted by module, then name, then rendered type arguments, never in
    /// `HashMap` order.
    pub instances: Vec<Instance<'a>>,
    /// The module-state initializers, in declaration order. Both
    /// backends emit them into a synthesized function.
    pub globals: NodeTypes<'a>,
    /// Where the worklist stopped, and why.
    pub unresolved: Vec<Unresolved>,
    /// Every refined type's `where` predicate, typed once. A predicate lives
    /// on a declaration, which has no type parameters, so a reader keyed by
    /// `(node, instantiation)` looks one up under the empty substitution.
    /// Its calls are not followed into the worklist: that would add instances
    /// the backends' own worklists do not have.
    pub predicates: NodeTypes<'a>,
    /// Every `Block` that is a lambda's body, by node, so an engine
    /// can tell those blocks apart without a second AST walk.
    pub lambda_bodies: std::collections::HashSet<NodeId>,
    /// The `test` and `bench` bodies, in declaration order. Not followed into
    /// the worklist, like [`Lowered::predicates`]: a generic only a test calls
    /// is an instantiation no backend emits.
    pub bodies: Vec<OutsideBody<'a>>,
    /// Every `impl` projection's body. No instance covers one, yet
    /// the core lowers an access site as a call by the projection's name. Not
    /// followed into the worklist: a projection is inlined at its site.
    pub places: Vec<PlaceBody<'a>>,
    /// The checker's record every [`NodeTypes`] here was read off.
    pub recorded: std::sync::Arc<checker::Recorded>,
    /// [`Program::source_names`]: the World's function table starts with
    /// these rows.
    pub source: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PlaceBody<'a> {
    pub func: &'a Function,
    pub id: FnId,
    pub facts: NodeTypes<'a>,
}

/// One body that is no function of the program: a `test` or a `bench`.
#[derive(Debug, Clone)]
pub struct OutsideBody<'a> {
    /// `test@<i>` or `bench@<i>`: the name the checker keys it by.
    pub name: String,
    pub id: FnId,
    pub block: &'a vyrn_frontend::ast::Block,
    pub module: Option<String>,
    pub line: usize,
    pub facts: NodeTypes<'a>,
}

impl<'a> Lowered<'a> {
    /// The instances declared in the root module, which `vyrn emit-lowered`
    /// prints by default.
    pub fn root(&self) -> impl Iterator<Item = &Instance<'a>> {
        self.instances.iter().filter(|i| i.func.module.is_none())
    }

    pub fn exprs(&self) -> usize {
        self.instances
            .iter()
            .map(|i| i.facts.exprs.len())
            .sum::<usize>()
            + self.globals.exprs.len()
            + self.predicates.exprs.len()
    }
}

/// The named core of `program`'s root-module instances, as `vyrn emit-lowered`
/// prints it: the version line, then each body's [`vyrn_frontend::core::Body::render`], or the
/// gap that stopped it. Root-module only, `vyrn why --memory`'s rule: a linked
/// program's imports are another file's answer. `world` is `program`'s.
pub fn render(program: &Program, world: &World, source: &str) -> String {
    let own = &world.ownership;
    let lowered = lower_with(program, own);
    let mut out = format!("; vyrn lowered {VERSION} -- {source}\n");
    for inst in lowered.root() {
        out.push('\n');
        match core::build(program, inst, own) {
            Ok(body) => out.push_str(&core::checked(program, own, &body).render()),
            Err(g) => out.push_str(&format!(
                "; {}: not lowered at line {}: {} {}\n",
                inst.spelling(),
                g.line,
                g.what,
                g.detail
            )),
        }
    }
    out
}

/// Lowers a checked program.
///
/// `program` must already be through `check_and_synthesize`, so the
/// synthesized JSON codecs are lowered like any other function.
pub fn lower(program: &Program) -> Lowered<'_> {
    let _p = vyrn_frontend::prof::phase("lower");
    let own_span = vyrn_frontend::prof::phase("lower: own::analyze");
    let world = analyze(program);
    drop(own_span);
    lower_with(program, &world.ownership)
}

/// The lowered form against an ownership analysis already made, for the
/// placer, which runs inside [`analyze`].
pub fn lower_with<'a>(
    program: &'a Program,
    ownership: &vyrn_frontend::own::Ownership,
) -> Lowered<'a> {
    lowered(program, ownership, false)
}

/// [`lower_with`] for the placer of a host that runs no emitter. An instance
/// whose walk [`Walks`] holds takes the calls the walk found, and adds no
/// facts and no lambda body; [`walked`] gives the facts to a reader that
/// builds it.
pub(crate) fn lower_reusing<'a>(
    program: &'a Program,
    ownership: &vyrn_frontend::own::Ownership,
) -> Lowered<'a> {
    lowered(program, ownership, true)
}

fn lowered<'a>(
    program: &'a Program,
    ownership: &vyrn_frontend::own::Ownership,
    reuse: bool,
) -> Lowered<'a> {
    let recorded = ownership.record.clone();
    let build_span = vyrn_frontend::prof::phase("lower: build");
    let mut lowered = build(program, &recorded, ownership, reuse);
    drop(build_span);
    lowered.instances.sort_by(|a, b| {
        (a.module(), &a.func.name, a.spelling()).cmp(&(b.module(), &b.func.name, b.spelling()))
    });
    lowered
}

/// The substitutions in scope at a node, outermost first.
///
/// A generic call's arguments are checked against the callee's parameter types,
/// so the checker's answer on `[]` in `push(xs, [])` is `Array<T>` with the
/// callee's `T`. The solution recorded on the call node applies to its subtree.
/// Applying the stack in order, not merged, keeps a caller's `T` and a callee's
/// `T` apart: the outer substitution makes the caller's concrete first.
type Chain = Vec<HashMap<String, Type>>;

fn apply(ty: &Type, chain: &Chain) -> Type {
    chain.iter().fold(ty.clone(), |t, s| substitute(&t, s))
}

/// The walk that gathers one body's [`NodeTypes`], and the generic calls it makes.
///
/// It reads the checker's record through the substitutions in scope, and it
/// walks the expansions every engine walks: a `place at` projection inlined at
/// its access site, an optional projection under `if let`, a `place atSet`
/// under `a[i] = v`, a `for` over a user container and `schemaOf`'s literal.
struct Walk<'a, 'r> {
    recorded: &'r checker::Recorded,
    impls: &'a [vyrn_frontend::ast::ImplBlock],
    expansions: &'a vyrn_frontend::project::Expansions,
    facts: NodeTypes<'a>,
    /// `(callee, its solved type arguments by name)`, already concrete.
    calls: Vec<(String, HashMap<String, Type>)>,
    lambda_bodies: std::collections::HashSet<NodeId>,
    chain: Chain,
    /// Per open expression: its recorded type, and whether it pushed a
    /// substitution onto `chain`.
    open: Vec<(Option<Type>, bool)>,
    /// The line of each open statement or module-state binding, innermost last.
    lines: Vec<u32>,
    /// The scrutinee of each open `if let`, innermost last. An optional
    /// projection there expands after the scrutinee and before the blocks.
    scrutinees: Vec<NodeId>,
}

impl<'a, 'r> Walk<'a, 'r> {
    fn new(
        recorded: &'r checker::Recorded,
        program: &'a Program,
        subst: HashMap<String, Type>,
    ) -> Self {
        Walk {
            recorded,
            impls: &program.impls,
            expansions: &program.expansions,
            facts: NodeTypes::default(),
            calls: Vec::new(),
            lambda_bodies: Default::default(),
            chain: vec![subst],
            open: Vec::new(),
            lines: Vec::new(),
            scrutinees: Vec::new(),
        }
    }

    fn recorded(&self, e: &Expr) -> Option<Type> {
        let key = e.id();
        self.recorded
            .node_types
            .get(&key)
            .map(|t| apply(t, &self.chain))
    }

    /// Closes the innermost open expression: its has-type, from its
    /// children's, and the substitution it pushed.
    fn close(&mut self, e: &Expr) {
        let Some((ty, pushed)) = self.open.pop() else {
            unreachable!("every closed expression was opened");
        };
        let key = e.id();
        let has = has_of(e, |k| self.facts.produced.get(&k.id()).cloned());
        if let Some(t) = ty.clone().or_else(|| has.clone()) {
            self.facts.types.insert(key, t);
        }
        if let Some(t) = has.or(ty) {
            self.facts.produced.insert(key, t);
        }
        if pushed {
            self.chain.pop();
        }
    }

    /// The expansion of a `place` projection at the call `e`, if the receiver
    /// has a user one. `project::site` expands once, so every engine walks
    /// the same nodes.
    fn site(&mut self, e: &Expr, method: &str, args: &[Expr]) {
        if args.is_empty() || self.impls.is_empty() {
            return;
        }
        let Some(recv) = self.recorded(&args[0]) else {
            return;
        };
        let Ok(Some(p)) = self.expansions.site(
            self.impls,
            Some(&recv),
            method,
            &args[0],
            &args[1..],
            e.line(),
        ) else {
            return;
        };
        self.projection(p);
    }

    fn projection(&mut self, p: &'static vyrn_frontend::project::Projection) {
        for s in &p.prologue {
            facts_stmt(s, &mut Default::default(), self);
        }
        facts_expr(&p.place, &Default::default(), self);
    }

    /// The expansion of an OPTIONAL projection as an `if let` scrutinee.
    fn optional_site(&mut self, scrutinee: &Expr, line: usize) {
        let Expr::Call { name, args, .. } = scrutinee else {
            return;
        };
        if args.is_empty() || self.impls.is_empty() {
            return;
        }
        let Some(recv) = self.recorded(&args[0]) else {
            return;
        };
        let Ok(Some(p)) = self.expansions.optional_site(
            self.impls,
            Some(&recv),
            name,
            &args[0],
            &args[1..],
            line,
        ) else {
            return;
        };
        for ps in &p.prologue {
            facts_stmt(ps, &mut Default::default(), self);
        }
        facts_expr(&p.miss, &Default::default(), self);
        for hs in &p.hit {
            facts_stmt(hs, &mut Default::default(), self);
        }
        facts_expr(&p.place, &Default::default(), self);
    }
}

vyrn_frontend::body_scope_descent!(FactsVisit, facts_block, facts_stmt, facts_expr);

impl<'a> FactsVisit<'a> for Walk<'a, '_> {
    const SCOPED: bool = false;

    fn stmt(&mut self, s: &'a Stmt, _: &std::collections::HashSet<String>) {
        self.lines.push(s.line() as u32);
        if let Some((_, scrutinee, ..)) = match s {
            Stmt::Expr(e, _) => e.as_if_let(),
            _ => None,
        } {
            self.scrutinees.push(scrutinee.id());
        }
    }

    fn after_stmt(&mut self, s: &'a Stmt, _: &std::collections::HashSet<String>) {
        match s {
            // The writing half of a projection: `a[i] = v` on a receiver with
            // a user `place atSet`, as the checker built and shared it.
            Stmt::IndexSet { index, .. } => {
                if let Some(blk) = self.expansions.stored(index) {
                    facts_block(blk, &mut Default::default(), self);
                }
            }
            // A `for` over a user container calls its `size` and reads each
            // element through its `place nth`.
            Stmt::ForIn { iter, line, .. } if !self.impls.is_empty() => {
                if let Some(ty) = self.recorded(iter) {
                    let impls = self.impls;
                    if let Some((imp, size, _)) = vyrn_frontend::types::iterate_impl(impls, &ty) {
                        let mut solved = HashMap::new();
                        vyrn_frontend::types::solve_param(&imp.ty, &ty, &mut solved);
                        self.calls.push((size, solved));
                    }
                    if let Ok(Some(p)) = self.expansions.for_element(impls, &ty, iter, *line) {
                        self.projection(p);
                    }
                }
            }
            _ => {}
        }
        self.lines.pop();
    }

    fn expr(&mut self, e: &'a Expr, locals: &std::collections::HashSet<String>) -> bool {
        let key = e.id();
        let line = match e.line() {
            0 => self.lines.last().copied().unwrap_or(0),
            l => l as u32,
        };
        self.facts.exprs.push((e, line));
        let ty = self.recorded(e);
        // A generic call solves its callee's parameters, and the answer
        // governs the subtree it was solved from (see [`Chain`]).
        let pushed = match self.recorded.node_substs.get(&key) {
            Some((callee, args)) => {
                let at: Vec<(String, Type)> = args
                    .iter()
                    .map(|(p, t)| (p.clone(), apply(t, &self.chain)))
                    .collect();
                let solved: HashMap<String, Type> = at.iter().cloned().collect();
                // A record literal solves parameters too, and it is not a call:
                // only a call, or a `?` on a `Fallible` operand, which the
                // checker types as a call of `Fallible__Key__success`, adds an
                // instance to the worklist.
                if matches!(e, Expr::Call { .. } | Expr::Try { .. }) {
                    self.calls.push((callee.clone(), solved.clone()));
                    if !at.is_empty() {
                        self.facts.solved.insert(key, at);
                    }
                }
                self.chain.push(solved);
                true
            }
            None => false,
        };
        self.open.push((ty, pushed));
        match e {
            Expr::Lambda {
                body: LambdaBody::Block(b),
                ..
            } => {
                self.lambda_bodies.insert(b.id());
            }
            // `schemaOf<T>()` lowers through the literal the checker expanded
            // for it, and has no arguments of its own to walk.
            Expr::Call { name, .. } if name == "schemaOf" => {
                if let Some(lit) = self.expansions.schema_at(e) {
                    facts_expr(lit, locals, self);
                }
                self.close(e);
                return false;
            }
            _ => {}
        }
        true
    }

    fn after_expr(&mut self, e: &'a Expr, _: &std::collections::HashSet<String>) {
        if let Expr::Call { name, args, .. } = e {
            // A named projection is the same site as `x[i]` under
            // its own method name.
            if name == vyrn_frontend::project::AT {
                self.site(e, "at", args);
            } else if self
                .impls
                .iter()
                .any(|i| i.places.iter().any(|p| p.name == *name))
            {
                self.site(e, name, args);
            }
        }
        self.close(e);
        if self.scrutinees.last() == Some(&e.id()) {
            self.scrutinees.pop();
            let line = self.lines.last().copied().unwrap_or(0) as usize;
            self.optional_site(e, line);
        }
    }
}

/// The default an unconstrained position gets. Both compiled backends write
/// `Int64` there: the element of an empty container, the unused side of a
/// `Result`, a `None` whose payload nothing names.
const UNCONSTRAINED: Type = Type::Int;

/// The type this node's own code produces, ignoring the destination
/// from what `kid` says each child
/// produces.
///
/// The checker cannot answer this: it types an expression against the type the
/// context wants. So it is derived here, from the node's own shape, in one
/// closed table: every arm is a node whose form settles its type without asking
/// the context, and everything else answers `None`.
fn has_of<'e>(e: &'e Expr, kid: impl Fn(&'e Expr) -> Option<Type>) -> Option<Type> {
    Some(match e {
        // A numeric literal is its own width, and the destination is a coercion
        // away. `Byte` too: both backends spell `'a'` as an `i64` immediate and
        // narrow at the use, which the checker does not; it answers `UInt8`.
        Expr::Int(_, _) | Expr::Byte(_, _) => Type::Int,
        Expr::Float(_, _) => Type::Float,
        // A pass-through: the node emits its child's value.
        Expr::Consume { place: inner, .. } | Expr::Unary { expr: inner, .. } => kid(inner)?,
        // A join carries the type of a branch, not of its destination. `panic`
        // in the then-branch makes it `Never`, and the else answers.
        Expr::IfExpr {
            then_branch,
            else_branch,
            ..
        } => match kid(then_branch)? {
            Type::Never => kid(else_branch.as_deref()?)?,
            t => t,
        },
        // A `match` is typed by its first arm that yields a value, and one whose
        // every arm leaves the function has the bottom type. A block arm yields
        // nothing, so the arms are counted against the expression arms alone.
        Expr::Match { arms, .. } => {
            let bodies: Vec<&Expr> = arms.iter().filter_map(|a| a.body.as_expr()).collect();
            let mut t = Type::Never;
            for i in 0..arms.len() {
                match bodies.get(i).and_then(|b| kid(b)) {
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
        // element to be typed by. A written array is a FIXED-size one until
        // something stores it somewhere growable.
        Expr::ArrayLit { elems, .. } => match elems.first() {
            None => Type::Array(Box::new(UNCONSTRAINED)),
            Some(first) => Type::ArrayN(Box::new(kid(first)?), elems.len()),
        },
        Expr::MapLit { entries, .. } => Type::Map(
            Box::new(Type::Str),
            Box::new(match entries.first() {
                None => UNCONSTRAINED,
                Some((_, v)) => kid(v)?,
            }),
        ),
        // A sum constructor names one side, and the other is unconstrained.
        Expr::Var { name, .. } if name == "None" => Type::option(UNCONSTRAINED),
        Expr::Call { name, args, .. } if args.len() == 1 => match name.as_str() {
            "Some" => Type::option(kid(&args[0])?),
            "Ok" => Type::result(kid(&args[0])?, UNCONSTRAINED),
            "Err" => Type::result(UNCONSTRAINED, kid(&args[0])?),
            _ => return None,
        },
        _ => return None,
    })
}

/// One walk of a body under a substitution: its facts, the generic calls it
/// makes and its lambda bodies.
struct Walked<'a> {
    facts: NodeTypes<'a>,
    calls: Vec<(String, HashMap<String, Type>)>,
    lambda_bodies: std::collections::HashSet<NodeId>,
}

fn walk<'a>(
    recorded: &checker::Recorded,
    program: &'a Program,
    body: &'a vyrn_frontend::ast::Block,
    subst: HashMap<String, Type>,
) -> Walked<'a> {
    let mut w = Walk::new(recorded, program, subst);
    facts_block(body, &mut Default::default(), &mut w);
    Walked {
        facts: w.facts,
        calls: w.calls,
        lambda_bodies: w.lambda_bodies,
    }
}

/// `inst` with its facts: `inst` itself, or its body walked again where
/// [`lower_reusing`] left the facts out. A body with no expression walks to
/// the same empty facts.
pub(crate) fn walked<'i, 'a>(
    program: &'a Program,
    recorded: &checker::Recorded,
    inst: &'i Instance<'a>,
) -> std::borrow::Cow<'i, Instance<'a>> {
    if !inst.facts.exprs.is_empty() {
        return std::borrow::Cow::Borrowed(inst);
    }
    let subst = inst.subst.clone().into_iter().collect();
    let facts = walk(recorded, program, &inst.func.body, subst).facts;
    std::borrow::Cow::Owned(Instance {
        facts,
        ..inst.clone()
    })
}

/// How many lowerings a kept walk outlives unused, so an edit undone at the
/// next keystroke finds its walk.
const WALK_STALE: u64 = 2;

thread_local! {
    static WALKS: std::cell::RefCell<(u64, HashMap<(u64, Vec<Type>), Kept>)> =
        Default::default();
}

/// The calls one walk found, and the lowering that last read or wrote them.
type Kept = (Vec<(String, HashMap<String, Type>)>, u64);

/// The walks a lowering reuses, keyed by the serial of the recheck entry that
/// holds the body's record ([`checker::Recorded::entries`]) and the type
/// arguments. Besides the record and the substitution, a walk reads `impls`
/// and the expansions. An entry answers only under the recheck world, which
/// holds every `impl`. A body whose typing expanded a site names a node of
/// another unit, so no entry holds it.
struct Walks {
    now: u64,
    kept: HashMap<(u64, Vec<Type>), Kept>,
    /// Each function body's serial in this check.
    serials: HashMap<FnId, u64>,
}

impl Walks {
    fn open(recorded: &checker::Recorded) -> Walks {
        let serials = (recorded.entries.iter())
            .filter_map(|(body, serial)| match body {
                SourceBody::Fn(i) => Some((FnId::nth(*i as usize), *serial)),
                _ => None,
            })
            .collect();
        let (now, kept) = WALKS.with(|w| std::mem::take(&mut *w.borrow_mut()));
        Walks {
            now: now + 1,
            kept,
            serials,
        }
    }

    /// The calls the kept walk of `f`'s body at `type_args` found.
    fn reuse(&mut self, f: FnId, type_args: &[Type]) -> Option<Walked<'static>> {
        let serial = *self.serials.get(&f)?;
        let (calls, used) = self.kept.get_mut(&(serial, type_args.to_vec()))?;
        *used = self.now;
        Some(Walked {
            facts: NodeTypes::default(),
            calls: calls.clone(),
            lambda_bodies: Default::default(),
        })
    }

    fn keep(&mut self, f: FnId, type_args: &[Type], w: &Walked) {
        if let Some(&serial) = self.serials.get(&f) {
            let kept = (w.calls.clone(), self.now);
            self.kept.insert((serial, type_args.to_vec()), kept);
        }
    }

    /// Drops every walk unused for [`WALK_STALE`] lowerings and hands the rest
    /// to the next lowering on this thread.
    fn close(mut self) {
        let now = self.now;
        self.kept.retain(|_, (_, used)| *used + WALK_STALE >= now);
        WALKS.with(|w| *w.borrow_mut() = (now, self.kept));
    }
}

fn build<'a>(
    program: &'a Program,
    recorded: &std::sync::Arc<checker::Recorded>,
    ownership: &vyrn_frontend::own::Ownership,
    reuse: bool,
) -> Lowered<'a> {
    // Typing expanded every site a walk reads, so the lowering makes no
    // expansion tree, and its walks run on many threads.
    let _sealed = program.expansions.seal();
    let mut walks = reuse.then(|| Walks::open(recorded));
    let no_steps: Vec<Release> = Vec::new();
    let by_name = by_name(program);
    let decls = ownership.proto.types();

    // The roots are every non-generic function with a body. A `std/mem`
    // primitive, like an `extern`, has none: it lowers to one instruction at
    // each call.
    let mut queue: VecDeque<(FnId, &Function, Vec<Type>)> = (program.functions.iter())
        .enumerate()
        .filter(|(_, f)| {
            f.type_params.is_empty()
                && !f.is_extern
                && !f.name.starts_with(vyrn_frontend::loader::MEM_PREFIX)
        })
        .map(|(i, f)| (FnId::nth(i), f, Vec::new()))
        .collect();
    let mut seen: Vec<(String, String)> = queue
        .iter()
        .map(|(_, f, _)| (f.name.clone(), String::new()))
        .collect();

    let mut instances: Vec<Instance<'a>> = Vec::new();
    let mut unresolved: Vec<Unresolved> = Vec::new();
    let mut lambda_bodies: std::collections::HashSet<NodeId> = Default::default();

    // Module state is the second root: an initializer instantiates generics
    // like any body. It is an expression, so it has no exit to place a release
    // at.
    let mut gw = Walk::new(recorded, program, HashMap::new());
    for g in &program.globals {
        gw.lines.push(g.line as u32);
        facts_expr(&g.init, &Default::default(), &mut gw);
        gw.lines.pop();
    }
    let globals = std::mem::take(&mut gw.facts);
    // Predicates, test and bench bodies, and projections are walked but their
    // calls are not followed ([`Lowered::predicates`]).
    let mut pw = Walk::new(recorded, program, HashMap::new());
    for d in &program.type_decls {
        if let Some(p) = &d.predicate {
            facts_expr(p, &Default::default(), &mut pw);
        }
    }
    let predicates = pw.facts;
    let id = |body| program.source_id(body);
    let place_fns: Vec<&Function> = (vyrn_frontend::project::all(program))
        .map(|(_, f)| f)
        .collect();
    let blocks: Vec<&vyrn_frontend::ast::Block> = (place_fns.iter().map(|f| &f.body))
        .chain(program.tests.iter().map(|t| &t.body))
        .chain(program.benches.iter().map(|b| &b.body))
        .collect();
    let mut facts = vyrn_frontend::par::in_parallel(
        &blocks,
        |b| b.stmts.len(),
        || (),
        |_, b| walk(recorded, program, b, HashMap::new()).facts,
    )
    .into_iter();
    let places: Vec<PlaceBody<'a>> = ((0..).zip(place_fns).zip(&mut facts))
        .map(|((i, f), facts)| PlaceBody {
            func: f,
            id: id(SourceBody::Place(i)),
            facts,
        })
        .collect();
    let mut outside: Vec<OutsideBody<'a>> = Vec::new();
    for ((i, t), facts) in (0..).zip(&program.tests).zip(&mut facts) {
        outside.push(OutsideBody {
            id: id(SourceBody::Test(i)),
            name: format!("test@{i}"),
            block: &t.body,
            module: t.module.clone(),
            line: t.line,
            facts,
        });
    }
    for ((i, b), facts) in (0..).zip(&program.benches).zip(&mut facts) {
        outside.push(OutsideBody {
            id: id(SourceBody::Bench(i)),
            name: format!("bench@{i}"),
            block: &b.body,
            module: b.module.clone(),
            line: b.line,
            facts,
        });
    }
    follow(
        "<module state>",
        std::mem::take(&mut gw.calls),
        &by_name,
        decls,
        &mut seen,
        &mut queue,
        &mut unresolved,
    );
    // An audited build's leak-check teardown drops every module-state binding
    // after `main`, so a generic declared release it reaches is
    // an instantiation. It is solved from the declared type, which the
    // teardown drops by; an unannotated global of such a type fails the gate
    // as a missing instantiation. Only an audited build emits the teardown.
    if vyrn_frontend::loader::audit_build(program.host.gen) {
        let mut teardown_calls: Vec<(String, HashMap<String, Type>)> = Vec::new();
        for g in &program.globals {
            let Some(gty) = &g.ty else { continue };
            let Some(vyrn_frontend::own::DropKind::Release(f, _)) =
                ownership.proto.release_kind(gty)
            else {
                continue;
            };
            let Some((_, target)) = by_name.get(f.as_str()) else {
                continue;
            };
            if target.type_params.is_empty() {
                continue;
            }
            let mut solved: HashMap<String, Type> = HashMap::new();
            if let Some(p) = target.params.first() {
                vyrn_frontend::types::solve_param(&p.ty, gty, &mut solved);
            }
            teardown_calls.push((target.name.clone(), solved));
        }
        follow(
            "<teardown>",
            teardown_calls,
            &by_name,
            decls,
            &mut seen,
            &mut queue,
            &mut unresolved,
        );
    }

    // The walks run ahead of the worklist in waves. When `ahead` is empty, the
    // body just taken and every body queued behind it are walked at once, on
    // every thread; then each body's calls are followed in queue order, as one
    // thread would. `ahead` holds a walk for each of the first bodies in the
    // queue, so a body queued after the wave waits for the next one.
    let mut ahead: VecDeque<Walked> = VecDeque::new();
    let subst_of = |func: &Function, type_args: &[Type]| -> BTreeMap<String, Type> {
        (func.type_params.iter().cloned())
            .zip(type_args.iter().cloned())
            .collect()
    };
    while let Some((func_id, func, type_args)) = queue.pop_front() {
        let subst = subst_of(func, &type_args);
        let flat: HashMap<String, Type> = subst.clone().into_iter().collect();

        if ahead.is_empty() {
            let wave: Vec<_> = std::iter::once((func_id, func, &type_args[..]))
                .chain(queue.iter().map(|(id, f, args)| (*id, *f, &args[..])))
                .map(|(id, f, args)| {
                    (
                        id,
                        f,
                        args,
                        walks.as_mut().and_then(|ws| ws.reuse(id, args)),
                    )
                })
                .collect();
            let fresh = vyrn_frontend::par::in_parallel(
                &wave,
                |(_, f, _, kept)| kept.as_ref().map_or(f.body.stmts.len(), |_| 0),
                || (),
                |_, (_, f, args, kept)| {
                    let flat = || subst_of(f, args).into_iter().collect();
                    kept.is_none()
                        .then(|| walk(recorded, program, &f.body, flat()))
                },
            );
            for ((id, f, args, kept), fresh) in wave.into_iter().zip(fresh) {
                ahead.push_back(match (kept, fresh) {
                    (Some(w), _) => w,
                    // `fresh` holds a walk for every body `kept` does not.
                    (None, fresh) => {
                        let flat = subst_of(f, args).into_iter().collect();
                        let w = fresh.unwrap_or_else(|| walk(recorded, program, &f.body, flat));
                        if let Some(ws) = &mut walks {
                            ws.keep(id, args, &w);
                        }
                        w
                    }
                });
            }
        }
        let w = (ahead.pop_front()).expect("a wave walks the body that starts it");
        // `own` decides against the declaration; an engine emits against the
        // instance, so a step's type is substituted here.
        let releases: Vec<Release> = ownership
            .releases
            .get(&func_id)
            .unwrap_or(&no_steps)
            .iter()
            .map(|r| match &r.kind {
                DropKind::Deep(t) => Release {
                    kind: DropKind::Deep(substitute(t, &flat)),
                    ..r.clone()
                },
                DropKind::Release(f, t) => Release {
                    kind: DropKind::Release(f.clone(), substitute(t, &flat)),
                    ..r.clone()
                },
                _ => r.clone(),
            })
            .collect();

        let mut calls = w.calls;
        calls.extend(
            dispatched(&releases, &by_name)
                .into_iter()
                .map(|(f, s)| (f.to_string(), s)),
        );
        follow(
            &func.name,
            calls,
            &by_name,
            decls,
            &mut seen,
            &mut queue,
            &mut unresolved,
        );

        lambda_bodies.extend(w.lambda_bodies);
        instances.push(Instance {
            func,
            func_id,
            type_args,
            subst,
            facts: w.facts,
            releases,
        });
    }

    if let Some(ws) = walks {
        ws.close();
    }
    Lowered {
        instances,
        globals,
        predicates,
        unresolved,
        lambda_bodies,
        bodies: outside,
        places,
        recorded: recorded.clone(),
        source: program.source_names(),
    }
}

/// Every program function by name, with its id.
pub(crate) fn by_name(program: &Program) -> HashMap<&str, (FnId, &Function)> {
    (program.functions.iter().enumerate())
        .map(|(i, f)| (f.name.as_str(), (FnId::nth(i), f)))
        .collect()
}

/// Every generic function as one instance whose type parameters stand for
/// themselves, as the checker typed it. The typed judgment reads these; no
/// emitter does.
pub fn as_written<'a>(
    program: &'a Program,
    ownership: &vyrn_frontend::own::Ownership,
) -> Vec<Instance<'a>> {
    let recorded = &ownership.record;
    (program.functions.iter().enumerate())
        .filter(|(_, f)| {
            !f.type_params.is_empty()
                && !f.is_extern
                && !f.name.starts_with(vyrn_frontend::loader::MEM_PREFIX)
        })
        .map(|(i, func)| {
            let func_id = FnId::nth(i);
            let type_args: Vec<Type> = func.type_params.iter().cloned().map(Type::Param).collect();
            let facts = walk(&recorded, program, &func.body, HashMap::new()).facts;
            Instance {
                func,
                func_id,
                subst: func
                    .type_params
                    .iter()
                    .cloned()
                    .zip(type_args.iter().cloned())
                    .collect(),
                type_args,
                facts,
                releases: ownership
                    .releases
                    .get(&func_id)
                    .cloned()
                    .unwrap_or_default(),
            }
        })
        .collect()
}

/// The generic calls the language writes at this body's exits: a placed
/// [`DropKind::Release`] step calls a declared release, and a generic one is
/// instantiated at the step's receiver, solved by
/// [`vyrn_frontend::types::solve_param`] like a written call. The lowering,
/// not the emitter, owns the worklist.
///
/// A generic release reached only inside a [`DropKind::Deep`] walk, as in
/// `Array<Slots<Int64>>`, is not seen here: that walk is the encoder's. The
/// corpus has none; one fails `tests/lowered.rs` as a missing instantiation.
pub(crate) fn dispatched<'f>(
    releases: &[Release],
    by_name: &HashMap<&str, (FnId, &'f Function)>,
) -> Vec<(&'f str, HashMap<String, Type>)> {
    let mut out = Vec::new();
    for r in releases {
        let DropKind::Release(f, recv) = &r.kind else {
            continue;
        };
        let Some((_, target)) = by_name.get(f.as_str()) else {
            continue;
        };
        if target.type_params.is_empty() {
            // A root of the worklist already.
            continue;
        }
        let mut solved: HashMap<String, Type> = HashMap::new();
        if let Some(p) = target.params.first() {
            vyrn_frontend::types::solve_param(&p.ty, recv, &mut solved);
        }
        out.push((target.name.as_str(), solved));
    }
    out
}

/// Adds the `isSuccess` twin of every `Fallible__Key__success` call: `?` on a
/// `Fallible` emits both, and the checker records only `success`. Both are the
/// same impl at the same instantiation.
fn fallible_twins(
    calls: Vec<(String, HashMap<String, Type>)>,
) -> Vec<(String, HashMap<String, Type>)> {
    let mut out = Vec::with_capacity(calls.len());
    for (callee, solved) in calls {
        if let Some(key) = callee
            .strip_prefix(&format!("{}__", vyrn_frontend::types::FALLIBLE))
            .and_then(|rest| rest.strip_suffix("__success"))
        {
            out.push((
                vyrn_frontend::types::impl_method_name(
                    vyrn_frontend::types::FALLIBLE,
                    key,
                    "isSuccess",
                ),
                solved.clone(),
            ));
        }
        out.push((callee, solved));
    }
    out
}

/// Turns the generic calls one body made into instantiations on the worklist,
/// for every root.
#[allow(clippy::too_many_arguments)]
fn follow<'a>(
    caller: &str,
    calls: Vec<(String, HashMap<String, Type>)>,
    by_name: &HashMap<&str, (FnId, &'a Function)>,
    decls: &HashMap<String, vyrn_frontend::ast::TypeDecl>,
    seen: &mut Vec<(String, String)>,
    queue: &mut VecDeque<(FnId, &'a Function, Vec<Type>)>,
    unresolved: &mut Vec<Unresolved>,
) {
    for (callee, solved) in fallible_twins(calls) {
        let callee: &str = &callee;
        let mut stop = |why, line, args: Vec<Type>| {
            unresolved.push(Unresolved {
                caller: caller.to_string(),
                callee: callee.to_string(),
                line,
                args,
                why,
            })
        };
        // A `std/mem` primitive has no body at any instance: `addr<T>` and
        // `adopt<T, U>` are generic and still one sequence at each call.
        if callee.starts_with(vyrn_frontend::loader::MEM_PREFIX) {
            continue;
        }
        let Some(&(id, target)) = by_name.get(callee) else {
            // A generic seeded builtin (`@join`, `close`, `fromArray`) records
            // a solution like a user call, but its row is a signature with no
            // body to instantiate.
            if vyrn_frontend::prelude::signature(callee).is_some() {
                continue;
            }
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
        // Polymorphic recursion (`f<T>` calling `f<P<T>>`) has no fixed point;
        // without this bound the worklist never ends
        // (`examples/polyrecursion.vyrn`).
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
        queue.push_back((id, target, next));
    }
}

/// Checks the form's structural invariants and returns one sentence per
/// violation: instances in order, concrete, and typed without `<type error>`.
/// It runs on the corpus gate and in the
/// `debug_assert` in [`core::augment`]. It does not re-derive types, which
/// would be a second derivation.
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
        let concrete = [&i.facts.types, &i.facts.produced];
        for (e, line) in &i.facts.exprs {
            let key = e.id();
            for t in concrete.iter().filter_map(|m| m.get(&key)) {
                if matches!(t, Type::Err) {
                    bad.push(format!(
                        "{} @{line}: the {} is typed `<type error>`, and a \
                         program the typed judgment accepts has none",
                        i.spelling(),
                        kind(e)
                    ));
                }
                if mentions_param(t) {
                    bad.push(format!(
                        "{} @{line}: the {} is typed `{t}`, which still names a type \
                         parameter after substitution",
                        i.spelling(),
                        kind(e)
                    ));
                }
            }
        }
        // A release step is not checked for concreteness; see
        // `Instance::releases`.
    }
    bad
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(src: &str) -> Program {
        let mut p = vyrn_frontend::check(src).expect("the fixture checks");
        // The type check alone: the kernel refuses `id`'s return of a `read`
        // parameter, and these tests are about the lowering.
        let (diags, _, _, _) =
            vyrn_frontend::check_and_synthesize(&mut p, None, &Default::default());
        assert!(diags.is_empty(), "{diags:?}");
        p
    }

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

        // The body is `return x`, and `x` is the parameter: concrete in each
        // instance, which is what monomorphizing before the split buys.
        for (inst, want) in l.instances.iter().zip([Type::Int, Type::Str]) {
            let tys: Vec<&Type> = inst
                .facts
                .exprs
                .iter()
                .filter_map(|(e, _)| inst.facts.types.get(&e.id()))
                .collect();
            assert_eq!(tys, vec![&want], "{}", inst.spelling());
        }
    }

    /// [A16]: a node carries what the value HAS and what it must END UP as.
    /// `1` under an `Int32` destination is the smallest program where they
    /// are two.
    #[test]
    fn a_literal_under_a_sized_destination_carries_both_types() {
        let p =
            program("fn main() -> Int64 {\n    let a: Int32 = 1\n    let b = 2\n    return 0\n}\n");
        let l = lower(&p);
        let f = &l.instances[0].facts;
        let pairs: Vec<(String, String)> = f
            .exprs
            .iter()
            .filter(|(e, _)| matches!(e, Expr::Int(_, _)))
            .map(|(e, _)| {
                let key = e.id();
                (f.produced[&key].to_string(), f.types[&key].to_string())
            })
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("Int64".to_string(), "Int32".to_string()),
                ("Int64".to_string(), "Int64".to_string()),
                ("Int64".to_string(), "Int64".to_string()),
            ]
        );
    }

    #[test]
    fn the_lint_refuses_an_instance_that_is_not_concrete() {
        let p = program("fn main() -> Int64 {\n    return 0\n}\n");
        let mut l = lower(&p);
        l.instances[0].type_args.push(Type::Param("T".into()));
        assert!(!lint(&l).is_empty());
    }
}
