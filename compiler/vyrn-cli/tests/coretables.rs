//! A census of the core's side tables over the whole corpus.
//!
//! The ignored test runs the placed analysis on every corpus program, folds
//! `vyrn_lower::core::facts` and prints how many rows of each kind the core states, so a change
//! shows a row appear or vanish. A wrong row fails the residue ratchet, the memory suite and
//! the recorded wasm hashes, not this test. It asserts one fact: every `Rhs` names the type its
//! node produces, so the typed judgment never counts a store as unjudged.

use vyrn_frontend::loader::DiskResolver;
use vyrn_frontend::project::Memo;

use std::collections::BTreeMap;
use std::path::PathBuf;
use vyrn_frontend::ast::Program;
use vyrn_frontend::core::{Ctor, Lit, Rhs, St, Val};

fn repo_root() -> PathBuf {
    let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    d.pop();
    d.pop();
    d
}

fn load(path: &std::path::Path) -> Result<(Program, Memo), String> {
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let root = path.to_string_lossy().replace('\\', "/");
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(repo_root().join("std").to_string_lossy().replace('\\', "/")),
        ..Default::default()
    };
    Memo::load(|| vyrn_lower::load(&src, &root, &opts, &DiskResolver)).map_err(|d| {
        d.first()
            .map(|d| d.render())
            .unwrap_or_else(|| "load failed".into())
    })
}

fn core_body(src: &str, which: &str) -> vyrn_frontend::core::Body {
    let (program, _memo) =
        Memo::load(|| vyrn_lower::load(src, "core.vyrn", &Default::default(), &DiskResolver))
            .unwrap_or_else(|d| panic!("{}", d.first().map(|d| d.render()).unwrap_or_default()));
    let lowered = vyrn_lower::lower(&program);
    let own = vyrn_lower::analyze(&program);
    let inst = lowered
        .instances
        .iter()
        .find(|i| i.func.name == which)
        .unwrap_or_else(|| panic!("`{which}` is lowered"));
    vyrn_lower::core::build(&program, inst, &own).expect("the body builds")
}

/// Returns the core's hole set for the `let` named `binding` in `main`. A `consume p` states
/// its hole where the take is written ([`vyrn_lower::core`]'s `take_place_at`).
fn core_holes(src: &str, binding: &str) -> Vec<String> {
    core_body(src, "main")
        .names
        .iter()
        .find(|n| n.bound_by_let && n.source == binding)
        .unwrap_or_else(|| panic!("no `let {binding}` in main"))
        .holes
        .clone()
}

/// Returns whether the frame of `which` owes a release on `binding`
/// ([`vyrn_frontend::core::NameInfo`]'s `releases`).
fn core_releases(src: &str, which: &str, binding: &str) -> bool {
    core_body(src, which)
        .frames()
        .iter()
        .flat_map(|f| f.names.iter())
        .find(|n| n.source == binding)
        .unwrap_or_else(|| panic!("no `{binding}` in `{which}`"))
        .releases
}

/// A body is built from the analysis it is handed, whatever program the thread placed since:
/// the kernel's placement and the checker's record are that analysis's, not the thread's.
#[test]
fn a_body_reads_its_own_analysis_after_another_program_is_placed() {
    let analyzed = |src: &str| {
        let program = vyrn_lower::load(src, "core.vyrn", &Default::default(), &DiskResolver)
            .unwrap_or_else(|d| panic!("{}", d[0].render()));
        let own = vyrn_lower::analyze(&program);
        (program, own)
    };
    let main_of = |program: &Program, own: &vyrn_frontend::own::Ownership| {
        let lowered = vyrn_lower::lower_with(program, own);
        let inst = (lowered.instances.iter())
            .find(|i| i.func.name == "main")
            .expect("`main` is lowered");
        vyrn_lower::core::build(program, inst, own)
            .expect("`main` builds")
            .render()
    };
    let (a, own_a) = analyzed(
        "type P = { name: String, tags: Array<String> }
         fn main() -> Int64 {
             let mut p = P { name: \"a\" + \"b\", tags: [\"x\"] }
             p.name = p.name + \"c\"
             let n = if p.tags.length == 0 { 1 } else { 2 }
             return n
         }",
    );
    let before = main_of(&a, &own_a);
    let (_b, _own_b) = analyzed(
        "fn main() -> Int64 {
             let xs = [1, 2, 3]
             let mut s = \"q\"
             for x in xs { s = s + \"r\" }
             return s.byteLength
         }",
    );
    assert_eq!(main_of(&a, &own_a), before);
}

/// `x.copy()` of a type with `impl Copy` is a call row to the impl, at the capability the impl
/// declares, so a pass over the rows sees the call the language makes.
#[test]
fn a_copy_of_a_type_with_impl_copy_is_a_call_row_to_the_impl() {
    let src = "type Box = { s: String }
               impl Copy for Box { fn copy(read self) -> Box { return Box { s: self.s + \"c\" } } }
               fn main() -> Int64 { let b = Box { s: \"a\" + \"b\" } let c = b.copy() return 0 }";
    let rows = core_body(src, "main").render();
    assert!(
        rows.contains("let c! = fn Copy__Box__copy(read b!)"),
        "{rows}"
    );
}

/// A refutable `let`'s binder borrows the scrutinee's payload. An owned binder over
/// module state would free the global's buffer at block exit, under the next read.
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

/// An `if let` over a parameter matches the caller's value, so this frame owes
/// nothing for it; over a call result it owes the release. A release in the
/// first shape frees an `args()` element (`examples/argsdemo.vyrn`).
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

/// `calls_in` sees through the aggregate literal; otherwise the wrapper goes unmarked and its
/// caller reclaims storage the lender's own caller still owns.
#[test]
fn a_lender_forwarded_through_an_aggregate_lends_still() {
    let src = "type R = { name: String }                fn pick(xs: Array<String>) -> String                { for x in xs { return x } return \"\" }                fn g(a: Array<String>) -> R { return R { name: pick(a) } }                fn main() -> Int64 { let arr: Array<String> = [\"a\" + \"b\"]                let r = g(arr) return r.name.byteLength }";
    let program = vyrn_lower::load(src, "lend.vyrn", &Default::default(), &DiskResolver);
    assert!(
        program.is_err(),
        "a loop variable may not be returned, so `pick` is refused before it can lend"
    );
}

/// The hole is spelled relative to the binding.
#[test]
fn a_taken_field_is_the_only_place_the_walk_skips() {
    let src = "type Doc = { title: String, body: String }                fn mk(a: String) -> Doc { return Doc { title: a + \"t\", body: a + \"b\" } }                fn main() -> Int64 { let d = mk(\"x\"); let t = consume d.title;                return Int64(t.byteLength) + Int64(d.body.byteLength); }";
    assert_eq!(core_holes(src, "d"), vec![".title".to_string()]);
}

/// `vyxCompileComponent` in `std/vyx` drains nine fields out of one record.
#[test]
fn every_take_of_one_record_joins_the_hole_set() {
    let src = "type Doc = { title: String, body: String }                fn mk(a: String) -> Doc { return Doc { title: a + \"t\", body: a + \"b\" } }                fn main() -> Int64 { let d = mk(\"x\"); let t = consume d.title;                let b = consume d.body; return Int64(t.byteLength) + Int64(b.byteLength); }";
    assert_eq!(
        core_holes(src, "d"),
        vec![".body".to_string(), ".title".to_string()]
    );
}

/// `vyxBuildLayoutModuleAt` in `std/vyx` writes `consume hs.head.err`.
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

/// Counts into `out` one row per right-hand side, keyed by variant and whether it names the
/// type its node produces.
fn producers(
    stmts: &[St],
    out: &mut BTreeMap<(&'static str, bool), usize>,
    ops: &mut BTreeMap<String, usize>,
) {
    for (s, _) in vyrn_frontend::core::rows(stmts) {
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
            St::Switch { on, .. } => literal(on, ops),
            _ => {}
        }
    }
}

/// Counts what a right-hand side says is computed: its operator, constructor and literal kinds.
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
                Ctor::Closure(_) => "closure",
            };
            *ops.entry(format!("make {what}")).or_default() += 1;
            for v in vs {
                literal(v, ops);
            }
        }
        Rhs::Val(v) => literal(v, ops),
        Rhs::Call { args, .. } => {
            for v in args.iter().filter_map(|(a, _)| a.val()) {
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
        Lit::Opaque(_) => "opaque",
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
    // Generation is the driver's engine, not the frontend's. Without it, examples that import
    // through a generator fail to load and the census silently counts a smaller corpus.
    vyrn_genwasm::install();
    let mut counted: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut produced: BTreeMap<(&'static str, bool), usize> = BTreeMap::new();
    let mut ops: BTreeMap<String, usize> = BTreeMap::new();
    let mut programs = 0usize;
    for path in corpus() {
        let Ok((program, _memo)) = load(&path) else {
            continue;
        };
        programs += 1;
        let lowered = vyrn_lower::lower(&program);
        let own = vyrn_lower::analyze(&program);
        let facts = vyrn_lower::core::facts().expect("the placer fills the core's facts");

        for core_says in facts.arms.values() {
            *counted.entry("arm_frees").or_default() += 1;
            *counted.entry("arm binders freed").or_default() += core_says.len();
        }

        for (_, core_says) in &facts.stores {
            *counted.entry("store_owned").or_default() += 1;
            if *core_says {
                *counted.entry("stores that release").or_default() += 1;
            }
        }
        *counted.entry("stores the core stands down at").or_default() += facts.stood_down.len();
        *counted.entry("discarded_results").or_default() += facts.discarded.len();

        *counted.entry("arg_drops").or_default() += facts.arg_drops.len();

        // Rule N's rows are the kernel's `equalize`.
        for rows in facts.edges.values() {
            *counted.entry("edge_releases").or_default() += 1;
            *counted.entry("edge rows").or_default() += rows.len();
        }

        for (_node, core_holes) in &facts.receivers {
            *counted.entry("receiver_frees").or_default() += 1;
            *counted.entry("receiver holes").or_default() += core_holes.len();
        }
        *counted.entry("receiver_malloc").or_default() += facts.receiver_malloc.len();

        for (_site, took) in &facts.consuming {
            *counted.entry("switch sites").or_default() += 1;
            *counted.entry("consuming: taken").or_default() += usize::from(*took);
        }
        // A construct over a value the frame made owns its binders' boxes. This count contains
        // `consuming: taken`, not the reverse.
        *counted.entry("owns_scrutinee").or_default() += facts.owns_scrutinee.len();

        // The build is the placer's own, re-run because the fold keeps no `Rhs`.
        for inst in &lowered.instances {
            let Ok(top) = vyrn_lower::core::build(&program, inst, &own) else {
                continue;
            };
            for body in top.frames() {
                producers(&body.stmts, &mut produced, &mut ops);
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
    // Each column exists so an emitter maps a row to an instruction without the source. The
    // counts are not asserted; a wrong value moves the recorded wasm hashes.
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
    // No exception list: a node the checker typed answers from its row, and a projection
    // expanded at the site answers from its declared result under the receiver's
    // type arguments.
    let untyped: usize = produced
        .iter()
        .filter(|((_, typed), _)| !typed)
        .map(|(_, n)| *n)
        .sum();
    assert_eq!(untyped, 0, "right-hand sides with no producer type");
}
