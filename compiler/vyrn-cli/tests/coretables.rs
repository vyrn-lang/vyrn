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
//! A third is pinned here since M6's third judgment took its third slice: every
//! `Rhs` in the core names the type its node produces, and none is an
//! exception. That is what lets the typed judgment ask what produced a value
//! rather than counting the store as unjudged.
//!
//! One is not pinned, and the reason is the finding. `St::Switch::consuming`
//! is not the plan's `consuming_matches`: it is the whole disjunction the
//! emitter computes in `frees_boxes` — a `consume`, a scrutinee that names
//! no place, or the table — narrowed to an owned scrutinee with no placed
//! release after the construct. The two answer different questions, so only
//! the count of sites the core calls consuming is recorded here.
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

        // The other direction, over the sites the core STATES an answer for
        // (a `match`; an `if let` or a `?` arm states none and the emitter
        // keeps reading the plan there): a row the plan placed and the core
        // leaves out is a site the flipped emitter would stop freeing at.
        for ((site, arm), rows) in own.plan.arm_frees.iter() {
            let Some(core_says) = facts.arms.get(&(*site, *arm)) else {
                continue;
            };
            for (n, _, h) in rows {
                if !core_says.iter().any(|(cn, ch)| cn == n && ch == h) {
                    diffs.push(format!(
                        "{file}: site {site} arm {arm}: arm_frees:                          the plan frees `{n}` {h:?} and the core does not"
                    ));
                }
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

        for inst in &lowered.instances {
            let Ok(top) = vyrn_lower::core::build(&program, inst, &own) else {
                continue;
            };
            let mut sites = Vec::new();
            for body in top.frames() {
                consuming(&body.stmts, &mut sites);
            }
            *counted.entry("consuming_matches").or_default() += sites.len();
            for body in top.frames() {
                producers(&body.stmts, &mut produced);
            }
            // No pin: `St::Switch::consuming` is a different rule from the
            // plan's table (see the head of this file), so the count alone
            // is recorded.
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
    for d in diffs.iter().take(400) {
        eprintln!("  DIFF {d}");
    }
    assert!(
        diffs.is_empty(),
        "{} sites where the core and the plan disagree",
        diffs.len()
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
