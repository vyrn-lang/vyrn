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
    /// A lambda or a `spawn` holds it. A LAMBDA's capture is a by-value deep
    /// snapshot since RFC-0114 §25 round three (both compiling backends
    /// duplicate heap captures into the block, and the release twin walks
    /// them), so the binding's own value is still this frame's to release —
    /// round fifty-seven turns that release on. A `spawn`'s capture crosses
    /// to a task that outlives the frame, and stays a recorded leak.
    Captured { line: usize, spawned: bool },
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
    /// Exit-residue round sixteen. `Some(true)` while every take recorded on
    /// this row came through the loop variable bound to it — an element
    /// departure, which COPIES the value out of the buffer. `Some(false)` once
    /// any other writer touched the row: a foreign take, a lender's result, a
    /// position that retains. `None` = nothing recorded. Read by `own.rs`'s
    /// `ForIn` arm: a consumed array row whose value is gone but whose writes
    /// were all elem-only still owns its BUFFER, and the buffer alone is freed
    /// (`DropKind::FreeArr`). Round fourteen's blanket version of that
    /// downgrade freed LENT buffers — this field is the WHICH-take attribution
    /// the refusal note asked for.
    pub elem_only: Option<bool>,
    /// The loop variable bound to this row, set where `ForIn` binds one.
    pub elem_name: Option<String>,
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
    /// The function (or `test@i`/`bench@i` body) the site lives in — what lets
    /// `plan.unconsumed` skip rows whose owner an emission never reached
    /// (RFC-0114 §26's finish check).
    pub owner: String,
    /// Round twenty: the callee is a VIEW whose result for this argument is a
    /// copy — the element type owns no heap, so the scalar the view hands out
    /// cannot alias the temporary. `bytes(l)[0]` in a line loop was one
    /// unfreed `bytes` buffer per line; with this the row is Released like
    /// any read argument instead of standing down as Lent.
    pub view_copies: bool,
    /// Round twenty-five: for a heapified array-literal argument with
    /// heap-owning elements, the callee of each element expression — screened
    /// in `arg_verdict` against the CLOSED lending set, because a lender's
    /// result inside the literal would make the deep free a use-after-free.
    pub elem_producers: Vec<String>,
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
/// One function exit the walk met (round twenty-one): a `return` or a
/// propagating `?`, with the walk order it happened at, the loop context it
/// sits in, and whether it is CLEAN — outside every `region` and every lambda
/// body, the two frames an early release must never be placed into (arena
/// memory is not this walk's, and a lambda's exit is a different runtime
/// frame).
#[derive(Clone, Debug)]
pub struct ExitEv {
    pub order: u32,
    pub site: usize,
    pub is_try: bool,
    pub fn_name: String,
    pub loops: Vec<u32>,
    pub clean: bool,
}

pub struct Facts {
    /// What every `let` owns at the end of its block — see [`ownership`].
    pub lets: HashMap<usize, LetOwnership>,
    /// The per-binding write/take event stream, in walk order (RFC-0114 M2).
    /// `own::analyze` folds it into the store-ownedness set; nothing else
    /// reads it.
    pub store_events: Vec<StoreEv>,
    /// The walk-order positions of every early exit — see [`fold context`] in
    /// `own::fold_revived`, its only reader.
    pub exit_orders: Vec<u32>,
    /// Round twenty-one: every `return` and `?` the walk met, with enough
    /// context to place an early release — see `own::fold_early_releases`.
    pub exit_sites: Vec<ExitEv>,
    /// The two closures over the call graph, kept so a reader can ask whether
    /// either says anything (RFC-0125 §3 M3, the checker's deletion path).
    ///
    /// `lending` names the functions whose result the caller must not release;
    /// `retains` names the `(callee, index)` positions that KEEP a borrowed
    /// parameter they are handed. Both are seeded by rule 2 and rule 3's own
    /// recording paths, and both are EMPTY over the corpus.
    ///
    /// **Empty over the corpus is not the same as unfillable, and the deletion
    /// slice found the difference.** The record read "the shadow of a rule that
    /// is enforced now": rule 3 refuses a returned borrow, so no function may
    /// lend its result. Rule 3 does not refuse a [`Borrow::Element`], and
    /// [`MoveCheck::check_return`] says so in its own words — a `for` variable
    /// keeps phase 4b's verdict. So `fn pick(xs: Array<String>) -> String { for
    /// x in xs { return x } .. }` is ACCEPTED and seeds `lending`, and
    /// `movecheck`'s own
    /// `a_lender_forwarded_through_an_aggregate_is_still_marked_lending` is that
    /// program. Delete the closures and its caller frees an element the caller's
    /// caller still owns. So these two stay until rule 3 covers an element, or
    /// something else states what they state (RFC-0125 §3 M3, the deletion
    /// slice).
    pub lending: HashSet<String>,
    pub retains: HashSet<(String, usize)>,
    /// Round nineteen's third closure: the functions whose result can HOLD a
    /// borrowed parameter's storage. Screened beside the other two in
    /// `core::Builder::store_is_fresh`, and handed on to the core with them.
    pub escapers: HashSet<String>,
    /// Round forty-six's meet, as a set of signature keys: the fn-value
    /// signatures whose whole target set READS the position, retains nothing
    /// there and lends nothing.
    ///
    /// A call through a `fn`-typed parameter, field or binding names no
    /// function, so no capability row answers for its positions and every
    /// argument temporary there would stand aside. Any runtime value of
    /// `Fn(ps) -> r` is either a program function of exactly that signature or
    /// a lambda; lambdas carry no capability and no retention rows, so a
    /// signature any lambda could inhabit is not in this set. What is in it is
    /// a signature the meet cleared, and the core reads it at the position
    /// ([`fn_sig_key`]) the way it reads `lending` and `retains`.
    pub fnval_clear: HashSet<String>,
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
    /// The stack of `if`/`match` branch ids the event sits inside — an event
    /// with the same branch path as the binding's `let` runs whenever the
    /// `let` did (modulo the exits `exit_orders` records), which is what the
    /// untake fold needs: a CONDITIONAL revive must not qualify.
    pub branch: Vec<u32>,
    pub kind: EvKind,
    /// The enclosing function — see [`ArgTemp::owner`].
    pub owner: String,
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

/// [`ownership`] and the rows the plan still folds, out of one walk.
pub fn facts(program: &Program) -> Facts {
    let r = run(program, Want::Lets);
    let mut lets = r.lets;
    // A call to a lender hands back storage the callee does not own, so the
    // binding that names it may not be released. Applied here rather than at the
    // `let`, because a lender is only known once every body has been read.
    for row in lets.values_mut() {
        // Round sixteen: these two rules answer "somebody else holds this
        // value's storage". A row a body already marked gone SKIPS them below
        // — but the elem-only attribution must still see them, because a
        // buffer-only free of a LENT or producer-owned buffer is the
        // use-after-free round fourteen's blanket downgrade shipped. Poison
        // first, so the skip cannot hide a foreign owner.
        if row
            .from_call
            .as_ref()
            .is_some_and(|n| r.lending.contains(n))
            || row
                .passed
                .iter()
                .any(|(c, i, _)| r.retains.contains(&(c.clone(), *i)) || r.lending.contains(c))
        {
            row.elem_only = Some(false);
        }
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
        store_events: r.store_events,
        exit_orders: r.exit_orders,
        exit_sites: r.exit_sites,
        lending: r.lending,
        retains: r.retains,
        escapers: r.param_escapers,
        fnval_clear: r.fnval_clear,
    }
}

/// The key a fn-value signature meets under — resolved parameter types and a
/// resolved result, spelled once so the pass that closes the call graph and the
/// core that reads the answer cannot spell it differently.
pub fn fn_sig_key(ps: &[Type], ret: &Type, decls: &HashMap<String, TypeDecl>) -> String {
    let rps: Vec<Type> = ps.iter().map(|t| crate::types::resolve(t, decls)).collect();
    format!("{rps:?}->{:?}", crate::types::resolve(ret, decls))
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
    projections: Vec<ProjectionSite>,
    store_events: Vec<StoreEv>,
    exit_orders: Vec<u32>,
    param_escapers: HashSet<String>,
    exit_sites: Vec<ExitEv>,
    fnval_clear: HashSet<String>,
}

/// The capability map [`arg_verdict`] answers a position under: a declared
/// function's parameters, and a protocol method's over them (a method call
/// reaches this pass under its SURFACE name, and the protocol is what both
/// sides agreed on).
///
/// Public because a second pass states the same rule at the same position
/// (RFC-0125 §3 M3, the argument slice): the core lowers the call and asks
/// [`arg_verdict`] there, so both must read the position the same way.
pub fn arg_caps(program: &Program) -> HashMap<String, Vec<Capability>> {
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
    for p in &program.protocols {
        for m in &p.methods {
            let mut cs = vec![m.recv];
            cs.extend(m.param_caps.iter().copied());
            caps.insert(m.name.clone(), cs);
        }
    }
    caps
}

/// The capability of one position: the declaration's word where there is one,
/// the seeded row's otherwise, and `None` where neither answers — which is
/// [`ArgVerdict::Unknown`] and frees nothing.
pub fn arg_cap(
    caps: &HashMap<String, Vec<Capability>>,
    callee: &str,
    ix: usize,
) -> Option<Capability> {
    caps.get(callee)
        .and_then(|c| c.get(ix))
        .copied()
        .or_else(|| crate::prelude::capability(callee, ix))
}

/// Whether the producer of an argument HANDS ITS ARGUMENT BACK — `blackBox`,
/// whose seeded row returns the same bare type parameter one of its own
/// parameters has. The result IS the argument, so no temporary stands here.
///
/// Public for the same reason [`arg_caps`] is: the core screens the same
/// producer at the same position.
pub fn hands_back(name: &str) -> bool {
    crate::prelude::signature(name).is_some_and(|f| {
        matches!(&f.ret, Type::Param(r)
            if f.params.iter().any(|p| matches!(&p.ty, Type::Param(q) if q == r)))
    })
}

/// Can a call to `name` return storage one of its arguments holds? The
/// copying builtins cannot — `@concat`, `@str`, `@copy` and every seeded row
/// that neither hands an argument back (identity-typed return), views, nor
/// lends builds a fresh value. Everything else — an `@`-desugar like `@push`,
/// a user function — is assumed able to, which is the leak direction.
///
/// Public because the core asks it at the store it is lowering (RFC-0125 §3
/// M3, the fresh-store slice).
pub fn call_may_forward(name: &str) -> bool {
    MoveCheck::call_may_forward_body(name)
}

/// Whether the builtin `name` hands back a pointer into its argument — see
/// [`views`], which this is the public name of.
pub fn lends_result(name: &str) -> bool {
    views(name)
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
pub fn arg_verdict(
    s: &ArgTemp,
    constructs: bool,
    cap: Option<Capability>,
    retains: &HashSet<(String, usize)>,
    lending: &HashSet<String>,
) -> ArgVerdict {
    // The producer handed back storage it does not own, so there is no
    // temporary here at all — the same rule [`ownership`] applies to a `let`
    // whose initializer is a call to a lender.
    if s.producer.as_deref().is_some_and(|p| lending.contains(p)) {
        return ArgVerdict::Lent;
    }
    // Round twenty-five: a heapified literal whose ELEMENTS came from a lender
    // holds borrowed storage — the deep free must stand down.
    if s.elem_producers.iter().any(|p| lending.contains(p)) {
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
    if constructs {
        return ArgVerdict::Retained;
    }
    if retains.contains(&(s.callee.clone(), s.ix)) {
        return ArgVerdict::Retained;
    }
    // A view LENDS — its result names a place inside this argument — except
    // where the element it hands out is a heap-free copy (round twenty): a
    // scalar read through `bytes(l)[0]` keeps no pointer into the buffer, so
    // the temporary is the caller's to free like any read argument.
    if lending.contains(&s.callee) || (views(&s.callee) && !s.view_copies) {
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
    // `x.copy()` READS its receiver and returns a value that shares nothing
    // with it (RFC-0089 M1b). Its row is held back from the return table —
    // the type is its receiver's — so no capability answers for it below,
    // and a temporary receiver (`("" + s).copy()`) fell to Unknown and
    // leaked (exit-residue round thirty-seven).
    if s.callee == "@copy" && s.ix == 0 {
        return ArgVerdict::Released;
    }
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
///
/// A USER projection (RFC-0120) says the same thing about the same value: a
/// result capability IS "the result is a place the receiver owns", which is the
/// builtin half's whole definition. It was left out because nothing could fire
/// on it — `Declared::type_of` refused a projection call a type, so every rule
/// that keys on a lender stood down for want of a type rather than for a
/// reason. The checker types a projection call like any other call
/// (RFC-0125 §3 M3, the type slice), so the fact has to be stated. Three
/// programs say what it costs otherwise: `namedplace` minted a temporary row
/// for `led.wrapped(2)` and the kernel refused the release, and `protoplace`
/// and `assoctype` freed the label a projection handed out.
fn views(name: &str) -> bool {
    crate::prelude::lends(name) || named_projection(name)
}

thread_local! {
    /// The user projection NAMES of the program under check (RFC-0120).
    ///
    /// This pass keys every element-read rule on the spelling `@at`, because
    /// `@at` is reserved and therefore IS an element read wherever it appears.
    /// A named projection is the same read under a user-chosen name, and the
    /// name alone cannot say so — so [`run`] records the program's projection
    /// names here and [`named_projection`] answers for the free functions
    /// ([`element_path`]) that have no `&self` to carry a set through. A name
    /// answers true whether or not the receiver at a given site is the
    /// projection's own type; that over-approximation only widens a borrow
    /// verdict, never narrows one, which is the conservative direction.
    static PLACE_NAMES: std::cell::RefCell<HashSet<String>> =
        std::cell::RefCell::new(HashSet::new());
}

/// Whether `name` is a user projection's name — see [`PLACE_NAMES`].
fn named_projection(name: &str) -> bool {
    PLACE_NAMES.with(|s| s.borrow().contains(name))
}

/// `@at`, or a user projection's own name: an element read either way.
fn projection_call(name: &str) -> bool {
    name == crate::project::AT || named_projection(name)
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
/// **The order a file's refusals come out in is the source's** (RFC-0125 §3
/// M3, the corpus slice). This pass walks top-level functions before `impl`
/// methods and the placer walks bodies in the lowering's order, so the same
/// two sentences came out swapped and the whole standard error moved even
/// where every sentence was identical. Neither walk order is a rule anybody
/// wrote down; the source's is, and it is the only one a reader can predict.
/// So both passes sort by line before they print, and the other statement of
/// the same rule is `vyrn-cli`'s `kernel_refuses`. Files keep the order they
/// were first named in — a module's refusals stay together — and two on one
/// line keep the walk's order, which is why the sort is stable.
pub fn check_accum(program: &Program) -> Vec<Diagnostic> {
    let mut diags = run(program, Want::Check).diags;
    in_source_order(&mut diags);
    diags
}

/// Put a file's refusals in the order the source states them — see
/// [`check_accum`], which is where the rule is written down.
fn in_source_order(diags: &mut [Diagnostic]) {
    let mut files: Vec<Option<String>> = Vec::new();
    for d in diags.iter() {
        if !files.contains(&d.file) {
            files.push(d.file.clone());
        }
    }
    diags.sort_by_key(|d| (files.iter().position(|f| *f == d.file).unwrap_or(0), d.line));
}

/// **Every ownership refusal a program earns, the checker's and the kernel's,
/// as ONE list** — RFC-0125 §3 M3, the accumulation slice. The one driver, and
/// the only entry point a tool should use: `vyrn check` and the editor call it,
/// so a rule that has left this file is stated in both.
///
/// Three rules make the list, and each of them is a defect that was measured.
///
/// 1. **The kernel judges every program the core can lower**, not only one the
///    checker accepted. It used to be asked after the load returned `Ok`, so a
///    file with a must-use error and a use after a take printed the checker's
///    sentence beside the must-use one — and would have printed NEITHER the day
///    the rule left, because a deleted rule with no second statement removes the
///    sentence rather than moving it. `examples/mustuse_abandoned.vyrn` is that
///    file. The core lowers every body now, so the condition is the one the
///    lowering itself has: the program type-checks, which is what the callers
///    gate on.
/// 2. **The kernel speaks at a line the checker was silent about.** Where the
///    checker spoke, its sentence stands — at its line, with its menu, in the
///    wording the census pins. This is the rule that keeps the merge from
///    ADDING one mistake said twice: `xs` moved into `fromArray(..)` and then
///    read is refused by the checker at the move and by the kernel at the
///    read, and the two are one mistake at one line.
///
///    The LINE is the key, and it was the line OR the binding until the
///    judgment became a list (`vyrn_lower`'s `kernel::Kernel::refusals`). The
///    binding clause was what a judgment that stopped at its first refusal
///    needed: a body said one thing, so a second sentence about the same
///    binding at another line could not be told from the first one said again,
///    and the merge dropped it. A body that states every refusal it has needs
///    no such guess — `r26_rebuild_a_borrowed_receiver.vyrn` is two mistakes
///    about `mt` at two lines, and a reader is owed both.
///
///    What the binding clause was really carrying is the must-use walk, and
///    that is a rule about a TYPE's obligation rather than about ownership: a
///    `Stream` closed twice is a must-use refusal AND a use after a take, at
///    two lines, and it is still one mistake. So a binding the obligation
///    names silences the kernel about that binding for the whole file, and
///    nothing else does. Measured over the corpus, six programs turn on it.
/// 3. **The order is the source's**, for the whole list at once — the rule
///    [`check_accum`] states, applied after the two passes are one.
///
/// `VYRN_NO_MOVECHECK=1` stands the checker aside so the kernel's own sentence
/// is reachable, which is the licence table's instrument, and it belongs here
/// now that this is the only place both passes are asked.
pub fn refusals(program: &Program) -> Vec<Diagnostic> {
    let mut diags = if std::env::var("VYRN_NO_MOVECHECK").is_ok_and(|v| v == "1") {
        Vec::new()
    } else {
        run(program, Want::Check).diags
    };
    // The must-use judgment, which `VYRN_NO_MOVECHECK=1` does NOT stand aside:
    // it is not the move check's, and the knob names the file it stands aside.
    let owed = crate::own::must_use_refusals(program);
    let mustuse: HashSet<(Option<String>, String)> = owed
        .iter()
        .filter_map(|d| Some((d.file.clone(), subject(&d.message)?.to_string())))
        .collect();
    diags.extend(owed);
    // A comptime program is judged by the checker alone: its refusals were
    // always discarded (`vyrn-cli`'s old `RefusalScope` cleared the
    // thread-local at the point the command's own program was linked), and the
    // placer it would run here is the one the interpreter runs to execute it.
    // MEASURED, because it is the editor that pays twice: a keystroke in
    // `site/app/docs.vyrn` re-runs two generators, and judging them cost 117 ms
    // of the 870 ms the kernel adds.
    if COMPTIME.with(|c| c.get()) {
        in_source_order(&mut diags);
        return diags;
    }
    // Runs the placer, which builds and judges a core body for every instance.
    // The answer is not handed on: see `own::Memo::open` for why a plan made
    // here does not fit the lowering a tool runs next.
    //
    // THIS analysis is the one a judgment may be reused for, and no other: a
    // generator load's and an engine's both run outside this call, and neither
    // reads the refusals ([`reuse_judgments`]).
    JUDGING.with(|j| j.set(true));
    let _ = crate::own::analyze(program);
    JUDGING.with(|j| j.set(false));
    let mut lines: HashSet<(Option<String>, usize)> = HashSet::new();
    for d in &diags {
        lines.insert((d.file.clone(), d.line));
    }
    diags.extend(crate::own::kernel_refusals().into_iter().filter(|d| {
        !lines.contains(&(d.file.clone(), d.line))
            && !subject(&d.message)
                .is_some_and(|s| mustuse.contains(&(d.file.clone(), s.to_string())))
    }));
    in_source_order(&mut diags);
    diags
}

thread_local! {
    /// Whether the program being checked is a `gen fn`'s own — see
    /// [`comptime`].
    static COMPTIME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Check a COMPTIME program inside `f` — a generator's own program, loaded and
/// run during the load of the program a tool asked about (RFC-0021).
///
/// [`refusals`] states the kernel's sentences about the program a tool holds.
/// A generator's program is not that program: it is machinery the load runs on
/// the way there, its refusals were always dropped, and the placer this would
/// run is the one the interpreter runs anyway to execute it.
pub fn comptime<T>(f: impl FnOnce() -> T) -> T {
    let was = COMPTIME.with(|c| c.replace(true));
    let out = f();
    COMPTIME.with(|c| c.set(was));
    out
}

/// Whether the program being worked on is a generator's own — see [`comptime`].
pub fn in_comptime() -> bool {
    COMPTIME.with(|c| c.get())
}

/// The kernel's verdict on one body as the cache holds it: every field of the
/// kernel's own refusal — file, line, message, body — and nothing an address
/// could reach. A body that earns none caches an empty list.
pub type Verdict = Vec<(Option<String>, usize, String, String)>;

/// The cache's key: the module a body is declared in, that module's content
/// hash, and the instance's spelling.
pub type JudgmentKey = (String, String, String);

thread_local! {
    /// Whether this host reuses the kernel's judgment — see [`reuse_judgments`].
    static REUSE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Whether the analysis running now is the one [`refusals`] asked for.
    static JUDGING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The declaration fingerprint the entries answer under, and the entries.
    /// Keyed by fingerprint at the top rather than inside the entry key, for
    /// `checker::CHECK_MEMO`'s reason: a fingerprint change invalidates every
    /// entry anyway, so the whole map is dropped and nothing needs eviction.
    static JUDGED: RefCell<(u64, HashMap<JudgmentKey, Verdict>)> =
        RefCell::new((0, HashMap::new()));
    /// `(judged, reused)` since [`reset_judgment_tally`] — the instrument the
    /// pin below counts bodies with.
    static TALLY: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}

/// Reuse the kernel's per-body judgment across calls — RFC-0125 §3 M3, the
/// memo slice. Armed once by the host, and only by a host that reads the
/// REFUSALS and lowers nothing: the editor.
///
/// A body whose key is unchanged is not built and not judged, so the release
/// rows the placer would have added for it are not added either, and its
/// frames are not folded into the core's facts. Neither has a reader HERE, and
/// that is the whole condition — one rule, stated once, for both:
///
///   * the facts are read by the two compiled backends and by the
///     interpreter's arm rows;
///   * the placer's rows are read by the emitters, and inside one analysis by
///     a later `core::build` of the SAME function — every table `place_frames`
///     writes is keyed by a node, and a node belongs to one function, so the
///     only bodies a served body's missing rows could reach are its own other
///     instances, which carry the same module and the same content hash and
///     are therefore served or built together.
///
/// So a host that lowers or emits must never arm this, and a host that arms it
/// gets refusals and nothing else. The editor is the one such host: it shows
/// diagnostics, and `memory_notes` reads the walk's own notes, which the placer
/// does not write.
pub fn reuse_judgments() {
    REUSE.with(|r| r.set(true));
}

/// Whether the analysis running now may reuse a judgment: the host armed it,
/// and this is the analysis [`refusals`] asked for rather than one a generator
/// load or an engine asked for.
pub fn reusing_judgments() -> bool {
    REUSE.with(|r| r.get()) && JUDGING.with(|j| j.get())
}

/// The cache, open for one analysis, or `None` where nothing is armed.
///
/// Opening it computes the declaration fingerprint and drops every entry if it
/// moved. It also takes the loader's module hashes once: a key reads them, and
/// asking the loader per body would clone the map 882 times.
pub struct Judgments {
    hashes: HashMap<String, String>,
}

impl Judgments {
    /// Open the cache for `program`, or `None` where nothing is armed.
    pub fn open(program: &Program) -> Option<Judgments> {
        if !reusing_judgments() {
            return None;
        }
        let fp = declaration_fingerprint(program);
        JUDGED.with(|j| {
            let mut j = j.borrow_mut();
            if j.0 != fp {
                j.0 = fp;
                j.1.clear();
            }
        });
        Some(Judgments {
            hashes: crate::loader::last_module_hashes(),
        })
    }

    /// This body's key, or `None` where it has none: the ROOT module, which is
    /// the one a keystroke edits, and a module the loader recorded no hash for.
    pub fn key(&self, module: Option<&str>, spelling: &str) -> Option<JudgmentKey> {
        let m = module?;
        let h = self.hashes.get(m)?;
        Some((m.to_string(), h.clone(), spelling.to_string()))
    }

    /// The verdict recorded for `key`, and a tally of the reuse.
    pub fn get(&self, key: &JudgmentKey) -> Option<Verdict> {
        let hit = JUDGED.with(|j| j.borrow().1.get(key).cloned());
        TALLY.with(|t| {
            let (judged, reused) = t.get();
            match hit {
                Some(_) => t.set((judged, reused + 1)),
                None => t.set((judged + 1, reused)),
            }
        });
        hit
    }

    /// Record what a body earned. Only its caller knows whether the body was
    /// inert, which is the condition an entry is written under.
    pub fn put(&self, key: JudgmentKey, verdict: Verdict) {
        JUDGED.with(|j| j.borrow_mut().1.insert(key, verdict));
    }
}

/// `(judged, reused)` since [`reset_judgment_tally`].
pub fn judgment_tally() -> (u64, u64) {
    TALLY.with(|t| t.get())
}

/// Start counting bodies again.
pub fn reset_judgment_tally() {
    TALLY.with(|t| t.set((0, 0)));
}

/// A cheap hash over every declaration the kernel's judgment of a body can
/// read across a module boundary — RFC-0125 §3 M3, the memo slice.
///
/// The key above says WHICH body and WHICH text; this says what else that text
/// was judged against. Function BODIES are excluded, which is the whole point:
/// an edit inside one body must not re-judge every other. Three things a
/// reader would not guess are in:
///
///   * every parameter's CAPABILITY, which the checker's own fingerprint has
///     no use for and this one cannot do without — `read x` to `consume x`
///     changes what every caller's body owes and moves no type;
///   * a projection's whole BODY (`impl`'s `places`), because a projection is
///     inlined into its caller's block rather than judged as a body of its
///     own, so its text is its callers' text;
///   * a validated type's predicate and a module-state binding's initializer,
///     for the same reason the checker's fingerprint has them.
///
/// Every part is sorted before it is folded, because two of the sources are
/// hash maps and one run's iteration order is not the next run's.
fn declaration_fingerprint(program: &Program) -> u64 {
    let sig = |f: &Function| {
        let mut bounds: Vec<String> = f
            .type_bounds
            .iter()
            .map(|(k, v)| format!("{k}:{v:?}"))
            .collect();
        bounds.sort_unstable();
        format!(
            "f{:?}/{}<{:?}{:?}>({:?})->{:?}|{}{}{}{}",
            f.module,
            f.name,
            f.type_params,
            bounds,
            f.params,
            f.ret,
            f.exported as u8,
            f.is_extern as u8,
            f.is_export_extern as u8,
            f.is_gen as u8,
        )
    };
    let mut parts: Vec<String> =
        Vec::with_capacity(program.functions.len() + program.type_decls.len());
    parts.extend(program.functions.iter().map(&sig));
    for t in &program.type_decls {
        parts.push(format!(
            "t{:?}/{}<{:?}>={:?}|{:?}",
            t.module, t.name, t.type_params, t.base, t.predicate
        ));
    }
    for g in &program.globals {
        parts.push(format!(
            "g{:?}/{}:{:?}|{}|{:?}",
            g.module, g.name, g.ty, g.mutable as u8, g.init
        ));
    }
    for p in &program.protocols {
        parts.push(format!(
            "p{:?}/{}<{:?}>={:?}",
            p.module, p.name, p.assoc, p.methods
        ));
    }
    for i in &program.impls {
        let mut bounds: Vec<String> = i
            .type_bounds
            .iter()
            .map(|(k, v)| format!("{k}:{v:?}"))
            .collect();
        bounds.sort_unstable();
        parts.push(format!(
            "i{}/{:?}<{:?}{:?}>{:?}",
            i.protocol, i.ty, i.type_params, bounds, i.assoc
        ));
        parts.extend(i.methods.iter().map(&sig));
        // A projection is never a body of its own: every access site inlines
        // it, so its body belongs to its callers' text.
        parts.extend(i.places.iter().map(|p| format!("j{p:?}")));
    }
    parts.extend(
        program
            .surface_shadows
            .iter()
            .map(|(m, n)| format!("s{m:?}/{n}")),
    );
    parts.sort_unstable();
    let mut h: u64 = 0xcbf29ce484222325;
    for p in &parts {
        for b in p.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// The binding a refusal is about: the root of the first path its message
/// quotes.
///
/// A read of the sentence rather than a field beside it, because both passes
/// already write the subject first and in backticks — ``` `b` is dropped here
/// ```, ``` `s.byteLength` is used here ```, ``` module state `names` may not
/// ``` — and a field would have to be filled at every one of the refusal sites
/// in this file and in the kernel, which is two lists to keep in step instead
/// of none. A message that quotes nothing has no subject and is never
/// suppressed.
fn subject(message: &str) -> Option<&str> {
    let rest = message.split_once('`')?.1;
    let path = rest.split_once('`')?.0;
    let root = root_of(path);
    (!root.is_empty() && root.chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(root)
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
    // The projection-name set for [`named_projection`], rebuilt per run so a
    // long-lived process (the LSP) always answers for the program in hand.
    PLACE_NAMES.with(|s| {
        *s.borrow_mut() = program
            .impls
            .iter()
            .flat_map(|i| i.places.iter().map(|p| p.name.clone()))
            .collect();
    });
    // A method call is written `s.insert(v)` and reaches this pass as
    // `insert(s, v)` — the SURFACE name, because the impl is selected by the
    // receiver's type and this pass does not select impls. The protocol is what
    // both sides agree on (conformance compares capabilities), so its
    // declaration is the discipline every call site reads: without this the
    // exclusivity rule and the `consume` move would both go silent the moment a
    // function became a method. Stated once, in [`arg_caps`].
    let caps = arg_caps(program);
    let globals: HashSet<String> = program.globals.iter().map(|g| g.name.clone()).collect();
    // `export extern fn` names. Rule 3 is stricter here, because the caller is
    // JS and JS frees every String it is handed (RFC-0089 M3b).
    let exported: HashSet<String> = program
        .functions
        .iter()
        .filter(|f| f.is_export_extern)
        .map(|f| f.name.clone())
        .collect();
    // RFC-0125 §3 M3, the type slice: the type of a node is the checker's
    // answer, read off its record. `recorded` serves the analysis's own check
    // where one was made and checks once where none was, and the record it
    // makes here is held for the rest of the analysis — so the check the
    // lowering used to pay for is the one this asks for.
    let rec_span = crate::prof::phase("movecheck: checker::record");
    let rec = crate::checker::recorded(program);
    drop(rec_span);
    let decl = Declared::new(program).recording(rec);
    let mc = MoveCheck {
        caps: &caps,
        impls: &program.impls,
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
        reads: RefCell::new(Scopes::new(HashMap::new())),
        lambda_base: RefCell::new(Vec::new()),
        lambda_escapes: RefCell::new(Vec::new()),
        arm_binders: RefCell::new(Vec::new()),
        call_keeps: std::cell::Cell::new(None),
        continue_seen: std::cell::Cell::new(false),
        sites: (want == Want::Sites).then(|| RefCell::new(Vec::new())),
        nodes: RefCell::new(Scopes::new(HashMap::new())),
        lets: (want == Want::Lets).then(|| RefCell::new(HashMap::new())),
        cur_fn: RefCell::new(String::new()),
        writeback: RefCell::new(None),
        lending: (want == Want::Lets).then(|| RefCell::new(HashSet::new())),
        forwards: (want == Want::Lets).then(|| RefCell::new(HashMap::new())),
        retains: (want == Want::Lets).then(|| RefCell::new(HashSet::new())),
        handed_on: (want == Want::Lets).then(|| RefCell::new(HashMap::new())),
        param_ix: RefCell::new(HashMap::new()),
        store_events: (want == Want::Lets).then(|| RefCell::new(Vec::new())),
        param_escapers: (want == Want::Lets).then(|| RefCell::new(HashSet::new())),
        carrying_locals: RefCell::new(HashSet::new()),
        exit_sites: (want == Want::Lets).then(|| RefCell::new(Vec::new())),
        lambda_arities: (want == Want::Lets).then(|| RefCell::new(Default::default())),
        typed_lambdas: (want == Want::Lets).then(|| RefCell::new(Default::default())),
        lambda_sigs: (want == Want::Lets).then(|| RefCell::new(Default::default())),
        walk_region: std::cell::Cell::new(0),
        ev_order: std::cell::Cell::new(0),
        loop_ids: RefCell::new(Vec::new()),
        next_loop: std::cell::Cell::new(0),
        branch_ids: RefCell::new(Vec::new()),
        next_branch: std::cell::Cell::new(0),
        exit_orders: (want == Want::Lets).then(|| RefCell::new(Vec::new())),
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
    for f in &program.functions {
        mc.errors.borrow_mut().clear();
        mc.function(f);
        drain(&mut projections, &mc, &f.module);
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
    for (i, t) in program.tests.iter().enumerate() {
        mc.errors.borrow_mut().clear();
        // The synthetic name `own::analyze` keys this body's rows by — the
        // finish check (RFC-0114 §26) matches owners against emitted names.
        *mc.cur_fn.borrow_mut() = format!("test@{i}");
        mc.body(&[], &Type::Unit, &t.body);
        drain(&mut projections, &mc, &t.module);
        for s in mc.errors.borrow_mut().drain(..) {
            let mut d = s;
            d.file = t.module.clone();
            out.push(d);
        }
    }
    // Bench bodies (RFC-0055) move-check identically.
    for (i, b) in program.benches.iter().enumerate() {
        mc.errors.borrow_mut().clear();
        *mc.cur_fn.borrow_mut() = format!("bench@{i}");
        mc.body(&[], &Type::Unit, &b.body);
        drain(&mut projections, &mc, &b.module);
        for s in mc.errors.borrow_mut().drain(..) {
            let mut d = s;
            d.file = b.module.clone();
            out.push(d);
        }
    }
    // RFC-0075's disposal obligation is NOT here, and RFC-0125 §3 M3's
    // obligation slice is why: it is a rule about a TYPE and this file states
    // rules about ownership. It was always a separate walk over the same
    // bodies — the two analyses want OPPOSITE merges at an `if`, because
    // use-after-consume is a may-analysis (consumed on either branch ⇒
    // consumed after) and "disposed exactly once" is a must-analysis — and it
    // is now the typed judgment's (`vyrn_lower::typed::obligation`), reached
    // from [`refusals`] through `own::must_use_refusals`.
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
    if std::env::var_os("VYRN_LEND_DUMP").is_some() {
        eprintln!("lending closed: {lending:?}");
        eprintln!("retains closed: {retains:?}");
    }
    // Round forty-six's meet, over the closed target set of every fn-value
    // signature the program declares. It waits for both closures above, for
    // the reason they exist: whether a position keeps what it is given is only
    // settled once every body has been read.
    //
    // The core asks this by signature at the call it lowers, because a call
    // through a fn value names no function and no capability row answers for
    // its positions (RFC-0125 §3 M3, the last table's slice — the plan asked
    // it per argument row until then).
    let lambda_arities = mc
        .lambda_arities
        .map(RefCell::into_inner)
        .unwrap_or_default();
    let lambda_sigs = mc.lambda_sigs.map(RefCell::into_inner).unwrap_or_default();
    let mut sig_groups: HashMap<String, (usize, Vec<String>)> = HashMap::new();
    for f in &program.functions {
        let ps: Vec<Type> = f.params.iter().map(|p| p.ty.clone()).collect();
        let key = fn_sig_key(&ps, &f.ret, decl.decls());
        sig_groups
            .entry(key)
            .or_insert_with(|| (ps.len(), Vec::new()))
            .1
            .push(f.name.clone());
    }
    let mut fnval_clear: HashSet<String> = HashSet::new();
    for (key, (arity, members)) in &sig_groups {
        if lambda_arities.contains(arity) || lambda_sigs.contains(key) {
            continue;
        }
        let all_clear = !members.is_empty()
            && members.iter().all(|m| {
                // The meet is per POSITION in the plan's reading, and the
                // position it was asked about is the one the temporary sits
                // in. A signature every one of whose members reads EVERY
                // position, retains nothing anywhere and lends nothing is
                // clear at every position, which is the answer a key can
                // carry.
                !lending.contains(m)
                    && caps.get(m).is_some_and(|cs| {
                        cs.iter().enumerate().all(|(ix, c)| {
                            *c == Capability::Read && !retains.contains(&(m.clone(), ix))
                        })
                    })
            });
        if std::env::var("VYRN_MEET_DUMP").is_ok() {
            eprintln!("meet: key={key} members={members:?} clear={all_clear}");
        }
        if all_clear {
            fnval_clear.insert(key.clone());
        }
    }
    Run {
        diags: out,
        sites: mc.sites.map(RefCell::into_inner).unwrap_or_default(),
        lets: mc.lets.map(RefCell::into_inner).unwrap_or_default(),
        lending,
        retains,
        projections,
        store_events: mc.store_events.map(RefCell::into_inner).unwrap_or_default(),
        exit_orders: mc.exit_orders.map(RefCell::into_inner).unwrap_or_default(),
        param_escapers: mc
            .param_escapers
            .map(RefCell::into_inner)
            .unwrap_or_default(),
        exit_sites: mc.exit_sites.map(RefCell::into_inner).unwrap_or_default(),
        fnval_clear,
    }
}

/// The refusal THIS PASS states about `program`, rendered as the historical
/// `"line {N}: {message}"` string. Thin shim over [`check_accum`], and the one
/// door a test asks this pass through.
///
/// **There is no acceptance answer here, and there may not be one**
/// (RFC-0125 §3 M3, the safety slice). Two passes judge ownership: this one and
/// the kernel, which states rules this file no longer holds and which
/// `vyrn-frontend` does not link. So this pass going quiet means it has nothing
/// to say, and a reader who takes that for "the compiler accepts the program"
/// states a second, weaker rule. Three unit tests did. One of them pinned
/// `for x in consume r.xs` as compiling; the compiler has refused it since the
/// kernel came in.
///
/// A test that wants "this program compiles" asks the whole compiler, in
/// `compiler/vyrn-cli/tests/refusals.rs`, which links the kernel and runs
/// `vyrn check`. This function panics where the pass is silent, and that is the
/// guard: the wrong pin cannot be written, because there is no value to write
/// it against.
pub fn refusal(program: &Program) -> String {
    match check_accum(program).into_iter().next() {
        Some(d) => d.render(),
        None => panic!(
            "this pass states no refusal about the program, and silence is not \
             acceptance: the kernel states ownership rules it does not, and \
             `vyrn-frontend` does not link the kernel. Pin acceptance in \
             `compiler/vyrn-cli/tests/refusals.rs`, which asks the whole compiler \
             (RFC-0125 §3 M3, the safety slice)."
        ),
    }
}

struct MoveCheck<'a> {
    caps: &'a HashMap<String, Vec<Capability>>,
    /// The program's impl blocks — what selects a `place atSet` projection,
    /// so the facts walk can read a projection store as the desugared group
    /// the lowering emits (round fifty-seven).
    impls: &'a [crate::ast::ImplBlock],
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
    /// What each borrow READS — the place's path and the binding's line — in
    /// lockstep with `borrows`. The fix for a borrow a rebuilding call takes
    /// is a `.copy()` where the borrow is bound. A write to that place ends
    /// the borrow, and the kernel is the pass that says so now (RFC-0125 §3
    /// M3, row 05).
    reads: RefCell<Scopes<Option<(String, usize)>>>,
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
    /// RFC-0125 M2: the binding a write-back statement `xs = xs.push(v)` is
    /// assigning, while its value is walked. A rebuilding row takes its
    /// receiver (`sinks`), and the statement form takes and revives it in one
    /// line — so the take is not recorded there, and the receiver is read.
    writeback: RefCell<Option<String>>,
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
    /// RFC-0114 M2: the write/take event stream (see [`StoreEv`]), and the
    /// assigns to module state, which are owned unconditionally.
    store_events: Option<RefCell<Vec<StoreEv>>>,
    param_escapers: Option<RefCell<HashSet<String>>>,
    /// Round fifty-six: per-body provenance for the escape screen — locals
    /// whose value may HOLD a borrowed parameter's storage (`let r =
    /// a.push(v)`), so a later `return r` reads as the escape it is. Cleared
    /// per body; consulted only where `param_escapers` records.
    carrying_locals: RefCell<HashSet<String>>,
    exit_sites: Option<RefCell<Vec<ExitEv>>>,
    /// Round forty-six: the arity of every lambda the walk met. A lambda has
    /// no capability rows and no retention rows, so a signature any lambda
    /// could inhabit (matched by arity — the declared reading does not type
    /// lambdas) stands down from the fn-value meet below.
    lambda_arities: Option<RefCell<std::collections::HashSet<usize>>>,
    /// Round fifty-five: lambdas whose type IS known — they sit in an
    /// argument position whose declared parameter is a fn type. They poison
    /// only their own signature, not their whole arity.
    typed_lambdas: Option<RefCell<std::collections::HashSet<usize>>>,
    /// The signature keys those typed lambdas inhabit.
    lambda_sigs: Option<RefCell<std::collections::HashSet<String>>>,
    walk_region: std::cell::Cell<u32>,
    ev_order: std::cell::Cell<u32>,
    /// The stack of loop ids the walk is inside — pushed by `while`/`for`,
    /// stamped onto every event, so the fold can see which pairs of events a
    /// back edge could reorder.
    loop_ids: RefCell<Vec<u32>>,
    next_loop: std::cell::Cell<u32>,
    /// The stack of branch ids (`if`/`match` arms) the walk is inside, and the
    /// walk-order positions of every early exit (`return`, `?`, `break`,
    /// `continue`) — both feed the untake fold and nothing else.
    branch_ids: RefCell<Vec<u32>>,
    next_branch: std::cell::Cell<u32>,
    exit_orders: Option<RefCell<Vec<u32>>>,
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

/// The base name of a place path: `r.a[0]` is `r`.
///
/// A message names a PATH and a borrow names the PARAMETER it came from, so the
/// two are comparable only at the root.
pub fn root_of(path: &str) -> &str {
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
                // RFC-0114: a `consume` parameter is a value this frame OWNS,
                // and until now it was the one owned value with no row — the
                // callee released it only if the body wrote `drop v`, and a
                // body that merely read it leaked its argument every call. It
                // gets a row keyed by the `Param` node, exactly as a `let` is
                // keyed by its statement; every take writes onto that row, so
                // a param that is moved on, dropped, or returned releases
                // nothing here, same as a `let`. A BORROWED parameter stays at
                // node 0 — minting a row for one releases somebody else's
                // value, which is the `argsdemo` corruption Phase 10a records.
                if p.capability == Capability::Consume && self.decl.owns_heap(&p.ty) {
                    let key = p as *const Param as usize;
                    n.bind(&p.name, key);
                    if let Some(sink) = &self.lets {
                        sink.borrow_mut().insert(
                            key,
                            LetOwnership {
                                ty: Some(p.ty.clone()),
                                gone: None,
                                from_call: None,
                                passed: Vec::new(),
                                elem_only: None,
                                elem_name: None,
                            },
                        );
                    }
                    self.store_ev(
                        key,
                        EvKind::Write {
                            id: 0,
                            owning: true,
                        },
                    );
                } else {
                    n.bind(&p.name, 0);
                }
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
        self.carrying_locals.borrow_mut().clear();
        self.block(body, &mut consumed, &mut scope);
    }

    /// Push a frame on ALL THREE stacks. They are read as one environment — a
    /// name's type, whether it is a borrow, and where it was declared — so they
    /// are never entered apart.
    fn enter(&self) {
        self.vars.borrow_mut().enter();
        self.borrows.borrow_mut().enter();
        self.reads.borrow_mut().enter();
        self.nodes.borrow_mut().enter();
    }

    fn exit(&self) {
        self.vars.borrow_mut().exit();
        self.borrows.borrow_mut().exit();
        self.reads.borrow_mut().exit();
        self.nodes.borrow_mut().exit();
    }

    /// Bind `name` with its type and its borrow status. Every binder that is not
    /// a `let` gets node 0 — it declares no reclaimable binding, and recording it
    /// is what stops it inheriting an outer `let`'s identity.
    fn bind(&self, name: &str, ty: Option<Type>, borrow: Option<Borrow>) {
        self.vars.borrow_mut().bind(name, ty);
        self.borrows.borrow_mut().bind(name, borrow);
        self.reads.borrow_mut().bind(name, None);
        self.nodes.borrow_mut().bind(name, 0);
    }

    /// The place a borrow of `value` reads, when the value spells one: `h.meta`,
    /// `xs[i]`, or a whole borrowed name.
    fn read_of(value: &Expr) -> Option<String> {
        place_path(value)
            .or_else(|| element_path(value))
            .map(|(_, path)| path)
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
                elem_only: None,
                elem_name: None,
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
            branch: self.branch_ids.borrow().clone(),
            kind,
            owner: self.cur_fn.borrow().clone(),
        });
    }

    fn enter_branch(&self) {
        let b = self.next_branch.get();
        self.next_branch.set(b + 1);
        self.branch_ids.borrow_mut().push(b);
    }

    fn leave_branch(&self) {
        self.branch_ids.borrow_mut().pop();
    }

    /// An early exit passed this point in walk order (`return`, `?`, `break`,
    /// `continue`). The untake fold refuses any binding whose take-to-revive
    /// window contains one: on that exit the binding still holds the taken
    /// state, and the exit path's releases must not touch it.
    /// Round twenty-seven: one READ of a tracked binding advances the event
    /// order, so a read is strictly ordered against the writes, takes and
    /// exits around it and every fold that compares orders sees the numbers
    /// it saw before.
    ///
    /// The reads themselves are nobody's since RFC-0125 §3 M3's third
    /// derivation slice: the core counts the reads of a name over its own
    /// statements, where a payload binder is a name of its own.
    fn mention_ev(&self, name: &str) {
        if self.lets.is_none() || self.nodes.borrow().get(name).copied().unwrap_or(0) == 0 {
            return;
        }
        self.ev_order.set(self.ev_order.get() + 1);
    }

    /// Round twenty-one: record a `return`/`?` with its placement context.
    /// Called BEFORE `exit_ev`, so the two share the order the exit runs at.
    fn exit_site(&self, site: usize, is_try: bool) {
        let Some(sink) = &self.exit_sites else { return };
        sink.borrow_mut().push(ExitEv {
            order: self.ev_order.get(),
            site,
            is_try,
            fn_name: self.cur_fn.borrow().clone(),
            loops: self.loop_ids.borrow().clone(),
            clean: self.walk_region.get() == 0 && self.lambda_base.borrow().is_empty(),
        });
    }

    fn exit_ev(&self) {
        if let Some(x) = &self.exit_orders {
            let o = self.ev_order.get();
            self.ev_order.set(o + 1);
            x.borrow_mut().push(o);
        }
    }

    fn took(&self, name: &str, gone: Gone) {
        self.mention_ev(name);
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
            // Which-take attribution (round sixteen): a take through the loop
            // variable bound to this row is an element leaving — the value was
            // copied OUT of the buffer. Any other taker poisons the row.
            let via_elem = row.elem_name.as_deref() == Some(name);
            row.elem_only = Some(row.elem_only.unwrap_or(true) && via_elem);
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
            // A hole taken out of the loop element still leaves through the
            // element — the payload was copied out (round sixteen).
            let via_elem = row.elem_name.as_deref() == Some(name);
            row.elem_only = Some(row.elem_only.unwrap_or(true) && via_elem);
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
    /// Walk the value of a store into `target` (a name or a field path). When
    /// the value is a rebuilding row applied to that same place —
    /// `xs = xs.push(v)`, `s.keys = s.keys.push(k)` — the receiver comes back
    /// through the result and the store revives the place, so the take the
    /// row would record is not recorded (RFC-0125 M2, `sinks`).
    fn walk_writeback(
        &self,
        target: &str,
        value: &Expr,
        consumed: &mut Consumed,
        scope: &mut Vec<HashSet<String>>,
    ) -> Result<(), Diagnostic> {
        let writeback = matches!(
            value,
            Expr::Call { name: callee, args, .. }
                if self.sinks(callee, 0)
                    && args.first().and_then(store_path).as_deref() == Some(target)
        );
        if writeback {
            *self.writeback.borrow_mut() = Some(target.to_string());
        }
        let walked = self.expr(value, consumed, scope);
        *self.writeback.borrow_mut() = None;
        walked
    }

    fn gave_up(&self, e: &Expr, gone: &Gone) {
        for n in reads(e) {
            self.took(&n, gone.clone());
        }
    }

    /// Rule 3's marking at a RETURN, narrowed (exit-residue round seven): a
    /// returned call whose callee is a KNOWN function carries an owned
    /// result that contains none of its read arguments — rule 3 refuses
    /// returning a borrow, and the positions the rule excepts are settled
    /// by the `facts()` post-passes reading `row.passed` (a lender marks
    /// them Lent, a retaining position marks them too). Marking everything
    /// under the call `Returned` left `out`'s buffer unreleased at every
    /// `return fromBytesOr(out, ..)` in `std/` — one scratch buffer per
    /// byte-building call, twelve sites and every caller's own spelling of
    /// the shape.
    ///
    /// The narrowing asks three guards, and every "no" keeps the
    /// conservative walk: a variant constructor RETAINS its arguments, a
    /// view lends a place inside one, and a name any scope binds is a
    /// `fn`-typed local whose body nothing can read.
    fn gave_up_returned(&self, e: &Expr, gone: &Gone) {
        let off = std::env::var("VYRN_RET_NARROW_OFF").unwrap_or_default();
        if off == "all" {
            return self.gave_up(e, gone);
        }
        if let Ok(skip) = std::env::var("VYRN_RET_NARROW_SKIP") {
            let f = self.cur_fn.borrow();
            if skip.split(',').any(|s| s == f.as_str()) {
                return self.gave_up(e, gone);
            }
        }
        match e {
            Expr::Call { name, .. } if off.contains("call") => self.gave_up(e, gone),
            Expr::Match { .. } if off.contains("join") => self.gave_up(e, gone),
            Expr::IfExpr { .. } if off.contains("join") => self.gave_up(e, gone),
            Expr::Binary { .. } if off.contains("add") => self.gave_up(e, gone),
            // A returned CONSTRUCTOR recurses per argument (exit-residue
            // round eleven): `return Err(errAt(p, ..))` read `p` through the
            // constructor and the conservative walk marked it Returned on
            // EVERY path of the function — `parseJson`'s cursor record, and
            // its whole byte buffer, leaked once per call because of the
            // error return at the bottom. A direct place argument still
            // falls to the conservative walk below (and the call walk has
            // already moved or refused it at the constructor position); a
            // call argument's reads are rule 3's business, same as at the
            // top level.
            Expr::Call { name, args, .. } if self.decl.constructs(name) => {
                for a in args {
                    // A CONSUMED place argument accounts for itself: the
                    // take already recorded its hole, only the taken field
                    // is in the returned value, and marking the ROOT
                    // Returned made the whole binding unreleasable —
                    // `return Ok(consume bd.op)` left every OTHER Builder
                    // field with no owner (exit-residue round fifty-one).
                    if matches!(a, Expr::Consume { .. }) {
                        continue;
                    }
                    self.gave_up_returned(a, gone);
                }
            }
            // The copying builtins build fresh storage out of what they
            // READ — `@copy`'s row is held back from the return table (its
            // type is its receiver's), so the is_function screen below never
            // clears it, and `return Err(bd.err.copy())` marked the whole
            // record moved into the return: every OTHER field lost its
            // owner. Round fifty's first cut of this un-masked a DOUBLE
            // release — round forty-four's Hole rows in the early fold — and
            // round fifty-one removed those (a holed binding places
            // structurally), which is what makes this narrowing sound.
            Expr::Call { name, .. } if name == "@copy" || name == "@str" || name == "@concat" => {}
            // The codec forms build FRESH values out of what they read —
            // `fromJson(T, s)` parses and copies, `toJson(v)` renders — so
            // neither result can carry an argument's storage, and neither is
            // a program function the screen below could clear (exit-residue
            // round fifty-six: `return match fromJson(IdReq, arg.json) { .. }`
            // — the generated GraphQL resolver arm — marked `arg` moved into
            // the return on every path, and the argument JSON leaked once per
            // argument-carrying resolve).
            Expr::Call { name, .. } if name == "fromJson" || name == "toJson" => {}
            // A NUMERIC CAST is a call spelled with a type's name and returns
            // a scalar copy — `return Some(Float32(toFloat(sc, ..)))` marked
            // `sc` through the cast the conservative walk could not see
            // through (exit-residue round fifty-four). The width names are
            // the checker's own cast set; the argument keeps its own
            // reading.
            Expr::Call { name, args, .. }
                if matches!(
                    name.as_str(),
                    "Int"
                        | "Int8"
                        | "Int16"
                        | "Int32"
                        | "Int64"
                        | "UInt8"
                        | "UInt16"
                        | "UInt32"
                        | "UInt64"
                        | "Float"
                        | "Float32"
                        | "Float64"
                ) =>
            {
                for a in args {
                    self.gave_up_returned(a, gone);
                }
            }
            Expr::Call { name, .. } => {
                // A seeded row whose return is the same bare parameter as an
                // argument's may hand that argument straight back —
                // `blackBox` is the identity — which `views` does not cover
                // (it lends no PLACE, it returns the value itself). Same
                // guard as `arg_verdict`'s.
                let hands_back = crate::prelude::signature(name).is_some_and(|f| {
                    matches!(&f.ret, Type::Param(r)
                        if f.params.iter().any(|p| matches!(&p.ty, Type::Param(q) if q == r)))
                });
                if self.decl.is_function(name)
                    && !self.decl.constructs(name)
                    && !views(name)
                    && !hands_back
                    && self.vars.borrow().get(name).is_none()
                {
                    return;
                }
                self.gave_up(e, gone);
            }
            // The value-position joins recurse per arm — `return match
            // stringFromBytes(out) { Ok(v) => v, Err(e) => "" }` is the
            // spelling every byte-builder in `std/` ends on, and marking the
            // whole match conservatively re-leaked exactly what the call arm
            // above releases.
            Expr::Match {
                scrutinee, arms, ..
            } => {
                // The scrutinee's row is marked, and NOT as a read: every arm
                // has already written its own verdict there.
                self.gave_up_returned(scrutinee, gone);
                for arm in arms {
                    if let crate::ast::ArmBody::Expr(b) = &arm.body {
                        self.gave_up_returned(b, gone);
                    }
                }
            }
            // The condition's reads are a test, not the result — nothing it
            // names can be inside the returned value, so only the branches
            // are walked.
            Expr::IfExpr {
                then_branch,
                else_branch,
                ..
            } => {
                self.gave_up_returned(then_branch, gone);
                if let Some(eb) = else_branch {
                    self.gave_up_returned(eb, gone);
                }
            }
            Expr::Try { expr, .. } => self.gave_up_returned(expr, gone),
            // A returned STRUCT LITERAL is a constructor spelled with braces
            // (exit-residue round fifty-three): its fields take the same
            // per-argument reading a constructor call's arguments do, and
            // marking the whole literal re-marked what the fields already
            // account for — `return Ok(Regex { op: consume bd.op, nsets:
            // bd.nsets, .. })` marked bd moved-into-the-return on its
            // SUCCESS path, and the un-consumed `pat` buffer lost its owner
            // once per compile.
            Expr::StructLit { fields, .. } => {
                for (_, v) in fields {
                    if matches!(v, Expr::Consume { .. }) {
                        continue;
                    }
                    self.gave_up_returned(v, gone);
                }
            }
            // A place read whose type owns no heap travels BY VALUE — a
            // scalar copied into the result carries none of the binding's
            // storage, so it marks nothing (the same top-level screen
            // `read_only_mentions` applies, one position over).
            e2 if place_path(e2).is_some()
                && self.type_of(e2).is_some_and(|t| !self.decl.owns_heap(&t)) => {}
            // `return acc + "}"` — the shape every `std/` emitter ends on. A
            // String `+` is `@concat`, which copies both operands into a
            // fresh buffer; a `Code +` concatenates arena handles; every
            // other `+` is scalar. Nothing an addition reads is inside its
            // result, so nothing is marked.
            Expr::Binary { op: BinOp::Add, .. } => {}
            _ => self.gave_up(e, gone),
        }
    }

    /// Whether a `let` of `value` names storage somebody else owns, for
    /// reclamation purposes.
    ///
    /// Wider than [`MoveCheck::borrow_from`], which answers rule 2 and therefore
    /// only fires where this reading can name a type. Reclamation must be right
    /// where the type is unknown too, so this asks the SHAPE: a field read, a
    /// view builtin, module state, or a name that is itself a borrow.
    /// Round twenty-nine: a bare name that is BOUND — a parameter included —
    /// is never a temporary, whatever `names_a_place` says about it. A read
    /// parameter whose type owns no heap carries no borrow (rule 2 has
    /// nothing to borrow), so the temporary-row guards used to fall through
    /// for exactly those and mint a row that released the CALLER's value —
    /// masked while such types had no release kind, exposed the day
    /// `Option<Handle>` got one.
    fn is_bound_name(&self, e: &Expr) -> bool {
        matches!(e, Expr::Var { name, .. } if self.vars.borrow().frame_of(name).is_some())
    }

    fn names_a_place(&self, value: &Expr) -> Option<&'static str> {
        match value {
            // A field read is somebody's place only when its base chain roots
            // at one. A field of a TEMPORARY — `makeRec(i).name` — is read out
            // of a value NOBODY owns: no row, no release, and calling it a
            // borrow was the leak (RFC-0114, the last receiver case). The
            // binding takes ownership of the extracted buffer instead; the
            // recursion keeps a view builtin's field a borrow, and a user
            // function cannot return a borrow at all ("a return is owned"),
            // so there is no owner left to double-free against.
            Expr::Field { expr, .. } => {
                if place_path(value).is_some() || element_path(value).is_some() {
                    Some("read out of a place that owns it")
                } else {
                    self.names_a_place(expr)
                }
            }
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
            // A block arm (RFC-0118) yields no value, so it names no place.
            Expr::Match {
                scrutinee, arms, ..
            } => arms.iter().find_map(|a| {
                let e = a.body.as_expr()?;
                // RFC-0121: an arm that yields a binder its own pattern bound
                // reads the scrutinee's PAYLOAD — a place whoever owns the
                // scrutinee still owns. Only when the scrutinee itself names
                // a place: matching a temporary hands its payload over, and
                // calling that a borrow would leak it (the same line
                // `Expr::Field` draws above). Without this, `let items =
                // match g { JArr(items) => items, .. }` on module state read
                // as a fresh owned value, and `own` freed the global's buffer
                // out from under it.
                if let Expr::Var { name, .. } = e {
                    if pattern_bindings(&a.pattern).contains(&name.as_str())
                        && self.names_a_place(scrutinee).is_some()
                    {
                        return Some("the payload of a place somebody owns");
                    }
                }
                self.names_a_place(e)
            }),
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

    /// Whether `name` is a NULLARY constructor rather than a binding.
    ///
    /// `None`, `Nothing`, `Leaf` — a variant with no payload parses as a bare
    /// name, and a bare name is what every rule about ownership keys on. It is
    /// a value with no owner: nothing binds it, nothing releases it, and two
    /// mentions of it are two values (RFC-0126 §8.8). Reading it as a binding
    /// made `take(None)` twice a use after a take.
    ///
    /// The builtins are named beside the declared ones because `Option` and
    /// `Result` are not `Type::Enum` declarations, so `Declared::constructs`
    /// does not answer for their variants.
    fn names_a_constructor(&self, name: &str) -> bool {
        self.decl.constructs(name)
            || matches!(name, "None" | "Some" | "Ok" | "Err" | "Success" | "Failure")
    }

    /// Whether `name` names a borrow here.
    fn borrow_of(&self, name: &str) -> Option<Borrow> {
        self.borrows.borrow().get(name).cloned().flatten()
    }

    /// The type of `e` here, or `None` where nothing names it.
    ///
    /// [`crate::declared::Declared::type_of`] — the checker's own answer for
    /// this node — and then the readings only this pass may have, which are the
    /// ones a record has a HOLE at: a node the checker did not walk, in a body
    /// a synthesis added after it. The widening stays here and not in
    /// `Declared` for the reason it always did: what this method answers
    /// decides what a program may SAY, and a type `Declared` answers decides
    /// what a program FREES.
    fn type_of(&self, e: &Expr) -> Option<Type> {
        if let Some(t) = self.decl.type_of(e) {
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
            // answered by `Declared`. The element type is the container's. A
            // NAMED projection (RFC-0120) reads an element too; the checker's
            // record answers for one, and where there is no record `elem_of`
            // answers only for builtin containers, so the named form falls
            // through to `None` exactly as `@at` on a user container does.
            Expr::Call { name, args, .. } if projection_call(name) => {
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
                // A block arm (RFC-0118) yields nothing to have a type.
                let t = arm.body.as_expr().and_then(|e| self.type_of(e));
                self.exit();
                t
            }
            _ => None,
        }
    }

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

    /// A store of `value` into `into` — RFC-0089 rule 1's move, and nothing else.
    ///
    /// `into` is the destination in words ("the binding `t`", "the field `r.s`").
    /// The answer is whether the store **took** the source place, which is what
    /// Phase 4c reads: a place that did not move is still somebody else's. A
    /// fresh value, a scalar, a borrow and a projection all take nothing.
    ///
    /// Rule 2's refusal was here — a borrow or a projection put anywhere that
    /// outlives the call — and it is the kernel's now (RFC-0125 §3 M3, census
    /// rows 01, 02, 03, 27 and 34). `outlives` stays, because it is what tells
    /// a store from a rebinding and the retention row is keyed on it.
    ///
    /// A type this reading cannot name does NOT move. That is the same
    /// under-approximation `own.rs` makes and it costs the same thing — a row
    /// that is not written. It is not the unsound direction: every value it
    /// fails to move is one today's engines already leak rather than free twice.
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
    ) -> bool {
        // An ELEMENT read stored inline: `out.push(xs[i])`. `xs[i]` reaches this
        // pass as `@at(xs, i)`, which is a call, so the `place_path` bail two
        // blocks down is where it used to leave — invisible to every rule.
        if let Some((_, path)) = element_path(value) {
            if self.projections.is_some() && outlives {
                let ty = self.type_of(value);
                self.note_projection("elem-store", &path, into(), ty, line);
            }
            if outlives && self.type_of(value).is_some_and(|t| self.decl.owns_heap(&t)) {
                self.note_retention(value);
                return false;
            }
        }
        let Some((root, path)) = place_path(value) else {
            return false;
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
            return false;
        }
        // A borrow does not move: the caller still owns it. The retention row
        // says the borrow was put somewhere that outlives the call, which is
        // what the call graph is closed over.
        if self.borrow_of(&root).is_some() {
            if outlives {
                self.note_retention(value);
            }
            return false;
        }
        // A projection does not move either: reading a field out of a record
        // does not take the record.
        if path != root {
            return false;
        }
        self.took(&root, Gone::Moved { line, by: into() });
        consumed.insert(root, Consumption { line, hole: false });
        true
    }

    /// The borrow status a `let` of `value` gives its binding.
    ///
    /// A field or element read binds a [`Borrow::Projection`]: the aggregate
    /// still owns it, so the new name may be read but not stored on. Everything
    /// else — a fresh value, or a place that just moved — binds an owner.
    fn borrow_from(&self, value: &Expr) -> Option<Borrow> {
        // RFC-0121: `let x = match p { V(b) => b, .. }` — the refutable-`let`
        // desugar, and its handwritten equivalent. Every value arm yields a
        // binder its own pattern bound out of the scrutinee, so the binding
        // is a projection of the scrutinee's place — the payload the enum
        // still owns. Without this the binding read as a fresh OWNED value,
        // `own` reclaimed it at block exit, and the enum's buffer was freed
        // out from under the enum (`vyrn why --memory` said so verbatim).
        if let Expr::Match {
            scrutinee, arms, ..
        } = value
        {
            let all_borrow = arms.iter().all(|arm| match arm.body.as_expr() {
                None => false,
                Some(Expr::Call { name, .. }) if crate::ast::is_panic(name) => true,
                Some(Expr::Var { name, .. }) => {
                    pattern_bindings(&arm.pattern).contains(&name.as_str())
                }
                Some(_) => false,
            });
            if all_borrow {
                if let Some((root, _)) = place_path(scrutinee).or_else(|| element_path(scrutinee)) {
                    return Some(self.borrow_of(&root).unwrap_or(Borrow::Projection));
                }
            }
            return None;
        }
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
            // One variant list for every sum since RFC-0126 §8.11's M4b, and
            // `??`'s pair names a TAG in it: variant 1 succeeds, variant 0 fails.
            (Some(Type::Enum(vs)), Pattern::Variant(name, _)) => vs
                .iter()
                .find(|v| &v.name == name)
                .map(|v| v.payload.iter().cloned().map(Some).collect())
                .unwrap_or_else(|| vec![None; n]),
            // `??`'s pair (RFC-0079) names a TAG. It types the binder on a
            // `Result` and not on an `Option`, which is where it stood before
            // RFC-0126 §8.11's M4b and is a defect of its own — §8.13 records it
            // rather than fixing it here, because fixing it moves bytes and this
            // step promised none.
            (Some(ref r), Pattern::Success(_) | Pattern::Failure(_))
                if crate::types::result_payloads(r).is_some() =>
            {
                let (ok, err) = crate::types::result_payloads(r).expect("the guard just asked");
                match p {
                    Pattern::Success(_) => vec![Some(ok.clone())],
                    _ => vec![Some(err.clone())],
                }
            }
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
            Expr::Call { name, args, .. } if projection_call(name) => {
                args.first().is_some_and(|a| self.iterable_is_a_place(a))
            }
            _ => false,
        }
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

    /// Rule 3's RECORD, and no longer its refusal (RFC-0125 §3 M3, row 17).
    ///
    /// A returned borrow was refused here at three exits — a place named
    /// straight at the `return`, a projection of a place the frame owns, and a
    /// borrow yielded by a `match` or an `if` arm — and by one shared sentence
    /// under them, which carried the export's own words. The kernel states the
    /// rule at ONE exit now, because the core carries the exit into the arms
    /// and does not release the place a returned projection reads out of.
    /// Sixteen programs of the corpus reach the rule and all sixteen get the
    /// same refusal with the checker standing aside, menu included.
    ///
    /// What is left is what nothing else states: RFC-0092's instrument counts
    /// the returned projections, and [`MoveCheck::lends`] records that this
    /// function hands a borrow on. That record is read after every body is
    /// walked, and half of it — the lend through a wrapper — was never a
    /// refusal at all.
    fn note_return(&self, e: &Expr, line: usize) {
        self.note_returned_projection(e, line);
        if !self.decl.releases(&self.ret.borrow()) {
            return;
        }
        // A place named straight at the `return` is no lend to record: the
        // refusal was the whole of what this walk had to say about it.
        if place_path(e).is_some() {
            return;
        }
        if self.returned_borrow(e).is_some() || self.lends_through_a_wrapper(e).is_some() {
            self.lends();
        }
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

    /// Can a call to `name` return storage one of its arguments holds? The
    /// copying builtins cannot — `@concat`, `@str`, `@copy` and every seeded
    /// row that neither hands an argument back (identity-typed return), views,
    /// nor lends builds a fresh value. Everything else — an `@`-desugar like
    /// `@push`, a user function — is assumed able to, which is the leak
    /// direction.
    fn call_may_forward(&self, name: &str) -> bool {
        call_may_forward(name)
    }

    /// The body of the free [`call_may_forward`], kept beside its one caller.
    fn call_may_forward_body(name: &str) -> bool {
        if matches!(name, "@concat" | "@str" | "@copy") {
            return false;
        }
        // Every other `@`-desugar forwards until proven otherwise — `@push`'s
        // seeded row returns `Array<T>`, not a bare parameter, and it hands
        // its receiver's buffer back all the same (map.vyrn's double free,
        // twice caught by parity's free audit).
        if name.starts_with('@') {
            return true;
        }
        if crate::prelude::signature(name).is_some() {
            return hands_back(name) || views(name) || crate::prelude::lends(name);
        }
        true
    }
    /// Can evaluating `e` yield a value that HOLDS a borrowed parameter's
    /// storage? Storage flow, not mention (round nineteen): a copying builtin
    /// (`bytes`, `@concat`, `@str`, `@copy`) and an operator both build fresh
    /// values however many parameters they read, so a value built from them
    /// carries nothing. A seeded builtin that neither hands an argument back
    /// (identity-typed return), views, nor lends is fresh by its row. Every
    /// other call — an `@`-desugar like `@push`, a user function — is assumed
    /// to forward whatever its arguments carry, which is the leak direction.
    fn carries_param_storage(&self, e: &Expr) -> bool {
        // A value whose type owns no heap carries no storage — a byte literal,
        // an index into a byte string, a length. Asked first, because the
        // conservative fallback below would otherwise mark on every expression
        // shape this walk does not name.
        if self.type_of(e).is_some_and(|t| !self.decl.owns_heap(&t)) {
            return false;
        }
        match e {
            // Round fifty-six: a LOCAL assigned a carrying value carries it in
            // turn — `let r = a.push(v); return r` launders the buffer through
            // a name, and the per-body provenance set is what still sees it
            // now that the escape is recorded at the return rather than at
            // every interior call.
            Expr::Var { name, .. } => {
                matches!(
                    self.borrow_of(name),
                    Some(Borrow::Read(_)) | Some(Borrow::Modify(_))
                ) || self.carrying_locals.borrow().contains(name)
            }
            Expr::Str(_) | Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) => false,
            Expr::Binary { .. } | Expr::Unary { .. } => false,
            Expr::Consume { place, .. } => self.carries_param_storage(place),
            Expr::Call { name, args, .. } => {
                self.call_may_forward(name) && args.iter().any(|a| self.carries_param_storage(a))
            }
            // The value-position joins yield one of their arms, so they carry
            // what an arm (or the scrutinee, through a binder) carries. The
            // binders SHADOW while an arm body is read — `sha1Hex(s: String)`
            // ends on `match stringFromBytes(out) { Ok(s) => s, .. }`, and
            // reading the arm's `s` as the parameter made the whole function
            // an escaper (199 digests leaked in `threeengines`' chain loop).
            // A binder over a CARRYING scrutinee is covered by the scrutinee
            // check short-circuiting first.
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.carries_param_storage(scrutinee)
                    || arms.iter().any(|a| {
                        let Some(b) = a.body.as_expr() else {
                            return false;
                        };
                        self.enter();
                        for n in pattern_bindings(&a.pattern) {
                            self.bind(n, None, None);
                        }
                        let r = self.carries_param_storage(b);
                        self.exit();
                        r
                    })
            }
            Expr::IfExpr {
                then_branch,
                else_branch,
                ..
            } => {
                self.carries_param_storage(then_branch)
                    || else_branch
                        .as_ref()
                        .is_some_and(|b| self.carries_param_storage(b))
            }
            // An aggregate literal carries what its parts carry.
            Expr::StructLit { fields, .. } => {
                fields.iter().any(|(_, v)| self.carries_param_storage(v))
            }
            Expr::ArrayLit { elems, .. } => elems.iter().any(|x| self.carries_param_storage(x)),
            Expr::MapLit { entries, .. } => entries
                .iter()
                .any(|(k, v)| self.carries_param_storage(k) || self.carries_param_storage(v)),
            other => {
                // A projection roots in a place; anything else unrecognized is
                // assumed to carry — the conservative direction.
                match place_path(other) {
                    Some((root, _)) => matches!(
                        self.borrow_of(&root),
                        Some(Borrow::Read(_)) | Some(Borrow::Modify(_))
                    ),
                    None => true,
                }
            }
        }
    }

    /// Round fifty-six: one field/element store's contribution to the escape
    /// screen. A carrying value written through a `modify` parameter or into
    /// module state leaves the call — the enclosing function is an escaper;
    /// written into a local place, the local carries it onward.
    fn note_carrying_store(&self, target: &str, value: &Expr) {
        if self.param_escapers.is_none() || !self.carries_param_storage(value) {
            return;
        }
        let root = target.split('.').next().unwrap_or(target);
        let root = root.trim_end_matches("[]");
        let outward = matches!(self.borrow_of(root), Some(Borrow::Modify(_)))
            || (self.globals.contains(root) && self.vars.borrow().frame_of(root) == Some(0));
        if outward {
            if let Some(sink) = &self.param_escapers {
                sink.borrow_mut().insert(self.cur_fn.borrow().clone());
            }
        } else {
            self.carrying_locals.borrow_mut().insert(root.to_string());
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
            if std::env::var_os("VYRN_LEND_DUMP").is_some() {
                eprintln!(
                    "retain seed: fn={} ix={ix} root={root} e={e:?}",
                    self.cur_fn.borrow()
                );
            }
            sink.borrow_mut().insert((self.cur_fn.borrow().clone(), ix));
        }
    }

    /// Record the function under check as one whose result the caller must not
    /// release, and note the plain `return f(..)` callees that pass it along.
    fn lends(&self) {
        if let Some(sink) = &self.lending {
            if std::env::var_os("VYRN_LEND_DUMP").is_some() {
                eprintln!("lend seed: {}", self.cur_fn.borrow());
            }
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
                    // A block arm (RFC-0118) is never a return value.
                    let r = arm.body.as_expr().and_then(|e| self.returned_borrow(e));
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
            // A borrow whose KNOWN type owns no heap is copied by value into
            // the payload and lends nothing — `JBool(b) => JBool(b)` in a
            // declared `copy` was read as a wrapped lend, which made the copy
            // a LENDER and stopped every `.copy()` result in the corpus from
            // being released (exit-residue round six). An unknown type keeps
            // the lend, conservatively.
            // A borrow whose KNOWN type owns no heap is copied by value into
            // the payload and lends nothing — `JBool(b) => JBool(b)` in a
            // declared `copy` read as a wrapped lend, which made the copy a
            // LENDER, the lending closure spread it through every
            // `copyJson`-forwarding reader (`fieldAt`, `elemAt`), and every
            // decoder's field snapshot was pinned Lent forever (exit-residue
            // rounds six and thirteen — round six refused this filter while
            // two real dangles behind it were still live; rounds seven and
            // ten fixed those, and the door now refuses heap-owning borrows
            // at constructor positions outright). An unknown type keeps the
            // lend, conservatively.
            // The filter is [`MoveCheck::arm_carries_heap`], not a bare
            // `type_of` probe (exit-residue round fifty-six): `Ok(s.byteLength)`
            // answers no type — a builtin scalar projection is not a record
            // field — and the old filter kept the lend, which made the whole
            // function a LENDER, pinned every caller's argument row Lent, and
            // stood the caller's own scrutinee release down with it. A scalar
            // projection carries no heap, so it lends nothing; unknown shapes
            // still keep the lend, conservatively.
            Expr::Call { name, args, .. } if self.decl.constructs(name) => args
                .iter()
                .find_map(|a| self.returned_borrow(a).filter(|_| self.arm_carries_heap(a))),
            Expr::StructLit { fields, .. } => fields
                .iter()
                .find_map(|(_, v)| self.returned_borrow(v).filter(|_| self.arm_carries_heap(v))),
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
                // A block arm (RFC-0118) is never a return value.
                let r = arm
                    .body
                    .as_expr()
                    .and_then(|e| self.lends_through_a_wrapper(e));
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
                );
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
                        // A NULLARY constructor parses as a bare name — `let
                        // mut head = None` is a construction, not an alias
                        // (round twenty-nine: the mislabel was invisible
                        // while `Option<Handle>` had no release kind, and
                        // then it stood every such binding's release down).
                        // The reading is [`MoveCheck::names_a_constructor`],
                        // which the `consume`-argument sites ask too.
                        if root == path && !self.names_a_constructor(&root) {
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
                            elem_only: None,
                            elem_name: None,
                        },
                    );
                }
                // Round fifty-six provenance: a binding initialized with a
                // carrying value carries it (read pre-bind, so `let x = x + b`
                // asks about the old `x`).
                if self.param_escapers.is_some() && self.carries_param_storage(value) {
                    self.carrying_locals.borrow_mut().insert(name.clone());
                }
                let is_borrow = borrow.is_some();
                self.bind(name, bty, borrow);
                if is_borrow {
                    let read = Self::read_of(value).map(|p| (p, *line));
                    self.reads.borrow_mut().bind(name, read);
                }
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
                // The write-back form of a rebuilding row: `xs = xs.push(v)`.
                // The receiver comes back through the result and the store
                // revives the binding, so its take is not recorded.
                let walked = self.walk_writeback(name, value, consumed, scope);
                walked?;
                // RFC-0114 M2: the write event, BEFORE the store's own effects
                // (`took(Borrowed)`, revive) so a reader sees the state the
                // store finds.
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
                self.store(value, &into, *line, global, consumed);
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
                    let read = b
                        .as_ref()
                        .and_then(|_| Self::read_of(value).map(|p| (p, *line)));
                    self.borrows.borrow_mut().rebind(name, b);
                    self.reads.borrow_mut().rebind(name, read);
                }
                // Round fifty-six: a store of a carrying value into module
                // state parks the storage where it outlives the call — the
                // enclosing function is an escaper; into a local, the local
                // carries.
                if self.param_escapers.is_some() && self.carries_param_storage(value) {
                    if global {
                        if let Some(sink) = &self.param_escapers {
                            sink.borrow_mut().insert(self.cur_fn.borrow().clone());
                        }
                    } else {
                        self.carrying_locals.borrow_mut().insert(name.clone());
                    }
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
                self.walk_writeback(&format!("{name}.{field}"), value, consumed, scope)?;
                self.store(
                    value,
                    &|| format!("the field `{name}.{field}`"),
                    *line,
                    true,
                    consumed,
                );
                // RFC-0093: a write fills the hole a take left. The same
                // sentence `Stmt::Assign` has carried since Phase 4b, one dot
                // down — and the reason no drop flag is needed to say it.
                revive(consumed, &format!("{name}.{field}"));
                self.wrote_into(name); // RFC-0093 M2: a filled hole is not skippable
                self.note_carrying_store(name, value);
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
                // Round fifty-seven: a store a `place atSet` PROJECTION
                // governs runs as the desugared statement group the checker
                // recorded (`project::stored`, the same leaked nodes the
                // lowering walks). The facts walk reads that group here, so
                // the element write-back inside it gets a plan row and the
                // displaced value a free — `strs[s] = tail` through
                // `std/slots` leaked every overwritten element (genref, and
                // the user-container half of §26's stand-aside). The CHECK
                // walk keeps the plain reading: its diagnostics name the
                // spelling the user wrote.
                if self.lets.is_some() && crate::project::memo_open() {
                    let aty = self.vars.borrow().get(name).cloned().flatten();
                    if let Some(aty) = aty {
                        if let Ok(Some(blk)) =
                            crate::project::store_index(self.impls, name, index, value, &aty)
                        {
                            if std::env::var_os("VYRN_PROJ_DUMP").is_some() {
                                eprintln!("proj-store walked: {name} line {line}");
                            }
                            self.block(blk, consumed, scope);
                            return Ok(false);
                        }
                    }
                }
                self.expr(index, consumed, scope)?;
                self.site("element", *line, value, None);
                self.expr(value, consumed, scope)?;
                self.store(value, &|| format!("`{name}`"), *line, true, consumed);
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
                self.store(index, &|| format!("`{name}`"), *line, true, consumed);
                self.wrote_into(name); // RFC-0093 M2: a filled hole is not skippable
                self.note_carrying_store(name, value);
                Ok(false)
            }
            Stmt::Return { value, line } => {
                if let Some(e) = value {
                    self.site("return", *line, e, None);
                    self.expr(e, consumed, scope)?;
                    // Round fifty-six: the escape screen's record, at the one
                    // site a result actually leaves — BEFORE `check_return`'s
                    // own refusals, so a shape the checker also refuses still
                    // marks the function while this walk's diagnostics are
                    // being collected rather than acted on.
                    if let Some(sink) = &self.param_escapers {
                        if self.carries_param_storage(e) {
                            sink.borrow_mut().insert(self.cur_fn.borrow().clone());
                        }
                    }
                    self.note_return(e, *line);
                    // Rule 3: the caller owns the result. Everything the returned
                    // expression reads may be inside it, so this block releases
                    // none of it. Only a heap return type can carry anything out.
                    if self.decl.owns_heap(&self.ret.borrow()) {
                        self.gave_up_returned(e, &Gone::Returned { line: *line });
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
                // AFTER the value walk, matching the runtime: the returned
                // expression evaluates first, and only then does the exit
                // release anything. Recorded first, a `return Parser { src:
                // ba, .. }` read as "an exit before `ba`'s take" and round
                // twenty-one's fold freed `ba` at the very return that embeds
                // it (parity's audit caught it on `{\"a\":1}`).
                self.exit_site(s as *const Stmt as usize, false);
                self.exit_ev();
                Ok(true)
            }
            // `break`/`continue` (RFC-0060) consume nothing but terminate the
            // path — code after them in the same block is unreachable.
            Stmt::Break { .. } => {
                self.exit_ev();
                Ok(true)
            }
            // A `continue` also diverges here, but it jumps to the NEXT
            // iteration: the loop body re-runs. Marking it is what stops the
            // loop arms from counting it as the "runs at most once"
            // divergence that skips the next-iteration reuse check.
            Stmt::Continue { .. } => {
                self.exit_ev();
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
                self.enter_branch();
                let then_div = self.block(then_block, &mut then_c, scope);
                self.leave_branch();
                let mut else_c = consumed.clone();
                self.enter_branch();
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
                    0 if self.names_a_place(scrutinee).is_none()
                        && !self.is_bound_name(scrutinee) =>
                    {
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
                self.enter_branch();
                let then_div = self.block(then_block, &mut then_c, scope);
                self.leave_branch();
                self.arm_binders.borrow_mut().pop();
                self.exit();
                scope.pop();
                let mut else_c = consumed.clone();
                self.enter_branch();
                let else_div = match else_block {
                    Some(eb) => self.block(eb, &mut else_c, scope),
                    None => false,
                };
                self.leave_branch();
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
                let _ = self.block(body, &mut body_c, scope);
                self.continue_seen.set(outer_continue);
                self.loop_ids.borrow_mut().pop();
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
                    0 if !*consuming
                        && self.names_a_place(iter).is_none()
                        && !self.is_bound_name(iter) =>
                    {
                        self.note_temporary(s, iter)
                    }
                    _ if *consuming => self.note_temporary(s, iter),
                    _ => 0,
                };
                let mut body_c = consumed.clone();
                self.enter();
                let is_borrow = borrow.is_some();
                self.bind(var, elem, borrow);
                if is_borrow {
                    let read = place_path(iter).map(|(_, p)| (format!("{p}[..]"), *line));
                    self.reads.borrow_mut().bind(var, read);
                }
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
                    // Round sixteen: name the loop variable on the row, so
                    // `took`/`hole` can tell an element departure — a take
                    // THROUGH this name — from any other writer.
                    if let Some(sink) = &self.lets {
                        if let Some(row) = sink.borrow_mut().get_mut(&key) {
                            row.elem_name = Some(var.clone());
                        }
                    }
                }
                let outer_continue = self.continue_seen.replace(false);
                let _ = self.block(body, &mut body_c, scope);
                self.exit();
                // The loop variable is fresh on every iteration, so a move of it
                // is not a move of anything the enclosing scope can still name.
                body_c.remove(var);
                self.loop_ids.borrow_mut().pop(); // RFC-0114 M2: for-loop extent ends
                self.continue_seen.set(outer_continue);
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
            Stmt::Expr(e) => {
                // Round twenty-eight was recorded here: a
                // statement-position call whose OWNED heap result nothing
                // binds. The core states it as a `St::Drop` at the
                // `Stmt::Expr`, and the emitter reads the core alone.
                self.expr(e, consumed, scope)
                    .map(|_| matches!(e, Expr::Call { name, .. } if crate::ast::is_panic(name)))
            }
            // A `region` is an ordinary nested block for move checking; it
            // diverges iff its body does (a `break` inside it exits the loop).
            // Its map is a CLONE, like every other nested block's. Sharing it
            // by `&mut` let a shadowing `let` inside the region run `revive`
            // on the ENCLOSING map and erase the outer binding's consumption
            // record — a use-after-move after the region then compiled.
            Stmt::Region { body, .. } => {
                let mut inner = consumed.clone();
                self.walk_region.set(self.walk_region.get() + 1);
                let div = self.block(body, &mut inner, scope);
                self.walk_region.set(self.walk_region.get() - 1);
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
                // Two of this statement's three refusals have LEFT (RFC-0125
                // §3 M3, rows 20 and 21): a `drop` of what a take already took,
                // and a `drop` of a borrow. The kernel states both, in these
                // words and at this line, and the accumulation driver puts them
                // beside whatever else the file earns — which is what the two
                // rows waited on, because the one program of the corpus that
                // breaks either of them, `examples/mustuse_abandoned.vyrn`,
                // breaks a must-use obligation as well.
                //
                // A PARTIAL take left a hole in the binding (RFC-0093), and the
                // kernel accepts that program. The taken place belongs to
                // whoever received it, and `drop`
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
                    Consumption {
                        line: *line,
                        hole: false,
                    },
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
            self.took(
                name,
                Gone::Captured {
                    line,
                    spawned: false,
                },
            );
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
                if i != j && mentions(b, &root) {
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

    fn expr(
        &self,
        e: &Expr,
        consumed: &mut Consumed,
        scope: &mut Vec<HashSet<String>>,
    ) -> Result<(), Diagnostic> {
        match e {
            Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => Ok(()),
            Expr::Var { name, line } => {
                self.mention_ev(name);
                self.capture_site(name, *line);
                self.check_capture(name, *line)?;
                self.note_capture(name, *line);
                Ok(())
            }
            Expr::Unary { expr, .. } => self.expr(expr, consumed, scope),
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs, consumed, scope)?;
                // An operand of a String `+`, of a String comparison and of
                // `=~` is a call argument — `@concat`'s — and the core states
                // its release row from the lowered operator (RFC-0125 §3 M3,
                // the last table's slice). This pass recorded the same three
                // shapes until then.
                self.expr(rhs, consumed, scope)
            }
            // A place chain asks ONE consumption question, of the whole path.
            // Walking into the root instead would ask it of `er` and refuse
            // `er.next` after `consume er.node`, which is the case RFC-0093
            // exists to allow. The root still gets its capture bookkeeping.
            Expr::Field { expr, .. } => match place_path(e) {
                Some(_) => {
                    let (root, rline) = root_var(e);
                    self.mention_ev(root);
                    self.capture_site(root, rline);
                    self.check_capture(root, rline)?;
                    self.note_capture(root, rline);
                    Ok(())
                }
                None => {
                    // RFC-0114 R1′ was recorded here: a receiver with no
                    // name — `.byteLength` on a String temporary, `.length`
                    // on a container one, a record field of one. The core
                    // states that row on the name itself now
                    // (`NameInfo::receiver`), and the emitter reads the core
                    // alone, so this walk records nothing for it
                    // (RFC-0125 §3 M3, the emitter-reads-the-core-alone
                    // slice).
                    self.expr(expr, consumed, scope)
                }
            },
            // RFC-0093 — the take. The record below is what makes a later read
            // of this place, or of anything overlapping it, a rule-1 error.
            Expr::Consume { place, line } => {
                self.expr(place, consumed, scope)?;
                // A `consume` of what names no place is the desugar's refusal
                // now (rows 08 and 09), so this walk records nothing for it.
                let Some((root, path)) = place_path(place) else {
                    return Ok(());
                };
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
                        hole: root != path,
                    },
                );
                Ok(())
            }
            Expr::Try { expr, .. } => {
                let r = self.expr(expr, consumed, scope);
                // `?` is a `return` in everything but the spelling — the
                // escape screen records here too (round fifty-six): the err
                // payload it hands the caller is a projection of the operand.
                if let Some(sink) = &self.param_escapers {
                    if self.carries_param_storage(expr) {
                        sink.borrow_mut().insert(self.cur_fn.borrow().clone());
                    }
                }
                self.exit_site(e as *const Expr as usize, true);
                self.exit_ev();
                r
            }
            // A literal's operands are places too: `Ring { slots: xs }` puts `xs`
            // where the record owns it, exactly as an argument does.
            Expr::StructLit { name, fields, line } => {
                // A literal RETAINS what it is given, so a lambda inside one
                // escapes whatever the enclosing call would have done with it.
                let outer = self.call_keeps.replace(Some(true));
                for (f, v) in fields {
                    self.site("literal", *line, v, None);
                    self.expr(v, consumed, scope)?;
                    self.store(
                        v,
                        &|| format!("the field `{name}.{f}`"),
                        *line,
                        true,
                        consumed,
                    );
                }
                self.call_keeps.set(outer);
                Ok(())
            }
            Expr::TryConstruct { name, args, line } => {
                let outer = self.call_keeps.replace(Some(true));
                for a in args {
                    self.site("literal", *line, a, None);
                    self.expr(a, consumed, scope)?;
                    self.store(a, &|| format!("`{name}`"), *line, true, consumed);
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
                    0 if self.names_a_place(scrutinee).is_none()
                        && !self.is_bound_name(scrutinee) =>
                    {
                        self.note_temporary_at(e as *const Expr as usize, scrutinee)
                    }
                    k => k,
                };
                // Round forty: whether this is the temp row minted just above
                // — the per-arm payload accounting below acts only on those.
                let minted_temp = key == e as *const Expr as usize && self.lets.is_some();
                let pre_gone = if minted_temp {
                    self.lets
                        .as_ref()
                        .and_then(|s| s.borrow().get(&key).and_then(|r| r.gone.clone()))
                } else {
                    None
                };
                let mut any_moved = false;
                let mut moved_gone: Option<Gone> = None;
                let base = consumed.clone();
                let mut arm_cs: Vec<Consumed> = Vec::new();
                for arm in arms {
                    let mut c = base.clone();
                    // Round forty: each arm is an ALTERNATIVE — a take through
                    // one arm's binder must not read as this arm's, so the
                    // temp row's verdict is reset per arm and the worst one is
                    // written back after the loop. Reset FIRST, before
                    // `note_arm_value` below writes an alias verdict this
                    // arm's accounting must see (resetting after it erased
                    // the mark, the whole-release fired on an aliased-out
                    // payload, and `ascii`'s caller freed a returned buffer
                    // twice — the audit caught it before anything shipped).
                    if minted_temp {
                        if let Some(s) = &self.lets {
                            if let Some(row) = s.borrow_mut().get_mut(&key) {
                                row.gone = pre_gone.clone();
                            }
                        }
                    }
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
                    // Asked HERE, one scope in, because the question is about
                    // the binders and this is where they are bound. A block arm
                    // (RFC-0118) yields nothing, so there is no arm value.
                    if let ArmBody::Expr(body) = &arm.body {
                        self.note_arm_value(body, *line, &binders);
                    }
                    self.arm_binders
                        .borrow_mut()
                        .push(binders.iter().map(|b| b.to_string()).collect());
                    self.enter_branch();
                    let r = match &arm.body {
                        ArmBody::Expr(body) => self.expr(body, &mut c, scope),
                        // The statements walk as statements, inside the same
                        // binder scope and branch stamp an expression arm gets.
                        ArmBody::Block(b) => {
                            self.block(b, &mut c, scope);
                            Ok(())
                        }
                    };
                    self.leave_branch();
                    self.arm_binders.borrow_mut().pop();
                    // Round forty's other half: which arm MOVED the temp
                    // scrutinee, which is what the row below writes back.
                    // Which binders an arm still holds at its end is the
                    // kernel's answer since RFC-0125 §3 M3's derivation
                    // slice, and the screens this walk kept for it — one
                    // binder, a silent release, an arm value that cannot
                    // alias — went with the table they fed.
                    if minted_temp {
                        let now = self
                            .lets
                            .as_ref()
                            .and_then(|s| s.borrow().get(&key).and_then(|r| r.gone.clone()));
                        let moved_here = now.is_some() != pre_gone.is_some();
                        if moved_here {
                            any_moved = true;
                            if moved_gone.is_none() {
                                moved_gone = now;
                            }
                        }
                    }
                    self.exit();
                    r?;
                    scope.pop();
                    arm_cs.push(c);
                }
                if minted_temp {
                    if let Some(s) = &self.lets {
                        if let Some(row) = s.borrow_mut().get_mut(&key) {
                            row.gone = if any_moved {
                                moved_gone.clone()
                            } else {
                                pre_gone.clone()
                            };
                        }
                    }
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
                self.enter_branch();
                let r = self.expr(then_branch, &mut then_c, scope);
                self.leave_branch();
                r?;
                let mut else_c = base.clone();
                if let Some(eb) = else_branch {
                    self.enter_branch();
                    let r = self.expr(eb, &mut else_c, scope);
                    self.leave_branch();
                    r?;
                }
                // RFC-0114 Rule N at an `if`-expression join — the statement
                // rule with the match's value guard: the releasing branch's
                // value must not alias the binding (no heap, no mention, or a
                // Binary/Unary body, whose result is a scalar or fresh). No
                // binders and no scrutinee here, so those guards do not apply;
                // the condition is a Bool read completed before the branch.
                for (k, v) in then_c.into_iter().chain(else_c) {
                    consumed.or_insert(k, v);
                }
                Ok(())
            }
            Expr::Call { name, args, line } => {
                self.check_exclusive(name, args, *line)?;
                let caps = self.caps.get(name);
                // Round eighteen's soundness screen, re-anchored in round
                // fifty-six: the question is whether the enclosing function's
                // RESULT can hand back a borrowed parameter's storage, and
                // recording every interior carrying call answered a different
                // question — `replace`'s `slice(s, ..)` operand is copied by
                // the `+` that consumes it, yet the record made `replace` an
                // escaper and stood down every `out = replace(out, ..)` store
                // in the corpus (`httpFill` leaked one filled template per
                // request, `regexredux` one sequence buffer per pattern). The
                // record now happens where storage actually leaves — a
                // `return`, a `?`, a store into module state or a `modify`
                // parameter — with `carrying_locals` carrying the provenance
                // through `let r = a.push(v); return r`.
                // Left-to-right: check each argument, then apply its consumption,
                // so passing the same variable to two `consume` params is caught.
                for (i, arg) in args.iter().enumerate() {
                    // Round fifty-five: a lambda in an argument position
                    // whose declared parameter is a fn type has a KNOWN
                    // signature — recorded before the walk meets the lambda,
                    // so the arity poison stands down for it and the meet
                    // refuses only its own signature.
                    if matches!(arg, Expr::Lambda { .. }) {
                        if let (Some(tl), Some(ls)) = (&self.typed_lambdas, &self.lambda_sigs) {
                            if let Some(pt) = self.decl.param_ty(name, i) {
                                if let Type::Fn(ps, r) =
                                    crate::types::resolve(pt, self.decl.decls())
                                {
                                    tl.borrow_mut().insert(arg as *const Expr as usize);
                                    ls.borrow_mut()
                                        .insert(fn_sig_key(&ps, &r, self.decl.decls()));
                                }
                            }
                        }
                    }
                    self.site("arg", *line, arg, None);
                    self.call_keeps.set(Some(self.callee_keeps(name, i)));
                    let r = self.expr(arg, consumed, scope);
                    self.call_keeps.set(None);
                    r?;
                    self.note_handover(arg, name, i, *line);
                    if caps.and_then(|c| c.get(i)) == Some(&Capability::Consume) {
                        // A NULLARY constructor is a value with no owner, not a
                        // name (RFC-0126 §8.8): `take(None)` twice hands the
                        // callee two values, and reading the second as a use of
                        // the first refused a program every engine runs.
                        if let Expr::Var { name: v, .. } = arg {
                            if !self.names_a_constructor(v) {
                                self.took(
                                    v,
                                    Gone::Moved {
                                        line: *line,
                                        by: format!(
                                            "`{}(..)`",
                                            crate::parser::method_surface(name)
                                        ),
                                    },
                                );
                                consumed.or_insert(
                                    v.clone(),
                                    Consumption {
                                        line: *line,
                                        hole: false,
                                    },
                                );
                            }
                        }
                    } else if self.decl.constructs(name) {
                        // Round fifty-six: only a value that can CARRY heap is
                        // retained. `Ok(s.byteLength)` copies a scalar out of
                        // the parameter — recording it made the whole function
                        // a retainer of its argument, pinned every caller's
                        // binding Lent, and stood the callers' releases down
                        // (`return match decode(arg.json) { .. }` leaked `arg`
                        // at every call). The screen is the one
                        // `arm_carries_heap` already states: a typed value
                        // answers `owns_heap`, an untyped field read on a
                        // non-record base is a builtin scalar projection, and
                        // anything else stays retained, conservatively.
                        if self.arm_carries_heap(arg) {
                            self.note_retention(arg);
                        }
                        // A variant constructor is a literal that reads like a
                        // call: the value it builds holds the argument and
                        // outlives the call, exactly as an array literal does.
                        // A whole OWNED name moves in; a PROJECTION or a
                        // borrowed name is REFUSED (exit-residue rounds seven
                        // and ten): the constructor position was the one door
                        // a borrow could smuggle through into a value the
                        // machinery releases as owned — `JStr(sel.key)` freed
                        // the selection key under `q.sels`, and admitting
                        // constructor-built argument temporaries at all
                        // requires the door closed. The store rule's `.copy()`
                        // menu applies, exactly as it does everywhere else.
                        // A `consume` take is a TRANSFER — the hole machinery
                        // accounts the field, and the constructed value owns
                        // what it took — so only a bare projection or borrow
                        // is refused.
                        let taken = matches!(arg, Expr::Consume { .. });
                        if let Some((root, path)) = place_path(arg) {
                            let borrowed = self.borrow_of(&root).is_some();
                            // A borrow put into a constructor is the kernel's
                            // refusal now (RFC-0125 §3 M3, row 19). The
                            // retention row is still this pass's: the value the
                            // constructor makes holds the argument and outlives
                            // the call.
                            if !taken
                                && (path != root || borrowed)
                                && self.type_of(arg).is_some_and(|t| self.decl.owns_heap(&t))
                            {
                                self.note_retention(arg);
                            } else if root == path {
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
                                // Round thirty-three: the move enters the
                                // consumption map, exactly as `store()`'s
                                // does — rule 1 refuses reuse, and Rule N's
                                // recorder can finally SEE a constructor take
                                // on one branch (`ops.push(Set(newAttrs))`
                                // under an `if` leaked the untaken path's
                                // value, std/html's whole differ). A scalar
                                // copies, exactly as everywhere else.
                                // A compiler-synthesized `@`-name is its
                                // template's business, not rule 1's.
                                if root.starts_with('@')
                                    || !self.type_of(arg).is_some_and(|t| self.decl.owns_heap(&t))
                                {
                                    continue;
                                }
                                consumed.or_insert(
                                    root.clone(),
                                    Consumption {
                                        line: *line,
                                        hole: false,
                                    },
                                );
                            }
                        }
                    } else if self.sinks(name, i)
                        && i == 0
                        && store_path(arg).as_deref() == self.writeback.borrow().as_deref()
                    {
                        // The receiver of a write-back statement (`xs = xs.push(v)`,
                        // `s.dense.push(i)`): the call takes the buffer and hands
                        // it back through the result, into the same place, so
                        // rule 1 has nothing to record. Whether the receiver is
                        // a borrow — `let mut mt = h.meta` then `mt.push(x)`
                        // rebuilds a buffer `h.meta` still owns — is the
                        // KERNEL's question now (RFC-0125 §3 M3, row 26): it
                        // asks it of the value, at the `let` where the borrow
                        // was read, with the same menu.
                    } else if self.sinks(name, i) {
                        // A builtin whose parameter declares `consume`. Rule 1
                        // governs it exactly as it governs `xs = [.., v]`, which
                        // is what it means.
                        self.store(
                            arg,
                            &|| format!("`{}(..)`", crate::parser::method_surface(name)),
                            *line,
                            true,
                            consumed,
                        );
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
                    self.store(
                        e,
                        &|| "the array literal".to_string(),
                        *line,
                        true,
                        consumed,
                    );
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
                    self.store(k, &|| "the map literal".to_string(), *line, true, consumed);
                    self.store(v, &|| "the map literal".to_string(), *line, true, consumed);
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
                if let Some(la) = &self.lambda_arities {
                    let typed = self
                        .typed_lambdas
                        .as_ref()
                        .is_some_and(|t| t.borrow().contains(&(e as *const Expr as usize)));
                    if !typed {
                        la.borrow_mut().insert(params.len());
                    }
                }
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
                // Rule 3 reaches closures (exit-residue round nine): a
                // lambda's result is its CALLER's, and a captured heap value
                // returned raw hands out storage the capture block still owns
                // — the emitted body is `ret ptr %cap`, no copy, so the first
                // caller to release its result frees the block's buffer and
                // the next call reads it freed. The kernel states it, from
                // the capture the core marks (`core::BorrowKind::Capture`) and
                // in these same words (RFC-0125 §3 M3, row 28).
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
                    self.gave_up(
                        arg,
                        &Gone::Captured {
                            line: *line,
                            spawned: true,
                        },
                    );
                    if caps.and_then(|c| c.get(i)) == Some(&Capability::Consume) {
                        // A nullary constructor is not a name — see the
                        // ordinary call above (RFC-0126 §8.8).
                        if let Expr::Var { name: v, .. } = arg {
                            if !self.names_a_constructor(v) {
                                consumed.or_insert(
                                    v.clone(),
                                    Consumption {
                                        line: *line,
                                        hole: false,
                                    },
                                );
                            }
                        }
                    }
                }
                Ok(())
            }
        }
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
            // A block arm (RFC-0118) answers `true` without being read — the
            // lambda-block precedent above, for the same reason: `true` costs
            // a leak and `false` can cost a use-after-free.
            Expr::Match {
                scrutinee, arms, ..
            } => {
                go(scrutinee, base)
                    || arms
                        .iter()
                        .any(|a| a.body.as_expr().is_none_or(|e| go(e, base)))
            }
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

// ---------------------------------------------------------------------------
// The AST predicates the must-use judgment reads, and `check_exclusive` with
// it (RFC-0125 §3 M3, the obligation slice).
//
// They were `mod linear`'s, and they are not the must-use RULE: they answer
// what an expression NAMES and which of its paths name it, which is a question
// about the tree. The rule moved to `vyrn_lower::typed::obligation`, and these
// stayed because a pass below the lowering asks them too.
// ---------------------------------------------------------------------------

/// The nested blocks of a statement, for the declaration walk.
pub fn sub_blocks(s: &Stmt) -> Vec<&Block> {
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
pub fn stmt_mentions(s: &Stmt, name: &str) -> bool {
    let here = match s {
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::SetField { value, .. }
        | Stmt::Expr(value) => mentions(value, name),
        Stmt::IndexSet { index, value, .. } => mentions(index, name) || mentions(value, name),
        Stmt::If { cond: e, .. }
        | Stmt::While { cond: e, .. }
        | Stmt::IfLet { scrutinee: e, .. } => mentions(e, name),
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
pub fn mentions(e: &Expr, name: &str) -> bool {
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
/// This is RFC-0095 M3. the must-use walk read a statement's expressions with
/// [`mentions`] alone, which answers "some path", and then treated the answer
/// as a disposal on every path — so `match p { Some(n) => t.join(), None => 0 }`
/// discharged a task the `None` path abandons. The `if` STATEMENT never had
/// the hole: `scan` walks its two blocks and merges them. The merge is
/// unchanged; what changed is that a branching EXPRESSION now reaches it.
pub fn paths(e: &Expr, name: &str) -> (bool, bool) {
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
            // A block arm (RFC-0118) exists only in statement position,
            // which is never an operand this hoisting question is asked
            // about; if one is ever met, (true, false) is conservative in
            // both directions.
            let any = arms
                .iter()
                .any(|a| a.body.as_expr().is_none_or(|e| paths(e, name).0));
            let every = arms
                .iter()
                .all(|a| a.body.as_expr().is_some_and(|e| paths(e, name).1));
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
/// The place an expression names, spelled as the store arms spell it:
/// `xs`, `s.keys`, `a.b.c`. `None` for anything that is not a place.
fn store_path(e: &Expr) -> Option<String> {
    match e {
        Expr::Var { name, .. } => Some(name.clone()),
        Expr::Field { expr, field, .. } => Some(format!("{}.{field}", store_path(expr)?)),
        _ => None,
    }
}

fn sinks(decl: &Declared, name: &str, i: usize) -> bool {
    let Some(f) = crate::prelude::signature(name) else {
        return false;
    };
    let Some(p) = f.params.get(i) else {
        return false;
    };
    if decl.linear_kind(&p.ty).is_some() {
        return false;
    }
    if p.capability == Capability::Consume {
        return true;
    }
    // RFC-0125 M2, the first defect the kernel found: a row that hands its
    // receiver's buffer back as its result — `push`, `reserve`, `append`,
    // `copyFrom`, a map's `tally` — TAKES the receiver. The statement form
    // `xs = xs.push(v)` takes and revives in one line, which the untake fold
    // already reads as reclaimed; the expression form `return xs.push(v)`
    // used to leave `xs` reclaimed at the return while the result carried
    // its buffer out, and the caller received freed memory
    // (`rfcs/probes-0125/push-in-expression-position.vyrn`).
    i == 0 && crate::prelude::rebuilds(name)
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
                    // A block arm (RFC-0118) is never part of a return or a
                    // spawn argument — statement position only, the checker's
                    // rule — so there is nothing here to carry out.
                    if let ArmBody::Expr(e) = &a.body {
                        go(e, out);
                    }
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
                // A block arm (RFC-0118) is never a return value, so it can
                // forward no lender.
                if let ArmBody::Expr(e) = &a.body {
                    calls_in(e, out);
                }
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
pub fn element_path(e: &Expr) -> Option<(String, String)> {
    match e {
        Expr::Call { name, args, .. } if projection_call(name) => {
            let a = args.first()?;
            let (root, path) = place_path(a).or_else(|| element_path(a))?;
            // A named projection quotes as the call the reader wrote; `@at`
            // keeps the index spelling `xs[i]` it has always had.
            if name == crate::project::AT {
                Some((root, format!("{path}[{}]", index_text(args.get(1)))))
            } else {
                Some((root, format!("{path}.{name}(..)")))
            }
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

pub fn place_path(e: &Expr) -> Option<(String, String)> {
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
        Pattern::Success(b) | Pattern::Failure(b) => vec![b],
        Pattern::Variant(_, binds) => binds.iter().map(|s| s.as_str()).collect(),
        Pattern::Other => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// The suite's own corpus, and the instrument that measures the deletion
    /// licence with it (RFC-0125 §3 M3). These tests are three times the
    /// census and they are the only corpus of programs the checker REFUSES:
    /// `examples/`, `std/` and `site/` all compile, so they say nothing about
    /// a refusal. `VYRN_DUMP_MOVECHECK=<dir> cargo test -p vyrn-frontend
    /// movecheck` writes every program checked here to that directory,
    /// `no_*` for them, and each through `VYRN_NO_MOVECHECK=1 vyrn check` is
    /// the licence: a program the checker refuses and the kernel accepts is a
    /// rule that may not leave this file.
    ///
    /// Every program asked here is refused, because [`super::refusal`] is the
    /// only door and it panics on silence (RFC-0125 §3 M3, the safety slice).
    /// The programs these tests read as ACCEPTED are asked of the whole
    /// compiler instead, in `compiler/vyrn-cli/tests/refusals.rs`.
    fn run(src: &str) -> String {
        let program = crate::parser::parse(crate::lexer::lex(src).unwrap()).unwrap();
        record(src);
        super::refusal(&program)
    }

    /// One program of the corpus, written out.
    fn record(src: &str) {
        let Ok(dir) = std::env::var("VYRN_DUMP_MOVECHECK") else {
            return;
        };
        let _ = std::fs::create_dir_all(&dir);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(src, &mut h);
        let n = std::hash::Hasher::finish(&h);
        let _ = std::fs::write(format!("{dir}/no_{n:016x}.vyrn"), src);
    }

    /// The guard itself: a program this pass says nothing about has no answer
    /// here to assert on (RFC-0125 §3 M3, the safety slice).
    #[test]
    #[should_panic(expected = "silence is not acceptance")]
    fn this_pass_has_no_acceptance_answer() {
        run("fn main() -> Int64 { return 0 }");
    }

    /// What `views` and `sinks` answer, now that both read a signature.
    ///
    /// The lists they read are gone (RFC-0094 M1) and the reservation check
    /// moved to `prelude` with the rows. What is pinned here is the READING: a
    /// row's `consume` must reach `sinks` and a row's projection body must reach
    /// `views`, or the passes are back to knowing nothing.
    #[test]
    fn the_passes_read_the_seeded_capabilities() {
        assert!(views(crate::project::AT));
        // `bytes` left the views set when the exit-residue census caught its
        // row lying: every engine copies, so the result is owned.
        assert!(!views("bytes"), "a copy is not a view");
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
        // RFC-0125 M2: the receiver is handed back through the result, so
        // it is taken — and revived by the statement form's write-back.
        assert!(
            sinks(&decl, "@push", 0),
            "a rebuilding row takes its receiver"
        );
        assert!(!sinks(&decl, "@at", 0), "a lookup takes nothing");
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

    /// RFC-0121: the refutable `let`'s binding is the PAYLOAD of the
    /// scrutinee's place — a borrow, never a fresh owner. When the scrutinee
    /// is module state, minting an owned row freed the global's buffer at
    /// block exit, out from under the next read (`vyrn why --memory` said
    /// "reclaimed at block exit — freeing the array buffer"; the loop
    /// crashed at iteration two).
    #[test]
    fn a_payload_binding_from_module_state_gets_no_reclamation_row() {
        let src = "type E = | Tag(Array<Int64>) | Blank\n\
                   let mut g: E = Blank\n\
                   fn main() -> Int64 {\n\
                       let Tag(xs) = g\n\
                       return xs.length\n\
                   }";
        let program = crate::parser::parse(crate::lexer::lex(src).unwrap()).unwrap();
        let rows = super::ownership(&program);
        let payload = rows
            .values()
            .find(|r| matches!(&r.ty, Some(crate::ast::Type::Array(_))))
            .expect("the binding has a row");
        assert!(
            payload.gone.is_some(),
            "the payload binding must be marked borrowed, not owned: {payload:?}"
        );
    }

    #[test]
    fn rejects_drop_after_a_partial_take() {
        // F2-049: the taken field belongs to whoever received it, and `drop`
        // reclaims storage by TYPE — freeing the whole binding here frees
        // what the receiver still holds.
        let src = "type T = { id: Int64, name: String }; \
                   fn main() -> Int64 { let t = T { id: 1, name: \"n\" }; \
                                      consume t.name; drop t; return 0; }";
        let e = run(src);
        assert!(e.contains("may not be dropped"), "{e}");
    }

    // ---- RFC-0089 Phase 4b: rules 1 and 3 --------------------------------

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

    // ---- RFC-0093: the take ---------------------------------------------

    // ---- rule 2 at the third exit: a borrow may not be consumed -----------

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
        );
        assert!(
            e.contains("may not be captured by a closure that outlives this call"),
            "{e}"
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
        assert!(
            super::ownership(&program).values().all(|r| {
                let fc = r.from_call.as_deref();
                (fc != Some("g") && fc != Some("h")) || r.gone.is_some()
            }),
            "a lender forwarded through an aggregate must never be reclaimed by its caller"
        );
    }

    #[test]
    fn a_modify_borrow_is_exclusive() {
        let src = "fn f(a: modify Array<Int64>, b: Array<Int64>) -> Int64 { return a.length } \
                   fn main() -> Int64 { let mut xs: Array<Int64> = [] return f(xs, xs) }";
        let e = run(src);
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
        let e = run(src);
        assert!(
            e.contains("as `modify` and read again in the same call"),
            "{e}"
        );
    }

    #[test]
    fn an_escaping_closure_may_not_capture_a_borrow() {
        // A lambda that is stored, or handed to a `consume fn` parameter that
        // may keep it, is a value and may not capture a borrow.
        let src = "fn go(s: String) -> Int64 { let f = n -> n + s.byteLength return f(1) } \
                   fn main() -> Int64 { return 0 }";
        let e = run(src);
        assert!(
            e.contains("may not be captured by a closure that outlives this call"),
            "{e}"
        );
    }

    // ---- RFC-0075: what a stream producer TAKES --------------------------
    //
    // The obligation itself left this file with the rule (RFC-0125 §3 M3, the
    // obligation slice): it is a TYPE's, and it is stated in the typed
    // judgment. What is left here is rule 1's question about the same
    // programs — what a producer takes, and what a combinator does to the
    // ownership of what it is handed — which is this file's.

    /// The producer every stream case below acquires from, and a consumer that
    /// discharges one — a call, so it fits in an expression position.
    const FEED: &str = "fn feed() -> Stream<Int64> { let xs: Array<Int64> = [1, 2] \
                        return fromArray(xs) } \
                        fn drain(s: Stream<Int64>) -> Int64 { let mut t = 0 \
                        for v in s { t = t + v } return t } ";

    /// A combinator, spelled locally rather than imported: nothing in the
    /// compiler knows about std/stream, and the point is that nothing has to.
    const TWICE: &str = "fn twice(s: Stream<Int64>) -> Stream<Int64> { \
                         let mut out: Array<Int64> = [] for x in s { out.push(x * 2) } \
                         return fromArray(out) } ";

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

        // Reading the name afterwards is row 07's error, in the kernel's words
        // since the row left: `tests/refusals.rs` asks it of the whole compiler.

        // A `read` parameter's buffer may not go into a stream: the caller
        // still owns it, and the stream's close would free it. That refusal is
        // the kernel's since RFC-0125 §3 M3, rows 01, 02, 03, 27 and 34, and
        // `tests/refusals.rs` asks it of the whole compiler.
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
            fn list_kinds(&self, resolved: &str) -> Result<Vec<String>, String> {
                let mut names: Vec<String> = std::fs::read_dir(resolved)
                    .map_err(|e| e.to_string())?
                    .filter_map(|e| e.ok())
                    .map(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        if e.file_type().is_ok_and(|t| t.is_dir()) {
                            format!("{name}/")
                        } else {
                            name
                        }
                    })
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
}
