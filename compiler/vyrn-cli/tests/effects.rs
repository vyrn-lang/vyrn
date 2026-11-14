//! Compares the effect judgment (`vyrn_lower::effects`) with the floor
//! and the audience pass over the corpus: every example and every
//! entry point of an example project. A function's effect set is the join of its atoms
//! and its callees', to a fixpoint. The floor answers per function body here (it unions
//! per module); the audience answers for the declaring module. The disagreeing kinds sum
//! to the ratchet. `VYRN_EFFECTS_GAPS=<substring>` lists where an instance has no core;
//! `VYRN_EFFECTS_DUMP=<file>:<fn>` prints one function's effects and their callees, where
//! `<file>` is a corpus file name, a substring of one, or a path to any `.vyrn` file.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use vyrn_frontend::ast::Program;
use vyrn_frontend::audience::{self, Audience};
use vyrn_frontend::floor::{self, Capability};
use vyrn_frontend::loader::DiskResolver;
use vyrn_lower::effects::{self, Effect, Effects};

fn repo_root() -> PathBuf {
    let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    d.pop();
    d.pop();
    d
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Loads a root under its project's `artifacts` map, as `vyrn check` does, so the floor
/// decides on a declared entry the same way.
fn load(path: &Path, project: Option<&Path>) -> Result<Program, String> {
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(slash(&repo_root().join("std"))),
        artifacts: project.and_then(manifest).and_then(|m| m.artifacts),
        expansions: vyrn_frontend::project::Expansions::shared(),
        ..Default::default()
    };
    // A floor refusal names the carrier in the note.
    vyrn_lower::load(
        &src,
        &slash(path),
        &opts,
        &DiskResolver,
        Some(&*vyrn_genwasm::engine()),
    )
    .map_err(|d| {
        d.first()
            .map(|d| match &d.note {
                Some(n) => format!(
                    "{}
  note: {n}",
                    d.render()
                ),
                None => d.render(),
            })
            .unwrap_or_else(|| "load failed".into())
    })
}

/// `None` for a directory with no `vyrn.json`.
fn manifest(dir: &Path) -> Option<vyrn_frontend::manifest::Manifest> {
    vyrn_frontend::manifest::find(dir).ok().flatten()
}

/// Every root to judge: `examples/*.vyrn`, then each entry point of each
/// `examples/*/vyrn.json` project.
fn corpus() -> Vec<(PathBuf, Option<PathBuf>)> {
    let ex = repo_root().join("examples");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&ex)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
        .collect();
    files.sort();
    let mut out: Vec<(PathBuf, Option<PathBuf>)> = files.into_iter().map(|p| (p, None)).collect();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&ex)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("vyrn.json").is_file())
        .collect();
    dirs.sort();
    for dir in dirs {
        let Some(m) = manifest(&dir) else { continue };
        let mut entries: BTreeSet<String> = BTreeSet::new();
        if let Some(a) = &m.audience {
            entries.extend(a.entries.iter().map(|(p, _, _)| p.clone()));
        }
        if let Some(a) = &m.artifacts {
            entries.extend(a.list.iter().map(|a| a.entry.clone()));
        }
        for e in entries {
            out.push((PathBuf::from(e), Some(dir.clone())));
        }
    }
    assert!(!out.is_empty(), "no examples found");
    out
}

/// How a function lands against the floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FloorKind {
    /// The body carries what the judgment computes.
    Agree,
    /// The judgment has more and a callee's body carries it, so the floor's union
    /// over the import closure agrees.
    CalleeCarried,
    /// A `gen fn` body: the floor skips it (it runs against the compiler's
    /// filesystem) and the judgment sees its reads. The verdict agrees.
    GenBody,
    /// The floor sees a call the core does not lower (a lambda body). A disagreement.
    CoreBlind,
    /// The judgment has more and no body in the program carries it, so the floor
    /// misses a program this function makes unbuildable. A disagreement.
    FloorBlind,
}

/// How a function lands against the audience fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AudienceKind {
    /// The project declares no audience, or the module is outside it (std, a remote).
    NoFence,
    /// Server-only with an effect a browser lacks, client-only with an
    /// extern, or universal with neither.
    Agree,
    /// Server-only or client-only with no target-restricted effect: the fence
    /// protects a declaration, not an effect. Not a disagreement.
    DeclaredOnly,
    /// Universal or client-only with an effect a browser lacks: the fence lets a
    /// client import it, and only a declared artifact's floor refuses. A disagreement.
    Unfenced,
    /// Server-only with an extern a native target lacks. A disagreement.
    ServerExtern,
}

struct Row {
    file: String,
    module: String,
    name: String,
    line: usize,
    effects: Effects,
    floor: FloorKind,
    audience: AudienceKind,
    /// What decided the audience kind, for the printout.
    who: String,
}

/// The floor capabilities a set of effects needs, by `floor::Capability::of`, the one
/// statement of the mapping.
fn caps_of(e: Effects) -> BTreeSet<Capability> {
    e.iter().filter_map(Capability::of).collect()
}

/// The floor's rule at function grain: the carriers the body spells, on any branch. A
/// `gen fn` carries nothing, as in `floor::carried`. `externs` is the program's host
/// imports, because a call to one is the `extern` carrier.
fn floor_carries(
    externs: &std::collections::HashSet<String>,
    f: &vyrn_frontend::ast::Function,
) -> BTreeSet<Capability> {
    let mut out = BTreeSet::new();
    if f.is_gen {
        return out;
    }
    let mut body = f.block().clone();
    vyrn_frontend::project::walk_block(&mut body, &mut |e| {
        if let vyrn_frontend::ast::Expr::Call { name, .. } = e {
            if let Some(cap) = floor::call_carrier(name, externs) {
                out.insert(cap);
            }
        }
    });
    out
}

/// Whether the effect set needs something a browser page lacks. A page has `extern`,
/// so the target's own capability row answers.
fn browser_lacks(e: Effects) -> bool {
    let has = floor::capabilities(vyrn_frontend::artifacts::Target::Browser);
    caps_of(e).iter().any(|c| !has.contains(c))
}

#[test]
#[ignore = "walks the whole corpus; run explicitly: cargo test -p vyrn-cli --test effects -- --ignored"]
fn the_effect_judgment_over_the_corpus() {
    std::thread::Builder::new()
        .stack_size(vyrn_frontend::trap::DEEP_STACK_BYTES)
        .spawn(run_corpus)
        .unwrap()
        .join()
        .unwrap();
}

fn run_corpus() {
    // Generation is the driver's engine, not the frontend's. Without it an example
    // that imports through a generator fails to link and the gate measures less.
    let dump = std::env::var("VYRN_EFFECTS_DUMP").ok();
    // The LAST colon: a Windows path carries one after its drive letter.
    let dump_target = dump.as_deref().and_then(|d| d.rsplit_once(':'));
    let mut roots = corpus();
    if let Some((file, _)) = dump_target {
        let p = PathBuf::from(file);
        if p.is_file() && !roots.iter().any(|(r, _)| r == &p) {
            let dir = p.parent().map(Path::to_path_buf);
            roots = vec![(p, dir.filter(|d| d.join("vyrn.json").is_file()))];
        } else {
            roots.retain(|(r, _)| slash(r).contains(file));
        }
    }

    let mut rows: Vec<Row> = Vec::new();
    let mut unknown: BTreeMap<String, usize> = BTreeMap::new();
    // Calls through a function value the sources answered for.
    let mut through_calls = 0usize;
    // Calls through a function value no value of which exists in the program:
    // such a call cannot run.
    let mut empty_sets: Vec<String> = Vec::new();
    let mut empty_calls = 0usize;
    // The floor rows the judgment states. Each must answer as the pass does,
    // function by function, or a refusal changed with the derivation.
    let mut judged_agree = 0usize;
    let mut judged_gen = 0usize;
    let mut judged_carried = 0usize;
    let mut judged_differ: Vec<String> = Vec::new();
    let mut gaps: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut unloadable = 0usize;
    let mut refused = 0usize;
    let mut programs = 0usize;
    for (path, project) in &roots {
        let program = match load(path, project.as_deref()) {
            Ok(p) => p,
            Err(e) => {
                // A project entry must load, unless its artifact's floor refusal
                // is the recorded one. A lone example that needs the root
                // manifest's remote dependencies is counted.
                if let Some(dir) = project {
                    let rel = format!(
                        "{}/{}",
                        dir.file_name().unwrap().to_string_lossy(),
                        path.strip_prefix(dir).map(slash).unwrap_or_default()
                    );
                    let recorded = common::EXPECTED_PROJECT_CHECK_FAILURE
                        .iter()
                        .find(|(entry, _, _)| *entry == rel);
                    match recorded {
                        Some((_, _, needle)) if e.contains(needle) => {
                            refused += 1;
                            continue;
                        }
                        _ => panic!("{} did not load: {e}", slash(path)),
                    }
                }
                unloadable += 1;
                continue;
            }
        };
        programs += 1;
        let man = project.as_deref().and_then(manifest);
        let root_key = slash(path);
        let file = match project {
            Some(dir) => format!(
                "{}/{}",
                dir.file_name().unwrap().to_string_lossy(),
                path.strip_prefix(dir)
                    .map(slash)
                    .unwrap_or_else(|_| root_key.clone())
            ),
            None => path.file_name().unwrap().to_string_lossy().to_string(),
        };
        let world = vyrn_lower::analyze(&program);
        let lowered = vyrn_lower::lower(&program);
        let survey = vyrn_lower::effects::survey(&program, &world, &lowered);
        through_calls += survey.through;
        empty_calls += survey.empty.len();
        for (what, n) in &survey.gaps {
            *gaps.entry(what).or_default() += n;
        }
        empty_sets.extend(
            survey
                .empty
                .iter()
                .map(|(body, c, line)| format!("{file}:{line} {c} (in {body})")),
        );
        for (body, name, line) in &survey.unknown {
            *unknown
                .entry(format!("{file}:{line} {name} (in {body})"))
                .or_default() += 1;
        }

        // The floor's union over the whole program: what its closure check
        // would see for any artifact rooted here.
        let externs = floor::extern_imports(&program);
        let mut program_carries: BTreeSet<Capability> = BTreeSet::new();
        for f in &program.functions {
            program_carries.extend(floor_carries(&externs, f));
        }
        for im in &program.impls {
            for m in im.methods.iter().chain(im.places.iter()) {
                program_carries.extend(floor_carries(&externs, m));
            }
        }

        for (inst, e) in &survey.sets {
            let (inst, e) = (*inst, *e);
            let want = caps_of(e);
            let have = floor_carries(&externs, inst.func);
            let floor = if inst.func.is_gen && !want.is_empty() {
                FloorKind::GenBody
            } else if have == want {
                FloorKind::Agree
            } else if have.is_subset(&want) {
                if want.difference(&have).all(|c| program_carries.contains(c)) {
                    FloorKind::CalleeCarried
                } else {
                    FloorKind::FloorBlind
                }
            } else {
                FloorKind::CoreBlind
            };

            // `gen-body` and `callee-carried` are not disagreements, as in `FloorKind`.
            let (hj, wj) = (&have, &want);
            if hj == wj {
                judged_agree += 1;
            } else if inst.func.is_gen && hj.is_empty() {
                judged_gen += 1;
            } else if hj.is_subset(&wj) && wj.difference(&hj).all(|c| program_carries.contains(c)) {
                judged_carried += 1;
            } else {
                judged_differ.push(format!(
                    "{file}:{} {} — the pass says {hj:?}, the judgment says {wj:?}",
                    inst.func.line,
                    inst.spelling()
                ));
            }

            let module_key = if inst.module().is_empty() {
                root_key.clone()
            } else {
                inst.module().to_string()
            };
            let (audience_kind, who) = match man.as_ref().and_then(|m| m.audience.as_ref()) {
                None => (AudienceKind::NoFence, String::new()),
                Some(map) => {
                    let v = audience::audience_of(&module_key, map, &mut |p| map.real(p));
                    let inside = !map.base.is_empty() && module_key.starts_with(&map.base);
                    let lacks = browser_lacks(e);
                    let ext = e.has(Effect::Extern);
                    let kind = match v.audience {
                        _ if !inside => AudienceKind::NoFence,
                        Audience::Server if ext => AudienceKind::ServerExtern,
                        Audience::Server if lacks => AudienceKind::Agree,
                        Audience::Server => AudienceKind::DeclaredOnly,
                        Audience::Client if lacks => AudienceKind::Unfenced,
                        Audience::Client if ext => AudienceKind::Agree,
                        Audience::Client => AudienceKind::DeclaredOnly,
                        Audience::Universal if lacks => AudienceKind::Unfenced,
                        Audience::Universal => AudienceKind::Agree,
                    };
                    (kind, format!("{} — {}", v.audience.phrase(), v.because()))
                }
            };
            if dump_target.is_some_and(|(_, want_fn)| inst.func.name == want_fn) {
                let at = if inst.module().is_empty() {
                    "<root>"
                } else {
                    inst.module()
                };
                eprintln!("{file}: {} in {at} — {e}", inst.spelling());
                eprintln!(
                    "  floor: {floor:?} (body carries {have:?}); audience: {audience_kind:?} {who}"
                );
            }
            rows.push(Row {
                file: file.clone(),
                module: module_key,
                name: inst.spelling(),
                line: inst.func.line,
                effects: e,
                floor,
                audience: audience_kind,
                who,
            });
        }
    }

    let mut floor_kinds: BTreeMap<FloorKind, usize> = BTreeMap::new();
    let mut audience_kinds: BTreeMap<AudienceKind, usize> = BTreeMap::new();
    let mut per_effect: BTreeMap<Effect, usize> = BTreeMap::new();
    let mut pure = 0usize;
    for r in &rows {
        *floor_kinds.entry(r.floor).or_default() += 1;
        *audience_kinds.entry(r.audience).or_default() += 1;
        if r.effects.is_pure() {
            pure += 1;
        }
        for e in r.effects.iter() {
            *per_effect.entry(e).or_default() += 1;
        }
    }
    let unlowered: usize = gaps.values().sum();
    eprintln!(
        "effects over the corpus: {programs} programs ({unloadable} not loadable here, \
         {refused} refused as recorded), {} functions judged, {pure} pure, {unlowered} unlowered, \
         {through_calls} calls through a function value judged over their sources, \
         {empty_calls} through one whose set is empty, {} unattributed",
        rows.len(),
        unknown.values().sum::<usize>()
    );
    empty_sets.sort();
    empty_sets.dedup();
    eprintln!("  empty sets: {}", empty_sets.len());
    for o in &empty_sets {
        eprintln!("    {o}");
    }
    for (what, n) in &gaps {
        eprintln!("  unlowered: {n:5}  {what}");
    }
    for (e, n) in &per_effect {
        eprintln!("  effect {n:5}  {}", e.name());
    }
    eprintln!("  floor:");
    for (k, n) in &floor_kinds {
        eprintln!("    {n:5}  {k:?}");
    }
    eprintln!("  audience:");
    for (k, n) in &audience_kinds {
        eprintln!("    {n:5}  {k:?}");
    }
    eprintln!(
        "  judged:     {judged_agree} agree, {judged_carried} callee-carried, {judged_gen} gen-body, {} differ",
        judged_differ.len()
    );
    for d in judged_differ.iter().take(20) {
        eprintln!("    {d}");
    }
    let disagreements: Vec<&Row> = rows
        .iter()
        .filter(|r| {
            matches!(r.floor, FloorKind::CoreBlind | FloorKind::FloorBlind)
                || matches!(
                    r.audience,
                    AudienceKind::Unfenced | AudienceKind::ServerExtern
                )
        })
        .collect();
    for r in disagreements.iter().take(60) {
        eprintln!(
            "  disagreement: {}:{} {} — {} — floor {:?}, audience {:?} {}",
            r.file, r.line, r.name, r.effects, r.floor, r.audience, r.who
        );
    }
    for (name, n) in &unknown {
        eprintln!("  unattributed {n:5}  {name}");
    }
    // Exact, not a bound: an unattributed call is a hole in the gate, not a number
    // to raise.
    const UNATTRIBUTED: usize = 0;
    assert_eq!(
        unknown.values().sum::<usize>(),
        UNATTRIBUTED,
        "calls the judgment could not attribute; the first: {}",
        unknown.keys().next().map(String::as_str).unwrap_or("none")
    );
    // An empty set is an answer, not a hole.
    const EMPTY_SETS: usize = 3;
    assert_eq!(
        empty_sets.len(),
        EMPTY_SETS,
        "calls through a function value no value of which exists in their program:\n{}",
        empty_sets.join("\n")
    );
    if std::env::var("VYRN_EFFECTS_MODULES").is_ok() {
        let mut by_module: BTreeMap<&str, Effects> = BTreeMap::new();
        for r in &rows {
            let e = by_module.entry(r.module.as_str()).or_default();
            *e = e.join(r.effects);
        }
        for (m, e) in by_module {
            eprintln!("  module {m}: {e}");
        }
    }
    // The disagreements, by function. It may fall, never rise.
    const RATCHET: usize = 0;
    assert!(
        disagreements.len() <= RATCHET,
        "{} functions where a pass and the effect judgment disagree, more than the {RATCHET} recorded; \
         the first new one is worth reading before the number is raised: {}:{} {}",
        disagreements.len(),
        disagreements[0].file,
        disagreements[0].line,
        disagreements[0].name
    );
    assert!(
        judged_differ.is_empty(),
        "{} functions where the moved floor rows and the judgment disagree; the first: {}",
        judged_differ.len(),
        judged_differ[0]
    );
    assert_eq!(
        refused,
        common::EXPECTED_PROJECT_CHECK_FAILURE.len(),
        "every registered project refusal is in the corpus"
    );
    assert!(!rows.is_empty(), "the judgment judged nothing");
    let _ = &rows[0].module;
}

/// The effect lattice, one row per atom: its effect, and whether a generator
/// may call it. A second statement of `effects::atoms` and `gen_allows` on
/// purpose: an edit to either is a changed refusal, and fails here until this
/// table moves with it.
const LATTICE: &[(&str, &str, bool)] = &[
    ("runtime$malloc", "alloc", true),
    ("mem$grow", "alloc", true),
    ("readLine", "read-input", false),
    ("print", "write-output", false),
    ("writeStdout", "write-output", false),
    ("@trace", "write-output", false),
    ("@debug", "write-output", false),
    ("@info", "write-output", false),
    ("@warn", "write-output", false),
    ("@error", "write-output", false),
    ("readFile", "fs-read", true),
    ("readFileBytes", "fs-read", false),
    ("writeFile", "fs-write", false),
    ("writeFileBytes", "fs-write", false),
    ("renameFile", "fs-write", false),
    ("fsyncFile", "fs-write", false),
    ("listDir", "fs-list", true),
    ("listDirKinds", "fs-list", true),
    ("args", "args", false),
    ("serveStream", "serve", false),
    ("hostSleep", "waits", false),
    ("hostFetch", "net", false),
    ("hostResume", "trap", true),
    ("panic", "trap", true),
    ("@panicAt", "trap", true),
    ("assert", "trap", true),
    ("assertEq", "trap", true),
    ("runtime$trap", "trap", true),
    ("mem$trap", "trap", true),
    ("moduleInterface", "gen-only", true),
    ("moduleCode", "gen-only", true),
    ("contractOf", "gen-only", true),
    ("lex", "gen-only", true),
    ("codeRuns", "gen-only", true),
    ("vyrnModule", "gen-only", true),
    ("vyrnType", "gen-only", true),
    ("syntaxParser", "gen-only", true),
    ("render", "gen-only", true),
    ("name", "gen-only", true),
    ("diag", "gen-only", true),
    ("codeAt", "gen-only", true),
    ("at", "gen-only", true),
    ("@codeQuote", "gen-only", true),
];

#[test]
fn the_lattice_is_the_table() {
    let from_table: BTreeSet<(String, Effect)> = LATTICE
        .iter()
        .map(|(n, e, _)| {
            let effect = Effect::parse(e).unwrap_or_else(|| panic!("`{e}` is not an effect"));
            (n.to_string(), effect)
        })
        .collect();
    let from_code: BTreeSet<(String, Effect)> =
        effects::atoms().map(|(n, e)| (n.to_string(), e)).collect();
    let only_table: Vec<_> = from_table.difference(&from_code).collect();
    let only_code: Vec<_> = from_code.difference(&from_table).collect();
    assert!(
        only_table.is_empty() && only_code.is_empty(),
        "LATTICE and effects::atoms() differ; in the table only: {only_table:?}; in the code only: {only_code:?}"
    );
    // The generation fence asks `gen_allows` alone, so an edited row is a changed refusal.
    let wrong: Vec<String> = LATTICE
        .iter()
        .filter(|(n, _, allowed)| effects::gen_allows(n) != *allowed)
        .map(|(n, _, allowed)| format!("`{n}`: the table says {allowed}"))
        .collect();
    assert!(
        wrong.is_empty(),
        "the table's `gen` column and the code differ: {}",
        wrong.join("; ")
    );
}
