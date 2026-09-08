//! RFC-0125 §3 M3, the endgame: the emitter's two walks, compared byte for
//! byte over the corpus.
//!
//! §2.3 says "the emitter reads the core and writes wasm ... it decides
//! nothing". The census before this one counted what the emitter reads and
//! named the blocker — the core stated no OPERATION — and the operation slice
//! wrote the three rows. What was left was a DRIVER: a walk over
//! [`vyrn_lower::core::Body`]'s statements beside the walk over the AST, and
//! `direct.rs` has one now (`Fn_::core_body`).
//!
//! This is its licence and its count in one test. Every corpus program is
//! emitted twice — once with the driver, once with `VYRN_NO_CORE_WALK=1`,
//! which takes the AST walk back — and the two modules are compared. What
//! differs is listed rather than asserted away, because a difference is a
//! finding the RFC's record has to explain: either the AST arm was wrong, or
//! the core states a shape the AST arm did not.
//!
//! The count beside it is what the driver reaches: how many of the corpus's
//! bodies the core's rows carry end to end, out of how many the emitter
//! lowers. The classification below says what each of the rest waits on, and
//! it is the same list §3 M3 records.

use std::path::PathBuf;
use vyrn_frontend::ast::Program;
use vyrn_lower::core::{Body, Callee, Op, Place, Rhs, St, Val};

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

/// What a body waits on before the core's rows could carry it, hardest first.
///
/// A body's class is the HARDEST thing in it, so the counts partition the
/// corpus and the ranked list in §3 M3 reads straight off them.
const CLASSES: [&str; 8] = [
    "the row names no value (`Val::Lit(Opaque)`)",
    "a lambda body the row does not carry (`Op::Closure`)",
    "a tag the arm does not carry (`St::Switch`)",
    "a layout: an aggregate made, read or taken",
    "a callee that is no declared function of the program (`Rhs::Call`)",
    "a release the driver must place (`St::Drop`, `St::Row`)",
    "an `&&` or `||`: a prim row for a branch the emitter writes",
    "nothing: the rows carry it",
];

/// The types `Fn_::core_walkable` admits a name of, spelled here so the count
/// beside each class is the emitter's own screen and not a second rule.
fn scalar(t: &vyrn_frontend::ast::Type) -> bool {
    use vyrn_frontend::ast::Type;
    matches!(
        t,
        Type::Int | Type::IntN { .. } | Type::Float | Type::Float32 | Type::Bool
    )
}

fn class_of(body: &Body) -> usize {
    let mut worst = CLASSES.len() - 1;
    let mut note = |c: usize| worst = worst.min(c);
    walk(&body.stmts, &mut note);
    worst
}

fn walk(ss: &[St], note: &mut impl FnMut(usize)) {
    for s in ss {
        match s {
            St::Let(_, r) => rhs(r, note),
            St::Store { place, value, .. } => {
                at(place, note);
                val(value, note);
            }
            St::Drop(..) | St::Row { .. } => note(5),
            St::If {
                cond, then, els, ..
            } => {
                val(cond, note);
                walk(then, note);
                walk(els, note);
            }
            // Since the loop slice the exit is the row's: the pass makes up
            // the two-way branch and the `break` at the head of the loop it
            // desugared, and the walk emits wasm's conditional branch for it.
            St::Loop(b) => walk(b, note),
            St::Block { body, .. } => walk(body, note),
            St::Break { .. } | St::Continue { .. } | St::Trap => {}
            St::Return { value, .. } => {
                if let Some(v) = value {
                    val(v, note);
                }
            }
            St::Switch { on, arms, .. } => {
                note(2);
                val(on, note);
                for a in arms {
                    walk(&a.body, note);
                }
            }
            St::Do(r, _) => rhs(r, note),
        }
    }
}

fn rhs(r: &Rhs, note: &mut impl FnMut(usize)) {
    match r {
        Rhs::Val(v) => val(v, note),
        Rhs::Read(p) | Rhs::Take(p) => {
            note(3);
            at(p, note);
        }
        // Since the callee slice the row says WHO: a function this program
        // declares is one the emitter's own table answers for, and only the
        // other eight kinds — and a `spawn`, and a write-back — are still
        // waiting on a row.
        Rhs::Call {
            args,
            kind,
            spawn,
            write_back,
            ..
        } => {
            if *kind != Callee::Fn || *spawn || *write_back {
                note(4);
            }
            for (v, _) in args {
                val(v, note);
            }
        }
        Rhs::Prim(
            Op::Bin(vyrn_frontend::ast::BinOp::And | vyrn_frontend::ast::BinOp::Or),
            vs,
            _,
        ) => {
            note(6);
            for v in vs {
                val(v, note);
            }
        }
        Rhs::Prim(Op::Closure, vs, _) => {
            note(1);
            for v in vs {
                val(v, note);
            }
        }
        Rhs::Prim(_, vs, _) => {
            for v in vs {
                val(v, note);
            }
        }
        Rhs::Make(_, vs) => {
            note(3);
            for v in vs {
                val(v, note);
            }
        }
    }
}

fn val(v: &Val, note: &mut impl FnMut(usize)) {
    if matches!(v, Val::Lit(vyrn_lower::core::Lit::Opaque)) {
        note(0);
    }
}

fn at(p: &Place, note: &mut impl FnMut(usize)) {
    match p {
        Place::Name(_) | Place::Global(_) => {}
        Place::Field(b, _) => at(b, note),
        Place::Elem(b, i) => {
            at(b, note);
            val(i, note);
        }
        Place::Key(b, k) => {
            at(b, note);
            val(k, note);
        }
    }
}

#[test]
#[ignore = "walks the whole corpus; run explicitly: cargo test -p vyrn-cli --test coredrive -- --ignored"]
fn the_two_walks_emit_the_same_wasm() {
    std::thread::Builder::new()
        .stack_size(vyrn_frontend::trap::DEEP_STACK_BYTES)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    vyrn_genwasm::install();
    vyrn_lower::install();
    let mut classes = [0usize; CLASSES.len()];
    // Beside each class, how many of its bodies name ONLY the scalar types the
    // driver's own screen admits — RFC-0125 §3 M3, the loop slice's finding.
    // A class's count says what the CORE still owes; this says what writing
    // that row would buy today, because a body the emitter's screen refuses is
    // one the row cannot reach. The two numbers ranked the list differently:
    // the tag on `Arm` is 792 bodies and none of them, because a scrutinee is
    // an enum and an enum is not a scalar.
    let mut scalars = [0usize; CLASSES.len()];
    let mut carried: std::collections::BTreeSet<String> = Default::default();
    let mut bodies = 0usize;
    let mut programs = 0usize;
    let mut from_core = 0usize;
    let mut emitted = 0usize;
    let mut differ: Vec<String> = Vec::new();
    let mut same = 0usize;
    for path in corpus() {
        let Ok(program) = load(&path) else { continue };
        programs += 1;
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        {
            let _memo = vyrn_frontend::project::Memo::open();
            let lowered = vyrn_lower::lower(&program);
            let own = vyrn_frontend::own::analyze(&program);
            for inst in &lowered.instances {
                let Ok(top) = vyrn_lower::core::build(&program, inst, &own) else {
                    continue;
                };
                for body in top.frames() {
                    bodies += 1;
                    let c = class_of(body);
                    classes[c] += 1;
                    if body.names.iter().all(|i| scalar(&i.ty)) {
                        scalars[c] += 1;
                    }
                    if c == CLASSES.len() - 1 {
                        carried.insert(body.name.clone());
                    }
                }
            }
        }

        // The two walks over the same program. The lowering is re-run for each
        // so the core's own side tables are the ones that emit answers.
        let core = emit(&program, false);
        let (f, e) = vyrn_codegen::direct::walks();
        from_core += f;
        emitted += e;
        let ast = emit(&program, true);
        match (core, ast) {
            (Ok(a), Ok(b)) if a == b => same += 1,
            (Ok(a), Ok(b)) => differ.push(format!(
                "{name}: {} bytes from the core, {} from the AST",
                a.len(),
                b.len()
            )),
            (Err(a), Err(b)) if a == b => same += 1,
            (a, b) => differ.push(format!("{name}: {a:?} against {b:?}")),
        }
    }
    eprintln!("{programs} programs, {bodies} bodies");
    eprintln!("what a body waits on before the core's rows could carry it:");
    for (i, what) in CLASSES.iter().enumerate() {
        eprintln!("  {:6}  {:6} scalar-only  {what}", classes[i], scalars[i]);
    }
    eprintln!(
        "  {} distinct bodies the rows carry end to end",
        carried.len()
    );
    eprintln!("the emitter took the core's walk for {from_core} of {emitted} bodies");
    eprintln!("{same} of {programs} programs emit the same module either way");
    for d in &differ {
        eprintln!("  {d}");
    }
    // The driver is a screen and not a judgement: where it stands down, the
    // AST walk emits exactly what it did. So a body it takes has to reach the
    // corpus at all, or this test measures nothing.
    assert!(from_core > 0, "the core walk emitted no body");
    // The licence. Three programs emit a different module, all for ONE shape,
    // and RFC-0125 §3 M3's record explains it: `return if c { a } else { b }`
    // reaches the AST walk as a join it writes with a typed `if` and one
    // branch out, and the core rewrites it into a `return` per arm
    // (`Builder::return_through`) so the linear judgment sees each exit. The
    // driver emits what the row says, which is one `br` per arm where the join
    // had one after the `if`. A FOURTH differing program is a shape nobody has
    // read, and this is where a reader is told to read it.
    let named: Vec<&str> = differ
        .iter()
        .map(|d| d.split(':').next().unwrap())
        .collect();
    assert_eq!(
        named,
        ["ifexpr.vyrn", "knucleotide.vyrn", "strpredbytes.vyrn"],
        "the two walks differ somewhere the record does not explain"
    );
}

fn emit(program: &Program, ast: bool) -> Result<Vec<u8>, String> {
    if ast {
        std::env::set_var("VYRN_NO_CORE_WALK", "1");
    } else {
        std::env::remove_var("VYRN_NO_CORE_WALK");
    }
    let _memo = vyrn_frontend::project::Memo::open();
    let _lowered = vyrn_lower::lower(program);
    vyrn_codegen::direct::forget_walks();
    let out = vyrn_codegen::direct::compile(program);
    std::env::remove_var("VYRN_NO_CORE_WALK");
    out
}
