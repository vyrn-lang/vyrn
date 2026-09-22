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
//! which takes the AST walk back — and the two modules are compared. Where the
//! bytes differ, both modules are run and must print and exit the same
//! (RFC-0125 M7): the core gives a local to a value the arm kept on the
//! operand stack, so a byte-identical module stopped being the witness.
//!
//! The unit of selection is the STATEMENT since the interleave slice, so the
//! count beside the licence is per FORM: how many occurrences of each form of
//! the AST dispatch the arm emitted, and how many the core's rows did. An arm
//! goes when nothing reaches it, which is what the count is for. The body
//! count stands beside it: how many of the corpus's bodies the rows carry end
//! to end, out of how many the emitter lowers. The classification below says what each of the rest waits on, and
//! it is the same list §3 M3 records.

mod common;

use std::path::{Path, PathBuf};
use vyrn_frontend::ast::Program;
use vyrn_lower::core::{Body, Callee, Rhs, St};

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

fn load_src(src: &str, root: &str) -> Result<Program, String> {
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(repo_root().join("std").to_string_lossy().replace('\\', "/")),
        ..Default::default()
    };
    vyrn_frontend::load(src, root, &opts, &Fs).map_err(|d| {
        d.first()
            .map(|d| d.render())
            .unwrap_or_else(|| "load failed".into())
    })
}

fn load(path: &std::path::Path) -> Result<Program, String> {
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    load_src(&src, &path.to_string_lossy().replace('\\', "/"))
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
const CLASSES: [&str; 6] = [
    "the row names no value (`Val::Lit(Opaque)`)",
    "a lambda body the row does not carry (`Op::Closure`)",
    "a switch whose arms a call chooses (`Test::Impl`)",
    "a layout: an aggregate made, read or taken",
    "a callee that is no declared function of the program (`Rhs::Call`)",
    "nothing: the rows carry it",
];

/// The two entries of [`vyrn_codegen::direct::FORMS`] this probe tables per
/// program: the exits whose arm is nearest retirement.
///
/// **A zero here is a zero over `examples/` and not over the language**, which
/// is what [`SHAPES`] is for: a shape the corpus does not write is emitted
/// beside it, both ways, and counted into the same table.
///
/// `Stmt::Continue`'s zero was measured over the whole gate list by the
/// occurrence slice and its arm is retired, so the column reads zero here
/// because there is no arm left to reach. `Stmt::Break`'s does not: it stands
/// at 8 over this corpus and at 298 over the gate list, and [`PIN`] names the
/// three programs this walk sees.
const BREAK: usize = 7;
const CONT: usize = 8;

/// Per program: how many `break` and how many `continue` occurrences the AST
/// arm emitted, for every program where either is not zero. The residue the
/// next slice on this line has to empty, program by program and not as one sum.
///
/// The one left is a projection's body INLINED at its caller.
/// `jchain.vyrn`'s is `doc.field("items")[1]`, where the emitter inlines
/// `Json`'s `at` and then inlines `field` from the CLONE of the receiver that
/// expansion holds, so the `break` stands on a node the core never saw; the
/// core would have to inline `a[i]` on a user container too, and the store
/// side of that is `atSet`.
///
/// The other rewrite that put an exit out of the core's reach,
/// `project::iterate_loop`'s clone of a user container's loop body, is off this
/// table since the exits slice — `own::ReleasePlan::key_of` maps a clone back
/// to the node the core keyed.
const PIN: [(&str, usize, usize); 1] = [("jchain.vyrn", 1, 0)];

/// Per program and callee: the projection CALL rows the core still states.
///
/// A projection is inlined at its access site (RFC-0091 M2, RFC-0120), so a
/// [`Callee::Projection`] row is a site the core did NOT inline — its rows
/// are in the projection's own body ([`vyrn_lower::Lowered::places`]) keyed to
/// the projection's own parameters, and at a site those parameters are the
/// caller's expressions, so no row there can stand for this call. That is what
/// the eight `break` occurrences on the AST arm were: the emitter inlines and
/// the core did not.
///
/// The table is EMPTY since the optional slice: the last five rows were the
/// OPTIONAL kind (RFC-0122), whose body splits into four parts at a miss test
/// and whose consumer is an `if let`, and the core states that split at the
/// site too (`Builder::optional_if_let`). A row here again is a site the core
/// stopped inlining.
const CALLS: [(&str, &str, usize); 0] = [];

/// The shapes `examples/` does not write, emitted both ways here so the licence
/// above and the exit count below are the LANGUAGE's and not one directory's.
///
/// Each is loaded from source rather than added to `examples/`, where it would
/// move every corpus census and the wasm manifest.
///
/// The first two are the readers the exits slice attributed `Stmt::Continue`'s
/// arm to, and neither is one. The other five are shapes the driver got wrong
/// and nothing asked: `examples/` writes none of them, and the file that does
/// compiles with no core at all.
const SHAPES: [(&str, &str); 10] = [
    (
        "a `for` over an array literal",
        "fn vyrnTestMain() -> Int64 { let mut s = 0 \
         for i in [0, 1, 2, 3, 4, 5] { if i % 2 == 1 { continue } s = s + i } \
         return s }",
    ),
    (
        "a `continue` under a `region`",
        "fn vyrnTestMain() -> Int64 { let mut n = 0 let mut i = 0 \
         while i < 100 { i = i + 1 \
         region { if i % 2 == 0 { continue } n = n + 1 } } \
         return n }",
    ),
    (
        "a `let` annotated with a `where` type",
        "type Age = Int64 where value >= 18 \
         fn vyrnTestMain() -> Int64 { let mut x = 30 x = x - 25 \
         let a: Age = x return a }",
    ),
    (
        "a store into a binding of a `where` type",
        "type Age = Int64 where value >= 18 \
         fn vyrnTestMain() -> Int64 { let mut a: Age = 20 a = a - 15 return a }",
    ),
    (
        "a `let` of an `if` expression",
        "fn vyrnTestMain() -> Int64 { let x = if 2 > 1 { 10 } else { 20 } return x }",
    ),
    (
        "a match on two string literals",
        "fn vyrnTestMain() -> Int64 { if \"abc\" =~ \"[a-z]+\" { return 1 } return 0 }",
    ),
    (
        "an order on two string literals",
        "fn vyrnTestMain() -> Int64 { if \"abc\" < \"abd\" { return 1 } return 0 }",
    ),
    // The tag family's own witness (RFC-0125 M7). Every `if let` of
    // `examples/` waits on another family as well — a builtin call in the
    // arm, a layout, a release — so the corpus cannot say whether the two
    // walks agree on the switch itself.
    (
        "an `if let` the rows carry",
        "fn vyrnTestMain() -> Int64 { let o = Some(7) let mut t = 0          if let Some(n) = o { t = t + n } else { t = 1 } return t }",
    ),
    // A `for` over a String literal gave the whole body up, so the
    // `continue` in the other loop had no row, and the retired arm failed the
    // build (RFC-0125 M7, `m7-forhead`).
    (
        "a `for` over a String literal beside a `continue`",
        "fn vyrnTestMain() -> Int64 { let xs: Array<Int64> = [1, 2, 3, 4, 5] let mut s = 0          for x in xs { if x % 2 == 0 { continue } s = s + x }          for c in \"abc\" { s = s + Int64(c) } return s }",
    ),
    // A key read is the runtime's lookup into a slot of its own, and the
    // `let` copies it out (RFC-0125 M7). Every key read of `examples/` reads
    // a String value, which the frame does not hold.
    (
        "a map key read the rows carry",
        "fn look(n: Map<String, Int64>, k: String) -> Int64 { let o = n[k] let mut r = 0          if let Some(v) = o { r = v } return r }          fn vyrnTestMain() -> Int64 { let n: Map<String, Int64> = [\"a\": 10, \"b\": 20]          return look(n, \"b\") + look(n, \"c\") }",
    ),
];

/// What `semantics.rs`'s `run` wraps a shape in, so what is emitted here is the
/// program that test compiles: the answer is printed, which puts a String and
/// its release in the frame the loop sits in.
const WRAP: &str = "fn main() -> Int64 { print(vyrnTestMain().toString()) return 0 }";

/// Per shape: how many `break` and how many `continue` occurrences the AST arm
/// emitted. An arm goes when this table and [`PIN`] both read zero.
const SHAPE_PIN: [(&str, usize, usize); 10] = [
    ("a `for` over an array literal", 0, 0),
    ("a `continue` under a `region`", 0, 0),
    ("a `let` annotated with a `where` type", 0, 0),
    ("a store into a binding of a `where` type", 0, 0),
    ("a `let` of an `if` expression", 0, 0),
    ("a match on two string literals", 0, 0),
    ("an order on two string literals", 0, 0),
    ("an `if let` the rows carry", 0, 0),
    ("a `for` over a String literal beside a `continue`", 0, 0),
    ("a map key read the rows carry", 0, 0),
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
    for tag in vyrn_lower::core::gaps(body) {
        // The tag is the core's, stated once in `core::gaps`; the ranking is
        // this census's. A tag with no class here is a row shape the core
        // learned to state and nobody ranked.
        let c = match tag.split(':').next().unwrap() {
            "Opaque" => 0,
            "Lambda" => 1,
            "Switch" => 2,
            "Read" | "Take" | "Make" => 3,
            "Call" => 4,
            other => panic!("the core states a gap this census does not rank: {other}"),
        };
        worst = worst.min(c);
    }
    worst
}

/// Every projection call this body still states, by callee name — the census
/// [`CALLS`] pins.
fn projection_calls(ss: &[St], out: &mut Vec<String>) {
    fn of(r: &Rhs, out: &mut Vec<String>) {
        if let Rhs::Call {
            callee,
            kind: Callee::Projection,
            ..
        } = r
        {
            out.push(callee.clone());
        }
    }
    for s in ss {
        match s {
            St::Let(_, r) | St::Do { rhs: r, .. } => of(r, out),
            St::If { then, els, .. } => {
                projection_calls(then, out);
                projection_calls(els, out);
            }
            St::Loop { body: b, .. } | St::Block { body: b, .. } => projection_calls(b, out),
            St::Switch { arms, .. } => {
                for a in arms {
                    projection_calls(&a.body, out);
                }
            }
            _ => {}
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
    let mut forms = [(0usize, 0usize); vyrn_codegen::direct::FORMS.len()];
    let mut differ: Vec<String> = Vec::new();
    let mut runs_apart: Vec<String> = Vec::new();
    let mut exits: Vec<(String, usize, usize)> = Vec::new();
    let mut calls: std::collections::BTreeMap<(String, String), usize> = Default::default();
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
                    let mut names = Vec::new();
                    projection_calls(&body.stmts, &mut names);
                    for n in names {
                        *calls.entry((name.clone(), n)).or_default() += 1;
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
        let per = vyrn_codegen::direct::forms();
        for (i, (arm, took)) in per.iter().enumerate() {
            forms[i].0 += arm;
            forms[i].1 += took;
        }
        if per[BREAK].0 > 0 || per[CONT].0 > 0 {
            exits.push((name.clone(), per[BREAK].0, per[CONT].0));
        }
        let ast = emit(&program, true);
        match (core, ast) {
            (Ok(a), Ok(b)) if a == b => same += 1,
            (Ok(a), Ok(b)) => match runs_the_same(&path) {
                Ok(()) => differ.push(format!(
                    "{name}: {} bytes from the core, {} from the AST, and they run the same",
                    a.len(),
                    b.len()
                )),
                Err(e) => runs_apart.push(format!("{name}: {e}")),
            },
            (Err(a), Err(b)) if a == b => same += 1,
            (a, b) => runs_apart.push(format!("{name}: {a:?} against {b:?}")),
        }
    }
    // The same two walks over the shapes the corpus does not write. Their
    // counts stay out of `forms` above, so the corpus totals a record compares
    // against the slice before it are the same measurement.
    let mut shapes: Vec<(&str, usize, usize)> = Vec::new();
    for (what, src) in SHAPES {
        let root = repo_root().join("examples/@shape.vyrn");
        let src = format!("{src}\n{WRAP}\n");
        let program = match load_src(&src, &root.to_string_lossy().replace('\\', "/")) {
            Ok(p) => p,
            Err(e) => panic!("{what} does not load: {e}"),
        };
        let core = emit(&program, false);
        let per = vyrn_codegen::direct::forms();
        let ast = emit(&program, true);
        // Two walks that fail alike are no witness of a shape.
        assert!(core.is_ok(), "{what}: {core:?}");
        if core != ast {
            let dir = common::scratch("coredrive");
            let file = dir.join("shape.vyrn");
            std::fs::write(&file, &src).unwrap();
            if let Err(e) = runs_the_same(&file) {
                panic!("{what}: the two walks' modules run differently\n{e}");
            }
        }
        shapes.push((what, per[BREAK].0, per[CONT].0));
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
    // The unit of selection is the STATEMENT since the interleave slice, so
    // the count that says what an AST arm still costs is per FORM: how many
    // occurrences the arm emitted, and how many the core's rows did.
    eprintln!("what each form of the AST dispatch still emits:");
    for (i, (what, arm_exists)) in vyrn_codegen::direct::FORMS.iter().enumerate() {
        let (arm, core) = forms[i];
        let retired = if *arm_exists { "" } else { "   (retired)" };
        eprintln!("  {arm:8} the arm   {core:8} the core's rows   {what}{retired}");
    }
    eprintln!("where a `break` or a `continue` still reaches the AST arm:");
    for (name, brk, cont) in &exits {
        eprintln!("  {brk:4} break   {cont:4} continue   {name}");
    }
    eprintln!("{same} of {programs} programs emit the same module either way");
    eprintln!(
        "{} more emit different modules that run the same",
        differ.len()
    );
    for d in &differ {
        eprintln!("  {d}");
    }
    eprintln!("and off the corpus, in the shapes `examples/` does not write:");
    for (what, brk, cont) in &shapes {
        eprintln!("  {brk:4} break   {cont:4} continue   {what}");
    }
    eprintln!("the projection calls the core still states, rather than inlining:");
    for ((program, callee), n) in &calls {
        eprintln!("  {n:4}  {callee}   {program}");
    }
    let named_exits: Vec<(&str, usize, usize)> =
        exits.iter().map(|(n, b, c)| (n.as_str(), *b, *c)).collect();
    assert_eq!(
        named_exits, PIN,
        "a `break` or a `continue` reaches the AST arm somewhere the record does not name"
    );
    let named_calls: Vec<(&str, &str, usize)> = calls
        .iter()
        .map(|((p, c), n)| (p.as_str(), c.as_str(), *n))
        .collect();
    assert_eq!(
        named_calls, CALLS,
        "the core states a projection call somewhere the record does not name"
    );
    assert_eq!(
        shapes, SHAPE_PIN,
        "a shape off the corpus reaches the AST arm a different number of times"
    );
    // A retired form has no arm to reach. The flag in `FORMS` is the schedule's
    // one home and this is what keeps it true over the corpus: a form marked
    // retired whose arm emits anything is an arm that came back.
    for (i, (what, arm_exists)) in vyrn_codegen::direct::FORMS.iter().enumerate() {
        if !arm_exists {
            assert_eq!(forms[i].0, 0, "the retired arm for {what} emitted again");
        }
    }
    // The forms whose arm the rows have started to relieve. An arm goes when
    // its first number reaches zero, and this pin says which nine are on that
    // road: a form that drops off the list has lost a reader the record has to
    // explain, and one that joins it is a slice's own count.
    let carrying: Vec<&str> = vyrn_codegen::direct::FORMS
        .iter()
        .enumerate()
        .filter(|(i, _)| forms[*i].1 > 0)
        .map(|(_, w)| w.0)
        .collect();
    assert_eq!(
        carrying,
        [
            "Stmt::Let",
            "Stmt::Assign",
            "Stmt::Return",
            "Stmt::If",
            "Stmt::Expr",
            "Stmt::While",
            "Stmt::Break",
            "Stmt::Continue",
            "Stmt::IfLet"
        ],
        "the forms the core's rows carry are not the ones the record names"
    );
    // The driver is a screen and not a judgement: where it stands down, the
    // AST walk emits exactly what it did. So a body it takes has to reach the
    // corpus at all, or this test measures nothing.
    assert!(from_core > 0, "the core walk emitted no body");
    // The licence. A program whose two modules differ runs the same under
    // both: same stdout, stderr and exit code. Two shapes make the bytes
    // differ. The core names every value, so it gives a local to the values
    // the arm kept on the stack (RFC-0125 M7); and it writes a `return`
    // per arm where the arm joins a `match` or an `if` and returns once
    // (`Builder::return_through`).
    assert!(
        runs_apart.is_empty(),
        "the two walks' modules run differently:\n{}",
        runs_apart.join("\n")
    );
}

/// Whether the module each walk emits for `file` runs the same: `vyrn run`
/// with and without `VYRN_NO_CORE_WALK=1`, under the corpus's conventions
/// (`common::run_io`). `Err` names the first stream that differs.
fn runs_the_same(file: &Path) -> Result<(), String> {
    let run = |ast: bool| {
        let mut cmd = common::vyrn();
        cmd.arg("run").arg(file);
        cmd.args(common::read_args(&file.with_extension("args")));
        if ast {
            cmd.env("VYRN_NO_CORE_WALK", "1");
        } else {
            cmd.env_remove("VYRN_NO_CORE_WALK");
        }
        common::run_io(cmd, &common::examples_dir(), &file.with_extension("stdin"))
    };
    let (core, ast) = (run(false), run(true));
    let (c, a) = (core.status.code(), ast.status.code());
    let mut why = String::new();
    if c != a {
        why = format!("exit {c:?} from the core, {a:?} from the AST\n");
    }
    for (stream, x, y) in [
        ("stdout", &core.stdout, &ast.stdout),
        ("stderr", &core.stderr, &ast.stderr),
    ] {
        let (x, y) = (common::norm(x), common::norm(y));
        why += &common::first_diff(stream, "core", &x, "AST", &y).unwrap_or_default();
    }
    if why.is_empty() {
        Ok(())
    } else {
        Err(why)
    }
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
