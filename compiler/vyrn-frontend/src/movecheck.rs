//! The ownership judgment's driver, and the facts the core reads at a call.
//! Every ownership rule is the kernel's (`vyrn_lower::kernel`) and every
//! obligation rule is the typed judgment's; this module states none. It sorts
//! their refusals into source order ([`in_source_order`]), memoizes the
//! kernel's per-body judgment for the editor ([`Judgments`]), and answers the argument-temporary screens the core asks
//! at a call ([`arg_verdict`]).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::ast::*;
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
    /// The callee hands out a place inside its argument
    /// ([`lends_result`]).
    pub views: bool,
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
/// `places` are the program's user projection names.
pub fn call_may_forward(name: &str, places: &HashSet<String>) -> bool {
    if matches!(name, "@concat" | "@str" | "@copy") {
        return false;
    }
    // `@push`'s row returns `Array<T>`, not a bare parameter, yet hands its
    // receiver's buffer back (a double free in map.vyrn).
    if name.starts_with('@') {
        return true;
    }
    if crate::prelude::signature(name).is_some() {
        return hands_back(name) || views(name, places) || crate::prelude::lends(name);
    }
    true
}

/// Whether `name` hands back a pointer into its argument.
pub fn lends_result(name: &str, places: &HashSet<String>) -> bool {
    views(name, places)
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
    (ArgVerdict::Lent, |s| s.views && !s.view_copies),
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
fn views(name: &str, places: &HashSet<String>) -> bool {
    crate::prelude::lends(name) || places.contains(name)
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
///
/// A body's verdict reads three things. The module's hash covers the body's
/// text and the module-state accumulators, which only the module's own bodies
/// decide because module state is private. [`declaration_fingerprint`] covers
/// the declarations the body's module can read and the fn-value signatures a
/// lambda in any body clears. The third is the effect judgment's answer for
/// each frame of the body: which callee may store into which module state.
/// That answer joins every body of the program, a callback's included, so no
/// key can hold it.
/// [`Judgment::state`] records it, and the placer serves the verdict only
/// where this analysis gives the same answer (`vyrn_lower::core::augment`).
pub type JudgmentKey = (String, String, String);

/// What the memo keeps of one body.
#[derive(Debug, Clone)]
pub struct Judgment {
    pub verdict: Verdict,
    /// Its frames as the effect judgment read them. A served body gives the
    /// judgment these in place of a build.
    pub frames: Vec<crate::effects::Walked>,
    /// Per frame, each callee with the module state a call to it may store
    /// into: the effect judgment's answer that `verdict` read.
    pub state: Vec<Vec<(String, Vec<String>)>>,
}

thread_local! {
    /// Set by [`reuse_judgments`].
    static REUSE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Set by [`emit_nothing`].
    static NO_EMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The declaration fingerprint and the entries valid under it. A new
    /// fingerprint drops the whole map, so nothing needs eviction.
    static JUDGED: RefCell<(u64, HashMap<JudgmentKey, Judgment>)> =
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

/// Declares that this host emits nothing from the program it checks (`vyrn
/// check`), so the placer skips the facts only an emitter reads.
pub fn emit_nothing() {
    NO_EMIT.with(|r| r.set(true));
}

/// Whether the host emits from the program it checks: no after [`emit_nothing`].
pub fn emitting() -> bool {
    !NO_EMIT.with(|r| r.get())
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
        if !REUSE.with(|r| r.get()) {
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

    /// Takes the entry recorded for `key` out of the memo. The caller puts
    /// back each entry it serves.
    pub fn take(&self, key: &JudgmentKey) -> Option<Judgment> {
        JUDGED.with(|j| j.borrow_mut().1.remove(key))
    }

    /// Tallies one keyed body, served or judged.
    pub fn tally(&self, served: bool) {
        TALLY.with(|t| {
            let (judged, reused) = t.get();
            match served {
                true => t.set((judged, reused + 1)),
                false => t.set((judged + 1, reused)),
            }
        });
    }

    /// Records what a body earned. The caller writes an entry only for an
    /// inert body, which only it can tell.
    pub fn put(&self, key: JudgmentKey, judgment: Judgment) {
        JUDGED.with(|j| j.borrow_mut().1.insert(key, judgment));
    }
}

/// Returns `(judged, reused)` since [`reset_judgment_tally`].
pub fn judgment_tally() -> (u64, u64) {
    TALLY.with(|t| t.get())
}

pub fn reset_judgment_tally() {
    TALLY.with(|t| t.set((0, 0)));
}

/// Hashes every declaration the kernel's judgment of an imported module's body
/// can read across a module boundary, and no function body, so an edit inside
/// one body re-judges no other. It includes each parameter's capability (`read
/// x` to `consume x` changes what every caller owes), each projection's whole
/// body (it is inlined into its callers), and each validated type's predicate
/// and module-state initializer. The parts are sorted first because three
/// sources are hash maps. Every part but a projection is written without its
/// positions ([`unplaced`]), so a line moved in the root moves no part; a
/// projection's expansion keeps its lines, which an imported caller's refusal
/// quotes.
///
/// It leaves out the root's functions and module state, which no imported body
/// reads: no module imports the root, and the link leaves every declared name
/// unique. Two kinds of root function stay in, because a module reaches them
/// without importing them: an `extern` (the link keeps the root's copy of a
/// shared one), and one under a name every module can spell without declaring
/// it (a builtin, a protocol member or a projection), which the core resolves
/// by name alone (`ArgCaps::named`). The root's impl methods stay in through
/// their `impl` blocks, and its types and protocols stay in: an imported
/// generic is instantiated at a root type and dispatches to the root's impls.
fn declaration_fingerprint(program: &Program) -> u64 {
    let spelled: HashSet<&str> = (program.protocols.iter())
        .flat_map(|p| p.methods.iter().map(|m| m.name.as_str()))
        .chain((program.impls.iter()).flat_map(|i| i.places.iter().map(|p| p.name.as_str())))
        .collect();
    let read = |f: &&Function| {
        f.module.is_some()
            || f.is_extern
            || spelled.contains(f.name.as_str())
            || crate::prelude::builtin(&f.name).is_some()
            || is_surface_builtin(&f.name)
    };
    let sig = |f: &Function| {
        let mut s = String::from("f");
        crate::checker::recheck::head(&mut s, f);
        s
    };
    let mut parts: Vec<String> =
        Vec::with_capacity(program.functions.len() + program.type_decls.len());
    parts.extend(program.functions.iter().filter(read).map(&sig));
    for t in &program.type_decls {
        parts.push(format!(
            "t{:?}/{}<{:?}>={:?}|{}",
            t.module,
            t.name,
            t.type_params,
            t.base,
            unplaced(&t.predicate)
        ));
    }
    for g in program.globals.iter().filter(|g| g.module.is_some()) {
        parts.push(format!(
            "g{:?}/{}:{:?}|{}|{:?}",
            g.module, g.name, g.ty, g.mutable as u8, g.init
        ));
    }
    for p in &program.protocols {
        parts.push(format!(
            "p{:?}/{}<{:?}>={}",
            p.module,
            p.name,
            p.assoc,
            unplaced(&p.methods)
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
        let none = HashSet::new();
        assert!(views(crate::project::AT, &none));
        // Every engine copies `bytes`, so its result is owned.
        assert!(!views("bytes", &none), "a copy is not a view");
        assert!(!views("stringFromBytes", &none), "its inverse allocates");
    }
}
