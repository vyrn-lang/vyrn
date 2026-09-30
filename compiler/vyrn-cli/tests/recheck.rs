//! The editor's per-body recheck (`vyrn_frontend::checker::recheck`): after
//! every edit of a sequence, the analysis of a thread that has analysed every
//! earlier text gives what a fresh thread's analysis gives, byte for byte.
//!
//! Both threads run the editor's pipeline (`analyze_judged` with
//! `vyrn_lower::JUDGE` and the generation engine) and arm the judgment memo.
//! Only the editing thread arms `record_reads`, so the fresh analysis checks
//! every body and walks every body the lowering reaches. It also witnesses
//! that a replayed body writes what a checked one writes, and that a reused
//! walk adds to the worklist what a walk adds.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;

use vyrn_frontend::loader::{DiskResolver, LoadOptions, ModuleResolver};
use vyrn_frontend::origin::OriginMaps;
use vyrn_frontend::Analysis;

mod common;

/// The disk, with some files' text replaced, as the editor's open buffers
/// replace them, keyed by [`OriginMaps::norm_path_key`]; and a remote import
/// the project's lock pins, from the cache, as the editor reads it.
struct Overlaid {
    texts: HashMap<String, String>,
    /// The directory of the project's `vyrn.json`.
    project: Option<String>,
}

impl ModuleResolver for Overlaid {
    fn read(&self, resolved: &str) -> Result<String, String> {
        if vyrn_frontend::loader::is_remote(resolved) {
            let dir = self.project.as_deref().ok_or("no project")?;
            let lock = vyrn_frontend::manifest::Lock::in_project(dir)?;
            let (_, sha) = lock.entries.get(resolved).ok_or("not pinned")?;
            return vyrn_frontend::manifest::pinned_blob(Some(dir), sha)
                .unwrap_or(Err("not cached".to_string()));
        }
        match self.texts.get(&OriginMaps::norm_path_key(resolved)) {
            Some(text) => Ok(text.clone()),
            None => DiskResolver.read(resolved),
        }
    }
    fn list_kinds(&self, resolved: &str) -> Result<Vec<String>, String> {
        DiskResolver.list_kinds(resolved)
    }
    fn gen_cache_get(&self, key: &str) -> Option<String> {
        DiskResolver.gen_cache_get(key)
    }
    fn gen_cache_put(&self, key: &str, value: &str) {
        DiskResolver.gen_cache_put(key, value)
    }
}

/// One edit: the root's text, the other files it replaces, and whether a
/// check of it refuses something.
struct Edit {
    what: &'static str,
    root: String,
    overlays: HashMap<String, String>,
    refused: bool,
}

/// What the editor shows of an analysis: diagnostics, relocated diagnostics,
/// typed bindings and memory notes; and the lowering's generic `instances`.
fn shown(a: &Analysis, instances: &[String]) -> String {
    format!(
        "{:#?}\n{:#?}\n{:#?}\n{:#?}\n{instances:#?}",
        a.diagnostics, a.remapped, a.locals, a.memory
    )
}

/// The analysis of `e` in the project `path` lies in, loaded as the editor
/// loads it.
fn analyze(path: &str, e: &Edit) -> Analysis {
    let (opts, resolver) = linker(path, e);
    let engine = vyrn_genwasm::engine();
    vyrn_frontend::analyze_judged(
        &e.root,
        Some((path, &opts, &resolver)),
        Some(&*engine),
        &vyrn_lower::JUDGE,
    )
}

/// The generic instances the editor's lowering of `e` makes, in the World's
/// order; none for a program that does not load.
fn instances(path: &str, e: &Edit) -> Vec<String> {
    let (opts, resolver) = linker(path, e);
    let engine = vyrn_genwasm::engine();
    let Ok(p) = vyrn_lower::load(&e.root, path, &opts, &resolver, Some(&*engine)) else {
        return Vec::new();
    };
    let world = vyrn_lower::refusals(&p).1;
    (world.fn_rows().iter())
        .filter(|r| r.generic.is_some())
        .map(|r| r.name.clone())
        .collect()
}

/// The load options and the resolver of `e` in the project `path` lies in,
/// as the editor loads it.
fn linker(path: &str, e: &Edit) -> (LoadOptions, Overlaid) {
    let mut opts = LoadOptions {
        std_root: vyrn_frontend::manifest::std_root(),
        ..Default::default()
    };
    let mut resolver = Overlaid {
        texts: e.overlays.clone(),
        project: None,
    };
    let dir = Path::new(path).parent().expect("a file has a directory");
    if let Some(m) = vyrn_frontend::manifest::find(dir).expect("the manifest parses") {
        opts.aliases = m.dependencies.into_iter().collect();
        opts.alias_base = m.dir.clone();
        opts.audience = m.audience;
        opts.artifacts = m.artifacts;
        resolver.project = Some(m.dir);
    }
    (opts, resolver)
}

/// Runs `f` on a fresh thread with the editor's stack and judgment memo.
fn on_fresh_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            vyrn_frontend::movecheck::reuse_judgments();
            f()
        })
        .expect("spawn an analysis thread")
        .join()
        .expect("the analysis thread panicked")
}

/// Replays `edits` over the program at `path` on one editing thread, and
/// compares each analysis with a fresh thread's. Returns how many bodies the
/// editing thread checked and replayed for each edit.
fn replay(path: &Path, edits: Vec<Edit>) -> Vec<(u64, u64)> {
    let path = path.to_string_lossy().replace('\\', "/");
    let edits = std::sync::Arc::new(edits);
    let (ask, asked) = mpsc::channel::<usize>();
    let (tell, told) = mpsc::channel::<(String, (u64, u64))>();
    let (p, es) = (path.clone(), edits.clone());
    let editor = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            vyrn_frontend::movecheck::reuse_judgments();
            vyrn_frontend::checker::record_reads();
            for i in asked {
                let _ = vyrn_frontend::checker::recheck::tally();
                let a = analyze(&p, &es[i]);
                let tally = vyrn_frontend::checker::recheck::tally();
                let shown = shown(&a, &instances(&p, &es[i]));
                tell.send((shown, tally)).expect("the test listens");
            }
        })
        .expect("spawn the editing thread");
    let mut rechecked = Vec::new();
    for (i, e) in edits.iter().enumerate() {
        ask.send(i).expect("the editing thread listens");
        let (incremental, (checked, replayed)) = told.recv().expect("the editing thread answers");
        let (p, es) = (path.clone(), edits.clone());
        let fresh = on_fresh_thread(move || {
            let a = analyze(&p, &es[i]);
            (shown(&a, &instances(&p, &es[i])), a.diagnostics)
        });
        let parse = fresh.1.iter().find(|d| d.stage == "parse");
        assert!(parse.is_none(), "{path}, {}: {parse:?}", e.what);
        let errors = fresh
            .1
            .iter()
            .any(|d| d.severity == vyrn_frontend::diagnostics::Severity::Error);
        assert_eq!(errors, e.refused, "{path}, {}: {:#?}", e.what, fresh.1);
        if let Some(d) =
            common::first_diff("analysis", "fresh", &fresh.0, "incremental", &incremental)
        {
            panic!(
                "{path}, {}: {checked} checked, {replayed} replayed\n{d}",
                e.what
            );
        }
        eprintln!("{path}: {}: {checked} checked, {replayed} replayed", e.what);
        rechecked.push((checked, replayed));
    }
    drop(ask);
    editor.join().expect("the editing thread panicked");
    rechecked
}

const ADDED: &str = "\nfn recheckAdded(x: Int64) -> Int64 {\n    return x\n}\n";
const ADDED_STR: &str = "\nfn recheckAdded(x: String) -> Int64 {\n    return 1\n}\n";
const RENAMED: &str = "\nfn recheckRenamed(x: Int64) -> Int64 {\n    return x\n}\n";
const CALLER: &str = "\nfn recheckCaller() -> Int64 {\n    return recheckAdded(1)\n}\n";
const POINT_A: &str = "\ntype RecheckPoint = { a: Int64 }\n";
const POINT_B: &str = "\ntype RecheckPoint = { b: Int64 }\n";
const FIELD: &str = "\nfn recheckField(p: RecheckPoint) -> Int64 {\n    return p.a\n}\n";
const STATE: &str = "\nlet recheckState: Int64 = 1\n";
const STATE_STR: &str = "\nlet recheckState: String = \"a\"\n";
const READER: &str = "\nfn recheckRead() -> Int64 {\n    return recheckState + 1\n}\n";
const TEST_OK: &str = "\ntest \"recheck\" {\n    assertEq(recheckAdded(1), 1)\n}\n";
const TEST_BAD: &str = "\ntest \"recheck\" {\n    assertEq(recheckAdded(1), \"a\")\n}\n";
/// One generic body instantiated at two types, each calling another generic.
const GENERIC: &str = "\nfn recheckOne<T>(x: T) -> Int64 {\n    return 1\n}\n\nfn recheckPair<T>(x: T) -> Int64 {\n    return recheckOne(x)\n}\n\nfn recheckBoth() -> Int64 {\n    return recheckPair(1) + recheckPair(\"a\")\n}\n";
/// A private function of `std/strings`: a root declaration of the same name
/// renames both apart, which rewrites the module's bodies and keeps its text.
const COLLIDE: &str = "\nfn isAsciiSpace(b: Int64) -> String {\n    return \"\"\n}\n";

/// The edits every program takes, from its text `base`: each kind of
/// dependency changes once and changes back.
fn edits(base: &str) -> Vec<Edit> {
    let at = base.rfind("\nfn ").expect("the root declares a function") + 1;
    let brace = at + base[at..].find("{\n").expect("the body opens on its line");
    let body = format!(
        "{} let recheckLocal = 1{}",
        &base[..=brace],
        &base[brace + 1..]
    );
    let all = |parts: &[&str]| format!("{base}{}", parts.concat());
    let typed = [ADDED, CALLER, POINT_A, FIELD, STATE, READER];
    let mut tested = typed.to_vec();
    tested.push(TEST_OK);
    let mut collided = tested.clone();
    collided.push(COLLIDE);
    let steps: Vec<(&'static str, String, bool)> = vec![
        ("no edit", base.to_string(), false),
        ("an edit inside the last function's body", body, false),
        ("add a function and a caller", all(&[ADDED, CALLER]), false),
        (
            "change the signature the caller reads",
            all(&[ADDED_STR, CALLER]),
            true,
        ),
        ("change it back", all(&[ADDED, CALLER]), false),
        ("remove the function the caller calls", all(&[CALLER]), true),
        ("declare it again", all(&[ADDED, CALLER]), false),
        ("rename it", all(&[RENAMED, CALLER]), true),
        ("rename it back", all(&[ADDED, CALLER]), false),
        (
            "add a type and a reader of its field",
            all(&[ADDED, CALLER, POINT_A, FIELD]),
            false,
        ),
        (
            "rename the field",
            all(&[ADDED, CALLER, POINT_B, FIELD]),
            true,
        ),
        ("add module state and its reader", all(&typed), false),
        (
            "change the state's type",
            all(&[ADDED, CALLER, POINT_A, FIELD, STATE_STR, READER]),
            true,
        ),
        ("add a test", all(&tested), false),
        (
            "edit the test",
            all(&[&typed[..], &[TEST_BAD]].concat()),
            true,
        ),
        (
            "collide with a private name of std/strings",
            all(&collided),
            false,
        ),
        ("move every line", format!("\n{}", all(&collided)), false),
        ("instantiate a generic at two types", all(&[GENERIC]), false),
        ("back to the start", base.to_string(), false),
    ];
    (steps.into_iter())
        .map(|(what, root, refused)| Edit {
            what,
            root,
            overlays: HashMap::new(),
            refused,
        })
        .collect()
}

fn program(rel: &str) -> (std::path::PathBuf, String) {
    let path = common::examples_dir()
        .join("..")
        .join(rel)
        .canonicalize()
        .unwrap();
    let path = std::path::PathBuf::from(path.to_string_lossy().trim_start_matches(r"\\?\"));
    let text = std::fs::read_to_string(&path).expect("read the program");
    (path, text)
}

#[test]
#[ignore = "walks corpus programs; in the gate list"]
fn corpus_edits_recheck_as_a_fresh_check() {
    for rel in [
        "site/export.vyrn",
        "examples/bin/server.vyrn",
        "examples/simdbench.vyrn",
    ] {
        let (path, text) = program(rel);
        // The second edit is inside one body, so most bodies are replayed.
        let (checked, replayed) = replay(&path, edits(&text))[1];
        assert!(
            replayed > checked,
            "{rel}: {checked} checked, {replayed} replayed"
        );
    }
}

/// A generator's input is an open buffer the root does not name. Its edit
/// changes what the generator writes, so the edit checks a body again.
#[test]
#[ignore = "walks corpus programs; in the gate list"]
fn a_generator_input_edit_rechecks_as_a_fresh_check() {
    let (path, text) = program("examples/gendemo.vyrn");
    let csv = path.with_file_name("data").join("palette.csv");
    let csv_text = std::fs::read_to_string(&csv).expect("read the generator's input");
    let csv = OriginMaps::norm_path_key(&csv.to_string_lossy());
    let mut steps = edits(&text);
    let at = steps.len();
    for (what, more) in [
        ("edit the generator's input", ",extra"),
        ("change the input back", ""),
    ] {
        steps.push(Edit {
            what,
            root: text.clone(),
            overlays: HashMap::from([(csv.clone(), format!("{csv_text}{more}"))]),
            refused: false,
        });
    }
    let rechecked = replay(&path, steps);
    assert!(
        rechecked[at].0 > 0,
        "the generator's input edit reached no body"
    );
}
