//! The editor's load reuse (`vyrn_frontend::loader`'s per-thread memos):
//! after every edit of a sequence, the analysis of a thread that has loaded
//! every earlier text gives what a fresh thread's analysis gives, byte for
//! byte.
//!
//! Both threads run the editor's pipeline (`analyze_judged` with
//! `vyrn_lower::JUDGE` and the generation engine), arm the judgment memo and
//! the read rows, and read the same open buffers.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;

use vyrn_frontend::loader::{DiskResolver, LoadOptions, ModuleResolver};
use vyrn_frontend::origin::OriginMaps;
use vyrn_frontend::Analysis;

mod common;

/// The disk, with some files' text replaced, as the editor's open buffers
/// replace them, keyed by [`OriginMaps::norm_path_key`].
struct Overlaid(HashMap<String, String>);

impl ModuleResolver for Overlaid {
    fn read(&self, resolved: &str) -> Result<String, String> {
        match self.0.get(&OriginMaps::norm_path_key(resolved)) {
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
/// typed bindings and memory notes.
fn shown(a: &Analysis) -> String {
    format!(
        "{:#?}\n{:#?}\n{:#?}\n{:#?}",
        a.diagnostics, a.remapped, a.locals, a.memory
    )
}

fn analyze(path: &str, e: &Edit) -> Analysis {
    let opts = LoadOptions {
        std_root: vyrn_frontend::manifest::std_root(),
        ..Default::default()
    };
    let resolver = Overlaid(e.overlays.clone());
    let engine = vyrn_genwasm::engine();
    vyrn_frontend::analyze_judged(
        &e.root,
        Some((path, &opts, &resolver)),
        Some(&*engine),
        &vyrn_lower::JUDGE,
    )
}

/// Arms what the editor's analysis thread arms.
fn arm() {
    vyrn_frontend::movecheck::reuse_judgments();
    vyrn_frontend::checker::record_reads();
}

/// Replays `edits` over the program at `path` on one editing thread, and
/// compares each analysis with a fresh thread's.
fn replay(path: &str, edits: Vec<Edit>) {
    let edits = std::sync::Arc::new(edits);
    let (ask, asked) = mpsc::channel::<usize>();
    let (tell, told) = mpsc::channel::<String>();
    let (p, es) = (path.to_string(), edits.clone());
    let editor = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            arm();
            for i in asked {
                let shown = shown(&analyze(&p, &es[i]));
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
                arm();
                let a = analyze(&p, &es[i]);
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

const GEN: &str = r#"export gen fn consts(dir: String) -> String {
    return match readFile("./data/n.txt") {
        Ok(s) => "export fn n() -> Int64 { return " + s + " }",
        Err(e) => e,
    }
}
"#;

/// A private function of `std/strings`: a root declaration of the same name
/// renames both apart, which rewrites the module's bodies and keeps its text.
const COLLIDE: &str = "\nfn isAsciiSpace(b: Int64) -> String {\n    return \"\"\n}\n";

/// A root, a module it imports, a generator it imports and the generator's
/// input: each changes once, and the whole program changes back.
#[test]
fn a_reused_load_analyzes_as_a_fresh_one() {
    let dir = common::scratch("reload");
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
