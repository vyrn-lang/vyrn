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
use vyrn_frontend::project::Memo;

use vyrn_frontend::ast::{Program, Type};
use vyrn_frontend::audience::{self, Audience};
use vyrn_frontend::floor::{self, Capability};
use vyrn_frontend::loader::DiskResolver;
use vyrn_lower::effects::{self, Callee, Effect, Effects};

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
fn load(path: &Path, project: Option<&Path>) -> Result<(Program, Memo), String> {
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(slash(&repo_root().join("std"))),
        artifacts: project.and_then(manifest).and_then(|m| m.artifacts),
        ..Default::default()
    };
    // A floor refusal names the carrier in the note.
    Memo::load(|| {
        vyrn_lower::load(
            &src,
            &slash(path),
            &opts,
            &DiskResolver,
            Some(&*vyrn_genwasm::engine()),
        )
    })
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
    let mut body = f.body.clone();
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
    // Calls through a function value the sources answered for, and the
    // function types with a source the corpus has no body for (open sets).
    let mut through_calls = 0usize;
    let mut open: Vec<String> = Vec::new();
    // Function types the program declares and holds no value of: the call through
    // such a name cannot run.
    let mut empty_sets: Vec<String> = Vec::new();
    let mut empty_calls = 0usize;
    // The floor rows the judgment states. Each must answer as the pass does,
    // function by function, or a refusal changed with the derivation.
    let mut judged_agree = 0usize;
    let mut judged_gen = 0usize;
    let mut judged_carried = 0usize;
    let mut judged_differ: Vec<String> = Vec::new();
    // The `module-state` row against the checker's `module_state_use`, which answer
    // one question. A miss is a hole in the judgment: deleting the checker's copy
    // would accept a program the checker refuses, so it stays zero. An extra is the
    // asymmetry of `StoredFnEffects::arg_sources`: the judgment follows a function
    // handed to a `fn`-typed parameter and the checker does not. Ratcheted.
    let mut module_state_missed: Vec<String> = Vec::new();
    let mut module_state_extra: Vec<String> = Vec::new();
    let mut gaps: BTreeMap<&'static str, usize> = BTreeMap::new();
    let show_gaps = std::env::var("VYRN_EFFECTS_GAPS").ok();
    let mut unloadable = 0usize;
    let mut refused = 0usize;
    let mut programs = 0usize;
    for (path, project) in &roots {
        let (program, _memo) = match load(path, project.as_deref()) {
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
        let lowered = vyrn_lower::lower(&program);
        let world = vyrn_lower::analyze(&program);
        let own = &world.ownership;
        let mut bodies = Vec::new();
        let mut insts = Vec::new();
        for inst in &lowered.instances {
            match vyrn_lower::core::build(&program, inst, &own) {
                Ok(b) => {
                    bodies.push(b);
                    insts.push(inst);
                }
                Err(g) => {
                    if show_gaps.as_deref().is_some_and(|w| g.what.contains(w)) {
                        eprintln!(
                            "  gap: {} {}:{}:{} {} {}",
                            slash(path),
                            inst.module(),
                            inst.spelling(),
                            g.line,
                            g.what,
                            g.detail
                        );
                    }
                    *gaps.entry(g.what).or_default() += 1;
                }
            }
        }
        // The module-state initializer and every `test` and `bench` body: no
        // instance and no pass verdict, but the lambdas they hold need a frame,
        // which is what a stored source names.
        let mut outside: Vec<vyrn_frontend::core::Body> = Vec::new();
        if !program.globals.is_empty() {
            match vyrn_lower::core::build_module_state(&program, &own, &lowered.globals) {
                Ok(b) => outside.push(b),
                Err(g) => *gaps.entry(g.what).or_default() += 1,
            }
        }
        for ob in &lowered.bodies {
            match vyrn_lower::core::build_outside(&program, &own, &mut Default::default(), ob) {
                Ok(b) => outside.push(b),
                Err(g) => {
                    if show_gaps.as_deref().is_some_and(|w| g.what.contains(w)) {
                        eprintln!(
                            "  gap: {} {}:{} {} {}",
                            slash(path),
                            ob.name,
                            g.line,
                            g.what,
                            g.detail
                        );
                    }
                    *gaps.entry(g.what).or_default() += 1;
                }
            }
        }
        // An `impl` projection's body. No instance covers it, yet the
        // core lowers an access site as a call by the projection's name, so the
        // judgment needs the body to bound what `x.field(k)` runs. Built under the
        // empty substitution a declaration has.
        let mut place_bodies: Vec<(&str, vyrn_frontend::core::Body)> = Vec::new();
        for pr in &lowered.places {
            let inst = vyrn_lower::Instance {
                func: pr.func,
                func_id: pr.id,
                type_args: Vec::new(),
                subst: Default::default(),
                facts: pr.facts.clone(),
                releases: Vec::new(),
            };
            match vyrn_lower::core::build(&program, &inst, &own) {
                Ok(b) => place_bodies.push((pr.func.name.as_str(), b)),
                Err(g) => {
                    if show_gaps.as_deref().is_some_and(|w| g.what.contains(w)) {
                        eprintln!(
                            "  gap: {} place {}:{} {} {}",
                            slash(path),
                            pr.func.name,
                            g.line,
                            g.what,
                            g.detail
                        );
                    }
                    *gaps.entry(g.what).or_default() += 1;
                }
            }
        }
        // Every frame, outermost first. `top[i]` is the slot of instance `i`'s own
        // body; a lambda frame is keyed by its enclosing function and line, as the
        // checker names a lambda source.
        let mut refs: Vec<&vyrn_frontend::core::Body> = Vec::new();
        let mut top: Vec<usize> = Vec::new();
        let mut lambda_frames: BTreeMap<(&str, usize), Vec<usize>> = BTreeMap::new();
        for (i, b) in bodies.iter().enumerate() {
            for f in b.frames() {
                if std::ptr::eq(f, b) {
                    top.push(refs.len());
                } else if let Some(line) = vyrn_lower::core::lambda_line(&f.name) {
                    lambda_frames
                        .entry((insts[i].func.name.as_str(), line))
                        .or_default()
                        .push(refs.len());
                }
                refs.push(f);
            }
        }
        // Bodies that are no instance get no `top` slot. Their lambdas are keyed by
        // the checker's name: empty for the module-state initializer, `test@<i>` or
        // `bench@<i>` otherwise.
        for b in &outside {
            for f in b.frames() {
                if let Some(line) = vyrn_lower::core::lambda_line(&f.name) {
                    lambda_frames
                        .entry((b.name.as_str(), line))
                        .or_default()
                        .push(refs.len());
                }
                refs.push(f);
            }
        }
        // The resolver reaches a projection's body by its surface name, the name
        // the core calls it by.
        let mut place_tops: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (name, b) in &place_bodies {
            place_tops.entry(name).or_default().push(refs.len());
            for f in b.frames() {
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
        let decls = vyrn_frontend::types::decl_map(&program);
        let pure = effects::PureNames::new(&decls);
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
            // A projection dispatched by name is in neither table above.
            if let Some(idx) = place_tops.get(name) {
                return Callee::Bodies(idx.clone());
            }
            // A program function without a judged body (a generic reached only
            // from a `test` body, a generic declared release) is judged pure
            // here: a hole the survey does not count.
            if pure.contains(name) || program.functions.iter().any(|f| f.name == name) {
                return Callee::Pure;
            }
            Callee::Unknown
        };
        // The closed set of functions a value of a function type may hold: the
        // defunctionalization sources and the functions handed to a
        // `fn`-typed parameter. A named source is its instances; a
        // lambda source is its frame.
        let stored = vyrn_frontend::checker::stored_fn_effects(&program);
        let mut module_state_of: std::collections::HashMap<String, bool> =
            std::collections::HashMap::new();
        let mut through = |ty: &Type| -> Callee {
            // The sources are collected with aliases resolved; a local is
            // typed as the program spelled it.
            let ty = &vyrn_frontend::types::resolve(ty, &decls);
            if !matches!(ty, Type::Fn(..)) {
                return Callee::Unknown;
            }
            let mut idx: Vec<usize> = Vec::new();
            let mut missing: Vec<String> = Vec::new();
            for src in stored.every_source() {
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
            // An open set names a source the corpus has no body for, so the join is
            // short. An empty set is a different answer: `Callee::Empty`.
            if !missing.is_empty() {
                open.push(format!(
                    "{file}: a `{ty}` value may hold {}",
                    missing.join(", ")
                ));
            } else if idx.is_empty() {
                empty_sets.push(format!("{file}: no `{ty}` value exists in this program"));
            }
            idx.sort_unstable();
            idx.dedup();
            if !idx.is_empty() {
                Callee::Bodies(idx)
            } else if missing.is_empty() {
                Callee::Empty
            } else {
                Callee::Unknown
            }
        };
        let judged = effects::judge(&refs, &mut resolve, &mut through);
        through_calls += judged.through.len();
        empty_calls += judged.empty.len();
        // Two reasons only: no collected source matches the callee's function type,
        // or the callee is no name of the body (a projection dispatched by name).
        for (i, name, line) in &judged.unknown {
            let ty = refs[*i]
                .names
                .iter()
                .find(|n| &n.source == name)
                .map(|n| n.ty.to_string());
            let why = match ty {
                Some(t) => format!("no collected source of `{t}`"),
                None => "not a name of the body: a projection dispatched by name".to_string(),
            };
            *unknown
                .entry(format!(
                    "{file}:{line} {name} (in {}) — {why}",
                    refs[*i].name
                ))
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

        for (i, inst) in insts.iter().enumerate() {
            let e = judged.effects[top[i]];
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

            // Memoized by name: the checker asks about a function, and an instance
            // repeats one.
            let ms_judged = e.has(Effect::ModuleState);
            let ms_checker = *module_state_of
                .entry(inst.func.name.clone())
                .or_insert_with(|| {
                    !program.globals.is_empty()
                        && vyrn_frontend::checker::module_state_use(
                            &program,
                            &inst.func.name,
                            &stored,
                        )
                        .is_some()
                });
            if ms_checker && !ms_judged {
                module_state_missed.push(format!("{file}:{} {}", inst.func.line, inst.spelling()));
            } else if ms_judged && !ms_checker {
                module_state_extra.push(format!("{file}:{} {}", inst.func.line, inst.spelling()));
            }

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
                            Callee::Empty => "an empty set".into(),
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
    open.sort();
    open.dedup();
    eprintln!("  open sets: {}", open.len());
    for o in &open {
        eprintln!("    {o}");
    }
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
    eprintln!(
        "  module state: {} the judgment misses, {} it reaches through an argument the checker does not follow",
        module_state_missed.len(),
        module_state_extra.len()
    );
    for d in module_state_missed
        .iter()
        .chain(module_state_extra.iter())
        .take(30)
    {
        eprintln!("    {d}");
    }
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
    const OPEN_SETS: usize = 0;
    assert_eq!(
        open.len(),
        OPEN_SETS,
        "function types whose closed set names a body the corpus does not have; the first: {}",
        open.first().map(String::as_str).unwrap_or("none")
    );
    // An empty set is an answer, not a hole.
    const EMPTY_SETS: usize = 6;
    assert_eq!(
        empty_sets.len(),
        EMPTY_SETS,
        "function types no value of which exists in their program:\n{}",
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
        module_state_missed.is_empty(),
        "{} functions the checker's walk says reach module state and the `module-state` row does not;          the first: {}",
        module_state_missed.len(),
        module_state_missed.first().map(String::as_str).unwrap_or("none")
    );
    // It may fall, never rise.
    const MODULE_STATE_EXTRA: usize = 23;
    assert!(
        module_state_extra.len() <= MODULE_STATE_EXTRA,
        "{} functions the `module-state` row reaches and the checker's walk does not, more than          the {MODULE_STATE_EXTRA} recorded; the first new one is worth reading: {}",
        module_state_extra.len(),
        module_state_extra.first().map(String::as_str).unwrap_or("none")
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

fn collect_callees(stmts: &[vyrn_frontend::core::St], out: &mut BTreeSet<String>) {
    use vyrn_frontend::core::{Rhs, St};
    for (s, _) in vyrn_frontend::core::rows(stmts) {
        if let St::Let(_, Rhs::Call { callee, .. })
        | St::Do {
            rhs: Rhs::Call { callee, .. },
            ..
        } = s
        {
            out.insert(callee.clone());
        }
    }
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
    ("hostNowMillis", "clock", false),
    ("hostMonotonicNanos", "clock", false),
    ("hostRandomSeed", "random", false),
    ("serveStream", "serve", false),
    ("panic", "trap", true),
    ("@panicAt", "trap", true),
    ("assert", "trap", true),
    ("assertEq", "trap", true),
    ("runtime$trap", "trap", true),
    ("mem$trap", "trap", true),
    ("moduleInterface", "gen-only", true),
    ("contractOf", "gen-only", true),
    ("lex", "gen-only", true),
    ("render", "gen-only", true),
    ("raw", "gen-only", true),
    ("rawAt", "gen-only", true),
    ("@codeText", "gen-only", true),
    ("@codeSplice", "gen-only", true),
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
