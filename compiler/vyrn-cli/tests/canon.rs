//! Relation identity (`vyrn_lower::canon`) over real core bodies.
//!
//! The ignored census canonicalizes every predicate, key lambda, loop-free
//! `-> Bool` function and `if` or `while` condition of the corpus roots
//! under `examples/`, `std/` and `site/`, and holds the counts of the
//! research report's section 4.5 per population, scope and level to
//! `tests/pins/canon.tsv`. The other tests hold the report's 21 worked pairs
//! and its float rules.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use vyrn_frontend::ast::{each_expr_in, ArmBody, Block, Expr, LambdaBody, NodeId, Program, Stmt};
use vyrn_frontend::core::Body;
use vyrn_lower::canon::{Level, Table, Ty, Unfit};
use vyrn_lower::verdict::TrapFree;
use vyrn_lower::World;

mod common;

const LEVELS: [Level; 3] = [Level::Syntax, Level::Boolean, Level::Atoms];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Every `.vyrn` file under `dir`, recursively, but a build's `target/`.
fn roots_in(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).expect("a corpus directory reads") {
        let p = e.expect("an entry reads").path();
        if p.is_dir() && !p.ends_with("target") {
            roots_in(&p, out);
        } else if p.extension().is_some_and(|x| x == "vyrn") {
            out.push(p);
        }
    }
}

/// The program and World of `src`, checked as `vyrn check` loads `path`.
fn load(src: &str, path: &Path) -> Option<(Program, std::sync::Arc<World>)> {
    let manifest = path
        .parent()
        .and_then(|d| vyrn_frontend::manifest::find(d).ok().flatten());
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(slash(&repo_root().join("std"))),
        artifacts: manifest.and_then(|m| m.artifacts),
        expansions: vyrn_frontend::project::Expansions::shared(),
        ..Default::default()
    };
    let engine = vyrn_genwasm::engine();
    let loaded = vyrn_lower::load_warned(
        src,
        &slash(path),
        &opts,
        &vyrn_frontend::loader::DiskResolver,
        Some(&*engine),
    );
    loaded.0.ok()
}

/// What the AST says of a function: the sites of its `if` statements and
/// expressions and of its `while` loops but `while true`, and whether each
/// lambda, by line and column, has a block body.
#[derive(Default)]
struct Ast {
    conds: HashSet<NodeId>,
    lambdas: HashMap<(usize, usize), bool>,
}

impl Ast {
    fn block(&mut self, b: &Block) {
        for s in &b.stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::Store { value, .. }
            | Stmt::Return {
                value: Some(value), ..
            }
            | Stmt::Expr(value, _) => self.expr(value),
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.conds.insert(s.id());
                self.expr(cond);
                self.block(then_block);
                if let Some(e) = else_block {
                    self.block(e);
                }
            }
            Stmt::While { cond, body, .. } => {
                if !matches!(cond, Expr::Bool(true, _)) {
                    self.conds.insert(s.id());
                }
                self.expr(cond);
                self.block(body);
            }
            Stmt::ForIn { iter, body, .. } => {
                self.expr(iter);
                self.block(body);
            }
            Stmt::Region { body, .. } => self.block(body),
            Stmt::Return { value: None, .. }
            | Stmt::Break { .. }
            | Stmt::Continue { .. }
            | Stmt::Drop { .. } => {}
        }
    }

    fn expr(&mut self, e: &Expr) {
        let mut blocks = Vec::new();
        each_expr_in(e, &mut |x| {
            match x {
                Expr::IfExpr { .. } => {
                    self.conds.insert(x.id());
                }
                Expr::Lambda {
                    body, line, col, ..
                } => {
                    let block = matches!(body, LambdaBody::Block(_));
                    self.lambdas.insert((*line, *col), block);
                    if let LambdaBody::Block(b) = body {
                        blocks.push(b.clone());
                    }
                }
                Expr::Match { arms, .. } => {
                    for a in arms {
                        if let ArmBody::Block(b) = &a.body {
                            blocks.push(b.clone());
                        }
                    }
                }
                _ => {}
            }
            true
        });
        for b in &blocks {
            self.block(b);
        }
    }
}

/// The population a body or a condition is counted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    /// An expression lambda with a Bool body.
    BoolLambda,
    /// A block lambda answering Bool.
    BoolBlockLambda,
    /// A `-> Bool` function whose body is one `return`.
    OneReturn,
    /// A loop-free `-> Bool` function of more statements.
    MoreStatements,
    /// An expression lambda of another type.
    KeyLambda,
    /// A block lambda of another type.
    KeyBlockLambda,
    Condition,
}

/// One member of a population: its file, its function, its key and its
/// negation's key at each level, and what deciding it took at layer 2.
struct Member {
    kind: Kind,
    file: String,
    func: String,
    text: String,
    keys: [String; 3],
    nots: [String; 3],
    atoms: usize,
    nodes: usize,
    step: String,
}

/// The members and the unfit bodies of one root, and whether it checks.
#[derive(Default)]
struct Found {
    checks: bool,
    members: Vec<Member>,
    unfit: Vec<(&'static str, Unfit)>,
}

fn measure(
    t: &mut Table<'_>,
    root: vyrn_lower::canon::Id,
    ty: &Ty,
) -> ([String; 3], [String; 3], vyrn_lower::canon::Canon) {
    let mut keys: [String; 3] = Default::default();
    let mut nots: [String; 3] = Default::default();
    let mut last = None;
    for (i, level) in LEVELS.into_iter().enumerate() {
        let c = t.identity(root, ty, level);
        keys[i] = t.key(c.id).to_string();
        if *ty == Ty::Bool {
            let n = t.not(c.id);
            nots[i] = t.key(n).to_string();
        }
        last = Some(c);
    }
    (keys, nots, last.expect("three levels"))
}

fn rows_of(rel: &str) -> Found {
    let path = repo_root().join(rel);
    let src = std::fs::read_to_string(&path).expect("the root reads");
    let Some((program, world)) = load(&src, &path) else {
        return Found::default();
    };
    let free = TrapFree::of(&program, &world);
    let calls = |b: &Body, r: &vyrn_frontend::core::Rhs| free.traps(b, r);
    let decls = world.ownership.proto.types();
    let mut t = Table::new(decls, &calls);
    let mut asts: HashMap<vyrn_frontend::ast::FnId, Ast> = HashMap::new();
    let mut found = Found {
        checks: true,
        ..Found::default()
    };
    for (s, body) in vyrn_lower::insight::root_frames(&program, &world) {
        let func = &program.functions[s.index()];
        // Column 0 marks a function the compiler wrote, such as the predicate
        // of a validated type; 4.5 counts relations as written.
        if func.col == 0 {
            continue;
        }
        let ast = asts.entry(s).or_insert_with(|| {
            let mut a = Ast::default();
            a.block(&func.body);
            a
        });
        let lambda = vyrn_lower::core::lambda_at(&body.name);
        let kind = match lambda {
            Some(at) => Some(match ast.lambdas.get(&at).copied().unwrap_or(false) {
                true => Kind::BoolBlockLambda,
                false => Kind::BoolLambda,
            }),
            None if func.ret == vyrn_frontend::ast::Type::Bool => {
                Some(match &func.body.stmts[..] {
                    [Stmt::Return { value: Some(_), .. }] => Kind::OneReturn,
                    _ => Kind::MoreStatements,
                })
            }
            None => None,
        };
        if let Some(kind) = kind {
            match t.relation(body) {
                Ok(r) => {
                    let kind = match (kind, r.ty == Ty::Bool) {
                        (Kind::BoolLambda, false) => Kind::KeyLambda,
                        (Kind::BoolBlockLambda, false) => Kind::KeyBlockLambda,
                        (k, _) => k,
                    };
                    let (keys, nots, c) = measure(&mut t, r.root, &r.ty);
                    found.members.push(Member {
                        kind,
                        file: rel.to_string(),
                        func: func.name.clone(),
                        text: body.name.clone(),
                        keys,
                        nots,
                        atoms: c.atoms,
                        nodes: c.nodes,
                        step: format!("{:?}", c.step),
                    });
                }
                Err(u) => {
                    let pop = if lambda.is_some() {
                        "lambdas"
                    } else {
                        "Bool functions"
                    };
                    found.unfit.push((pop, u));
                }
            }
        }
        for (site, root) in t.conditions(body) {
            if !ast.conds.contains(&site) {
                continue;
            }
            let (keys, nots, c) = measure(&mut t, root, &Ty::Bool);
            found.members.push(Member {
                kind: Kind::Condition,
                file: rel.to_string(),
                func: func.name.clone(),
                text: format!("{} {site:?}", body.name),
                keys,
                nots,
                atoms: c.atoms,
                nodes: c.nodes,
                step: format!("{:?}", c.step),
            });
        }
    }
    found
}

/// Pairs sharing a key, and pairs where one is the other's negation, among
/// `ms` grouped by `group`, at level `i`.
fn pairs(ms: &[&Member], group: &dyn Fn(&Member) -> String, i: usize) -> (usize, usize, usize) {
    let mut by: BTreeMap<(String, &str), usize> = BTreeMap::new();
    for m in ms {
        *by.entry((group(m), m.keys[i].as_str())).or_default() += 1;
    }
    let shared = by.values().map(|n| n * (n - 1) / 2).sum();
    let nots: usize = (ms.iter())
        .filter(|m| !m.nots[i].is_empty())
        .map(|m| {
            by.get(&(group(m), m.nots[i].as_str()))
                .copied()
                .unwrap_or(0)
        })
        .sum();
    (by.len(), shared, nots / 2)
}

/// The pairs that share a key at level `i` but not at `i - 1`.
fn joined_at(ms: &[&Member], group: &dyn Fn(&Member) -> String, i: usize) -> Vec<String> {
    let mut by: BTreeMap<(String, &str), Vec<&Member>> = BTreeMap::new();
    for m in ms {
        by.entry((group(m), m.keys[i].as_str()))
            .or_default()
            .push(m);
    }
    let mut out = Vec::new();
    for ((g, _), xs) in by {
        for (a, x) in xs.iter().enumerate() {
            for y in &xs[a + 1..] {
                if x.keys[i - 1] != y.keys[i - 1] {
                    out.push(format!("{g}: {} | {}", x.text, y.text));
                }
            }
        }
    }
    out
}

#[test]
#[ignore = "checks every corpus root; run explicitly: cargo test -p vyrn-cli --test canon -- --ignored"]
fn the_corpus_census_holds() {
    let mut roots = Vec::new();
    for dir in ["examples", "std", "site"] {
        roots_in(&repo_root().join(dir), &mut roots);
    }
    let mut rels: Vec<String> = (roots.iter())
        .map(|p| slash(p.strip_prefix(repo_root()).expect("under the repo")))
        .collect();
    rels.sort();
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut done: Vec<(usize, Found)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                let deep =
                    std::thread::Builder::new().stack_size(vyrn_frontend::trap::DEEP_STACK_BYTES);
                deep.spawn_scoped(s, || {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(rel) = rels.get(i) else { break };
                        out.push((i, rows_of(rel)));
                    }
                    out
                })
                .expect("a census worker starts")
            })
            .collect();
        (workers.into_iter())
            .flat_map(|w| w.join().expect("a census worker finishes"))
            .collect()
    });
    done.sort_by_key(|(i, _)| *i);
    let mut members = Vec::new();
    let mut unfit: BTreeMap<String, usize> = BTreeMap::new();
    let unchecked = done.iter().filter(|(_, f)| !f.checks).count();
    for (_, f) in done {
        members.extend(f.members);
        for (pop, u) in f.unfit {
            *unfit.entry(format!("unfit {pop} {u:?}")).or_default() += 1;
        }
    }
    let mut rows: Vec<(String, usize)> = vec![
        ("roots".into(), rels.len()),
        ("roots that do not check".into(), unchecked),
    ];
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for m in &members {
        *kinds.entry(format!("members {:?}", m.kind)).or_default() += 1;
    }
    rows.extend(kinds);
    rows.extend(unfit);
    let of =
        |ks: &[Kind]| -> Vec<&Member> { members.iter().filter(|m| ks.contains(&m.kind)).collect() };
    let research = of(&[Kind::BoolLambda, Kind::OneReturn]);
    let all = of(&[
        Kind::BoolLambda,
        Kind::BoolBlockLambda,
        Kind::OneReturn,
        Kind::MoreStatements,
    ]);
    let keys = of(&[Kind::KeyLambda, Kind::KeyBlockLambda]);
    let conds = of(&[Kind::Condition]);
    let corpus = |_: &Member| String::new();
    let file = |m: &Member| m.file.clone();
    let func = |m: &Member| format!("{} {}", m.file, m.func);
    let pops: [(&str, &[&Member], &dyn Fn(&Member) -> String); 6] = [
        (
            "predicates as 4.5 counts them, across the corpus",
            &research,
            &corpus,
        ),
        (
            "predicates as 4.5 counts them, within one file",
            &research,
            &file,
        ),
        ("every predicate, across the corpus", &all, &corpus),
        ("every predicate, within one file", &all, &file),
        ("key and other lambdas, across the corpus", &keys, &corpus),
        ("conditions, within one function", &conds, &func),
    ];
    for (name, ms, group) in pops {
        rows.push((format!("{name}: members"), ms.len()));
        for i in 0..3 {
            let (ids, shared, nots) = pairs(ms, group, i);
            rows.push((format!("{name}: L{i} identities"), ids));
            rows.push((format!("{name}: L{i} pairs"), shared));
            if !ms.iter().any(|m| m.nots[i].is_empty()) {
                rows.push((format!("{name}: L{i} complement pairs"), nots));
            }
            if i > 0 {
                for j in joined_at(ms, group, i) {
                    eprintln!("joined at L{i} ({name}): {j}");
                }
            }
        }
        let atoms = ms.iter().map(|m| m.atoms).max().unwrap_or(0);
        let nodes = ms.iter().map(|m| m.nodes).max().unwrap_or(0);
        rows.push((format!("{name}: most atoms at L2"), atoms));
        rows.push((format!("{name}: most diagram nodes at L2"), nodes));
        let mut steps: BTreeMap<&str, usize> = BTreeMap::new();
        for m in ms.iter() {
            *steps.entry(m.step.as_str()).or_default() += 1;
        }
        for (s, n) in steps {
            rows.push((format!("{name}: decided at L2 by step {s}"), n));
        }
    }
    let lines = rows.into_iter().map(|(k, n)| format!("{k}\t{n}"));
    common::pin("canon", "census\tcount", lines);
}

/// The research's worked pairs as functions `lN` and `rN` over typed
/// parameters, and its float rules as `flN` and `frN`.
const PAIRS: &str = r#"
type U = { age: Int64, active: Bool }
fn l1(x: Int64) -> Bool { return x > 3 }
fn r1(x: Int64) -> Bool { return 3 < x }
fn l2(a: Bool, b: Bool) -> Bool { return !(a && b) }
fn r2(a: Bool, b: Bool) -> Bool { return !a || !b }
fn l3(a: Bool, b: Bool) -> Bool { return if a { true } else { b } }
fn r3(a: Bool, b: Bool) -> Bool { return a || b }
fn l4(a: Bool) -> Bool { return !!a }
fn r4(a: Bool) -> Bool { return a }
fn l5(a: Bool, b: Bool, c: Bool) -> Bool { return if a { if b { c } else { false } } else { false } }
fn r5(a: Bool, b: Bool, c: Bool) -> Bool { return a && b && c }
fn l6(x: Int64) -> Bool { return x == 3 }
fn r6(x: Int64) -> Bool { return 3 == x }
fn l7(x: Int64, y: Int64) -> Bool { return x + 1 == y }
fn r7(x: Int64, y: Int64) -> Bool { return x == y - 1 }
fn l8(x: Int64) -> Bool { return x + 1 > x }
fn r8(x: Int64) -> Bool { return true }
fn l9(x: Int64) -> Bool { return x <= 4 }
fn r9(x: Int64) -> Bool { return x < 5 }
fn l10(x: Int64) -> Bool { return x >= 18 }
fn r10(x: Int64) -> Bool { return !(x < 18) }
fn l11(x: Int64) -> Bool { return x > 3 && x > 2 }
fn r11(x: Int64) -> Bool { return x > 3 }
fn l12(a: Bool) -> Bool { return a == true }
fn r12(a: Bool) -> Bool { return a }
fn l13(u: U) -> Bool { return u.age >= 18 && u.active }
fn r13(u: U) -> Bool { return u.active && 18 <= u.age }
fn l14(x: Int64, y: Int64) -> Bool { return x - y < 0 }
fn r14(x: Int64, y: Int64) -> Bool { return x < y }
fn l15(x: Int64) -> Bool { return 2 * x == 0 }
fn r15(x: Int64) -> Bool { return x == 0 }
fn l16(x: Int64, y: Int64) -> Bool { return x * 2 + y == y + x + x }
fn r16(x: Int64, y: Int64) -> Bool { return true }
fn l17(f: Float64) -> Bool { return f < 1.5 }
fn r17(f: Float64) -> Bool { return !(f >= 1.5) }
fn l18(f: Float64) -> Bool { return f > 1.5 }
fn r18(f: Float64) -> Bool { return 1.5 < f }
fn l19(a: Int64, b: Int64) -> Bool { return a != b }
fn r19(a: Int64, b: Int64) -> Bool { return !(b == a) }
fn l20(x: Int64) -> Bool { return x <= 9223372036854775807 }
fn r20(x: Int64) -> Bool { return true }
fn l21(x: Int64) -> Bool { return x < 0 - 9223372036854775807 - 1 }
fn r21(x: Int64) -> Bool { return false }
fn fl1(a: Float64, b: Float64) -> Bool { return a > b }
fn fr1(a: Float64, b: Float64) -> Bool { return b < a }
fn fl2(a: Float64, b: Float64) -> Bool { return a >= b }
fn fr2(a: Float64, b: Float64) -> Bool { return b <= a }
fn fl3(a: Float64, b: Float64) -> Bool { return a != b }
fn fr3(a: Float64, b: Float64) -> Bool { return !(a == b) }
fn fl4(a: Float64, b: Float64) -> Bool { return a == b }
fn fr4(a: Float64, b: Float64) -> Bool { return b == a }
fn fl5(a: Float64, b: Float64) -> Bool { return !(a < b) }
fn fr5(a: Float64, b: Float64) -> Bool { return b <= a }
fn fl6(a: Float64, b: Float64) -> Bool { return a == a }
fn fr6(a: Float64, b: Float64) -> Bool { return true }
fn main() -> Int64 { return 0 }
"#;

/// Whether `lN` and `rN` (or `flN` and `frN`) share an identity at each level.
fn agree(prefix: (&str, &str), n: usize) -> Vec<[bool; 3]> {
    let path = std::env::temp_dir().join("canon.vyrn");
    let (program, world) = load(PAIRS, &path).expect("the pairs check");
    let free = TrapFree::of(&program, &world);
    let calls = |b: &Body, r: &vyrn_frontend::core::Rhs| free.traps(b, r);
    let mut t = Table::new(world.ownership.proto.types(), &calls);
    let frames = vyrn_lower::insight::root_frames(&program, &world);
    let mut rel = |name: String| {
        let body = (frames.iter())
            .find(|f| f.1.name == name)
            .unwrap_or_else(|| panic!("`{name}` has a core body"))
            .1;
        t.relation(body)
            .unwrap_or_else(|u| panic!("`{name}` is {u:?}"))
    };
    let pairs: Vec<_> = (1..=n)
        .map(|i| {
            (
                rel(format!("{}{i}", prefix.0)),
                rel(format!("{}{i}", prefix.1)),
            )
        })
        .collect();
    (pairs.into_iter())
        .map(|(l, r)| {
            LEVELS.map(|lv| t.identity(l.root, &l.ty, lv).id == t.identity(r.root, &r.ty, lv).id)
        })
        .collect()
}

/// The 21 pairs of the research's `selftest.py` join at the levels it
/// states, but where the core already writes both sides alike: `a || b` is
/// the core's `if a { true } else { b }`, so pair 3 is one tree at layer 0.
#[test]
fn the_worked_pairs_join_at_their_levels() {
    let (f, t) = (false, true);
    let want: [[bool; 3]; 21] = [
        [f, f, t],
        [f, t, t],
        [t, t, t],
        [f, t, t],
        [f, t, t],
        [t, t, t],
        [f, f, t],
        [f, f, f],
        [f, f, t],
        [f, f, t],
        [f, f, f],
        [f, t, t],
        [f, f, t],
        [f, f, f],
        [f, f, f],
        [f, f, t],
        [f, f, f],
        [f, f, t],
        [f, t, t],
        [f, f, t],
        [f, f, t],
    ];
    assert_eq!(agree(("l", "r"), 21), want);
}

/// Over IEEE floats only the flips and `!=` as `!(==)` hold: `!(a < b)` is
/// not `b <= a`, and `a == a` is false at NaN.
#[test]
fn a_float_comparison_only_flips() {
    let got: Vec<bool> = agree(("fl", "fr"), 6).into_iter().map(|l| l[2]).collect();
    assert_eq!(got, [true, true, true, true, false, false]);
}
