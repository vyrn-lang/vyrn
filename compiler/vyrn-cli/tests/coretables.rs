//! RFC-0125 §3 M3, the deletion-preparation slice: the census's "core carries
//! it" column, pinned by a diff over the whole corpus.
//!
//! The direct wasm emitter reads the plan's per-node tables
//! (`compiler/vyrn-codegen/src/direct.rs`). M3's end state is that it reads
//! the core instead, so those tables in `own.rs` can go. Before an emitter is
//! moved off a table, the two answers have to be proved equal — otherwise a
//! flip changes the emitted bytes and nobody knows which source was right.
//!
//! This test walks every corpus program, runs the analysis with the placer
//! installed, and diffs the core's side table (`vyrn_lower::core::facts`,
//! folded out of every body and every lambda frame after the placer has
//! added its rows) against the plan's answer at the same site. A difference
//! is printed with program, function and site.
//!
//! One table is pinned at zero differences, and the emitter reads the core
//! for it: `receiver_frees` and `receiver_holes`, from a `St::Drop` of a name
//! whose `NameInfo::receiver` names the `Expr::Field` node.
//!
//! `arm_frees` is DERIVED and no longer diffed: `own.rs` states no arm table
//! since RFC-0125 §3 M3's derivation slice, so the kernel's answer is the
//! only one and this test counts it. What a wrong answer fails is the
//! residue ratchet and the memory suite, which measure.
//!
//! **The derivation slice (RFC-0125 §3 M3) changed what a difference MEANS
//! for a table the core derives.** While the core read the plan, a diff was a
//! filter of the plan's own set and could not disagree; and where the placer
//! had written the row, the core's answer was the plan's answer handed back.
//! A derived table has neither property, so the plan's side is the ANALYSIS's
//! own answer and a difference is a real second opinion. Each is read at the
//! source and its verdict recorded in the RFC; the direction that would free
//! twice stays pinned, and the direction where the core states what the
//! analysis alone does not is counted with the count pinned exactly, so a
//! new site is read rather than absorbed.
//!
//! `receiver_malloc` (row 11b) is pinned one way and counted the other: the
//! core states it from the producer it records beside that name, and a plan
//! row the core loses would stop a free inside a `region`, while the core
//! answering yes where the plan's spelling of the producer says no is
//! counted, at one site.
//!
//! A third is pinned since M6's third judgment took its third slice: every
//! `Rhs` in the core names the type its node produces, and none is an
//! exception. That is what lets the typed judgment ask what produced a value
//! rather than counting the store as unjudged.
//!
//! Four more are pinned since the emitter-reads-the-core slice, and each
//! needed the core taught to carry a key first ([`vyrn_lower::core::Site`]):
//!
//!   - `store_owned` and `store_fresh`, from a `St::Store` at the store
//!     statement's node — the core states the two as one answer, because
//!     both compiled backends read them as one;
//!   - `discarded_results`, from a `St::Drop` at the `Stmt::Expr`'s node;
//!   - `arg_drops`, from `NameInfo::arg_drop` on the name the argument
//!     bound;
//!   - `edge_releases`, from a `St::Drop` at a `Site::Edge` — DERIVED and no
//!     longer diffed since the derivation slice, for the reason `arm_frees`
//!     is not.
//!
//! The diff is structural in both directions: a plan row the core states
//! nothing for is a site a flipped emitter would stop releasing at, and a
//! core answer the plan does not have is one it would release twice.
//!
//! One is half COUNTED and half pinned. `St::Switch`'s `consuming` is not
//! the plan's `consuming_matches`: it is the whole disjunction the emitter
//! computes in `frees_boxes` — a `consume`, a scrutinee that names no place,
//! or the table — narrowed to an owned scrutinee with no placed release
//! after the construct. So the core says yes at sites the table does not
//! name, and that direction is counted; a site the plan calls consuming and
//! the core does not is a payload box the emitter would stop freeing, and
//! that direction is pinned (RFC-0125 §3 M3, row 14).

use std::collections::BTreeMap;
use std::path::PathBuf;
use vyrn_frontend::ast::Program;
use vyrn_lower::core::{Rhs, St};

struct Fs;

impl vyrn_frontend::loader::ModuleResolver for Fs {
    fn read(&self, resolved: &str) -> Result<String, String> {
        std::fs::read_to_string(resolved).map_err(|e| e.to_string())
    }
}

fn repo_root() -> PathBuf {
    let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    d.pop();
    d.pop();
    d
}

fn load(path: &std::path::Path) -> Result<Program, String> {
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let root = path.to_string_lossy().replace('\\', "/");
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(repo_root().join("std").to_string_lossy().replace('\\', "/")),
        ..Default::default()
    };
    vyrn_frontend::load(&src, &root, &opts, &Fs).map_err(|d| {
        d.first()
            .map(|d| d.render())
            .unwrap_or_else(|| "load failed".into())
    })
}

fn corpus() -> Vec<PathBuf> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(repo_root().join("examples"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no examples found");
    names
}

/// Every producer type the core carries, and every one it does not.
///
/// RFC-0125 §3 M6, the third judgment's third slice: `Rhs::Prim` and
/// `Rhs::Call` carry the type their node produces, from the checker's row.
/// `out` counts one row per right-hand side — the variant's name, and whether
/// it named a type — so the pin below is a diff over the corpus rather than a
/// claim about one program.
fn producers(stmts: &[St], out: &mut BTreeMap<(&'static str, bool), usize>) {
    for s in stmts {
        match s {
            St::Let(_, rhs) => {
                let row = match rhs {
                    Rhs::Prim(_, t) => ("prim", t.is_some()),
                    Rhs::Call { ret, .. } => ("call", ret.is_some()),
                    Rhs::Val(_) => ("val", true),
                    Rhs::Read(_) => ("read", true),
                    Rhs::Take(_) => ("take", true),
                    Rhs::Make(_) => ("make", true),
                };
                *out.entry(row).or_default() += 1;
            }
            St::If { then, els, .. } => {
                producers(then, out);
                producers(els, out);
            }
            St::Loop(b) | St::Block { body: b, .. } => producers(b, out),
            St::Switch { arms, .. } => {
                for a in arms {
                    producers(&a.body, out);
                }
            }
            _ => {}
        }
    }
}

#[test]
#[ignore = "walks the whole corpus; run explicitly: cargo test -p vyrn-cli --test coretables -- --ignored"]
fn the_core_and_the_plan_agree_on_every_table() {
    // The frontend recurses deeply on a realistic program; the CLI runs it on
    // a thread with the interpreter's reserve, and so does this.
    std::thread::Builder::new()
        .stack_size(vyrn_frontend::trap::INTERP_STACK_BYTES)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    vyrn_lower::install();
    let mut diffs: Vec<String> = Vec::new();
    let mut counted: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut produced: BTreeMap<(&'static str, bool), usize> = BTreeMap::new();
    let mut programs = 0usize;
    for path in corpus() {
        let Ok(program) = load(&path) else { continue };
        programs += 1;
        let _memo = vyrn_frontend::project::Memo::open();
        let lowered = vyrn_lower::lower(&program);
        let own = vyrn_frontend::own::analyze(&program);
        let facts = vyrn_lower::core::facts().expect("the placer fills the core's facts");
        let file = path.file_name().unwrap().to_string_lossy().to_string();
        // The plan holds a row for every function the checker walked; the core
        // holds one for every function the lowering instantiated. A row in a
        // function nothing instantiated is nobody's to state, exactly as
        // `ReleasePlan::unconsumed` skips one in a function nothing emitted.
        let built: std::collections::HashSet<String> = lowered
            .instances
            .iter()
            .map(|i| i.func.name.clone())
            .collect();
        let owner = |at: &usize| own.plan.owners.get(at).cloned().unwrap_or_default();
        let reached = |at: &usize| {
            own.plan
                .owners
                .get(at)
                .is_some_and(|f| built.contains(f) || f.is_empty())
        };

        // RFC-0125 §3 M3, the derivation slice: `own.rs` states no arm
        // table any more, so there is nothing left to diff here. What the
        // core frees is counted, and the ratchet (`residue`) and the memory
        // suite are what a wrong answer fails.
        for core_says in facts.arms.values() {
            *counted.entry("arm_frees").or_default() += 1;
            *counted.entry("arm binders freed").or_default() += core_says.len();
        }

        // The plan's own totals, so the census can be read off this test.
        for (what, n) in [
            (
                "store_owned (plan)",
                own.plan.store_owned.iter().filter(|a| reached(a)).count(),
            ),
            (
                "discarded_results (plan)",
                own.plan
                    .discarded_results
                    .iter()
                    .filter(|a| reached(a))
                    .count(),
            ),
            (
                "arg_drops (plan)",
                own.plan.arg_drops.iter().filter(|a| reached(a)).count(),
            ),
        ] {
            *counted.entry(what).or_default() += n;
        }

        for (at, core_says) in &facts.stores {
            *counted.entry("store_owned").or_default() += 1;
            // The core's answer is the whole conjunction both compiled
            // backends compute, so only its `true` implies the plan's row.
            if *core_says && !own.plan.store_owned.contains(at) {
                diffs.push(format!(
                    "{file}: site {at}: store_owned: the core releases the old                      value and the plan does not"
                ));
            }
        }
        // A plan store row the core states no answer for. Every one in the
        // corpus stands on a statement RFC-0091 M2's `place at` rewrite
        // BUILT: a user container's `c[h] = v` is checked on the rewritten
        // block, and this pass walks the source statement. A reader falls
        // back to the plan at such a site, so the count is pinned here
        // rather than diffed — a thirteenth would be a site nobody looked
        // at.
        *counted.entry("store rows left to the plan").or_default() += own
            .plan
            .store_owned
            .iter()
            .filter(|a| reached(a) && !facts.stores.contains_key(*a))
            .count();

        for at in &facts.discarded {
            *counted.entry("discarded_results").or_default() += 1;
            if !own.plan.discarded_results.contains(at) {
                diffs.push(format!(
                    "{file}: site {at}: discarded_results: core yes, plan no"
                ));
            }
        }
        for at in own.plan.discarded_results.iter().filter(|a| reached(a)) {
            if !facts.discarded.contains(at) {
                diffs.push(format!(
                    "{file}: site {at}: discarded_results: plan yes, core no"
                ));
            }
        }

        for at in &facts.arg_drops {
            *counted.entry("arg_drops").or_default() += 1;
            if !own.plan.arg_drops.contains(at) {
                diffs.push(format!("{file}: site {at}: arg_drops: core yes, plan no"));
            }
        }
        for at in own.plan.arg_drops.iter().filter(|a| reached(a)) {
            if !facts.arg_drops.contains(at) {
                diffs.push(format!(
                    "{file}: site {at}: arg_drops: plan yes, core no (fn {})",
                    owner(at)
                ));
            }
        }

        // RFC-0125 §3 M3, the derivation slice: Rule N's rows are the
        // kernel's `equalize` and nothing else, so there is no second answer
        // to diff. Counted; a wrong one fails the ratchet and the memory
        // suite.
        for rows in facts.edges.values() {
            *counted.entry("edge_releases").or_default() += 1;
            *counted.entry("edge rows").or_default() += rows.len();
        }

        for (node, core_holes) in &facts.receivers {
            *counted.entry("receiver_frees").or_default() += 1;
            let plan_free = own.plan.receiver_free(*node);
            let plan_holes = own.plan.receiver_holes_at(*node);
            // A hole the PLAN names and the core does not is a place a take
            // already gave an owner, released twice: pinned. The other way
            // round the core frees LESS than the row asks, which is the
            // direction the derivation was built to correct, so it is
            // counted with the free itself below.
            if plan_free && plan_holes.iter().any(|h| !core_holes.contains(h)) {
                diffs.push(format!(
                    "{file}: site {node}: receiver_frees: core {core_holes:?}, \
                     plan free {plan_free} holes {plan_holes:?}"
                ));
            }
            // The core derives this row since the derivation slice, so the
            // plan's is the ANALYSIS's own answer and no longer the core's
            // fed back through the placer. Where the two differ the core is
            // the reader every engine has, and the count is pinned so a
            // third site is read rather than absorbed.
            if !plan_free || *core_holes != plan_holes {
                *counted.entry("receiver frees: core only").or_default() += 1;
            }
            // Row 11b, the region stand-down: whether a CALLEE allocated the
            // block. The emitter asks its own region depth beside it. The
            // plan losing a row here would stop a free inside a `region`,
            // so that direction is pinned; the core saying yes where the
            // plan's SPELLING of the producer says no is counted. It was
            // one — `gqlParseQuery(query).sels`, whose producer the analysis
            // spells `@fieldof:gqlParseQuery` and screens out with the
            // arena's own `@` names, though a callee allocated it — and the
            // reach slice adds thirteen receivers in bodies outside a
            // function, which the analysis states nothing about at all.
            let core_malloc = facts.receiver_malloc.contains(node);
            let plan_malloc = own.plan.receiver_malloc_at(*node);
            if plan_malloc && !core_malloc {
                diffs.push(format!(
                    "{file}: site {node} in {}: receiver_malloc: plan yes, core no",
                    owner(node)
                ));
            }
            *counted.entry("receiver_malloc: core only").or_default() +=
                usize::from(core_malloc && !plan_malloc);
        }

        // The other direction, added by the derivation slice: a receiver the
        // ANALYSIS frees and the core states no R1′ row for. Every engine
        // reads "no free" out of a missing key here, so the row stands for
        // nothing — which is the close-out's own finding about a HEAP field
        // read: the receiver of `gqlSplitDecl(src).rhs.startsWith("{")` must
        // outlive the consumer, so its free is the argument-temporary drop
        // keyed by the producer and not this row. Counted, and the count is
        // pinned, so a second such row is read at the source.
        for node in own.plan.receiver_frees.iter().filter(|a| reached(a)) {
            if !facts.receivers.contains_key(node) {
                *counted
                    .entry("receiver rows the core states nothing for")
                    .or_default() += 1;
            }
        }

        // `St::Switch`'s `consuming` is a WIDER rule than the plan's
        // `consuming_matches`: it is the whole disjunction the emitter
        // computes, a `consume` or a scrutinee that names no place
        // included, so the core says yes at sites the table does not name
        // and that direction is COUNTED. The other direction is PINNED
        // (RFC-0125 §3 M3, row 14): a site the plan calls consuming and the
        // core does not is a payload box the flipped emitter would stop
        // freeing, and six such sites are what kept this row on the plan
        // until the core stated a scrutinee's ownership apart from the
        // decision it feeds.
        for (site, took) in &facts.consuming {
            *counted.entry("switch sites").or_default() += 1;
            if *took {
                *counted.entry("consuming: core only").or_default() +=
                    usize::from(!own.plan.consuming_matches.contains(site));
            } else if own.plan.consuming_matches.contains(site) {
                diffs.push(format!(
                    "{file}: site {site}: consuming: the plan took the                      scrutinee and the core did not (in {})",
                    owner(site)
                ));
            }
        }

        // RFC-0125 §3 M6, the third judgment's third slice: every right-hand
        // side of every instance, counted by whether it names its producer
        // type. The build is the placer's own, re-run here because the fold
        // keeps no `Rhs`.
        for inst in &lowered.instances {
            let Ok(top) = vyrn_lower::core::build(&program, inst, &own) else {
                continue;
            };
            for body in top.frames() {
                producers(&body.stmts, &mut produced);
            }
        }
    }
    eprintln!("core-vs-plan over the corpus: {programs} programs");
    for (what, n) in &counted {
        eprintln!("  {n:6} sites  {what}");
    }
    eprintln!("producer types over the corpus:");
    for ((what, typed), n) in &produced {
        eprintln!(
            "  {n:6} {what}  {}",
            if *typed { "typed" } else { "UNTYPED" }
        );
    }
    // Grouped: the class first, then a handful of each, so a run that finds
    // thousands of one shape is still readable.
    let mut classes: BTreeMap<String, Vec<&String>> = BTreeMap::new();
    for d in &diffs {
        let class = d.split(": ").skip(2).take(2).collect::<Vec<_>>().join(": ");
        classes.entry(class).or_default().push(d);
    }
    for (class, ds) in &classes {
        eprintln!("  {:6} DIFF {class}", ds.len());
        for d in ds.iter().take(40) {
            eprintln!("           {d}");
        }
    }
    assert!(
        diffs.is_empty(),
        "{} sites where the core and the plan disagree",
        diffs.len()
    );
    assert_eq!(
        counted
            .get("receiver_malloc: core only")
            .copied()
            .unwrap_or(0),
        14,
        "`gqlParseQuery(query).sels` and the thirteen bodies outside a function"
    );
    // RFC-0125 §3 M3, the derivation slice: the two sites in `std/graphql`
    // the analysis alone does not state — `gqlParseQuery(query).sels` in
    // `gqlTestProject`, whose row the placer used to write, and
    // `gqlSplitDecl(t.source).name` in `sdl`, whose row the analysis writes
    // without the hole the binding's take leaves. Both were the placer's
    // answer through the plan before, so no emitted byte moves.
    //
    // The reach slice adds twelve, one per corpus file whose `test` or
    // `bench` body reads a heap field off a temporary: `enumarray`,
    // `enumcodec`, `graphql`, `jsoncodec`, `jsondecbytes`, `jsonplace`,
    // `mapdemo`, `membench`, `rest`, `storage`, `vlog`, `wirekey`. The core
    // reaches those bodies now and states the row the analysis never wrote
    // there, so a compiled `vyrn test` frees a receiver it used to keep.
    assert_eq!(
        counted
            .get("receiver frees: core only")
            .copied()
            .unwrap_or(0),
        14,
        "the two `std/graphql` receivers and the twelve bodies outside a function"
    );
    assert_eq!(
        counted
            .get("receiver rows the core states nothing for")
            .copied()
            .unwrap_or(0),
        1,
        "`gqlSplitDecl(src).rhs` in `gqlIsRecord`, and nothing else"
    );
    assert_eq!(
        counted
            .get("store rows left to the plan")
            .copied()
            .unwrap_or(0),
        12,
        "the `place at` rewrite's own store statements, and nothing else"
    );
    // The producer-type pin (RFC-0125 §3 M6, the third judgment's third
    // slice): every `Rhs` in the corpus names the type its node produces.
    // There is no exception list, because there is no exception: a node the
    // checker typed answers from its row, and the one class with no row of
    // its own — a projection the checker expanded at the site (RFC-0122) —
    // answers from its declared result under the receiver's type arguments.
    // A right-hand side that stopped naming a type would make the typed
    // judgment count a store as unjudged instead of judging it.
    let untyped: usize = produced
        .iter()
        .filter(|((_, typed), _)| !typed)
        .map(|(_, n)| *n)
        .sum();
    assert_eq!(untyped, 0, "right-hand sides with no producer type");
}
