//! The typed-by-construction judgment (`vyrn_lower::typed`) over the
//! corpus: every example and every entry point of an example project with a
//! `vyrn.json`. For every store into a validated place it asks what produced
//! the value. The type's constructor, a name already of the type, a proven
//! literal and a constant into a sized integer are the rule; any other producer
//! is a finding. Which crossings are validated is `vyrn_frontend::validate`'s
//! question, not this file's.
//!
//! `VYRN_TYPED_DUMP=<file>:<fn>` prints one body's judged stores. `<file>` is a
//! corpus file name, a substring of one, or a path to any `.vyrn` file.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use vyrn_frontend::project::Memo;

use vyrn_frontend::ast::{Program, Type, TypeDecl};
use vyrn_frontend::loader::DiskResolver;
use vyrn_lower::typed::{self, Step};

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

fn load(path: &Path, project: Option<&Path>) -> Result<(Program, Memo), String> {
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(slash(&repo_root().join("std"))),
        artifacts: project.and_then(manifest).and_then(|m| m.artifacts),
        ..Default::default()
    };
    Memo::load(|| vyrn_lower::load(&src, &slash(path), &opts, &DiskResolver))
        .map_err(|d| d.first().map(|d| d.render()).unwrap_or_default())
}

/// Every root to judge: the corpus `tests/effects.rs` judges.
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

fn decls(p: &Program) -> BTreeMap<String, TypeDecl> {
    p.type_decls
        .iter()
        .map(|d| (d.name.clone(), d.clone()))
        .collect()
}

/// The type one place step reaches from `base`. `None` for a generic parameter
/// or a type this program does not declare.
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
            // A String indexes as bytes.
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

/// Every body of `program` the core builds, the module-state initializer
/// included. The caller holds the projection memo open.
fn bodies_of(program: &Program) -> Vec<vyrn_frontend::core::Body> {
    let lowered = vyrn_lower::lower(program);
    let own = vyrn_lower::analyze(program);
    let mut bodies = Vec::new();
    for inst in &lowered.instances {
        if let Ok(b) = vyrn_lower::core::build(program, inst, &own) {
            bodies.push(b);
        }
    }
    if !program.globals.is_empty() {
        if let Ok(b) = vyrn_lower::core::build_module_state(program, &own, &lowered.globals) {
            bodies.push(b);
        }
    }
    bodies
}

/// Judges `refs`, with `program`'s declarations answering which types are
/// validated and what each place step holds. A call's type is the core's
/// (`Rhs::Call::ret`).
fn judge(program: &Program, refs: &[&vyrn_frontend::core::Body]) -> typed::Judged {
    let types = decls(program);
    let globals: BTreeMap<String, Type> = program
        .globals
        .iter()
        .filter_map(|g| g.ty.clone().map(|t| (g.name.clone(), t)))
        .collect();
    let map = types.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    typed::judge(refs, &mut |to| validated(to, &map), &mut |base, s| {
        step_ty(base, s, &types, &globals)
    })
}

/// Holds even where the declarations type no step to the place (an
/// unannotated global).
#[test]
fn a_read_of_a_validated_place_produces_its_type() {
    let dir = std::env::temp_dir().join("vyrn-typed-read");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("read.vyrn");
    std::fs::write(
        &path,
        "type Age = Int64 where value >= 18\n\
         type Roster = { ages: Array<Age> }\n\
         let mut shared = Roster { ages: [] }\n\
         fn main() -> Int64 {\n\
         shared.ages.push(Age(41))\n\
         let mut a = Age(20)\n\
         a = 30\n\
         print(shared.ages[0])\n\
         print(a)\n\
         return 0\n\
         }\n",
    )
    .unwrap();
    let (program, _memo) = load(&path, None).unwrap();
    let bodies = bodies_of(&program);
    let refs: Vec<&vyrn_frontend::core::Body> = bodies.iter().flat_map(|b| b.frames()).collect();
    let judged = judge(&program, &refs);
    let kind_of = |producer: &str| {
        judged
            .stores
            .iter()
            .filter(|s| refs[s.body].name == "main" && s.producer == producer)
            .map(|s| s.how.kind())
            .collect::<Vec<_>>()
    };
    assert_eq!(kind_of("shared.ages[]"), ["by-name"]);
    // Three proven constants, each a `Callee::Proven` row; `a = 30` also
    // stores the row's temporary into `a`, the fourth store.
    assert_eq!(kind_of("@lit"), ["by-literal"; 4]);
}

#[test]
fn an_uncalled_generic_calls_a_protocol_member_no_impl_answers() {
    let dir = common::scratch("typed-uncalled");
    let path = dir.join("uncalled.vyrn");
    std::fs::write(
        &path,
        "protocol Weigh {
         fn weigh(self) -> Int64
         }
         fn weighOne<T: Weigh>(x: T) -> Int64 {
         return x.weigh()
         }
         fn main() -> Int64 {
         return 0
         }
",
    )
    .unwrap();
    let out = common::vyrn().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "walks the whole corpus; run explicitly: cargo test -p vyrn-cli --test typed -- --ignored"]
fn the_typed_judgment_over_the_corpus() {
    std::thread::Builder::new()
        .stack_size(vyrn_frontend::trap::DEEP_STACK_BYTES)
        .spawn(run_corpus)
        .unwrap()
        .join()
        .unwrap();
}

fn run_corpus() {
    // Generation is the driver's engine; without it an example that imports
    // through a generator fails to load and the gate silently measures a
    // smaller corpus.
    vyrn_genwasm::install();
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
        let Ok((program, _memo)) = load(path, project.as_deref()) else {
            continue;
        };
        programs += 1;
        let file = path.file_name().unwrap().to_string_lossy().to_string();
        let bodies = bodies_of(&program);
        let refs: Vec<&vyrn_frontend::core::Body> =
            bodies.iter().flat_map(|b| b.frames()).collect();
        let judgement = judge(&program, &refs);
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
    // Zero is a fact about this corpus, not a refusal in the language: a raw
    // value entering a validated slot stays legal and every engine runs the
    // constructor at it.
    const RATCHET: usize = 0;
    assert_eq!(
        findings.len(),
        RATCHET,
        "a store into a validated place with a raw producer; the ratchet is          {RATCHET} and this one is worth reading before it is raised: {}",
        findings[0]
    );
    assert!(judged > 0, "the judgment judged nothing");
}

/// Names the rule `to` carries: a named type's `where` through `validate::of`,
/// or a sized integer's narrowing rows. Whether a store crossed it is the
/// judgment's.
fn validated(to: &Type, types: &std::collections::HashMap<String, TypeDecl>) -> Option<String> {
    match to {
        // `of`, not `required`: `required` exempts a store of a name already
        // of the type, which must still be judged and land as `by-name`.
        Type::Named(n) => vyrn_frontend::validate::of(types.get(n)).map(|d| d.name.clone()),
        // Likewise every sized-integer store is judged; `validate::narrows`
        // tells a same-width producer apart.
        Type::IntN { .. } => Some(to.to_string()),
        _ => None,
    }
}
