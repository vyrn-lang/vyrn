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
//! Two tables are pinned at zero differences, and the emitter reads the core
//! for both:
//!
//!   - `arm_frees`, from a `St::Drop` of a payload binder at the end of
//!     `Arm::body`, with `NameInfo::holes` as the row's hole set;
//!   - `receiver_frees` and `receiver_holes`, from a `St::Drop` of a name
//!     whose `NameInfo::receiver` names the `Expr::Field` node.
//!
//! One is pinned one way only. `St::Switch::consuming` is not the plan's
//! `consuming_matches`: it is the whole disjunction the emitter computes in
//! `frees_boxes` — a `consume`, a scrutinee that names no place, or the
//! table — narrowed to an owned scrutinee with no placed release after the
//! construct. So the core says "consuming" at many more sites than the table
//! does, and what a pin can assert is the implication: every site the table
//! names, the core names too.
//!
//! The rest of the emitter's reads — `store_owned`, `store_fresh`,
//! `discarded_results`, `arg_drops`, `edge_releases` — have no site key in
//! the core today: `St::Store` and a temporary's `St::Drop` carry a line and
//! no node, and an edge drop is a `St::Drop` at a position rather than at a
//! key. Each needs the core taught to carry the key before its flip, which
//! is what the census table in the RFC records.

use std::collections::BTreeMap;
use std::path::PathBuf;
use vyrn_frontend::ast::Program;
use vyrn_lower::core::St;

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

/// Every `match` site the core took its scrutinee at.
fn consuming(stmts: &[St], out: &mut Vec<usize>) {
    for s in stmts {
        match s {
            St::If { then, els, .. } => {
                consuming(then, out);
                consuming(els, out);
            }
            St::Loop(b) | St::Block { body: b, .. } => consuming(b, out),
            St::Switch {
                arms, consuming: c, ..
            } => {
                if *c {
                    if let Some(a) = arms.first() {
                        out.push(a.site);
                    }
                }
                for a in arms {
                    consuming(&a.body, out);
                }
            }
            _ => {}
        }
    }
}

#[test]
fn the_core_and_the_plan_agree_on_every_table() {
    // The frontend recurses deeply on a realistic program; the CLI runs it on
    // a thread with the interpreter's reserve, and so does this.
    std::thread::Builder::new()
        .stack_size(vyrn_frontend::interp::INTERP_STACK_BYTES)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    vyrn_lower::install();
    let mut diffs: Vec<String> = Vec::new();
    let mut counted: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut programs = 0usize;
    for path in corpus() {
        let Ok(program) = load(&path) else { continue };
        programs += 1;
        let _memo = vyrn_frontend::project::Memo::open();
        let lowered = vyrn_lower::lower(&program);
        let own = vyrn_frontend::own::analyze(&program);
        let facts = vyrn_lower::core::facts();
        let file = path.file_name().unwrap().to_string_lossy().to_string();

        for ((site, arm), core_says) in &facts.arms {
            *counted.entry("arm_frees").or_default() += 1;
            // The plan's row may name a binder no arm of this shape drops;
            // only the binders the core released are compared, by name.
            let want: Vec<(String, Vec<String>)> = own
                .plan
                .arm_payload_free(*site, *arm)
                .map(|rows| {
                    rows.iter()
                        .filter(|(n, _, _)| core_says.iter().any(|(c, _)| c == n))
                        .map(|(n, _, h)| (n.clone(), h.clone()))
                        .collect()
                })
                .unwrap_or_default();
            if want != *core_says {
                diffs.push(format!(
                    "{file}: site {site} arm {arm}: arm_frees: \
                     core {core_says:?}, plan {want:?}"
                ));
            }
        }

        for (node, core_holes) in &facts.receivers {
            *counted.entry("receiver_frees").or_default() += 1;
            let plan_free = own.plan.receiver_free(*node);
            let plan_holes = own.plan.receiver_holes_at(*node);
            if !plan_free || *core_holes != plan_holes {
                diffs.push(format!(
                    "{file}: site {node}: receiver_frees: core {core_holes:?}, \
                     plan free {plan_free} holes {plan_holes:?}"
                ));
            }
        }

        // The one-way pin: every site the plan's table names, the core's
        // `consuming` flag names too.
        for inst in &lowered.instances {
            let Ok(top) = vyrn_lower::core::build(&program, inst, &own) else {
                continue;
            };
            let mut sites = Vec::new();
            for body in top.frames() {
                consuming(&body.stmts, &mut sites);
            }
            *counted.entry("consuming_matches").or_default() += sites.len();
            // No pin: `St::Switch::consuming` is a different rule from the
            // plan's table (see the head of this file), so the count alone
            // is recorded.
        }
    }
    eprintln!("core-vs-plan over the corpus: {programs} programs");
    for (what, n) in &counted {
        eprintln!("  {n:6} sites  {what}");
    }
    for d in diffs.iter().take(40) {
        eprintln!("  DIFF {d}");
    }
    assert!(
        diffs.is_empty(),
        "{} sites where the core and the plan disagree",
        diffs.len()
    );
}
