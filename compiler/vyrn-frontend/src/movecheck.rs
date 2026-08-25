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
//! **It enforces RFC-0089 rules 1–3** (Phase 4b, completed by 4b-2). Three
//! families of error, on top of the `consume` family above:
//!
//! - **Rule 1 — a value moves.** A store of a value that transitively owns heap
//!   (a `let`, an assignment, a field or element store, a literal operand)
//!   takes the source place. The analysis is *last-use aware*: `let t = s` with
//!   no later use of `s` is legal, and the error only fires on the later use.
//!   That is the machinery `consume` already had; rule 1 adds the stores.
//! - **Rule 2 — a borrow is second-class.** A `read`/`modify`/`share` parameter,
//!   a `for` variable over a container the loop does not own, and a local bound
//!   to a field or element read are all [`Borrow`]s. They may be observed and
//!   passed on, but not stored, not captured by an escaping closure, and not
//!   returned. A loop owns its elements in two cases: `for x in consume xs`
//!   takes the container, and `for x in f()` iterates a temporary nobody else
//!   holds.
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
use crate::own::DropKind;

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
    run(program, Want::Sites).sites
}

/// One place RFC-0092's rule reaches: a **projection** — a field, an element or
/// a pattern binder over a place — read out and put somewhere the frame does not
/// end with.
///
/// M0 recorded these and refused nothing; the count was the gate on M1. **M1
/// refuses them, and this stays as the regression guard**: the two places that
/// refuse are still the two places that record, so a site that reappears in the
/// corpus is counted rather than argued about. A site whose type owns no heap
/// costs nothing, so [`ProjectionSite::owns_heap`] separates the bill from the
/// noise.
#[derive(Clone, Debug)]
pub struct ProjectionSite {
    /// `store` (the value put into a field, an element, a literal, module state
    /// or a `push`) or `return`.
    pub kind: &'static str,
    /// The module the site is in, or `None` for the linked root.
    pub module: Option<String>,
    /// The function it is in.
    pub func: String,
    pub line: usize,
    /// The projection as written: `d.title`, `@at(xs, i)`.
    pub path: String,
    /// The destination in words, for a store; the return type, for a return.
    pub into: String,
    /// The type, or `?` where even a linked reading cannot name it.
    pub ty: String,
    /// Whether that type transitively owns heap. A scalar carries no obligation
    /// and the rule does not reach it.
    pub owns_heap: bool,
}

/// Every [`ProjectionSite`] in `program` (RFC-0092).
///
/// The same walk [`check_accum`] runs, with recording on. Since M1 the recorded
/// sites are also refused, so over a corpus that compiles this answers empty.
/// Pass a **linked** program: a file read alone cannot name an imported type, so
/// every cross-module result reads as `?` — the error Phase 4b's per-file
/// measurement made, by 81 sites.
pub fn projection_sites(program: &Program) -> Vec<ProjectionSite> {
    run(program, Want::Projections).projections
}

/// Why a `let` binding does **not** hold its value at the end of its block.
///
/// This is rule 1 read backwards. The pass already decides, at every store,
/// whether a place hands its value over; recording the answer costs one map
/// insert and turns the rules into the reclamation rule (RFC-0089 rule 4,
/// Phase 4c). Nothing here is a second opinion — every row is written by the
/// same code that writes the diagnostic.
#[derive(Clone, Debug)]
pub enum Gone {
    /// The binding names a value somebody else owns: a projection of a place, a
    /// borrowed parameter passed on, module state, or a builtin view. Carries
    /// what it is, in words.
    Borrowed(&'static str),
    /// A store took it. `by` is the destination, in the words the diagnostic
    /// uses ("the binding `t`", "`push(..)`").
    Moved { line: usize, by: String },
    /// A `return` carried it out of the function.
    Returned { line: usize },
    /// `drop name` reclaims it, so the automatic path must not.
    Dropped { line: usize },
    /// A lambda or a `spawn` holds it, and either can outlive this block.
    Captured { line: usize },
    /// A second name reads it without taking it: `let d = c` on a type rule 1
    /// leaves alone. `Ref<T>` was the built-in case and is deleted (RFC-0090 M4);
    /// what reaches here now is a type that DECLARES `impl Owned` and holds no
    /// heap of its own — census U4's shape. Neither name may be released, because
    /// neither of them is the owner.
    Aliased { line: usize },
    /// It was handed to a declared function, which may keep it.
    Lent { line: usize, to: String },
    /// A `consume` took one or more of its PLACES (RFC-0093 M1), so the value
    /// has holes in it. Carries every place taken, RELATIVE to the binding
    /// (`title`, `head.err`), and the line of the first take.
    ///
    /// It is a SET, not one path: `std/vyx.vyrn:1431` drains nine fields out of
    /// one record. RFC-0093 M2 carries the set to [`crate::own`], which hands it
    /// to the release walk so the walk skips exactly these places and reclaims
    /// the rest.
    ///
    /// `skippable` is false where the walk may not be told to skip. A write to
    /// a place of this binding is the case: it revives the hole, and a store
    /// into an owning place releases what the place held — which is the buffer
    /// the take gave away. Then nothing may be skipped and nothing may be
    /// released, so the whole binding leaks, exactly as M1 shipped it. A leak is
    /// a task; a double free is a bug in a language that promises memory safety.
    Hole {
        line: usize,
        paths: Vec<String>,
        skippable: bool,
    },
}

/// One `let` binding, as the rules see it: its type and what became of it.
#[derive(Clone, Debug, Default)]
pub struct LetOwnership {
    /// The binding's type — its annotation, else what the declared-types
    /// reading makes of the initializer. `None` where neither names one.
    pub ty: Option<Type>,
    /// `None` means the binding still owns its value where the block ends.
    pub gone: Option<Gone>,
    /// The callee, when the initializer is a plain call. Read after the walk, to
    /// answer whether that callee lends its result rather than transferring it.
    pub from_call: Option<String>,
    /// Every `(callee, argument index, line)` this binding was handed to. Read
    /// after the walk: a position that KEEPS what it is given means this block
    /// must not release the value.
    pub passed: Vec<(String, usize, usize)>,
}

/// One call-argument position whose argument expression BUILT the value it
/// hands over — the census's shape A (an owning call's result) and shape B (an
/// allocating String expression), in `rfcs/census-call-arguments.md` §1.
///
/// The value has no name, so [`crate::own`] — which keys every release on a
/// `let` — has nothing to write a row against. This is that row: the key is the
/// ARGUMENT's node address, the way a block-exit release is keyed by the
/// `Stmt::Let`'s.
#[derive(Clone, Debug)]
pub struct ArgTemp {
    /// The argument expression's node address, in the AST the backend lowers.
    pub id: usize,
    /// The name the call site carries.
    pub callee: String,
    /// Which parameter it fills.
    pub ix: usize,
    pub line: usize,
    /// The module the body lives in, `None` for the root's own file — the
    /// stamping an error and a projection site already carry.
    pub module: Option<String>,
    /// The call that BUILT the value, or `None` where a String `+` did — which
    /// is the census's shape B and the one RFC-0096 M3 already frees at four
    /// consumers.
    pub producer: Option<String>,
    /// How a value of this type is reclaimed. Every recorded site has one — a
    /// type that releases nothing is not recorded at all.
    pub kind: DropKind,
    /// What the callee does with it, decided after the walk: the retention set
    /// is only closed over the call graph when every body has been read.
    pub verdict: ArgVerdict,
}

/// What the callee at a call-argument position does with the temporary it is
/// given. Only [`ArgVerdict::Released`] frees, and the other five are the
/// census's own classification (`rfcs/census-call-arguments.md` §3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArgVerdict {
    /// A `read` parameter that keeps nothing. **The caller releases the
    /// temporary after the call** — rules 2 and 3 refuse every way the callee
    /// could have kept it, except the constructor below.
    Released,
    /// A `consume` parameter: the callee owns it now, so the caller must not.
    Transferred,
    /// The callee keeps it — a variant constructor, or a position
    /// [`MoveCheck::note_retention`] recorded. A leak, and not this rule's.
    Retained,
    /// The result points into the argument, or the argument's own producer
    /// handed back storage it does not own. Either way somebody else owns it.
    Lent,
    /// No signature is visible, so nothing is proved and nothing is freed.
    Unknown,
    /// RFC-0096 M3's consumer rule already frees this operand at this consumer.
    /// Recorded so the two rules cannot both fire on one value.
    AlreadyFreed,
}

/// Everything one `Want::Lets` walk answers. Two facts out of one walk, because
/// [`crate::own::analyze`] needs both and the walk is not cheap.
pub struct Facts {
    /// What every `let` owns at the end of its block — see [`ownership`].
    pub lets: HashMap<usize, LetOwnership>,
    /// Every call-argument temporary, with the callee's verdict on it.
    pub arg_temps: Vec<ArgTemp>,
    /// The per-binding write/take event stream, in walk order (RFC-0114 M2).
    /// `own::analyze` folds it into the store-ownedness set; nothing else
    /// reads it.
    pub store_events: Vec<StoreEv>,
    /// Assigns to MODULE STATE, which are always owned: a global owns what it
    /// holds for the whole module and nothing may `consume` it, so every store
    /// into one releases what it replaces (Phase 5's rule, now stated as data).
    pub global_stores: std::collections::HashSet<usize>,
    /// RFC-0114 Rule N: the asymmetric-consumption joins (see [`EdgeRel`]).
    pub edge_releases: Vec<EdgeRel>,
}

/// One ownership-relevant event on one binding (RFC-0114 M2).
///
/// `key` is the binding's `Stmt::Let` address — the same key everything else
/// here uses. `loops` is the stack of loop ids the event sits inside, because
/// a back edge makes walk order meaningless between two events that share a
/// loop, and the fold refuses rather than guesses there.
pub struct StoreEv {
    pub key: usize,
    pub order: u32,
    pub loops: Vec<u32>,
    pub kind: EvKind,
}

/// RFC-0114 Rule N: an `if` consumed a binding on exactly one branch, both
/// branches continue to the join, and the other branch still holds the value —
/// so a release belongs ON THE EDGE that did not consume. `edge` names the
/// edge that releases: for an `if`, 0 is the then-edge and 1 the else-edge;
/// for a `match`, the arm's source index. `own::analyze` folds these against
/// the write events (every write must be owning) before either backend sees
/// one.
pub struct EdgeRel {
    pub if_key: usize,
    pub key: usize,
    pub name: String,
    pub edge: u32,
}

pub enum EvKind {
    /// A write into the binding: its `let` initializer (`id` 0) or an assign
    /// (`id` = the `Stmt::Assign` address). `owning` is false when the value
    /// is a projection of a place — the binding then HOLDS a borrow, and the
    /// next store over it must not release.
    Write { id: usize, owning: bool },
    /// The value left or was compromised: moved, dropped, returned, captured,
    /// lent, or a field taken out (a hole). One kind, because the fold only
    /// asks "may the value still be released", and every answer here is no.
    Take,
}

/// What every `let` in `program` owns at the end of its block, keyed by the
/// `Stmt::Let` node address — the same key [`crate::own`] emits drops with.
///
/// The addresses are the ones in `program`, so the caller must pass the very
/// AST the backend lowers. Nothing is cloned on the way through, and a test or
/// bench body is walked in place for exactly that reason.
pub fn ownership(program: &Program) -> HashMap<usize, LetOwnership> {
    facts(program).lets
}

/// [`ownership`] and the call-argument rows, out of one walk.
pub fn facts(program: &Program) -> Facts {
    let r = run(program, Want::Lets);
    let arg_temps = r.arg_temps;
    let mut lets = r.lets;
    // A call to a lender hands back storage the callee does not own, so the
    // binding that names it may not be released. Applied here rather than at the
    // `let`, because a lender is only known once every body has been read.
    for row in lets.values_mut() {
        // A HOLE row is not a decision yet — it says the binding is reclaimed
        // minus a few places (RFC-0093 M2), so the two rules below still apply
        // to it and still overwrite it. They only ever answer "somebody else
        // holds this", which is a leak, and a leak beats releasing a value a
        // callee kept.
        if row.gone.is_some() && !matches!(row.gone, Some(Gone::Hole { .. })) {
            continue;
        }
        if let Some(name) = &row.from_call {
            if r.lending.contains(name) {
                row.gone = Some(Gone::Borrowed("a value its producer does not own"));
                continue;
            }
        }
        // Handed to a position that keeps what it is given. Rule 2 promises a
        // `read` callee does not, and refuses every way of breaking that promise
        // except one: a variant constructor, which `el(tag, attrs, kids)` is all
        // through `std/html`. So this asks per position, instead of assuming
        // every call may retain — the assumption this phase deleted, and the one
        // that left `let s = a + b; takes(s)` leaking.
        if let Some((to, _, line)) = row
            .passed
            .iter()
            .find(|(c, i, _)| r.retains.contains(&(c.clone(), *i)))
        {
            row.gone = Some(Gone::Lent {
                line: *line,
                to: to.clone(),
            });
            continue;
        }
        // Handed to a LENDER. A lender returns a projection of what it was given
        // (`tagOf(v)` is `match v { JStr(s) => s, .. }`), so its result names
        // storage inside this argument and this block may not release it.
        //
        // Phase 10a found this by writing the bug. `if let Some(j) = maybe(x) {
        // g = tagOf(j) }` released the scrutinee and left module state pointing
        // at the freed buffer — a store of a CALL result records no move, so
        // nothing else here could see it. The rule is wider than the row that
        // needed it, and wider is the safe direction: it can only stop a release.
        if let Some((to, _, line)) = row.passed.iter().find(|(c, _, _)| r.lending.contains(c)) {
            row.gone = Some(Gone::Lent {
                line: *line,
                to: to.clone(),
            });
        }
    }
    Facts {
        lets,
        arg_temps,
        store_events: r.store_events,
        global_stores: r.global_stores,
        edge_releases: r.edge_releases,
    }
}

/// What a run of the pass is for. The check is the hot path — a keystroke pays
/// for it — so neither record is built unless somebody asked.
#[derive(PartialEq, Clone, Copy)]
enum Want {
    Check,
    Sites,
    Lets,
    /// RFC-0092 M0: record every projection the rule would refuse, refuse none.
    Projections,
}

/// One run's outputs.
struct Run {
    diags: Vec<Diagnostic>,
    sites: Vec<OwningSite>,
    lets: HashMap<usize, LetOwnership>,
    lending: HashSet<String>,
    retains: HashSet<(String, usize)>,
    arg_temps: Vec<ArgTemp>,
    projections: Vec<ProjectionSite>,
    store_events: Vec<StoreEv>,
    global_stores: HashSet<usize>,
    edge_releases: Vec<EdgeRel>,
}

/// What the callee does with the temporary at `(callee, ix)`.
///
/// Every clause is a rule that already shipped, read at a position instead of at
/// a binding: `constructs` is `59c8a0c`'s recorded exit, `retains` is
/// [`MoveCheck::note_retention`]'s set closed over the call graph, `lending` is
/// [`MoveCheck::lends`]'s, and the capability is the parameter's own
/// declaration. Rules 2 and 3 are what make `read` mean "keeps nothing": a
/// borrow may not be stored and may not be returned, and `59c8a0c` closed the
/// hand-over exit.
fn arg_verdict(
    s: &ArgTemp,
    caps: &HashMap<String, Vec<Capability>>,
    retains: &HashSet<(String, usize)>,
    lending: &HashSet<String>,
    decl: &Declared,
) -> ArgVerdict {
    // The producer handed back storage it does not own, so there is no
    // temporary here at all — the same rule [`ownership`] applies to a `let`
    // whose initializer is a call to a lender.
    if s.producer.as_deref().is_some_and(|p| lending.contains(p)) {
        return ArgVerdict::Lent;
    }
    // RFC-0096 M3's four consumer sites free this operand already. The two
    // rules must not both fire on one value, and this is where they partition.
    let allocating_operand = match s.producer.as_deref() {
        None => true,
        Some(p) => p == "@str" || p == "@concat",
    };
    if allocating_operand && (s.callee == "@str" || s.callee == "@concat") {
        return ArgVerdict::AlreadyFreed;
    }
    // A variant constructor is a literal that reads like a call: the value it
    // builds holds the argument and outlives the call. It has no signature, so
    // it is asked for first.
    if decl.constructs(&s.callee) {
        return ArgVerdict::Retained;
    }
    if retains.contains(&(s.callee.clone(), s.ix)) {
        return ArgVerdict::Retained;
    }
    if lending.contains(&s.callee) || views(&s.callee) {
        return ArgVerdict::Lent;
    }
    // A row whose RETURN is the same bare type parameter as this argument's may
    // hand the argument straight back, and `blackBox` does exactly that: it is
    // the identity, written so an optimizer cannot see through it. Freeing at
    // the call would free a buffer the result still names, which is a
    // use-after-free and not a leak — `blackBox(concatFresh(blackBox(pad()),
    // ..))` is the shape, six times over in `examples/membench.vyrn`.
    //
    // Read off the signature rather than off a name: `lends` answers for a row
    // whose body yields a place INSIDE a parameter, and this row yields the
    // parameter itself, which no body spelling can say. A user function cannot
    // reach here — rule 3 refuses returning a borrow, and `lending` above
    // catches what it allows.
    if let Some(f) = crate::prelude::signature(&s.callee) {
        if let (Type::Param(r), Some(Type::Param(p))) = (&f.ret, f.params.get(s.ix).map(|p| &p.ty))
        {
            if r == p {
                return ArgVerdict::Lent;
            }
        }
    }
    let cap = caps
        .get(&s.callee)
        .and_then(|c| c.get(s.ix))
        .copied()
        .or_else(|| crate::prelude::capability(&s.callee, s.ix));
    match cap {
        Some(Capability::Read) => ArgVerdict::Released,
        Some(Capability::Consume) => ArgVerdict::Transferred,
        // `modify` and `share` write through the argument, which no temporary
        // can be the destination of. The corpus has none.
        Some(_) | None => ArgVerdict::Unknown,
    }
}

/// The identity of a `let`: its node address, the key `own.rs` emits drops with.
fn let_id(s: &Stmt) -> usize {
    s as *const Stmt as usize
}

/// Whether the builtin `name` hands back a pointer **into** its argument.
///
/// It was `RESERVED_VIEWS`, two names in a hand list. RFC-0094 M1 moved the
/// fact onto the seeded signature: a row whose body yields a place inside a
/// parameter lends its result, and `crate::prelude` holds the rows. `@at` reads
/// an element out of a container; `bytes` is a view of a String's buffer, which
/// is what `std/codecs` and `std/text` are written on. A binding to one of these
/// owns nothing, so nothing may release it.
///
/// `get` was in the old list, for Path B's cell read. RFC-0090 M4 deleted that
/// builtin AND took `cell`/`get`/`set` out of [`crate::checker::RESERVED`] in
/// the same stroke, which handed the names to users — and the list matched on
/// the CALL, not on a builtin table. So `get` stayed, and any user function
/// called `get` handed back a view that owns nothing. `std/slots`' own reader
/// copies its element out. A `Slots<String>` read through it leaked, silently.
/// The pin that stops that is now `prelude`'s own
/// `every_seeded_name_is_reserved_or_unspellable`.
fn views(name: &str) -> bool {
    crate::prelude::lends(name)
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
    run(program, Want::Check).diags
}

/// Every place rule 2 refuses a **store** of a borrow, out of `program`.
///
/// A filter over [`check_accum`] rather than a mode of its own: rule 2 is
/// enforced on every check since Phase 4b-2, so this is a reading of the
/// diagnostics rather than a second walk. The corpus test below asks for it by
/// name and expects zero.
pub fn borrow_store_sites(program: &Program) -> Vec<Diagnostic> {
    check_accum(program)
        .into_iter()
        .filter(|d| d.message.contains("may not be stored into"))
        .collect()
}

/// The one walk, shared by [`check_accum`], [`owning_sites`] and [`ownership`].
/// `want` turns each record on; with neither the pass still builds and carries
/// its type environment, and asks `owns_heap` nowhere.
fn run(program: &Program, want: Want) -> Run {
    let mut caps: HashMap<String, Vec<Capability>> = program
        .functions
        .iter()
        .map(|f| {
            (
                f.name.clone(),
                f.params.iter().map(|p| p.capability).collect(),
            )
        })
        .collect();
    // A method call is written `s.insert(v)` and reaches this pass as
    // `insert(s, v)` — the SURFACE name, because the impl is selected by the
    // receiver's type and this pass does not select impls. The protocol is what
    // both sides agree on (conformance compares capabilities), so its
    // declaration is the discipline every call site reads: without this the
    // exclusivity rule and the `consume` move would both go silent the moment a
    // function became a method.
    for p in &program.protocols {
        for m in &p.methods {
            let mut cs = vec![m.recv];
            cs.extend(m.param_caps.iter().copied());
            caps.insert(m.name.clone(), cs);
        }
    }
    let globals: HashSet<String> = program.globals.iter().map(|g| g.name.clone()).collect();
    // `export extern fn` names. Rule 3 is stricter here, because the caller is
    // JS and JS frees every String it is handed (RFC-0089 M3b).
    let exported: HashSet<String> = program
        .functions
        .iter()
        .filter(|f| f.is_export_extern)
        .map(|f| f.name.clone())
        .collect();
    let decl = Declared::new(program);
    let mc = MoveCheck {
        caps: &caps,
        globals: &globals,
        exported: &exported,
        errors: RefCell::new(Vec::new()),
        decl: &decl,
        // Module state is the outermost frame and is built once, not per body.
        vars: RefCell::new(Scopes::new(decl.globals())),
        // Module state is nobody's borrow: it owns what it holds for the whole
        // module, which is why nothing may `consume` it either.
        borrows: RefCell::new(Scopes::new(
            program
                .globals
                .iter()
                .map(|g| (g.name.clone(), None))
                .collect(),
        )),
        ret: RefCell::new(Type::Unit),
        lambda_base: RefCell::new(Vec::new()),
        lambda_escapes: RefCell::new(Vec::new()),
        arm_binders: RefCell::new(Vec::new()),
        call_keeps: std::cell::Cell::new(None),
        continue_seen: std::cell::Cell::new(false),
        sites: (want == Want::Sites).then(|| RefCell::new(Vec::new())),
        nodes: RefCell::new(Scopes::new(HashMap::new())),
        lets: (want == Want::Lets).then(|| RefCell::new(HashMap::new())),
        cur_fn: RefCell::new(String::new()),
        lending: (want == Want::Lets).then(|| RefCell::new(HashSet::new())),
        forwards: (want == Want::Lets).then(|| RefCell::new(HashMap::new())),
        retains: (want == Want::Lets).then(|| RefCell::new(HashSet::new())),
        handed_on: (want == Want::Lets).then(|| RefCell::new(HashMap::new())),
        param_ix: RefCell::new(HashMap::new()),
        arg_temps: (want == Want::Lets).then(|| RefCell::new(Vec::new())),
        store_events: (want == Want::Lets).then(|| RefCell::new(Vec::new())),
        global_stores: (want == Want::Lets).then(|| RefCell::new(HashSet::new())),
        edge_releases: (want == Want::Lets).then(|| RefCell::new(Vec::new())),
        ev_order: std::cell::Cell::new(0),
        loop_ids: RefCell::new(Vec::new()),
        next_loop: std::cell::Cell::new(0),
        projections: (want == Want::Projections).then(|| RefCell::new(Vec::new())),
    };
    let mut out = Vec::new();
    let mut projections = Vec::new();
    // A projection site is stamped with the module of the body it was found in,
    // the same way an error is — the sink itself has no idea which file it is
    // reading, and a linked program is most of somebody else's.
    let drain = |into: &mut Vec<ProjectionSite>, mc: &MoveCheck, module: &Option<String>| {
        if let Some(sink) = &mc.projections {
            for mut p in sink.borrow_mut().drain(..) {
                p.module.clone_from(module);
                into.push(p);
            }
        }
    };
    // An argument temporary is stamped the same way, and for the same reason: a
    // linked program is most of somebody else's, and the row has to say whose.
    // It stays in place rather than draining, because the verdict below wants
    // every row of the whole program at once.
    let stamp = |mc: &MoveCheck, module: &Option<String>| {
        if let Some(sink) = &mc.arg_temps {
            for s in sink.borrow_mut().iter_mut() {
                if s.module.is_none() {
                    s.module.clone_from(module);
                }
            }
        }
    };
    for f in &program.functions {
        mc.errors.borrow_mut().clear();
        mc.function(f);
        drain(&mut projections, &mc, &f.module);
        stamp(&mc, &f.module);
        for s in mc.errors.borrow_mut().drain(..) {
            let mut d = s;
            d.file = f.module.clone();
            out.push(d);
        }
    }
    // Test bodies (RFC-0015) move-check as ordinary Unit function bodies, so
    // use-after-consume inside a test is caught unchanged. The body is walked
    // **in place**: a clone would carry different node addresses, and Phase 4c
    // keys reclamation on them.
    for t in &program.tests {
        mc.errors.borrow_mut().clear();
        mc.body(&[], &Type::Unit, &t.body);
        drain(&mut projections, &mc, &t.module);
        stamp(&mc, &t.module);
        for s in mc.errors.borrow_mut().drain(..) {
            let mut d = s;
            d.file = t.module.clone();
            out.push(d);
        }
    }
    // Bench bodies (RFC-0055) move-check identically.
    for b in &program.benches {
        mc.errors.borrow_mut().clear();
        mc.body(&[], &Type::Unit, &b.body);
        drain(&mut projections, &mc, &b.module);
        stamp(&mc, &b.module);
        for s in mc.errors.borrow_mut().drain(..) {
            let mut d = s;
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
    out.extend(linear::check(program, &decl));
    // Close the lending set: a function that returns what a lender returned is
    // a lender too. It only grows and the function count bounds it, so the loop
    // stops. Two passes settle the whole corpus; the loop is here because
    // "usually two" is not an argument.
    let mut lending = mc.lending.map(RefCell::into_inner).unwrap_or_default();
    let forwards = mc.forwards.map(RefCell::into_inner).unwrap_or_default();
    loop {
        let before = lending.len();
        for (caller, callees) in &forwards {
            if callees.iter().any(|c| lending.contains(c)) {
                lending.insert(caller.clone());
            }
        }
        if lending.len() == before {
            break;
        }
    }
    // Retention travels backwards: a parameter forwarded into a position that
    // keeps what it is given is kept too. Same shape as the lending closure
    // above, and it stops for the same reason.
    let mut retains = mc.retains.map(RefCell::into_inner).unwrap_or_default();
    let handed_on = mc.handed_on.map(RefCell::into_inner).unwrap_or_default();
    loop {
        let before = retains.len();
        for (pos, callers) in &handed_on {
            if retains.contains(pos) {
                for c in callers {
                    retains.insert(c.clone());
                }
            }
        }
        if retains.len() == before {
            break;
        }
    }
    // The verdict per call-argument temporary. It waits for both closures above:
    // whether a position keeps what it is given is only settled once every body
    // has been read, which is why the walk records the site and decides nothing.
    let mut arg_temps = mc.arg_temps.map(RefCell::into_inner).unwrap_or_default();
    for s in &mut arg_temps {
        s.verdict = arg_verdict(s, &caps, &retains, &lending, &decl);
    }
    Run {
        diags: out,
        sites: mc.sites.map(RefCell::into_inner).unwrap_or_default(),
        lets: mc.lets.map(RefCell::into_inner).unwrap_or_default(),
        lending,
        retains,
        arg_temps,
        projections,
        store_events: mc.store_events.map(RefCell::into_inner).unwrap_or_default(),
        global_stores: mc
            .global_stores
            .map(RefCell::into_inner)
            .unwrap_or_default(),
        edge_releases: mc
            .edge_releases
            .map(RefCell::into_inner)
            .unwrap_or_default(),
    }
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
    /// Every `export extern fn` (RFC-0012). Rule 3 admits no lend here: the
    /// caller is JS, and since RFC-0089 M3b the wrapper frees every String an
    /// export hands back. See [`MoveCheck::check_return`].
    exported: &'a HashSet<String>,
    /// Per-function statement-boundary error sink (RFC-0006 accumulation).
    /// Cleared at the start of each function, drained by `check_accum`.
    errors: RefCell<Vec<Diagnostic>>,
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
    /// The pattern binders of each open `match`/`if let` arm, innermost last.
    /// A binder over an owned scrutinee binds a [`Borrow::Projection`], but the
    /// fact it names is not "this frame still owns the aggregate" — the
    /// scrutinee handed the payload up and the arm's value carries it out. A
    /// `let t = d.title` binds the same row with the dangerous meaning, so
    /// [`MoveCheck::check_handover`] tells them apart by this mark.
    arm_binders: RefCell<Vec<HashSet<String>>>,
    /// Set while one call argument is being walked, to whether the callee may
    /// KEEP a `fn` value it is handed. A lambda anywhere but a call argument
    /// can be stored and outlive the frame; one at a call argument still
    /// escapes when the parameter is `consume`, because a kept closure's
    /// captures leave the frame with it (RFC-0037).
    call_keeps: std::cell::Cell<Option<bool>>,
    /// Set when the walk under a loop body reached a `continue`. `continue`
    /// starts the NEXT iteration rather than leaving the loop, so it must not
    /// count as the divergence [`MoveCheck::check_loop_reuse`] may skip on —
    /// a body every path of which LEAVES (`break`/`return`/`panic`) runs at
    /// most once; one that continues does not. Each loop saves and resets it
    /// around its body walk, so an inner loop's `continue` stays the inner
    /// loop's.
    continue_seen: std::cell::Cell<bool>,
    /// Where recorded [`OwningSite`]s go, or `None` on the ordinary check path —
    /// which is what keeps a build and a keystroke paying for nothing.
    sites: Option<RefCell<Vec<OwningSite>>>,
    /// The declaring `Stmt::Let` of every name in scope, in lockstep with `vars`
    /// — 0 for a parameter, a loop variable, a pattern binder or a lambda
    /// parameter. **Every** binder is recorded, so an inner `let s` shadowing an
    /// outer one takes the move rather than passing it up.
    nodes: RefCell<Scopes<usize>>,
    /// Where the per-`let` ownership rows go, or `None` on the check path.
    lets: Option<RefCell<HashMap<usize, LetOwnership>>>,
    /// The function being checked, so a recorded fact can name it.
    cur_fn: RefCell<String>,
    /// Functions whose result the caller must NOT release — see
    /// [`MoveCheck::check_return`]. `None` on the check path.
    lending: Option<RefCell<HashSet<String>>>,
    /// `caller -> every callee named in a `return` whose result the caller
    /// releases`. A function that hands one of these straight back lends what it
    /// was lent, so the set is closed over this before it is used.
    forwards: Option<RefCell<HashMap<String, Vec<String>>>>,
    /// Parameter positions that KEEP what they are handed — see
    /// [`MoveCheck::note_retention`]. `None` on the check path.
    retains: Option<RefCell<HashSet<(String, usize)>>>,
    /// `(callee, i) -> every (caller, its own parameter index)` that forwards a
    /// parameter into that position. Retention travels backwards along these.
    handed_on: Option<RefCell<HashMap<(String, usize), Vec<(String, usize)>>>>,
    /// The index of each parameter of the function under check.
    param_ix: RefCell<HashMap<String, usize>>,
    /// Where the call-argument temporaries go — see [`MoveCheck::note_arg_temp`].
    /// `None` on the check path, with `lets`.
    arg_temps: Option<RefCell<Vec<ArgTemp>>>,
    /// RFC-0114 M2: the write/take event stream (see [`StoreEv`]), and the
    /// assigns to module state, which are owned unconditionally.
    store_events: Option<RefCell<Vec<StoreEv>>>,
    global_stores: Option<RefCell<HashSet<usize>>>,
    edge_releases: Option<RefCell<Vec<EdgeRel>>>,
    ev_order: std::cell::Cell<u32>,
    /// The stack of loop ids the walk is inside — pushed by `while`/`for`,
    /// stamped onto every event, so the fold can see which pairs of events a
    /// back edge could reorder.
    loop_ids: RefCell<Vec<u32>>,
    next_loop: std::cell::Cell<u32>,
    /// Where RFC-0092 M0's projection sites go, or `None` everywhere else. The
    /// measurement is a mode, not a second walk: the two places that would refuse
    /// are the two places that record.
    projections: Option<RefCell<Vec<ProjectionSite>>>,
}

/// A binding that names a value somebody else owns (RFC-0089 rule 2).
///
/// The variants exist for the diagnostic, not for the rule: every borrow is
/// refused in the same three positions, and each variant names a different fix.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Borrow {
    /// A `read` or `share` parameter — the caller still owns it. Carries the
    /// PARAMETER's name, which is not always the name at the offending line: a
    /// local inherits the borrow (`let t = s`), and Phase 9 recorded that the
    /// menu then offered ``declare the parameter `t: consume ..` `` for a `t`
    /// that is a local. `vyrn fix` never applies that entry, so it was wording
    /// and not correctness — and wording a reader has to see through.
    Read(String),
    /// A `modify` parameter — exclusive in-place access, still the caller's.
    /// Carries the parameter's name, for [`Borrow::Read`]'s reason.
    Modify(String),
    /// A `for` variable over a container the loop does NOT own, carrying that
    /// container's name so the diagnostic can spell the consuming form. A loop
    /// over a `consume`d container or over a temporary binds an owner instead.
    Element(String),
    /// A local bound to a field or element read: `let t = r.s`. A place owns its
    /// contents (rule 4), so reading one out does not take it.
    Projection,
}

impl Borrow {
    /// What this borrow is, in words, for the message.
    ///
    /// `at` is the name the message is about. It is not always the parameter:
    /// `let t = s` gives `t` the borrow `s` carries, and calling `t` a parameter
    /// is the wording defect Phase 9 recorded on the fix menu. Both halves say
    /// the same thing now.
    fn what(&self, at: &str) -> String {
        let of = |kind: &str, p: &String| {
            if root_of(at) == *p {
                format!("a `{kind}` parameter")
            } else {
                format!("a second name for the `{kind}` parameter `{p}`")
            }
        };
        match self {
            Borrow::Read(p) => of("read", p),
            Borrow::Modify(p) => of("modify", p),
            Borrow::Element(_) => "a loop variable".to_string(),
            Borrow::Projection => "read out of a place that owns it".to_string(),
        }
    }

    /// The named ways out (RFC-0087 U2). The order is the order a reader should
    /// try them: take ownership if the callee should have it, copy if both sides
    /// genuinely need a value. `root` is the binding, `path` what was read out
    /// of it — a `consume` goes on the binding, a `.copy()` on the path.
    fn fixes(&self, root: &str, path: &str) -> Vec<String> {
        let copy = format!("`{path}.copy()` if both sides need a value");
        match self {
            Borrow::Read(p) | Borrow::Modify(p) => vec![
                format!("declare the parameter `{p}: consume ..` if this function should own it"),
                copy,
            ],
            // A loop variable has a second way out: let the loop take the
            // container. It only works when the whole element is stored — a
            // stored field of it is a partial move — so `copy` stays first when
            // the two differ.
            Borrow::Element(c) if root == path => vec![
                format!("`for {root} in consume {c}` if the loop should take the elements"),
                copy,
            ],
            Borrow::Element(_) | Borrow::Projection => vec![copy],
        }
    }
}

/// Which form wrote the `consume` — the two share [`MoveCheck::check_take`] and
/// differ only in how the first refusal reads.
#[derive(Clone, Copy)]
enum TakeForm {
    /// `for x in consume xs`.
    Loop,
    /// `consume p` as a prefix on a place (RFC-0093).
    Prefix,
}

impl TakeForm {
    fn what(self) -> &'static str {
        match self {
            TakeForm::Loop => "a `for` loop",
            TakeForm::Prefix => "a take",
        }
    }

    fn nothing_to_take(self) -> String {
        match self {
            TakeForm::Loop => "`consume` here has nothing to take — the loop already owns a \
                               container that is not a binding"
                .to_string(),
            TakeForm::Prefix => {
                "`consume` here has nothing to take — the value is already owned, so there is \
                 no place to leave a hole in"
                    .to_string()
            }
        }
    }

    fn drop_it(self) -> String {
        match self {
            TakeForm::Loop => "drop the `consume`: the elements are already owned".to_string(),
            TakeForm::Prefix => "drop the `consume`: the value is already owned".to_string(),
        }
    }
}

/// The base name of a place path: `r.a[0]` is `r`.
///
/// A message names a PATH and a borrow names the PARAMETER it came from, so the
/// two are comparable only at the root.
fn root_of(path: &str) -> &str {
    match path.find(['.', '[']) {
        Some(i) => &path[..i],
        None => path,
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
    /// Whether this consumption left a HOLE — a take of a projection, and the
    /// only thing that makes reading the root as a whole an error (RFC-0093).
    ///
    /// It is a flag and not a test on the key, because RFC-0082's place desugar
    /// names its temporaries after the paths they took: `o.i[].xs[]` is one
    /// binding, moved whole, and reading `o.i[]` afterwards is not a hole. The
    /// take is the one thing that can make one.
    hole: bool,
}

/// Consumed places: PATH -> what took it (RFC-0093), bucketed by ROOT.
///
/// It was keyed by root name until the take arrived. A take makes a hole in one
/// path rather than emptying the whole binding, so `consume er.node` records
/// `er.node` and leaves `er.next` readable. A whole-binding move still records
/// the bare name, which is the same key it always recorded.
///
/// The root is back, one level up. A flat `HashMap<String, Consumption>` bought
/// nothing: [`overlaps`] is a string-prefix relation, so every use paid the hash
/// AND then scanned the whole map anyway — 250 / 500 / 1,000 / 2,000 drops
/// interleaved with reads of a live binding measured 10 / 15 / 31 / 99 ms, which
/// is quadratic. [`overlaps`] is false whenever the roots differ ([`under`] is
/// `starts_with` at a `.` boundary, and `root_of` cuts at the first `.` or `[`),
/// so one bucket holds every path a given use can collide with, and a lookup
/// replaces the scan.
#[derive(Clone, Default)]
struct Consumed(HashMap<String, HashMap<String, Consumption>>);

impl Consumed {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn insert(&mut self, path: String, c: Consumption) {
        self.0
            .entry(root_of(&path).to_string())
            .or_default()
            .insert(path, c);
    }

    /// Record `c` for `path` only if the path has no consumption yet — the merge
    /// after a branch, where the FIRST arm to consume a path names the line.
    fn or_insert(&mut self, path: String, c: Consumption) {
        self.0
            .entry(root_of(&path).to_string())
            .or_default()
            .entry(path)
            .or_insert(c);
    }

    fn get(&self, path: &str) -> Option<&Consumption> {
        self.0.get(root_of(path))?.get(path)
    }

    fn contains_key(&self, path: &str) -> bool {
        self.get(path).is_some()
    }

    fn remove(&mut self, path: &str) {
        let root = root_of(path);
        if let Some(b) = self.0.get_mut(root) {
            b.remove(path);
            if b.is_empty() {
                self.0.remove(root);
            }
        }
    }

    /// A write to `path` revives it and every path inside it — one bucket, since
    /// nothing outside `path`'s root can be inside `path`.
    fn revive(&mut self, path: &str) {
        let root = root_of(path);
        if let Some(b) = self.0.get_mut(root) {
            b.retain(|k, _| k != path && !under(k, path));
            if b.is_empty() {
                self.0.remove(root);
            }
        }
    }

    /// Every recorded path that names storage overlapping `path`. The one bucket
    /// the roots can agree on, which is the whole of the fix.
    fn overlapping<'a>(
        &'a self,
        path: &'a str,
    ) -> impl Iterator<Item = (&'a String, &'a Consumption)> {
        self.0
            .get(root_of(path))
            .into_iter()
            .flat_map(|b| b.iter())
            .filter(move |(k, _)| overlaps(k, path))
    }

    fn iter(&self) -> impl Iterator<Item = (&String, &Consumption)> {
        self.0.values().flat_map(|b| b.iter())
    }
}

impl IntoIterator for Consumed {
    type Item = (String, Consumption);
    type IntoIter = std::vec::IntoIter<(String, Consumption)>;
    fn into_iter(self) -> Self::IntoIter {
        self.0
            .into_values()
            .flatten()
            .collect::<Vec<_>>()
            .into_iter()
    }
}

impl<'a> IntoIterator for &'a Consumed {
    type Item = (&'a String, &'a Consumption);
    type IntoIter = Box<dyn Iterator<Item = Self::Item> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

/// Whether two place paths name overlapping storage: equal, or one a prefix of
/// the other at a `.`/`[` boundary.
///
/// This is the whole of the path rule. Reading `er.node` after taking `er` is
/// the prefix direction; reading `er` whole after taking `er.node` is the other
/// one, and both are refused for the same reason — the storage they name is not
/// all there.
fn overlaps(a: &str, b: &str) -> bool {
    a == b || under(a, b) || under(b, a)
}

/// Whether `long` names storage inside `short`: `er.node` is under `er`.
///
/// FIELDS ONLY, and a name carrying a `[` relates to nothing but itself. Two
/// reasons, and `tests/places.rs` found both.
///
/// RFC-0082's place desugar names its temporaries after the paths they took, so
/// `o.i.xs[k] = v` moves through bindings literally called `o.i[]` and
/// `o.i[].xs[]`. Read as paths, the second is inside the first, and every write
/// back then reads as a use of something moved. They are not paths; they are one
/// binding each, and identity is the whole relation they want.
///
/// The element case needs nothing more either: a take never reaches an element —
/// `consume xs[i]` is refused and `swapRemove` is the answer — so no key this
/// relation has to widen can carry a `[`.
fn under(long: &str, short: &str) -> bool {
    !long.contains('[')
        && !short.contains('[')
        && long.len() > short.len()
        && long.starts_with(short)
        && long.as_bytes()[short.len()] == b'.'
}

/// A write to `path` revives it and every path inside it.
///
/// `movecheck`'s own module comment has said *"reassignment revives a variable"*
/// since Phase 4b; RFC-0093 makes the same sentence true one dot down. Only
/// downward: writing `er.node` fills that field, it does not put back an `er`
/// that was moved away whole.
fn revive(consumed: &mut Consumed, path: &str) {
    consumed.revive(path);
}

impl Consumption {
    /// A `consume` capability took it — the wording this pass has always used.
    fn by_capability(line: usize, by: String) -> Self {
        Consumption {
            line,
            by,
            fixes: Vec::new(),
            hole: false,
        }
    }
}

impl MoveCheck<'_> {
    fn function(&self, f: &Function) {
        *self.cur_fn.borrow_mut() = f.name.clone();
        self.body(&f.params, &f.ret, &f.body);
    }

    /// One body, with its parameters and return type. Takes the pieces rather
    /// than a `Function` so a test or bench body is walked **in place** — the
    /// node addresses are the reclamation key (Phase 4c).
    fn body(&self, params: &[Param], ret: &Type, body: &Block) {
        let f_params = params;
        *self.param_ix.borrow_mut() = f_params
            .iter()
            .enumerate()
            .map(|(i, p)| (p.name.clone(), i))
            .collect();
        let mut consumed = Consumed::default();
        let mut scope: Vec<HashSet<String>> =
            vec![f_params.iter().map(|p| p.name.clone()).collect()];
        // Module state is the outermost frame, the parameters the next one — the
        // order every function body sees them in (RFC-0013).
        {
            let mut v = self.vars.borrow_mut();
            let mut b = self.borrows.borrow_mut();
            let mut n = self.nodes.borrow_mut();
            v.truncate(1);
            b.truncate(1);
            n.truncate(1);
            v.enter();
            b.enter();
            n.enter();
            for p in f_params {
                v.bind(&p.name, Some(p.ty.clone()));
                n.bind(&p.name, 0);
                // RFC-0089 rule 2: everything but `consume` is a borrow, and only
                // a type that owns heap has anything to borrow.
                b.bind(
                    &p.name,
                    match p.capability {
                        Capability::Consume => None,
                        _ if !self.decl.owns_heap(&p.ty) => None,
                        Capability::Modify => Some(Borrow::Modify(p.name.clone())),
                        _ => Some(Borrow::Read(p.name.clone())),
                    },
                );
            }
        }
        *self.ret.borrow_mut() = ret.clone();
        self.lambda_base.borrow_mut().clear();
        self.lambda_escapes.borrow_mut().clear();
        self.block(body, &mut consumed, &mut scope);
    }

    /// Push a frame on ALL THREE stacks. They are read as one environment — a
    /// name's type, whether it is a borrow, and where it was declared — so they
    /// are never entered apart.
    fn enter(&self) {
        self.vars.borrow_mut().enter();
        self.borrows.borrow_mut().enter();
        self.nodes.borrow_mut().enter();
    }

    fn exit(&self) {
        self.vars.borrow_mut().exit();
        self.borrows.borrow_mut().exit();
        self.nodes.borrow_mut().exit();
    }

    /// Bind `name` with its type and its borrow status. Every binder that is not
    /// a `let` gets node 0 — it declares no reclaimable binding, and recording it
    /// is what stops it inheriting an outer `let`'s identity.
    fn bind(&self, name: &str, ty: Option<Type>, borrow: Option<Borrow>) {
        self.vars.borrow_mut().bind(name, ty);
        self.borrows.borrow_mut().bind(name, borrow);
        self.nodes.borrow_mut().bind(name, 0);
    }

    /// Record what became of the binding `name` names, if this run is recording
    /// and if the binding is a `let` that has not already lost its value.
    ///
    /// First answer wins: a value can only leave once, and the earliest reason
    /// is the one a reader needs.
    /// The reclamation row of the place `e` names, or 0 for anything else.
    ///
    /// A pattern binder over a place is a PROJECTION of it, so what becomes of
    /// the binder becomes of the place:
    /// `if let Some(resp) = answer { return Some(apply(resp)) }` hands `answer`'s
    /// payload to the caller. Keying the binder to the place's row is what
    /// records that. Without it Phase 5 released `answer` on the way out and the
    /// caller read freed memory — `examples/rest.vyrn`, in one parity run.
    fn place_key(&self, e: &Expr) -> usize {
        let Some((root, _)) = place_path(e) else {
            return 0;
        };
        self.nodes.borrow().get(&root).copied().unwrap_or(0)
    }

    /// Give a statement that walks a TEMPORARY a row of its own, keyed by the
    /// statement's node address, and hand back that key (Phase 10a).
    ///
    /// The temporary owns what it holds and has no name, so the row is what makes
    /// it releasable at all: the binders bind to this key, and the ordinary
    /// `took` path then records a `return`, a store, a capture or a handover onto
    /// it exactly as it does for a `let`. `from_call` is filled for the same
    /// reason a `let`'s is — a call to a LENDER hands back storage nobody here
    /// owns, and [`ownership`] reads that after every body.
    ///
    /// Two statements ask for one: `if let PAT = f()` (Phase 10a) and
    /// `for x in f()` (RFC-0092 M5). The key is the statement address, which is
    /// the key `own.rs` reads.
    ///
    /// A `match` asks too, and it is an EXPRESSION — so it keys on its own node
    /// address through [`MoveCheck::note_temporary_at`]. One AST, one arena: a
    /// `Stmt` address and an `Expr` address cannot collide.
    fn note_temporary(&self, s: &Stmt, value: &Expr) -> usize {
        self.note_temporary_at(let_id(s), value)
    }

    fn note_temporary_at(&self, key: usize, value: &Expr) -> usize {
        let Some(sink) = &self.lets else { return 0 };
        sink.borrow_mut().insert(
            key,
            LetOwnership {
                ty: self.type_of(value),
                gone: None,
                from_call: match value {
                    Expr::Call { name, .. } => Some(name.clone()),
                    _ => None,
                },
                passed: Vec::new(),
            },
        );
        key
    }

    /// Record one RFC-0114 M2 event for `key` (0 = untracked, dropped).
    fn store_ev(&self, key: usize, kind: EvKind) {
        if key == 0 {
            return;
        }
        let Some(sink) = &self.store_events else {
            return;
        };
        let order = self.ev_order.get();
        self.ev_order.set(order + 1);
        sink.borrow_mut().push(StoreEv {
            key,
            order,
            loops: self.loop_ids.borrow().clone(),
            kind,
        });
    }

    fn took(&self, name: &str, gone: Gone) {
        let Some(sink) = &self.lets else { return };
        let key = self.nodes.borrow().get(name).copied().unwrap_or(0);
        if key == 0 {
            return;
        }
        // RFC-0114 M2: everything reaching here except a borrow-rebind means
        // the value left or is compromised, and a later store must not release
        // what is no longer there. A `Borrowed` row is the WRITE's property —
        // its event was already pushed with `owning: false`.
        if !matches!(gone, Gone::Borrowed(_)) {
            self.store_ev(key, EvKind::Take);
        }
        if let Some(row) = sink.borrow_mut().get_mut(&key) {
            // A HOLE is not the last word (RFC-0093 M2): it says the binding is
            // reclaimed minus a few places, and every row written here says the
            // value LEFT — moved, returned, captured, lent. Those win, and they
            // have to. `gave_up` marks the root of every name a `return`
            // expression reads, so `return f(er.next)` after `consume er.node`
            // records that `er` left; keeping the hole instead would release a
            // value the caller now holds. The wasm generator engine trapped on
            // `std/vyx` within one run of this rule being wrong.
            if row.gone.is_none() || matches!(row.gone, Some(Gone::Hole { .. })) {
                row.gone = Some(gone);
            }
        }
    }

    /// A take of the place `rel` out of the binding `name` (RFC-0093 M2).
    ///
    /// Not [`MoveCheck::took`], because a hole ACCUMULATES: the ninth take out
    /// of one record must not overwrite the first eight, and `took` writes only
    /// where nothing is written yet. A row that already says something else —
    /// moved, captured, lent — keeps saying it, and that row is a leak or a
    /// move, so it is the safe answer either way.
    fn hole(&self, name: &str, line: usize, rel: Option<String>) {
        let Some(sink) = &self.lets else { return };
        let key = self.nodes.borrow().get(name).copied().unwrap_or(0);
        if key == 0 {
            return;
        }
        // RFC-0114 M2: a hole compromises the whole value for release purposes
        // — freeing around it is RFC-0093's job at block exit, not a store's.
        self.store_ev(key, EvKind::Take);
        if let Some(row) = sink.borrow_mut().get_mut(&key) {
            match &mut row.gone {
                None => {
                    row.gone = Some(Gone::Hole {
                        line,
                        paths: rel.iter().cloned().collect(),
                        // A path this pass cannot state relative to the binding
                        // is a path the walk cannot be told to skip.
                        skippable: rel.is_some(),
                    })
                }
                Some(Gone::Hole {
                    paths, skippable, ..
                }) => match rel {
                    Some(p) => {
                        if !paths.contains(&p) {
                            paths.push(p);
                        }
                    }
                    None => *skippable = false,
                },
                Some(_) => {}
            }
        }
    }

    /// A write to a place of `name`, after a take took one (RFC-0093 M2).
    ///
    /// The write fills the hole, and the store that fills it releases what the
    /// place held — the buffer the take gave away. So the binding stops being
    /// skippable and leaks whole. Only a binding that already carries a hole is
    /// touched; a write to any other binding means nothing here.
    fn wrote_into(&self, name: &str) {
        let Some(sink) = &self.lets else { return };
        let key = self.nodes.borrow().get(name).copied().unwrap_or(0);
        if key == 0 {
            return;
        }
        if let Some(row) = sink.borrow_mut().get_mut(&key) {
            if let Some(Gone::Hole { skippable, .. }) = &mut row.gone {
                *skippable = false;
            }
        }
    }

    /// Every name `e` reads, so a `return` or a `spawn` can give up all of them.
    ///
    /// A whole-expression sweep and not a place read: `return Some(s)` puts `s`
    /// in the caller's hands just as `return s` does, and an aggregate does not
    /// release its payload until Phase 5. Erring toward "it left" costs a leak.
    fn gave_up(&self, e: &Expr, gone: &Gone) {
        for n in reads(e) {
            self.took(&n, gone.clone());
        }
    }

    /// Whether a `let` of `value` names storage somebody else owns, for
    /// reclamation purposes.
    ///
    /// Wider than [`MoveCheck::borrow_from`], which answers rule 2 and therefore
    /// only fires where this reading can name a type. Reclamation must be right
    /// where the type is unknown too, so this asks the SHAPE: a field read, a
    /// view builtin, module state, or a name that is itself a borrow.
    fn names_a_place(&self, value: &Expr) -> Option<&'static str> {
        match value {
            Expr::Field { .. } => Some("read out of a place that owns it"),
            Expr::Var { name, .. } => {
                if self.borrow_of(name).is_some() {
                    return Some("a borrow of somebody else's value");
                }
                // Module state lives for the whole module and is never dropped,
                // so naming it takes nothing (RFC-0013). Frame 0 IS the globals
                // frame, which is also what tells a global from a local shadow.
                let global =
                    self.globals.contains(name) && self.vars.borrow().frame_of(name) == Some(0);
                global.then_some("module state, which nothing may take")
            }
            Expr::Call { name, .. } if views(name) => Some("a view into its argument"),
            // An arm can name a place as easily as the whole initializer can:
            // `let ty = if k < n { types[k] } else { "Int64" }` binds an element
            // of `types` on one path. One arm is enough — nothing here can say
            // which path runs.
            Expr::IfExpr {
                then_branch,
                else_branch,
                ..
            } => self
                .names_a_place(then_branch)
                .or_else(|| else_branch.as_ref().and_then(|b| self.names_a_place(b))),
            Expr::Match { arms, .. } => arms.iter().find_map(|a| self.names_a_place(&a.body)),
            // `x.copy()` allocates for exactly the types `owns_heap` counts
            // (RFC-0089 M1b). Everything else it is called on is a handle, and a
            // copied handle SHARES what it points at — releasing both copies of
            // a declared container would release one value twice.
            Expr::Call { name, args, .. } if name == "@copy" => (!args
                .first()
                .and_then(|a| self.type_of(a))
                .is_some_and(|t| self.decl.owns_heap(&t)))
            .then_some("a copy of a handle, which shares what it points at"),
            _ => None,
        }
    }

    /// The ways out of a borrow error in THIS function.
    ///
    /// [`Borrow::fixes`] offers `consume` first, and inside an `export extern fn`
    /// that fix does not exist: the caller is JS, which frees the String when the
    /// call returns whatever the declaration says, so `consume` is refused at the
    /// signature (RFC-0089 M3b). Offering it would send a reader to a second
    /// error. `.copy()` is the one answer, so it is the only one named.
    fn fixes_here(&self, b: &Borrow, root: &str, path: &str) -> Vec<String> {
        if matches!(b, Borrow::Read(_) | Borrow::Modify(_))
            && self.exported.contains(&*self.cur_fn.borrow())
        {
            return vec![format!(
                "`{path}.copy()` — an `export extern fn` may not take ownership of a String \
                 its JS caller releases"
            )];
        }
        // RFC-0093: the take is the first answer where it exists. A projection of
        // a root this frame OWNS may be moved out — that is the case RFC-0092 M1
        // could only answer with `.copy()`, and the 44 copies it landed are what
        // this entry removes. A borrowed root, module state and a container
        // element are all still `.copy()`, so the menu names the take only where
        // `check_take` would accept it.
        if matches!(b, Borrow::Projection)
            && path != root
            && self.borrow_of(root).is_none()
            && !self.is_module_state(root)
        {
            let mut fixes = vec![format!(
                "`consume {path}` if `{root}` should give it up — the field is dead afterwards"
            )];
            fixes.extend(b.fixes(root, path));
            return fixes;
        }
        b.fixes(root, path)
    }

    /// Whether `name` names module state here (RFC-0013) rather than a local
    /// that shadows one. Frame 0 IS the globals frame, which is what tells them
    /// apart — the same reading [`MoveCheck::names_a_place`] makes.
    fn is_module_state(&self, name: &str) -> bool {
        self.globals.contains(name) && self.vars.borrow().frame_of(name) == Some(0)
    }

    /// Whether `name` names a borrow here.
    fn borrow_of(&self, name: &str) -> Option<Borrow> {
        self.borrows.borrow().get(name).cloned().flatten()
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
                    Type::Record(fs) => fs.iter().find(|f| &f.name == field).map(|f| f.ty.clone()),
                    _ => None,
                }
            }
            // A take has the type of the place it takes (RFC-0093 M1). Without
            // this every `let t = consume d.title` is an unknown type, so
            // nothing reclaims it — which cost nothing while a record released
            // nothing, and costs the taken field the day M3 gives one a row.
            Expr::Consume { place, .. } => self.type_of(place),
            // An element read: `xs[i]` lowers to `@at`, and `x.copy()` is already
            // answered by `Declared`. The element type is the container's.
            Expr::Call { name, args, .. } if name == crate::project::AT => {
                let c = self.type_of(args.first()?)?;
                self.decl.elem_of(&c)
            }
            // A `match` yields one of its arms, and an arm's body reads the
            // payload the pattern binds. The binders have to be in scope for
            // that, which is why this reading is here and not in `Declared`:
            // resolving `m` past `Some(m)` to an outer String read `m + 1` as a
            // concatenation and the backend freed the integer.
            Expr::Match {
                scrutinee, arms, ..
            } => {
                let arm = arms.first()?;
                let (tys, borrow) = self.payload_binding(scrutinee, &arm.pattern);
                self.enter();
                for (i, b) in pattern_bindings(&arm.pattern).into_iter().enumerate() {
                    self.bind(b, tys.get(i).cloned().flatten(), borrow.clone());
                }
                let t = self.type_of(&arm.body);
                self.exit();
                t
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
    /// Answers whether the store **took** the source place, which is what
    /// Phase 4c reads: a place that did not move is still somebody else's.
    /// Whether parameter `i` of the builtin `name` takes its argument for good,
    /// under **rule 1**.
    ///
    /// It was `RESERVED_SINKS`, three rows in a hand list (RFC-0087 §2b).
    /// RFC-0094 M1 reads `consume` off the seeded signature instead
    /// ([`crate::prelude`]), so the fact is written once where every rule sees
    /// it. Everything else a builtin does with a heap argument is a read:
    /// `print` formats it, `@concat` copies out of it, `at` looks inside it.
    ///
    /// **A linear parameter is not rule 1's.** `close`, `boxStream` and
    /// `serveStream` each declare `consume Stream<T>`, and a `Stream<T>` already
    /// carries a disposal obligation the [`linear`] walk proves: every mention
    /// of a stream binding is a disposal there, so a second one is refused
    /// before rule 1 is asked. Two rules over one value would refuse the same
    /// program twice with the worse words — rule 1's menu offers `.copy()`,
    /// which a stream has no answer for. So the obligation on the TYPE wins, and
    /// the census's claim that these three had a rule "nowhere at all" is
    /// corrected rather than acted on.
    fn sinks(&self, name: &str, i: usize) -> bool {
        sinks(self.decl, name, i)
    }

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
    ) -> Result<bool, Diagnostic> {
        // An ELEMENT read stored inline: `out.push(xs[i])`. `xs[i]` reaches this
        // pass as `@at(xs, i)`, which is a call, so the `place_path` bail two
        // blocks down is where it used to leave — invisible to every rule.
        //
        // M1 widens `store` to see it, which M0 left as this milestone's
        // decision. The reason is the RFC's own thesis: `let t = xs[i]` already
        // binds a `Borrow::Projection` (see [`MoveCheck::borrow_from`]) and rule
        // 2 already refuses storing `t`. Leaving the inline form alone would
        // reproduce, for elements, exactly the two-spellings-two-verdicts defect
        // this RFC exists to remove. Refused whatever the container is, because a
        // borrowed container's element is no more storable than an owned one's.
        if let Some((root, path)) = element_path(value) {
            if self.projections.is_some() && outlives {
                let ty = self.type_of(value);
                self.note_projection("elem-store", &path, into(), ty, line);
            }
            if outlives && self.type_of(value).is_some_and(|t| self.decl.owns_heap(&t)) {
                let b = self.borrow_of(&root).unwrap_or(Borrow::Projection);
                self.note_retention(value);
                return Err(menu(
                    line,
                    format!(
                        "`{path}` may not be stored into {} — it is {}",
                        into(),
                        b.what(&path)
                    ),
                    self.fixes_here(&b, &root, &path),
                ));
            }
        }
        let Some((root, path)) = place_path(value) else {
            return Ok(false);
        };
        // RFC-0092 M0's instrument, kept as M1's regression guard: it records
        // what the branch below now refuses, so a site that reappears is counted.
        // Recorded BEFORE the `owns_heap` guard, so a scalar field is counted and
        // told apart rather than lost.
        if self.projections.is_some() && path != root && outlives && self.borrow_of(&root).is_none()
        {
            let ty = self.type_of(value);
            self.note_projection("store", &path, into(), ty, line);
        }
        // A scalar copies. An unnamed type is left alone — see the doc above.
        if !self.type_of(value).is_some_and(|t| self.decl.owns_heap(&t)) {
            return Ok(false);
        }
        if let Some(b) = self.borrow_of(&root) {
            if !outlives {
                return Ok(false);
            }
            self.note_retention(value);
            // Rule 2. A borrow may be observed and passed on; it may not be put
            // anywhere that outlives the call.
            return Err(menu(
                line,
                format!(
                    "`{path}` may not be stored into {} — it is {}",
                    into(),
                    b.what(&path)
                ),
                self.fixes_here(&b, &root, &path),
            ));
        }
        // RFC-0092's rule: **a projection is a borrow of its root, whatever the
        // root is.** Reading a field out of a record does not take the record —
        // the record still owns the buffer — so putting that buffer anywhere the
        // frame does not end with gives it two owners. The old bail said the
        // binding this store makes is "recorded by the caller", which is true of
        // a `let` and false of `out.push(d.title)`: there is no `let` and no
        // caller to record anything.
        //
        // A rebinding is not a store, and rule 2 does not refuse one either:
        // `let t = d.title` is fine and [`MoveCheck::borrow_from`] gives `t` the
        // projection, which rule 2 then enforces on.
        if path != root {
            if !outlives {
                return Ok(false);
            }
            return Err(menu(
                line,
                format!(
                    "`{path}` may not be stored into {} — it is {}",
                    into(),
                    Borrow::Projection.what(&path)
                ),
                self.fixes_here(&Borrow::Projection, &root, &path),
            ));
        }
        self.took(&root, Gone::Moved { line, by: into() });
        consumed.insert(
            root,
            Consumption {
                line,
                by: into(),
                fixes: vec![format!("`{path}.copy()` if both sides need a value")],
                hole: false,
            },
        );
        Ok(true)
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
            // **Whose place, when the answer is known.** `let n = nodes[i]` on a
            // `read` parameter used to bind a bare projection, which says "this
            // frame owns the root" — so `f(nodes[i])` was refused at a `consume`
            // parameter and `let n = nodes[i]` then `f(n)` was not. One fact, two
            // spellings, two verdicts. The root's borrow travels here for the
            // same reason it travels through `let t = s` one line down.
            Some((root, path)) if path != root => {
                Some(self.borrow_of(&root).unwrap_or(Borrow::Projection))
            }
            // `let t = s` where `s` is itself a borrow: the borrow travels.
            Some((root, _)) => self.borrow_of(&root),
            // An ELEMENT read, and a field OF one. [`place_path`] answers `None`
            // as soon as it meets the `@at(..)` call, so `element_path` is what
            // walks both — the same widening M1 gave `store` and
            // `returned_borrow`, arriving here late because a `let` of `ps[0].xs`
            // is written by the RFC-0082 place desugar and never by a person.
            // Without it that binding was an OWNER, and storing it gave two
            // elements one buffer.
            None => element_path(value)
                .map(|(root, _)| self.borrow_of(&root).unwrap_or(Borrow::Projection)),
        }
    }

    /// What a pattern's binders name: one type per binder, and whether they are
    /// borrows.
    ///
    /// Destructuring a place looks INTO it — `match o { Some(v) => .. }` does not
    /// take `o` apart, so `v` is a projection of it and rule 2 applies. A
    /// scrutinee that is a fresh value has no other owner, so its payload is
    /// owned.
    fn payload_binding(
        &self,
        scrutinee: &Expr,
        p: &Pattern,
    ) -> (Vec<Option<Type>>, Option<Borrow>) {
        let n = pattern_bindings(p).len();
        let ty = self.type_of(scrutinee);
        let tys: Vec<Option<Type>> = match (
            ty.as_ref()
                .map(|t| crate::types::resolve(t, self.decl.decls())),
            p,
        ) {
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
        // A place, and since RFC-0092 M3 an ELEMENT of one. `m[k]` reaches this
        // pass as `@at(m, k)`, which is a call, so `place_path` answered `None`
        // and the binder was an OWNER — `match ps[k] { Some(v) => v, .. }` handed
        // out a value the map still held. It leaked while a `Map` gave back only
        // its two buffers, and frees it twice now that the map releases what is
        // in them (`examples/rest.vyrn`).
        // The scrutinee's own borrow travels to the binder, for
        // [`MoveCheck::borrow_from`]'s reason: `match b.opt { Some(v) => f(v) }`
        // on a `read` parameter names a payload the caller still owns, and a bare
        // projection would have said this frame owned it.
        let borrow = place_path(scrutinee)
            .or_else(|| element_path(scrutinee))
            .map(|(root, _)| self.borrow_of(&root).unwrap_or(Borrow::Projection));
        (tys, borrow)
    }

    /// Whether iterating `e` reads a container somebody else still owns.
    ///
    /// A variable and a field are places. A call is a fresh value — except
    /// `@at(..)`, which is what `xs[i]` lowers to and is a projection of its
    /// receiver. The same judgment [`MoveCheck::borrow_from`] makes for a `let`.
    fn iterable_is_a_place(&self, e: &Expr) -> bool {
        match e {
            Expr::Var { .. } | Expr::Field { .. } => true,
            Expr::Call { name, args, .. } if name == crate::project::AT => {
                args.first().is_some_and(|a| self.iterable_is_a_place(a))
            }
            _ => false,
        }
    }

    /// Rule 1, asked of a PATH: is the storage `path` names still all there?
    ///
    /// It refuses an overlap in either direction. `er.node` after `consume er`
    /// reads part of something that is gone; `er` after `consume er.node` reads
    /// a whole record with a hole in it. The second message is RFC-0093's own,
    /// and it names the take rather than the read, because the take is the line
    /// the reader has to change.
    fn check_use(&self, path: &str, line: usize, consumed: &Consumed) -> Result<(), Diagnostic> {
        if consumed.is_empty() {
            return Ok(());
        }
        // Deterministic: the earliest consumption wins, ties broken by path, so
        // one program prints one message however the map is laid out.
        let Some((key, c)) = consumed
            .overlapping(path)
            .min_by(|(ak, a), (bk, b)| (a.line, *ak).cmp(&(b.line, *bk)))
        else {
            return Ok(());
        };
        // A hole: the read is the WHOLE of something a take emptied part of.
        if c.hole && under(key, path) {
            return Err(menu(
                c.line,
                format!(
                    "`{key}` was taken out of `{path}` here\nline {line}: ... and `{path}` is \
                     used as a whole here, with the hole still in it"
                ),
                vec![
                    format!(
                        "`{key}.copy()` on line {} if `{path}` is still needed whole",
                        c.line
                    ),
                    format!("write `{key}` back before this line"),
                ],
            ));
        }
        // A `consume` capability keeps the wording it has always had: the
        // capability IS the fix, so there is no menu to print.
        if c.fixes.is_empty() {
            let (cline, consumer) = (c.line, &c.by);
            return Err(Diagnostic::error(
                line,
                0,
                "movecheck",
                format!(
                    "`{path}` is used here but was already consumed by \
                     {consumer} on line {cline}\n  (a `consume` parameter takes \
                     ownership; the value can't be used afterward)"
                ),
            ));
        }
        // RFC-0089 rule 1, worded exactly as Phase 4b left it. Both lines name
        // the storage that moved rather than the longer path that reads it: the
        // line number already says which read, and `vyrn fix` looks for the
        // moved name.
        Err(menu(
            c.line,
            format!(
                "`{key}` was moved here into {}\nline {line}: ... and `{key}` is \
                 used again here",
                c.by
            ),
            c.fixes.clone(),
        ))
    }

    /// A take: `consume p` as a prefix (RFC-0093), or the `p` of
    /// `for x in consume p`. Three refusals, and RFC-0093 M1 deleted the fourth.
    ///
    /// The deleted one refused a PROJECTION — `consume b.tags` — on the ground
    /// that it would leave a hole in `b`. It offered `for .. in consume b`
    /// instead, which does not typecheck when `b` is a record, so the reader was
    /// sent to a second error. A hole is now a thing the pass can say, so the
    /// menu no longer has to name the root: the path is the answer.
    ///
    /// `by` names the form for the message — a loop and a prefix refuse the same
    /// three things and word the first one differently.
    fn check_take(
        &self,
        e: &Expr,
        line: usize,
        scope: &[HashSet<String>],
        by: TakeForm,
    ) -> Result<(), Diagnostic> {
        let Some((root, path)) = place_path(e) else {
            // A container element is the one place that CAN hold a hole at run
            // time, and `swapRemove` already spells it (RFC-0011). Naming it
            // beats "nothing to take", which is true and useless here.
            if let Some((root, path)) = element_path(e) {
                return Err(menu(
                    line,
                    format!("`{path}` may not be taken — an element is not a place a take reaches"),
                    vec![format!(
                        "`{root}.swapRemove(..)` returns the element and leaves the container \
                         one shorter"
                    )],
                ));
            }
            return Err(menu(line, by.nothing_to_take(), vec![by.drop_it()]));
        };
        if self.globals.contains(&root) && !Self::in_scope(scope, &root) {
            return Err(Diagnostic::error(
                line,
                0,
                "movecheck",
                format!(
                    "module state `{root}` may not be consumed by {} — \
                     nothing may take ownership of module state (it lives for the whole module \
                     and is never dropped)",
                    by.what()
                ),
            ));
        }
        if let Some(b) = self.borrow_of(&root) {
            return Err(menu(
                line,
                format!("`{root}` may not be consumed — it is {}", b.what(&root)),
                b.fixes(&root, &path),
            ));
        }
        Ok(())
    }

    /// Rule 2 at the third exit: **a borrow may not be consumed.**
    ///
    /// A store and a return are the two exits Phase 4b named. A move into a
    /// declared `consume` parameter is the third, and nothing asked this
    /// question there. `fn caller(ys: Array<Int64>) -> Int64 { return take(ys) }`
    /// handed a buffer the caller still owns to a frame that drops it: `vyrn
    /// check` said `ok`, the interpreter printed an answer by refcounting, and
    /// the native binary exited `0xC0000374`. It is PR #118's signature, one
    /// path over.
    ///
    /// RFC-0094 M1 put the two readings of `consume` on one page and called them
    /// an inconsistency. They are not: the builtin path goes through
    /// [`MoveCheck::store`] and is right, and this path is what was missing. So
    /// the question here is `store`'s question — a borrowed root, and RFC-0092's
    /// projection of any root — asked with the move left where it is, because a
    /// capability keeps the wording it has always had (see
    /// [`MoveCheck::check_use`]).
    ///
    /// **A linear type stands aside**, exactly as [`sinks`] stands aside for it.
    /// A `Stream<T>` carries its disposal obligation on the type and the
    /// [`linear`] walk refuses a second hand-over first; rule 1's menu offers
    /// `.copy()`, which a stream has no answer for.
    ///
    /// Module state spelled as a WHOLE keeps its own sentence one line later,
    /// by [`MoveCheck::reject_consume_global`]; spelled as a projection
    /// (`sink(g.title)`) it is refused here, because the callee would free a
    /// buffer the module reads again forever.
    ///
    /// **A field or element path of a place this frame OWNS is refused.** The
    /// constructor analogy this exit used to lean on does not hold here: a
    /// variant constructor RETAINS what it is given, so the caller stopping its
    /// releases costs a leak — safe; a `consume` parameter DISPOSES of what it
    /// is given, so the caller keeping its releases costs a double free (a
    /// record's release walks every place, so `d`'s block exit frees the very
    /// buffer the callee already freed). The language's answer for giving up a
    /// field is the prefix take (`consume d.title`), which [`check_take`] and
    /// [`MoveCheck::hole`] record so the release walk skips the place — the
    /// menu names it first. An ELEMENT has no take ([`check_take`] refuses
    /// one), so its menu offers `.copy()` and the `swapRemove` spelling.
    ///
    /// A name BOUND to a projection keeps its verdict when it is a match or
    /// `if let` binder over an owned scrutinee: the binder is keyed to the
    /// scrutinee's row and the arm records the hand-off, and `std/html`'s
    /// `keyed` drain depends on it. The same row on an ordinary `let`
    /// (`let t = d.title`) means the frame still owns the aggregate, so handing
    /// `t` to a `consume` parameter is refused like the spelling it hides.
    fn check_handover(&self, arg: &Expr, callee: &str, line: usize) -> Result<(), Diagnostic> {
        let Some(ty) = self.type_of(arg) else {
            return Ok(());
        };
        // A scalar copies, and a linear type is answered by its own obligation.
        if !self.decl.owns_heap(&ty) || self.decl.linear_kind(&ty).is_some() {
            return Ok(());
        }
        // An ELEMENT of a borrowed container is the same fact one level down:
        // `f(nodes[k])` on a `read` parameter hands the callee a buffer the
        // caller still holds. [`place_path`] answers `None` at the `@at(..)`
        // call, so `element_path` is what reaches it.
        let Some((root, path)) = place_path(arg).or_else(|| element_path(arg)) else {
            // An `if`/`match` expression yields one of its arm values, and
            // that value IS a projection of a place this frame may own —
            // `sink(if c { d.title } else { "" })` hands `d`'s field to a
            // `consume` parameter exactly as the direct spelling would. Each
            // arm answers the question its own spelling gets.
            return match arg {
                Expr::IfExpr {
                    then_branch,
                    else_branch,
                    ..
                } => {
                    self.check_handover(then_branch, callee, line)?;
                    match else_branch {
                        Some(b) => self.check_handover(b, callee, line),
                        None => Ok(()),
                    }
                }
                Expr::Match { arms, .. } => {
                    for a in arms {
                        self.check_handover(&a.body, callee, line)?;
                    }
                    Ok(())
                }
                _ => Ok(()),
            };
        };
        if path != root && self.is_module_state(&root) {
            return Err(menu(
                line,
                format!(
                    "`{path}` may not be passed to a `consume` parameter via `{}(..)` — \
                     it is module state, which nothing may take",
                    crate::parser::method_surface(callee)
                ),
                vec![format!(
                    "`{path}.copy()` — the callee releases what it is handed"
                )],
            ));
        }
        match self.borrow_of(&root) {
            // A whole place this frame owns is the move the caller records.
            None if path == root => Ok(()),
            None => Err(self.refuse_projected_arg(arg, callee, &root, &path, line)),
            // A binder over an owned scrutinee gave the payload up with the arm.
            Some(Borrow::Projection) if path == root && self.arm_binder(&root) => Ok(()),
            Some(Borrow::Projection) => {
                Err(self.refuse_projected_arg(arg, callee, &root, &path, line))
            }
            Some(b) => Err(menu(
                line,
                format!(
                    "`{path}` may not be passed to a `consume` parameter via `{}(..)` — it is {}",
                    crate::parser::method_surface(callee),
                    b.what(&path)
                ),
                self.fixes_here(&b, &root, &path),
            )),
        }
    }

    /// The refusal a projected argument to a `consume` parameter gets.
    ///
    /// An element has no take ([`check_take`] refuses one), so its menu names
    /// the two spellings that exist for it instead of a prefix take.
    fn refuse_projected_arg(
        &self,
        arg: &Expr,
        callee: &str,
        root: &str,
        path: &str,
        line: usize,
    ) -> Diagnostic {
        let fixes = if place_path(arg).is_none() {
            vec![
                format!("`{path}.copy()` — the callee owns its copy"),
                format!(
                    "`{root}.swapRemove(..)` returns the element and leaves the container \
                     one shorter"
                ),
            ]
        } else {
            self.fixes_here(&Borrow::Projection, root, path)
        };
        menu(
            line,
            format!(
                "`{path}` may not be passed to a `consume` parameter via `{}(..)` — it is {}",
                crate::parser::method_surface(callee),
                Borrow::Projection.what(path)
            ),
            fixes,
        )
    }

    /// Whether `name` is a pattern binder of the innermost open arm.
    fn arm_binder(&self, name: &str) -> bool {
        self.arm_binders
            .borrow()
            .last()
            .is_some_and(|s| s.contains(name))
    }

    /// Whether the callee may KEEP a `fn` value passed as argument `i` — the
    /// question a lambda written at a call argument has to answer, because a
    /// kept closure outlives the call and its captures leave the frame with it.
    ///
    /// A declared capability answers for a program function, a seeded row for a
    /// builtin. Facts unknown (no declaration, no row) answer YES: the safe
    /// direction is refusing an author once, never dangling a capture.
    fn callee_keeps(&self, callee: &str, i: usize) -> bool {
        match self.caps.get(callee).and_then(|c| c.get(i)) {
            Some(cap) => *cap == Capability::Consume,
            None => {
                crate::prelude::capability(callee, i).map_or(true, |cap| cap == Capability::Consume)
            }
        }
    }

    /// Rule 3: a function returns an owned value, always.
    ///
    /// It looks THROUGH a `match` and an if-expression, because each of them
    /// yields one of its arms and an arm can be a place. Phase 4c found the hole:
    /// `fn text(h: Html) -> String { return match h { Text(s) => s, .. } }` names
    /// no place at the `return`, so the check passed, and the caller then owned —
    /// and freed — a payload the enum still held.
    /// **The reach through the arms is narrower than the rule**, on purpose.
    /// A place named directly at the `return` is refused for every type that
    /// owns heap, exactly as Phase 4b left it. An arm is refused only where the
    /// caller RELEASES the result — a `String`, an `Array`, a `Map`, a cell, or a
    /// declared `Owned` type. That is where the hole costs a use-after-free.
    ///
    /// **RFC-0092 M1 widened it to a projection.** Phase 4b stopped at a borrowed
    /// PARAMETER because the named fix is `.copy()`, which `Html` and `Json` could
    /// not answer: a type that refers to itself had no structural copy (RFC-0089
    /// M1b). RFC-0091 M1's `Copy` protocol answers it, so `return d.title` out of
    /// a record this frame owns is refused now, and so is a projection or a
    /// pattern binder yielded by a `match` arm.
    fn check_return(&self, e: &Expr, line: usize) -> Result<(), Diagnostic> {
        self.note_returned_projection(e, line);
        if !self.decl.owns_heap(&self.ret.borrow()) {
            return Ok(());
        }
        // A place named straight at the `return` keeps Phase 4b's rule whole:
        // every borrow is refused, whatever kind it is.
        if let Some((root, path)) = place_path(e) {
            if let Some(b) = self.borrow_of(&root) {
                return Err(self.refuse_return(&b, &root, &path, line));
            }
            // Module state is not a borrow, and Phase 6 found that this is where
            // that costs a use-after-free. A global lives for the whole module
            // and nothing may take it (RFC-0013), so `return title` hands the
            // caller a buffer the module still holds — and rule 3 makes the
            // caller free it. `examples/` never wrote it, so parity never saw it:
            // the interpreter's values cannot dangle and the wasm allocator
            // handed the block straight back out.
            //
            // This one does NOT go through [`MoveCheck::refuse_return`], and the
            // reason is that it is a different FACT rather than a different
            // caller: "nothing may take module state" is true of a Vyrn caller
            // and a JS caller alike, and its menu already names `.copy()` alone.
            // Routing it through the shared exit would replace a true sentence
            // with a vaguer one.
            if self.decl.releases(&self.ret.borrow()) && self.is_module_state(&root) {
                return Err(menu(
                    line,
                    format!(
                        "`{path}` may not be returned — it is module state, which nothing \
                         may take, and a return is owned"
                    ),
                    vec![format!(
                        "`{path}.copy()` — the caller releases what it is handed"
                    )],
                ));
            }
            // RFC-0092's rule at the return. `return d.title` out of a record
            // this frame owns hands the caller a buffer `d` still holds, and rule
            // 3 makes the caller free it. `borrow_of` answers `None` for an owned
            // local, so this shape reached no reading at all — not refused, and
            // not even recorded as a lend.
            if path != root {
                return Err(self.refuse_return(&Borrow::Projection, &root, &path, line));
            }
            return Ok(());
        }
        if !self.decl.releases(&self.ret.borrow()) {
            return Ok(());
        }
        let found = self.returned_borrow(e);
        let wrapped = self.lends_through_a_wrapper(e);
        // Whatever it is, the caller may not release it. Recorded even where it
        // is not refused below — that is the half of the hole this phase closes
        // without a diagnostic.
        if found.is_some() || wrapped.is_some() {
            self.lends();
        }
        let Some((b, root, path)) = found else {
            // The census's finding 2, refused: `fn wrap(s: String) -> Option<String>
            // { return Some(s) }` puts a borrow into the value it hands back, so
            // the caller may release NEITHER — not the argument, which escapes
            // into the constructor, nor the result, whose producer does not own
            // it. Both leak, 48.8 MB over a million turns, and no release rule
            // may ever close it: freeing around a lend is the alias analysis this
            // repo deleted. The fix is the signature, which is the language's own
            // answer since RFC-0089 — a capability is declared, not inferred.
            //
            // The narrowing is what makes it safe to refuse. This wrapper reading
            // exists to RECORD, because `return Some(m)` over a loop element is
            // most of the corpus, and an element is a [`Borrow::Element`] or a
            // [`Borrow::Projection`]. A whole `read` parameter is the one root
            // whose owner is the CALLER, so `consume` is a fix the author can
            // always write, and it is the shape the census measured.
            if let Some((b, root, path)) = wrapped {
                if root == path
                    && matches!(b, Borrow::Read(_) | Borrow::Modify(_))
                    && self.param_ix.borrow().contains_key(&root)
                {
                    return Err(menu(
                        line,
                        format!(
                            "`{path}` may not be put into the value this function returns — \
                             it is {}, and a return is owned",
                            b.what(&path)
                        ),
                        b.fixes(&root, &path),
                    ));
                }
            }
            return Ok(());
        };
        // An export refuses every kind, so it is asked before the narrowing.
        if self.exported.contains(&*self.cur_fn.borrow()) {
            return Err(self.refuse_return(&b, &root, &path, line));
        }
        // A borrowed PARAMETER and, since RFC-0092 M1, a PROJECTION. The
        // parameter is the class Phase 4b missed: 4b read `return p` as a
        // statement, and these return a parameter from inside a `match` arm,
        // which is an expression. The projection is the other half — `numText(j)`
        // hands back the enum's own text — and 4b left it recorded as a lend
        // because the named fix is `.copy()`, which `Json` and `Html` could not
        // answer. RFC-0091 M1's `Copy` protocol answers it.
        //
        // A `for` variable ([`Borrow::Element`]) is NOT widened here. It is a
        // projection of its container in kind, but it is outside this RFC's three
        // sites and outside the count that priced them, so it keeps 4b's verdict.
        if !matches!(b, Borrow::Read(_) | Borrow::Modify(_) | Borrow::Projection) {
            return Ok(());
        }
        Err(self.refuse_return(&b, &root, &path, line))
    }

    /// The refusal a returned borrow gets — **the one exit for all three of
    /// them**.
    ///
    /// [`MoveCheck::check_return`] refuses a return from three different places:
    /// a place named straight at the `return` whose root is a borrow (`return
    /// q`), a projection of a place the frame owns (`return d.title`), and a
    /// borrow yielded by a `match` or `if` arm (`return match t { W(s) => s }`).
    /// They are separate because each answers a different question first, and
    /// merging them would merge those questions.
    ///
    /// What they must NOT differ about is the answer, and they did. The
    /// `exported` check lived at the arm exit alone, so an `export extern fn`
    /// that returned a `read` parameter directly was handed the general menu and
    /// told to ``declare the parameter `q: consume ..` `` — which its own
    /// signature then refuses: *"the caller across this boundary is JS, and it
    /// releases the String when the call returns"* (RFC-0089 M3b). One program
    /// spelled two ways got two menus, one of which sent the reader to a second
    /// error. Fixing that at one exit fixed two spellings and missed the third,
    /// twice, which is what this function exists to stop.
    fn refuse_return(&self, b: &Borrow, root: &str, path: &str, line: usize) -> Diagnostic {
        // A Vyrn caller reads the lend out of `lending` and releases nothing; a
        // JS caller reads nothing, and since RFC-0089 M3b `wasi-min.js` frees
        // every String an export hands back. So an export owns its result or it
        // does not compile, and `.copy()` is the only fix that exists for it.
        if self.exported.contains(&*self.cur_fn.borrow()) {
            return menu(
                line,
                format!(
                    "`{path}` may not be returned from an exported function — it is {}, \
                     and the JS caller releases what it is handed",
                    b.what(path)
                ),
                vec![format!(
                    "`{path}.copy()` — an `export extern fn` owns its result"
                )],
            );
        }
        menu(
            line,
            format!(
                "`{path}` may not be returned — it is {}, and a return is owned",
                b.what(path)
            ),
            b.fixes(root, path),
        )
    }

    /// Note that argument `i` of `callee` was handed a place.
    ///
    /// Two records, both read after every body has been walked: a LOCAL passed
    /// here must not be released if `(callee, i)` turns out to keep what it is
    /// given, and a PARAMETER passed here makes this function keep it in turn.
    fn note_handover(&self, arg: &Expr, callee: &str, i: usize, line: usize) {
        let Some(sink) = &self.lets else { return };
        let Some((root, _)) = place_path(arg) else {
            return;
        };
        if let Some(&ix) = self.param_ix.borrow().get(&root) {
            if let Some(edges) = &self.handed_on {
                edges
                    .borrow_mut()
                    .entry((callee.to_string(), i))
                    .or_default()
                    .push((self.cur_fn.borrow().clone(), ix));
            }
        }
        let key = self.nodes.borrow().get(&root).copied().unwrap_or(0);
        if key != 0 {
            if let Some(row) = sink.borrow_mut().get_mut(&key) {
                row.passed.push((callee.to_string(), i, line));
            }
        }
    }

    /// Record an argument whose own expression BUILT the value it hands over —
    /// the census's shape A and shape B (`rfcs/census-call-arguments.md` §1).
    ///
    /// The verdict is not taken here. What is taken here is the TYPE, because
    /// the scope stack that answers it only exists during the walk. The reading
    /// is [`Declared::type_of`] and not this pass's widened
    /// [`MoveCheck::type_of`], for the reason that method's own comment gives: a
    /// type answered there that was not answered before changes what a program
    /// FREES, and this decides a free.
    fn note_arg_temp(&self, arg: &Expr, callee: &str, ix: usize, line: usize) {
        let Some(sink) = &self.arg_temps else { return };
        let producer = match arg {
            // A constructor's payload is the constructed value's, and a lending
            // builtin hands back a place inside its receiver. Neither BUILT
            // anything the caller may release.
            Expr::Call { name, .. } if views(name) || self.decl.constructs(name) => return,
            Expr::Call { name, .. } => Some(name.clone()),
            // The one allocating operator. `+` is also integer addition, so the
            // TYPE is what tells them apart — the check `own::str_temporary`'s
            // doc comment demands of every caller.
            Expr::Binary { op: BinOp::Add, .. } => None,
            _ => return,
        };
        let Some(ty) = self.decl.type_of(&self.vars.borrow(), arg) else {
            return;
        };
        if producer.is_none() && !matches!(crate::types::resolve(&ty, self.decl.decls()), Type::Str)
        {
            return;
        }
        let Some(kind) = self.decl.release_kind(&ty) else {
            return;
        };
        sink.borrow_mut().push(ArgTemp {
            id: arg as *const Expr as usize,
            callee: callee.to_string(),
            ix,
            line,
            module: None,
            producer,
            kind,
            // Overwritten in `run` once the retention set is closed.
            verdict: ArgVerdict::Unknown,
        });
    }

    /// Whether this `+` builds a **String** — the one shape of the operator that
    /// allocates, and so the one whose operands are `@concat`'s arguments.
    ///
    /// `+` is also integer addition and `Code` concatenation, and the type is
    /// what tells the three apart — the check [`crate::own::str_temporary`]'s
    /// doc comment demands of every caller. The reading is
    /// [`Declared::type_of`], which answers a `+` with its left operand's type.
    fn concatenates(&self, e: &Expr) -> bool {
        self.decl
            .type_of(&self.vars.borrow(), e)
            .is_some_and(|t| matches!(crate::types::resolve(&t, self.decl.decls()), Type::Str))
    }

    /// An arm of an if-expression or a `match` can yield a PLACE, and the value
    /// then has two names: the arm's and whatever the expression is bound to,
    /// stored into or returned as.
    ///
    /// `let rel = if prefix == "" { st } else { prefix + "/" + st }` is the shape,
    /// out of `std/rpc`'s scanner. Both `rel` and `st` named one buffer and both
    /// were released, and the generator running as wasm then built its stub names
    /// out of reused memory. Neither name may be released, exactly as for the
    /// bare `let d = c` alias.
    /// A `match` is not walked here. Its arms bind PAYLOAD names, and this walk
    /// runs with those names out of scope — `Some(v) => v` would ask what `v`
    /// meant in the enclosing block, which is either nothing or the wrong
    /// binding. [`MoveCheck::expr`]'s `Expr::Match` arm asks the same question
    /// one scope in, where the binders are bound and keyed to the scrutinee's
    /// row, and that is the only place it can be asked correctly.
    fn note_arm_aliases(&self, e: &Expr, line: usize, binders: &[String]) {
        if self.lets.is_none() {
            return;
        }
        let Expr::IfExpr {
            then_branch,
            else_branch,
            ..
        } = e
        else {
            return;
        };
        self.note_arm_value(then_branch, line, binders);
        if let Some(b) = else_branch {
            self.note_arm_value(b, line, binders);
        }
    }

    /// One arm's VALUE, and what naming a place in it costs.
    ///
    /// A whole place yielded by an arm is the alias above: two names, one
    /// buffer, and only the name the expression is bound to may release it.
    ///
    /// A place rooted at one of this arm's own `binders` is the same fact about
    /// the SCRUTINEE, because a binder is keyed to the scrutinee's row — so
    /// `Some(v) => v` and `Ok(d) => d.title` both say the scrutinee gave its
    /// payload up, and nothing may release the scrutinee afterwards. The path
    /// does not have to be the whole root for a binder, and it does for anything
    /// else: `d.title` out of an outer record leaves the record's own release
    /// alone, which is what a projection means.
    fn note_arm_value(&self, a: &Expr, line: usize, binders: &[String]) {
        self.note_arm_aliases(a, line, binders);
        let Some((root, path)) = place_path(a).or_else(|| element_path(a)) else {
            return;
        };
        if !self.arm_carries_heap(a) {
            return;
        }
        if root == path || binders.iter().any(|b| *b == root) {
            self.took(&root, Gone::Aliased { line });
        }
    }

    /// Whether an arm's value can carry HEAP out of the arm.
    ///
    /// `Ok(s) => s.byteLength` names a place and hands out a number, and reading
    /// that as a handover left the scrutinee unreleased — the leak this rule
    /// exists to close, closed by the rule itself.
    ///
    /// The type answers wherever it can be named. Where it cannot, a field read
    /// whose BASE is not a record is a builtin scalar projection (`byteLength`,
    /// `length`, `charCount`) and carries nothing; everything else is unknown
    /// and answers yes, because a leak is the direction this analysis fails in.
    fn arm_carries_heap(&self, a: &Expr) -> bool {
        if let Some(t) = self.type_of(a) {
            return self.decl.owns_heap(&t);
        }
        match a {
            Expr::Field { expr, .. } => match self.type_of(expr) {
                Some(t) => matches!(
                    crate::types::resolve(&t, self.decl.decls()),
                    Type::Record(_)
                ),
                None => true,
            },
            _ => true,
        }
    }

    /// Whether `e`'s VALUE provably cannot alias `root`'s heap (RFC-0114
    /// Rule N's edge guard), structurally: an operator result is a scalar or a
    /// fresh allocation; a scalar projection carries nothing; a call cannot
    /// return heap it was never handed, so its result is safe when every
    /// argument is — a lending function can only lend what it was passed, and
    /// a return of module state or a projection is refused elsewhere. A
    /// closure capturing the binding is `Gone::Captured`, which the fold's
    /// veto refuses before this is asked. Anything unproven answers false,
    /// which is the leak direction.
    fn value_cannot_alias(&self, e: &Expr, root: &str) -> bool {
        if !mentions_place(e, root) {
            return true;
        }
        match e {
            Expr::Binary { .. } | Expr::Unary { .. } => true,
            Expr::Field { .. } => !self.arm_carries_heap(e),
            Expr::Call { args, .. } => args.iter().all(|a| self.value_cannot_alias(a, root)),
            _ => false,
        }
    }

    /// Record that a borrowed PARAMETER was put somewhere that outlives the call.
    fn note_retention(&self, e: &Expr) {
        let Some(sink) = &self.retains else { return };
        let Some((root, _)) = place_path(e) else {
            return;
        };
        if !matches!(
            self.borrow_of(&root),
            Some(Borrow::Read(_)) | Some(Borrow::Modify(_))
        ) {
            return;
        }
        if let Some(&ix) = self.param_ix.borrow().get(&root) {
            sink.borrow_mut().insert((self.cur_fn.borrow().clone(), ix));
        }
    }

    /// Record the function under check as one whose result the caller must not
    /// release, and note the plain `return f(..)` callees that pass it along.
    fn lends(&self) {
        if let Some(sink) = &self.lending {
            sink.borrow_mut().insert(self.cur_fn.borrow().clone());
        }
    }

    /// A borrow WRAPPED into a local — the lend `check_return` cannot see.
    ///
    /// [`MoveCheck::lends_through_a_wrapper`] reads the returned expression, and
    /// `std/html`'s `attrKey` does not return one: it writes `found = match a {
    /// Key(k) => Some(k), .. }` in a loop and returns the local. `found` is a
    /// plain name at the `return`, and `borrow_of` answers `None` for it, so
    /// nothing recorded that this function hands back an element of its
    /// argument's array — and the caller then released one. Reachable without a
    /// `match` scrutinee anywhere: `let got = attrKey(xs)` frees the element
    /// natively on `310753c`.
    ///
    /// Recording, never refusing, exactly as the wrapper reading is: refusing
    /// `found = match a { Key(k) => Some(k) }` would refuse most of `std/html`,
    /// and a function marked a lender only ever STOPS a release.
    fn note_wrapped_lend(&self, value: &Expr) {
        if self.lending.is_some()
            && self.decl.releases(&self.ret.borrow())
            && self.lends_through_a_wrapper(value).is_some()
        {
            self.lends();
        }
    }

    /// The first borrow a returned expression yields, looking through the forms
    /// that yield one of their arms.
    ///
    /// It carries RFC-0092's leaf: **a projection is a borrow of its root
    /// whatever the root is**, so `d.title` out of a locally built record is a
    /// borrow too, and so is `items[i]`. Before M1 this walk asked
    /// `borrow_of(&root)?`, and `borrow_of` answers `None` for an owned local, so
    /// `?` dropped the whole projection — the hole the RFC names. M0 read the
    /// leaf behind a flag to count it; M1 is that flag deleted.
    fn returned_borrow(&self, e: &Expr) -> Option<(Borrow, String, String)> {
        match e {
            Expr::Match {
                scrutinee, arms, ..
            } => {
                let mut found = None;
                for arm in arms {
                    let (tys, borrow) = self.payload_binding(scrutinee, &arm.pattern);
                    self.enter();
                    for (i, b) in pattern_bindings(&arm.pattern).into_iter().enumerate() {
                        self.bind(b, tys.get(i).cloned().flatten(), borrow.clone());
                    }
                    let r = self.returned_borrow(&arm.body);
                    self.exit();
                    if r.is_some() {
                        found = r;
                        break;
                    }
                }
                found
            }
            Expr::IfExpr {
                then_branch,
                else_branch,
                ..
            } => self
                .returned_borrow(then_branch)
                .or_else(|| else_branch.as_ref().and_then(|b| self.returned_borrow(b))),
            _ => {
                let Some((root, path)) = place_path(e) else {
                    // An element read is a projection too, and `place_path`
                    // answers `None` for the `@at(..)` call it lowers to.
                    let (root, path) = element_path(e)?;
                    return Some((Borrow::Projection, root, path));
                };
                match self.borrow_of(&root) {
                    Some(b) => Some((b, root, path)),
                    None if path != root => Some((Borrow::Projection, root, path)),
                    None => None,
                }
            }
        }
    }

    /// RFC-0092: record a returned projection, for the instrument.
    ///
    /// Two shapes reach here: a place named straight at the `return` whose root
    /// the frame OWNS (`return d.title`), and a projection or pattern binder
    /// yielded by a `match`/`if` arm. Before M1 the first was invisible to
    /// `check_return` and the second was recorded as a lend and waved through. M1
    /// refuses both, a few lines below this call.
    ///
    /// A root that is already a borrow was refused before this RFC (rule 3, Phase
    /// 4b), so it is not part of this bill.
    fn note_returned_projection(&self, e: &Expr, line: usize) {
        if self.projections.is_none() {
            return;
        }
        if let Some((root, _)) = place_path(e) {
            if self.borrow_of(&root).is_some() {
                return;
            }
        }
        let Some((b, _, path)) = self.returned_borrow(e) else {
            return;
        };
        if b != Borrow::Projection {
            return;
        }
        let ret = self.ret.borrow().clone();
        let kind = if path.ends_with("[..]") {
            "elem-return"
        } else {
            "return"
        };
        self.note_projection(kind, &path, ret.to_string(), Some(ret), line);
    }

    /// Push one [`ProjectionSite`]. The module is stamped by [`run`], which is
    /// the only place that knows which body is being read.
    fn note_projection(
        &self,
        kind: &'static str,
        path: &str,
        into: String,
        ty: Option<Type>,
        line: usize,
    ) {
        let Some(sink) = &self.projections else {
            return;
        };
        sink.borrow_mut().push(ProjectionSite {
            kind,
            module: None,
            func: self.cur_fn.borrow().clone(),
            line,
            path: path.to_string(),
            into,
            ty: ty.as_ref().map_or("?".to_string(), |t| t.to_string()),
            owns_heap: ty.is_some_and(|t| self.decl.owns_heap(&t)),
        });
    }

    /// The same question, looking THROUGH a constructor and a struct literal —
    /// used to RECORD a lend and never to refuse one (Phase 10a).
    ///
    /// `openRule(c)` is `for m in c.members { return Some(m) }`: a projection of
    /// a `read` parameter, wrapped. Phase 5 recorded exactly this shape as the
    /// one nothing could see — "`returned_borrow` reads a returned PLACE, and a
    /// struct literal is not one" — and Phase 10a paid for it the moment it
    /// released an `if let` scrutinee: `std/contract` read freed members and the
    /// `components` generator emitted a mangled spelling.
    ///
    /// It is separate from [`MoveCheck::returned_borrow`] because refusing here
    /// would refuse `return Some(m)` over any loop element, which is most of the
    /// corpus. Recording is the whole job: a lender's result is the one thing
    /// this analysis never releases, so this can only stop a free, never cause
    /// one.
    fn lends_through_a_wrapper(&self, e: &Expr) -> Option<(Borrow, String, String)> {
        match e {
            Expr::Call { name, args, .. } if self.decl.constructs(name) => {
                args.iter().find_map(|a| self.returned_borrow(a))
            }
            Expr::StructLit { fields, .. } => {
                fields.iter().find_map(|(_, v)| self.returned_borrow(v))
            }
            Expr::IfExpr {
                then_branch,
                else_branch,
                ..
            } => self.lends_through_a_wrapper(then_branch).or_else(|| {
                else_branch
                    .as_ref()
                    .and_then(|b| self.lends_through_a_wrapper(b))
            }),
            Expr::Match {
                scrutinee, arms, ..
            } => arms.iter().find_map(|arm| {
                let (tys, borrow) = self.payload_binding(scrutinee, &arm.pattern);
                self.enter();
                for (i, b) in pattern_bindings(&arm.pattern).into_iter().enumerate() {
                    self.bind(b, tys.get(i).cloned().flatten(), borrow.clone());
                }
                let r = self.lends_through_a_wrapper(&arm.body);
                self.exit();
                r
            }),
            _ => None,
        }
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
    ) -> Result<bool, Diagnostic> {
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
                // The `a[i].f = v` desugar's ELEMENT TEMP is exempt from rule 2:
                // the place is read out, mutated, and written straight back to
                // where it came from, so the round trip is not a store of a
                // borrow — the parser built both halves and there is no second
                // owner. [`is_place_temp`] is what says which: a round-trip temp
                // ends in `[]`.
                //
                // The test used to be `name.contains('[')`, which also caught
                // `ps[]val` and `ps[]idx` — the operands RFC-0082 M2 hoists so
                // they run before the move-out. Those are arbitrary expressions,
                // and `[]val` is the statement's right-hand side, so the exemption
                // made `ps[1].xs = ps[0].xs` an unchecked store of a projection:
                // two elements naming one buffer. It leaked while nothing released
                // an element and corrupts the heap now that RFC-0092 M3 does
                // (`examples/placeorder.vyrn`, native exit `0xC0000374`).
                let borrow = if crate::ast::is_place_temp(name) {
                    None
                } else {
                    self.borrow_from(value)
                };
                // A projection is a borrow rather than a move, so only a whole
                // place moves here — `store` decides which.
                //
                // A `let` does not outlive itself, so rule 2 does not fire on
                // one. RFC-0082 M2's hoisted VALUE temp is the exception: it is
                // the right-hand side of a place assignment, lifted out so it
                // runs before the move-out, so it outlives exactly as the store
                // it feeds does. Refusing it HERE rather than at that store is
                // what makes the diagnostic name `ps[0].xs`, which the reader
                // wrote, instead of `ps[]val`, which the parser minted.
                let hoisted = name.ends_with("[]val");
                let moved = self.store(
                    value,
                    &|| {
                        if hoisted {
                            format!(
                                "`{}`",
                                name.trim_end_matches("[]val").trim_end_matches("[]")
                            )
                        } else {
                            format!("the binding `{name}`")
                        }
                    },
                    *line,
                    hoisted,
                    consumed,
                )?;
                self.note_wrapped_lend(value);
                // Phase 4c: the row this binding is reclaimed by. Written BEFORE
                // the binding enters scope, so `let s = s + "x"` records the new
                // `s` and the move of the old one lands on the old row.
                let mut place = self.names_a_place(value);
                // A whole place that did NOT move is an alias: rule 1 leaves the
                // type alone, so this name did not take it. Neither name may be
                // released, or one value is released twice.
                if place.is_none() && !moved {
                    if let Some((root, path)) = place_path(value) {
                        if root == path {
                            self.took(&root, Gone::Aliased { line: *line });
                            place = Some("a second name for a value it did not take");
                        }
                    }
                }
                if let Some(sink) = &self.lets {
                    sink.borrow_mut().insert(
                        let_id(s),
                        LetOwnership {
                            ty: bty.clone(),
                            gone: place.map(Gone::Borrowed),
                            from_call: match value {
                                Expr::Call { name, .. } => Some(name.clone()),
                                _ => None,
                            },
                            passed: Vec::new(),
                        },
                    );
                }
                self.bind(name, bty, borrow);
                self.nodes.borrow_mut().bind(name, let_id(s));
                // RFC-0114 M2: the initializer is the binding's first write.
                // `id` 0 because a `let` is never itself a release site — it is
                // only the store before the first assign.
                self.store_ev(
                    let_id(s),
                    EvKind::Write {
                        id: 0,
                        owning: place.is_none(),
                    },
                );
                revive(consumed, name); // a fresh binding is alive again
                scope.last_mut().unwrap().insert(name.clone());
                Ok(false)
            }
            Stmt::Assign { name, value, line } => {
                self.expr(value, consumed, scope)?;
                // RFC-0114 M2: the write event, BEFORE the store's own effects
                // (`took(Borrowed)`, revive) so the fold sees the state the
                // store finds. A global is recorded in its own set — module
                // state is owned unconditionally.
                {
                    let key = self.nodes.borrow().get(name).copied().unwrap_or(0);
                    let sid = s as *const Stmt as usize;
                    if key != 0 {
                        self.store_ev(
                            key,
                            EvKind::Write {
                                id: sid,
                                owning: self.names_a_place(value).is_none(),
                            },
                        );
                    } else if self.globals.contains(name) && !Self::in_scope(scope, name) {
                        if let Some(g) = &self.global_stores {
                            g.borrow_mut().insert(sid);
                        }
                    }
                }
                // Module state (RFC-0013) is a place with a whole-module lifetime,
                // so 4b treats a store into it differently from a local's.
                let global = self.globals.contains(name) && !Self::in_scope(scope, name);
                self.site(
                    if global { "assign-global" } else { "assign" },
                    *line,
                    value,
                    None,
                );
                let into = || {
                    if global {
                        format!("module state `{name}`")
                    } else {
                        format!("`{name}`")
                    }
                };
                let _ = self.store(value, &into, *line, global, consumed)?;
                // An assignment rebinds, exactly as a `let` does, so it must
                // carry the same answer: `t = d.title` makes `t` a projection of
                // `d`. Without this, `let t = d.title` was refused at the next
                // store and `let mut t = "" ; t = d.title` was not — RFC-0092's
                // two-spellings-two-verdicts defect, one statement over.
                self.note_wrapped_lend(value);
                if !global {
                    let b = self.borrow_from(value);
                    // And it must carry the RECLAMATION answer too, which is the
                    // other half of the same sentence. `let t = d.title` gets
                    // `names_a_place` and is therefore never released; `t =
                    // d.title` got nothing, so the block freed a buffer the
                    // record still holds and releases again — `out = v` inside
                    // `if let Some(v) = o` is the same store one keyword over.
                    // One question, asked at both spellings.
                    if let Some(why) = self.names_a_place(value) {
                        self.took(name, Gone::Borrowed(why));
                    }
                    self.borrows.borrow_mut().rebind(name, b);
                }
                revive(consumed, name); // reassignment revives it
                self.wrote_into(name); // RFC-0093 M2: a filled hole is not skippable
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
                let _ = self.store(
                    value,
                    &|| format!("the field `{name}.{field}`"),
                    *line,
                    true,
                    consumed,
                )?;
                // RFC-0093: a write fills the hole a take left. The same
                // sentence `Stmt::Assign` has carried since Phase 4b, one dot
                // down — and the reason no drop flag is needed to say it.
                revive(consumed, &format!("{name}.{field}"));
                self.wrote_into(name); // RFC-0093 M2: a filled hole is not skippable
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
                let _ = self.store(value, &|| format!("`{name}`"), *line, true, consumed)?;
                // A map takes its KEY. Both backends write the key pointer into
                // `keys[len]` and copy nothing, so `hs[k] = v` moves `k` — and
                // no rule said so until RFC-0092 M5 needed it to. `httpHeaders`
                // (`std/http`) is `for k in base.keys() { hs[k] = .. }`, and a
                // loop that released its snapshot would hand back a key the map
                // still holds.
                //
                // Recorded AFTER the value, because the value may read the key.
                //
                // `outlives` is TRUE — the rule RFC-0092 M5 named and deferred.
                // With it FALSE the key was the ONE store in the language that
                // did not ask rule 2, and the map literal one arm over asked it:
                // `[ks[0]: 1]` was refused and `m[ks[0]] = 1` was taken, from the
                // same borrow, in the same function. The second spelling wrote
                // the array's own buffer into the map's key slot, so the array
                // and the map both released it — the interpreter answered and
                // the native binary died with no output. One fact, one verdict:
                // a borrowed key is refused here exactly as it is there, with
                // `.copy()` offered as the fix.
                //
                // A key that is nobody's borrow is unaffected. `httpHeaders`
                // (`std/http`) is `for k in base.keys() { hs[k] = .. }`, and the
                // snapshot is a temporary the loop owns (M5), so `k` binds an
                // OWNED element and the store still records the move.
                let _ = self.store(index, &|| format!("`{name}`"), *line, true, consumed)?;
                self.wrote_into(name); // RFC-0093 M2: a filled hole is not skippable
                Ok(false)
            }
            Stmt::Return { value, line } => {
                if let Some(e) = value {
                    self.site("return", *line, e, None);
                    self.expr(e, consumed, scope)?;
                    self.check_return(e, *line)?;
                    // Rule 3: the caller owns the result. Everything the returned
                    // expression reads may be inside it, so this block releases
                    // none of it. Only a heap return type can carry anything out.
                    if self.decl.owns_heap(&self.ret.borrow()) {
                        self.gave_up(e, &Gone::Returned { line: *line });
                    }
                    // A result handed straight back is lent if what it names is.
                    if self.decl.releases(&self.ret.borrow()) {
                        if let Some(sink) = &self.forwards {
                            let mut names = Vec::new();
                            calls_in(e, &mut names);
                            sink.borrow_mut()
                                .entry(self.cur_fn.borrow().clone())
                                .or_default()
                                .extend(names);
                        }
                    }
                }
                Ok(true)
            }
            // `break`/`continue` (RFC-0060) consume nothing but terminate the
            // path — code after them in the same block is unreachable.
            Stmt::Break { .. } => Ok(true),
            // A `continue` also diverges here, but it jumps to the NEXT
            // iteration: the loop body re-runs. Marking it is what stops the
            // loop arms from counting it as the "runs at most once"
            // divergence that skips the next-iteration reuse check.
            Stmt::Continue { .. } => {
                self.continue_seen.set(true);
                Ok(true)
            }
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
                // RFC-0114 Rule N: a binding consumed on exactly one branch,
                // both branches continuing to the join. The union below will say
                // "consumed", so nothing after the `if` may read it (that is A1)
                // and block exit will not release it — the edge that did NOT
                // consume is the one place the value can still be given back.
                // Whole bindings only, taken clean (no hole), untouched before
                // the `if` and untouched on the other branch — a projection
                // take, or any prior activity in the binding's bucket, refuses.
                if !then_div && !else_div {
                    if let Some(sink) = &self.edge_releases {
                        let if_key = s as *const Stmt as usize;
                        for (taker, other, edge) in
                            [(&then_c, &else_c, 1u32), (&else_c, &then_c, 0u32)]
                        {
                            for (root, bucket) in &taker.0 {
                                if consumed.0.contains_key(root)
                                    || other.0.contains_key(root)
                                    || bucket.len() != 1
                                {
                                    continue;
                                }
                                let Some(c) = bucket.get(root) else { continue };
                                if c.hole {
                                    continue;
                                }
                                let key = self.nodes.borrow().get(root).copied().unwrap_or(0);
                                if key == 0 {
                                    continue;
                                }
                                sink.borrow_mut().push(EdgeRel {
                                    if_key,
                                    key,
                                    name: root.clone(),
                                    edge,
                                });
                            }
                        }
                    }
                }
                // may-consume, but a branch that DIVERGES (break/continue/return)
                // carries its consumptions out the exit path, not to the code
                // after the `if` — so a value moved only on a break-path is not
                // considered moved on the fall-through (RFC-0060).
                if !then_div {
                    for (k, v) in then_c {
                        consumed.or_insert(k, v);
                    }
                }
                if !else_div {
                    for (k, v) in else_c {
                        consumed.or_insert(k, v);
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
                // Census §14: the scrutinee of an `if let` over a FRESH value is
                // a binding of its own — `if let Some(s) = f()` matches a heap
                // value with no name, and until Phase 10a nothing released it.
                //
                // Phase 5 built the release and took it out again, because the
                // payload escapes the arm as a projection or through a call and
                // neither was recorded against the scrutinee. **Recording it is
                // the whole fix**, and the record is the one every `let` already
                // gets: the statement's own node address is the key, the binders
                // are bound to it, and every `took` in this pass then writes the
                // arm's escape onto that row. A row with `gone: None` at the end
                // is a value nothing took, and `own.rs` releases exactly those.
                //
                // A binder over a PLACE keeps Phase 5's answer: it is keyed to
                // that place's row, so returning one gives the place up.
                //
                // `place_key` answers 0 for two different things, and Phase 10a
                // read them as one: an expression that names no place, and a
                // place with no `let` row — a PARAMETER (bound to node 0 above),
                // module state, a field read, an element. Minting a row for the
                // second kind releases somebody else's value. `showOpt(name, v)`
                // in `examples/argsdemo.vyrn` is `if let Some(s) = v` over a
                // plain `Option<String>` parameter, and it freed the `args()`
                // element `opt` had lent it; the next `opt` call then read that
                // freed String's header, and CI's three-way parity went red for
                // twenty-four runs. Windows hid it — glibc writes the tcache
                // link over the String header and the Windows allocator does
                // not, so only the Linux job ever read the corrupt length.
                //
                // `names_a_place` is the question a `let` already asks before it
                // is given a reclamation row, and it is the same question here:
                // a borrow, module state, a field read or a view builtin owns
                // nothing this block may release.
                let key = match self.place_key(scrutinee) {
                    0 if self.names_a_place(scrutinee).is_none() => {
                        self.note_temporary(s, scrutinee)
                    }
                    k => k,
                };
                for (i, b) in pattern_bindings(pattern).into_iter().enumerate() {
                    scope.last_mut().unwrap().insert(b.to_string());
                    // Recording it is the point: an unrecorded binder falls
                    // through to whatever the enclosing scope calls that name
                    // (`own.rs`'s shadowing lesson).
                    self.bind(b, tys.get(i).cloned().flatten(), borrow.clone());
                    if key != 0 {
                        self.nodes.borrow_mut().bind(b, key);
                    }
                }
                let binders: HashSet<String> = pattern_bindings(pattern)
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                self.arm_binders.borrow_mut().push(binders);
                let then_div = self.block(then_block, &mut then_c, scope);
                self.arm_binders.borrow_mut().pop();
                self.exit();
                scope.pop();
                let mut else_c = consumed.clone();
                let else_div = match else_block {
                    Some(eb) => self.block(eb, &mut else_c, scope),
                    None => false,
                };
                if !then_div {
                    for (k, v) in then_c {
                        consumed.or_insert(k, v);
                    }
                }
                if !else_div {
                    for (k, v) in else_c {
                        consumed.or_insert(k, v);
                    }
                }
                Ok(then_div && else_div)
            }
            Stmt::While { cond, body, .. } => {
                // RFC-0114 M2: everything inside carries this loop's id, so the
                // fold can refuse to order two events a back edge could swap.
                let lid = self.next_loop.get();
                self.next_loop.set(lid + 1);
                self.loop_ids.borrow_mut().push(lid);
                // The condition re-runs on every iteration, so consumption in it
                // is loop-consumption exactly like the body's (`while take(x)`
                // would use `x` again next time around) — track both in the
                // in-loop map and run the same next-iteration check.
                let mut body_c = consumed.clone();
                self.expr(cond, &mut body_c, scope)?;
                let outer_continue = self.continue_seen.replace(false);
                let body_div = self.block(body, &mut body_c, scope);
                // A `continue` reaches the top again, so a consumption on its
                // path is still a next-iteration reuse; only a body every
                // path of which LEAVES the loop runs at most once.
                let leaves_loop = body_div && !self.continue_seen.get();
                self.continue_seen.set(outer_continue);
                self.loop_ids.borrow_mut().pop();
                self.check_loop_reuse(consumed, &body_c, scope, leaves_loop)?;
                for (k, v) in body_c {
                    consumed.or_insert(k, v);
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
                consuming,
            } => {
                // RFC-0114 M2: same loop stamp as `while` — see there.
                let m2_lid = self.next_loop.get();
                self.next_loop.set(m2_lid + 1);
                self.loop_ids.borrow_mut().push(m2_lid);
                self.expr(iter, consumed, scope)?;
                self.site("iterate", *line, iter, None);
                if *consuming {
                    self.check_take(iter, *line, scope, TakeForm::Loop)?;
                }
                let elem = self.type_of(iter).and_then(|t| self.decl.elem_of(&t));
                // RFC-0089 rule 2: the loop variable is a borrow only while the
                // container outlives the loop. A `consume`d container is the
                // loop's, and a container that is not a place — `for o in
                // diff(..)` — has no other owner at all, so both bind an OWNED
                // element and storing one is a move.
                let borrow = (!*consuming
                    && self.iterable_is_a_place(iter)
                    && elem.as_ref().is_some_and(|t| self.decl.owns_heap(t)))
                .then(|| Borrow::Element(place_path(iter).map(|(r, _)| r).unwrap_or_default()));
                // RFC-0092 M5, census "U4's price". `for k in m.keys()` walks a
                // TEMPORARY, and until here nothing released it. It is Phase
                // 10a's row for an `if let` over a temporary, at the second
                // statement that can walk one, and it is guarded the same way:
                // `place_key` answers 0 both for an expression that names no
                // place and for a place with no `let` row, and minting a row for
                // the second kind releases somebody else's value. That mistake
                // turned CI red for twenty-four runs — see the `if let` arm.
                //
                // `names_a_place` is the guard, unchanged and asked again. It
                // reads `m.keys()` as a temporary, which is right: `@keys` has
                // no seeded row and no `place` body, and since RFC-0092 M2 the
                // snapshot holds
                // COPIES of the keys rather than the map's own pointers, so the
                // loop is the only owner there is.
                //
                // A CONSUMING loop gets the SAME row (RFC-0092 M5's other half,
                // RFC-0095 M3). `for x in consume xs` takes the buffer, and the
                // row the place has says `Moved` — which is the truth about the
                // place and the end of the matter for it, so nothing freed the
                // buffer at all. The loop is its last owner: `check_take` has
                // already refused a borrowed root and refused module state, so
                // the value is this frame's, and the take (whole binding) or the
                // hole (one field) is what stops anything else from releasing it
                // too.
                //
                // The elements are the loop's on the same terms as a snapshot's:
                // the loop variable binds to this row below, so a body that
                // hands one on marks the row gone and the whole container leaks.
                // That is the direction this analysis is allowed to be wrong in,
                // and it is what makes an early `break` safe — a row that
                // survives is a body that kept nothing, so the release at the
                // exit gives back the visited and the unvisited elements alike,
                // each exactly once.
                let key = match self.place_key(iter) {
                    0 if !*consuming && self.names_a_place(iter).is_none() => {
                        self.note_temporary(s, iter)
                    }
                    _ if *consuming => self.note_temporary(s, iter),
                    _ => 0,
                };
                let mut body_c = consumed.clone();
                self.enter();
                self.bind(var, elem, borrow);
                // The loop variable is bound to the snapshot's row, so every way
                // an ELEMENT can leave the loop is written on it: a store
                // (`fs.push(Field { key: k, .. })`, which is what `httpInput`
                // does), a `return`, a `drop`, a capture, a handover to a
                // position that retains. A row that says the value left is a row
                // `own.rs` reclaims nothing from — the elements the body kept
                // stay allocated, and so does the buffer with them. That is a
                // leak and not a double free, which is the direction this
                // analysis is allowed to be wrong in.
                if key != 0 {
                    self.nodes.borrow_mut().bind(var, key);
                }
                let outer_continue = self.continue_seen.replace(false);
                let body_div = self.block(body, &mut body_c, scope);
                self.exit();
                // The loop variable is fresh on every iteration, so a move of it
                // is not a move of anything the enclosing scope can still name.
                body_c.remove(var);
                // A `continue` reaches the top of the NEXT iteration, so it is
                // not the runs-at-most-once divergence the reuse check may
                // skip on — see the `while` arm.
                let leaves_loop = body_div && !self.continue_seen.get();
                self.loop_ids.borrow_mut().pop(); // RFC-0114 M2: for-loop extent ends
                self.continue_seen.set(outer_continue);
                self.check_loop_reuse(consumed, &body_c, scope, leaves_loop)?;
                for (k, v) in body_c {
                    consumed.or_insert(k, v);
                }
                // The container is dead after a consuming loop: using it again is
                // the rule 1 error `expr` already reports.
                if *consuming {
                    // RFC-0093: the loop takes the PATH, so `for t in consume
                    // b.tags` empties that field and leaves the rest of `b`
                    // readable — the same hole the prefix makes, recorded the
                    // same way and handed to the same walk (M2).
                    if let Some((root, path)) = place_path(iter) {
                        let fixes = vec![format!("`{path}.copy()` if both sides need a value")];
                        if root == path {
                            self.took(
                                &root,
                                Gone::Moved {
                                    line: *line,
                                    by: "the `for .. in consume` loop".into(),
                                },
                            );
                        } else {
                            let rel = path
                                .strip_prefix(&root)
                                .and_then(|r| r.strip_prefix('.'))
                                .filter(|r| !r.contains('['))
                                .map(str::to_string);
                            self.hole(&root, *line, rel);
                        }
                        consumed.insert(
                            path.clone(),
                            Consumption {
                                line: *line,
                                by: "the `for .. in consume` loop".into(),
                                fixes,
                                hole: root != path,
                            },
                        );
                    }
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
                .map(|_| matches!(e, Expr::Call { name, .. } if crate::ast::is_panic(name))),
            // A `region` is an ordinary nested block for move checking; it
            // diverges iff its body does (a `break` inside it exits the loop).
            // Its map is a CLONE, like every other nested block's. Sharing it
            // by `&mut` let a shadowing `let` inside the region run `revive`
            // on the ENCLOSING map and erase the outer binding's consumption
            // record — a use-after-move after the region then compiled.
            Stmt::Region { body, .. } => {
                let mut inner = consumed.clone();
                let div = self.block(body, &mut inner, scope);
                // Consumption of an OUTER binding inside the region survives
                // it — that is the use-after-move the propagation exists for.
                // A binding the region DECLARES dies with the region, and its
                // record must die too: propagated by name, `drop s` on a
                // region-local `s` marked an unrelated later `s` — a match
                // arm's payload binding, in the corpus — as already consumed.
                let mut local = std::collections::HashSet::new();
                declared_in(body, &mut local);
                for (k, v) in inner {
                    let root = k.split('.').next().unwrap_or(&k).to_string();
                    if !local.contains(&root) {
                        consumed.or_insert(k, v);
                    }
                }
                Ok(div)
            }
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
                // Rule 2 at the drop. `drop x` reclaims storage, and a borrow
                // names storage somebody else owns — so `let owned = box.items ;
                // drop owned` hands back a buffer the record still holds. It
                // leaked one owner short while nothing released a record's
                // fields; RFC-0092 M3 releases them, and the native binary then
                // corrupts its heap (`examples/fieldmut.vyrn`, `0xC0000374`).
                //
                // The same sentence a store gets, at the third place a value can
                // leave — which is what stops one program from having two
                // verdicts a keyword apart.
                if let Some(b) = self.borrow_of(name) {
                    return Err(menu(
                        *line,
                        format!("`{name}` may not be dropped — it is {}", b.what(name)),
                        vec![
                            format!(
                                "`consume` the place where `{name}` is bound, so `{name}` takes \
                                 the value rather than naming it"
                            ),
                            format!(
                                "delete the `drop` — the place that owns it releases it \
                                 (RFC-0089 rule 4)"
                            ),
                        ],
                    ));
                }
                // A PARTIAL take left a hole in the binding (RFC-0093): the
                // taken place belongs to whoever received it, and `drop`
                // reclaims storage BY TYPE — it cannot be told to skip the
                // places a take handed away. Dropping here would free what
                // the receiver still holds, so the spelling is refused.
                if let Some((path, c)) = consumed
                    .overlapping(name)
                    .find(|(k, c)| k.as_str() != name && c.hole)
                {
                    return Err(menu(
                        *line,
                        format!(
                            "`{name}` may not be dropped — `{path}` was taken out of it on \
                             line {}, and `drop` releases the whole binding",
                            c.line
                        ),
                        vec![
                            format!(
                                "write `{path}` back before the `drop`, so the binding is \
                                 whole again"
                            ),
                            format!(
                                "delete the `drop` — the parts still here are released when \
                                 the block exits"
                            ),
                        ],
                    ));
                }
                self.took(name, Gone::Dropped { line: *line });
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

    /// A lambda reads a name from an enclosing frame, so the enclosing block
    /// gives it up (census §16).
    ///
    /// Every lambda, not only an escaping one. RFC-0037 puts a capture in the
    /// closure's payload by value; a non-escaping lambda does not outlive the
    /// call, but its captures are still a second word pointing at one buffer,
    /// and Phase 5 is where a closure releases what it holds. Until then the
    /// honest answer is that this block does not own it.
    fn note_capture(&self, name: &str, line: usize) {
        if self.lets.is_none() {
            return;
        }
        let Some(&inside) = self.lambda_base.borrow().last() else {
            return;
        };
        // A lambda whose callee provably only borrows it cannot outlive the
        // call, so it borrows and this block keeps the value — the same
        // condition `check_capture` applies to rule 2. A STORED one, and one
        // handed to a callee that may keep it, is a value under RFC-0037 and
        // can outlive the frame.
        if !self
            .lambda_escapes
            .borrow()
            .last()
            .copied()
            .unwrap_or(false)
        {
            return;
        }
        if self
            .vars
            .borrow()
            .frame_of(name)
            .is_some_and(|f| f < inside)
        {
            self.took(name, Gone::Captured { line });
        }
    }

    /// Exclusivity: `f(modify a, .. a ..)` is refused.
    ///
    /// `modify` is exclusive in-place access. Handing the same place to a
    /// `modify` parameter and to any other parameter of the same call gives the
    /// callee two names for one value, and the callee was told it had one.
    fn check_exclusive(&self, callee: &str, args: &[Expr], line: usize) -> Result<(), Diagnostic> {
        let Some(caps) = self.caps.get(callee) else {
            return Ok(());
        };
        for (i, a) in args.iter().enumerate() {
            if caps.get(i) != Some(&Capability::Modify) {
                continue;
            }
            let Some((root, path)) = place_path(a) else {
                continue;
            };
            for (j, b) in args.iter().enumerate() {
                if i != j && linear::mentions(b, &root) {
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
    /// A lambda whose callee provably only borrows it (`map(xs, |x| ..)`, where
    /// `map`'s parameter is a plain borrow) does not outlive the call, so it
    /// borrows freely — that is the common case and the rule leaves it alone.
    /// A lambda that is stored, or handed to a callee that may KEEP it (a
    /// `consume fn` parameter can store what it owns), is a value under
    /// RFC-0037's defunctionalization, and a borrow inside one has no lifetime
    /// to stand on.
    fn check_capture(&self, name: &str, line: usize) -> Result<(), Diagnostic> {
        let Some(&inside) = self.lambda_base.borrow().last() else {
            return Ok(());
        };
        if !self
            .lambda_escapes
            .borrow()
            .last()
            .copied()
            .unwrap_or(false)
        {
            return Ok(());
        }
        if !self
            .vars
            .borrow()
            .frame_of(name)
            .is_some_and(|f| f < inside)
        {
            return Ok(());
        }
        let Some(b) = self.borrow_of(name) else {
            return Ok(());
        };
        Err(menu(
            line,
            format!(
                "`{name}` may not be captured by a closure that outlives this call — it is {}",
                b.what(name)
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
    ) -> Result<(), Diagnostic> {
        if body_div {
            return Ok(());
        }
        for (k, c) in body_c {
            // RFC-0093 keys a take of a projection by its dotted PATH, but
            // the frames of `scope` hold binding NAMES: after `consume
            // er.node`, `k` is `"er.node"`, which no frame contains, so a
            // full-path test silently skipped every partial take. Membership
            // is asked of the key's ROOT — but a key carrying a `[` is
            // RFC-0082's place desugar, one binding and not a path, and it
            // relates to nothing but itself (the same sentence [`under`]
            // says).
            if !k.contains('[') && !consumed.contains_key(k) && Self::in_scope(scope, root_of(k)) {
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
    ) -> Result<(), Diagnostic> {
        match e {
            Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => Ok(()),
            Expr::Var { name, line } => {
                self.capture_site(name, *line);
                self.check_capture(name, *line)?;
                self.note_capture(name, *line);
                self.check_use(name, *line, consumed)
            }
            Expr::Unary { expr, .. } => self.expr(expr, consumed, scope),
            Expr::Binary { op, lhs, rhs, line } => {
                self.expr(lhs, consumed, scope)?;
                let r = self.expr(rhs, consumed, scope);
                // A String `+` is `@concat` written as an operator, so its
                // operands are call arguments and take the argument rule
                // (`rfcs/census-call-arguments.md` §9, finding 3). `"n" + label(i)`
                // reaches the operator lowering rather than a call, so it sat in
                // neither the census's 1505 nor RFC-0096 M3's operand class, and
                // leaked the same 48 bytes a turn.
                //
                // The name is `@concat` and not a spelling of its own, so
                // [`arg_verdict`]'s partition holds: an operand that ALLOCATED
                // its own value is M3's to free and answers `AlreadyFreed` here,
                // and a call result answers what its callee's signature says.
                if *op == BinOp::Add && self.concatenates(e) {
                    self.note_arg_temp(lhs, "@concat", 0, *line);
                    self.note_arg_temp(rhs, "@concat", 1, *line);
                }
                r
            }
            // A place chain asks ONE consumption question, of the whole path.
            // Walking into the root instead would ask it of `er` and refuse
            // `er.next` after `consume er.node`, which is the case RFC-0093
            // exists to allow. The root still gets its capture bookkeeping.
            Expr::Field { expr, line, .. } => match place_path(e) {
                Some((_, path)) => {
                    let (root, rline) = root_var(e);
                    self.capture_site(root, rline);
                    self.check_capture(root, rline)?;
                    self.note_capture(root, rline);
                    self.check_use(&path, *line, consumed)
                }
                None => self.expr(expr, consumed, scope),
            },
            // RFC-0093 — the take. `check_take` says whether the frame may give
            // this place away; the record below is what makes a later read of it,
            // or of anything overlapping it, a rule-1 error.
            Expr::Consume { place, line } => {
                self.expr(place, consumed, scope)?;
                self.check_take(place, *line, scope, TakeForm::Prefix)?;
                let (root, path) = place_path(place).expect("check_take proved this is a place");
                // A whole binding writes the same `Gone::Moved` the consuming
                // loop writes, so `own.rs` suppresses its drop through the two
                // lines it already has.
                //
                // A PARTIAL take leaves a hole, and the release walk is the TYPE
                // — which does not know that one field left. RFC-0093 M2 hands
                // the hole set to [`crate::own`], which hands it to the walk: the
                // binding is reclaimed MINUS these places. Every take of the same
                // root joins the set, because a record is drained a field at a
                // time.
                if root == path {
                    self.took(
                        &root,
                        Gone::Moved {
                            line: *line,
                            by: "`consume`".into(),
                        },
                    );
                } else {
                    // The walk starts at the binding, so the path it skips is
                    // relative to it. A name that is not a prefix of its own path
                    // is RFC-0082's place desugar, whose temporaries are named
                    // after the paths they took (`o.i[]`); it is one binding and
                    // not a path, and the walk cannot be told to skip inside it.
                    let rel = path
                        .strip_prefix(&root)
                        .and_then(|r| r.strip_prefix('.'))
                        .filter(|r| !r.contains('['))
                        .map(str::to_string);
                    self.hole(&root, *line, rel);
                }
                consumed.insert(
                    path.clone(),
                    Consumption {
                        line: *line,
                        by: "`consume`".into(),
                        fixes: vec![format!("`{path}.copy()` if both sides need a value")],
                        hole: root != path,
                    },
                );
                Ok(())
            }
            Expr::Try { expr, .. } => self.expr(expr, consumed, scope),
            // A literal's operands are places too: `Ring { slots: xs }` puts `xs`
            // where the record owns it, exactly as an argument does.
            Expr::StructLit { name, fields, line } => {
                // A literal RETAINS what it is given, so a lambda inside one
                // escapes whatever the enclosing call would have done with it.
                let outer = self.call_keeps.replace(Some(true));
                for (f, v) in fields {
                    self.site("literal", *line, v, None);
                    self.expr(v, consumed, scope)?;
                    let _ = self.store(
                        v,
                        &|| format!("the field `{name}.{f}`"),
                        *line,
                        true,
                        consumed,
                    )?;
                }
                self.call_keeps.set(outer);
                Ok(())
            }
            Expr::TryConstruct { name, args, line } => {
                let outer = self.call_keeps.replace(Some(true));
                for a in args {
                    self.site("literal", *line, a, None);
                    self.expr(a, consumed, scope)?;
                    let _ = self.store(a, &|| format!("`{name}`"), *line, true, consumed)?;
                }
                self.call_keeps.set(outer);
                Ok(())
            }
            Expr::Match {
                scrutinee,
                arms,
                line,
            } => {
                self.expr(scrutinee, consumed, scope)?;
                // The scrutinee's row, and the same two cases `if let` has since
                // Phase 10a — with the same guard, for the same reason.
                //
                // A PLACE keeps its own row: an arm that hands the payload out
                // gives that place up, and `note_arm_value` below writes it
                // there. A TEMPORARY has no name and therefore had no row at
                // all, so nothing released `match makeResult(i) { .. }` and a
                // statement-position match leaked its scrutinee every turn. The
                // row is minted here, keyed by the MATCH EXPRESSION's address —
                // a match is an expression and has no statement to key on — and
                // `own` releases exactly the rows nothing wrote on.
                let key = match self.place_key(scrutinee) {
                    0 if self.names_a_place(scrutinee).is_none() => {
                        self.note_temporary_at(e as *const Expr as usize, scrutinee)
                    }
                    k => k,
                };
                let base = consumed.clone();
                let mut arm_cs: Vec<Consumed> = Vec::new();
                let mut all_binders: HashSet<String> = HashSet::new();
                let mut arm_heap: Vec<bool> = Vec::new();
                for arm in arms {
                    let mut c = base.clone();
                    scope.push(HashSet::new());
                    self.enter();
                    let (tys, borrow) = self.payload_binding(scrutinee, &arm.pattern);
                    let binders: Vec<String> = pattern_bindings(&arm.pattern)
                        .into_iter()
                        .map(str::to_string)
                        .collect();
                    for (i, b) in binders.iter().enumerate() {
                        scope.last_mut().unwrap().insert(b.clone());
                        self.bind(b, tys.get(i).cloned().flatten(), borrow.clone());
                        if key != 0 {
                            self.nodes.borrow_mut().bind(b, key);
                        }
                    }
                    all_binders.extend(binders.iter().cloned());
                    // Asked HERE, one scope in, because the question is about the
                    // binders and this is where they are bound.
                    self.note_arm_value(&arm.body, *line, &binders);
                    // Asked here too, and kept per arm: the answer needs the
                    // binder types, which leave with this scope.
                    arm_heap.push(self.arm_carries_heap(&arm.body));
                    self.arm_binders
                        .borrow_mut()
                        .push(binders.iter().map(|b| b.to_string()).collect());
                    let r = self.expr(&arm.body, &mut c, scope);
                    self.arm_binders.borrow_mut().pop();
                    self.exit();
                    r?;
                    scope.pop();
                    arm_cs.push(c);
                }
                // RFC-0114 Rule N at a MATCH join: a binding cleanly whole-taken
                // in some arms and untouched in the rest is released on each
                // untouched arm's edge — the `if` rule with `edge` = the arm's
                // source index. Guards, all failing toward the leak: the name is
                // nobody's binder (a binder shadows it), the scrutinee does not
                // mention it (an arm's payload projects into the scrutinee), and
                // an untouched arm either carries no heap out or never mentions
                // the binding — its value must not alias what the edge frees. An
                // arm yielding the binding whole is `Gone::Aliased`, which the
                // fold's veto refuses.
                if let Some(sink) = &self.edge_releases {
                    let match_key = e as *const Expr as usize;
                    let mut roots: Vec<&String> = Vec::new();
                    for c in &arm_cs {
                        roots.extend(c.0.keys());
                    }
                    roots.sort();
                    roots.dedup();
                    for root in roots {
                        if base.0.contains_key(root)
                            || all_binders.contains(root.as_str())
                            || mentions_place(scrutinee, root)
                        {
                            continue;
                        }
                        let bkey = self.nodes.borrow().get(root).copied().unwrap_or(0);
                        if bkey == 0 {
                            continue;
                        }
                        let clean = |c: &Consumed| {
                            c.0.get(root).is_some_and(|b| {
                                b.len() == 1 && b.get(root).is_some_and(|t| !t.hole)
                            })
                        };
                        let untouched = |c: &Consumed| !c.0.contains_key(root);
                        if !arm_cs.iter().all(|c| clean(c) || untouched(c))
                            || !arm_cs.iter().any(|c| clean(c))
                            || arm_cs.iter().all(|c| clean(c))
                        {
                            continue;
                        }
                        for (i, (c, arm)) in arm_cs.iter().zip(arms).enumerate() {
                            // `arm_heap` keeps the in-scope answer (binder
                            // types are gone by now); the structural proof —
                            // operator results are scalar or fresh, a call
                            // returns only what it was handed — covers what
                            // the type alone could not.
                            let safe_value =
                                !arm_heap[i] || self.value_cannot_alias(&arm.body, root);
                            if untouched(c) && safe_value {
                                sink.borrow_mut().push(EdgeRel {
                                    if_key: match_key,
                                    key: bkey,
                                    name: root.clone(),
                                    edge: i as u32,
                                });
                            }
                        }
                    }
                }
                let mut merged: Option<Consumed> = None;
                for c in arm_cs {
                    match &mut merged {
                        None => merged = Some(c),
                        Some(m) => {
                            for (k, v) in c {
                                m.or_insert(k, v);
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
                line,
            } => {
                self.expr(cond, consumed, scope)?;
                self.note_arm_aliases(e, *line, &[]);
                let base = consumed.clone();
                let mut then_c = base.clone();
                self.expr(then_branch, &mut then_c, scope)?;
                let mut else_c = base.clone();
                if let Some(eb) = else_branch {
                    self.expr(eb, &mut else_c, scope)?;
                }
                // RFC-0114 Rule N at an `if`-expression join — the statement
                // rule with the match's value guard: the releasing branch's
                // value must not alias the binding (no heap, no mention, or a
                // Binary/Unary body, whose result is a scalar or fresh). No
                // binders and no scrutinee here, so those guards do not apply;
                // the condition is a Bool read completed before the branch.
                if let Some(sink) = &self.edge_releases {
                    if let Some(eb) = else_branch {
                        let if_key = e as *const Expr as usize;
                        let branches: [&Expr; 2] = [then_branch, eb];
                        for (taker, edge) in [(&then_c, 1usize), (&else_c, 0usize)] {
                            let other = if edge == 1 { &else_c } else { &then_c };
                            let value: &Expr = branches[edge];
                            for (root, bucket) in &taker.0 {
                                if base.0.contains_key(root)
                                    || other.0.contains_key(root)
                                    || bucket.len() != 1
                                {
                                    continue;
                                }
                                let Some(c) = bucket.get(root) else { continue };
                                if c.hole {
                                    continue;
                                }
                                let key = self.nodes.borrow().get(root).copied().unwrap_or(0);
                                if key == 0 {
                                    continue;
                                }
                                if !self.value_cannot_alias(value, root) {
                                    continue;
                                }
                                sink.borrow_mut().push(EdgeRel {
                                    if_key,
                                    key,
                                    name: root.clone(),
                                    edge: edge as u32,
                                });
                            }
                        }
                    }
                }
                for (k, v) in then_c.into_iter().chain(else_c) {
                    consumed.or_insert(k, v);
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
                    self.call_keeps.set(Some(self.callee_keeps(name, i)));
                    let r = self.expr(arg, consumed, scope);
                    self.call_keeps.set(None);
                    r?;
                    self.note_handover(arg, name, i, *line);
                    self.note_arg_temp(arg, name, i, *line);
                    if caps.and_then(|c| c.get(i)) == Some(&Capability::Consume) {
                        self.check_handover(arg, name, *line)?;
                        if let Expr::Var { name: v, line: vl } = arg {
                            if !Self::in_scope(scope, v) {
                                self.reject_consume_global(v, name, false, *vl)?;
                            }
                            self.took(
                                v,
                                Gone::Moved {
                                    line: *line,
                                    by: format!("`{}(..)`", crate::parser::method_surface(name)),
                                },
                            );
                            consumed.or_insert(
                                v.clone(),
                                Consumption::by_capability(
                                    *line,
                                    format!("`{}(..)`", crate::parser::method_surface(name)),
                                ),
                            );
                        }
                    } else if self.decl.constructs(name) {
                        self.note_retention(arg);
                        // A variant constructor is a literal that reads like a
                        // call: the value it builds holds the argument and
                        // outlives the call, exactly as an array literal does.
                        // Recorded, not refused. An aggregate releases nothing
                        // until Phase 5, so a payload put here is a leak today;
                        // making it a rule-2 store would refuse `return Some(s)`
                        // on a `read` parameter across the whole corpus, which is
                        // Phase 5's migration and not this one's.
                        if let Some((root, path)) = place_path(arg) {
                            if root == path {
                                self.took(
                                    &root,
                                    Gone::Moved {
                                        line: *line,
                                        by: format!(
                                            "`{}(..)`",
                                            crate::parser::method_surface(name)
                                        ),
                                    },
                                );
                            }
                        }
                    } else if self.sinks(name, i) {
                        // A builtin whose parameter declares `consume`. Rule 1
                        // governs it exactly as it governs `xs = [.., v]`, which
                        // is what it means.
                        let _ = self.store(
                            arg,
                            &|| format!("`{}(..)`", crate::parser::method_surface(name)),
                            *line,
                            true,
                            consumed,
                        )?;
                    }
                }
                Ok(())
            }
            Expr::ArrayLit { elems, line } => {
                // A literal RETAINS what it is given, so a lambda inside one
                // escapes whatever the enclosing call would have done with it.
                let outer = self.call_keeps.replace(Some(true));
                for e in elems {
                    self.site("literal", *line, e, None);
                    self.expr(e, consumed, scope)?;
                    let _ = self.store(
                        e,
                        &|| "the array literal".to_string(),
                        *line,
                        true,
                        consumed,
                    )?;
                }
                self.call_keeps.set(outer);
                Ok(())
            }
            Expr::MapLit { entries, line } => {
                let outer = self.call_keeps.replace(Some(true));
                for (k, v) in entries {
                    self.expr(k, consumed, scope)?;
                    self.site("literal", *line, v, None);
                    self.expr(v, consumed, scope)?;
                    let _ =
                        self.store(k, &|| "the map literal".to_string(), *line, true, consumed)?;
                    let _ =
                        self.store(v, &|| "the map literal".to_string(), *line, true, consumed)?;
                }
                self.call_keeps.set(outer);
                Ok(())
            }
            // A lambda body (RFC-0023): its untyped params are fresh locals; walk
            // the body so a `consume`-misuse inside it is still caught. Captured
            // bindings are read-only (the checker forbids consuming/dropping them),
            // so a reference to one that was already consumed surfaces the standard
            // use-after-consume error here too.
            Expr::Lambda { params, body, .. } => {
                // A lambda that is not a call argument can be stored and
                // outlive the frame. One written AT a call argument still
                // escapes when the callee may keep it: a `consume fn`
                // parameter owns what it is handed and can store it, and a
                // stored closure's captures leave the frame with it. Only a
                // parameter that provably borrows keeps the old fast path.
                let outer_keeps = self.call_keeps.replace(None);
                let escapes = outer_keeps.unwrap_or(true);
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
                        // A CLONE, like every other nested block's. Sharing
                        // the enclosing map by `&mut` let a shadowing `let`
                        // inside the block run `revive` on it and erase the
                        // outer binding's consumption record — the exact bug
                        // `Stmt::Region`'s clone fixed.
                        let mut inner = consumed.clone();
                        self.block(b, &mut inner, scope);
                        // Consumption of an OUTER binding inside the lambda
                        // survives it; anything the lambda's own frame
                        // declares — its parameters and its lets — dies with
                        // the frame.
                        let mut local = std::collections::HashSet::new();
                        local.extend(params.iter().cloned());
                        declared_in(b, &mut local);
                        for (k, v) in inner {
                            let root = k.split('.').next().unwrap_or(&k).to_string();
                            if !local.contains(&root) {
                                consumed.or_insert(k, v);
                            }
                        }
                        Ok(())
                    }
                };
                self.lambda_base.borrow_mut().pop();
                self.lambda_escapes.borrow_mut().pop();
                // Whatever the body's nested walks left in the cell, the
                // enclosing context's answer is restored: a lambda walked as
                // one argument must not decide whether a SIBLING lambda in a
                // later argument of the same call escapes.
                self.call_keeps.set(outer_keeps);
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
                    // A spawned frame outlives the statement that spawns it
                    // (census §10), so this block releases nothing it was handed.
                    self.gave_up(arg, &Gone::Captured { line: *line });
                    if caps.and_then(|c| c.get(i)) == Some(&Capability::Consume) {
                        self.check_handover(arg, name, *line)?;
                        if let Expr::Var { name: v, line: vl } = arg {
                            if !Self::in_scope(scope, v) {
                                self.reject_consume_global(v, name, true, *vl)?;
                            }
                            consumed.or_insert(
                                v.clone(),
                                Consumption::by_capability(*line, format!("`spawn {name}(..)`")),
                            );
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
    ) -> Result<(), Diagnostic> {
        if self.globals.contains(v) {
            let form = if spawned {
                format!("spawn {callee}(..)")
            } else {
                format!("{callee}(..)")
            };
            return Err(Diagnostic::error(
                line,
                0,
                "movecheck",
                format!(
                    "module state `{v}` may not be passed to a `consume` parameter \
                     via `{form}` — nothing may take ownership of module state (it lives for the \
                     whole module and is never dropped)"
                ),
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
///
/// Whether `e` reads the place `base`, or anything derived from it.
///
/// The store half of RFC-0089 rule 4 asks this: a store releases what the place
/// held, and the old value is usually an operand of the new one — `acc = acc +
/// x` reads the old buffer and `a = @push(a, i)` grows it. A value that names the
/// place therefore releases nothing. The self-append spine reclaims that shape
/// by not allocating at all; every other shape is a recorded leak, which is the
/// side of the trade a language that promises memory safety takes.
///
/// **Derived, not just equal.** A place desugar (RFC-0082) names its temporary
/// after the path it took: `t.xs[k] = v` becomes a move-out into `t.xs[]`, the
/// element store, and the write-back `t.xs = t.xs[]` — which hands the SAME
/// buffer back. Comparing the base name alone reads that write-back as a store of
/// an unrelated value and frees what it is about to store. `placeorder.vyrn`
/// caught it in one parity run, and it is the shape RFC-0087 §4 warned about in
/// its own words.
///
/// A lambda with a block body answers `true` without being read. The question is
/// "may this store free the old value", where `true` costs a leak and `false` can
/// cost a use-after-free.
pub fn mentions_place(e: &Expr, base: &str) -> bool {
    fn derived(n: &str, base: &str) -> bool {
        n == base
            || (n.len() > base.len()
                && n.starts_with(base)
                && matches!(n.as_bytes()[base.len()], b'.' | b'['))
    }
    fn go(e: &Expr, base: &str) -> bool {
        match e {
            Expr::Var { name, .. } => derived(name, base),
            Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => false,
            Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
                go(expr, base)
            }
            Expr::Consume { place, .. } => go(place, base),
            Expr::Binary { lhs, rhs, .. } => go(lhs, base) || go(rhs, base),
            Expr::Call { args, .. }
            | Expr::Spawn { args, .. }
            | Expr::TryConstruct { args, .. }
            | Expr::ArrayLit { elems: args, .. } => args.iter().any(|a| go(a, base)),
            Expr::MapLit { entries, .. } => entries.iter().any(|(k, v)| go(k, base) || go(v, base)),
            Expr::StructLit { fields, .. } => fields.iter().any(|(_, v)| go(v, base)),
            Expr::Match {
                scrutinee, arms, ..
            } => go(scrutinee, base) || arms.iter().any(|a| go(&a.body, base)),
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                go(cond, base)
                    || go(then_branch, base)
                    || else_branch.as_ref().is_some_and(|x| go(x, base))
            }
            Expr::Lambda {
                body: LambdaBody::Expr(inner),
                ..
            } => go(inner, base),
            Expr::Lambda { .. } => true,
        }
    }
    go(e, base)
}

/// The **must-use** obligation: a value of a linear type is acquired once and
/// disposed exactly once, and this is where that is proved (RFC-0086 M3).
///
/// It was `mod streams`, and the rename is the milestone. The rules below never
/// mentioned a stream's representation — they are about a name, a block and the
/// paths out of it — but three of them matched `Type::Stream` directly, so the
/// one compile-time reclamation proof in the language served exactly one type.
/// The matches are now a lookup in [`crate::own::Owned`], the same table
/// `impl Owned for T` adds a row to, so a user's file handle, transaction or
/// reply obligation joins the mechanism with no compiler change.
///
/// What the lookup answers is *whether*. [`crate::own::Linear`] answers *which
/// row*, and the only thing that reads it is the wording of the fix menu: a
/// stream is closed, a declared type is dropped, and offering either menu for
/// the other names a disposal that reclaims nothing.
mod linear {
    use std::collections::HashMap;

    use crate::ast::*;
    use crate::declared::Declared;
    use crate::diagnostics::Diagnostic;
    use crate::own::Linear;

    /// One live must-use binding, as a diagnostic about it needs it: the type
    /// spelled the way the program spelled it, and which row obliged it.
    #[derive(Clone)]
    struct Owed {
        ty: String,
        row: Linear,
    }

    /// What a straight-line statement list does to one live binding.
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
        // Functions whose return type carries the obligation, with the rendering
        // the diagnostic quotes.
        //
        // The seeded rows are read the same way as the declared ones, which is
        // RFC-0094 M1's whole change here: `fromArray`, `fromStep` and
        // `unboxStream` were a three-name `match` in `owed_let`, and they are now
        // three return types. Each is `Stream<T>` over a bound `T`, so [`owed`]
        // quotes the type CONSTRUCTOR — plainly `Stream` — which is what the
        // `match` said and what this pass can say without types.
        let producers: HashMap<&str, Owed> = program
            .functions
            .iter()
            .chain(crate::prelude::all())
            .filter_map(|f| {
                Some((
                    f.name.as_str(),
                    owed(&f.ret, f.type_params.as_slice(), decl)?,
                ))
            })
            .collect();
        let mut out = Vec::new();
        for f in &program.functions {
            // A must-use parameter carries the obligation into the callee: the
            // caller discharged its own by moving it, and `fn sink(s: Stream<T>) {}`
            // must not be the hole that lets it evaporate.
            let mut live: Vec<(String, Owed)> = Vec::new();
            for p in &f.params {
                // A **receiver** does not, and the obligation would be circular
                // if it did: `impl Owned for Txn { fn release(self) }` IS the
                // disposal, so a rule that made it discharge its own receiver
                // before reading it would leave the declared release unwritable.
                // `self` is a keyword, so a parameter carrying that name is an
                // impl receiver and nothing else.
                if p.name == "self" {
                    continue;
                }
                if let Some(o) = owed(&p.ty, &[], decl) {
                    let s = scan(&f.body.stmts, &p.name, false);
                    report(&mut out, &s, f.line, &p.name, &o, &f.module);
                    live.push((p.name.clone(), o));
                }
            }
            block(&f.body, &mut live, &producers, &f.module, decl, &mut out);
        }
        for t in &program.tests {
            block(
                &t.body,
                &mut Vec::new(),
                &producers,
                &t.module,
                decl,
                &mut out,
            );
        }
        for b in &program.benches {
            block(
                &b.body,
                &mut Vec::new(),
                &producers,
                &b.module,
                decl,
                &mut out,
            );
        }
        out
    }

    /// The obligation `ty` carries, with the spelling a diagnostic quotes it by,
    /// or `None` where it carries none.
    ///
    /// `binders` are the type parameters in scope where `ty` was written. A
    /// generic producer — every std/stream combinator is one — returns
    /// `Stream<U>`, and quoting that at `let m = map(feed(), double)` names a
    /// type parameter the program never wrote. This pass has no types, so it
    /// cannot say `Stream<Int64>` either; it quotes the type CONSTRUCTOR, which
    /// is what it already said for `fromArray` and is an under-specification
    /// rather than a wrong name. The test is on the rendered spelling because a
    /// signature's type parameter is not reliably a `Type::Param` before the
    /// checker runs.
    fn owed(ty: &Type, binders: &[String], decl: &Declared) -> Option<Owed> {
        let row = decl.linear_kind(ty)?;
        let r = ty.to_string();
        let mentions = |p: &String| {
            r.split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|w| w == p.as_str())
        };
        let ty = match binders.iter().any(mentions) {
            true => r.split('<').next().unwrap_or(&r).to_string(),
            false => r,
        };
        Some(Owed { ty, row })
    }

    fn report(
        out: &mut Vec<Diagnostic>,
        s: &Scan,
        line: usize,
        name: &str,
        o: &Owed,
        module: &Option<String>,
    ) {
        let ty = &o.ty;
        // `Array<Txn>` reads as "an" and `Stream<Int64>` reads as "a". The
        // container spellings arrived with RFC-0092 M4, and the sentence has said
        // "is a" since RFC-0075.
        let art = match ty.chars().next() {
            Some('A' | 'E' | 'I' | 'O' | 'U' | 'a' | 'e' | 'i' | 'o' | 'u') => "an",
            _ => "a",
        };
        let msg = if s.doubled {
            format!("`{name}` is {art} `{ty}` and is disposed more than once")
        } else if s.leaked || !(s.disposed || s.diverges) {
            format!("`{name}` is {art} `{ty}` and is never disposed")
        } else {
            return;
        };
        let mut d = Diagnostic::error(line, 0, "movecheck", msg);
        // The two menus differ because the two disposals do. A stream's release
        // is pushed by its own lowering, so `drop` on one reclaims nothing; a
        // declared type has no `close` and is not iterable unless it says so.
        d.note = Some(match &o.row {
            Linear::Stream => format!(
                "a stream must be consumed with `for … in`, forwarded by returning it, \
                 or released with `close({name})` — on every path"
            ),
            // RFC-0095 M1. `drop` is named last because it throws the result
            // away: a task is normally discharged by reading it.
            Linear::Task if ty == "Task" || ty.starts_with("Task<") => format!(
                "a task must be joined with `{name}.join()`, which yields its result, \
                 forwarded by returning it, or released with `drop {name}`, which waits \
                 for it and discards the result — on every path"
            ),
            // The container case (RFC-0092 M4), and the menu is not the one
            // above: `{name}.join()` is not a thing a container has, and a
            // `drop` of one frees the buffer and NOT the tasks in it. Walking it
            // with `for … in consume` is the discharge that works — the loop
            // takes the container and every element is joined by name.
            Linear::Task => format!(
                "a `{ty}` holds a task, so the container must be handed on by name — walked \
                 with `for t in consume {name}`, joining each element, passed to a call, or \
                 forwarded by returning it — on every path"
            ),
            Linear::Declared(by) if by == ty => format!(
                "`{ty}` declares `impl MustUse`, so a value of it must be handed on by \
                 name — passed to a call, forwarded by returning it, or released with \
                 `drop {name}` — on every path"
            ),
            // The container case (RFC-0092 M4). Naming both types is the whole
            // point: the reader wrote `Array<Txn>` and the row is `Txn`'s, and a
            // note that named only one of them sends them to the wrong file.
            Linear::Declared(by) => format!(
                "`{by}` declares `impl MustUse` and a `{ty}` holds one, so the container \
                 must be handed on by name — passed to a call, forwarded by returning it, \
                 or released with `drop {name}`, which releases each element — on every path"
            ),
        });
        d.file = module.clone();
        out.push(d);
    }

    /// Check one block: every must-use binding declared in it must be disposed
    /// on every path out of the REST of that block. `live` is the enclosing
    /// scopes' obliged names, needed only so `let t = s` is recognised as a move.
    fn block(
        b: &Block,
        live: &mut Vec<(String, Owed)>,
        producers: &HashMap<&str, Owed>,
        module: &Option<String>,
        decl: &Declared,
        out: &mut Vec<Diagnostic>,
    ) {
        let base = live.len();
        for (i, st) in b.stmts.iter().enumerate() {
            if let Stmt::Let {
                name,
                ty,
                value,
                line,
                ..
            } = st
            {
                if let Some(o) = owed_let(ty.as_ref(), value, live, producers, decl) {
                    // `false`: a `break` in the rest of THIS block leaves the block
                    // that declared the value, so it abandons it. Inside a loop
                    // nested below, a `break` only leaves that loop and control
                    // comes back here still owning it — which is what the flag
                    // distinguishes.
                    let s = scan(&b.stmts[i + 1..], name, false);
                    report(out, &s, *line, name, &o, module);
                    live.push((name.clone(), o));
                }
            }
            for sub in sub_blocks(st) {
                block(sub, live, producers, module, decl, out);
            }
        }
        live.truncate(base);
    }

    /// The obligation this `let` binds, if it binds one.
    fn owed_let(
        ty: Option<&Type>,
        value: &Expr,
        live: &[(String, Owed)],
        producers: &HashMap<&str, Owed>,
        decl: &Declared,
    ) -> Option<Owed> {
        // The must-use row, not a `Stream` match: an alias of a must-use type
        // carries the obligation its base does.
        if let Some(o) = ty.and_then(|t| owed(t, &[], decl)) {
            return Some(o);
        }
        match value {
            // The builtin producers arrive here through `producers` like every
            // declared one — RFC-0094 M1 deleted the three-name `match` that
            // stood in front of this arm.
            Expr::Call { name, .. } => producers.get(name.as_str()).cloned(),
            // `spawn f(x)` is the one producer that is a KEYWORD rather than a
            // named function, so it cannot arrive through `producers`
            // (RFC-0095 M1). The type is quoted as the constructor, `Task`, for
            // the reason [`owed`] gives: this pass has no types, and `spawn`'s
            // result type is the callee's return type in a `Task`.
            Expr::Spawn { .. } => Some(Owed {
                ty: "Task".into(),
                row: Linear::Task,
            }),
            // `let t = s` moves the value; `t` inherits both the obligation and
            // the rendering, and the mention of `s` discharges `s`'s.
            Expr::Var { name, .. } => live.iter().find(|(l, _)| l == name).map(|(_, o)| o.clone()),
            // An arm is a path here as much as it is in [`scan`] (RFC-0095 M3,
            // which recorded this one as open). A branch hands on whichever arm
            // ran, so the binding inherits the obligation ANY arm carries: the
            // union is what makes `let t2 = match c { A => t, B => u }` a task
            // `t2` answers for, where before it was a task nothing answered for.
            //
            // The first arm that carries one answers for the rendering as well.
            // The checker has already made the arms agree on the type, so a
            // second arm would quote the same spelling.
            Expr::Match { arms, .. } => arms
                .iter()
                .find_map(|a| owed_let(None, &a.body, live, producers, decl)),
            Expr::IfExpr {
                then_branch,
                else_branch,
                ..
            } => owed_let(None, then_branch, live, producers, decl).or_else(|| {
                else_branch
                    .as_ref()
                    .and_then(|e| owed_let(None, e, live, producers, decl))
            }),
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
            // `.0` is "some path through this statement disposes it", `.1` is
            // "every path does". They differ only where a `match` or an
            // if-expression branches (RFC-0095 M3).
            let none = (false, false);
            let moved = match st {
                // A write back INTO the binding is not a disposal: whatever the
                // right-hand side did with the value, the binding holds one
                // again when the statement ends. RFC-0092 M4 is what made this
                // matter. `pool.push(t)` is parsed as `pool = @push(pool, t)`
                // (see `hoist_mutating_receiver` and the `@push` arm beside it),
                // so with a container carrying its element's obligation, every
                // mutation of the pool read as "handed on by name" and the
                // obligation evaporated at the one statement the milestone
                // exists to catch.
                Stmt::Assign { name: n, value, .. } if n == name => none,
                Stmt::Assign { value, .. }
                | Stmt::Let { value, .. }
                | Stmt::SetField { value, .. }
                | Stmt::Expr(value) => paths(value, name),
                Stmt::IndexSet { index, value, .. } => {
                    let (i, v) = (paths(index, name), paths(value, name));
                    (i.0 || v.0, i.1 || v.1)
                }
                Stmt::If { cond, .. } | Stmt::While { cond, .. } => paths(cond, name),
                Stmt::IfLet { scrutinee, .. } => paths(scrutinee, name),
                Stmt::ForIn { iter, .. } => paths(iter, name),
                Stmt::Drop { name: n, .. } => (n == name, n == name),
                Stmt::Return { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => none,
                Stmt::Region { .. } => none,
            };
            // One arm disposes it and another does not. Whatever follows, one of
            // the two paths is wrong — the same authoring mistake two disagreeing
            // `if` blocks make below, reported the same way and at the same
            // point, rather than left to a later statement to make look fine.
            if moved.0 && !moved.1 {
                acc.leaked = true;
                return acc;
            }
            if moved.1 {
                acc.disposed = true;
                // The probe walks the REACHABLE rest: a statement after a
                // diverging one is unreachable and [`MoveCheck::block`] never
                // checks it, so a mention there is not a second disposal.
                let mut doubled = false;
                for s in &stmts[i + 1..] {
                    if stmt_mentions(s, name) {
                        doubled = true;
                        break;
                    }
                    if diverges(std::slice::from_ref(s)) {
                        break;
                    }
                }
                acc.doubled = doubled;
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
                    // else leaves the function still owning it. `paths` and not
                    // `mentions`, for the reason it exists: `return match p {
                    // Some(n) => t, None => 0 }` forwards the task on one path
                    // and abandons it on the other (RFC-0095 M3).
                    acc.diverges = true;
                    acc.leaked |= !value.as_ref().is_some_and(|e| paths(e, name).1);
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
            Stmt::Expr(Expr::Call { name, .. }) => crate::ast::is_panic(name),
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
    pub(super) fn sub_blocks(s: &Stmt) -> Vec<&Block> {
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
            Stmt::While { body, .. } | Stmt::ForIn { body, .. } | Stmt::Region { body, .. } => {
                vec![body]
            }
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
        paths(e, name).0
    }

    /// How the paths through `e` treat the binding: `.0` where SOME path names
    /// it, `.1` where EVERY path does.
    ///
    /// The two answers differ at exactly two shapes — a `match` and an `if` used
    /// as an expression — because those are the only expressions with a path
    /// that skips a sub-expression. Everything else evaluates all of its parts,
    /// so a mention in one part is a mention on every path through the whole.
    ///
    /// This is RFC-0095 M3. [`scan`] read a statement's expressions with
    /// [`mentions`] alone, which answers "some path", and then treated the answer
    /// as a disposal on every path — so `match p { Some(n) => t.join(), None => 0 }`
    /// discharged a task the `None` path abandons. The `if` STATEMENT never had
    /// the hole: `scan` walks its two blocks and merges them. The merge is
    /// unchanged; what changed is that a branching EXPRESSION now reaches it.
    fn paths(e: &Expr, name: &str) -> (bool, bool) {
        // Two sub-expressions that both run: a mention in either is a mention,
        // and a disposal on every path through either is one through the pair.
        let seq = |a: (bool, bool), b: (bool, bool)| (a.0 || b.0, a.1 || b.1);
        let all = |m: bool| (m, m);
        match e {
            Expr::Var { name: n, .. } => all(n == name),
            Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {
                (false, false)
            }
            Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
                paths(expr, name)
            }
            Expr::Consume { place, .. } => paths(place, name),
            Expr::Binary { lhs, rhs, .. } => seq(paths(lhs, name), paths(rhs, name)),
            Expr::Call { args, .. }
            | Expr::Spawn { args, .. }
            | Expr::TryConstruct { args, .. }
            | Expr::ArrayLit { elems: args, .. } => args
                .iter()
                .fold((false, false), |acc, a| seq(acc, paths(a, name))),
            Expr::MapLit { entries, .. } => entries.iter().fold((false, false), |acc, (k, v)| {
                seq(seq(acc, paths(k, name)), paths(v, name))
            }),
            Expr::StructLit { fields, .. } => fields
                .iter()
                .fold((false, false), |acc, (_, v)| seq(acc, paths(v, name))),
            // The scrutinee runs whatever arm is taken, so it is sequenced with
            // the arms rather than merged into them. An arm list that is empty
            // has no path of its own to say anything about.
            Expr::Match {
                scrutinee, arms, ..
            } => {
                let s = paths(scrutinee, name);
                if arms.is_empty() {
                    return s;
                }
                let any = arms.iter().any(|a| paths(&a.body, name).0);
                let every = arms.iter().all(|a| paths(&a.body, name).1);
                seq(s, (any, every))
            }
            // A missing `else` is a path that names nothing. The checker refuses
            // an if-expression without one, so this is the incomplete tree and
            // not a shape a program can write.
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                let t = paths(then_branch, name);
                let e = match else_branch {
                    Some(b) => paths(b, name),
                    None => (false, false),
                };
                seq(paths(cond, name), (t.0 || e.0, t.1 && e.1))
            }
            // A lambda body may never run, and reading it as a disposal on every
            // path is the answer this walk has always given. Narrowing it would
            // widen what compiles, which is not this milestone.
            Expr::Lambda { body, .. } => all(match body {
                LambdaBody::Expr(e) => mentions(e, name),
                LambdaBody::Block(b) => b.stmts.iter().any(|s| stmt_mentions(s, name)),
            }),
        }
    }
}

/// Whether parameter `i` of the builtin `name` takes its argument for good,
/// under **rule 1**.
///
/// It was `RESERVED_SINKS`, three rows in a hand list (RFC-0087 §2b). RFC-0094
/// M1 reads `consume` off the seeded signature instead ([`crate::prelude`]), so
/// the fact is written once where every rule sees it. Everything else a builtin
/// does with a heap argument is a read: `print` formats it, `@concat` copies out
/// of it, `at` looks inside it.
///
/// **A linear parameter is not rule 1's.** `close`, `boxStream` and
/// `serveStream` each declare `consume Stream<T>`, and a `Stream<T>` already
/// carries a disposal obligation the [`linear`] walk proves: every mention of a
/// stream binding is a disposal there, so a second one is refused before rule 1
/// is asked. Two rules over one value would refuse the same program twice with
/// the worse words — rule 1's menu offers `.copy()`, which a stream has no
/// answer for. The obligation on the TYPE wins, and the census's claim that
/// these three carry a rule "nowhere at all" is corrected rather than acted on.
fn sinks(decl: &Declared, name: &str, i: usize) -> bool {
    let Some(p) = crate::prelude::signature(name).and_then(|f| f.params.get(i)) else {
        return false;
    };
    p.capability == Capability::Consume && decl.linear_kind(&p.ty).is_none()
}

/// Every name `e` reads, root names only, in no particular order.
///
/// Used where a whole expression carries values out of the frame — a `return`,
/// a `spawn`. It over-collects on purpose: a name it lists costs a leak, and a
/// name it misses costs a use-after-free.
fn reads(e: &Expr) -> Vec<String> {
    fn go(e: &Expr, out: &mut Vec<String>) {
        match e {
            Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {}
            Expr::Var { name, .. } => out.push(name.clone()),
            Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
                go(expr, out)
            }
            Expr::Consume { place, .. } => go(place, out),
            Expr::Binary { lhs, rhs, .. } => {
                go(lhs, out);
                go(rhs, out);
            }
            Expr::Call { args, .. }
            | Expr::TryConstruct { args, .. }
            | Expr::ArrayLit { elems: args, .. }
            | Expr::Spawn { args, .. } => {
                for a in args {
                    go(a, out);
                }
            }
            Expr::StructLit { fields, .. } => {
                for (_, v) in fields {
                    go(v, out);
                }
            }
            Expr::MapLit { entries, .. } => {
                for (k, v) in entries {
                    go(k, out);
                    go(v, out);
                }
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                go(scrutinee, out);
                for a in arms {
                    go(&a.body, out);
                }
            }
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                go(cond, out);
                go(then_branch, out);
                if let Some(eb) = else_branch {
                    go(eb, out);
                }
            }
            Expr::Lambda { body, .. } => match body {
                LambdaBody::Expr(inner) => go(inner, out),
                LambdaBody::Block(_) => {}
            },
        }
    }
    let mut out = Vec::new();
    go(e, &mut out);
    out
}

/// Every function name called anywhere in `e`.
///
/// The walk is what marks a lender's FORWARDERS: a call nested inside an
/// aggregate (`return R { name: pick(a) }`, `return [pick(a)]`) forwards the
/// lender's result exactly as a bare `return pick(a)` does, so the literal,
/// constructor, take, spawn and operator forms descend like the call arm. A
/// form missed here leaves its wrapper unmarked, and the wrapper's caller then
/// releases storage the lender's own caller still owns.
fn calls_in(e: &Expr, out: &mut Vec<String>) {
    if let Expr::Call { name, .. } = e {
        out.push(name.clone());
    }
    match e {
        Expr::Match {
            scrutinee, arms, ..
        } => {
            calls_in(scrutinee, out);
            for a in arms {
                calls_in(&a.body, out);
            }
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            calls_in(cond, out);
            calls_in(then_branch, out);
            if let Some(b) = else_branch {
                calls_in(b, out);
            }
        }
        Expr::Call { args, .. }
        | Expr::Spawn { args, .. }
        | Expr::TryConstruct { args, .. }
        | Expr::ArrayLit { elems: args, .. } => {
            for a in args {
                calls_in(a, out);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
            calls_in(expr, out)
        }
        Expr::Consume { place, .. } => calls_in(place, out),
        Expr::Binary { lhs, rhs, .. } => {
            calls_in(lhs, out);
            calls_in(rhs, out);
        }
        Expr::StructLit { fields, .. } => {
            for (_, v) in fields {
                calls_in(v, out);
            }
        }
        Expr::MapLit { entries, .. } => {
            for (k, v) in entries {
                calls_in(k, out);
                calls_in(v, out);
            }
        }
        _ => {}
    }
}

/// The place `e` reads, as `(root name, whole path)`.
///
/// `s` is `("s", "s")` and `r.a.b` is `("r", "r.a.b")` — the root is what a move
/// takes, the path is what the diagnostic quotes. Anything else (a call, a
/// literal, an operator) is not a place and answers `None`: it has no earlier
/// owner, so nothing about it can be a move.
/// The place an ELEMENT read looks into: `xs[i]` reaches this pass as `@at(xs, i)`,
/// which is a call, so [`place_path`] answers `None` for it.
///
/// M0 found that the RFC was wrong to say an element read is covered "by the
/// same three lines as a field read". It is true of `borrow_from`, which reads
/// `@at(..)` itself, and false of [`MoveCheck::store`] and of
/// [`MoveCheck::returned_borrow`], both of which bailed at `place_path` before
/// deciding anything. **M1 took the decision M0 left open and widened both**, so
/// `out.push(xs[i])` and `return items[i]` are refused like the field they are.
/// The instrument still counts them apart, under `elem-store` and `elem-return`.
fn element_path(e: &Expr) -> Option<(String, String)> {
    match e {
        Expr::Call { name, args, .. } if name == crate::project::AT => {
            let a = args.first()?;
            let (root, path) = place_path(a).or_else(|| element_path(a))?;
            Some((root, format!("{path}[{}]", index_text(args.get(1)))))
        }
        // A field OF an element: `fs[0].key`. [`place_path`] walks a `Field` down
        // to a `Var` and answers `None` as soon as it meets the `@at(..)` call, so
        // without this arm the escape hatch is one dot wide — `let f = fs[0]`
        // then `return f.key` is refused and `return fs[0].key` is not.
        Expr::Field { expr, field, .. } => {
            let (root, path) = element_path(expr)?;
            Some((root, format!("{path}.{field}")))
        }
        _ => None,
    }
}

/// An index as the reader wrote it, for the quoted path in a diagnostic.
///
/// A whole name and a whole integer are spelled back, so `xs[i]` and `fs[0]`
/// print as themselves and the `.copy()` on the menu is text `vyrn fix` can find
/// in the line. Anything else prints `..`: the message still says which read is
/// the problem, and `vyrn fix` then refuses rather than guessing where to put the
/// call — which is the behaviour it already has for a path it cannot locate.
fn index_text(e: Option<&Expr>) -> String {
    match e {
        Some(Expr::Var { name, .. }) => name.clone(),
        Some(Expr::Int(n)) => n.to_string(),
        _ => "..".to_string(),
    }
}

fn place_path(e: &Expr) -> Option<(String, String)> {
    match e {
        Expr::Var { name, .. } => Some((name.clone(), name.clone())),
        Expr::Field { expr, field, .. } => {
            let (root, path) = place_path(expr)?;
            Some((root, format!("{path}.{field}")))
        }
        // RFC-0093: a take is not a place. `consume d.title` names no storage
        // the frame can still reach, so it is an OWNER at every store, every
        // return and every pattern position — with no second rule, which is the
        // whole reason the prefix costs so little.
        _ => None,
    }
}

/// The root variable of a place chain, with its own line.
///
/// [`MoveCheck::expr`]'s `Field` arm asks the consumption question of the whole
/// path, so the capture bookkeeping the root would have got from the recursive
/// walk is done here instead.
fn root_var(e: &Expr) -> (&str, usize) {
    match e {
        Expr::Field { expr, .. } => root_var(expr),
        Expr::Var { name, line } => (name, *line),
        _ => ("", 0),
    }
}

/// One diagnostic with its menu of fixes (RFC-0087 U2).
///
/// Every rule-1/2/3 error names the ways out rather than only the problem. The
/// shape is fixed — the offending line, then one `fix:` per way out — so the
/// editor, `vyrn check` and a future `vyrn fix` all read the same thing.
fn menu(line: usize, message: String, fixes: Vec<String>) -> Diagnostic {
    let mut s = message;
    for f in fixes {
        s.push_str(&format!("\n  fix: {f}"));
    }
    Diagnostic::error(line, 0, "movecheck", s)
}

/// The payload names a `match` pattern binds.
/// Every name a block's statements DECLARE, at any depth — the bindings that
/// cannot outlive it. Match-arm binders live in expressions and are scoped by
/// `arm_binders`; statement-level declarations are what a region's consumption
/// propagation must filter on.
fn declared_in(block: &crate::ast::Block, out: &mut std::collections::HashSet<String>) {
    for s in &block.stmts {
        match s {
            crate::ast::Stmt::Let { name, .. } => {
                out.insert(name.clone());
            }
            crate::ast::Stmt::If {
                then_block,
                else_block,
                ..
            } => {
                declared_in(then_block, out);
                if let Some(e) = else_block {
                    declared_in(e, out);
                }
            }
            crate::ast::Stmt::IfLet {
                pattern,
                then_block,
                else_block,
                ..
            } => {
                for b in pattern_bindings(pattern) {
                    out.insert(b.to_string());
                }
                declared_in(then_block, out);
                if let Some(e) = else_block {
                    declared_in(e, out);
                }
            }
            crate::ast::Stmt::While { body, .. } | crate::ast::Stmt::Region { body, .. } => {
                declared_in(body, out);
            }
            crate::ast::Stmt::ForIn { var, body, .. } => {
                out.insert(var.clone());
                declared_in(body, out);
            }
            _ => {}
        }
    }
}

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

    /// What `views` and `sinks` answer, now that both read a signature.
    ///
    /// The lists they read are gone (RFC-0094 M1) and the reservation check
    /// moved to `prelude` with the rows. What is pinned here is the READING: a
    /// row's `consume` must reach `sinks` and a row's projection body must reach
    /// `views`, or the passes are back to knowing nothing.
    #[test]
    fn the_passes_read_the_seeded_capabilities() {
        assert!(views(crate::project::AT) && views("bytes"));
        assert!(!views("stringFromBytes"), "its inverse allocates");

        let p = crate::parser::parse(crate::lexer::lex("fn main() -> Int64 { return 0 }").unwrap())
            .unwrap();
        let decl = Declared::new(&p);
        for (name, i) in [("@push", 1), ("fromArray", 0), ("fromStep", 2)] {
            assert!(
                sinks(&decl, name, i),
                "`{name}` argument {i} declares `consume`"
            );
        }
        assert!(
            !sinks(&decl, "@push", 0),
            "the receiver is rebuilt, not taken"
        );
        // The four linear ones declare `consume` too, and the must-use walk
        // owns them — see [`sinks`]. `@join` is the fourth (RFC-0095 M1): a
        // `Task<T>` is linear exactly as a `Stream<T>` is, so the obligation on
        // the TYPE refuses a second join and rule 1 stands aside.
        for name in ["close", "boxStream", "serveStream", "@join"] {
            assert_eq!(
                crate::prelude::capability(name, 0),
                Some(Capability::Consume)
            );
            assert!(!sinks(&decl, name, 0));
        }
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
    fn rejects_drop_after_a_partial_take() {
        // F2-049: the taken field belongs to whoever received it, and `drop`
        // reclaims storage by TYPE — freeing the whole binding here frees
        // what the receiver still holds.
        let src = "type T = { id: Int64, name: String }; \
                   fn main() -> Int64 { let t = T { id: 1, name: \"n\" }; \
                                      consume t.name; drop t; return 0; }";
        let e = run(src).unwrap_err();
        assert!(e.contains("may not be dropped"), "{e}");
    }

    #[test]
    fn rejects_consume_before_a_trailing_continue() {
        // F2-052: `continue` starts the NEXT iteration, so the body re-runs —
        // it is not the runs-at-most-once divergence the reuse check skips on.
        let src = "type T = { id: Int64 }; \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let x = T { id: 1 }; \
                                      for i in [1, 2] { let a = take(x); continue; } \
                                      return 0; }";
        let e = run(src).unwrap_err();
        assert!(e.contains("inside a loop"), "{e}");
    }

    #[test]
    fn rejects_partial_take_repeated_across_iterations() {
        // F2-053: a take of a projection is keyed by its dotted PATH, which
        // no scope frame contains — the reuse check must ask the key's ROOT.
        let src = "type T = { node: String, rest: Int64 }; \
                   fn main() -> Int64 { let er = T { node: \"n\", rest: 0 }; \
                                      for i in [1, 2] { consume er.node } return 0; }";
        let e = run(src).unwrap_err();
        assert!(e.contains("inside a loop"), "{e}");
    }

    #[test]
    fn allows_partial_take_of_the_loop_variable() {
        // The loop variable is fresh every iteration, so taking a projection
        // of it each turn is legal — the root of `u.name` is not in scope and
        // the reuse check must keep skipping it.
        let src = "type E = { name: String, id: Int64 }; \
                   fn main() -> Int64 { let mut out = 0; \
                                      let xs = [E { name: \"a\", id: 1 }, E { name: \"b\", id: 2 }]; \
                                      for u in consume xs { consume u.name } return out; }";
        assert!(run(src).is_ok());
    }

    #[test]
    fn lambda_block_shadowing_let_does_not_revive_a_moved_binding() {
        // F2-051: a lambda block body shares the enclosing map no more — a
        // shadowing `let` inside it must not erase the outer consumption.
        let src = "type T = { id: Int64 }; \
                   fn make() -> T { return T { id: 1 }; } \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let s = make(); let n = take(s); \
                                      let g = x -> { let s = 0; x + s }; \
                                      return g(n) + take(s); }";
        let e = run(src).unwrap_err();
        assert!(e.contains("already consumed"), "{e}");
    }

    #[test]
    fn rejects_if_expr_arm_handing_a_field_to_a_consume_param() {
        // F2-050: the value an `if` expression yields is a projection of a
        // place this frame owns, and it answers the same question its direct
        // spelling would.
        let src = "type T = { title: String, id: Int64 }; \
                   fn sink(s: consume String) -> Int64 { return 0; } \
                   fn main() -> Int64 { let d = T { title: \"t\", id: 1 }; \
                                      let r = sink(if 1 > 0 { d.title } else { \"\" }); \
                                      return r; }";
        let e = run(src).unwrap_err();
        assert!(
            e.contains("may not be passed to a `consume` parameter"),
            "{e}"
        );
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
        assert!(
            e.contains("fix: declare the parameter `s: consume ..`"),
            "{e}"
        );
        assert!(e.contains("fix: `s.copy()`"), "{e}");
        // Both named fixes work.
        assert!(run("fn id(s: consume String) -> String { return s } \
                     fn main() -> Int64 { return 0 }")
        .is_ok());
        assert!(run("fn id(s: String) -> String { return s.copy() } \
                     fn main() -> Int64 { return 0 }")
        .is_ok());
    }

    /// Phase 10a. `openRule(c)` is `for m in c.members { return Some(m) }` — a
    /// projection of a `read` parameter, wrapped in a constructor. Phase 5 named
    /// this as the shape nothing could see, and Phase 10a paid for it: the
    /// moment an `if let` scrutinee was released, `std/contract` read freed
    /// members and the `components` generator emitted a mangled spelling.
    ///
    /// The record is a LEND, never a refusal — refusing here would refuse
    /// `return Some(m)` over any loop element, which is most of the corpus.
    #[test]
    fn a_borrow_wrapped_in_a_constructor_is_recorded_as_a_lend() {
        let src = "type M = { name: String } \
                   type C = { members: Array<M> } \
                   fn openRule(c: C) -> Option<M> { for m in c.members { return Some(m) } \
                   return None } \
                   fn main() -> Int64 { let c = C { members: [] } \
                   if let Some(r) = openRule(c) { return r.name.byteLength } return 0 }";
        let program = crate::parser::parse(crate::lexer::lex(src).unwrap()).unwrap();
        // It compiles: the wrapper is recorded, not refused.
        assert!(super::check(&program).is_ok());
        // And every row that names its result is off the release list, so the
        // `if let` scrutinee is not freed under the arm that reads it.
        assert!(
            super::ownership(&program)
                .values()
                .all(|r| r.from_call.as_deref() != Some("openRule") || r.gone.is_some()),
            "a lender's result must never be reclaimed by its caller"
        );
    }

    /// Phase 10a keyed the scrutinee row on `place_key == 0`, and 0 means two
    /// things: no place at all, and a place with no `let` row. A parameter is
    /// the second — it binds node 0 — so `if let Some(s) = v` released the
    /// caller's value. `examples/argsdemo.vyrn` fed it an `args()` element and
    /// CI's parity job went red for twenty-four runs.
    #[test]
    fn an_if_let_over_a_parameter_gets_no_reclamation_row() {
        let src = "fn show(v: Option<String>) -> Int64 { \
                   if let Some(s) = v { return s.byteLength } return 0 } \
                   fn main() -> Int64 { return 0 }";
        let program = crate::parser::parse(crate::lexer::lex(src).unwrap()).unwrap();
        assert!(super::check(&program).is_ok());
        assert!(
            super::ownership(&program)
                .values()
                .all(|r| r.gone.is_some()),
            "a parameter is the caller's, so the `if let` over one may not be reclaimed"
        );
        // A genuine temporary still gets one — the row Phase 10a is for.
        let tmp = "fn maybe() -> Option<String> { return Some(\"a\" + \"b\") } \
                   fn main() -> Int64 { if let Some(s) = maybe() { return s.byteLength } \
                   return 0 }";
        let program = crate::parser::parse(crate::lexer::lex(tmp).unwrap()).unwrap();
        assert!(
            super::ownership(&program)
                .values()
                .any(|r| r.gone.is_none()),
            "an `if let` over a call result owns what it matched"
        );
    }

    #[test]
    fn a_returned_scalar_parameter_is_not_a_borrow() {
        // The surface only exists where heap is owned (RFC-0089 "What it costs").
        assert!(
            run("fn id(n: Int64) -> Int64 { return n } fn main() -> Int64 { return id(1) }")
                .is_ok()
        );
    }

    #[test]
    fn a_loop_variable_is_a_read_borrow() {
        // The PLAN's decision log: iteration binds a `read` borrow, so the
        // element belongs to the container and returning one is rule 3.
        let src = "fn first(xs: Array<String>) -> String { for x in xs { return x } return \"\" } \
                   fn main() -> Int64 { return 0 }";
        let e = run(src).unwrap_err();
        assert!(
            e.contains("`x` may not be returned") && e.contains("a loop variable"),
            "{e}"
        );
        assert!(run(
            "fn first(xs: Array<String>) -> String { for x in xs { return x.copy() } \
                     return \"\" } fn main() -> Int64 { return 0 }"
        )
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

    /// RFC-0089 rule 3, Phase 6. Module state is nobody's borrow, and that is
    /// where `check_return` let a lend through: `return title` handed the caller
    /// a buffer the module still holds, and the caller freed it at block exit.
    /// The interpreter's values cannot dangle, so the wasm column printed the
    /// next allocation's bytes where the interpreter printed the String.
    #[test]
    fn returning_module_state_is_refused() {
        let src = "let mut title = \"x\" \
                   fn get() -> String { return title } \
                   fn main() -> Int64 { return 0 }";
        let e = run(src).unwrap_err();
        assert!(
            e.contains("`title` may not be returned") && e.contains("module state"),
            "{e}"
        );
        assert!(e.contains("fix: `title.copy()`"), "{e}");
        // A field of module state is the same buffer through one more hop.
        let src = "type R = { s: String } let mut r = R { s: \"x\" } \
                   fn get() -> String { return r.s } \
                   fn main() -> Int64 { return 0 }";
        assert!(
            run(src).unwrap_err().contains("`r.s` may not be returned"),
            "field"
        );
        // The named fix compiles.
        assert!(run(
            "let mut title = \"x\" fn get() -> String { return title.copy() } \
                     fn main() -> Int64 { return 0 }"
        )
        .is_ok());
        // A RECORD of module state is refused too since RFC-0092 M3, and the
        // reason is the row rather than this pass: the gate is "does the caller
        // release the result", and until M3 a record answered no. Handing one
        // out was a leak; it is a use-after-free now, and the same sentence
        // refuses it.
        let e = run("type R = { s: String } let mut r = R { s: \"x\" } \
                     fn get() -> R { return r } fn main() -> Int64 { return 0 }")
        .unwrap_err();
        assert!(
            e.contains("`r` may not be returned") && e.contains("module state"),
            "{e}"
        );
        // A type nobody releases is still not a use-after-free, so it is still
        // not refused: a record of scalars owns no heap and has no row.
        assert!(run("type R = { n: Int64 } let mut r = R { n: 1 } \
                     fn get() -> R { return r } fn main() -> Int64 { return 0 }")
        .is_ok());
    }

    /// An arm-yielded projection is refused for EVERY caller since RFC-0092 M1,
    /// and the export boundary still says something the general rule does not.
    ///
    /// This test used to open by asserting that an ordinary function MAY lend
    /// one — the `lending` set records it and the Vyrn caller releases nothing.
    /// That is the guesser RFC-0092's "Rejected" section exists to remove, and
    /// it was Phase 4b's asymmetry written down: Phase 6 already refused the
    /// direct spelling of the very same program (`return title` on module state,
    /// `movecheck.rs`'s module-state branch), so `return match tag { Word(s) =>
    /// s }` on module state was one program with two verdicts. Both are refused
    /// now, and both name `.copy()`.
    ///
    /// What is still the export's own is the WORDING and the FIX: a JS caller
    /// reads no `lending` set, and since RFC-0089 M3b `wasi-min.js` frees every
    /// String an export hands back.
    #[test]
    fn an_export_may_not_lend_its_result() {
        let enum_and_state = "type Tag = | Word(String) | Num(Int64) \
                              let mut tag = Word(\"w\") ";
        let body = "return match tag { Word(s) => s, Num(n) => \"num\", } } \
                    fn main() -> Int64 { return 0 }";
        // An ordinary function may not lend one either (RFC-0092 M1), and the
        // general refusal is what it gets.
        let e = run(&format!("{enum_and_state} fn text() -> String {{ {body}")).unwrap_err();
        assert!(e.contains("`s` may not be returned"), "{e}");
        assert!(e.contains("read out of a place that owns it"), "{e}");
        assert!(!e.contains("exported function"), "{e}");
        // The export says the same no in its own words, and offers its own fix.
        let e = run(&format!(
            "{enum_and_state} export extern fn text() -> String {{ {body}"
        ))
        .unwrap_err();
        assert!(
            e.contains("may not be returned from an exported function"),
            "{e}"
        );
        assert!(
            e.contains("the JS caller releases what it is handed"),
            "{e}"
        );
        assert!(e.contains("fix: `s.copy()`"), "{e}");
        // The one fix both of them name compiles, either side of the boundary.
        let fixed = "return match tag { Word(s) => s.copy(), Num(n) => \"num\", } } \
                     fn main() -> Int64 { return 0 }";
        assert!(run(&format!("{enum_and_state} fn text() -> String {{ {fixed}")).is_ok());
        assert!(run(&format!(
            "{enum_and_state} export extern fn text() -> String {{ {fixed}"
        ))
        .is_ok());
    }

    /// Phase 6's other half of the menu: inside an `export extern fn` the
    /// `consume` fix does not exist, so it is not offered.
    ///
    /// **Every way of getting the refusal, in one test, because they drifted.**
    /// `check_return` refuses a return from three places, and RFC-0092 M1 put
    /// the `exported` question at one of them, then at two. The third —
    /// `return q`, the plainest spelling there is — kept offering ``declare the
    /// parameter `q: consume ..` ``, which the same compiler then refuses at the
    /// signature (RFC-0089 M3b). All three share
    /// [`MoveCheck::refuse_return`] now, and all three are asserted here so the
    /// next person to touch one has to look at the others.
    #[test]
    fn an_exports_borrow_menu_names_copy_alone() {
        // A store into module state.
        let src = "let mut kept = \"x\" \
                   export extern fn set(arg: String) { kept = arg } \
                   fn main() -> Int64 { return 0 }";
        let e = run(src).unwrap_err();
        assert!(e.contains("fix: `arg.copy()`"), "{e}");
        assert!(
            !e.contains("consume"),
            "an export may not consume a String: {e}"
        );
        // Every spelling of a returned borrow: the parameter named straight at
        // the `return`, a projection of a place the frame owns, and a borrow
        // yielded by an arm. Each reaches `check_return` by a different route.
        let spellings = [
            "export extern fn plain(q: String) -> String { return q }",
            "type D = { s: String } \
             export extern fn field(q: String) -> String \
             { let d = D { s: q.copy() } return d.s }",
            "export extern fn pick(p: String, q: String) -> String \
             { return if p == \"\" { q } else { p } }",
        ];
        for s in spellings {
            let e = run(&format!("{s} fn main() -> Int64 {{ return 0 }}")).unwrap_err();
            assert!(
                e.contains("may not be returned from an exported function"),
                "{s}\n{e}"
            );
            assert!(
                e.contains("the JS caller releases what it is handed"),
                "{s}\n{e}"
            );
            assert!(
                e.contains("an `export extern fn` owns its result"),
                "{s}\n{e}"
            );
            assert!(
                !e.contains("consume"),
                "an export may not consume a String: {s}\n{e}"
            );
        }
        assert!(run("let mut kept = \"x\" \
                     export extern fn set(arg: String) { kept = arg.copy() } \
                     fn main() -> Int64 { return 0 }")
        .is_ok());
    }

    #[test]
    fn rule_2_refuses_a_stored_borrow() {
        let src = "type R = { s: String }                    fn keep(x: String) -> R { return R { s: x } }                    fn main() -> Int64 { return 0 }";
        let e = run(src).unwrap_err();
        assert!(e.contains("may not be stored into the field `R.s`"), "{e}");
        assert!(
            e.contains("fix: declare the parameter `x: consume ..`"),
            "{e}"
        );
        // Both named fixes work.
        assert!(run("type R = { s: String } fn keep(x: consume String) -> R { return R { s: x } }                      fn main() -> Int64 { return 0 }")
            .is_ok());
        assert!(run("type R = { s: String } fn keep(x: String) -> R { return R { s: x.copy() } }                      fn main() -> Int64 { return 0 }")
            .is_ok());
    }

    #[test]
    fn a_stored_loop_variable_names_the_consuming_form() {
        // The half of rule 2 the corpus is made of, and the fix that is not a
        // copy: the loop takes the container.
        let src = "fn go(xs: Array<String>) -> Int64 { let mut out: Array<String> = []                    for x in xs { out.push(x) } return out.length }                    fn main() -> Int64 { return 0 }";
        let e = run(src).unwrap_err();
        assert!(
            e.contains("fix: `for x in consume xs` if the loop should take"),
            "{e}"
        );
        // Taking the container needs the function to own it, and the diagnostic
        // says so before it says `copy`.
        assert!(e.contains("fix: `x.copy()`"), "{e}");
    }

    #[test]
    fn a_consuming_loop_owns_its_elements() {
        // `for x in consume xs`: the container is the loop's, so storing an
        // element is a move and needs no copy.
        assert!(run("fn go() -> Int64 { let xs: Array<String> = [\"a\" + \"b\"]                      let mut out: Array<String> = []                      for x in consume xs { out.push(x) } return out.length }                      fn main() -> Int64 { return 0 }")
            .is_ok());
        // And the container is dead afterwards — rule 1's own error.
        let src = "fn go() -> Int64 { let xs: Array<String> = [\"a\" + \"b\"]                    let mut out: Array<String> = []                    for x in consume xs { out.push(x) } return xs.length }                    fn main() -> Int64 { return 0 }";
        let e = run(src).unwrap_err();
        assert!(
            e.contains("`xs` was moved here into the `for .. in consume` loop"),
            "{e}"
        );
    }

    #[test]
    fn a_consuming_loop_needs_a_container_it_may_take() {
        // A borrow is not the loop's to give away, and the two-step fix says so.
        let src = "fn go(xs: Array<String>) -> Int64 { let mut out: Array<String> = []                    for x in consume xs { out.push(x) } return out.length }                    fn main() -> Int64 { return 0 }";
        let e = run(src).unwrap_err();
        assert!(
            e.contains("`xs` may not be consumed — it is a `read` parameter"),
            "{e}"
        );
        assert!(run("fn go(xs: consume Array<String>) -> Int64 { let mut out: Array<String> = []                      for x in consume xs { out.push(x) } return out.length }                      fn main() -> Int64 { return 0 }")
            .is_ok());
        // A field of a BORROWED record is still refused, and RFC-0093 moved the
        // reason to the true one: the frame does not own `r`, so it may not give
        // any part of it away. The old refusal said "would leave a hole" and
        // offered `for .. in consume r`, which does not typecheck for a record —
        // one question, two answers. That branch is gone.
        let src = "type R = { xs: Array<String> }                    fn go(r: R) -> Int64 { let mut out: Array<String> = []                    for x in consume r.xs { out.push(x) } return out.length }                    fn main() -> Int64 { return 0 }";
        let e = run(src).unwrap_err();
        assert!(
            e.contains("`r` may not be consumed — it is a `read` parameter"),
            "{e}"
        );
        assert!(!e.contains("for .. in consume r`"), "{e}");
        // A field of a record this frame OWNS is the take, and it is legal now.
        assert!(run("type R = { xs: Array<String> }                      fn make() -> R { return R { xs: [\"a\"] } }                      fn go() -> Int64 { let r = make() let mut out: Array<String> = []                      for x in consume r.xs { out.push(x) } return out.length }                      fn main() -> Int64 { return 0 }")
            .is_ok());
        // Module state lives for the whole module and is nobody's to take.
        let src = "let g: Array<String> = []                    fn go() -> Int64 { let mut out: Array<String> = []                    for x in consume g { out.push(x) } return out.length }                    fn main() -> Int64 { return 0 }";
        let e = run(src).unwrap_err();
        assert!(e.contains("module state `g` may not be consumed"), "{e}");
    }

    #[test]
    fn a_loop_over_a_temporary_owns_its_elements() {
        // 91 of the corpus sites. `for o in diff(..)` iterates a container
        // nobody else holds, so the elements are the loop's with no word for it.
        assert!(run("fn make() -> Array<String> { return [\"a\" + \"b\"] }                      fn go() -> Int64 { let mut out: Array<String> = []                      for x in make() { out.push(x) } return out.length }                      fn main() -> Int64 { return 0 }")
            .is_ok());
        // An element read is NOT a temporary: `xs[i]` is a place the container
        // still owns, so a loop over one still borrows.
        let src = "fn go(xs: Array<Array<String>>) -> Int64 { let mut out: Array<String> = []                    for x in xs[0] { out.push(x) } return out.length }                    fn main() -> Int64 { return 0 }";
        let e = run(src).unwrap_err();
        assert!(e.contains("may not be stored"), "{e}");
    }

    #[test]
    fn consume_before_an_iterable_is_contextual() {
        // `consume` is a capability only when an identifier follows it, so a
        // user function called `consume` is untouched — the same rule a
        // parameter's capability follows.
        assert!(run("fn consume(n: Int64) -> Array<Int64> { return [n] }                      fn main() -> Int64 { let mut t = 0 for x in consume(1) { t = t + x }                      return t }")
            .is_ok());
        // The prefix reads the word at a third position and is contextual by the
        // same test: `consume(1)` in an argument is still a call (RFC-0093).
        assert!(run("fn consume(n: Int64) -> Array<Int64> { return [n] }                      fn take(xs: consume Array<Int64>) -> Int64 { return xs.length }                      fn main() -> Int64 { return take(consume(1)) }")
            .is_ok());
    }

    // ---- RFC-0093: the take ---------------------------------------------

    /// The whole of the path-keyed hole, in the four sentences that define it.
    #[test]
    fn a_take_empties_one_path_and_leaves_the_rest() {
        const DECLS: &str = "type Bag = { a: String, b: String } \
                             fn make() -> Bag { return Bag { a: \"x\" + \"y\", b: \"p\" + \"q\" } } ";
        let go = |body: &str| {
            run(&format!(
                "{DECLS} fn go() -> Int64 {{ {body} }} fn main() -> Int64 {{ return 0 }}"
            ))
        };

        // A sibling field survives the take. This is the sentence a whole-root
        // move cannot say, and the nine-line drain in `std/vyx` is made of it.
        assert!(go("let d = make() let mut o: Array<String> = [] o.push(consume d.a) o.push(consume d.b) return o.length").is_ok());
        // The taken path does not.
        let e = go("let d = make() let mut o: Array<String> = [] o.push(consume d.a) return d.a.byteLength").unwrap_err();
        assert!(e.contains("`d.a` was moved here into `consume`"), "{e}");
        // Nor does the root as a whole: the record has a hole in it.
        let e = go("let d = make() let mut o: Array<String> = [] o.push(consume d.a) let t = d return t.b.byteLength").unwrap_err();
        assert!(e.contains("`d.a` was taken out of `d` here"), "{e}");
        assert!(e.contains("with the hole still in it"), "{e}");
        // A write fills the hole.
        assert!(go("let mut d = make() let mut o: Array<String> = [] o.push(consume d.a) d.a = \"z\" return d.a.byteLength").is_ok());
    }

    /// The union at a branch join: a take on one arm is a take on both, so the
    /// arm that did not take leaks rather than freeing something twice.
    #[test]
    fn a_take_on_one_branch_is_a_take_on_both() {
        let src = "type Bag = { a: String } \
                   fn make() -> Bag { return Bag { a: \"x\" + \"y\" } } \
                   fn go(n: Int64) -> Int64 { let d = make() let mut o: Array<String> = [] \
                       if n > 0 { o.push(consume d.a) } \
                       return d.a.byteLength } \
                   fn main() -> Int64 { return 0 }";
        let e = run(src).unwrap_err();
        assert!(e.contains("`d.a` was moved here into `consume`"), "{e}");
    }

    /// The three refusals that stay, each with the menu it prints.
    #[test]
    fn a_take_needs_a_place_the_frame_owns() {
        const DECLS: &str = "type Bag = { a: String } \
                             let g: String = \"m\" \
                             fn fresh() -> String { return \"f\" } ";
        let go = |sig: &str, body: &str| {
            run(&format!("{DECLS} fn go({sig}) -> Int64 {{ let mut o: Array<String> = [] {body} return o.length }} fn main() -> Int64 {{ return 0 }}"))
        };
        // A borrowed root: the frame does not own it, so it may not give it away.
        let e = go("d: read Bag", "o.push(consume d.a)").unwrap_err();
        assert!(
            e.contains("`d` may not be consumed — it is a `read` parameter"),
            "{e}"
        );
        // Module state is nobody's to take.
        let e = go("", "o.push(consume g)").unwrap_err();
        assert!(
            e.contains("module state `g` may not be consumed by a take"),
            "{e}"
        );
        // A fresh value is already owned — there is no place to leave a hole in.
        let e = go("", "o.push(consume fresh())").unwrap_err();
        assert!(e.contains("nothing to take"), "{e}");
        // An element is the one place that CAN hold a hole at run time, and
        // `swapRemove` already spells it.
        let e = go("", "let mut xs: Array<String> = [] o.push(consume xs[0])").unwrap_err();
        assert!(e.contains("`xs.swapRemove(..)`"), "{e}");
    }

    /// The menu RFC-0092 M1 could only answer with `.copy()` names the take
    /// first now — and only where `check_take` would accept it.
    #[test]
    fn the_projection_menu_offers_the_take_where_it_exists() {
        const DECLS: &str = "type Bag = { a: String } \
                             fn make() -> Bag { return Bag { a: \"x\" + \"y\" } } ";
        let owned = run(&format!("{DECLS} fn go() -> Int64 {{ let d = make() let mut o: Array<String> = [] o.push(d.a) return o.length }} fn main() -> Int64 {{ return 0 }}")).unwrap_err();
        assert!(
            owned.contains("fix: `consume d.a` if `d` should give it up"),
            "{owned}"
        );
        // A borrowed root has no take, so the menu must not name one.
        let borrowed = run(&format!("{DECLS} fn go(d: read Bag) -> Int64 {{ let mut o: Array<String> = [] o.push(d.a) return o.length }} fn main() -> Int64 {{ return 0 }}")).unwrap_err();
        assert!(!borrowed.contains("`consume d.a`"), "{borrowed}");
    }

    // ---- rule 2 at the third exit: a borrow may not be consumed -----------

    /// The shape that opened this: a `read` parameter reaching a `consume`
    /// parameter. `vyrn check` said `ok`, the interpreter printed an answer by
    /// refcounting, and the native binary exited `0xC0000374`.
    #[test]
    fn a_borrowed_parameter_may_not_reach_a_consume_parameter() {
        const TAKE: &str = "fn take(xs: consume Array<Int64>) -> Int64 \
                            { let n = xs.length drop xs return n } ";
        let go = |sig: &str, body: &str| {
            run(&format!(
                "{TAKE} fn go({sig}) -> Int64 {{ {body} }} fn main() -> Int64 {{ return 0 }}"
            ))
        };

        let e = go("ys: read Array<Int64>", "return take(ys)").unwrap_err();
        assert!(
            e.contains(
                "`ys` may not be passed to a `consume` parameter via `take(..)` — it is a \
                 `read` parameter"
            ),
            "{e}"
        );
        // The menu is the two real answers, in the order a reader should try them.
        assert!(
            e.contains("fix: declare the parameter `ys: consume ..`"),
            "{e}"
        );
        assert!(e.contains("fix: `ys.copy()`"), "{e}");

        // A `modify` parameter is exclusive access to somebody else's storage,
        // not ownership of it.
        let e = go("ys: modify Array<Int64>", "return take(ys)").unwrap_err();
        assert!(e.contains("it is a `modify` parameter"), "{e}");

        // Both fixes compile.
        assert!(go("ys: consume Array<Int64>", "return take(ys)").is_ok());
        assert!(go("ys: read Array<Int64>", "return take(ys.copy())").is_ok());
        // And a value this frame owns still moves, as it always did.
        assert!(go("", "let xs: Array<Int64> = [1] return take(xs)").is_ok());
    }

    /// One fact, one verdict, however the borrow is spelled at the call.
    #[test]
    fn the_borrow_travels_to_every_spelling_of_the_argument() {
        const DECLS: &str = "type Bag = { xs: Array<Int64> } \
                             fn take(xs: consume Array<Int64>) -> Int64 \
                             { let n = xs.length drop xs return n } ";
        let go = |sig: &str, body: &str| {
            run(&format!(
                "{DECLS} fn go({sig}) -> Int64 {{ {body} }} fn main() -> Int64 {{ return 0 }}"
            ))
        };

        // A field of a borrowed record.
        let e = go("b: read Bag", "return take(b.xs)").unwrap_err();
        assert!(e.contains("`b.xs` may not be passed"), "{e}");
        // An element of a borrowed container, and the same element through a
        // `let` — the spelling that used to bind a bare projection and pass.
        let e = go("ns: read Array<Array<Int64>>", "return take(ns[0])").unwrap_err();
        assert!(e.contains("`ns[0]` may not be passed"), "{e}");
        let e = go(
            "ns: read Array<Array<Int64>>",
            "let n = ns[0] return take(n)",
        )
        .unwrap_err();
        assert!(e.contains("`n` may not be passed"), "{e}");
        // A pattern binder over a borrowed scrutinee.
        let e = go(
            "o: read Option<Array<Int64>>",
            "return match o { Some(v) => take(v), None => 0 }",
        )
        .unwrap_err();
        assert!(e.contains("`v` may not be passed"), "{e}");
        // A loop variable: the container still owns the element.
        let e = go(
            "",
            "let ns: Array<Array<Int64>> = [[1]] let mut t = 0 for r in ns { t = t + take(r) } \
             return t",
        )
        .unwrap_err();
        assert!(e.contains("it is a loop variable"), "{e}");
        assert!(e.contains("fix: `for r in consume ns`"), "{e}");
    }

    /// What the rule refuses past a borrowed root, and the one projected shape
    /// it still waves through.
    #[test]
    fn the_third_exit_refuses_a_projected_argument() {
        const DECLS: &str = "type Bag = { xs: Array<Int64> } \
                             let g: Array<Int64> = [1] \
                             let gb: Bag = Bag { xs: [1] } \
                             fn take(xs: consume Array<Int64>) -> Int64 \
                             { let n = xs.length drop xs return n } ";
        let go = |body: &str| {
            run(&format!(
                "{DECLS} fn go() -> Int64 {{ {body} }} fn main() -> Int64 {{ return 0 }}"
            ))
        };

        // A field of a place THIS frame owns used to pass silently: the callee
        // dropped the array and the record's release freed the same buffer
        // again. Refused, with the prefix take — the spelling whose check_take
        // + hole record the hand-off — first in the menu.
        let e = go("let b = Bag { xs: [1] } return take(b.xs)").unwrap_err();
        assert!(e.contains("`b.xs` may not be passed"), "{e}");
        assert!(e.contains("fix: `consume b.xs`"), "{e}");
        // And the take spelling compiles.
        assert!(go("let b = Bag { xs: [1] } let ys = consume b.xs return take(ys)").is_ok());
        // An element of an owned container is the same fact; it has no take, so
        // the menu names the two spellings that exist for it.
        let e = go("let nn: Array<Array<Int64>> = [[1]] return take(nn[0])").unwrap_err();
        assert!(e.contains("`nn[0]` may not be passed"), "{e}");
        assert!(e.contains("fix: `nn[0].copy()`"), "{e}");
        // A projection of module state keeps RFC-0013's sentence.
        let e = go("return take(gb.xs)").unwrap_err();
        assert!(
            e.contains("`gb.xs` may not be passed to a `consume` parameter")
                && e.contains("module state"),
            "{e}"
        );
        // Module state as a WHOLE keeps its own sentence at the call arm.
        let e = go("return take(g)").unwrap_err();
        assert!(
            e.contains("module state `g` may not be passed to a `consume` parameter"),
            "{e}"
        );
        // A binder over an OWNED scrutinee is the one projected shape still
        // waved through: it is keyed to the scrutinee's row and the arm records
        // the hand-off, which is the drain `std/html`'s `keyed` is written as.
        assert!(go("let o: Option<Array<Int64>> = Some([1]) \
             return match o { Some(v) => take(v), None => 0 }")
        .is_ok());
        // An ordinary `let` of the same projection is refused like the spelling
        // it hides: the frame still owns the aggregate.
        let e = go("let d = Bag { xs: [1] } let t = d.xs return take(t)").unwrap_err();
        assert!(e.contains("`t` may not be passed"), "{e}");
        // A linear type answers with its own obligation and never reaches this
        // rule — the menu here offers `.copy()`, which a stream has no answer
        // for. Handing a stream PARAMETER on is how a stream is disposed
        // (RFC-0075: every mention of a stream binding is a disposal), so this
        // rule must not read it as a borrow given away.
        assert!(run(
            "fn takeS(s: consume Stream<Int64>) -> Int64 { close(s) return 0 } \
                 fn go(s: Stream<Int64>) -> Int64 { return takeS(s) } \
                 fn main() -> Int64 { return 0 }"
        )
        .is_ok());
    }

    /// A lambda handed to a callee that may KEEP it escapes, so its captures
    /// are checked like a stored closure's (RFC-0037). A `consume fn`
    /// parameter owns what it is handed and can store it; storing one whose
    /// capture borrows the frame leaves the capture dangling.
    #[test]
    fn a_lambda_at_a_consume_parameter_escapes() {
        let e = run(
            "fn reg(f: consume fn(Int64) -> Int64) -> Int64 { return f(0) } \
             fn go(q: read String) -> Int64 { return reg(n -> n + q.byteLength) } \
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(
            e.contains("may not be captured by a closure that outlives this call"),
            "{e}"
        );
        // An owned capture is safe: the closure takes it along, and the block
        // releases nothing it gave up.
        assert!(run(
            "fn reg(f: consume fn(Int64) -> Int64) -> Int64 { return f(0) } \
             fn go() -> Int64 { let s = \"a\" + \"b\" return reg(n -> n + s.byteLength) } \
             fn main() -> Int64 { return 0 }"
        )
        .is_ok());
        // The fast path survives: a parameter that provably borrows keeps the
        // non-escaping assumption, which is every `map`-style call.
        assert!(
            run("fn apply(f: fn(Int64) -> Int64) -> Int64 { return f(0) } \
             fn go(q: read String) -> Int64 { return apply(n -> n + q.byteLength) } \
             fn main() -> Int64 { return 0 }")
            .is_ok()
        );
    }

    /// A lender forwarded through an AGGREGATE is still a lender: `calls_in`
    /// has to see through the literal, or the wrapper goes unmarked and its
    /// caller reclaims storage the lender's own caller still owns.
    #[test]
    fn a_lender_forwarded_through_an_aggregate_is_still_marked_lending() {
        let src = "type R = { name: String } \
                   fn pick(xs: Array<String>) -> String \
                   { for x in xs { return if true { x } else { \"\" } } return \"\" } \
                   fn g(a: Array<String>) -> R { return R { name: pick(a) } } \
                   fn h(a: Array<String>) -> Array<String> { return [pick(a)] } \
                   fn main() -> Int64 { let arr: Array<String> = [\"a\" + \"b\"] \
                   let r = g(arr) let s2 = h(arr) \
                   return r.name.byteLength + s2[0].byteLength }";
        let program = crate::parser::parse(crate::lexer::lex(src).unwrap()).unwrap();
        assert!(super::check(&program).is_ok());
        assert!(
            super::ownership(&program).values().all(|r| {
                let fc = r.from_call.as_deref();
                (fc != Some("g") && fc != Some("h")) || r.gone.is_some()
            }),
            "a lender forwarded through an aggregate must never be reclaimed by its caller"
        );
    }

    /// A shadowing `let` inside a region revives only the region's own map: the
    /// outer binding's consumption record must survive the region, or the
    /// use-after-move after it compiles.
    #[test]
    fn a_region_let_does_not_revive_the_outer_binding() {
        let src = "fn sink(t: consume String) -> Int64 { drop t return 0 } \
                   fn main() -> Int64 { let x = \"a\" + \"b\" let n = sink(x) \
                   region { let x = 9 } return x.byteLength + n }";
        let e = run(src).unwrap_err();
        assert!(e.contains("already consumed"), "{e}");
    }

    /// The linear walk reads unreachable code the way [`MoveCheck::block`] does:
    /// a mention after a diverging statement is not a second disposal.
    #[test]
    fn a_mention_in_unreachable_code_is_not_a_second_disposal() {
        assert!(stream("let s = feed() close(s) return 0 close(s)").is_ok());
        assert!(stream("let s = feed() close(s) panic(\"gone\") close(s)").is_ok());
        // A REACHABLE second disposal is still refused.
        let e = stream("let s = feed() close(s) let n = 0 close(s) return 0").unwrap_err();
        assert!(e.contains("disposed more than once"), "{e}");
    }

    /// `spawn f(x)` moves its arguments exactly as a direct call does, so it asks
    /// the same question.
    #[test]
    fn a_spawned_call_asks_the_same_question() {
        let src = "fn take(xs: consume Array<Int64>) -> Int64 \
                   { let n = xs.length drop xs return n } \
                   fn go(ys: read Array<Int64>) -> Int64 { let t = spawn take(ys) return 0 } \
                   fn main() -> Int64 { return 0 }";
        let e = run(src).unwrap_err();
        assert!(
            e.contains("`ys` may not be passed to a `consume` parameter via `take(..)`"),
            "{e}"
        );
    }

    /// A method is the same rule under its surface name: a `read self` may not
    /// hand itself, or a field of itself, to a `consume` parameter.
    #[test]
    fn a_read_receiver_may_not_be_consumed() {
        const DECLS: &str = "type Bag = { xs: Array<Int64> } \
                             fn take(xs: consume Array<Int64>) -> Int64 \
                             { let n = xs.length drop xs return n } \
                             fn takeBag(b: consume Bag) -> Int64 { return b.xs.length } ";
        let go = |body: &str| {
            run(&format!(
                "{DECLS} protocol Giving {{ fn give(read self) -> Int64 }} \
                 impl Giving for Bag {{ fn give(read self) -> Int64 {{ {body} }} }} \
                 fn main() -> Int64 {{ return 0 }}"
            ))
        };
        let e = go("return takeBag(self)").unwrap_err();
        assert!(e.contains("`self` may not be passed"), "{e}");
        let e = go("return take(self.xs)").unwrap_err();
        assert!(e.contains("`self.xs` may not be passed"), "{e}");
    }

    #[test]
    fn a_modify_borrow_is_exclusive() {
        let src = "fn f(a: modify Array<Int64>, b: Array<Int64>) -> Int64 { return a.length } \
                   fn main() -> Int64 { let mut xs: Array<Int64> = [] return f(xs, xs) }";
        let e = run(src).unwrap_err();
        assert!(
            e.contains("as `modify` and read again in the same call"),
            "{e}"
        );
        assert!(e.contains("fix: `xs.copy()`"), "{e}");
    }

    #[test]
    fn a_modify_receiver_is_exclusive_too() {
        // The receiver form of the rule above. It falls out of one check, but
        // only because the PROTOCOL carries the capability: a method call
        // reaches this pass under its surface name (`merge`), and the impl it
        // will dispatch to is flattened under a mangled one this pass never
        // sees. Without the protocol's declaration there is nothing under
        // `merge` and the rule goes silent.
        let src = "type T = { n: Int64 } \
                   protocol Merging { fn merge(modify self, other: T) -> Unit } \
                   impl Merging for T { fn merge(modify self, other: T) -> Unit \
                   { self.n = self.n + other.n } } \
                   fn main() -> Int64 { let mut t = T { n: 1 } t.merge(t) return 0 }";
        let e = run(src).unwrap_err();
        assert!(
            e.contains("as `modify` and read again in the same call"),
            "{e}"
        );
    }

    #[test]
    fn a_consume_parameter_of_a_method_still_moves() {
        // The same map, for the other capability. `insert(s, v)` moving `v` and
        // `s.insert(v)` not moving it would be two languages.
        let src = "type T = { n: Int64 } \
                   protocol Taking { fn take(modify self, s: consume String) -> Unit } \
                   impl Taking for T { fn take(modify self, s: consume String) -> Unit \
                   { self.n = self.n + s.byteLength } } \
                   fn main() -> Int64 { let mut t = T { n: 1 } let s = \"a\" \
                   t.take(s) return s.byteLength }";
        let e = run(src).unwrap_err();
        assert!(e.contains("already consumed by `take(..)`"), "{e}");
    }

    #[test]
    fn a_map_key_may_not_be_a_borrow() {
        // `m[k] = v` used to be the one store that did not ask rule 2, while the
        // map LITERAL asked it — one fact, two spellings, two verdicts. Both
        // spellings of the borrow are here: the element read inline, and the
        // `for` variable over a borrowed container.
        let inline = "fn build(ks: Array<String>) -> Map<String, Int64> \
                      { let mut m: Map<String, Int64> = [:] m[ks[0]] = 1 return m } \
                      fn main() -> Int64 { return 0 }";
        let e = run(inline).unwrap_err();
        assert!(e.contains("`ks[0]` may not be stored into `m`"), "{e}");
        assert!(e.contains("`ks[0].copy()`"), "{e}");

        let loop_var = "fn build(ks: Array<String>) -> Map<String, Int64> \
                        { let mut m: Map<String, Int64> = [:] \
                        for k in ks { m[k] = 1 } return m } \
                        fn main() -> Int64 { return 0 }";
        let e = run(loop_var).unwrap_err();
        assert!(e.contains("`k` may not be stored into `m`"), "{e}");

        // A copy is a value of this frame's, and so is a key with no other
        // owner — neither is refused.
        assert!(run("fn build(ks: Array<String>) -> Map<String, Int64> \
                 { let mut m: Map<String, Int64> = [:] m[ks[0].copy()] = 1 return m } \
                 fn main() -> Int64 { return 0 }")
        .is_ok());
        assert!(
            run("fn main() -> Int64 { let mut m: Map<String, Int64> = [:] \
                 m[\"a\" + \"b\"] = 1 return 0 }")
            .is_ok()
        );
    }

    #[test]
    fn an_escaping_closure_may_not_capture_a_borrow() {
        // A lambda whose callee provably borrows it (`apply`'s parameter is a
        // plain borrow) does not outlive the call, so it captures freely; one
        // that is stored, or handed to a `consume fn` parameter that may keep
        // it, is a value and may not.
        assert!(
            run("fn apply(f: fn(Int64) -> Int64) -> Int64 { return f(1) } \
                     fn go(s: String) -> Int64 { return apply(n -> n + s.byteLength) } \
                     fn main() -> Int64 { return 0 }")
            .is_ok()
        );
        let src = "fn go(s: String) -> Int64 { let f = n -> n + s.byteLength return f(1) } \
                   fn main() -> Int64 { return 0 }";
        let e = run(src).unwrap_err();
        assert!(
            e.contains("may not be captured by a closure that outlives this call"),
            "{e}"
        );
    }

    // ---- RFC-0075: the disposal obligation -------------------------------

    /// The producer every stream case below acquires from, and a consumer that
    /// discharges one — a call, so it fits in an expression position.
    const FEED: &str = "fn feed() -> Stream<Int64> { let xs: Array<Int64> = [1, 2] \
                        return fromArray(xs) } \
                        fn drain(s: Stream<Int64>) -> Int64 { let mut t = 0 \
                        for v in s { t = t + v } return t } ";

    fn stream(body: &str) -> Result<(), String> {
        run(&format!("{FEED} fn main() -> Int64 {{ {body} }}"))
    }

    #[test]
    fn an_abandoned_stream_does_not_build() {
        // The milestone's whole claim: the `#6193` shape is a compile error.
        let e = stream("let events = feed() return 0").unwrap_err();
        assert!(
            e.contains("`events` is a `Stream<Int64>` and is never disposed"),
            "{e}"
        );
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
        // A lazy wrapper's source is DISCHARGED by `boxStream`, which is an
        // ordinary mention of the binding and therefore an ordinary move; the
        // stream the wrapper hands back is a new obligation. Both halves matter,
        // and RFC-0090 M3 added a third: the source comes back out of the box
        // with `unboxStream`, which ACQUIRES one — so a wrapper's own release
        // path is checked here rather than trusted to a walk inside the runtime.
        let base = "fn tick(sl: Int64, gn: Int64, cl: Bool) -> Option<Int64> { \
                    if cl { return None } return Some(sl) } ";
        let src = format!(
            "{base} fn main() -> Int64 {{ let s = fromStep(0, 1, tick) \
             let a = boxStream(s) return 0 }}"
        );
        // The box is not a disposal: whatever holds the address owes the stream.
        assert!(run(&src).is_ok(), "the wrapper owes it, not `main`");
        let src = format!(
            "{base} fn main() -> Int64 {{ let s = fromStep(0, 1, tick) \
             let a = boxStream(s) let t: Stream<Int64> = unboxStream(a) return 0 }}"
        );
        let e = run(&src).unwrap_err();
        assert!(
            e.contains("`t` is a `Stream<Int64>` and is never disposed"),
            "{e}"
        );
        let src = format!(
            "{base} fn main() -> Int64 {{ let s = fromStep(0, 1, tick) \
             let a = boxStream(s) let t: Stream<Int64> = unboxStream(a) close(t) return 0 }}"
        );
        assert!(run(&src).is_ok());
        // And the source may not be closed as well as boxed — that is the double
        // release the wrapper's own close would then complete.
        let src = format!(
            "{base} fn main() -> Int64 {{ let s = fromStep(0, 1, tick) \
             let a = boxStream(s) close(s) return 0 }}"
        );
        let e = run(&src).unwrap_err();
        assert!(e.contains("is disposed more than once"), "{e}");
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

    /// RFC-0095 M3. "Every path" now reaches into an ARM.
    ///
    /// The `if` STATEMENT was refused from RFC-0075 M1, because [`scan`] walks
    /// its two blocks. A `match` is an expression, so the walk read the whole
    /// statement at once with `mentions` — "some path names it" — and treated
    /// that as a disposal on all of them. One `||` was the difference between
    /// the two spellings of one program.
    #[test]
    fn an_arm_is_a_path_like_a_branch_is() {
        let pick = "let o: Option<Int64> = Some(1) ";
        // Disposed in one arm and not the other: refused, both spellings.
        let e = stream(&format!(
            "{pick} let s = feed() let n = match o {{ Some(k) => drain(s) + k, None => 0 }} \
             return n"
        ))
        .unwrap_err();
        assert!(
            e.contains("`s` is a `Stream<Int64>` and is never disposed"),
            "{e}"
        );
        let e = stream(&format!(
            "{pick} let s = feed() let n = if true {{ drain(s) }} else {{ 0 }} return n"
        ))
        .unwrap_err();
        assert!(e.contains("never disposed"), "{e}");
        // The same shape returned rather than bound — the `return` reads its
        // expression the same way.
        let e = stream(&format!(
            "{pick} let s = feed() return match o {{ Some(k) => drain(s), None => 0 }}"
        ))
        .unwrap_err();
        assert!(e.contains("never disposed"), "{e}");
        // EVERY arm disposes: accepted. `examples/branchtypes.vyrn` is this
        // shape, and it must keep compiling.
        assert!(stream(&format!(
            "{pick} let s = feed() let n = match o {{ Some(k) => drain(s) + k, \
                 None => drain(s) }} return n"
        ))
        .is_ok());
        assert!(stream(&format!(
            "{pick} let s = feed() let n = if true {{ drain(s) }} else {{ drain(s) }} return n"
        ))
        .is_ok());
        // The scrutinee runs whatever arm is taken, so a disposal there is one
        // on every path.
        assert!(stream(
            "let s = feed() let n = match Some(drain(s)) { Some(k) => k, None => 0 } \
                    return n"
        )
        .is_ok());
    }

    /// The limit RFC-0095 M3 recorded and did not close: a branch ACQUIRES in
    /// each arm, and the binding it acquires into inherited nothing.
    ///
    /// The RFC wrote the shape as `let t2 = match c { A => t, B => u }` over two
    /// live bindings, and that spelling is already refused — at `t`, which one
    /// arm hands on and the other does not, which is M3's own rule. The shape
    /// that reaches the hole acquires in the arm instead, so no earlier binding
    /// is there to answer, and the program was accepted with a stream nobody
    /// answered for.
    #[test]
    fn a_branch_acquires_into_the_binding() {
        let pick = "let o: Option<Int64> = Some(1) ";
        let e = stream(&format!(
            "{pick} let t = match o {{ Some(k) => feed(), None => feed() }} return 0"
        ))
        .unwrap_err();
        assert!(
            e.contains("`t` is a `Stream<Int64>` and is never disposed"),
            "{e}"
        );
        // Disposing it is the fix, and it is accepted.
        assert!(stream(&format!(
            "{pick} let t = match o {{ Some(k) => feed(), None => feed() }} close(t) return 0"
        ))
        .is_ok());
        // The if-expression spelling of the same program.
        let e = stream("let t = if true { feed() } else { feed() } return 0").unwrap_err();
        assert!(e.contains("is never disposed"), "{e}");
        // An arm that acquires nothing leaves the binding alone.
        assert!(stream("let n = if true { 1 } else { 2 } return n").is_ok());
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
        assert!(
            e.contains("`s` is a `Stream<Int64>` and is never disposed"),
            "{e}"
        );
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
        assert!(
            e.contains("`m` is a `Stream<Int64>` and is never disposed"),
            "{e}"
        );

        // A combinator that drops its argument on the floor does not build.
        let e = run(&format!(
            "{FEED} fn sink(s: Stream<Int64>) -> Stream<Int64> {{ return feed() }} \
             fn main() -> Int64 {{ close(sink(feed())) return 0 }}"
        ))
        .unwrap_err();
        assert!(
            e.contains("`s` is a `Stream<Int64>` and is never disposed"),
            "{e}"
        );

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
    fn a_stream_producer_takes_what_it_is_handed() {
        // RFC-0092 M5. A buffer-tagged stream's close frees the array's buffer,
        // and a stepped one's close frees the step's capture block. So the frame
        // must not release either a second time — it did, and the native binary
        // corrupted its heap.
        let moved = |src: &str| {
            let p = crate::parser::parse(crate::lexer::lex(src).unwrap()).unwrap();
            ownership(&p)
                .values()
                .any(|r| matches!(&r.gone, Some(Gone::Moved { .. })))
        };
        assert!(moved(
            "fn main() -> Int64 { let xs: Array<Int64> = [1, 2] let s = fromArray(xs) \
             for v in s { print(v) } return 0 }"
        ));
        assert!(moved(
            "fn main() -> Int64 { let n = 1 \
             let run: fn(Int64, Int64, Bool) -> Option<Int64> = (a, b, c) -> { return Some(n) } \
             let s = fromStep(0, 0, run) close(s) return 0 }"
        ));

        // Reading the name afterwards is the rule 1 error, and it says where the
        // array went.
        let e = run("fn main() -> Int64 { let xs: Array<Int64> = [1, 2] \
                     let s = fromArray(xs) close(s) return xs.length }")
        .unwrap_err();
        assert!(e.contains("moved here into `fromArray(..)`"), "{e}");

        // A `read` parameter's buffer may not go into a stream: the caller still
        // owns it, and the stream's close would free it.
        let e = run(
            "fn mk(xs: Array<Int64>) -> Stream<Int64> { return fromArray(xs) } \
                     fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("may not be stored into `fromArray(..)`"), "{e}");
    }

    /// `boxStream` and `serveStream` — the two the census counted as carrying
    /// their ownership fact **nowhere at all** (Q1). They carry it on the TYPE,
    /// and this is where that is written down.
    ///
    /// Each takes a `Stream<T>` and hands it away for good. A second call on one
    /// binding is a double free, and each has exactly one corpus caller — which
    /// the census read as "the only reason no heap has been corrupted". The
    /// reason is stronger than that: `Stream<T>` is linear, every mention of a
    /// stream binding is a disposal in the must-use walk, and a second mention
    /// is refused whatever the name is. The signatures now say `consume` as
    /// well, so the fact is legible; the refusal was always there.
    #[test]
    fn a_stream_is_handed_away_once_however_it_is_handed_away() {
        for call in ["boxStream(s)", "serveStream(s)"] {
            let e = run(&format!(
                "{FEED} fn go(s: Stream<Int64>) -> Int64 {{ let a = {call} let b = {call} \
                 return 0 }} fn main() -> Int64 {{ return go(feed()) }}"
            ))
            .unwrap_err();
            assert!(e.contains("disposed more than once"), "{call}: {e}");
        }
        // And on a local, where the binding is the frame's own.
        let e = run(&format!(
            "{FEED} fn main() -> Int64 {{ let s = feed() let a = boxStream(s) \
             let b = boxStream(s) return 0 }}"
        ))
        .unwrap_err();
        assert!(e.contains("disposed more than once"), "{e}");
        // One is fine — the box owes the release, which `close` after
        // `unboxStream` discharges.
        assert!(run(&format!(
            "{FEED} fn main() -> Int64 {{ let s = feed() let a = boxStream(s) \
             let back: Stream<Int64> = unboxStream(a) close(back) return 0 }}"
        ))
        .is_ok());
    }

    #[test]
    fn a_generic_producer_is_quoted_as_plain_stream() {
        // `Stream<U>` at a call site names a type parameter the program never
        // wrote. This pass has no types, so it under-specifies instead — the
        // same `Stream` it has always used for `fromArray`.
        //
        // `consume` on the parameter, because `fromArray` TAKES the array
        // (RFC-0092 M5): the stream's close frees the buffer, so a `read`
        // parameter's buffer may not go into one. The rule refuses it and names
        // `consume` on the menu; this test was written before it did.
        let e = run(
            "fn mk<T>(xs: consume Array<T>) -> Stream<T> { return fromArray(xs) } \
                     fn main() -> Int64 { let s = mk([1, 2]) return 0 }",
        )
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
                   return apply(p -> p.byteLength + s.byteLength, \"c\") }";
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
            let Ok(src) = std::fs::read_to_string(path) else {
                continue;
            };
            let Ok(tokens) = crate::lexer::lex(&src) else {
                continue;
            };
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

    /// `rfcs/census-call-arguments.md` §3, re-derived from the compiler.
    ///
    /// The census took its table with a harness it wrote into this test module
    /// and then removed. This is that table, taken from the rule itself: every
    /// call-argument temporary the corpus holds, bucketed by what the callee
    /// does with it. Each file is parsed ALONE — no loader, no linking — which
    /// is the convention every corpus measurement here uses, and which makes a
    /// cross-module callee's signature invisible. The linked reading is larger.
    ///
    /// Ignored by default: it reads the repository. Run it with
    /// `cargo test -p vyrn-frontend --lib census_call_arguments -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn census_call_arguments_over_the_corpus() {
        let mut files = Vec::new();
        crate::own::tests::sources("examples", &mut files);
        crate::own::tests::sources("std", &mut files);
        files.sort();

        let mut by_verdict: BTreeMap<String, usize> = BTreeMap::new();
        let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
        let mut unknown_callees: BTreeMap<String, usize> = BTreeMap::new();
        let (mut total, mut parsed, mut released, mut freed) = (0, 0, 0, 0);
        for path in &files {
            let Ok(src) = std::fs::read_to_string(path) else {
                continue;
            };
            let Ok(tokens) = crate::lexer::lex(&src) else {
                continue;
            };
            let (program, errs) = crate::parser::parse_accum(tokens);
            if !errs.is_empty() {
                continue;
            }
            parsed += 1;
            for s in facts(&program).arg_temps {
                total += 1;
                *by_verdict.entry(format!("{:?}", s.verdict)).or_default() += 1;
                if s.verdict == ArgVerdict::Released {
                    released += 1;
                    *by_kind.entry(format!("{:?}", s.kind)).or_default() += 1;
                    if s.kind == DropKind::FreeStr {
                        freed += 1;
                    }
                }
                if s.verdict == ArgVerdict::Unknown {
                    *unknown_callees.entry(s.callee.clone()).or_default() += 1;
                }
            }
        }
        println!("corpus: {} files ({parsed} parsed)", files.len());
        println!("call-argument temporaries: {total}");
        for (v, c) in &by_verdict {
            println!("  {v:>13}: {c:>5}");
        }
        println!("released, by release kind ({released} sites, {freed} emitted today)");
        for (k, c) in &by_kind {
            println!("  {k:>13}: {c:>5}");
        }
        let mut un: Vec<_> = unknown_callees.into_iter().collect();
        un.sort_by(|a, b| b.1.cmp(&a.1));
        println!("unknown, by callee");
        for (n, c) in un.iter().take(20) {
            println!("  {c:>5}  {n}");
        }
    }

    /// The same census, over the LINKED corpus — the reading the backends get.
    ///
    /// A file parsed alone cannot see an imported function's signature, so every
    /// cross-module callee reads `Unknown` above: `trim` alone is 43 sites. The
    /// linker puts both bodies in one program before either backend runs, which
    /// is the census's own §5 row 4 — a release crosses a module boundary today.
    /// Each row is counted once, keyed by module, line and position, because a
    /// `std/` body is linked into many roots.
    ///
    /// Ignored by default: it reads the repository and links it. Run it with
    /// `cargo test -p vyrn-frontend --lib census_call_arguments_linked -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn census_call_arguments_linked() {
        std::thread::Builder::new()
            .stack_size(256 * 1024 * 1024)
            .spawn(census_linked_count)
            .unwrap()
            .join()
            .unwrap();
    }

    fn census_linked_count() {
        struct Disk;
        impl crate::loader::ModuleResolver for Disk {
            fn read(&self, resolved: &str) -> Result<String, String> {
                std::fs::read_to_string(resolved).map_err(|e| e.to_string())
            }
            fn list(&self, resolved: &str) -> Result<Vec<String>, String> {
                let mut names: Vec<String> = std::fs::read_dir(resolved)
                    .map_err(|e| e.to_string())?
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                Ok(names)
            }
        }
        let slashed = |p: &std::path::Path| {
            p.canonicalize()
                .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
                .to_string_lossy()
                .replace('\\', "/")
                .replace("//?/", "")
        };
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let std_root = slashed(&repo.join("std"));

        let mut files = Vec::new();
        crate::own::tests::sources("examples", &mut files);
        crate::own::tests::sources("std", &mut files);
        files.sort();

        let mut seen: HashSet<(String, usize, String, usize)> = HashSet::new();
        let mut by_verdict: BTreeMap<String, usize> = BTreeMap::new();
        let mut unknown_callees: BTreeMap<String, usize> = BTreeMap::new();
        let (mut total, mut linked, mut freed) = (0, 0, 0);
        for path in &files {
            let Ok(src) = std::fs::read_to_string(path) else {
                continue;
            };
            let root_key = slashed(path);
            let opts = crate::loader::LoadOptions {
                std_root: Some(std_root.clone()),
                ..Default::default()
            };
            let Ok(program) = crate::loader::load(&src, &root_key, &opts, &Disk) else {
                continue;
            };
            linked += 1;
            for s in facts(&program).arg_temps {
                let key = (
                    s.module.clone().unwrap_or_else(|| root_key.clone()),
                    s.line,
                    s.callee.clone(),
                    s.ix,
                );
                if !seen.insert(key) {
                    continue;
                }
                total += 1;
                *by_verdict.entry(format!("{:?}", s.verdict)).or_default() += 1;
                if s.verdict == ArgVerdict::Released && s.kind == DropKind::FreeStr {
                    freed += 1;
                }
                if s.verdict == ArgVerdict::Unknown {
                    *unknown_callees.entry(s.callee.clone()).or_default() += 1;
                }
            }
        }
        println!("corpus: {} files, {linked} linked", files.len());
        println!("call-argument temporaries: {total}");
        for (v, c) in &by_verdict {
            println!("  {v:>13}: {c:>5}");
        }
        println!("released AND emitted (a String): {freed}");
        let mut un: Vec<_> = unknown_callees.into_iter().collect();
        un.sort_by(|a, b| b.1.cmp(&a.1));
        println!("unknown, by callee");
        for (n, c) in un.iter().take(20) {
            println!("  {c:>5}  {n}");
        }
    }

    /// RFC-0089 rule 2 over the whole corpus: **zero**.
    ///
    /// The number Phase 4b measured and gated off. It parses each file ALONE,
    /// like the other corpus measurements here, which under-counts a linked
    /// program — `vyrn check` over every root is the reading that migrated the
    /// corpus, and it is also zero.
    ///
    /// Ignored by default: it reads the repository. Run it with
    /// `cargo test -p vyrn-frontend --lib borrow_store_sites -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn borrow_store_sites_over_the_corpus() {
        let mut files = Vec::new();
        crate::own::tests::sources("examples", &mut files);
        crate::own::tests::sources("std", &mut files);
        files.sort();

        let mut rows: Vec<String> = Vec::new();
        let mut parsed = 0;
        for path in &files {
            let Ok(src) = std::fs::read_to_string(path) else {
                continue;
            };
            let Ok(tokens) = crate::lexer::lex(&src) else {
                continue;
            };
            let (program, errs) = crate::parser::parse_accum(tokens);
            if !errs.is_empty() {
                continue;
            }
            parsed += 1;
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            for d in borrow_store_sites(&program) {
                rows.push(format!(
                    "{name}:{} {}",
                    d.line,
                    d.message.lines().next().unwrap_or("")
                ));
            }
        }
        println!("corpus: {} files ({parsed} parsed)", files.len());
        println!("rule 2 store refusals: {}", rows.len());
        for r in &rows {
            println!("    {r}");
        }
        assert!(rows.is_empty(), "{rows:#?}");
    }

    /// RFC-0092 M0 — the gate. How many sites the rule "a projection is a borrow
    /// of its root, whatever the root is" would refuse over the whole corpus.
    ///
    /// It **links**. The two measurements above parse each file alone, which is
    /// the reading Phase 4b got wrong by 81 sites: a file read on its own cannot
    /// name an imported type, so `owns_heap` answers "unknown" and the site
    /// disappears. Every `.vyrn` under `examples/` and `std/` is loaded as a
    /// root, and a site is counted once per (file, line, path, kind) however many
    /// roots reach the module it lives in.
    ///
    /// Three numbers, and the third is inside the first two: stores of a
    /// projection, returns of a projection, and how many of each name a type that
    /// owns no heap — the rule does not reach those and they cost nothing.
    ///
    /// M0 measured with this and refused nothing; **M1 refuses, and this is the
    /// regression guard**. The store classes must read zero. The returns that
    /// remain are the ones waiting on M3's release row — see the assertions.
    ///
    /// Ignored by default: it reads the repository and links it. Run it with
    /// `cargo test -p vyrn-frontend --lib rfc0092 -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn rfc0092_projection_sites_over_the_corpus() {
        // A debug build overflows the 2 MB test stack linking the larger roots —
        // loading and checking are both recursive over the AST. The measurement
        // runs on a thread big enough for the deepest one.
        std::thread::Builder::new()
            .stack_size(256 * 1024 * 1024)
            .spawn(rfc0092_count)
            .unwrap()
            .join()
            .unwrap();
    }

    fn rfc0092_count() {
        struct Disk;
        impl crate::loader::ModuleResolver for Disk {
            fn read(&self, resolved: &str) -> Result<String, String> {
                std::fs::read_to_string(resolved).map_err(|e| e.to_string())
            }
            fn list(&self, resolved: &str) -> Result<Vec<String>, String> {
                let mut names: Vec<String> = std::fs::read_dir(resolved)
                    .map_err(|e| e.to_string())?
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                Ok(names)
            }
        }
        // Canonical, slash-separated, and the same spelling the loader resolves
        // an import to. A root passed in as `<crate>/../../std/x.vyrn` and the
        // same file reached through an import are two strings for one file, and
        // the count would double every module every root reaches.
        let slashed = |p: &std::path::Path| {
            p.canonicalize()
                .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
                .to_string_lossy()
                .replace('\\', "/")
                .replace("//?/", "")
        };
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let std_root = slashed(&repo.join("std"));
        let repo_prefix = format!("{}/", slashed(&repo));

        let mut files = Vec::new();
        crate::own::tests::sources("examples", &mut files);
        crate::own::tests::sources("std", &mut files);
        files.sort();

        let mut seen: HashSet<(String, usize, String, &'static str)> = HashSet::new();
        let mut rows: Vec<(String, ProjectionSite)> = Vec::new();
        let (mut linked, mut unlinkable) = (0, Vec::new());
        for path in &files {
            let Ok(src) = std::fs::read_to_string(path) else {
                continue;
            };
            let root_key = slashed(path);
            let opts = crate::loader::LoadOptions {
                std_root: Some(std_root.clone()),
                ..Default::default()
            };
            let Ok(program) = crate::loader::load(&src, &root_key, &opts, &Disk) else {
                unlinkable.push(root_key);
                continue;
            };
            linked += 1;
            for s in projection_sites(&program) {
                let file = s.module.clone().unwrap_or_else(|| root_key.clone());
                let key = (file.clone(), s.line, s.path.clone(), s.kind);
                if seen.insert(key) {
                    rows.push((file, s));
                }
            }
        }
        rows.sort_by(|a, b| (&a.0, a.1.line).cmp(&(&b.0, b.1.line)));

        let count = |kind: &str, heap: bool| {
            rows.iter()
                .filter(|(_, s)| s.kind == kind && s.owns_heap == heap)
                .count()
        };
        let unknown = |kind: &str| {
            rows.iter()
                .filter(|(_, s)| s.kind == kind && s.ty == "?")
                .count()
        };
        let (stores, returns) = (count("store", true), count("return", true));
        println!(
            "corpus: {} files, {linked} linked ({} would not link)",
            files.len(),
            unlinkable.len()
        );
        for f in &unlinkable {
            println!("    not linked: {f}");
        }
        println!("RFC-0092 projection sites over the corpus");
        println!("  stores:  {stores}  (+{} scalar)", count("store", false));
        println!("  returns: {returns}  (+{} scalar)", count("return", false));
        println!("  total:   {}", stores + returns);
        println!(
            "  unnameable even linked, so counted as scalar: {} store, {} return",
            unknown("store"),
            unknown("return")
        );
        // M1 widened `store` and `returned_borrow` to see an element read, so
        // these are refused like the field they are. Still counted apart,
        // because M0 counted them apart and the two numbers have to stay
        // comparable.
        println!(
            "  element reads: {} store (+{} scalar), {} return (+{} scalar)",
            count("elem-store", true),
            count("elem-store", false),
            count("elem-return", true),
            count("elem-return", false)
        );
        for (file, s) in &rows {
            let name = file.strip_prefix(&repo_prefix).unwrap_or(file);
            let cost = if s.owns_heap { "*" } else { " " };
            println!(
                "  {cost} {:<7} {name}:{} {}: `{}` -> {} [{}]",
                s.kind, s.line, s.func, s.path, s.into, s.ty
            );
        }
        // M1's regression guard. Every store the rule refuses is migrated, and
        // the corpus compiles, so the instrument reads zero for all three store
        // classes and for an element return. A site that reappears fails here
        // with its file and line already printed above.
        assert_eq!(stores, 0, "RFC-0092 M1: a projection store came back");
        assert_eq!(
            count("elem-store", true),
            0,
            "RFC-0092 M1: an element store came back"
        );
        assert_eq!(
            count("elem-return", true),
            0,
            "RFC-0092 M1: an element return came back"
        );
        // **M1 left seven of these and M3 closed all seven**, in the change that
        // gave the row — which is what M1 said would happen and why it asserted
        // the number rather than migrating them early.
        //
        // They were all one shape: `return match hit { Some(r) => r, .. }` on an
        // owned `Option<Response>` or `Option<Cargo>`. `check_return` refuses an
        // arm-yielded projection only where the caller RELEASES the result
        // (Phase 4b's guard, which this RFC does not move), and a record had no
        // release rule, so they could not dangle. M3 gives `Type::Record` its
        // row and they can.
        //
        // The fix is not the copy M1 priced and refused to pay. RFC-0093 M1
        // shipped the take in between, so each of them reads
        // `match consume hit { .. }`: the arm yields a value the frame gave up,
        // and nothing is copied at all. Six are one line of the `pages`
        // generator.
        assert_eq!(
            returns, 0,
            "RFC-0092 M3: a projection return came back — see the list above"
        );
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
