//! Every `tests/check/<name>.vyrn` is a program, and `<name>.stderr` beside it
//! holds what `vyrn check <name>.vyrn` prints, run from `tests/check/`. A
//! program with no `.stderr`, or an empty one, is accepted: exit 0 and nothing
//! printed. Any other program is refused: a nonzero exit and exactly the bytes
//! of its `.stderr`.
//!
//! Add a test by adding the two files. `VYRN_PIN=write` writes every `.stderr`
//! from the run and deletes the ones an accepted program leaves empty.
//! `VYRN_ONLY=<substring>` runs the programs whose name contains it.

mod common;
use common::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

fn check_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/check")
}

/// Checks one program; returns why it failed, or `None`.
fn verdict(name: &str, write: bool) -> Option<String> {
    let out = vyrn()
        .arg("check")
        .arg(name)
        .current_dir(check_dir())
        .output()
        .expect("run vyrn check");
    let got = norm(&out.stderr);
    let file = check_dir().join(name).with_extension("stderr");
    if write {
        if got.is_empty() {
            let _ = std::fs::remove_file(&file);
        } else {
            std::fs::write(&file, &got).expect("write the .stderr");
        }
    }
    let want = std::fs::read(&file).map(|b| norm(&b)).unwrap_or_default();
    let code = out.status.code();
    if got == want && out.status.success() == want.is_empty() {
        return None;
    }
    Some(format!(
        "{name}: exit {code:?}, and the program is {}\n{}",
        if want.is_empty() {
            "accepted"
        } else {
            "refused"
        },
        first_diff("stderr", "recorded", &want, "run", &got).unwrap_or_default()
    ))
}

#[test]
fn every_program_prints_its_stderr() {
    let only = std::env::var("VYRN_ONLY").unwrap_or_default();
    let mut names: Vec<String> = std::fs::read_dir(check_dir())
        .expect("read tests/check")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .filter(|n| n.strip_suffix(".vyrn").is_some_and(|s| s.contains(&only)))
        .collect();
    names.sort();
    assert!(
        !names.is_empty(),
        "no program in tests/check matches `{only}`"
    );

    let write = pin_write();
    let next = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                while let Some(name) = names.get(next.fetch_add(1, Ordering::Relaxed)) {
                    if let Some(why) = verdict(name, write) {
                        failures.lock().unwrap().push(why);
                    }
                }
            });
        }
    });
    let mut failures = failures.into_inner().unwrap();
    failures.sort();
    assert!(
        failures.is_empty(),
        "{} of {} programs moved; rewrite with VYRN_PIN=write and read the diff\n{}",
        failures.len(),
        names.len(),
        failures.join("\n")
    );
}

/// Prints what `vyrn check` prints for a program the load refuses, checked in this process.
fn check_here(root: &str) -> String {
    let src = std::fs::read_to_string(root).expect("read the program");
    let std_root = check_dir().join("../../../../std");
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(std_root.to_string_lossy().replace('\\', "/")),
        expansions: vyrn_frontend::project::Expansions::shared(),
        ..Default::default()
    };
    let engine = vyrn_genwasm::engine();
    let mut out = String::new();
    let loaded = {
        let resolver = vyrn_frontend::loader::DiskResolver;
        vyrn_lower::load_warned(&src, root, &opts, &resolver, Some(&*engine)).0
    };
    for d in loaded.err().expect("the program is refused") {
        let file = d.file.as_deref().unwrap_or(root);
        out += &format!("{file}:{}:{}: {}\n", d.line, d.col, d.message);
        if let Some(note) = &d.note {
            out += &format!("  note: {note}\n");
        }
    }
    out
}

/// Checking one program leaves nothing behind for the next: two programs checked in one
/// process, then the first again, print what a fresh `vyrn check` prints for each.
#[test]
fn a_check_in_one_process_prints_what_a_fresh_process_prints() {
    vyrn_frontend::movecheck::emit_nothing();
    let path = |name: &str| {
        let p = check_dir()
            .join(name)
            .canonicalize()
            .expect("the program exists");
        p.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .replace('\\', "/")
    };
    let first = path("mut_a_field_store.vyrn");
    let second = path("a_derive_entry_must_match_its_call.vyrn");
    let fresh = |root: &str| {
        let out = vyrn()
            .arg("check")
            .arg(root)
            .output()
            .expect("run vyrn check");
        assert!(!out.status.success(), "{root} is refused");
        norm(&out.stderr)
    };
    let runs = [check_here(&first), check_here(&second), check_here(&first)];
    assert_eq!(runs[0], fresh(&first));
    assert_eq!(runs[1], fresh(&second));
    assert_eq!(runs[2], runs[0]);
}

/// The options and resolver `vyrn` loads `root` with: the std root and the
/// nearest manifest's keys.
fn cli_load(root: &str) -> vyrn_frontend::loader::LoadOptions {
    let mut opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(
            check_dir()
                .join("../../../../std")
                .to_string_lossy()
                .replace('\\', "/"),
        ),
        expansions: vyrn_frontend::project::Expansions::shared(),
        ..Default::default()
    };
    let dir = Path::new(root).parent().expect("a file has a directory");
    if let Some(m) = vyrn_frontend::manifest::find(dir).expect("the manifest parses") {
        opts.aliases = m.dependencies.into_iter().collect();
        opts.alias_base = m.dir;
        opts.audience = m.audience;
        opts.artifacts = m.artifacts;
    }
    opts
}

/// A file's absolute path, slash-separated, as `vyrn` keys a root.
fn root_of(path: &Path) -> String {
    let p = path.canonicalize().expect("the program exists");
    p.to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/")
}

/// Building one program leaves nothing behind for the next: two programs
/// compiled in one process, then the first again, write the module a fresh
/// `vyrn build --target wasm` writes for each. The first runs a `derive`
/// generator, the second a generator import.
#[test]
fn a_build_in_one_process_writes_what_a_fresh_process_writes() {
    let examples = check_dir().join("../../../../examples");
    let first = root_of(&examples.join("derive.vyrn"));
    let second = root_of(&examples.join("gendemo.vyrn"));
    let engine = vyrn_genwasm::engine();
    let here = |root: &str| -> Vec<u8> {
        let src = std::fs::read_to_string(root).expect("read the program");
        let opts = cli_load(root);
        let resolver = vyrn_frontend::loader::DiskResolver;
        let (program, world) =
            vyrn_lower::load_warned(&src, root, &opts, &resolver, Some(&*engine))
                .0
                .expect("the program loads");
        vyrn_codegen::direct::compile(&program, world).expect("the program compiles")
    };
    let dir = scratch("one-process-build");
    let fresh = |root: &str| -> Vec<u8> {
        let out = dir.join("out.wasm");
        let run = vyrn()
            .args(["build", root, "--target", "wasm", "-o"])
            .arg(&out)
            .output()
            .expect("run vyrn build");
        assert!(run.status.success(), "{}", norm(&run.stderr));
        std::fs::read(&out).expect("read the module")
    };
    let runs = [here(&first), here(&second), here(&first)];
    assert!(runs[0] == fresh(&first), "derive.vyrn built after nothing");
    assert!(
        runs[1] == fresh(&second),
        "gendemo.vyrn built after derive.vyrn"
    );
    assert!(runs[2] == runs[0], "derive.vyrn built after gendemo.vyrn");
}

/// The editor's analysis leaves nothing behind on its thread: two programs
/// analysed on one thread, then the first again, give the diagnostics a fresh
/// thread gives for each. Each thread arms what the editor's thread arms.
#[test]
fn an_editor_analysis_on_one_thread_gives_what_a_fresh_thread_gives() {
    let first = root_of(&check_dir().join("mut_a_field_store.vyrn"));
    let second = root_of(&check_dir().join("a_derive_entry_must_match_its_call.vyrn"));
    let analyze = |root: &str| -> String {
        let src = std::fs::read_to_string(root).expect("read the program");
        let opts = cli_load(root);
        let opts = vyrn_frontend::loader::LoadOptions {
            expansions: Default::default(),
            ..opts
        };
        let engine = vyrn_genwasm::engine();
        let resolver = vyrn_frontend::loader::DiskResolver;
        let a = vyrn_frontend::analyze_judged(
            &src,
            Some((root, &opts, &resolver)),
            Some(&*engine),
            &vyrn_lower::JUDGE,
        );
        assert!(!a.diagnostics.is_empty(), "{root} is refused");
        format!("{:?}", a.diagnostics)
    };
    let on_thread = |roots: Vec<String>| -> Vec<String> {
        std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(move || {
                vyrn_frontend::movecheck::reuse_judgments();
                vyrn_frontend::checker::record_reads();
                roots.iter().map(|r| analyze(r)).collect()
            })
            .expect("spawn a thread")
            .join()
            .expect("the analyses finish")
    };
    let runs = on_thread(vec![first.clone(), second.clone(), first.clone()]);
    assert_eq!(runs[0], on_thread(vec![first])[0]);
    assert_eq!(runs[1], on_thread(vec![second])[0]);
    assert_eq!(runs[2], runs[0]);
}
