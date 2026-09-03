//! RFC-0125 M6, the third judgment — typed by construction, over the corpus.
//!
//! For every example, and for every entry point of the example projects that
//! carry a `vyrn.json`, every function instance is lowered into the named core
//! and judged (`vyrn_lower::typed`): for every store into a place whose type is
//! validated, what produced the value. Four answers are the rule — the type's
//! own constructor, a name already of the type, a literal the checker proved,
//! and a constant into a sized integer, which the `int-narrowing` row answers
//! rather than refuses — and every other answer is a finding, counted by kind
//! and ratcheted.
//!
//! WHICH crossings are validated is not decided here. The judgment asks
//! `vyrn_frontend::validate`, which is where the census of §3 M6 put the rule
//! so that all three engines ask one question, and this file hands it the two
//! types the core carries. The narrowing rows (`int-narrowing`, `float-to-int`)
//! are the second half: a store into a sized integer out of a numeric type of
//! another width is a crossing the same way, and `validate::narrows` decides
//! it. Since the third slice the core names the type of every producer, so
//! that question has both its halves at every store.
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
            Type::Array(t) | Type::ArrayN(t, _) | Type::SmallArray(t, _) => Some(*t),
            // A String indexes as bytes (RFC-0022's `string-index` row), and
            // its element is the byte.
            Type::Str => Some(Type::IntN {
                bits: 8,
                signed: false,
            }),
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
        // What a call answers is the core's now (`Rhs::Call::ret`), so this
        // file no longer keeps a table of return types beside the checker's.
        let judgement = typed::judge(&refs, &mut |to| validated(to, &map), &mut |base, s| {
            step_ty(base, s, &types, &globals)
        });
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
         validated place judged, {unjudged} unjudged (a store whose producer is a read of a \
         place these declarations resolve no type for)"
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
    // type's constructor, nor a name already of the type, nor a literal, nor a
    // constant into a sized integer. It may fall, never rise, and it is ZERO
    // since RFC-0125 §3 M6's fourth slice — so a finding is a REFUSAL here and
    // not a row in a record. Six before it: three record literals, which are a
    // validated record type's own second producer and read as constructors now,
    // and three sites the corpus rewrote to call the constructor the boundary
    // was going to run anyway.
    //
    // What zero does NOT mean is that the language refuses the shape. A raw
    // value entering a validated slot is RFC-0003's automatic validation and
    // stays legal; every engine runs the constructor at it
    // (`rfcs/probes-0125/raw-value-into-a-validated-slot.vyrn` refuses on all
    // three, in the census's words). Zero is a fact about this corpus.
    const RATCHET: usize = 0;
    assert_eq!(
        findings.len(),
        RATCHET,
        "a store into a validated place with a raw producer; the ratchet is          {RATCHET} and this one is worth reading before it is raised: {}",
        findings[0]
    );
    assert!(judged > 0, "the judgment judged nothing");
}

/// Which types carry a rule, asked where the rule is stated: a named type's
/// `where` through `validate::of`, and a sized integer through the census's
/// two narrowing rows. Whether a given store crossed it is the judgment's.
fn validated(to: &Type, types: &std::collections::HashMap<String, TypeDecl>) -> Option<String> {
    match to {
        // Asked in the interpreter's form — `of`, the declaration's own
        // question — rather than `required`'s, so that a store of a name
        // ALREADY of the type is judged and lands as `by-name` instead of
        // vanishing. The two forms differ by exactly that exemption (finding 5
        // of the census), and a judgment that skipped it would report only the
        // crossings it dislikes.
        Type::Named(n) => vyrn_frontend::validate::of(types.get(n)).map(|d| d.name.clone()),
        // The narrowing rows, for the same reason: a sized integer IS the
        // row, whatever produced the value, so a store out of a producer
        // already at that width is judged and lands as `by-name` rather than
        // vanishing. `validate::narrows` tells the two apart, and the judgment
        // reads it there.
        Type::IntN { .. } => Some(to.to_string()),
        _ => None,
    }
}
