//! Move checking for the `consume` capability (RFC-0004).
//!
//! A `consume` parameter takes ownership of its argument: after a variable is
//! passed to one, using it again is an error. This is the first, tractable slice
//! of the capability model — ownership expressed as *intent* (`consume`) and
//! enforced by the compiler, rather than through `&`/move mechanics. It runs as
//! a separate pass after type checking, so the type checker stays unaware of it.
//!
//! `Read`/`Modify`/`Share` impose no restriction in v0.1 (they are surface-only);
//! only `Consume` moves. Analysis is flow-sensitive: `if` merges branches with
//! "may-consume" (a value consumed on either path is consumed afterward), a
//! reassignment revives a variable, and consuming a pre-loop variable inside a
//! loop body is rejected (it would be reused next iteration).
//!
//! **This pass carries types** (RFC-0089 M2, Phase 4a). It keeps a
//! [`crate::declared`] type environment beside its scope stack and can answer
//! `owns_heap` at every binding, argument, return, store, iterable and capture —
//! see [`owning_sites`]. The [`streams`] sub-module below is the one part still
//! name-based and typeless.
//!
//! **It enforces RFC-0089 rules 1–3** (Phase 4b). Three families of error, on
//! top of the `consume` family above:
//!
//! - **Rule 1 — a value moves.** A store of a value that transitively owns heap
//!   (a `let`, an assignment, a field or element store, a literal operand)
//!   takes the source place. The analysis is *last-use aware*: `let t = s` with
//!   no later use of `s` is legal, and the error only fires on the later use.
//!   That is the machinery `consume` already had; rule 1 adds the stores.
//! - **Rule 2 — a borrow is second-class.** A `read`/`modify`/`share` parameter,
//!   a `for` variable and a local bound to a field or element read are all
//!   [`Borrow`]s. They may be observed and passed on, but not stored, not
//!   captured by an escaping closure, and not returned.
//! - **Rule 3 — a return is owned.** Returning a borrow is refused, with the
//!   two fixes named.
//!
//! Every one of these diagnostics is a **menu** (RFC-0087 U2): it prints the
//! move and the later use, then the named ways out (`consume`, `.copy()`).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::declared::{Declared, Scopes};
use crate::diagnostics::Diagnostic;

/// One place a value crosses a boundary RFC-0089 rule 1 governs: a binding, an
/// argument, a return, a store, a `for` iterable or a lambda capture.
///
/// Phase 4a records these and enforces nothing. The count is what sizes 4b: it
/// is how many places 4b's analysis has to be *correct* at, which is a much
/// larger number than the places today's analysis reports on.
#[derive(Clone, Debug)]
pub struct OwningSite {
    /// Which boundary — `bind`, `arg`, `return`, `assign`, `assign-global`,
    /// `field`, `element`, `iterate`, `literal` or `capture`.
    pub kind: &'static str,
    pub line: usize,
    /// The type moved, or `?` where the declared-types reading cannot name it.
    pub ty: String,
    /// Whether the value is read out of a **named place** (a variable or a
    /// field) rather than produced fresh. A place read is what rule 1 turns into
    /// a move, so this is the half of the count 4b must get right; a fresh value
    /// has no earlier owner and can only transfer.
    pub place: bool,
}

impl OwningSite {
    /// Whether the declared-types reading could not name the type. 4b has to
    /// decide what an unknown means, and `own.rs`'s answer — leak, never a wrong
    /// free — is not available to it: a skipped move is a use-after-free.
    pub fn unknown(&self) -> bool {
        self.ty == "?"
    }
}

/// Every [`OwningSite`] in `program`.
///
/// Runs the same walk [`check_accum`] runs, with recording on, and discards the
/// diagnostics. 4a answers the question at every site; 4b enforces on the answer.
pub fn owning_sites(program: &Program) -> Vec<OwningSite> {
    run(program, true, false).1
}

/// Check every function for use-after-consume, returning **all** problems found
/// as structured [`Diagnostic`]s. Each function is checked independently, so
/// a use-after-consume error in one function does not suppress errors in others.
/// Within a function, errors accumulate at **statement boundaries** (the same
/// RFC-0006 model as the type checker): `block` push-and-continues, so two
/// independent consume bugs in one body are both reported. A statement's
/// internals still use `?`, so within a single statement (and a single expression)
/// the first error wins — this is sound because every statement does its
/// sub-expression checking *before* mutating `consumed`/`scope`, so after an
/// error the flow state is consistent for the next statement.
pub fn check_accum(program: &Program) -> Vec<Diagnostic> {
    run(program, false, false).0
}

/// Every place RFC-0089 rule 2 would refuse a **store** of a borrow.
///
/// Rule 2's other two positions — a return and an escaping capture — are
/// enforced by [`check_accum`]. The store position is not, and this is how its
/// cost is measured rather than argued about: it is 288 sites over `examples/`
/// and `std/`, 154 of them `for x in xs { out.push(x) }`, which has no `consume`
/// spelling to migrate to. RFC-0089 "What rule 2 costs" carries the table, and
/// the plan's Phase 4b-2 is where the decision belongs.
///
/// This is the same walk with one flag flipped, so the rule cannot rot while it
/// waits: it is compiled, tested and countable today.
pub fn borrow_store_sites(program: &Program) -> Vec<Diagnostic> {
    run(program, false, true).0
}

/// The one walk, shared by [`check_accum`] and [`owning_sites`]. `record` turns
/// the site collection on; with it off the pass still builds and carries its type
/// environment, and asks `owns_heap` nowhere.
fn run(program: &Program, record: bool, stores: bool) -> (Vec<Diagnostic>, Vec<OwningSite>) {
    let caps: HashMap<String, Vec<Capability>> = program
        .functions
        .iter()
        .map(|f| {
            (
                f.name.clone(),
                f.params.iter().map(|p| p.capability).collect(),
            )
        })
        .collect();
    let globals: HashSet<String> = program.globals.iter().map(|g| g.name.clone()).collect();
    let decl = Declared::new(program);
    let mc = MoveCheck {
        caps: &caps,
        globals: &globals,
        errors: RefCell::new(Vec::new()),
        decl: &decl,
        // Module state is the outermost frame and is built once, not per body.
        vars: RefCell::new(Scopes::new(decl.globals())),
        // Module state is nobody's borrow: it owns what it holds for the whole
        // module, which is why nothing may `consume` it either.
        borrows: RefCell::new(Scopes::new(
            program.globals.iter().map(|g| (g.name.clone(), None)).collect(),
        )),
        ret: RefCell::new(Type::Unit),
        lambda_base: RefCell::new(Vec::new()),
        lambda_escapes: RefCell::new(Vec::new()),
        at_call_site: std::cell::Cell::new(false),
        refuse_stores: stores,
        sites: record.then(|| RefCell::new(Vec::new())),
    };
    let mut out = Vec::new();
    for f in &program.functions {
        mc.errors.borrow_mut().clear();
        mc.function(f);
        for s in mc.errors.borrow_mut().drain(..) {
            let mut d = Diagnostic::from_rendered(s, "movecheck");
            d.file = f.module.clone();
            out.push(d);
        }
    }
    // Test bodies (RFC-0015) move-check as ordinary Unit function bodies under a
    // synthetic name, so use-after-consume inside a test is caught unchanged.
    for (i, t) in program.tests.iter().enumerate() {
        let synthetic = Function {
            name: format!("test@{i}"),
            exported: false,
            module: t.module.clone(),
            doc: None,
            type_params: Vec::new(),
            type_bounds: Default::default(),
            params: Vec::new(),
            ret: Type::Unit,
            body: t.body.clone(),
            line: t.line,
            is_extern: false,
            is_export_extern: false,
            is_gen: false,
            is_mut: false,
        };
        mc.errors.borrow_mut().clear();
        mc.function(&synthetic);
        for s in mc.errors.borrow_mut().drain(..) {
            let mut d = Diagnostic::from_rendered(s, "movecheck");
            d.file = t.module.clone();
            out.push(d);
        }
    }
    // Bench bodies (RFC-0055) move-check identically, under a synthetic `bench@`
    // name.
    for (i, b) in program.benches.iter().enumerate() {
        let synthetic = Function {
            name: format!("bench@{i}"),
            exported: false,
            module: b.module.clone(),
            doc: None,
            type_params: Vec::new(),
            type_bounds: Default::default(),
            params: Vec::new(),
            ret: Type::Unit,
            body: b.body.clone(),
            line: b.line,
            is_extern: false,
            is_export_extern: false,
            is_gen: false,
            is_mut: false,
        };
        mc.errors.borrow_mut().clear();
        mc.function(&synthetic);
        for s in mc.errors.borrow_mut().drain(..) {
            let mut d = Diagnostic::from_rendered(s, "movecheck");
            d.file = b.module.clone();
            out.push(d);
        }
    }
    // RFC-0075: the disposal obligation on a `Stream<T>`. A separate walk over the
    // same bodies rather than a fifth thing threaded through `Consumed`, because
    // the two analyses want OPPOSITE merges at an `if`: use-after-consume is a
    // may-analysis (consumed on either branch ⇒ consumed after), and "disposed
    // exactly once" is a must-analysis. Folding them would have made one of the
    // two wrong at every branch.
    out.extend(streams::check(program, &decl));
    let sites = mc.sites.map(RefCell::into_inner).unwrap_or_default();
    (out, sites)
}

/// Check every function for use-after-consume. Runs after type checking. Returns
/// the first problem found (rendered as the historical `"line {N}: {message}"`
/// string). Thin shim over [`check_accum`].
pub fn check(program: &Program) -> Result<(), String> {
    match check_accum(program).into_iter().next() {
        Some(d) => Err(d.render()),
        None => Ok(()),
    }
}

struct MoveCheck<'a> {
    caps: &'a HashMap<String, Vec<Capability>>,
    /// Module-state binding names (RFC-0013). A global may never be passed to a
    /// `consume` parameter — nothing may take ownership of module state.
    globals: &'a HashSet<String>,
    /// Per-function statement-boundary error sink (RFC-0006 accumulation).
    /// Cleared at the start of each function, drained by `check_accum`.
    errors: RefCell<Vec<String>>,
    /// The declared-types reading (RFC-0089 M2, Phase 4a) — the same one
    /// `own.rs` decides releases with. **Nothing in this pass's diagnostics
    /// reads it yet**; 4b is where it starts to decide.
    decl: &'a Declared,
    /// The type of every binding in scope. A second stack rather than types on
    /// `scope`, because `scope` answers a different question: a `for` variable is
    /// deliberately absent from it (which is what makes a loop variable shadowing
    /// module state still refuse a `consume`), and it is typed here.
    vars: RefCell<Scopes<Option<Type>>>,
    /// Which bindings are BORROWS (RFC-0089 rule 2), in lockstep with `vars` —
    /// see [`MoveCheck::enter`]. A borrow is second-class: observable, passable,
    /// but never stored, captured or returned.
    borrows: RefCell<Scopes<Option<Borrow>>>,
    /// The return type of the function being checked, for rule 3.
    ret: RefCell<Type>,
    /// The frame depth at each enclosing lambda's parameter frame. A name that
    /// resolves BELOW the innermost of these is a capture, not a local.
    lambda_base: RefCell<Vec<usize>>,
    /// Whether each enclosing lambda ESCAPES — RFC-0089 says a non-escaping
    /// lambda (a `map`/`filter` argument) borrows freely and a stored one may
    /// not. Parallel to `lambda_base`.
    lambda_escapes: RefCell<Vec<bool>>,
    /// Set for the duration of one call argument that IS a lambda. A lambda
    /// written directly at a call site is consumed by that call and cannot
    /// outlive it; a lambda anywhere else is stored, and RFC-0037's
    /// defunctionalization makes it a value that can outlive the frame.
    at_call_site: std::cell::Cell<bool>,
    /// Whether rule 2's STORE refusal is on. `false` on every ordinary check —
    /// see [`borrow_store_sites`] for what it costs and why it waits.
    refuse_stores: bool,
    /// Where recorded [`OwningSite`]s go, or `None` on the ordinary check path —
    /// which is what keeps a build and a keystroke paying for nothing.
    sites: Option<RefCell<Vec<OwningSite>>>,
}

/// A binding that names a value somebody else owns (RFC-0089 rule 2).
///
/// The variants exist for the diagnostic, not for the rule: every borrow is
/// refused in the same three positions, and each variant names a different fix.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Borrow {
    /// A `read` or `share` parameter — the caller still owns it.
    Read,
    /// A `modify` parameter — exclusive in-place access, still the caller's.
    Modify,
    /// A `for` variable. Iteration binds a `read` borrow of the element (PLAN
    /// decision log), so the container still owns what the loop sees.
    Element,
    /// A local bound to a field or element read: `let t = r.s`. A place owns its
    /// contents (rule 4), so reading one out does not take it.
    Projection,
}

impl Borrow {
    /// What this borrow is, in words, for the message.
    fn what(self) -> &'static str {
        match self {
            Borrow::Read => "a `read` parameter",
            Borrow::Modify => "a `modify` parameter",
            Borrow::Element => "a loop variable",
            Borrow::Projection => "read out of a place that owns it",
        }
    }

    /// The named ways out (RFC-0087 U2). The order is the order a reader should
    /// try them: take ownership if the callee should have it, copy if both sides
    /// genuinely need a value. `root` is the binding, `path` what was read out
    /// of it — a `consume` goes on the binding, a `.copy()` on the path.
    fn fixes(self, root: &str, path: &str) -> Vec<String> {
        let copy = format!("`{path}.copy()` if both sides need a value");
        match self {
            Borrow::Read | Borrow::Modify => vec![
                format!("declare the parameter `consume {root}` if this function should own it"),
                copy,
            ],
            Borrow::Element | Borrow::Projection => vec![copy],
        }
    }
}

/// A name that has been moved out of, and the menu its later use prints.
#[derive(Clone)]
struct Consumption {
    /// Where the move happened.
    line: usize,
    /// What took it, in words: ``​`take(..)`​``, ``​`drop`​``, "the binding `t`".
    by: String,
    /// The named fixes. Empty for a `consume` parameter, which keeps its own
    /// historical wording — the capability IS the fix there.
    fixes: Vec<String>,
}

/// Consumed variables: name -> what took it.
type Consumed = HashMap<String, Consumption>;

impl Consumption {
    /// A `consume` capability took it — the wording this pass has always used.
    fn by_capability(line: usize, by: String) -> Self {
        Consumption { line, by, fixes: Vec::new() }
    }
}

impl MoveCheck<'_> {
    fn function(&self, f: &Function) {
        let mut consumed: Consumed = HashMap::new();
        let mut scope: Vec<HashSet<String>> =
            vec![f.params.iter().map(|p| p.name.clone()).collect()];
        // Module state is the outermost frame, the parameters the next one — the
        // order every function body sees them in (RFC-0013).
        {
            let mut v = self.vars.borrow_mut();
            let mut b = self.borrows.borrow_mut();
            v.truncate(1);
            b.truncate(1);
            v.enter();
            b.enter();
            for p in &f.params {
                v.bind(&p.name, Some(p.ty.clone()));
                // RFC-0089 rule 2: everything but `consume` is a borrow, and only
                // a type that owns heap has anything to borrow.
                b.bind(
                    &p.name,
                    match p.capability {
                        Capability::Consume => None,
                        _ if !self.decl.owns_heap(&p.ty) => None,
                        Capability::Modify => Some(Borrow::Modify),
                        _ => Some(Borrow::Read),
                    },
                );
            }
        }
        *self.ret.borrow_mut() = f.ret.clone();
        self.lambda_base.borrow_mut().clear();
        self.lambda_escapes.borrow_mut().clear();
        self.block(&f.body, &mut consumed, &mut scope);
    }

    /// Push a frame on BOTH stacks. They are read as one environment — a name's
    /// type and whether it is a borrow — so they are never entered apart.
    fn enter(&self) {
        self.vars.borrow_mut().enter();
        self.borrows.borrow_mut().enter();
    }

    fn exit(&self) {
        self.vars.borrow_mut().exit();
        self.borrows.borrow_mut().exit();
    }

    /// Bind `name` with its type and its borrow status.
    fn bind(&self, name: &str, ty: Option<Type>, borrow: Option<Borrow>) {
        self.vars.borrow_mut().bind(name, ty);
        self.borrows.borrow_mut().bind(name, borrow);
    }

    /// Whether `name` names a borrow here.
    fn borrow_of(&self, name: &str) -> Option<Borrow> {
        self.borrows.borrow().get(name).copied().flatten()
    }

    /// The declared type of `e` here, or `None` where this reading cannot name it.
    ///
    /// [`crate::declared::Declared::type_of`] first, then the readings only this
    /// pass may have. **The widening lives here and not in `Declared`** because
    /// `own.rs` shares that reading and decides `free` with it: a type answered
    /// there that was not answered before changes what a program releases. Here
    /// it changes only what the program is allowed to say.
    fn type_of(&self, e: &Expr) -> Option<Type> {
        if let Some(t) = self.decl.type_of(&self.vars.borrow(), e) {
            return Some(t);
        }
        match e {
            // A field's type comes out of the record declaration. Without this
            // every field read is unknown, and so is everything read out of one.
            Expr::Field { expr, field, .. } => {
                let base = self.type_of(expr)?;
                match crate::types::resolve(&base, self.decl.decls()) {
                    Type::Record(fs) => {
                        fs.iter().find(|f| &f.name == field).map(|f| f.ty.clone())
                    }
                    _ => None,
                }
            }
            // An element read: `xs[i]` lowers to `at`, and `x.copy()` is already
            // answered by `Declared`. The element type is the container's.
            Expr::Call { name, args, .. } if name == "at" => {
                let c = self.type_of(args.first()?)?;
                self.decl.elem_of(&c)
            }
            _ => None,
        }
    }

    /// A store of `value` into `into` — RFC-0089 rule 1, and rule 2's refusal.
    ///
    /// `into` is the destination in words ("the binding `t`", "the field `r.s`").
    /// Three outcomes:
    ///
    /// - the value owns no heap, or is fresh (a call result, a literal, an
    ///   operator result): nothing happens, because nothing else holds it;
    /// - the source is a **borrow**: refused here, with its fixes named;
    /// - the source is an owned place: it MOVES, and a later use of it is the
    ///   error [`MoveCheck::expr`] reports.
    ///
    /// `outlives` is what separates a store from a rebinding. A borrow's
    /// lifetime is the call, so putting one in a field, a container, module
    /// state or a return is refused, and giving it a second local name is not:
    /// `let t = s` cannot outlive `s`, and rule 2's whole point is that a borrow
    /// needs no lifetime because it never leaves the frame. The new name is a
    /// borrow too — see [`MoveCheck::borrow_from`].
    ///
    /// A type this reading cannot name does NOT move. That is the same
    /// under-approximation `own.rs` makes and it costs the same thing — a
    /// diagnostic that is not printed. It is not the unsound direction: 4b
    /// decides what a program may SAY, and every value it fails to move is one
    /// today's engines already leak rather than free twice.
    fn store(
        &self,
        value: &Expr,
        // A closure and not a string: most stores take a fresh value and print
        // nothing, and rendering a destination at every `let` in the corpus cost
        // more than the rest of this pass put together.
        into: &dyn Fn() -> String,
        line: usize,
        outlives: bool,
        consumed: &mut Consumed,
    ) -> Result<(), String> {
        let Some((root, path)) = place_path(value) else { return Ok(()) };
        // A scalar copies. An unnamed type is left alone — see the doc above.
        if !self.type_of(value).is_some_and(|t| self.decl.owns_heap(&t)) {
            return Ok(());
        }
        if let Some(b) = self.borrow_of(&root) {
            if !outlives || !self.refuse_stores {
                return Ok(());
            }
            // Rule 2. A borrow may be observed and passed on; it may not be put
            // anywhere that outlives the call.
            return Err(menu(
                line,
                format!("`{path}` may not be stored into {} — it is {}", into(), b.what()),
                b.fixes(&root, &path),
            ));
        }
        // Reading a field out of a record does not take the record: the binding
        // it makes is itself a borrow (recorded by the caller), so nothing moves.
        if path != root {
            return Ok(());
        }
        consumed.insert(
            root,
            Consumption {
                line,
                by: into(),
                fixes: vec![format!("`{path}.copy()` if both sides need a value")],
            },
        );
        Ok(())
    }

    /// The borrow status a `let` of `value` gives its binding.
    ///
    /// A field or element read binds a [`Borrow::Projection`]: the aggregate
    /// still owns it, so the new name may be read but not stored on. Everything
    /// else — a fresh value, or a place that just moved — binds an owner.
    fn borrow_from(&self, value: &Expr) -> Option<Borrow> {
        // Shape first, type second. Every other initializer is a fresh value, and
        // asking the type of one walks a concat chain for an answer nothing reads.
        if !matches!(
            value,
            Expr::Var { .. } | Expr::Field { .. } | Expr::Call { .. }
        ) {
            return None;
        }
        if !self.type_of(value).is_some_and(|t| self.decl.owns_heap(&t)) {
            return None;
        }
        match place_path(value) {
            // `let t = r.s` / `let t = xs[i]` — a projection of somebody's place.
            Some((root, path)) if path != root => Some(Borrow::Projection),
            // `let t = s` where `s` is itself a borrow: the borrow travels.
            Some((root, _)) => self.borrow_of(&root),
            None => match value {
                Expr::Call { name, args, .. } if name == "at" => args
                    .first()
                    .and_then(place_path)
                    .map(|_| Borrow::Projection),
                _ => None,
            },
        }
    }

    /// What a pattern's binders name: one type per binder, and whether they are
    /// borrows.
    ///
    /// Destructuring a place looks INTO it — `match o { Some(v) => .. }` does not
    /// take `o` apart, so `v` is a projection of it and rule 2 applies. A
    /// scrutinee that is a fresh value has no other owner, so its payload is
    /// owned.
    fn payload_binding(&self, scrutinee: &Expr, p: &Pattern) -> (Vec<Option<Type>>, Option<Borrow>) {
        let n = pattern_bindings(p).len();
        let ty = self.type_of(scrutinee);
        let tys: Vec<Option<Type>> = match (ty.as_ref().map(|t| crate::types::resolve(t, self.decl.decls())), p) {
            (Some(Type::Option(t)), Pattern::Some(_)) => vec![Some(*t)],
            (Some(Type::Result(t, _)), Pattern::Ok(_) | Pattern::Success(_)) => vec![Some(*t)],
            (Some(Type::Result(_, e)), Pattern::Err(_) | Pattern::Failure(_)) => vec![Some(*e)],
            (Some(Type::Enum(vs)), Pattern::Variant(name, _)) => vs
                .iter()
                .find(|v| &v.name == name)
                .map(|v| v.payload.iter().cloned().map(Some).collect())
                .unwrap_or_else(|| vec![None; n]),
            _ => vec![None; n],
        };
        let borrow = place_path(scrutinee).map(|_| Borrow::Projection);
        (tys, borrow)
    }

    /// Rule 3: a function returns an owned value, always.
    fn check_return(&self, e: &Expr, line: usize) -> Result<(), String> {
        if !self.decl.owns_heap(&self.ret.borrow()) {
            return Ok(());
        }
        let Some((root, path)) = place_path(e) else { return Ok(()) };
        let Some(b) = self.borrow_of(&root) else { return Ok(()) };
        Err(menu(
            line,
            format!("`{path}` may not be returned — it is {}, and a return is owned", b.what()),
            b.fixes(&root, &path),
        ))
    }

    /// Record one site RFC-0089 rule 1 governs. `declared` overrides the
    /// expression's own type, for a `let` that carries an annotation.
    ///
    /// A no-op unless [`owning_sites`] asked for the record, so the ordinary
    /// check path never asks `owns_heap` at all.
    fn site(&self, kind: &'static str, line: usize, e: &Expr, declared: Option<&Type>) {
        let Some(sink) = &self.sites else { return };
        // A literal has no earlier owner, so nothing about it can be a move. The
        // filter is here rather than at each call site because every one of them
        // would need it.
        if declared.is_none()
            && matches!(
                e,
                Expr::Str(_) | Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_)
            )
        {
            return;
        }
        let ty = match declared {
            Some(t) => Some(t.clone()),
            None => self.type_of(e),
        };
        let place = matches!(e, Expr::Var { .. } | Expr::Field { .. });
        match ty {
            Some(t) if self.decl.owns_heap(&t) => sink.borrow_mut().push(OwningSite {
                kind,
                line,
                ty: t.to_string(),
                place,
            }),
            None => sink.borrow_mut().push(OwningSite {
                kind,
                line,
                ty: "?".to_string(),
                place,
            }),
            Some(_) => {}
        }
    }

    /// Returns whether this block **diverges** — every path out of it leaves via
    /// `return`/`break`/`continue` (RFC-0060). A statement after a diverging one
    /// is unreachable, so it is not checked (use-after-move there is not an
    /// error), and its consumptions never flow to the block's exit.
    fn block(&self, b: &Block, consumed: &mut Consumed, scope: &mut Vec<HashSet<String>>) -> bool {
        scope.push(HashSet::new());
        self.enter();
        let mut diverged = false;
        for s in &b.stmts {
            if diverged {
                // Unreachable after a `return`/`break`/`continue`: skip it
                // (the `return` precedent — code after it is unreachable-clean).
                break;
            }
            match self.stmt(s, consumed, scope) {
                Ok(d) => diverged = d,
                Err(msg) => {
                    self.errors.borrow_mut().push(msg);
                    // Keep going: the statement's sub-expression check ran before
                    // any mutation, so state is consistent for the next statement.
                }
            }
        }
        self.exit();
        scope.pop();
        diverged
    }

    fn in_scope(scope: &[HashSet<String>], name: &str) -> bool {
        scope.iter().any(|f| f.contains(name))
    }

    /// Returns whether this statement **diverges** (leaves via
    /// `return`/`break`/`continue` on every path) — see [`MoveCheck::block`].
    fn stmt(
        &self,
        s: &Stmt,
        consumed: &mut Consumed,
        scope: &mut Vec<HashSet<String>>,
    ) -> Result<bool, String> {
        match s {
            Stmt::Let {
                name,
                value,
                ty,
                line,
                ..
            } => {
                self.expr(value, consumed, scope)?;
                // The binding's type: what it was declared, else what the
                // initializer yields — read against the PRE-binding environment,
                // so `let x = x + b` resolves the old `x`.
                let bty = ty.clone().or_else(|| self.type_of(value));
                self.site("bind", *line, value, bty.as_ref());
                let borrow = self.borrow_from(value);
                // A projection is a borrow rather than a move, so only a whole
                // place moves here — `store` decides which.
                self.store(value, &|| format!("the binding `{name}`"), *line, false, consumed)?;
                self.bind(name, bty, borrow);
                consumed.remove(name); // a fresh binding is alive again
                scope.last_mut().unwrap().insert(name.clone());
                Ok(false)
            }
            Stmt::Assign { name, value, line } => {
                self.expr(value, consumed, scope)?;
                // Module state (RFC-0013) is a place with a whole-module lifetime,
                // so 4b treats a store into it differently from a local's.
                let global = self.globals.contains(name) && !Self::in_scope(scope, name);
                self.site(if global { "assign-global" } else { "assign" }, *line, value, None);
                let into = || if global {
                    format!("module state `{name}`")
                } else {
                    format!("`{name}`")
                };
                self.store(value, &into, *line, global, consumed)?;
                consumed.remove(name); // reassignment revives it
                Ok(false)
            }
            Stmt::SetField {
                name,
                field,
                value,
                line,
            } => {
                self.site("field", *line, value, None);
                self.expr(value, consumed, scope)?;
                self.store(value, &|| format!("the field `{name}.{field}`"), *line, true, consumed)?;
                Ok(false)
            }
            // `a[i] = v` — the stored value is consumed like a `push` argument
            // (neither `push` nor the store marks it consumed, since no user
            // `consume` capability is involved), so just check both sub-exprs.
            // An element store and a map-value store are the same node, so one
            // row covers both places.
            Stmt::IndexSet {
                name,
                index,
                value,
                line,
            } => {
                self.expr(index, consumed, scope)?;
                self.site("element", *line, value, None);
                self.expr(value, consumed, scope)?;
                self.store(value, &|| format!("`{name}`"), *line, true, consumed)?;
                Ok(false)
            }
            Stmt::Return { value, line } => {
                if let Some(e) = value {
                    self.site("return", *line, e, None);
                    self.expr(e, consumed, scope)?;
                    self.check_return(e, *line)?;
                }
                Ok(true)
            }
            // `break`/`continue` (RFC-0060) consume nothing but terminate the
            // path — code after them in the same block is unreachable.
            Stmt::Break { .. } | Stmt::Continue { .. } => Ok(true),
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.expr(cond, consumed, scope)?;
                let mut then_c = consumed.clone();
                let then_div = self.block(then_block, &mut then_c, scope);
                let mut else_c = consumed.clone();
                let else_div = match else_block {
                    Some(eb) => self.block(eb, &mut else_c, scope),
                    None => false,
                };
                // may-consume, but a branch that DIVERGES (break/continue/return)
                // carries its consumptions out the exit path, not to the code
                // after the `if` — so a value moved only on a break-path is not
                // considered moved on the fall-through (RFC-0060).
                if !then_div {
                    for (k, v) in then_c {
                        consumed.entry(k).or_insert(v);
                    }
                }
                if !else_div {
                    for (k, v) in else_c {
                        consumed.entry(k).or_insert(v);
                    }
                }
                Ok(then_div && else_div)
            }
            // `if let PAT = e { .. } else { .. }` (RFC-0060): the scrutinee is
            // consumed eagerly (like a `match` scrutinee), the binders are fresh
            // locals of the then-arm, and the two arms merge exactly like `if` —
            // a branch that diverges carries its consumptions out, not through.
            Stmt::IfLet {
                pattern,
                scrutinee,
                then_block,
                else_block,
                ..
            } => {
                self.expr(scrutinee, consumed, scope)?;
                let mut then_c = consumed.clone();
                scope.push(HashSet::new());
                self.enter();
                let (tys, borrow) = self.payload_binding(scrutinee, pattern);
                for (i, b) in pattern_bindings(pattern).into_iter().enumerate() {
                    scope.last_mut().unwrap().insert(b.to_string());
                    // Recording it is the point: an unrecorded binder falls
                    // through to whatever the enclosing scope calls that name
                    // (`own.rs`'s shadowing lesson).
                    self.bind(b, tys.get(i).cloned().flatten(), borrow);
                }
                let then_div = self.block(then_block, &mut then_c, scope);
                self.exit();
                scope.pop();
                let mut else_c = consumed.clone();
                let else_div = match else_block {
                    Some(eb) => self.block(eb, &mut else_c, scope),
                    None => false,
                };
                if !then_div {
                    for (k, v) in then_c {
                        consumed.entry(k).or_insert(v);
                    }
                }
                if !else_div {
                    for (k, v) in else_c {
                        consumed.entry(k).or_insert(v);
                    }
                }
                Ok(then_div && else_div)
            }
            Stmt::While { cond, body, .. } => {
                // The condition re-runs on every iteration, so consumption in it
                // is loop-consumption exactly like the body's (`while take(x)`
                // would use `x` again next time around) — track both in the
                // in-loop map and run the same next-iteration check.
                let mut body_c = consumed.clone();
                self.expr(cond, &mut body_c, scope)?;
                let body_div = self.block(body, &mut body_c, scope);
                self.check_loop_reuse(consumed, &body_c, scope, body_div)?;
                for (k, v) in body_c {
                    consumed.entry(k).or_insert(v);
                }
                Ok(false)
            }
            // A `for` loop consumes like a `while`: the iterable is read once,
            // and consuming an outer binding in the body is a use-again error.
            Stmt::ForIn {
                var,
                iter,
                body,
                line,
            } => {
                self.expr(iter, consumed, scope)?;
                self.site("iterate", *line, iter, None);
                // RFC-0089 decided the loop variable is a `read` borrow, so 4b
                // needs its type. It is the iterable's element type.
                let elem = self.type_of(iter).and_then(|t| self.decl.elem_of(&t));
                let borrow = elem
                    .as_ref()
                    .is_some_and(|t| self.decl.owns_heap(t))
                    .then_some(Borrow::Element);
                let mut body_c = consumed.clone();
                self.enter();
                self.bind(var, elem, borrow);
                let body_div = self.block(body, &mut body_c, scope);
                self.exit();
                self.check_loop_reuse(consumed, &body_c, scope, body_div)?;
                for (k, v) in body_c {
                    consumed.entry(k).or_insert(v);
                }
                Ok(false)
            }
            // A `panic(..)` statement diverges (RFC-0079), which here means
            // exactly what `break`/`continue` mean: what follows is unreachable
            // and a consumption on this path never flows to the block's exit.
            // Matched by name rather than by type because this pass has no
            // types; `panic` is reserved, so no user function can be it.
            Stmt::Expr(e) => self
                .expr(e, consumed, scope)
                .map(|_| matches!(e, Expr::Call { name, .. } if name == "panic")),
            // A `region` is an ordinary nested block for move checking; it
            // diverges iff its body does (a `break` inside it exits the loop).
            Stmt::Region { body, .. } => Ok(self.block(body, consumed, scope)),
            // `drop name;` consumes the binding: using it afterward is a
            // use-after-drop, caught by the same machinery as `consume`.
            Stmt::Drop { name, line } => {
                if let Some(c) = consumed.get(name) {
                    let (cline, consumer) = (c.line, &c.by);
                    return Err(menu(
                        *line,
                        format!(
                            "`{name}` is dropped here but was already consumed by \
                             {consumer} on line {cline}"
                        ),
                        c.fixes.clone(),
                    ));
                }
                consumed.insert(
                    name.clone(),
                    Consumption::by_capability(*line, "`drop`".to_string()),
                );
                Ok(false)
            }
        }
    }

    /// Record a lambda capture: a name read inside a lambda that resolves to a
    /// frame BELOW the lambda's own parameter frame.
    ///
    /// It counts mentions, not distinct names — a value captured and read twice
    /// is two sites, because 4b checks each read.
    fn capture_site(&self, name: &str, line: usize) {
        if self.sites.is_none() {
            return;
        }
        let inside = match self.lambda_base.borrow().last() {
            Some(&base) => base,
            None => return,
        };
        let (frame, ty) = {
            let v = self.vars.borrow();
            (v.frame_of(name), v.get(name).cloned().flatten())
        };
        if frame.is_some_and(|f| f < inside) {
            self.site(
                "capture",
                line,
                &Expr::Var {
                    name: name.to_string(),
                    line,
                },
                ty.as_ref(),
            );
        }
    }

    /// Exclusivity: `f(modify a, .. a ..)` is refused.
    ///
    /// `modify` is exclusive in-place access. Handing the same place to a
    /// `modify` parameter and to any other parameter of the same call gives the
    /// callee two names for one value, and the callee was told it had one.
    fn check_exclusive(&self, callee: &str, args: &[Expr], line: usize) -> Result<(), String> {
        let Some(caps) = self.caps.get(callee) else { return Ok(()) };
        for (i, a) in args.iter().enumerate() {
            if caps.get(i) != Some(&Capability::Modify) {
                continue;
            }
            let Some((root, path)) = place_path(a) else { continue };
            for (j, b) in args.iter().enumerate() {
                if i != j && streams::mentions(b, &root) {
                    return Err(menu(
                        line,
                        format!(
                            "`{path}` is passed to `{callee}` as `modify` and read again in the \
                             same call — a `modify` borrow is exclusive"
                        ),
                        vec![
                            format!("`{root}.copy()` for the second argument"),
                            "or split the call so the two accesses do not overlap".to_string(),
                        ],
                    ));
                }
            }
        }
        Ok(())
    }

    /// RFC-0089 rule 2 at a capture: an ESCAPING closure may not hold a borrow.
    ///
    /// A lambda written directly as a call argument (`map(xs, |x| ..)`) does not
    /// outlive the call, so it borrows freely — that is the common case and the
    /// rule leaves it alone. A lambda that is stored is a value under RFC-0037's
    /// defunctionalization, and a borrow inside one has no lifetime to stand on.
    fn check_capture(&self, name: &str, line: usize) -> Result<(), String> {
        let Some(&inside) = self.lambda_base.borrow().last() else { return Ok(()) };
        if !self.lambda_escapes.borrow().last().copied().unwrap_or(false) {
            return Ok(());
        }
        if !self.vars.borrow().frame_of(name).is_some_and(|f| f < inside) {
            return Ok(());
        }
        let Some(b) = self.borrow_of(name) else { return Ok(()) };
        Err(menu(
            line,
            format!(
                "`{name}` may not be captured by a closure that outlives this call — it is {}",
                b.what()
            ),
            b.fixes(name, name),
        ))
    }

    /// The loop-body reuse check: a variable consumed in the body (`body_c`) that
    /// was live before the loop would be consumed *again* on the next iteration.
    /// Skipped when the body **diverges unconditionally** (`body_div`) — it then
    /// runs at most once (a straight-line `consume(x); break`), so the
    /// consumption is legal and flows out to the enclosing scope instead.
    fn check_loop_reuse(
        &self,
        consumed: &Consumed,
        body_c: &Consumed,
        scope: &[HashSet<String>],
        body_div: bool,
    ) -> Result<(), String> {
        if body_div {
            return Ok(());
        }
        for (k, c) in body_c {
            if !consumed.contains_key(k) && Self::in_scope(scope, k) {
                let (line, consumer) = (c.line, &c.by);
                return Err(menu(
                    line,
                    format!(
                        "`{k}` is consumed by {consumer} inside a loop, \
                         so it would be used again on the next iteration"
                    ),
                    c.fixes.clone(),
                ));
            }
        }
        Ok(())
    }

    fn expr(
        &self,
        e: &Expr,
        consumed: &mut Consumed,
        scope: &mut Vec<HashSet<String>>,
    ) -> Result<(), String> {
        match e {
            Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => Ok(()),
            Expr::Var { name, line } => {
                self.capture_site(name, *line);
                self.check_capture(name, *line)?;
                if let Some(c) = consumed.get(name) {
                    // A `consume` capability keeps the wording it has always had:
                    // the capability IS the fix, so there is no menu to print.
                    if c.fixes.is_empty() {
                        let (cline, consumer) = (c.line, &c.by);
                        return Err(format!(
                            "line {line}: `{name}` is used here but was already consumed by \
                             {consumer} on line {cline}\n  (a `consume` parameter takes \
                             ownership; the value can't be used afterward)"
                        ));
                    }
                    // RFC-0089 rule 1. Both lines, then the ways out.
                    return Err(menu(
                        c.line,
                        format!(
                            "`{name}` was moved here into {}\nline {line}: ... and `{name}` is \
                             used again here",
                            c.by
                        ),
                        c.fixes.clone(),
                    ));
                }
                Ok(())
            }
            Expr::Unary { expr, .. } => self.expr(expr, consumed, scope),
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs, consumed, scope)?;
                self.expr(rhs, consumed, scope)
            }
            Expr::Field { expr, .. } => self.expr(expr, consumed, scope),
            Expr::Try { expr, .. } => self.expr(expr, consumed, scope),
            // A literal's operands are places too: `Ring { slots: xs }` puts `xs`
            // where the record owns it, exactly as an argument does.
            Expr::StructLit { name, fields, line } => {
                for (f, v) in fields {
                    self.site("literal", *line, v, None);
                    self.expr(v, consumed, scope)?;
                    self.store(v, &|| format!("the field `{name}.{f}`"), *line, true, consumed)?;
                }
                Ok(())
            }
            Expr::TryConstruct { name, args, line } => {
                for a in args {
                    self.site("literal", *line, a, None);
                    self.expr(a, consumed, scope)?;
                    self.store(a, &|| format!("`{name}`"), *line, true, consumed)?;
                }
                Ok(())
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee, consumed, scope)?;
                let base = consumed.clone();
                let mut merged: Option<Consumed> = None;
                for arm in arms {
                    let mut c = base.clone();
                    scope.push(HashSet::new());
                    self.enter();
                    let (tys, borrow) = self.payload_binding(scrutinee, &arm.pattern);
                    for (i, b) in pattern_bindings(&arm.pattern).into_iter().enumerate() {
                        scope.last_mut().unwrap().insert(b.to_string());
                        self.bind(b, tys.get(i).cloned().flatten(), borrow);
                    }
                    let r = self.expr(&arm.body, &mut c, scope);
                    self.exit();
                    r?;
                    scope.pop();
                    match &mut merged {
                        None => merged = Some(c),
                        Some(m) => {
                            for (k, v) in c {
                                m.entry(k).or_insert(v);
                            }
                        }
                    }
                }
                if let Some(m) = merged {
                    *consumed = m;
                }
                Ok(())
            }
            // `if` as an expression (RFC-0030): its two branches are match arms —
            // the condition consumes eagerly, then each branch runs from the same
            // base and a value consumed on either path is may-consumed afterward.
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                self.expr(cond, consumed, scope)?;
                let base = consumed.clone();
                let mut then_c = base.clone();
                self.expr(then_branch, &mut then_c, scope)?;
                let mut else_c = base;
                if let Some(eb) = else_branch {
                    self.expr(eb, &mut else_c, scope)?;
                }
                for (k, v) in then_c.into_iter().chain(else_c) {
                    consumed.entry(k).or_insert(v);
                }
                Ok(())
            }
            Expr::Call { name, args, line } => {
                self.check_exclusive(name, args, *line)?;
                let caps = self.caps.get(name);
                // Left-to-right: check each argument, then apply its consumption,
                // so passing the same variable to two `consume` params is caught.
                for (i, arg) in args.iter().enumerate() {
                    self.site("arg", *line, arg, None);
                    self.at_call_site.set(true);
                    let r = self.expr(arg, consumed, scope);
                    self.at_call_site.set(false);
                    r?;
                    if caps.and_then(|c| c.get(i)) == Some(&Capability::Consume) {
                        if let Expr::Var { name: v, line: vl } = arg {
                            if !Self::in_scope(scope, v) {
                                self.reject_consume_global(v, name, false, *vl)?;
                            }
                            consumed.entry(v.clone()).or_insert(Consumption::by_capability(
                                *line,
                                format!("`{name}(..)`"),
                            ));
                        }
                    } else if sinks(name, i) {
                        // A builtin that STORES its argument in a container is a
                        // `consume` parameter that has no signature to say so
                        // (RFC-0087 §2b). Rule 1 governs it exactly as it governs
                        // `xs = [.., v]`, which is what it means.
                        self.store(arg, &|| format!("`{name}(..)`"), *line, true, consumed)?;
                    }
                }
                Ok(())
            }
            Expr::ArrayLit { elems, line } => {
                for e in elems {
                    self.site("literal", *line, e, None);
                    self.expr(e, consumed, scope)?;
                    self.store(e, &|| "the array literal".to_string(), *line, true, consumed)?;
                }
                Ok(())
            }
            Expr::MapLit { entries, line } => {
                for (k, v) in entries {
                    self.expr(k, consumed, scope)?;
                    self.site("literal", *line, v, None);
                    self.expr(v, consumed, scope)?;
                    self.store(k, &|| "the map literal".to_string(), *line, true, consumed)?;
                    self.store(v, &|| "the map literal".to_string(), *line, true, consumed)?;
                }
                Ok(())
            }
            // A lambda body (RFC-0023): its untyped params are fresh locals; walk
            // the body so a `consume`-misuse inside it is still caught. Captured
            // bindings are read-only (the checker forbids consuming/dropping them),
            // so a reference to one that was already consumed surfaces the standard
            // use-after-consume error here too.
            Expr::Lambda { params, body, .. } => {
                let escapes = !self.at_call_site.replace(false);
                self.lambda_escapes.borrow_mut().push(escapes);
                scope.push(HashSet::new());
                self.enter();
                for p in params {
                    scope.last_mut().unwrap().insert(p.clone());
                    self.bind(p, None, None);
                }
                // Everything read below this frame is a capture (RFC-0089's
                // no-retain rule is about exactly these).
                self.lambda_base
                    .borrow_mut()
                    .push(self.vars.borrow().depth() - 1);
                let r = match body {
                    LambdaBody::Expr(inner) => self.expr(inner, consumed, scope),
                    LambdaBody::Block(b) => {
                        self.block(b, consumed, scope);
                        Ok(())
                    }
                };
                self.lambda_base.borrow_mut().pop();
                self.lambda_escapes.borrow_mut().pop();
                self.exit();
                scope.pop();
                r
            }
            // `spawn f(args)` moves arguments exactly like a direct call: a
            // `consume` parameter takes ownership across the task boundary.
            Expr::Spawn { name, args, line } => {
                self.check_exclusive(name, args, *line)?;
                let caps = self.caps.get(name);
                for (i, arg) in args.iter().enumerate() {
                    self.site("arg", *line, arg, None);
                    self.expr(arg, consumed, scope)?;
                    if caps.and_then(|c| c.get(i)) == Some(&Capability::Consume) {
                        if let Expr::Var { name: v, line: vl } = arg {
                            if !Self::in_scope(scope, v) {
                                self.reject_consume_global(v, name, true, *vl)?;
                            }
                            consumed.entry(v.clone()).or_insert(Consumption::by_capability(
                                *line,
                                format!("`spawn {name}(..)`"),
                            ));
                        }
                    }
                }
                Ok(())
            }
        }
    }

    /// Reject passing a module-state binding to a `consume` parameter (RFC-0013):
    /// nothing may take ownership of module state. A local of the same name is
    /// tracked in `scope` elsewhere; this only fires when `v` is genuinely a
    /// global. The `scope` shadowing check is done by the caller having already
    /// excluded locals — here we only know the name is a global if it is in the
    /// global set AND not shadowed, which the type checker's scope resolves; for
    /// move checking a global is never in `scope`'s binder sets, so membership in
    /// `globals` alone (when not a param/let) is decisive.
    fn reject_consume_global(
        &self,
        v: &str,
        callee: &str,
        spawned: bool,
        line: usize,
    ) -> Result<(), String> {
        if self.globals.contains(v) {
            let form = if spawned {
                format!("spawn {callee}(..)")
            } else {
                format!("{callee}(..)")
            };
            return Err(format!(
                "line {line}: module state `{v}` may not be passed to a `consume` parameter \
                 via `{form}` — nothing may take ownership of module state (it lives for the \
                 whole module and is never dropped)"
            ));
        }
        Ok(())
    }
}

/// RFC-0075 — the linearity of `Stream<T>`: acquired once, disposed exactly once.
///
/// This is the milestone's whole claim, so it is worth stating what it is not.
/// `own.rs` already reclaims an owned heap value at block exit and on every
/// divergent exit (RFC-0060), which is "owned and dropped". A stream is stronger:
/// disposal must be *written*, because M2's producer has a teardown that no
/// generic memory drop can run, and the tRPC incidents this RFC quotes were live
/// producers rather than unreachable bytes. So the obligation is checked here and
/// the release is emitted by the construct that discharges it — a stream binding
/// is never in a `drop_stack` frame of its own.
///
/// The analysis is deliberately name-based and typeless, like the rest of this
/// file: movecheck runs only on programs the checker already accepted, so
/// `close(x)` implies `x` is a stream and there is no read operation on a stream
/// at all — every mention of a stream binding is a move. That last fact is what
/// makes a one-pass syntactic walk exact instead of approximate.
///
/// Known limit, shared with the `Consumed` map above: bindings are keyed by NAME,
/// so an inner `let s = 1` shadowing an outer stream `s` reads as a disposal of
/// the outer one. Erring toward accepting matches the existing pass; a scope-id
/// key would have to be introduced for both at once.
mod streams {
    use std::collections::HashMap;

    use crate::ast::*;
    use crate::declared::Declared;
    use crate::diagnostics::Diagnostic;

    /// What a straight-line statement list does to one live stream binding.
    #[derive(Clone, Copy, Default)]
    struct Scan {
        /// Disposed on every path that FALLS OUT of the list.
        disposed: bool,
        /// Nothing falls out — every path leaves via `return`/`break`/`continue`,
        /// so `disposed` says nothing about what follows.
        diverges: bool,
        /// Some path abandons it: a `return` that does not move it out, or two
        /// branches that disagree about whether it was disposed (one of those two
        /// paths is wrong whatever comes next, so it is reported here rather than
        /// left to a later statement to make look fine).
        leaked: bool,
        /// Disposed, then mentioned again on the same path.
        doubled: bool,
    }

    pub fn check(program: &Program, decl: &Declared) -> Vec<Diagnostic> {
        // Functions whose return type is a stream, with the rendering the
        // diagnostic quotes. `fromArray`/`fromStep` are the builtin producers and
        // carry no element type here — this pass has no types — so a stream from
        // one of them is spelled plainly `Stream`.
        let producers: HashMap<&str, String> = program
            .functions
            .iter()
            .filter(|f| decl.must_use(&f.ret))
            .map(|f| (f.name.as_str(), rendered_ret(f)))
            .collect();
        let mut out = Vec::new();
        for f in &program.functions {
            // A `Stream` parameter carries the obligation into the callee: the
            // caller discharged its own by moving it, and `fn sink(s: Stream<T>) {}`
            // must not be the hole that lets it evaporate.
            let mut live: Vec<(String, String)> = Vec::new();
            for p in &f.params {
                if let Type::Stream(_) = p.ty {
                    let s = scan(&f.body.stmts, &p.name, false);
                    report(&mut out, &s, f.line, &p.name, &p.ty.to_string(), &f.module);
                    live.push((p.name.clone(), p.ty.to_string()));
                }
            }
            block(&f.body, &mut live, &producers, &f.module, decl, &mut out);
        }
        for t in &program.tests {
            block(&t.body, &mut Vec::new(), &producers, &t.module, decl, &mut out);
        }
        for b in &program.benches {
            block(&b.body, &mut Vec::new(), &producers, &b.module, decl, &mut out);
        }
        out
    }

    /// How a producer's stream type is quoted at a CALL site.
    ///
    /// A generic producer — every std/stream combinator is one — returns
    /// `Stream<U>`, and quoting that at `let m = map(feed(), double)` names a
    /// type parameter the program never wrote. This pass has no types, so it
    /// cannot say `Stream<Int64>` either; it says `Stream`, which is what it
    /// already said for `fromArray` and is an under-specification rather than a
    /// wrong name. The match is on the rendered spelling because a signature's
    /// type parameter is not reliably a `Type::Param` before the checker runs.
    fn rendered_ret(f: &Function) -> String {
        let r = f.ret.to_string();
        let mentions = |p: &String| {
            r.split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|w| w == p.as_str())
        };
        if f.type_params.iter().any(mentions) {
            return "Stream".to_string();
        }
        r
    }

    fn report(
        out: &mut Vec<Diagnostic>,
        s: &Scan,
        line: usize,
        name: &str,
        ty: &str,
        module: &Option<String>,
    ) {
        let msg = if s.doubled {
            format!("`{name}` is a `{ty}` and is disposed more than once")
        } else if s.leaked || !(s.disposed || s.diverges) {
            format!("`{name}` is a `{ty}` and is never disposed")
        } else {
            return;
        };
        let mut d = Diagnostic::error(line, 0, "movecheck", msg);
        d.note = Some(format!(
            "a stream must be consumed with `for … in`, forwarded by returning it, \
             or released with `close({name})` — on every path"
        ));
        d.file = module.clone();
        out.push(d);
    }

    /// Check one block: every stream binding declared in it must be disposed on
    /// every path out of the REST of that block. `live` is the enclosing scopes'
    /// stream names, needed only so `let t = s` is recognised as a move.
    fn block(
        b: &Block,
        live: &mut Vec<(String, String)>,
        producers: &HashMap<&str, String>,
        module: &Option<String>,
        decl: &Declared,
        out: &mut Vec<Diagnostic>,
    ) {
        let base = live.len();
        for (i, st) in b.stmts.iter().enumerate() {
            if let Stmt::Let {
                name, ty, value, line, ..
            } = st
            {
                if let Some(rendered) = stream_let(ty.as_ref(), value, live, producers, decl) {
                    // `false`: a `break` in the rest of THIS block leaves the block
                    // that declared the stream, so it abandons it. Inside a loop
                    // nested below, a `break` only leaves that loop and control
                    // comes back here still owning it — which is what the flag
                    // distinguishes.
                    let s = scan(&b.stmts[i + 1..], name, false);
                    report(out, &s, *line, name, &rendered, module);
                    live.push((name.clone(), rendered));
                }
            }
            for sub in sub_blocks(st) {
                block(sub, live, producers, module, decl, out);
            }
        }
        live.truncate(base);
    }

    /// The rendered stream type this `let` binds, if it binds one.
    fn stream_let(
        ty: Option<&Type>,
        value: &Expr,
        live: &[(String, String)],
        producers: &HashMap<&str, String>,
        decl: &Declared,
    ) -> Option<String> {
        // The must-use row, not a `Stream` match: an alias of a must-use type
        // carries the obligation its base does.
        if let Some(t) = ty {
            if decl.must_use(t) {
                return Some(t.to_string());
            }
        }
        match value {
            Expr::Call { name, .. }
                if name == "fromArray" || name == "fromStep" || name == "fromWrap" =>
            {
                Some("Stream".to_string())
            }
            Expr::Call { name, .. } => producers.get(name.as_str()).cloned(),
            // `let t = s` moves the stream; `t` inherits both the obligation and
            // the rendering, and the mention of `s` discharges `s`'s.
            Expr::Var { name, .. } => live
                .iter()
                .find(|(l, _)| l == name)
                .map(|(_, rendered)| rendered.clone()),
            _ => None,
        }
    }

    /// `nested_loop` is whether this list is (transitively) the body of a loop
    /// *inside* the block that declared the stream. It is the whole difference
    /// between the two things `break` can mean: leaving the declaring block, which
    /// abandons the stream, and leaving a loop below it, after which control
    /// returns to the declaring block still owning it.
    fn scan(stmts: &[Stmt], name: &str, nested_loop: bool) -> Scan {
        let mut acc = Scan::default();
        for (i, st) in stmts.iter().enumerate() {
            // The one place a disposal is decided: any mention of the binding in a
            // statement's own expressions moves it (`close(s)`, `for x in s`,
            // `sink(s)`, `let t = s`). A second mention anywhere in the rest of the
            // list is then a double disposal on this path.
            let moved = match st {
                Stmt::Let { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::SetField { value, .. }
                | Stmt::Expr(value) => mentions(value, name),
                Stmt::IndexSet { index, value, .. } => {
                    mentions(index, name) || mentions(value, name)
                }
                Stmt::If { cond, .. } | Stmt::While { cond, .. } => mentions(cond, name),
                Stmt::IfLet { scrutinee, .. } => mentions(scrutinee, name),
                Stmt::ForIn { iter, .. } => mentions(iter, name),
                Stmt::Drop { name: n, .. } => n == name,
                Stmt::Return { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => false,
                Stmt::Region { .. } => false,
            };
            if moved {
                acc.disposed = true;
                acc.doubled = stmts[i + 1..].iter().any(|s| stmt_mentions(s, name));
                // The disposal settles `disposed`, but the caller's branch merge
                // still needs to know whether anything falls out of this list —
                // `if c { close(s) return 1 }` disposes AND diverges, and reading
                // it as a plain fall-through made the merge see two branches
                // disagreeing when only one of them continues.
                acc.diverges = diverges(&stmts[i + 1..]);
                return acc;
            }
            match st {
                Stmt::Return { value, .. } => {
                    // Forwarding by returning it is a disposal; returning anything
                    // else leaves the function still owning it.
                    acc.diverges = true;
                    acc.leaked |= !value.as_ref().is_some_and(|e| mentions(e, name));
                    return acc;
                }
                // Inside a loop below the declaring block, `break`/`continue` land
                // back in the declaring block still owning the stream — nothing to
                // report. At the declaring block's own level they leave it, so an
                // undisposed stream is abandoned exactly as by a bare `return`.
                Stmt::Break { .. } | Stmt::Continue { .. } => {
                    acc.diverges = true;
                    acc.leaked |= !nested_loop;
                    return acc;
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
                    let t = scan(&then_block.stmts, name, nested_loop);
                    let e = match else_block {
                        Some(b) => scan(&b.stmts, name, nested_loop),
                        None => Scan::default(),
                    };
                    acc.leaked |= t.leaked || e.leaked;
                    acc.doubled |= t.doubled || e.doubled;
                    match (t.diverges, e.diverges) {
                        (true, true) => {
                            acc.diverges = true;
                            return acc;
                        }
                        (true, false) => {
                            if e.disposed {
                                acc.disposed = true;
                                return acc;
                            }
                        }
                        (false, true) => {
                            if t.disposed {
                                acc.disposed = true;
                                return acc;
                            }
                        }
                        (false, false) => {
                            if t.disposed && e.disposed {
                                acc.disposed = true;
                                return acc;
                            }
                            // The branches DISAGREE. Whatever follows, one of the
                            // two paths is wrong: if nothing disposes later the
                            // disposing branch is the only correct one, and if
                            // something does, it double-frees on that branch. Both
                            // are the same authoring mistake, so it is reported
                            // once, here, rather than turned into a puzzle by a
                            // later statement that makes the merge look clean.
                            acc.leaked |= t.disposed != e.disposed;
                        }
                    }
                }
                // A loop body may run zero times, so a disposal inside it never
                // discharges the obligation on the fall-through — and disposing on
                // one iteration would dispose again on the next, which is the same
                // shape `check_loop_reuse` already rejects for `consume`.
                Stmt::While { body, .. } | Stmt::ForIn { body, .. } => {
                    let b = scan(&body.stmts, name, true);
                    acc.leaked |= b.leaked || b.disposed;
                    acc.doubled |= b.doubled;
                }
                Stmt::Region { body, .. } => {
                    let b = scan(&body.stmts, name, nested_loop);
                    acc.leaked |= b.leaked;
                    acc.doubled |= b.doubled;
                    if b.disposed || b.diverges {
                        acc.disposed = b.disposed;
                        acc.diverges = b.diverges;
                        return acc;
                    }
                }
                _ => {}
            }
        }
        acc
    }

    /// Whether every path out of `stmts` leaves via `return`/`break`/`continue`
    /// (or `panic`, which diverges for the same reason it does above).
    ///
    /// The same question `MoveCheck::block` answers as its return value; asked
    /// again here because [`scan`] stops at the disposal and so never reaches the
    /// `return` that follows it.
    fn diverges(stmts: &[Stmt]) -> bool {
        stmts.iter().any(|s| match s {
            Stmt::Return { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => true,
            Stmt::Expr(Expr::Call { name, .. }) => name == "panic",
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
                diverges(&then_block.stmts)
                    && else_block.as_ref().is_some_and(|b| diverges(&b.stmts))
            }
            Stmt::Region { body, .. } => diverges(&body.stmts),
            _ => false,
        })
    }

    /// The nested blocks of a statement, for the declaration walk.
    fn sub_blocks(s: &Stmt) -> Vec<&Block> {
        match s {
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
                let mut v = vec![then_block];
                v.extend(else_block.as_ref());
                v
            }
            Stmt::While { body, .. }
            | Stmt::ForIn { body, .. }
            | Stmt::Region { body, .. } => vec![body],
            _ => Vec::new(),
        }
    }

    /// Whether a whole statement (including everything nested in it) mentions the
    /// binding — the double-disposal probe.
    fn stmt_mentions(s: &Stmt, name: &str) -> bool {
        let here = match s {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::SetField { value, .. }
            | Stmt::Expr(value) => mentions(value, name),
            Stmt::IndexSet { index, value, .. } => mentions(index, name) || mentions(value, name),
            Stmt::If { cond, .. } | Stmt::While { cond, .. } => mentions(cond, name),
            Stmt::IfLet { scrutinee, .. } => mentions(scrutinee, name),
            Stmt::ForIn { iter, .. } => mentions(iter, name),
            Stmt::Return { value, .. } => value.as_ref().is_some_and(|e| mentions(e, name)),
            Stmt::Drop { name: n, .. } => n == name,
            _ => false,
        };
        here || sub_blocks(s)
            .iter()
            .any(|b| b.stmts.iter().any(|s| stmt_mentions(s, name)))
    }

    /// Whether `e` names the binding anywhere. Every mention of a stream is a
    /// move — a `Stream` has no field, no length, and no indexing — so this needs
    /// no notion of position, which is what keeps it a dozen lines.
    pub(super) fn mentions(e: &Expr, name: &str) -> bool {
        match e {
            Expr::Var { name: n, .. } => n == name,
            Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => false,
            Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
                mentions(expr, name)
            }
            Expr::Binary { lhs, rhs, .. } => mentions(lhs, name) || mentions(rhs, name),
            Expr::Call { args, .. }
            | Expr::Spawn { args, .. }
            | Expr::TryConstruct { args, .. }
            | Expr::ArrayLit { elems: args, .. } => args.iter().any(|a| mentions(a, name)),
            Expr::MapLit { entries, .. } => entries
                .iter()
                .any(|(k, v)| mentions(k, name) || mentions(v, name)),
            Expr::StructLit { fields, .. } => fields.iter().any(|(_, v)| mentions(v, name)),
            Expr::Match {
                scrutinee, arms, ..
            } => mentions(scrutinee, name) || arms.iter().any(|a| mentions(&a.body, name)),
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                mentions(cond, name)
                    || mentions(then_branch, name)
                    || else_branch.as_ref().is_some_and(|e| mentions(e, name))
            }
            Expr::Lambda { body, .. } => match body {
                LambdaBody::Expr(e) => mentions(e, name),
                LambdaBody::Block(b) => b.stmts.iter().any(|s| stmt_mentions(s, name)),
            },
        }
    }
}

/// Whether parameter `i` of the builtin `name` **stores** its argument.
///
/// A builtin has no signature to carry a capability, so the three that put a
/// value somewhere it outlives the call are listed here (RFC-0087 §2b — the
/// hand list this replaces was the producer half of the same gap). Everything
/// else a builtin does with a heap argument is a read: `print` formats it,
/// `@concat` copies out of it, `at` looks inside it.
fn sinks(name: &str, i: usize) -> bool {
    matches!((name, i), ("push", 1) | ("set", 1) | ("cell", 0))
}

/// The place `e` reads, as `(root name, whole path)`.
///
/// `s` is `("s", "s")` and `r.a.b` is `("r", "r.a.b")` — the root is what a move
/// takes, the path is what the diagnostic quotes. Anything else (a call, a
/// literal, an operator) is not a place and answers `None`: it has no earlier
/// owner, so nothing about it can be a move.
fn place_path(e: &Expr) -> Option<(String, String)> {
    match e {
        Expr::Var { name, .. } => Some((name.clone(), name.clone())),
        Expr::Field { expr, field, .. } => {
            let (root, path) = place_path(expr)?;
            Some((root, format!("{path}.{field}")))
        }
        _ => None,
    }
}

/// One diagnostic with its menu of fixes (RFC-0087 U2).
///
/// Every rule-1/2/3 error names the ways out rather than only the problem. The
/// shape is fixed — the offending line, then one `fix:` per way out — so the
/// editor, `vyrn check` and a future `vyrn fix` all read the same thing.
fn menu(line: usize, message: String, fixes: Vec<String>) -> String {
    let mut s = format!("line {line}: {message}");
    for f in fixes {
        s.push_str(&format!("\n  fix: {f}"));
    }
    s
}

/// The payload names a `match` pattern binds.
pub fn pattern_bindings(p: &Pattern) -> Vec<&str> {
    match p {
        Pattern::Some(b) | Pattern::Ok(b) | Pattern::Err(b) => vec![b],
        Pattern::Success(b) | Pattern::Failure(b) => vec![b],
        Pattern::Variant(_, binds) => binds.iter().map(|s| s.as_str()).collect(),
        Pattern::None => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn run(src: &str) -> Result<(), String> {
        let program = crate::parser::parse(crate::lexer::lex(src).unwrap()).unwrap();
        super::check(&program)
    }

    #[test]
    fn rejects_use_after_consume() {
        let src = "type T = { id: Int64 }; \
                   fn use_up(t: consume T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let x = T { id: 1 }; let a = use_up(x); return use_up(x); }";
        let e = run(src).unwrap_err();
        assert!(e.contains("already consumed"), "{e}");
    }

    #[test]
    fn rejects_use_after_consume_inside_a_test_body() {
        // RFC-0015: a test body is move-checked exactly like a function body.
        let src = "type T = { id: Int64 }; \
                   fn use_up(t: consume T) -> Int64 { return t.id; } \
                   test \"consumes twice\" { let x = T { id: 1 }; \
                       let a = use_up(x); let b = use_up(x); assert(a == b) }";
        let e = run(src).unwrap_err();
        assert!(e.contains("already consumed"), "{e}");
    }

    #[test]
    fn rejects_smallarray_use_after_drop() {
        // RFC-0056: a moved-from `SmallArray` is dead (move copies the whole
        // struct incl. inline slots, but movecheck semantics are unchanged) —
        // using it after `drop` is rejected, exactly like any owned value.
        let src = "fn main() -> Int64 { \
                   let mut xs: SmallArray<Int64, 4> = []  xs.push(1)  \
                   drop xs  return xs.length }";
        let e = run(src).unwrap_err();
        assert!(e.contains("consumed") || e.contains("drop"), "{e}");
    }

    #[test]
    fn allows_read_reuse() {
        let src = "type T = { id: Int64 }; \
                   fn peek(t: read T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let x = T { id: 1 }; return peek(x) + peek(x); }";
        assert!(run(src).is_ok());
    }

    #[test]
    fn consume_then_no_reuse_is_ok() {
        let src = "type T = { id: Int64 }; \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let x = T { id: 1 }; return take(x); }";
        assert!(run(src).is_ok());
    }

    #[test]
    fn reassignment_revives() {
        let src = "type T = { id: Int64 }; \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let mut x = T { id: 1 }; let a = take(x); \
                                      x = T { id: 2 }; return a + take(x); }";
        assert!(run(src).is_ok());
    }

    #[test]
    fn rejects_consume_in_while_condition() {
        // The condition re-runs every iteration — consuming there is the same
        // bug as consuming in the body.
        let src = "type T = { id: Int64 }; \
                   fn take(t: consume T) -> Bool { return t.id > 0; } \
                   fn main() -> Int64 { let x = T { id: 1 }; \
                                      while take(x) { let y = 1; } return 0; }";
        let e = run(src).unwrap_err();
        assert!(e.contains("inside a loop"), "{e}");
    }

    #[test]
    fn spawn_applies_consume_capabilities() {
        // `spawn take(x)` moves x across the task boundary; a second use is a
        // double move.
        let src = "type T = { id: Int64 }; \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let x = T { id: 1 }; \
                                      let t = spawn take(x); \
                                      let z = take(x); return t.join() + z; }";
        let e = run(src).unwrap_err();
        assert!(e.contains("already consumed by `spawn take(..)`"), "{e}");
    }

    #[test]
    fn rejects_passing_global_to_consume_param() {
        // RFC-0013: nothing may take ownership of module state.
        let src = "type T = { id: Int64 } \
                   let g = T { id: 1 } \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn use_it() -> Int64 { return take(g); } \
                   fn main() -> Int64 { return 0; }";
        let e = run(src).unwrap_err();
        assert!(e.contains("module state") && e.contains("consume"), "{e}");
    }

    #[test]
    fn local_shadowing_global_may_be_consumed() {
        // A local `g` shadows the global, so consuming it is fine.
        let src = "type T = { id: Int64 } \
                   let g = T { id: 1 } \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn use_it() -> Int64 { let g = T { id: 2 } return take(g); } \
                   fn main() -> Int64 { return 0; }";
        assert!(run(src).is_ok(), "{:?}", run(src));
    }

    #[test]
    fn break_path_consume_rejected_after_loop() {
        // Consumed on the way out of the loop, then used after it — rejected.
        let src = "type T = { id: Int64 }; \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let x = T { id: 1 }; \
                       for i in [0, 1] { let a = take(x); break } \
                       return take(x); }";
        let e = run(src).unwrap_err();
        assert!(e.contains("already consumed"), "{e}");
    }

    #[test]
    fn consume_on_break_branch_not_moved_on_fall_through() {
        // `x` is consumed only on the branch that breaks; the fall-through path
        // never consumed it, so a later read in the same body is fine (RFC-0060).
        let src = "type T = { id: Int64 }; \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn peek(t: read T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let x = T { id: 1 }; let mut s = 0; \
                       for i in [0, 1, 2] { \
                           if i == 2 { let a = take(x); break } \
                           s = s + peek(x) } \
                       return s; }";
        assert!(run(src).is_ok(), "{:?}", run(src));
    }

    #[test]
    fn use_after_break_is_unreachable_clean() {
        // The second `take(x)` is after an unconditional `break` — unreachable, so
        // it is not a use-after-consume (RFC-0060: code after break is dead).
        let src = "type T = { id: Int64 }; \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let x = T { id: 1 }; \
                       while true { break let a = take(x); let b = take(x); } \
                       return 0; }";
        assert!(run(src).is_ok(), "{:?}", run(src));
    }

    // ---- RFC-0089 Phase 4b: rules 1 and 3 --------------------------------

    #[test]
    fn a_store_of_an_owned_place_moves_it() {
        // Rule 1. The binding takes the String; reading `s` afterward is the
        // error, and the message prints both lines and the way out.
        let src = "fn main() -> Int64 { let s = \"a\" + \"b\" let t = s \
                   return s.byteLength + t.byteLength }";
        let e = run(src).unwrap_err();
        assert!(e.contains("`s` was moved here into the binding `t`"), "{e}");
        assert!(e.contains("... and `s` is used again here"), "{e}");
        assert!(e.contains("fix: `s.copy()`"), "{e}");
    }

    #[test]
    fn a_last_use_may_move() {
        // The half that keeps rule 1 usable: `let t = s` with no later `s` is
        // not an error, so the common rename costs nothing.
        assert!(run("fn main() -> Int64 { let s = \"a\" + \"b\" let t = s \
                     return t.byteLength }")
            .is_ok());
        // And a scalar never moves at all.
        assert!(run("fn main() -> Int64 { let a = 1 let b = a return a + b }").is_ok());
    }

    #[test]
    fn a_move_into_a_container_is_a_move() {
        // `push` stores its argument, so it is the `consume` parameter a builtin
        // has no signature to declare.
        let src = "fn main() -> Int64 { let s = \"a\" + \"b\" let mut xs: Array<String> = [] \
                   xs.push(s) return s.byteLength }";
        let e = run(src).unwrap_err();
        assert!(e.contains("was moved here into `push(..)`"), "{e}");
        // A record literal takes its operands the same way.
        let src = "type R = { s: String } \
                   fn main() -> Int64 { let s = \"a\" + \"b\" let r = R { s: s } \
                   return s.byteLength + r.s.byteLength }";
        let e = run(src).unwrap_err();
        assert!(e.contains("was moved here into the field `R.s`"), "{e}");
    }

    #[test]
    fn a_borrowed_parameter_may_not_be_returned() {
        // Rule 3, and the 36 corpus sites Phase 1's gate counted.
        let src = "fn id(s: String) -> String { return s } fn main() -> Int64 { return 0 }";
        let e = run(src).unwrap_err();
        assert!(e.contains("`s` may not be returned"), "{e}");
        assert!(e.contains("fix: declare the parameter `consume s`"), "{e}");
        assert!(e.contains("fix: `s.copy()`"), "{e}");
        // Both named fixes work.
        assert!(run("fn id(s: consume String) -> String { return s } \
                     fn main() -> Int64 { return 0 }")
            .is_ok());
        assert!(run("fn id(s: String) -> String { return s.copy() } \
                     fn main() -> Int64 { return 0 }")
            .is_ok());
    }

    #[test]
    fn a_returned_scalar_parameter_is_not_a_borrow() {
        // The surface only exists where heap is owned (RFC-0089 "What it costs").
        assert!(run("fn id(n: Int64) -> Int64 { return n } fn main() -> Int64 { return id(1) }")
            .is_ok());
    }

    #[test]
    fn a_loop_variable_is_a_read_borrow() {
        // The PLAN's decision log: iteration binds a `read` borrow, so the
        // element belongs to the container and returning one is rule 3.
        let src = "fn first(xs: Array<String>) -> String { for x in xs { return x } return \"\" } \
                   fn main() -> Int64 { return 0 }";
        let e = run(src).unwrap_err();
        assert!(e.contains("`x` may not be returned") && e.contains("a loop variable"), "{e}");
        assert!(run("fn first(xs: Array<String>) -> String { for x in xs { return x.copy() } \
                     return \"\" } fn main() -> Int64 { return 0 }")
            .is_ok());
    }

    #[test]
    fn a_field_read_does_not_take_the_record() {
        // A place owns its contents (rule 4), so `r.s` is a projection: naming it
        // locally is free, and returning that name is still rule 3.
        let src = "type R = { s: String } \
                   fn get(r: R) -> String { let t = r.s return t } \
                   fn main() -> Int64 { return 0 }";
        let e = run(src).unwrap_err();
        assert!(e.contains("`t` may not be returned"), "{e}");
        // Reading two fields out of one record is not two moves of the record.
        assert!(run("type R = { a: String, b: String } \
                     fn use2(r: R) -> Int64 { let x = r.a let y = r.b \
                     return x.byteLength + y.byteLength } fn main() -> Int64 { return 0 }")
            .is_ok());
    }

    #[test]
    fn rule_2_refuses_a_stored_borrow_when_it_is_turned_on() {
        // The half of rule 2 that is written and waiting (RFC-0089 "What rule 2
        // costs"). It is off on every ordinary check, so this asks for it by
        // name — a rule nothing exercises is a rule that rots.
        let src = "type R = { s: String } \
                   fn keep(x: String) -> R { return R { s: x } } \
                   fn main() -> Int64 { return 0 }";
        let p = crate::parser::parse(crate::lexer::lex(src).unwrap()).unwrap();
        assert!(super::check_accum(&p).is_empty(), "off by default");
        let d = borrow_store_sites(&p);
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].message.contains("may not be stored into the field `R.s`"), "{d:?}");
        assert!(d[0].message.contains("fix: declare the parameter `consume x`"), "{d:?}");
    }

    #[test]
    fn a_modify_borrow_is_exclusive() {
        let src = "fn f(a: modify Array<Int64>, b: Array<Int64>) -> Int64 { return a.length } \
                   fn main() -> Int64 { let mut xs: Array<Int64> = [] return f(xs, xs) }";
        let e = run(src).unwrap_err();
        assert!(e.contains("as `modify` and read again in the same call"), "{e}");
        assert!(e.contains("fix: `xs.copy()`"), "{e}");
    }

    #[test]
    fn an_escaping_closure_may_not_capture_a_borrow() {
        // A lambda written at a call site does not outlive the call, so it
        // borrows freely; one that is stored is a value and may not.
        assert!(run("fn apply(f: fn(Int64) -> Int64) -> Int64 { return f(1) } \
                     fn go(s: String) -> Int64 { return apply(|n| n + s.byteLength) } \
                     fn main() -> Int64 { return 0 }")
            .is_ok());
        let src = "fn go(s: String) -> Int64 { let f = |n| n + s.byteLength return f(1) } \
                   fn main() -> Int64 { return 0 }";
        let e = run(src).unwrap_err();
        assert!(e.contains("may not be captured by a closure that outlives this call"), "{e}");
    }

    // ---- RFC-0075: the disposal obligation -------------------------------

    /// The producer every stream case below acquires from.
    const FEED: &str = "fn feed() -> Stream<Int64> { let xs: Array<Int64> = [1, 2] \
                        return fromArray(xs) } ";

    fn stream(body: &str) -> Result<(), String> {
        run(&format!("{FEED} fn main() -> Int64 {{ {body} }}"))
    }

    #[test]
    fn an_abandoned_stream_does_not_build() {
        // The milestone's whole claim: the `#6193` shape is a compile error.
        let e = stream("let events = feed() return 0").unwrap_err();
        assert!(e.contains("`events` is a `Stream<Int64>` and is never disposed"), "{e}");
    }

    #[test]
    fn a_stepped_producer_carries_the_same_obligation() {
        // RFC-0075 M2b's producer is a second builtin, and this pass keys on the
        // NAME — so an abandoned `fromStep` result had to be added here or the
        // one stream the language cannot materialise would be the one it lets
        // leak. Its endlessness is the checker's business, not this pass's: the
        // obligation is the same obligation.
        let src = "fn tick(c: Ref<Int64>) -> Option<Int64> { let n = get(c) set(c, n + 1) \
                   return Some(n) } \
                   fn main() -> Int64 { let s = fromStep(0, tick) return 0 }";
        let e = run(src).unwrap_err();
        assert!(e.contains("`s` is a `Stream` and is never disposed"), "{e}");
        let src = "fn tick(c: Ref<Int64>) -> Option<Int64> { let n = get(c) set(c, n + 1) \
                   return Some(n) } \
                   fn main() -> Int64 { let s = fromStep(0, tick) close(s) return 0 }";
        assert!(run(src).is_ok());
    }

    #[test]
    fn a_wrapper_carries_the_obligation_and_swallows_its_source() {
        // RFC-0075 M2c's producer is a third builtin, and this pass keys on the
        // NAME. Both halves matter: an abandoned wrapper is a leak, and the
        // source it wrapped is DISCHARGED by being passed to it — releasing that
        // source as well would be the double free the walk in `close` would then
        // complete.
        let src = "fn tick(c: Ref<Int64>) -> Option<Int64> { let n = get(c) set(c, n + 1)                    return Some(n) }                    fn step(c: Ref<Int64>) -> Option<Int64> { let x: Option<Int64> = pull(c)                    return x }                    fn main() -> Int64 { let s = fromStep(0, tick)                    let w = fromWrap(s, step) return 0 }";
        let e = run(src).unwrap_err();
        assert!(e.contains("`w` is a `Stream` and is never disposed"), "{e}");
        let src = "fn tick(c: Ref<Int64>) -> Option<Int64> { let n = get(c) set(c, n + 1)                    return Some(n) }                    fn step(c: Ref<Int64>) -> Option<Int64> { let x: Option<Int64> = pull(c)                    return x }                    fn main() -> Int64 { let s = fromStep(0, tick)                    let w = fromWrap(s, step) close(w) return 0 }";
        assert!(run(src).is_ok());
    }

    #[test]
    fn the_three_discharges_are_accepted() {
        assert!(stream("for p in feed() { print(p) } return 0").is_ok());
        assert!(stream("let s = feed() close(s) return 0").is_ok());
        assert!(run(&format!(
            "{FEED} fn fwd() -> Stream<Int64> {{ let s = feed() return s }} \
             fn main() -> Int64 {{ close(fwd()) return 0 }}"
        ))
        .is_ok());
    }

    #[test]
    fn a_stream_must_be_disposed_on_every_path() {
        // Disposing on one branch only is the tRPC pathology in miniature: the
        // cleanup exists, and there is a path that skips it.
        let e = stream("let s = feed() if true { close(s) } return 0").unwrap_err();
        assert!(e.contains("never disposed"), "{e}");
        let e = stream("let s = feed() if true { return 1 } close(s) return 0").unwrap_err();
        assert!(e.contains("never disposed"), "{e}");
        // Both branches, or a branch that leaves, are fine.
        assert!(stream("let s = feed() if true { close(s) } else { close(s) } return 0").is_ok());
        assert!(stream("let s = feed() if true { close(s) return 1 } close(s) return 0").is_ok());
    }

    #[test]
    fn breaking_out_of_the_declaring_block_abandons_it() {
        // `break` means two different things depending on which side of the
        // declaring block the loop it leaves is on.
        let e = stream("for i in [0, 1] { let s = feed() break } return 0").unwrap_err();
        assert!(e.contains("never disposed"), "{e}");
        // Here the loop is BELOW the declaration, so control comes back owning it.
        assert!(stream("let s = feed() for i in [0, 1] { break } close(s) return 0").is_ok());
    }

    #[test]
    fn a_stream_may_not_be_disposed_twice() {
        // The direction the leak check does not cover, and the worse bug of the
        // two: `close` frees the buffer, so a second one is a double free.
        let e = stream("let s = feed() close(s) close(s) return 0").unwrap_err();
        assert!(e.contains("disposed more than once"), "{e}");
        let e = stream("let s = feed() for p in s { print(p) } close(s) return 0").unwrap_err();
        assert!(e.contains("disposed more than once"), "{e}");
    }

    #[test]
    fn aliasing_moves_the_obligation_rather_than_dropping_it() {
        let e = stream("let s = feed() let t = s return 0").unwrap_err();
        assert!(e.contains("`t` is a `Stream<Int64>`"), "{e}");
        assert!(stream("let s = feed() let t = s close(t) return 0").is_ok());
    }

    #[test]
    fn a_stream_parameter_carries_the_obligation_into_the_callee() {
        // Without this, `fn sink(s: Stream<Int64>) {}` is a one-line hole through
        // the whole analysis: the caller discharges by moving, and nobody else has
        // to do anything.
        let e = run(&format!(
            "{FEED} fn sink(s: Stream<Int64>) -> Int64 {{ return 0 }} \
             fn main() -> Int64 {{ return sink(feed()) }}"
        ))
        .unwrap_err();
        assert!(e.contains("`s` is a `Stream<Int64>` and is never disposed"), "{e}");
    }

    // ---- RFC-0075 M2: the obligation through a combinator ----------------

    /// A combinator, spelled locally rather than imported: nothing in the
    /// compiler knows about std/stream, and the point is that nothing has to.
    const TWICE: &str = "fn twice(s: Stream<Int64>) -> Stream<Int64> { \
                         let mut out: Array<Int64> = [] for x in s { out.push(x * 2) } \
                         return fromArray(out) } ";

    #[test]
    fn a_combinator_neither_swallows_the_obligation_nor_launders_it() {
        // The hole that only opens once combinators exist, in both directions.
        // M1's two rules already close it — a `Stream` parameter carries the
        // obligation in, a `Stream` return hands one back — so this pins that
        // they compose rather than adding a rule about combinators.

        // The result is owed exactly as `fromArray`'s is.
        let e = run(&format!(
            "{FEED}{TWICE} fn main() -> Int64 {{ let m = twice(feed()) return 0 }}"
        ))
        .unwrap_err();
        assert!(e.contains("`m` is a `Stream<Int64>` and is never disposed"), "{e}");

        // A combinator that drops its argument on the floor does not build.
        let e = run(&format!(
            "{FEED} fn sink(s: Stream<Int64>) -> Stream<Int64> {{ return feed() }} \
             fn main() -> Int64 {{ close(sink(feed())) return 0 }}"
        ))
        .unwrap_err();
        assert!(e.contains("`s` is a `Stream<Int64>` and is never disposed"), "{e}");

        // Consumed and then closed is still the double free.
        let e = run(&format!(
            "{FEED}{TWICE} fn main() -> Int64 {{ let m = twice(feed()) \
             for v in m {{ print(v) }} close(m) return 0 }}"
        ))
        .unwrap_err();
        assert!(e.contains("disposed more than once"), "{e}");

        // A discharged chain is accepted, including the intermediate that never
        // gets a name.
        assert!(run(&format!(
            "{FEED}{TWICE} fn main() -> Int64 {{ for v in twice(twice(feed())) \
             {{ print(v) }} return 0 }}"
        ))
        .is_ok());
    }

    #[test]
    fn a_generic_producer_is_quoted_as_plain_stream() {
        // `Stream<U>` at a call site names a type parameter the program never
        // wrote. This pass has no types, so it under-specifies instead — the
        // same `Stream` it has always used for `fromArray`.
        let e = run("fn mk<T>(xs: Array<T>) -> Stream<T> { return fromArray(xs) } \
                     fn main() -> Int64 { let s = mk([1, 2]) return 0 }")
            .unwrap_err();
        assert!(e.contains("`s` is a `Stream` and is never disposed"), "{e}");
    }

    // ---- RFC-0089 Phase 4a: the site census ------------------------------

    fn sites_of(src: &str) -> Vec<OwningSite> {
        let p = crate::parser::parse(crate::lexer::lex(src).unwrap()).unwrap();
        owning_sites(&p)
    }

    fn kinds(src: &str, kind: &str) -> Vec<String> {
        sites_of(src)
            .into_iter()
            .filter(|s| s.kind == kind)
            .map(|s| s.ty)
            .collect()
    }

    #[test]
    fn a_binding_of_an_owning_value_is_a_site() {
        // The annotation answers where the initializer cannot: `[]` is three
        // shapes, and only `Array<Int64>` says which.
        let src = "fn main() -> Int64 { let a: Array<Int64> = [] return 0 }";
        assert_eq!(kinds(src, "bind"), vec!["Array<Int64>"]);
        // A scalar is not a site at all, and neither is a literal.
        assert!(sites_of("fn main() -> Int64 { let i = 1 return i }").is_empty());
    }

    #[test]
    fn an_argument_a_return_and_a_store_are_sites() {
        let src = "type R = { s: String } \
                   fn take(s: String) -> String { return s } \
                   fn main() -> Int64 { let mut r = R { s: \"\" } let t = take(\"a\" + \"b\") \
                   r.s = t return 0 }";
        assert_eq!(kinds(src, "arg"), vec!["String"]);
        assert_eq!(kinds(src, "return"), vec!["String"]);
        assert_eq!(kinds(src, "field"), vec!["String"]);
    }

    #[test]
    fn a_loop_variable_takes_its_element_type() {
        // Without `elem_of` every loop variable is unknown, and so is everything
        // read out of one after it.
        let src = "fn main() -> Int64 { let xs: Array<String> = [] \
                   for x in xs { print(x) } return 0 }";
        assert_eq!(kinds(&src.to_string(), "iterate"), vec!["Array<String>"]);
        assert_eq!(kinds(src, "arg"), vec!["String"]);
    }

    #[test]
    fn a_capture_is_a_site_and_a_parameter_is_not() {
        // The lambda's own parameter resolves in the lambda's frame; `s` does not.
        let src = "fn apply(f: fn(String) -> Int64, x: String) -> Int64 { return f(x) } \
                   fn main() -> Int64 { let s = \"a\" + \"b\" \
                   return apply(|p| p.byteLength + s.byteLength, \"c\") }";
        assert_eq!(kinds(src, "capture"), vec!["String"]);
    }

    #[test]
    fn a_pattern_payload_takes_the_scrutinee_s_type() {
        // 4a recorded this binder as UNKNOWN and left 4b to decide what an
        // unknown means. 4b names it instead: an `Option<String>` matched by
        // `Some(v)` binds a `String`, so the site is typed rather than guessed
        // at. The widening is movecheck-only — `own.rs` decides `free` with the
        // reading it had.
        let src = "fn main() -> Int64 { let o: Option<String> = None \
                   match o { Some(v) => print(v), None => print(\"\") } return 0 }";
        let sites = sites_of(src);
        assert!(sites.iter().all(|s| !s.unknown()), "{sites:?}");
        assert_eq!(kinds(src, "arg"), vec!["String"]);
    }

    /// RFC-0089 Phase 4a's deliverable: how many places Phase 4b's analysis has
    /// to be **correct** at, over the whole corpus.
    ///
    /// It parses each file ALONE — no loader, no linking — for the same reason
    /// the M0 gate does: one number per source line rather than one per import
    /// graph. A cross-module call's return type is therefore unknown here, which
    /// is why the unknown column is an upper bound.
    ///
    /// Ignored by default: it reads the repository, so it is a measurement, not
    /// a unit test. Run it with
    /// `cargo test -p vyrn-frontend movecheck::tests::rfc0089 -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn rfc0089_owning_sites_over_the_corpus() {
        let mut files = Vec::new();
        crate::own::tests::sources("examples", &mut files);
        crate::own::tests::sources("std", &mut files);
        files.sort();

        let mut per_file: Vec<(String, usize, usize, usize)> = Vec::new();
        let mut by_kind: BTreeMap<&'static str, [usize; 4]> = BTreeMap::new();
        let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
        let (mut total, mut places, mut unknowns, mut parsed) = (0, 0, 0, 0);
        // An unknown that reads a NAMED PLACE is the dangerous cell: it is where
        // a move can be missed. An unknown that is a call result is mostly this
        // measurement's own artifact — a file parsed alone cannot see an imported
        // function's return type.
        let mut unknown_places = 0;

        for path in &files {
            let Ok(src) = std::fs::read_to_string(path) else { continue };
            let Ok(tokens) = crate::lexer::lex(&src) else { continue };
            let (program, errs) = crate::parser::parse_accum(tokens);
            if !errs.is_empty() {
                continue;
            }
            parsed += 1;
            let sites = owning_sites(&program);
            let (mut p, mut u) = (0, 0);
            for s in &sites {
                let row = by_kind.entry(s.kind).or_default();
                row[0] += 1;
                if s.place {
                    row[1] += 1;
                    p += 1;
                }
                if s.unknown() {
                    row[2] += 1;
                    u += 1;
                    if s.place {
                        row[3] += 1;
                        unknown_places += 1;
                    }
                } else {
                    *by_type.entry(s.ty.clone()).or_default() += 1;
                }
            }
            total += sites.len();
            places += p;
            unknowns += u;
            if !sites.is_empty() {
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                per_file.push((name, sites.len(), p, u));
            }
        }

        per_file.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        println!("corpus: {} files ({parsed} parsed)", files.len());
        println!("sites: {total} — {places} read a named place, {unknowns} of unknown type");
        println!("unknown AND a named place: {unknown_places}");
        println!("by kind (kind: total, place, unknown, unknown+place)");
        for (k, r) in &by_kind {
            println!("  {k:>14}: {:>5} {:>5} {:>5} {:>5}", r[0], r[1], r[2], r[3]);
        }
        println!("by type");
        let mut types: Vec<_> = by_type.into_iter().collect();
        types.sort_by(|a, b| b.1.cmp(&a.1));
        for (t, c) in types.iter().take(20) {
            println!("  {c:>5}  {t}");
        }
        println!("per file (file: total, place, unknown)");
        for (f, t, p, u) in &per_file {
            println!("  {t:>5} {p:>5} {u:>5}  {f}");
        }
    }

    #[test]
    fn rejects_consume_in_loop() {
        let src = "type T = { id: Int64 }; \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let x = T { id: 1 }; let mut i = 0; \
                                      while i < 3 { let a = take(x); i = i + 1; } return 0; }";
        let e = run(src).unwrap_err();
        assert!(e.contains("inside a loop"), "{e}");
    }
}
