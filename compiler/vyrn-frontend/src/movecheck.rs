//! The ownership judgment's driver, and the facts the core reads at a call.
//! Every ownership rule is the kernel's (`vyrn_lower::kernel`) and every
//! obligation rule is the typed judgment's; this module states none. It sorts
//! their refusals into source order ([`in_source_order`]), marks a generator's
//! program ([`comptime`]), memoizes the kernel's per-body judgment for the editor
//! ([`Judgments`]), and answers the fn-value meet ([`facts`]) and the
//! argument-temporary screens the core asks at a call ([`arg_verdict`]).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::declared::Declared;
use crate::diagnostics::Diagnostic;

/// A call-argument position whose argument expression built the value it hands
/// over, as [`arg_verdict`] reads it.
#[derive(Clone, Debug)]
pub struct ArgTemp {
    pub callee: String,
    /// The parameter index it fills.
    pub ix: usize,
    /// The call that built the value, or `None` where a String `+` did.
    pub producer: Option<String>,
    /// The callee is a view whose element type owns no heap, so the scalar it
    /// hands out cannot alias the temporary (`bytes(l)[0]`).
    pub view_copies: bool,
    /// The callee is a variant constructor.
    pub constructs: bool,
    /// The parameter's declared capability.
    pub cap: Option<Capability>,
}

/// What the callee at a call-argument position does with the temporary it is
/// given. Only [`ArgVerdict::Released`] frees.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArgVerdict {
    /// A `read` parameter: the caller releases the temporary after the call.
    /// Rules 2 and 3 refuse every way the callee could keep it.
    Released,
    /// A `consume` parameter: the callee owns it.
    Transferred,
    /// A variant constructor keeps it.
    Retained,
    /// The result points into the argument, or the producer handed back
    /// storage it does not own.
    Lent,
    /// No signature is visible, so nothing is freed.
    Unknown,
    /// The consumer rule frees this operand; recorded so both rules
    /// cannot fire on one value.
    AlreadyFreed,
}

pub struct Facts {
    /// The fn-value signature keys ([`fn_sig_key`]) whose every target reads
    /// every position. A call through a fn value names no function, so no
    /// capability row answers for it. A lambda carries no capability, so a
    /// signature any lambda could inhabit is never in this set.
    pub fnval_clear: HashSet<String>,
}

pub fn facts(program: &Program) -> Facts {
    let decl = declarations(program);
    let lets = Lets::over(program, &decl);
    let caps = crate::declared::arg_caps(program);
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
        if lets.arities.contains(arity) || lets.sigs.contains(key) {
            continue;
        }
        let all_clear = !members.is_empty()
            && members.iter().all(|m| {
                // A key carries no position, so a member must read every one.
                caps.get(m)
                    .is_some_and(|cs| cs.iter().all(|c| *c == Capability::Read))
            });
        if all_clear {
            fnval_clear.insert(key.clone());
        }
    }
    Facts { fnval_clear }
}

/// Runs the checker's record for its effect too: it expands each `place atSet`
/// store into `project`'s memo, which [`Lets`] reads. It also records the
/// projection names that `views` and `project::element_path` read, including
/// from the core where no walk of this file has run.
fn declarations(program: &Program) -> Declared {
    crate::project::note_place_names(program);
    let rec_span = crate::prof::phase("movecheck: checker::record");
    let rec = crate::checker::recorded(program);
    drop(rec_span);
    Declared::new(program).recording(rec)
}

crate::body_scope_descent!(LetsVisit, lets_block, lets_stmt, lets_expr);

/// The lambdas that stand the fn-value meet down. An untyped lambda poisons
/// its arity; one at an argument position with a declared fn type poisons only
/// that signature. An `a[i] = v` store expanded through a `place atSet`
/// projection is read as the expansion, the nodes the lowering walks.
struct Lets<'a> {
    decl: &'a Declared,
    /// Lambda nodes an argument position already gave a signature.
    typed: HashSet<NodeId>,
    /// Index and value nodes a projection store's expansion stands in for.
    skipped: HashSet<NodeId>,
    arities: HashSet<usize>,
    sigs: HashSet<String>,
    stores: Vec<String>,
}

impl<'a> LetsVisit<'a> for Lets<'_> {
    const SCOPED: bool = false;

    fn stmt(&mut self, s: &'a Stmt, _: &HashSet<String>) {
        let Stmt::IndexSet {
            name,
            index,
            value,
            line,
            id: _,
        } = s
        else {
            return;
        };
        let Some(blk) = crate::project::stored(name, index, value) else {
            return;
        };
        self.skipped.insert(index.id());
        self.skipped.insert(value.id());
        self.stores.push(format!("store {name}:{line}"));
        lets_block(blk, &mut HashSet::new(), self);
    }

    fn expr(&mut self, e: &'a Expr, _: &HashSet<String>) -> bool {
        if self.skipped.contains(&e.id()) {
            return false;
        }
        match e {
            // Recorded before the walk reaches the lambda, so it poisons only
            // its own signature and not its arity.
            Expr::Call { name, args, .. } => {
                for (i, a) in args.iter().enumerate() {
                    if !matches!(a, Expr::Lambda { .. }) {
                        continue;
                    }
                    let Some(pt) = self.decl.param_ty(name, i) else {
                        continue;
                    };
                    if let Type::Fn(ps, r) = crate::types::resolve(pt, self.decl.decls()) {
                        self.typed.insert(a.id());
                        self.sigs.insert(fn_sig_key(&ps, &r, self.decl.decls()));
                    }
                }
            }
            Expr::Lambda { params, .. } => {
                if !self.typed.contains(&e.id()) {
                    self.arities.insert(params.len());
                }
            }
            _ => {}
        }
        true
    }
}

impl<'a> Lets<'a> {
    /// Walks every function, test and bench body, and no module-state
    /// initializer.
    fn over(program: &'a Program, decl: &'a Declared) -> Lets<'a> {
        let mut v = Lets {
            decl,
            typed: HashSet::new(),
            skipped: HashSet::new(),
            arities: HashSet::new(),
            sigs: HashSet::new(),
            stores: Vec::new(),
        };
        let mut locals = HashSet::new();
        let bodies = program
            .functions
            .iter()
            .map(|f| &f.body)
            .chain(program.tests.iter().map(|t| &t.body))
            .chain(program.benches.iter().map(|b| &b.body));
        for b in bodies {
            lets_block(b, &mut locals, &mut v);
        }
        v
    }
}

/// Returns the key a fn-value signature meets under: the resolved parameter
/// and result types. This pass and the core both key on it.
pub fn fn_sig_key(ps: &[Type], ret: &Type, decls: &HashMap<String, TypeDecl>) -> String {
    let rps: Vec<Type> = ps.iter().map(|t| crate::types::resolve(t, decls)).collect();
    format!("{rps:?}->{:?}", crate::types::resolve(ret, decls))
}

/// Returns what [`Lets`] reads off a program, one sorted line per row: an
/// untyped lambda's arity, a typed lambda's signature key, or a projection
/// store. `vyrn-cli/tests/letswalk.rs` prints them over the corpus.
pub fn lets_outputs(program: &Program) -> Vec<String> {
    let decl = declarations(program);
    let lets = Lets::over(program, &decl);
    let mut out: Vec<String> = lets.arities.iter().map(|n| format!("arity {n}")).collect();
    out.extend(lets.sigs.iter().map(|k| format!("sig {k}")));
    out.extend(lets.stores);
    out.sort();
    out.dedup();
    out
}

/// Whether the builtin `name` hands an argument back: its seeded row returns
/// the bare type parameter of one of its parameters (`blackBox`).
pub fn hands_back(name: &str) -> bool {
    crate::prelude::signature(name).is_some_and(|f| {
        matches!(&f.ret, Type::Param(r)
            if f.params.iter().any(|p| matches!(&p.ty, Type::Param(q) if q == r)))
    })
}

/// Whether a call to `name` may return storage one of its arguments holds.
/// `@concat`, `@str`, `@copy` and a seeded row that neither hands back, views
/// nor lends build a fresh value. Anything else may, which errs toward a leak.
pub fn call_may_forward(name: &str) -> bool {
    if matches!(name, "@concat" | "@str" | "@copy") {
        return false;
    }
    // `@push`'s row returns `Array<T>`, not a bare parameter, yet hands its
    // receiver's buffer back (a double free in map.vyrn).
    if name.starts_with('@') {
        return true;
    }
    if crate::prelude::signature(name).is_some() {
        return hands_back(name) || views(name) || crate::prelude::lends(name);
    }
    true
}

/// Whether `name` hands back a pointer into its argument.
pub fn lends_result(name: &str) -> bool {
    views(name)
}

/// What a callee does with a temporary at an argument position: the verdict
/// of the first row that holds, and [`ArgVerdict::Unknown`] where none does.
/// Rules 2 and 3 make `read` mean "keeps nothing": a borrow may be neither
/// stored nor returned.
const ARG_ROWS: [(ArgVerdict, fn(&ArgTemp) -> bool); 7] = [
    // The consumer sites free this operand already; this is where the two
    // rules partition.
    (ArgVerdict::AlreadyFreed, |s| {
        matches!(s.producer.as_deref(), None | Some("@str" | "@concat"))
            && matches!(s.callee.as_str(), "@str" | "@concat")
    }),
    // A constructor has no signature, so it is asked first.
    (ArgVerdict::Retained, |s| s.constructs),
    // A view lends, unless the element it hands out is a heap-free copy.
    (ArgVerdict::Lent, |s| views(&s.callee) && !s.view_copies),
    // A row that returns this argument's bare type parameter may hand the
    // argument back (`blackBox`), so freeing it here is a use-after-free
    // (`examples/membench.vyrn`). `lends` cannot say this: the row yields the
    // parameter itself, not a place inside it.
    (ArgVerdict::Lent, |s| {
        crate::prelude::signature(&s.callee).is_some_and(|f| {
            matches!((&f.ret, f.params.get(s.ix).map(|p| &p.ty)),
                (Type::Param(r), Some(Type::Param(p))) if r == p)
        })
    }),
    // `@copy`'s row is held back from the return table, so no capability
    // answers for its receiver below, and `("" + s).copy()` would leak.
    (ArgVerdict::Released, |s| s.callee == "@copy" && s.ix == 0),
    (ArgVerdict::Released, |s| s.cap == Some(Capability::Read)),
    // `modify` and `share` write through the argument, which no temporary
    // can be the destination of: no row, so `Unknown`.
    (ArgVerdict::Transferred, |s| {
        s.cap == Some(Capability::Consume)
    }),
];

/// Returns what the callee does with the temporary `s` ([`ARG_ROWS`]).
pub fn arg_verdict(s: &ArgTemp) -> ArgVerdict {
    ARG_ROWS
        .iter()
        .find(|(_, holds)| holds(s))
        .map_or(ArgVerdict::Unknown, |(v, _)| *v)
}

/// Whether `name` hands back a pointer into its argument: a seeded row whose
/// body yields a place inside a parameter (`@at`), or a user projection.
/// A binding to its result owns nothing, so nothing releases it.
fn views(name: &str) -> bool {
    crate::prelude::lends(name) || crate::project::named_projection(name)
}

/// Sorts refusals into source order, since no walk order is one a reader can
/// predict. Files keep the order they were first named in; two refusals on one
/// line keep the walk's order, so the sort is stable.
pub fn in_source_order(diags: &mut [Diagnostic]) {
    let mut files: Vec<Option<String>> = Vec::new();
    for d in diags.iter() {
        if !files.contains(&d.file) {
            files.push(d.file.clone());
        }
    }
    diags.sort_by_key(|d| (files.iter().position(|f| *f == d.file).unwrap_or(0), d.line));
}

thread_local! {
    /// Set inside [`comptime`].
    static COMPTIME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `f` with the program marked as a generator's own, whose lowered form
/// the core does not lint (`vyrn_lower::core`).
pub fn comptime<T>(f: impl FnOnce() -> T) -> T {
    let was = COMPTIME.with(|c| c.replace(true));
    let out = f();
    COMPTIME.with(|c| c.set(was));
    out
}

/// Whether the program being worked on is a generator's own.
pub fn in_comptime() -> bool {
    COMPTIME.with(|c| c.get())
}

/// The kernel's refusals of one body, with no address in them. A body that
/// earns none caches an empty list.
pub type Verdict = Vec<Refusal>;

/// One kernel refusal, worded as the checker words it so the CLI prints it
/// as it prints the checker's.
#[derive(Debug, Clone)]
pub struct Refusal {
    pub diagnostic: Diagnostic,
    /// The body the refusal is in, for the per-body count and the corpus
    /// test's tally.
    pub body: String,
}

/// The body's module, that module's content hash, and the instance's spelling.
pub type JudgmentKey = (String, String, String);

thread_local! {
    /// Set by [`reuse_judgments`].
    static REUSE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Set by [`emit_nothing`].
    static NO_EMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Whether the analysis running now is the one [`judging`] runs.
    static JUDGING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The declaration fingerprint and the entries valid under it. A new
    /// fingerprint drops the whole map, so nothing needs eviction.
    static JUDGED: RefCell<(u64, HashMap<JudgmentKey, Verdict>)> =
        RefCell::new((0, HashMap::new()));
    /// `(judged, reused)` since [`reset_judgment_tally`].
    static TALLY: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}

/// Arms reuse of the kernel's per-body judgment across calls.
///
/// A reused body is not built, so the placer adds no release rows for it and
/// folds none of its frames into the core's facts. Only a host that reads
/// refusals and lowers nothing (the editor) may arm this. Within one analysis
/// the missing rows cannot reach another body: `place_frames` keys every table
/// by node, and a body's other instances share its key's module and hash.
pub fn reuse_judgments() {
    REUSE.with(|r| r.set(true));
}

/// Whether the analysis running now may reuse a judgment: the host armed it,
/// and [`judging`] runs this analysis.
pub fn reusing_judgments() -> bool {
    REUSE.with(|r| r.get()) && JUDGING.with(|j| j.get())
}

/// Declares that this host emits nothing from the program it checks (`vyrn
/// check`), so the placer skips the facts only an emitter reads.
pub fn emit_nothing() {
    NO_EMIT.with(|r| r.set(true));
}

/// Whether the analysis running now feeds an emitter. Only the analysis
/// [`judging`] runs can answer no: a generator compiled during the load
/// still needs its facts.
pub fn emitting() -> bool {
    !(NO_EMIT.with(|r| r.get()) && JUDGING.with(|j| j.get()))
}

/// Runs `f`, the analysis whose refusals `vyrn_lower::refusals` reports. Only
/// that analysis may reuse a judgment or skip the emitter's facts.
pub fn judging<T>(f: impl FnOnce() -> T) -> T {
    JUDGING.with(|j| j.set(true));
    let out = f();
    JUDGING.with(|j| j.set(false));
    out
}

/// The judgment cache, open for one analysis. It copies the program's module
/// hashes once rather than cloning the map per body.
pub struct Judgments {
    hashes: std::collections::BTreeMap<String, String>,
}

impl Judgments {
    /// Opens the cache for `program`, or returns `None` where nothing is
    /// armed. Drops every entry if the declaration fingerprint moved.
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
            hashes: program.module_hashes.clone(),
        })
    }

    /// Returns `None` for the root module, which a keystroke edits, and for a
    /// module the loader recorded no hash for.
    pub fn key(&self, module: Option<&str>, spelling: &str) -> Option<JudgmentKey> {
        let m = module?;
        let h = self.hashes.get(m)?;
        Some((m.to_string(), h.clone(), spelling.to_string()))
    }

    /// Returns the verdict recorded for `key`, and tallies the hit or miss.
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

    /// Records what a body earned. The caller writes an entry only for an
    /// inert body, which only it can tell.
    pub fn put(&self, key: JudgmentKey, verdict: Verdict) {
        JUDGED.with(|j| j.borrow_mut().1.insert(key, verdict));
    }
}

/// Returns `(judged, reused)` since [`reset_judgment_tally`].
pub fn judgment_tally() -> (u64, u64) {
    TALLY.with(|t| t.get())
}

pub fn reset_judgment_tally() {
    TALLY.with(|t| t.set((0, 0)));
}

/// Hashes every declaration the kernel's judgment of a body can read across a
/// module boundary, and no function body, so an edit inside one body re-judges
/// no other. It includes each parameter's capability (`read x` to `consume x`
/// changes what every caller owes), each projection's whole body (it is inlined
/// into its callers), and each validated type's predicate and module-state
/// initializer. The parts are sorted first because two sources are hash maps.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_read_the_seeded_rows() {
        assert!(views(crate::project::AT));
        // Every engine copies `bytes`, so its result is owned.
        assert!(!views("bytes"), "a copy is not a view");
        assert!(!views("stringFromBytes"), "its inverse allocates");
    }
}
