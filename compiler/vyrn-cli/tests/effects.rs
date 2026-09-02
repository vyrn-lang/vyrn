//! RFC-0125 M6, first slice — the effect judgment beside the audience pass
//! (RFC-0072) and the floor (RFC-0103), over the corpus.
//!
//! For every example, and for every entry point of the four example projects
//! that carry a `vyrn.json`, every function instance is lowered into the named
//! core and judged (`vyrn_lower::effects`): its effect set is the join of its
//! own atoms and its callees', to a fixpoint. Beside it stand the two passes'
//! answers for the same function, computed by the passes' own functions:
//!
//!   - the floor's: which of `fs`, `stdin`, `args` the function's BODY carries,
//!     by the floor's rule (presence of a `floor::CALLS` name; a `gen fn` body
//!     is skipped) — per function here, where the floor unions per module;
//!   - the audience's: the verdict of `audience::audience_of` for the module
//!     the function was declared in, under the project's manifest.
//!
//! Every function lands in one floor kind and one audience kind. The kinds
//! that are disagreements — a verdict the effect judgment and a pass would
//! give differently on some program — sum to the RATCHET; the kinds that are
//! not (a rule that is not an effect, a context that differs while the
//! verdict agrees) are tallied and printed but not ratcheted. RFC-0125 §3 M6
//! lists each kind as a numbered finding with the program and line.
//!
//! `VYRN_EFFECTS_DUMP=<file>:<fn>` prints one function's effect set and the
//! callees it took them from; `<file>` is a corpus file name (or a substring
//! of one) or a path to any `.vyrn` file.
//!
//! The lattice is stated once, as the table in RFC-0125 §3 M6. The first
//! test reads it out of the RFC and holds `effects::ATOMS` equal to it.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use vyrn_frontend::ast::{Program, Type};
use vyrn_frontend::audience::{self, Audience};
use vyrn_frontend::floor::{self, Capability};
use vyrn_lower::effects::{self, Callee, Effect, Effects};

struct Fs;

/// The listing a generator asks for at generation time (`std/ui` walks its
/// routes directory), as the CLI's resolver answers it: bare names, sorted,
/// a directory's with a trailing `/` when kinds are asked for.
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

fn repo_root() -> PathBuf {
    let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    d.pop();
    d.pop();
    d
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// A root, loaded as the CLI loads it: under its project's `artifacts` map
/// when it has one, so the floor (RFC-0103) decides on a declared entry
/// here as it does at `vyrn check`.
fn load(path: &Path, project: Option<&Path>) -> Result<Program, String> {
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(slash(&repo_root().join("std"))),
        artifacts: project.and_then(manifest).and_then(|m| m.artifacts),
        ..Default::default()
    };
    // The message and its note: a floor refusal names the carrier in the note.
    vyrn_frontend::load(&src, &slash(path), &opts, &Fs).map_err(|d| {
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

/// A project's manifest, read the way the CLI reads it. `None` for a
/// directory with no `vyrn.json`.
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
    /// The judgment has more, and a callee's body carries it: the floor's
    /// union over the import closure agrees. Per-function grain only.
    CalleeCarried,
    /// A `gen fn` body: the floor skips it by design (it runs against the
    /// compiler's filesystem); the judgment sees its reads. The context
    /// differs, the verdict agrees.
    GenBody,
    /// The floor sees a call in the body the judgment does not: the core
    /// does not lower it (a lambda body). A disagreement.
    CoreBlind,
    /// The judgment has more and no body in the program carries it: the
    /// floor would not refuse a program this function makes unbuildable. A
    /// disagreement.
    FloorBlind,
}

/// How a function lands against the audience fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AudienceKind {
    /// The project declares no audience, or the module is outside it (std,
    /// a remote): the fence says nothing, and nothing is compared.
    NoFence,
    /// Server-only with an effect a browser lacks, client-only with an
    /// extern, or universal with neither.
    Agree,
    /// Server-only or client-only with no target-restricted effect at all:
    /// the fence protects a declaration, not an effect (RFC-0103 §4). Not an
    /// effect; stays.
    DeclaredOnly,
    /// Universal or client-only, with an effect a browser lacks: the fence
    /// lets a client import it, and only a declared artifact's floor refuses.
    /// A disagreement.
    Unfenced,
    /// Server-only, with an extern a native target lacks. The fence lets the
    /// server import it. A disagreement.
    ServerExtern,
}

/// One judged function.
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

/// The floor's capability a set of effects needs.
fn caps_of(e: Effects) -> BTreeSet<Capability> {
    let mut out = BTreeSet::new();
    if e.has(Effect::FsRead) || e.has(Effect::FsWrite) || e.has(Effect::FsList) {
        out.insert(Capability::Fs);
    }
    if e.has(Effect::ReadInput) {
        out.insert(Capability::Stdin);
    }
    if e.has(Effect::Args) {
        out.insert(Capability::Args);
    }
    out
}

/// The floor's own rule at function grain: which `CALLS` names the body
/// spells, whatever branch they are on. A `gen fn` carries nothing, as in
/// `floor::carried`.
fn floor_carries(f: &vyrn_frontend::ast::Function) -> BTreeSet<Capability> {
    let mut out = BTreeSet::new();
    if f.is_gen {
        return out;
    }
    let mut body = f.body.clone();
    vyrn_frontend::project::walk_block(&mut body, &mut |e| {
        if let vyrn_frontend::ast::Expr::Call { name, .. } = e {
            if let Some((_, cap)) = floor::CALLS.iter().find(|(n, _)| n == name) {
                out.insert(*cap);
            }
        }
    });
    out
}

/// Whether the effect set needs something a browser page lacks.
fn browser_lacks(e: Effects) -> bool {
    !caps_of(e).is_empty()
}

#[test]
fn the_lattice_is_the_rfc_table() {
    // The table in RFC-0125 §3 M6: a row per effect, whose second column
    // lists the atoms in backticks. ATOMS is derived from it, so the two
    // must agree exactly — the RFC is the statement, the code is the copy.
    let rfc = std::fs::read_to_string(repo_root().join("rfcs/RFC-0125-a-rule-is-stated-once.md"))
        .unwrap();
    let start = rfc
        .find("| effect | atoms")
        .expect("the lattice table is in RFC-0125 §3 M6");
    let mut from_rfc: BTreeSet<(String, Effect)> = BTreeSet::new();
    let mut rows = 0;
    for line in rfc[start..].lines().skip(2) {
        if !line.starts_with('|') {
            break;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        let effect = Effect::parse(cells[0].trim_matches('`'))
            .unwrap_or_else(|| panic!("`{}` is not an effect", cells[0]));
        rows += 1;
        let mut rest = cells[1];
        while let Some(open) = rest.find('`') {
            let after = &rest[open + 1..];
            let close = after.find('`').expect("closed backtick");
            let name = &after[..close];
            if name != "—" && !name.is_empty() {
                from_rfc.insert((name.to_string(), effect));
            }
            rest = &after[close + 1..];
        }
    }
    assert_eq!(rows, Effect::ALL.len(), "one row per effect");
    let from_code: BTreeSet<(String, Effect)> = effects::ATOMS
        .iter()
        .map(|(n, e)| (n.to_string(), *e))
        .collect();
    let only_rfc: Vec<_> = from_rfc.difference(&from_code).collect();
    let only_code: Vec<_> = from_code.difference(&from_rfc).collect();
    assert!(
        only_rfc.is_empty() && only_code.is_empty(),
        "the RFC table and effects::ATOMS differ; in the RFC only: {only_rfc:?}; in the code only: {only_code:?}"
    );
}

#[test]
fn the_effect_judgment_over_the_corpus() {
    std::thread::Builder::new()
        .stack_size(vyrn_frontend::interp::INTERP_STACK_BYTES)
        .spawn(run_corpus)
        .unwrap()
        .join()
        .unwrap();
}

fn run_corpus() {
    vyrn_lower::install();
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
    // Calls through a function value the sources answered for, and the
    // function types with a source the corpus has no body for (open sets).
    let mut through_calls = 0usize;
    let mut open: Vec<String> = Vec::new();
    // Every spawn site, and the ones whose callee's set is outside the rule.
    let mut spawn_sites = 0usize;
    let mut spawn_outside: Vec<String> = Vec::new();
    let mut gaps: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut unloadable = 0usize;
    let mut refused = 0usize;
    let mut programs = 0usize;
    for (path, project) in &roots {
        let program = match load(path, project.as_deref()) {
            Ok(p) => p,
            Err(e) => {
                // A project entry must load here: the audience comparison
                // has nothing else to stand on — unless the registry says
                // its artifact's floor refuses it, and the refusal is the
                // one recorded. An example that needs the root manifest's
                // remote dependencies is counted, as in `kernel.rs`.
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
        let _memo = vyrn_frontend::project::Memo::open();
        let lowered = vyrn_lower::lower(&program);
        let own = vyrn_frontend::own::analyze(&program);
        let mut bodies = Vec::new();
        let mut insts = Vec::new();
        for inst in &lowered.instances {
            match vyrn_lower::core::build(&program, inst, &own) {
                Ok(b) => {
                    bodies.push(b);
                    insts.push(inst);
                }
                Err(g) => *gaps.entry(g.what).or_default() += 1,
            }
        }
        // Every frame, outermost first: the judgment's slice. `top[i]` is
        // the slot of instance `i`'s own body; a lambda frame is keyed by
        // the function it was written in and its line, which is how the
        // checker names a lambda source (RFC-0037).
        let mut refs: Vec<&vyrn_lower::core::Body> = Vec::new();
        let mut top: Vec<usize> = Vec::new();
        let mut lambda_frames: BTreeMap<(&str, usize), Vec<usize>> = BTreeMap::new();
        for (i, b) in bodies.iter().enumerate() {
            for f in b.frames() {
                if std::ptr::eq(f, b) {
                    top.push(refs.len());
                } else if let Some(line) = f
                    .name
                    .rsplit("@lambda:")
                    .next()
                    .and_then(|l| l.parse::<usize>().ok())
                {
                    lambda_frames
                        .entry((insts[i].func.name.as_str(), line))
                        .or_default()
                        .push(refs.len());
                }
                refs.push(f);
            }
        }
        // A callee's name: every instance of the function by that name, and
        // every impl method with that surface name.
        let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (i, inst) in insts.iter().enumerate() {
            by_name
                .entry(inst.func.name.as_str())
                .or_default()
                .push(top[i]);
        }
        let mut impl_methods: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for im in &program.impls {
            for m in im.methods.iter().chain(im.places.iter()) {
                if let Some(key) = vyrn_frontend::types::type_key(&im.ty) {
                    let mangled =
                        vyrn_frontend::types::impl_method_name(&im.protocol, &key, &m.name);
                    if let Some(idx) = by_name.get(mangled.as_str()) {
                        impl_methods
                            .entry(m.name.as_str())
                            .or_default()
                            .extend(idx.iter().copied());
                    }
                }
            }
        }
        // A variant constructor takes its payload and does nothing else.
        let decls = vyrn_frontend::types::decl_map(&program);
        let variants: BTreeSet<&str> = decls
            .values()
            .filter_map(|d| match &d.base {
                vyrn_frontend::ast::Type::Enum(vs) => Some(vs),
                _ => None,
            })
            .flat_map(|vs| vs.iter().map(|v| v.name.as_str()))
            .collect();
        let externs: BTreeSet<&str> = program
            .functions
            .iter()
            .filter(|f| f.is_extern)
            .map(|f| f.name.as_str())
            .collect();
        let mut resolve = |name: &str| -> Callee {
            if let Some(e) = effects::atom(name) {
                return Callee::Atom(Effects::of(e));
            }
            if externs.contains(name) {
                return Callee::Atom(Effects::of(Effect::Extern));
            }
            if let Some(idx) = by_name.get(name) {
                return Callee::Bodies(idx.clone());
            }
            if let Some(idx) = impl_methods.get(name) {
                return Callee::Bodies(idx.clone());
            }
            if vyrn_frontend::prelude::signature(name).is_some()
                || vyrn_frontend::checker::RESERVED.contains(&name)
                || name.starts_with('@')
                || name.starts_with(vyrn_frontend::loader::MEM_PREFIX)
                || name.starts_with(vyrn_frontend::loader::RUNTIME_PREFIX)
                || program.functions.iter().any(|f| f.name == name)
                || variants.contains(name)
                || decls.contains_key(name)
                || matches!(name, "Some" | "Ok" | "Err" | "logger" | "print")
            {
                return Callee::Pure;
            }
            Callee::Unknown
        };
        // RFC-0037: the closed set of functions a value of a function type
        // may hold, as the checker's defunctionalization collected it. A
        // named source is its instances; a lambda source is its frame.
        let stored = vyrn_frontend::checker::stored_fn_effects(&program);
        let mut through = |ty: &Type| -> Callee {
            // The sources are collected with aliases resolved; a local is
            // typed as the program spelled it.
            let ty = &vyrn_frontend::types::resolve(ty, &decls);
            if !matches!(ty, Type::Fn(..)) {
                return Callee::Unknown;
            }
            let mut idx: Vec<usize> = Vec::new();
            let mut missing: Vec<String> = Vec::new();
            for src in &stored.sources {
                if !vyrn_frontend::checker::fn_sigs_match(&src.sig, ty) {
                    continue;
                }
                if let Some(n) = &src.named {
                    match by_name.get(n.as_str()) {
                        Some(i) => idx.extend(i.iter().copied()),
                        None => missing.push(n.clone()),
                    }
                }
                if let Some(l) = &src.lambda {
                    match lambda_frames.get(&(l.defined_in.as_str(), l.line)) {
                        Some(i) => idx.extend(i.iter().copied()),
                        None => {
                            missing.push(format!("a lambda in {} at line {}", l.defined_in, l.line))
                        }
                    }
                }
            }
            if !missing.is_empty() {
                open.push(format!(
                    "{file}: a `{ty}` value may hold {}",
                    missing.join(", ")
                ));
            } else if idx.is_empty() {
                open.push(format!("{file}: a `{ty}` value has no source"));
            }
            idx.sort_unstable();
            idx.dedup();
            if idx.is_empty() {
                Callee::Unknown
            } else {
                Callee::Bodies(idx)
            }
        };
        let judged = effects::judge(&refs, &mut resolve, &mut through);
        through_calls += judged.through.len();
        spawn_sites += judged.spawns.len();
        for sp in &judged.spawns {
            if !sp.outside().is_pure() {
                spawn_outside.push(format!(
                    "{file}:{} spawn {}(..) in {} — {}",
                    sp.line,
                    sp.callee,
                    refs[sp.body].name,
                    sp.outside()
                ));
            }
        }
        for (i, name) in &judged.unknown {
            *unknown
                .entry(format!("{name} (in {})", refs[*i].name))
                .or_default() += 1;
        }

        // The floor's union over the whole program: what its closure check
        // would see for any artifact rooted here.
        let mut program_carries: BTreeSet<Capability> = BTreeSet::new();
        for f in &program.functions {
            program_carries.extend(floor_carries(f));
        }
        for im in &program.impls {
            for m in im.methods.iter().chain(im.places.iter()) {
                program_carries.extend(floor_carries(m));
            }
        }

        for (i, inst) in insts.iter().enumerate() {
            let e = judged.effects[top[i]];
            let want = caps_of(e);
            let have = floor_carries(inst.func);
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

            let module_key = if inst.module().is_empty() {
                root_key.clone()
            } else {
                inst.module().to_string()
            };
            let (audience_kind, who) = match man.as_ref().and_then(|m| m.audience.as_ref()) {
                None => (AudienceKind::NoFence, String::new()),
                Some(map) => {
                    let v = audience::audience_of(&module_key, map);
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
            if let Some((_, want_fn)) = dump_target {
                if inst.func.name == want_fn {
                    eprintln!(
                        "{file}: {} in {} — {e}",
                        inst.spelling(),
                        if inst.module().is_empty() {
                            "<root>"
                        } else {
                            inst.module()
                        }
                    );
                    eprintln!(
                        "  floor: {floor:?} (body carries {:?}); audience: {audience_kind:?} {who}",
                        have
                    );
                    let mut callees: BTreeSet<String> = BTreeSet::new();
                    for f in bodies[i].frames() {
                        collect_callees(&f.stmts, &mut callees);
                    }
                    for c in callees {
                        let mut r = resolve(&c);
                        if matches!(r, Callee::Unknown) {
                            if let Some(n) = bodies[i]
                                .frames()
                                .iter()
                                .flat_map(|f| f.names.iter())
                                .find(|n| n.source == c)
                            {
                                r = through(&n.ty);
                            }
                        }
                        let ce = match r {
                            Callee::Atom(a) => a.to_string(),
                            Callee::Bodies(idx) => idx
                                .iter()
                                .map(|j| judged.effects[*j])
                                .fold(Effects::PURE, Effects::join)
                                .to_string(),
                            Callee::Pure => "pure".into(),
                            Callee::Unknown => "unknown".into(),
                        };
                        eprintln!("  calls {c}: {ce}");
                    }
                }
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

    // The tally.
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
         {} unattributed",
        rows.len(),
        unknown.values().sum::<usize>()
    );
    open.sort();
    open.dedup();
    eprintln!("  open sets: {}", open.len());
    for o in &open {
        eprintln!("    {o}");
    }
    for (what, n) in &gaps {
        eprintln!("  unlowered: {n:5}  {what}");
    }
    for (e, n) in &per_effect {
        eprintln!("  effect {n:5}  {}", e.name());
    }
    eprintln!(
        "  spawn:      {spawn_sites} sites judged, {} outside `alloc, trap`",
        spawn_outside.len()
    );
    for s in &spawn_outside {
        eprintln!("  spawn outside the rule: {s}");
    }
    eprintln!("  floor:");
    for (k, n) in &floor_kinds {
        eprintln!("    {n:5}  {k:?}");
    }
    eprintln!("  audience:");
    for (k, n) in &audience_kinds {
        eprintln!("    {n:5}  {k:?}");
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
    if std::env::var("VYRN_EFFECTS_UNKNOWN").is_ok() {
        for (name, n) in &unknown {
            eprintln!("  call through a function value {n:5}  {name}");
        }
    }
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
    // The ratchet: the disagreements, by function, and the spawn sites
    // outside the rule. It may fall, never rise. 1 when the first slice
    // landed (`listdir.vyrn`'s `main`, whose `listDir` the floor had no row
    // for — RFC-0125 §3 M6 finding 6); 0 since the second slice.
    const RATCHET: usize = 0;
    assert!(
        spawn_outside.is_empty(),
        "{} spawn sites whose callee's effects are outside the rule the checker accepted; the first: {}",
        spawn_outside.len(),
        spawn_outside[0]
    );
    assert!(
        disagreements.len() <= RATCHET,
        "{} functions where a pass and the effect judgment disagree, more than the {RATCHET} recorded; \
         the first new one is worth reading before the number is raised: {}:{} {}",
        disagreements.len(),
        disagreements[0].file,
        disagreements[0].line,
        disagreements[0].name
    );
    assert_eq!(
        refused,
        common::EXPECTED_PROJECT_CHECK_FAILURE.len(),
        "every registered project refusal is in the corpus"
    );
    assert!(!rows.is_empty(), "the judgment judged nothing");
    let _ = &rows[0].module;
}

fn collect_callees(stmts: &[vyrn_lower::core::St], out: &mut BTreeSet<String>) {
    use vyrn_lower::core::{Rhs, St};
    for s in stmts {
        match s {
            St::Let(_, Rhs::Call { callee, .. }) | St::Do(Rhs::Call { callee, .. }, _) => {
                out.insert(callee.clone());
            }
            St::If { then, els, .. } => {
                collect_callees(then, out);
                collect_callees(els, out);
            }
            St::Loop(b) | St::Block { body: b, .. } => collect_callees(b, out),
            St::Switch { arms, .. } => {
                for a in arms {
                    collect_callees(&a.body, out);
                }
            }
            _ => {}
        }
    }
}
