//! RFC-0125 §3 M3: the tables the emitters read, counted over the whole
//! corpus.
//!
//! The direct wasm emitter used to read the plan's per-node tables
//! (`compiler/vyrn-codegen/src/direct.rs`) and this test diffed each against
//! the core's answer at the same site, so a table could not be flipped until
//! the two were proved equal. Every one of them is flipped now, and since
//! the emitter-reads-the-core-alone slice `own.rs` states NONE of them: the
//! last four — `receiver_frees`, `receiver_holes`, `receiver_malloc` and
//! `discarded_results` — went with the fallbacks that read them.
//!
//! So there is no second answer left to diff, and this test is a CENSUS: it
//! walks every corpus program, runs the analysis with the placer installed,
//! folds the core's side table (`vyrn_lower::core::facts`, out of every body
//! and every lambda frame after the placer has added its rows) and prints
//! how many rows of each kind the core states. The counts are what a later
//! slice reads to see a row appear or vanish. What a WRONG answer fails is
//! measurement — the residue ratchet and the memory suite — and the recorded
//! wasm hashes, which move when a release moves.
//!
//! One pin is left, and it is not about placement: every `Rhs` in the core
//! names the type its node produces, and none is an exception. That is what
//! lets the typed judgment ask what produced a value rather than counting
//! the store as unjudged (M6's third judgment, third slice).

use vyrn_frontend::loader::DiskResolver;

use std::collections::BTreeMap;
use std::path::PathBuf;
use vyrn_frontend::ast::Program;
use vyrn_lower::core::{Rhs, St};

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
    vyrn_frontend::load(&src, &root, &opts, &DiskResolver).map_err(|d| {
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
fn the_core_states_every_table_the_emitters_read() {
    // The frontend recurses deeply on a realistic program; the CLI runs it on
    // a thread with the interpreter's reserve, and so does this.
    std::thread::Builder::new()
        .stack_size(vyrn_frontend::trap::DEEP_STACK_BYTES)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    // A corpus example may import through a generator, and generation is the
    // DRIVER's engine rather than the frontend's (RFC-0125 §3 M5). Without this
    // those examples fail to link and the gate silently measures a smaller
    // corpus. Installing is idempotent.
    vyrn_genwasm::install();
    vyrn_lower::install();
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

        // RFC-0125 §3 M3, the derivation slice: `own.rs` states no arm
        // table any more, so there is nothing left to diff here. What the
        // core frees is counted, and the ratchet (`residue`) and the memory
        // suite are what a wrong answer fails.
        for core_says in facts.arms.values() {
            *counted.entry("arm_frees").or_default() += 1;
            *counted.entry("arm binders freed").or_default() += core_says.len();
        }

        // RFC-0125 §3 M3, the store slice: `own.rs` states no store table any
        // more, so there is nothing left to diff here either. The core's
        // answer is the only one, and what a wrong answer fails is the
        // residue ratchet, the parity harness and the memory suite, which
        // measure. Counted, like `arm_frees` and `edge_releases`.
        for (_, core_says) in &facts.stores {
            *counted.entry("store_owned").or_default() += 1;
            if *core_says {
                *counted.entry("stores that release").or_default() += 1;
            }
        }
        *counted.entry("stores the core stands down at").or_default() += facts.stood_down.len();
        // The same again since the emitter-reads-the-core-alone slice:
        // `own.rs` states no discarded table, so the core's row is counted
        // rather than diffed.
        *counted.entry("discarded_results").or_default() += facts.discarded.len();

        // RFC-0125 §3 M3, the last table's slice: `own.rs` states no
        // argument table any more, so there is nothing left to diff. The two
        // counts this was pinned at said the equality had been read — 548
        // rows the core alone stated, and ONE the plan alone did — and both
        // sides were read at the source before the table went. The core was
        // right both times; the RFC's record says why. Counted, like every
        // other derived table, and a wrong answer fails the residue ratchet,
        // the parity harness and the memory suite, which measure.
        *counted.entry("arg_drops").or_default() += facts.arg_drops.len();

        // RFC-0125 §3 M3, the derivation slice: Rule N's rows are the
        // kernel's `equalize` and nothing else, so there is no second answer
        // to diff. Counted; a wrong one fails the ratchet and the memory
        // suite.
        for rows in facts.edges.values() {
            *counted.entry("edge_releases").or_default() += 1;
            *counted.entry("edge rows").or_default() += rows.len();
        }

        // R1′ and row 11b, since the emitter-reads-the-core-alone slice:
        // `own.rs` states no receiver table and no producer screen any more,
        // so there is no second answer to diff. The counts stay, because the
        // corpus totals are what a later slice reads to see a row appear or
        // vanish; what a wrong answer fails is the residue ratchet and the
        // memory suite, which measure.
        for (_node, core_holes) in &facts.receivers {
            *counted.entry("receiver_frees").or_default() += 1;
            *counted.entry("receiver holes").or_default() += core_holes.len();
        }
        *counted.entry("receiver_malloc").or_default() += facts.receiver_malloc.len();

        // RFC-0125 §3 M3, the third derivation slice: `St::Switch`'s
        // `consuming` is the core's own answer and `own.rs` states none, so
        // there is no second opinion to diff. Counted; what a wrong answer
        // fails is the ratchet (`residue`), the memory suite and parity.
        for (_site, took) in &facts.consuming {
            *counted.entry("switch sites").or_default() += 1;
            *counted.entry("consuming: taken").or_default() += usize::from(*took);
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
    eprintln!("the core's tables over the corpus: {programs} programs");
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
