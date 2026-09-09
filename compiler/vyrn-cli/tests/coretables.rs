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
use vyrn_lower::core::{Ctor, Lit, Rhs, St, Val};

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

/// The core's own hole set for the `let` named `binding` in `main`.
///
/// RFC-0125 §3 M3, the input-circle slice: a `consume p` states the hole
/// where the take is written ([`vyrn_lower::core`]'s `take_place_at`), so
/// the set is the core's own answer rather than a table it reads back out of
/// `own.rs`. These three assertions were `own.rs`'s until the plan's copy
/// lost its last reader.
fn core_holes(src: &str, binding: &str) -> Vec<String> {
    vyrn_lower::install();
    let program = vyrn_frontend::load(src, "holes.vyrn", &Default::default(), &DiskResolver)
        .unwrap_or_else(|d| panic!("{}", d.first().map(|d| d.render()).unwrap_or_default()));
    let _memo = vyrn_frontend::project::Memo::open();
    let lowered = vyrn_lower::lower(&program);
    let own = vyrn_frontend::own::analyze(&program);
    let inst = lowered
        .instances
        .iter()
        .find(|i| i.func.name == "main")
        .expect("main is lowered");
    let top = vyrn_lower::core::build(&program, inst, &own).expect("main builds");
    top.names
        .iter()
        .find(|n| n.bound_by_let && n.source == binding)
        .unwrap_or_else(|| panic!("no `let {binding}` in main"))
        .holes
        .clone()
}

/// Whether the core says the frame OWES a release on the binding named
/// `binding` in `which` — [`vyrn_lower::core::NameInfo`]'s `releases`.
///
/// RFC-0125 §3 M3, the container slice: `movecheck`'s row table said this
/// with a `Gone` verdict, and three of its tests asserted the verdict. The
/// table is deleted and the core states the same fact on the name, so the
/// assertions moved here, where a built body can be seen.
fn core_releases(src: &str, which: &str, binding: &str) -> bool {
    vyrn_lower::install();
    let program = vyrn_frontend::load(src, "owns.vyrn", &Default::default(), &DiskResolver)
        .unwrap_or_else(|d| panic!("{}", d.first().map(|d| d.render()).unwrap_or_default()));
    let _memo = vyrn_frontend::project::Memo::open();
    let lowered = vyrn_lower::lower(&program);
    let own = vyrn_frontend::own::analyze(&program);
    let inst = lowered
        .instances
        .iter()
        .find(|i| i.func.name == which)
        .unwrap_or_else(|| panic!("`{which}` is lowered"));
    let top = vyrn_lower::core::build(&program, inst, &own).expect("the body builds");
    top.frames()
        .iter()
        .flat_map(|f| f.names.iter())
        .find(|n| n.source == binding)
        .unwrap_or_else(|| panic!("no `{binding}` in `{which}`"))
        .releases
}

/// RFC-0121: a refutable `let`'s binder is the PAYLOAD of the scrutinee's
/// place — a borrow, never a fresh owner. Where the scrutinee is module
/// state, an owned binder freed the global's buffer at block exit, out from
/// under the next read.
#[test]
fn a_payload_binding_from_module_state_owes_no_release() {
    let src = "type E = | Tag(Array<Int64>) | Blank
               let mut g: E = Blank
               fn main() -> Int64 {
                   let Tag(xs) = g
                   return xs.length
               }";
    assert!(
        !core_releases(src, "main", "xs"),
        "a payload read out of module state is the global's, not this frame's"
    );
}

/// An `if let` over a PARAMETER matches the caller's value, so this frame
/// owes nothing for it; over a call result it owes the release.
/// `examples/argsdemo.vyrn` fed the first shape an `args()` element and CI's
/// parity job went red for twenty-four runs.
#[test]
fn an_if_let_over_a_parameter_owes_no_release() {
    let param = "fn show(v: Option<String>) -> Int64 {                  if let Some(s) = v { return s.byteLength } return 0 }                  fn main() -> Int64 { return show(Some(\"a\" + \"b\")) }";
    assert!(
        !core_releases(param, "show", "s"),
        "a parameter is the caller's, so the `if let` over one owes no release"
    );
    let tmp = "fn maybe() -> Option<String> { return Some(\"a\" + \"b\") }                fn main() -> Int64 { if let Some(s) = maybe() { return s.byteLength }                return 0 }";
    assert!(
        core_releases(tmp, "main", "s"),
        "an `if let` over a call result owns what it matched"
    );
}

/// A lender forwarded through an AGGREGATE is still a lender: `calls_in` has
/// to see through the literal, or the wrapper goes unmarked and its caller
/// reclaims storage the lender's own caller still owns.
#[test]
fn a_lender_forwarded_through_an_aggregate_lends_still() {
    // The refusal is the kernel's, reached through the slot `install` fills;
    // nextest runs each test in its own process, so this one installs it too.
    vyrn_lower::install();
    let src = "type R = { name: String }                fn pick(xs: Array<String>) -> String                { for x in xs { return x } return \"\" }                fn g(a: Array<String>) -> R { return R { name: pick(a) } }                fn main() -> Int64 { let arr: Array<String> = [\"a\" + \"b\"]                let r = g(arr) return r.name.byteLength }";
    let program = vyrn_frontend::load(src, "lend.vyrn", &Default::default(), &DiskResolver);
    assert!(
        program.is_err(),
        "a loop variable may not be returned, so `pick` is refused before it can lend"
    );
}

/// A take of one field leaves one hole, and it is spelled relative to the
/// binding.
#[test]
fn a_taken_field_is_the_only_place_the_walk_skips() {
    let src = "type Doc = { title: String, body: String }                fn mk(a: String) -> Doc { return Doc { title: a + \"t\", body: a + \"b\" } }                fn main() -> Int64 { let d = mk(\"x\"); let t = consume d.title;                return Int64(t.byteLength) + Int64(d.body.byteLength); }";
    assert_eq!(core_holes(src, "d"), vec![".title".to_string()]);
}

/// The hole is a SET. `std/vyx.vyrn:1431` drains nine fields out of one
/// record, so a second take must join the set rather than replace it.
#[test]
fn every_take_of_one_record_joins_the_hole_set() {
    let src = "type Doc = { title: String, body: String }                fn mk(a: String) -> Doc { return Doc { title: a + \"t\", body: a + \"b\" } }                fn main() -> Int64 { let d = mk(\"x\"); let t = consume d.title;                let b = consume d.body; return Int64(t.byteLength) + Int64(b.byteLength); }";
    assert_eq!(
        core_holes(src, "d"),
        vec![".body".to_string(), ".title".to_string()]
    );
}

/// The path may be more than one hop: `std/vyx.vyrn:4091` writes
/// `consume hs.head.err`.
#[test]
fn a_hole_can_be_a_chain_of_fields() {
    let src = "type Inner = { err: String, n: Int64 }                type Outer = { head: Inner, tail: String }                fn mk(a: String) -> Outer { return Outer { head: Inner { err: a + \"e\", n: 1 }, tail: a + \"l\" } }                fn main() -> Int64 { let hs = mk(\"y\"); let e = consume hs.head.err;                return Int64(e.byteLength) + Int64(hs.tail.byteLength); }";
    assert_eq!(core_holes(src, "hs"), vec![".head.err".to_string()]);
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
fn producers(
    stmts: &[St],
    out: &mut BTreeMap<(&'static str, bool), usize>,
    ops: &mut BTreeMap<String, usize>,
) {
    for s in stmts {
        match s {
            St::Let(_, rhs) => {
                let row = match rhs {
                    Rhs::Prim(_, _, t) => ("prim", t.is_some()),
                    Rhs::Call { ret, .. } => ("call", ret.is_some()),
                    Rhs::Val(_) => ("val", true),
                    Rhs::Read(_) => ("read", true),
                    Rhs::Take(_) => ("take", true),
                    Rhs::Make(..) => ("make", true),
                };
                *out.entry(row).or_default() += 1;
                operation(rhs, ops);
            }
            St::Do { rhs, .. } => operation(rhs, ops),
            St::Store { value, .. } => literal(value, ops),
            St::Return { value: Some(v), .. } => literal(v, ops),
            St::Switch { on, arms, .. } => {
                literal(on, ops);
                for a in arms {
                    producers(&a.body, out, ops);
                }
            }
            St::If { then, els, .. } => {
                producers(then, out, ops);
                producers(els, out, ops);
            }
            St::Loop { body: b, .. } | St::Block { body: b, .. } => producers(b, out, ops),
            _ => {}
        }
    }
}

/// What each row SAYS IS COMPUTED, counted the same way (RFC-0125 §3 M3, the
/// operation slice).
///
/// The core stated who owns what and where control goes, and not what is
/// computed: `a + b` and `a - b` were one `prim` row, four literal forms were
/// one `make`, and `5` and `7` were one `lit`. These three columns are what
/// the rows gained, and a column that stops being filled is a row that
/// stopped stating its operation — the census reads the totals, exactly as it
/// reads the placement ones above.
fn operation(rhs: &Rhs, ops: &mut BTreeMap<String, usize>) {
    match rhs {
        Rhs::Prim(op, vs, _) => {
            *ops.entry(format!("prim {op:?}")).or_default() += 1;
            for v in vs {
                literal(v, ops);
            }
        }
        Rhs::Make(c, vs) => {
            let what = match c {
                Ctor::Record(..) => "record",
                Ctor::Array => "array",
                Ctor::Map => "map",
                Ctor::Try(_) => "try",
            };
            *ops.entry(format!("make {what}")).or_default() += 1;
            for v in vs {
                literal(v, ops);
            }
        }
        Rhs::Val(v) => literal(v, ops),
        Rhs::Call { args, .. } => {
            for (v, _) in args {
                literal(v, ops);
            }
        }
        Rhs::Read(_) | Rhs::Take(_) => {}
    }
}

fn literal(v: &Val, ops: &mut BTreeMap<String, usize>) {
    let Val::Lit(l) = v else { return };
    let what = match l {
        Lit::Int(_) => "int",
        Lit::Byte(_) => "byte",
        Lit::Float(_) => "float",
        Lit::Bool(_) => "bool",
        Lit::Str(_) => "string",
        Lit::Opaque => "opaque",
    };
    *ops.entry(format!("lit {what}")).or_default() += 1;
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
    let mut ops: BTreeMap<String, usize> = BTreeMap::new();
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
        // RFC-0125 §3 M3, the box slice: the wider question beside the take —
        // the construct switches on a value the frame MADE, so the boxes its
        // binders came out of are its own. It contains `consuming: taken` and
        // is not contained by it.
        *counted.entry("owns_scrutinee").or_default() += facts.owns_scrutinee.len();

        // RFC-0125 §3 M6, the third judgment's third slice: every right-hand
        // side of every instance, counted by whether it names its producer
        // type. The build is the placer's own, re-run here because the fold
        // keeps no `Rhs`.
        for inst in &lowered.instances {
            let Ok(top) = vyrn_lower::core::build(&program, inst, &own) else {
                continue;
            };
            for body in top.frames() {
                producers(&body.stmts, &mut produced, &mut ops);
                // RFC-0125 §3 M3, the input-circle slice: the holes a
                // binding's release walks around are the core's own answer,
                // stated where the `consume` is written. `own.rs` kept a
                // copy the core read back; the copy is gone, and the census
                // counts what the core states.
                for info in &body.names {
                    *counted.entry("binding holes").or_default() += info.holes.len();
                }
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
    eprintln!("what the rows say is computed, over the corpus:");
    for (what, n) in &ops {
        eprintln!("  {n:6} {what}");
    }
    // Every operation column is filled (RFC-0125 §3 M3, the operation
    // slice). A `prim` names its operator, a `make` names its constructor and
    // a literal names its value, so an emitter can map a row to an
    // instruction without reading the source beside it. The assertion is that
    // the columns EXIST over the corpus rather than a number: a count is what
    // a later slice reads to see a row appear or vanish, and what a wrong
    // value fails is the recorded wasm hashes, which move when an emitted
    // byte does.
    for column in [
        "prim Bin",
        "prim Un",
        "prim Closure",
        "make record",
        "lit int",
        "lit string",
    ] {
        assert!(
            ops.keys().any(|k| k.starts_with(column)),
            "no `{column}` row over the corpus"
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
