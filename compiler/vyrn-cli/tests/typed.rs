//! RFC-0125 M6, the third judgment's second slice — typed by construction,
//! over the corpus.
//!
//! For every example, and for every entry point of the example projects that
//! carry a `vyrn.json`, every function instance is lowered into the named core
//! and judged (`vyrn_lower::typed`): for every store into a place whose type is
//! validated, what produced the value. Three answers are the rule — the type's
//! own constructor, a name already of the type, a literal the checker proved —
//! and every other answer is a finding, counted by kind and ratcheted.
//!
//! WHICH crossings are validated is not decided here. The judgment asks
//! `vyrn_frontend::validate`, which is where the census of §3 M6 put the rule
//! so that all three engines ask one question, and this file hands it the two
//! types the core carries. The narrowing rows (`int-narrowing`, `float-to-int`)
//! are the second half: a store into a sized integer out of a WIDER numeric
//! type is a crossing the same way, and `validate::narrows` decides it.
//!
//! `VYRN_TYPED_DUMP=<file>:<fn>` prints one body's judged stores, as
//! `VYRN_EFFECTS_DUMP` prints one function's effects. `<file>` is a corpus file
//! name (or a substring of one) or a path to any `.vyrn` file.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use vyrn_frontend::ast::{Program, Type, TypeDecl};
use vyrn_lower::typed::{self, Step};

struct Fs;

impl vyrn_frontend::loader::ModuleResolver for Fs {
    fn read(&self, resolved: &str) -> Result<String, String> {
        std::fs::read_to_string(resolved).map_err(|e| e.to_string())
    }
    fn list(&self, resolved: &str) -> Result<Vec<String>, String> {
        list_dir(resolved, false)
    }
    fn list_kinds(&self, resolved: &str) -> Result<Vec<String>, String> {
        list_dir(resolved, true)
    }
}

/// The listing a generator asks for at generation time, as the CLI's resolver
/// answers it (the shape `tests/effects.rs` uses).
fn list_dir(dir: &str, kinds: bool) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(dir).map_err(|_| vyrn_frontend::trap::io_at("listerr", dir))?;
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if kinds && e.file_type().is_ok_and(|t| t.is_dir()) {
                format!("{name}/")
            } else {
                name
            }
        })
        .collect();
    names.sort();
    Ok(names)
}

fn repo_root() -> PathBuf {
    let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    d.pop();
    d.pop();
    d
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn manifest(dir: &Path) -> Option<vyrn_frontend::manifest::Manifest> {
    vyrn_frontend::manifest::find(dir).ok().flatten()
}

fn load(path: &Path, project: Option<&Path>) -> Result<Program, String> {
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(slash(&repo_root().join("std"))),
        artifacts: project.and_then(manifest).and_then(|m| m.artifacts),
        ..Default::default()
    };
    vyrn_frontend::load(&src, &slash(path), &opts, &Fs)
        .map_err(|d| d.first().map(|d| d.render()).unwrap_or_default())
}

/// Every root to judge — the corpus `tests/effects.rs` judges.
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

/// The program's declarations, keyed as `validate::required` wants them.
fn decls(p: &Program) -> BTreeMap<String, TypeDecl> {
    p.type_decls
        .iter()
        .map(|d| (d.name.clone(), d.clone()))
        .collect()
}

/// A record type's field, an array's element, a map's value, a global's type —
/// the steps a place takes. `None` where the step does not resolve, which is
/// the answer for a generic parameter and for a type this program does not
/// declare.
fn step_ty(
    base: Option<&Type>,
    s: Step,
    types: &BTreeMap<String, TypeDecl>,
    globals: &BTreeMap<String, Type>,
) -> Option<Type> {
    let resolve = |t: &Type| -> Type {
        match t {
            Type::Named(n) => types.get(n).map(|d| d.base.clone()).unwrap_or(t.clone()),
            other => other.clone(),
        }
    };
    match s {
        Step::Global(g) => globals.get(g).cloned(),
        Step::Field(f) => match resolve(base?) {
            Type::Record(fields) => fields.iter().find(|x| x.name == f).map(|x| x.ty.clone()),
            _ => None,
        },
        Step::Elem => match resolve(base?) {
            Type::Array(t) => Some(*t),
            _ => None,
        },
        Step::Key => match resolve(base?) {
            Type::Map(_, v) => Some(*v),
            _ => None,
        },
    }
}

#[test]
fn the_typed_judgment_over_the_corpus() {
    std::thread::Builder::new()
        .stack_size(vyrn_frontend::interp::INTERP_STACK_BYTES)
        .spawn(run_corpus)
        .unwrap()
        .join()
        .unwrap();
}

fn run_corpus() {
    vyrn_lower::install();
    let dump = std::env::var("VYRN_TYPED_DUMP").ok();
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

    let mut by_kind: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    let mut findings: Vec<String> = Vec::new();
    let mut unjudged = 0usize;
    let mut programs = 0usize;
    let mut judged = 0usize;
    for (path, project) in &roots {
        let Ok(program) = load(path, project.as_deref()) else {
            continue;
        };
        programs += 1;
        let file = path.file_name().unwrap().to_string_lossy().to_string();
        let types = decls(&program);
        let globals: BTreeMap<String, Type> = program
            .globals
            .iter()
            .filter_map(|g| g.ty.clone().map(|t| (g.name.clone(), t)))
            .collect();
        let _memo = vyrn_frontend::project::Memo::open();
        let lowered = vyrn_lower::lower(&program);
        let own = vyrn_frontend::own::analyze(&program);
        let mut bodies = Vec::new();
        for inst in &lowered.instances {
            if let Ok(b) = vyrn_lower::core::build(&program, inst, &own) {
                bodies.push(b);
            }
        }
        if program.globals.is_empty() {
            // (The module-state initializer is a body and no function; it is
            // built below only where there are globals to initialize.)
        } else if let Ok(b) = vyrn_lower::core::build_module_state(&program, &own, &lowered.globals)
        {
            bodies.push(b);
        }
        let refs: Vec<&vyrn_lower::core::Body> = bodies.iter().flat_map(|b| b.frames()).collect();
        let map = types.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        // What each function answers, for the producer of a store that is a
        // call. A builtin is in no table and answers nothing, which is what
        // `Judged::unjudged` counts.
        let rets: BTreeMap<String, Type> = program
            .functions
            .iter()
            .map(|f| (f.name.clone(), f.ret.clone()))
            .collect();
        let judgement = typed::judge(
            &refs,
            &mut |from, to| validated(from, to, &map),
            &mut |base, s| step_ty(base, s, &types, &globals),
            &mut |callee, args| answers(callee, args, &rets, &types),
        );
        unjudged += judgement.unjudged;
        judged += judgement.stores.len();
        for s in &judgement.stores {
            *by_kind.entry(s.how.kind()).or_default() += 1;
            *by_type.entry(s.ty.clone()).or_default() += 1;
            if s.how.is_finding() {
                let root = format!("{}/", slash(&repo_root()));
                findings.push(format!(
                    "{}:{} {} — `{}` into `{}`: `{}` = {}",
                    refs[s.body]
                        .file
                        .as_deref()
                        .map(|f| f.trim_start_matches(&root).to_string())
                        .unwrap_or_else(|| file.clone()),
                    s.line,
                    refs[s.body].name,
                    s.how.kind(),
                    s.ty,
                    s.place,
                    s.producer
                ));
            }
        }
        if let Some((_, want)) = dump_target {
            for (i, b) in refs.iter().enumerate() {
                if b.name != *want {
                    continue;
                }
                eprintln!("{file} {}:", b.name);
                for s in judgement.stores.iter().filter(|s| s.body == i) {
                    eprintln!(
                        "  {:5} {:16} {} : {} = {}",
                        s.line,
                        s.how.kind(),
                        s.place,
                        s.ty,
                        s.producer
                    );
                }
            }
        }
    }

    eprintln!(
        "typed by construction over the corpus: {programs} programs, {judged} stores into a \
         validated place judged, {unjudged} unjudged (a primitive into a sized integer — the \
         core carries no type for one)"
    );
    for (k, n) in &by_kind {
        eprintln!("  {n:5}  {k}");
    }
    for (t, n) in &by_type {
        eprintln!("  type {n:5}  {t}");
    }
    findings.sort();
    for f in findings.iter().take(60) {
        eprintln!("  finding: {f}");
    }
    // The ratchet: a store into a validated place whose producer is neither the
    // type's constructor, nor a name already of the type, nor a literal. It may
    // fall, never rise. Six when this slice landed — one primitive, two reads
    // of a place, three record literals — and RFC-0125 §3 M6 records each with
    // its program and line. None of the six is a value that reaches its slot
    // unchecked: `rfcs/probes-0125/raw-value-into-a-validated-slot.vyrn` runs
    // all three shapes on all three engines and each refuses in the census's
    // words. They are the sites the boundary check exists for, which is what
    // §2.3's constructor removes.
    const RATCHET: usize = 6;
    assert!(
        findings.len() <= RATCHET,
        "{} stores into a validated place with a raw producer, more than the {RATCHET} \
         recorded; the first is worth reading before the number is raised: {}",
        findings.len(),
        findings[0]
    );
    assert!(judged > 0, "the judgment judged nothing");
}

/// What a call answers. A declared function answers what it declares. A
/// builtin declares nothing here, and the three the corpus stores through hand
/// their RECEIVER's value back — `xs.copy()` of a `Title` is a `Title`,
/// `xs[i]` and `xs.swapRemove(i)` are the array's element — so their answer is
/// read off the argument. Every other builtin answers nothing, and a store
/// whose producer answers nothing is unjudged rather than guessed at.
fn answers(
    callee: &str,
    args: &[Option<Type>],
    rets: &BTreeMap<String, Type>,
    types: &BTreeMap<String, TypeDecl>,
) -> Option<Type> {
    if let Some(t) = rets.get(callee) {
        return Some(t.clone());
    }
    let arg0 = args.first().cloned().flatten();
    match callee {
        "@copy" => arg0,
        "@at" | "@swapRemove" | "@pop" => match arg0.map(|t| resolve(t, types)) {
            Some(Type::Array(e)) | Some(Type::ArrayN(e, _)) | Some(Type::SmallArray(e, _)) => {
                Some(*e)
            }
            Some(Type::Map(_, v)) => Some(*v),
            _ => None,
        },
        _ => None,
    }
}

/// A named type stepped down to the type it is declared over.
fn resolve(t: Type, types: &BTreeMap<String, TypeDecl>) -> Type {
    match &t {
        Type::Named(n) => types.get(n).map(|d| d.base.clone()).unwrap_or(t),
        _ => t,
    }
}

/// The rule, asked where it is stated. A named type's `where` is
/// `validate::required`'s question (the interpreter's form when the core
/// carries no source type); a narrowing into a sized integer is
/// `validate::narrows`.
fn validated(
    from: Option<&Type>,
    to: &Type,
    types: &std::collections::HashMap<String, TypeDecl>,
) -> Option<String> {
    match to {
        // Asked in the interpreter's form — `of`, the declaration's own
        // question — rather than `required`'s, so that a store of a name
        // ALREADY of the type is judged and lands as `by-name` instead of
        // vanishing. The two forms differ by exactly that exemption (finding 5
        // of the census), and a judgment that skipped it would report only the
        // crossings it dislikes.
        Type::Named(n) => vyrn_frontend::validate::of(types.get(n)).map(|d| d.name.clone()),
        // The narrowing rows. With a source type in hand the rule is
        // `validate::narrows`; with none, the judgment has already decided the
        // producer names the type (a conversion call) or is a literal, and the
        // store is the row either way.
        Type::IntN { .. } => match from {
            Some(f) => vyrn_frontend::validate::narrows(f, to).then(|| to.to_string()),
            None => Some(to.to_string()),
        },
        _ => None,
    }
}
