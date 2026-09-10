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
//! `owns_heap` at every binding, argument, return, store, iterable and capture.
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
    /// Round twenty: the callee is a VIEW whose result for this argument is a
    /// copy — the element type owns no heap, so the scalar the view hands out
    /// cannot alias the temporary. `bytes(l)[0]` in a line loop was one
    /// unfreed `bytes` buffer per line; with this the row is Released like
    /// any read argument instead of standing down as Lent.
    pub view_copies: bool,
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
    /// The callee keeps it — a variant constructor. A leak, and not this
    /// rule's.
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

/// Everything one `Want::Lets` walk answers.
pub struct Facts {
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
    /// ([`fn_sig_key`]).
    pub fnval_clear: HashSet<String>,
}

/// The closure over the call graph, out of one walk.
pub fn facts(program: &Program) -> Facts {
    let r = run(program, Want::Lets);
    Facts {
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
    Lets,
    /// RFC-0092 M0: record every projection the rule would refuse, refuse none.
    Projections,
}

/// One run's outputs.
struct Run {
    projections: Vec<ProjectionSite>,
    fnval_clear: HashSet<String>,
    /// The three things a [`Want::Lets`] run produces, for [`lets_outputs`].
    arities: HashSet<usize>,
    sigs: HashSet<String>,
    stores: Vec<String>,
}

/// Everything one [`Want::Lets`] walk produces, one sorted line per row.
///
/// Three kinds of row and no fourth: the arity of a lambda whose signature the
/// declaration does not name, the signature key of one it does, and the
/// projection store whose desugared group the walk descends into instead of the
/// index and the value. READER: `compiler/vyrn-cli/tests/letswalk.rs`, which
/// prints these over the corpus so the walk can be rewritten against them
/// (RFC-0125 Section 3 M3).
pub fn lets_outputs(program: &Program) -> Vec<String> {
    let r = run(program, Want::Lets);
    let mut out: Vec<String> = r.arities.iter().map(|n| format!("arity {n}")).collect();
    out.extend(r.sigs.iter().map(|k| format!("sig {k}")));
    out.extend(r.stores);
    out.sort();
    out.dedup();
    out
}

/// Whether the producer of an argument HANDS ITS ARGUMENT BACK — `blackBox`,
/// whose seeded row returns the same bare type parameter one of its own
/// parameters has. The result IS the argument, so no temporary stands here.
///
/// Public for the same reason [`crate::declared::arg_caps`] is: the core
/// screens the same producer at the same position.
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
/// Every clause is a rule that already shipped, read at a position instead of
/// at a binding: `constructs` is `59c8a0c`'s recorded exit, `views` is the
/// seeded row's own shape, and the capability is the parameter's own
/// declaration. Rules 2 and 3 are what make `read` mean "keeps nothing": a
/// borrow may not be stored and may not be returned, and `59c8a0c` closed the
/// hand-over exit.
///
/// **No call-graph set is asked here.** Two of them used to be — the
/// functions whose result the caller must not release, and the positions
/// that keep a borrowed parameter they are handed. Both were empty over
/// every corpus program and over every witness the deletion slice could
/// build, because the kernel refuses the shapes that fill them at the
/// constructor and field doors (RFC-0125 §3 M3, the wrapped lend).
pub fn arg_verdict(s: &ArgTemp, constructs: bool, cap: Option<Capability>) -> ArgVerdict {
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
    // A view LENDS — its result names a place inside this argument — except
    // where the element it hands out is a heap-free copy (round twenty): a
    // scalar read through `bytes(l)[0]` keeps no pointer into the buffer, so
    // the temporary is the caller's to free like any read argument.
    if views(&s.callee) && !s.view_copies {
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

/// **The order a file's refusals come out in is the source's** (RFC-0125 §3
/// M3, the corpus slice). This pass walked top-level functions before `impl`
/// methods and the placer walks bodies in the lowering's order, so the same
/// two sentences came out swapped and the whole standard error moved even
/// where every sentence was identical. Neither walk order is a rule anybody
/// wrote down; the source's is, and it is the only one a reader can predict.
/// So [`refusals`] sorts by line before it prints, and the other statement of
/// the same rule is `vyrn-cli`'s `kernel_refuses`. Files keep the order they
/// were first named in — a module's refusals stay together — and two on one
/// line keep the walk's order, which is why the sort is stable.
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
pub fn refusals(program: &Program) -> Vec<Diagnostic> {
    // The must-use judgment, and nothing beside it out of this file: the move
    // check states no rule of its own any more (RFC-0125 §3 M3, the plumbing
    // slice), so what a reader gets is the kernel's list and the obligation's.
    let mut diags = Vec::new();
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
    // The answer IS handed on: a command opens `own::Memo` next and adopts it,
    // so a command analyses its program once (RFC-0125 §3 M3, the one analysis).
    //
    // THIS analysis is the one a judgment may be reused for, and no other: a
    // generator load's and an engine's both run outside this call, and neither
    // reads the refusals ([`reuse_judgments`]).
    JUDGING.with(|j| j.set(true));
    crate::own::hand_on(program, &crate::own::analyze(program));
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

/// The one walk, shared by [`owning_sites`], [`facts`] and [`projection_sites`].
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
    // function became a method. Stated once, in [`crate::declared::arg_caps`].
    let caps = crate::declared::arg_caps(program);
    let globals: HashSet<String> = program.globals.iter().map(|g| g.name.clone()).collect();
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
        cur_fn: RefCell::new(String::new()),
        writeback: RefCell::new(None),
        lets: want == Want::Lets,
        lambda_arities: (want == Want::Lets).then(|| RefCell::new(Default::default())),
        typed_lambdas: (want == Want::Lets).then(|| RefCell::new(Default::default())),
        lambda_sigs: (want == Want::Lets).then(|| RefCell::new(Default::default())),
        projections: (want == Want::Projections).then(|| RefCell::new(Vec::new())),
        stores: RefCell::new(Vec::new()),
    };
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
        mc.function(f);
        drain(&mut projections, &mc, &f.module);
    }
    // Test bodies (RFC-0015) move-check as ordinary Unit function bodies, so
    // use-after-consume inside a test is caught unchanged. The body is walked
    // **in place**: a clone would carry different node addresses, and Phase 4c
    // keys reclamation on them.
    for (i, t) in program.tests.iter().enumerate() {
        // The synthetic name `own::analyze` keys this body's rows by — the
        // finish check (RFC-0114 §26) matches owners against emitted names.
        *mc.cur_fn.borrow_mut() = format!("test@{i}");
        mc.body(&[], &Type::Unit, &t.body);
        drain(&mut projections, &mc, &t.module);
    }
    // Bench bodies (RFC-0055) move-check identically.
    for (i, b) in program.benches.iter().enumerate() {
        *mc.cur_fn.borrow_mut() = format!("bench@{i}");
        mc.body(&[], &Type::Unit, &b.body);
        drain(&mut projections, &mc, &b.module);
    }
    // RFC-0075's disposal obligation is NOT here, and RFC-0125 §3 M3's
    // obligation slice is why: it is a rule about a TYPE and this file states
    // rules about ownership. It was always a separate walk over the same
    // bodies — the two analyses want OPPOSITE merges at an `if`, because
    // use-after-consume is a may-analysis (consumed on either branch ⇒
    // consumed after) and "disposed exactly once" is a must-analysis — and it
    // is now the typed judgment's (`vyrn_lower::typed::obligation`), reached
    // from [`refusals`] through `own::must_use_refusals`.
    // Round forty-six's meet, over the target set of every fn-value signature
    // the program declares.
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
                // position is clear at every position, which is the answer a
                // key can carry. It used to ask two call-graph sets beside
                // the capability; both were empty for every program the
                // kernel accepts (RFC-0125 §3 M3, the wrapped lend).
                caps.get(m)
                    .is_some_and(|cs| cs.iter().all(|c| *c == Capability::Read))
            });
        if std::env::var("VYRN_MEET_DUMP").is_ok() {
            eprintln!("meet: key={key} members={members:?} clear={all_clear}");
        }
        if all_clear {
            fnval_clear.insert(key.clone());
        }
    }
    Run {
        projections,
        fnval_clear,
        arities: lambda_arities,
        sigs: lambda_sigs,
        stores: mc.stores.into_inner(),
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
    /// Per-function statement-boundary error sink (RFC-0006 accumulation).
    /// Cleared at the start of each function, drained by `check_accum`.
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
    /// The declaring `Stmt::Let` of every name in scope, in lockstep with `vars`
    /// — 0 for a parameter, a loop variable, a pattern binder or a lambda
    /// parameter. **Every** binder is recorded, so an inner `let s` shadowing an
    /// outer one takes the move rather than passing it up.
    /// Where the per-`let` ownership rows go, or `None` on the check path.
    /// The function being checked, so a recorded fact can name it.
    cur_fn: RefCell<String>,
    /// RFC-0125 M2: the binding a write-back statement `xs = xs.push(v)` is
    /// assigning, while its value is walked. A rebuilding row takes its
    /// receiver (`sinks`), and the statement form takes and revives it in one
    /// line — so the take is not recorded there, and the receiver is read.
    writeback: RefCell<Option<String>>,
    /// Whether this is the [`Want::Lets`] walk — the one `own::analyze` runs,
    /// which walks a projection's desugared statement group and records what
    /// a lambda captures. It was read off `lending.is_some()` until the two
    /// call-graph sets went (RFC-0125 §3 M3, the wrapped lend), which made a
    /// sink stand in for a mode.
    lets: bool,
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
    /// Where RFC-0092 M0's projection sites go, or `None` everywhere else. The
    /// measurement is a mode, not a second walk: the two places that would refuse
    /// are the two places that record.
    projections: Option<RefCell<Vec<ProjectionSite>>>,
    /// Every projection store this walk descended into, for [`lets_outputs`].
    stores: RefCell<Vec<String>>,
}

/// A binding that names a value somebody else owns (RFC-0089 rule 2).
///
/// Two states, because the walk asks two questions and no more: is this name a
/// borrow at all, and is the borrow a PROJECTION. There were four, one per
/// fix a menu offered, and each carried the name that fix had to spell. The
/// menus left with the refusals: `core::BorrowKind::what` and `::fixes` word a
/// borrow now, and nothing outside the kernel does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Borrow {
    /// A `read`, `modify` or `share` parameter, or a `for` variable over a
    /// container the loop does NOT own. The caller still owns it, and a loop
    /// over a `consume`d container or over a temporary binds an owner instead.
    Lent,
    /// A local bound to a field or element read: `let t = r.s`. A place owns its
    /// contents (rule 4), so reading one out does not take it.
    Projection,
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
        let mut scope: Vec<HashSet<String>> =
            vec![f_params.iter().map(|p| p.name.clone()).collect()];
        // Module state is the outermost frame, the parameters the next one — the
        // order every function body sees them in (RFC-0013).
        {
            let mut v = self.vars.borrow_mut();
            let mut b = self.borrows.borrow_mut();
            v.truncate(1);
            b.truncate(1);
            v.enter();
            b.enter();
            for p in f_params {
                v.bind(&p.name, Some(p.ty.clone()));
                // RFC-0089 rule 2: everything but `consume` is a borrow, and only
                // a type that owns heap has anything to borrow.
                b.bind(
                    &p.name,
                    match p.capability {
                        Capability::Consume => None,
                        _ if !self.decl.owns_heap(&p.ty) => None,
                        _ => Some(Borrow::Lent),
                    },
                );
            }
        }
        *self.ret.borrow_mut() = ret.clone();
        self.block(body, &mut scope);
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
    fn walk_writeback(&self, target: &str, value: &Expr, scope: &mut Vec<HashSet<String>>) {
        let writeback = matches!(
            value,
            Expr::Call { name: callee, args, .. }
                if self.sinks(callee, 0)
                    && args.first().and_then(store_path).as_deref() == Some(target)
        );
        if writeback {
            *self.writeback.borrow_mut() = Some(target.to_string());
        }
        self.expr(value, scope);
        *self.writeback.borrow_mut() = None;
    }

    /// Whether `name` names a borrow here.
    fn borrow_of(&self, name: &str) -> Option<Borrow> {
        self.borrows.borrow().get(name).copied().flatten()
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
                for (i, b) in arm.pattern.bindings().into_iter().enumerate() {
                    self.bind(b, tys.get(i).cloned().flatten(), borrow);
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
    ) {
        // Everything the four screens here used to guard — a scalar copies, a
        // borrow does not move, a projection does not move — decided a
        // `Consumed` entry, and it went with the table (RFC-0125 §3 M3, the
        // table slice). What is left is RFC-0092's instrument, and an
        // instrument is a MODE: the check walk and the facts walk record
        // nothing, so they ask nothing. The screen is here rather than at the
        // ten call sites because every one of them would need it.
        if self.projections.is_none() {
            return;
        }
        // An ELEMENT read stored inline: `out.push(xs[i])`. `xs[i]` reaches this
        // pass as `@at(xs, i)`, which is a call, so the `place_path` bail two
        // blocks down is where it used to leave — invisible to every rule.
        if let Some((_, path)) = element_path(value) {
            if outlives {
                let ty = self.type_of(value);
                self.note_projection("elem-store", &path, into(), ty, line);
            }
            if outlives && self.type_of(value).is_some_and(|t| self.decl.owns_heap(&t)) {
                return;
            }
        }
        let Some((root, path)) = place_path(value) else {
            return;
        };
        // Recorded BEFORE the `owns_heap` guard, so a scalar field is counted
        // and told apart rather than lost.
        if path != root && outlives && self.borrow_of(&root).is_none() {
            let ty = self.type_of(value);
            self.note_projection("store", &path, into(), ty, line);
        }
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
                Some(Expr::Var { name, .. }) => arm.pattern.bindings().contains(&name.as_str()),
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
        let n = p.bindings().len();
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

    /// The body of the free [`call_may_forward`], read by the core at the
    /// same call (`core::read_only_mentions`).
    ///
    /// Can a call to `name` return storage one of its arguments holds? The
    /// copying builtins cannot — `@concat`, `@str`, `@copy` and every seeded
    /// row that neither hands an argument back (identity-typed return), views,
    /// nor lends builds a fresh value. Everything else — an `@`-desugar like
    /// `@push`, a user function — is assumed able to, which is the leak
    /// direction.
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
                    for (i, b) in arm.pattern.bindings().into_iter().enumerate() {
                        self.bind(b, tys.get(i).cloned().flatten(), borrow);
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

    /// Returns whether this block **diverges** — every path out of it leaves via
    /// `return`/`break`/`continue` (RFC-0060). A statement after a diverging one
    /// is unreachable, so it is not checked (use-after-move there is not an
    /// error), and its consumptions never flow to the block's exit.
    fn block(&self, b: &Block, scope: &mut Vec<HashSet<String>>) -> bool {
        scope.push(HashSet::new());
        self.enter();
        let mut diverged = false;
        for s in &b.stmts {
            if diverged {
                // Unreachable after a `return`/`break`/`continue`: skip it
                // (the `return` precedent — code after it is unreachable-clean).
                break;
            }
            diverged = self.stmt(s, scope);
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
    fn stmt(&self, s: &Stmt, scope: &mut Vec<HashSet<String>>) -> bool {
        match s {
            Stmt::Let {
                name,
                value,
                ty,
                line,
                ..
            } => {
                self.expr(value, scope);
                // The binding's type: what it was declared, else what the
                // initializer yields — read against the PRE-binding environment,
                // so `let x = x + b` resolves the old `x`.
                let bty = ty.clone().or_else(|| self.type_of(value));
                // The `a[i].f = v` desugar's round-trip temp is exempt from
                // rule 2: the place is read out, mutated, and written straight
                // back to where it came from, so it is not a store of a borrow
                // — the parser built both halves and there is no second owner.
                // The desugar names the temp and [`ast::is_place_temp`] reads
                // that name back; this pass does not spell a suffix of its own.
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
                // it feeds does. Asking HERE rather than at that store is what
                // names `ps`, which the reader wrote, rather than the temp the
                // parser minted.
                let hoisted = crate::ast::hoisted_value(name);
                self.store(
                    value,
                    &|| match hoisted {
                        Some(place) => format!("`{place}`"),
                        None => format!("the binding `{name}`"),
                    },
                    *line,
                    hoisted.is_some(),
                );
                self.bind(name, bty, borrow);
                scope.last_mut().unwrap().insert(name.clone());
                false
            }
            Stmt::Assign { name, value, line } => {
                // The write-back form of a rebuilding row: `xs = xs.push(v)`.
                // The receiver comes back through the result and the store
                // revives the binding, so its take is not recorded.
                self.walk_writeback(name, value, scope);
                // Module state (RFC-0013) is a place with a whole-module lifetime,
                // so 4b treats a store into it differently from a local's.
                let global = self.globals.contains(name) && !Self::in_scope(scope, name);
                let into = || {
                    if global {
                        format!("module state `{name}`")
                    } else {
                        format!("`{name}`")
                    }
                };
                self.store(value, &into, *line, global);
                // An assignment rebinds, exactly as a `let` does, so it must
                // carry the same answer: `t = d.title` makes `t` a projection of
                // `d`. Without this, `let t = d.title` was refused at the next
                // store and `let mut t = "" ; t = d.title` was not — RFC-0092's
                // two-spellings-two-verdicts defect, one statement over.
                if !global {
                    let b = self.borrow_from(value);
                    // And it must carry the RECLAMATION answer too, which is the
                    // other half of the same sentence. `let t = d.title` gets
                    // `names_a_place` and is therefore never released; `t =
                    // d.title` got nothing, so the block freed a buffer the
                    // record still holds and releases again — `out = v` inside
                    // `if let Some(v) = o` is the same store one keyword over.
                    // One question, asked at both spellings.
                    self.borrows.borrow_mut().rebind(name, b);
                }
                false
            }
            Stmt::SetField {
                name,
                field,
                value,
                line,
            } => {
                self.walk_writeback(&format!("{name}.{field}"), value, scope);
                self.store(
                    value,
                    &|| format!("the field `{name}.{field}`"),
                    *line,
                    true,
                );
                // RFC-0093: a write fills the hole a take left. The same
                // sentence `Stmt::Assign` has carried since Phase 4b, one dot
                // down — and the reason no drop flag is needed to say it.
                false
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
                if self.lets && crate::project::memo_open() {
                    let aty = self.vars.borrow().get(name).cloned().flatten();
                    if let Some(aty) = aty {
                        if let Ok(Some(blk)) =
                            crate::project::store_index(self.impls, name, index, value, &aty)
                        {
                            if std::env::var_os("VYRN_PROJ_DUMP").is_some() {
                                eprintln!("proj-store walked: {name} line {line}");
                            }
                            self.stores
                                .borrow_mut()
                                .push(format!("store {name}:{line}"));
                            self.block(blk, scope);
                            return false;
                        }
                    }
                }
                self.expr(index, scope);
                self.expr(value, scope);
                self.store(value, &|| format!("`{name}`"), *line, true);
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
                self.store(index, &|| format!("`{name}`"), *line, true);
                false
            }
            Stmt::Return { value, line } => {
                if let Some(e) = value {
                    self.expr(e, scope);
                    self.note_returned_projection(e, *line);
                }
                true
            }
            // `break`/`continue` (RFC-0060) consume nothing but terminate the
            // path — code after them in the same block is unreachable.
            Stmt::Break { .. } => true,
            Stmt::Continue { .. } => true,
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.expr(cond, scope);
                let then_div = self.block(then_block, scope);
                let else_div = match else_block {
                    Some(eb) => self.block(eb, scope),
                    None => false,
                };
                then_div && else_div
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
                self.expr(scrutinee, scope);
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
                for (i, b) in pattern.bindings().into_iter().enumerate() {
                    scope.last_mut().unwrap().insert(b.to_string());
                    // Recording it is the point: an unrecorded binder falls
                    // through to whatever the enclosing scope calls that name
                    // (`own.rs`'s shadowing lesson).
                    self.bind(b, tys.get(i).cloned().flatten(), borrow);
                }
                let then_div = self.block(then_block, scope);
                self.exit();
                scope.pop();
                let else_div = match else_block {
                    Some(eb) => self.block(eb, scope),
                    None => false,
                };
                then_div && else_div
            }
            Stmt::While { cond, body, .. } => {
                self.expr(cond, scope);
                let _ = self.block(body, scope);
                false
            }
            // A `for` loop consumes like a `while`: the iterable is read once,
            // and consuming an outer binding in the body is a use-again error.
            Stmt::ForIn {
                var,
                iter,
                body,
                consuming,
                ..
            } => {
                self.expr(iter, scope);
                let elem = self.type_of(iter).and_then(|t| self.decl.elem_of(&t));
                // RFC-0089 rule 2: the loop variable is a borrow only while the
                // container outlives the loop. A `consume`d container is the
                // loop's, and a container that is not a place — `for o in
                // diff(..)` — has no other owner at all, so both bind an OWNED
                // element and storing one is a move.
                let borrow = (!*consuming
                    && self.iterable_is_a_place(iter)
                    && elem.as_ref().is_some_and(|t| self.decl.owns_heap(t)))
                .then_some(Borrow::Lent);
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
                self.enter();
                self.bind(var, elem, borrow);
                let _ = self.block(body, scope);
                self.exit();
                false
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
                self.expr(e, scope);
                matches!(e, Expr::Call { name, .. } if crate::ast::is_panic(name))
            }
            // A `region` is an ordinary nested block for move checking; it
            // diverges iff its body does (a `break` inside it exits the loop).
            Stmt::Region { body, .. } => self.block(body, scope),
            // `drop name` has nothing left for this pass. All THREE of its
            // refusals are the kernel's (RFC-0125 §3 M3, rows 20, 21 and 22) —
            // a `drop` of what a take already took, a `drop` of a borrow, and a
            // `drop` of a binding a take left a hole in — and the record it
            // wrote for the walk's own table went with the table.
            Stmt::Drop { .. } => false,
        }
    }

    fn expr(&self, e: &Expr, scope: &mut Vec<HashSet<String>>) {
        match e {
            Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {}
            Expr::Var { .. } => {}
            Expr::Unary { expr, .. } => self.expr(expr, scope),
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs, scope);
                // An operand of a String `+`, of a String comparison and of
                // `=~` is a call argument — `@concat`'s — and the core states
                // its release row from the lowered operator (RFC-0125 §3 M3,
                // the last table's slice). This pass recorded the same three
                // shapes until then.
                self.expr(rhs, scope)
            }
            // A place chain asks ONE consumption question, of the whole path.
            // Walking into the root instead would ask it of `er` and refuse
            // `er.next` after `consume er.node`, which is the case RFC-0093
            // exists to allow.
            Expr::Field { expr, .. } => match place_path(e) {
                Some(_) => {}
                None => {
                    // RFC-0114 R1′ was recorded here: a receiver with no
                    // name — `.byteLength` on a String temporary, `.length`
                    // on a container one, a record field of one. The core
                    // states that row on the name itself now
                    // (`NameInfo::receiver`), and the emitter reads the core
                    // alone, so this walk records nothing for it
                    // (RFC-0125 §3 M3, the emitter-reads-the-core-alone
                    // slice).
                    self.expr(expr, scope)
                }
            },
            // RFC-0093 — the take. It is the kernel's rule and the kernel's
            // hole set now, so the walk carries the operand and records
            // nothing.
            Expr::Consume { place, .. } => self.expr(place, scope),
            Expr::Try { expr, .. } => {
                self.expr(expr, scope);
            }
            // A literal's operands are places too: `Ring { slots: xs }` puts `xs`
            // where the record owns it, exactly as an argument does.
            Expr::StructLit { name, fields, line } => {
                for (f, v) in fields {
                    self.expr(v, scope);
                    self.store(v, &|| format!("the field `{name}.{f}`"), *line, true);
                }
            }
            Expr::TryConstruct { name, args, line } => {
                for a in args {
                    self.expr(a, scope);
                    self.store(a, &|| format!("`{name}`"), *line, true);
                }
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee, scope);
                for arm in arms {
                    scope.push(HashSet::new());
                    self.enter();
                    let (tys, borrow) = self.payload_binding(scrutinee, &arm.pattern);
                    for (i, b) in arm.pattern.bindings().into_iter().enumerate() {
                        scope.last_mut().unwrap().insert(b.to_string());
                        self.bind(b, tys.get(i).cloned().flatten(), borrow);
                    }
                    match &arm.body {
                        ArmBody::Expr(body) => self.expr(body, scope),
                        // The statements walk as statements, inside the same
                        // binder scope and branch stamp an expression arm gets.
                        ArmBody::Block(b) => {
                            self.block(b, scope);
                        }
                    }
                    self.exit();
                    scope.pop();
                }
            }
            // `if` as an expression (RFC-0030): its two branches are match arms.
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                self.expr(cond, scope);
                self.expr(then_branch, scope);
                if let Some(eb) = else_branch {
                    self.expr(eb, scope);
                }
            }
            Expr::Call { name, args, line } => {
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
                    self.expr(arg, scope);
                    // A builtin whose parameter declares `consume` takes its
                    // argument, and rule 1 governs the take exactly as it
                    // governs `xs = [.., v]`. Three positions stand aside, and
                    // each is somebody else's rule now:
                    //
                    //   * a `consume` parameter of a declared function, which
                    //     the kernel takes at the call;
                    //   * a variant constructor's argument, which the kernel
                    //     refuses when it is a borrow or a projection and takes
                    //     when it is a whole owned name (RFC-0125 §3 M3, row
                    //     19);
                    //   * the receiver of a write-back statement
                    //     (`xs = xs.push(v)`), which the call hands back into
                    //     the same place, so nothing moves. Whether that
                    //     receiver was itself a borrow is the kernel's question
                    //     at the `let` (row 26).
                    if caps.and_then(|c| c.get(i)) != Some(&Capability::Consume)
                        && !self.decl.constructs(name)
                        && self.sinks(name, i)
                        && !(i == 0
                            && store_path(arg).as_deref() == self.writeback.borrow().as_deref())
                    {
                        self.store(
                            arg,
                            &|| format!("`{}(..)`", crate::parser::method_surface(name)),
                            *line,
                            true,
                        );
                    }
                }
            }
            Expr::ArrayLit { elems, line } => {
                for e in elems {
                    self.expr(e, scope);
                    self.store(e, &|| "the array literal".to_string(), *line, true);
                }
            }
            Expr::MapLit { entries, line } => {
                for (k, v) in entries {
                    self.expr(k, scope);
                    self.expr(v, scope);
                    self.store(k, &|| "the map literal".to_string(), *line, true);
                    self.store(v, &|| "the map literal".to_string(), *line, true);
                }
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
                scope.push(HashSet::new());
                self.enter();
                for p in params {
                    scope.last_mut().unwrap().insert(p.clone());
                    self.bind(p, None, None);
                }
                // Rule 3 reaches closures (exit-residue round nine): a
                // lambda's result is its CALLER's, and a captured heap value
                // returned raw hands out storage the capture block still owns
                // — the emitted body is `ret ptr %cap`, no copy, so the first
                // caller to release its result frees the block's buffer and
                // the next call reads it freed. The kernel states it, from
                // the capture the core marks (`core::BorrowKind::Capture`) and
                // in these same words (RFC-0125 §3 M3, row 28).
                match body {
                    LambdaBody::Expr(inner) => self.expr(inner, scope),
                    LambdaBody::Block(b) => {
                        self.block(b, scope);
                    }
                }
                self.exit();
                scope.pop();
            }
            // `spawn f(args)` moves arguments exactly like a direct call: a
            // `consume` parameter takes ownership across the task boundary.
            Expr::Spawn { args, .. } => {
                for arg in args {
                    self.expr(arg, scope);
                }
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
///
/// The descent is `ast::body_scope_descent!`'s since RFC-0125 §3 M6; what is
/// this probe's own is the derived-name test and the two forms it answers
/// `true` for without descending.
pub fn mentions_place(e: &Expr, base: &str) -> bool {
    /// The probe's line at each site: a name derived from the base is a
    /// mention, and two forms answer `true` without being read.
    struct Mentions<'a> {
        base: &'a str,
        found: bool,
    }

    impl Mentions<'_> {
        fn derived(&self, n: &str) -> bool {
            let base = self.base;
            n == base
                || (n.len() > base.len()
                    && n.starts_with(base)
                    && matches!(n.as_bytes()[base.len()], b'.' | b'['))
        }
    }

    impl BodyVisit<'_> for Mentions<'_> {
        const SCOPED: bool = false;

        fn expr(&mut self, e: &Expr, _: &HashSet<String>) -> bool {
            match e {
                Expr::Var { name, .. } if self.derived(name) => self.found = true,
                // A block-bodied lambda and a block match arm (RFC-0118) both
                // answer `true` without being read, for the reason the doc
                // gives: `true` costs a leak and `false` can cost a
                // use-after-free.
                Expr::Lambda {
                    body: LambdaBody::Block(_),
                    ..
                } => self.found = true,
                Expr::Match { arms, .. } if arms.iter().any(|a| a.body.as_expr().is_none()) => {
                    self.found = true
                }
                _ => {}
            }
            !self.found
        }
    }

    let mut v = Mentions { base, found: false };
    body_expr(e, &HashSet::new(), &mut v);
    v.found
}

// ---------------------------------------------------------------------------
// The AST predicates the must-use judgment reads, and the checker's
// exclusivity rule with it (RFC-0125 §3 M3, the obligation slice and row 23).
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

// The descent over a body is `ast::body_scope_descent!`'s, where the AST is
// declared (RFC-0125 §3 M6). This file's collectors read it; the judgment
// itself — `MoveCheck::stmt` and `MoveCheck::expr` — states a fact per arm and
// keeps its own.
crate::body_scope_descent!(BodyVisit, body_block, body_stmt, body_expr);

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

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
}
