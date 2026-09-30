//! The emitter reads the core and nothing else: every
//! body of the corpus and of `tests/shapes/` is taken from the core's rows
//! (`Fn_::core_body`). Beside the pin stands a census of the gaps a body's rows
//! still state, by class.

mod common;

use std::path::{Path, PathBuf};
use vyrn_frontend::ast::Program;
use vyrn_frontend::core::{Body, Callee, Rhs, St};
use vyrn_frontend::project::Memo;

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

fn load_src(src: &str, root: &str) -> Result<(Program, Memo), String> {
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(repo_root().join("std").to_string_lossy().replace('\\', "/")),
        ..Default::default()
    };
    Memo::load(|| vyrn_lower::load(src, root, &opts, &Fs, Some(&*vyrn_genwasm::engine()))).map_err(
        |d| {
            d.first()
                .map(|d| d.render())
                .unwrap_or_else(|| "load failed".into())
        },
    )
}

fn load(path: &std::path::Path) -> Result<(Program, Memo), String> {
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

/// Parses `VYRN_SHARD=k/n`: the run walks the roots and shapes whose index in
/// path order is `k` modulo `n`; unset, it walks all. A shard prints its own
/// totals, so a check that needs the whole corpus runs only unsharded.
fn shard() -> Option<(usize, usize)> {
    let v = std::env::var("VYRN_SHARD").ok()?;
    let parsed = v
        .split_once('/')
        .and_then(|(k, n)| Some((k.parse().ok()?, n.parse().ok()?)))
        .filter(|&(k, n): &(usize, usize)| k < n);
    Some(parsed.unwrap_or_else(|| panic!("VYRN_SHARD must be `k/n` with k < n, not {v:?}")))
}

fn mine(i: usize) -> bool {
    shard().is_none_or(|(k, n)| i % n == k)
}

/// What a body waits on before the core's rows could carry it, hardest first.
/// A body's class is the hardest thing in it, so the counts partition the
/// corpus.
const CLASSES: [&str; 5] = [
    "the row names no value (`Val::Lit(Opaque)`)",
    "a lambda body the row does not carry (`Op::Closure`)",
    "a layout: an aggregate made, read or taken",
    "a callee that is no declared function of the program (`Rhs::Call`)",
    "nothing: the rows carry it",
];

/// Per program and callee: the projection call rows the core states. A
/// projection is inlined at its access site, so a
/// [`Callee::Projection`] row is a site the core did not inline while the
/// emitter does. Empty: a row here is a site the core stopped inlining.
const CALLS: [(&str, &str, usize); 0] = [];

/// A shape `examples/` does not write, so the pin covers the language and not
/// one directory. It lives under `tests/shapes/` because an example would move
/// every corpus census and the wasm manifest. The file's first line is
/// `// <what it is>`; the comment lines after it say why; the rest is source.
struct Shape {
    what: String,
    src: String,
}

fn shapes() -> Vec<Shape> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/shapes");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("tests/shapes")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|file| {
            let text = std::fs::read_to_string(&file).expect("a shape");
            let what = text
                .lines()
                .next()
                .and_then(|l| l.strip_prefix("// "))
                .unwrap_or_else(|| panic!("{}: line 1 is not `// `", file.display()))
                .to_string();
            let src: String = text
                .split_inclusive('\n')
                .skip_while(|l| l.starts_with("//"))
                .collect();
            let src = src.strip_suffix('\n').unwrap_or(&src).to_string();
            Shape { what, src }
        })
        .collect()
}

/// The wrapper `semantics.rs`'s `run` puts around a shape; keep the two equal.
/// Printing the answer puts a String and its release in the loop's frame.
const WRAP: &str = "fn main() -> Int64 { print(vyrnTestMain().toString()) return 0 }";

/// The types `Fn_::core_walkable` admits a name of; keep the two equal.
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
        // The tags are `core::gaps`'; the ranking is this census's.
        let c = match tag.split(':').next().unwrap() {
            "Opaque" => 0,
            "Lambda" => 1,
            "Read" | "Take" | "Make" => 2,
            "Call" => 3,
            other => panic!("the core states a gap this census does not rank: {other}"),
        };
        worst = worst.min(c);
    }
    worst
}

/// Collects the callee of every projection call in `ss`; [`CALLS`] pins them.
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
    for (s, _) in vyrn_frontend::core::rows(ss) {
        if let St::Let(_, r) | St::Do { rhs: r, .. } = s {
            of(r, out);
        }
    }
}

#[test]
#[ignore = "walks the whole corpus; run explicitly: cargo test -p vyrn-cli --test coredrive -- --ignored"]
fn every_body_is_taken_from_the_core() {
    std::thread::Builder::new()
        .stack_size(vyrn_frontend::trap::DEEP_STACK_BYTES)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    let mut classes = [0usize; CLASSES.len()];
    // Per class, the bodies that name only the scalar types `scalar` admits.
    let mut scalars = [0usize; CLASSES.len()];
    let mut carried: std::collections::BTreeSet<String> = Default::default();
    let mut bodies = 0usize;
    let mut judged_only = 0usize;
    let mut programs = 0usize;
    let mut from_core = 0usize;
    let mut emitted = 0usize;
    let mut refused: Vec<String> = Vec::new();
    let mut calls: std::collections::BTreeMap<(String, String), usize> = Default::default();
    let mut ours: Vec<String> = Vec::new();
    for (i, path) in corpus().into_iter().enumerate() {
        if !mine(i) {
            continue;
        }
        let Ok((program, memo)) = load(&path) else {
            continue;
        };
        programs += 1;
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        ours.push(name.clone());
        {
            let lowered = vyrn_lower::lower(&program);
            let own = vyrn_lower::analyze(&program);
            for inst in &lowered.instances {
                let Ok(top) = vyrn_lower::core::build(&program, inst, &own) else {
                    continue;
                };
                // A gen body is judged and emitted by the generator's own
                // compile, so it is not counted among the bodies read here.
                if inst.judged_only() {
                    judged_only += top.frames().len();
                    continue;
                }
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
        let out = emit(&program, &memo);
        let (f, e) = vyrn_codegen::direct::walks();
        from_core += f;
        emitted += e;
        // A program the language refuses (`polyrecursion.vyrn`) stays refused;
        // only the emitter's own refusal names a body the core did not state.
        match out {
            Err(e) if e.contains("the core did not state") => {
                refused.push(format!("{name}: {e}"));
            }
            _ => {}
        }
    }
    for Shape { what, src } in shapes()
        .into_iter()
        .enumerate()
        .filter_map(|(i, s)| mine(i).then_some(s))
    {
        let root = repo_root().join("examples/@shape.vyrn");
        let src = format!("{src}\n{WRAP}\n");
        let (program, memo) = match load_src(&src, &root.to_string_lossy().replace('\\', "/")) {
            Ok(p) => p,
            Err(e) => panic!("{what} does not load: {e}"),
        };
        let out = emit(&program, &memo);
        let (f, e) = vyrn_codegen::direct::walks();
        from_core += f;
        emitted += e;
        if let Err(e) = out {
            refused.push(format!("{what}: {e}"));
        }
    }

    if let Some((k, n)) = shard() {
        eprintln!("shard {k}/{n}: the totals below are this shard's alone");
    }
    eprintln!("{programs} programs, {bodies} bodies");
    eprintln!("{judged_only} more bodies the kernel judges and no emitter reads");
    eprintln!("what a body waits on before the core's rows could carry it:");
    for (i, what) in CLASSES.iter().enumerate() {
        eprintln!("  {:6}  {:6} scalar-only  {what}", classes[i], scalars[i]);
    }
    eprintln!(
        "  {} distinct bodies the rows carry end to end",
        carried.len()
    );
    eprintln!("the emitter took the core's walk for {from_core} of {emitted} bodies");
    eprintln!("the projection calls the core still states, rather than inlining:");
    for ((program, callee), n) in &calls {
        eprintln!("  {n:4}  {callee}   {program}");
    }
    let named_calls: Vec<(&str, &str, usize)> = calls
        .iter()
        .map(|((p, c), n)| (p.as_str(), c.as_str(), *n))
        .collect();
    let pinned: Vec<(&str, &str, usize)> = CALLS
        .into_iter()
        .filter(|(n, ..)| ours.iter().any(|o| o == n))
        .collect();
    assert_eq!(
        named_calls, pinned,
        "the core states a projection call somewhere the record does not name"
    );
    assert!(
        refused.is_empty(),
        "the emitter refused a program the core did not state:\n{}",
        refused.join("\n")
    );
    assert_eq!(
        from_core, emitted,
        "a body was emitted statement by statement and not taken whole"
    );
}

fn emit(program: &Program, memo: &Memo) -> Result<Vec<u8>, String> {
    let _lowered = vyrn_lower::lower(program);
    vyrn_codegen::direct::forget_walks();
    vyrn_codegen::direct::compile(program, memo)
}
