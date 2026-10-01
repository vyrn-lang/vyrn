//! The editor's load reuse (the loader's memos and the session's disk):
//! after every edit of a sequence, the analysis of a session that has loaded
//! every earlier text gives what a fresh session's analysis gives, byte for
//! byte.
//!
//! Both sessions run the editor's pipeline (`analyze_judged` with
//! `vyrn_lower::JUDGE` and the generation engine) with the judgment memo, on
//! threads of their own, and read the same open buffers.

use std::collections::HashMap;
use std::sync::{mpsc, Arc};

use vyrn_frontend::loader::{DiskResolver, LoadOptions, ModuleResolver};
use vyrn_frontend::origin::OriginMaps;
use vyrn_frontend::session::Session;
use vyrn_frontend::Analysis;

mod common;

/// The session's disk, with some files' text replaced, as the editor's open
/// buffers replace them, keyed by [`OriginMaps::norm_path_key`].
struct Overlaid(HashMap<String, String>, Arc<Session>);

impl ModuleResolver for Overlaid {
    fn read(&self, resolved: &str) -> Result<String, String> {
        match self.0.get(&OriginMaps::norm_path_key(resolved)) {
            Some(text) => Ok(text.clone()),
            None => self.1.read(resolved),
        }
    }
    fn list_kinds(&self, resolved: &str) -> Result<Vec<String>, String> {
        self.1.list_kinds(resolved)
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
/// typed bindings and memory notes.
fn shown(a: &Analysis) -> String {
    format!(
        "{:#?}\n{:#?}\n{:#?}\n{:#?}",
        a.diagnostics, a.remapped, a.locals, a.memory
    )
}

fn analyze(path: &str, e: &Edit, session: &Arc<Session>) -> Analysis {
    let opts = LoadOptions {
        std_root: vyrn_frontend::manifest::std_root(),
        session: Some(session.clone()),
        ..Default::default()
    };
    let resolver = Overlaid(e.overlays.clone(), session.clone());
    let engine = vyrn_genwasm::engine();
    vyrn_frontend::analyze_judged(
        &e.root,
        Some((path, &opts, &resolver)),
        Some(&*engine),
        &vyrn_lower::JUDGE,
    )
}

/// Replays `edits` over the program at `path` in one editing session, and
/// compares each analysis with a fresh session's.
fn replay(path: &str, edits: Vec<Edit>) {
    let edits = std::sync::Arc::new(edits);
    let (ask, asked) = mpsc::channel::<usize>();
    let (tell, told) = mpsc::channel::<String>();
    let (p, es) = (path.to_string(), edits.clone());
    let editor = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let session = Session::new(true);
            for i in asked {
                let shown = shown(&analyze(&p, &es[i], &session));
                tell.send(shown).expect("the test listens");
            }
        })
        .expect("spawn the editing thread");
    for (i, e) in edits.iter().enumerate() {
        ask.send(i).expect("the editing thread listens");
        let reused = told.recv().expect("the editing thread answers");
        let (p, es) = (path.to_string(), edits.clone());
        let fresh = std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                let a = analyze(&p, &es[i], &Session::new(true));
                (shown(&a), a.diagnostics)
            })
            .expect("spawn a fresh thread")
            .join()
            .expect("the fresh thread panicked");
        let errors = fresh
            .1
            .iter()
            .any(|d| d.severity == vyrn_frontend::diagnostics::Severity::Error);
        assert_eq!(errors, e.refused, "{}: {:#?}", e.what, fresh.1);
        if let Some(d) = common::first_diff("analysis", "fresh", &fresh.0, "reused", &reused) {
            panic!("{}\n{d}", e.what);
        }
    }
    drop(ask);
    editor.join().expect("the editing thread panicked");
}

const ROOT: &str = r#"import { helper } from "./other"
import { consts } from "./gen"
import { n } from consts("./data")
import { trim } from "std/strings"

fn main() -> Int64 {
    print(trim(" a "))
    return helper(n())
}
"#;

const OTHER: &str = "export fn helper(x: Int64) -> Int64 {\n    return x + 1\n}\n";

/// A second entry in `data/` adds a declaration that refuses.
const GEN: &str = r#"export gen fn consts(dir: String) -> String {
    let many = match listDir(dir) {
        Ok(names) => names.length > 1,
        Err(e) => false,
    }
    let extra = if many { "\nexport fn m() -> Int64 { return \"many\" }" } else { "" }
    return match readFile("./data/n.txt") {
        Ok(s) => "export fn n() -> Int64 { return " + s + " }" + extra,
        Err(e) => e,
    }
}
"#;

/// A private function of `std/strings`: a root declaration of the same name
/// renames both apart, which rewrites the module's bodies and keeps its text.
const COLLIDE: &str = "\nfn isAsciiSpace(b: Int64) -> String {\n    return \"\"\n}\n";

/// A root, a module it imports, a generator it imports and the generator's
/// input, written to a fresh directory.
fn fixture(tag: &str) -> common::Scratch {
    let dir = common::scratch(tag);
    for (name, text) in [
        ("main.vyrn", ROOT),
        ("other.vyrn", OTHER),
        ("gen.vyrn", GEN),
        ("data/n.txt", "1"),
    ] {
        let file = dir.join(name);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    dir
}

/// Each file of the fixture changes once, and the whole program changes back.
#[test]
fn a_reused_load_analyzes_as_a_fresh_one() {
    let dir = fixture("reload");
    let at = |name: &str| {
        let p = dir.join(name).to_string_lossy().replace('\\', "/");
        (OriginMaps::norm_path_key(&p), p)
    };
    let (other, _) = at("other.vyrn");
    let (data, _) = at("data/n.txt");
    let (_, path) = at("main.vyrn");
    let body = ROOT.replace(
        "    return helper(n())",
        "    let k = n()\n    return helper(k)",
    );
    let steps: Vec<(&'static str, String, Vec<(&String, String)>, bool)> = vec![
        ("no edit", ROOT.to_string(), vec![], false),
        ("an edit inside the root's body", body, vec![], false),
        (
            "declare a private name of std/strings in the root",
            format!("{ROOT}{COLLIDE}"),
            vec![],
            false,
        ),
        (
            "add a namespace import of std/json",
            format!("import * as js from \"std/json\"\n{ROOT}"),
            vec![],
            false,
        ),
        (
            "the other module mentions toJson",
            ROOT.to_string(),
            vec![(
                &other,
                format!(
                    "{OTHER}export fn shown(x: Int64) -> String {{\n    return toJson(x)\n}}\n"
                ),
            )],
            false,
        ),
        (
            "the other module refuses",
            ROOT.to_string(),
            vec![(&other, OTHER.replace("return x + 1", "return \"a\""))],
            true,
        ),
        (
            "edit the generator's input",
            ROOT.to_string(),
            vec![(&data, "\"a\"".to_string())],
            true,
        ),
        ("back to the start", ROOT.to_string(), vec![], false),
    ];
    let edits = (steps.into_iter())
        .map(|(what, root, overlays, refused)| Edit {
            what,
            root,
            overlays: (overlays.into_iter())
                .map(|(k, v)| (k.clone(), v))
                .collect(),
            refused,
        })
        .collect();
    replay(&path, edits);
}

/// One disk step: the files the test writes, the files the editing session is
/// told changed, and whether its analysis must show the writes so far.
struct DiskStep {
    what: &'static str,
    writes: Vec<(&'static str, String)>,
    events: Vec<&'static str>,
    seen: bool,
}

fn step(
    what: &'static str,
    writes: &[(&'static str, &str)],
    events: &[&'static str],
    seen: bool,
) -> DiskStep {
    DiskStep {
        what,
        writes: writes.iter().map(|(n, t)| (*n, t.to_string())).collect(),
        events: events.to_vec(),
        seen,
    }
}

/// Writes each step's files, tells an editing session that loads the fixture's
/// root the step's events, and compares its analysis with a fresh session's.
/// A seen step equals the fresh analysis. An unseen step equals the editing
/// session's previous analysis and differs from the fresh one. Every write
/// changes the fresh analysis, so an ignored event fails a seen step.
fn replay_disk(tag: &str, watch: bool, steps: Vec<DiskStep>) {
    let dir = fixture(tag);
    let at = |name: &str| dir.join(name).to_string_lossy().replace('\\', "/");
    let path = at("main.vyrn");
    let edit = std::sync::Arc::new(Edit {
        what: "the fixture's root",
        root: ROOT.to_string(),
        overlays: HashMap::new(),
        refused: false,
    });
    let (ask, asked) = mpsc::channel::<Vec<String>>();
    let (tell, told) = mpsc::channel::<String>();
    let (p, e, root) = (path.clone(), edit.clone(), at(""));
    let editor = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let session = Session::new(true);
            if watch {
                session.watch(&[root]);
            }
            for events in asked {
                for changed in events {
                    session.changed(&changed);
                }
                tell.send(shown(&analyze(&p, &e, &session)))
                    .expect("the test listens");
            }
        })
        .expect("spawn the editing thread");
    let (mut last, mut last_fresh) = (String::new(), String::new());
    for step in steps {
        for (name, text) in &step.writes {
            std::fs::write(at(name), text).unwrap();
        }
        let events = step.events.iter().map(|n| at(n)).collect();
        ask.send(events).expect("the editing thread listens");
        let reused = told.recv().expect("the editing thread answers");
        let (p, e) = (path.clone(), edit.clone());
        let fresh = std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || shown(&analyze(&p, &e, &Session::new(true))))
            .expect("spawn a fresh thread")
            .join()
            .expect("the fresh thread panicked");
        if !step.writes.is_empty() {
            assert_ne!(
                fresh, last_fresh,
                "{}: the write changes nothing",
                step.what
            );
        }
        if step.seen {
            if let Some(d) = common::first_diff("analysis", "fresh", &fresh, "reused", &reused) {
                panic!("{}\n{d}", step.what);
            }
        } else {
            assert_eq!(
                reused, last,
                "{}: the editing thread read the disk",
                step.what
            );
            assert_ne!(reused, fresh, "{}", step.what);
        }
        (last, last_fresh) = (reused, fresh);
    }
    drop(ask);
    editor.join().expect("the editing thread panicked");
}

/// A session that watches the fixture keeps what it read until an event names
/// the file: a generator's input, an imported module, and an entry created in
/// a directory a generator lists.
#[test]
fn a_watched_load_reads_a_file_again_only_after_its_event() {
    let refusing = OTHER.replace("return x + 1", "return \"a\"");
    replay_disk(
        "reload-watch",
        true,
        vec![
            step("no edit", &[], &[], true),
            step(
                "the generator's input changes without an event",
                &[("data/n.txt", "\"a\"")],
                &[],
                false,
            ),
            step("its event arrives", &[], &["data/n.txt"], true),
            step(
                "the input changes back, with its event",
                &[("data/n.txt", "1")],
                &["data/n.txt"],
                true,
            ),
            step(
                "the other module refuses, with its event",
                &[("other.vyrn", &refusing)],
                &["other.vyrn"],
                true,
            ),
            step(
                "the other module changes back, with its event",
                &[("other.vyrn", OTHER)],
                &["other.vyrn"],
                true,
            ),
            step(
                "a file created in the listed directory, with its event",
                &[("data/more.txt", "")],
                &["data/more.txt"],
                true,
            ),
        ],
    );
}

/// A session that is sent no events, as for a client that cannot send them,
/// reads every edit on disk.
#[test]
fn an_unwatched_load_reads_every_edit() {
    replay_disk(
        "reload-unwatched",
        false,
        vec![
            step("no edit", &[], &[], true),
            step(
                "the generator's input changes",
                &[("data/n.txt", "\"a\"")],
                &[],
                true,
            ),
            step(
                "a file created in the listed directory",
                &[("data/more.txt", "")],
                &[],
                true,
            ),
        ],
    );
}
