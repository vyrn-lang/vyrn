//! Which hosts install a generation engine, and which install none.
//!
//! Every `.rs` file under `compiler/`, with its comment lines dropped, is a host
//! if it names a compile entry (`direct::compile`, `direct::compile_gen_host`,
//! `direct::wat`) or `vyrn_genwasm::install()`. A host with no engine fails
//! every `derive` and generator import, so [`NO_ENGINE`] names each one with
//! the reason. This census finds a host by what it calls, so a host that
//! forgets the line fails here.
//!
//! A file that reaches a backend only through `common::run_compiled` is not a
//! host: the helper is.

use std::path::{Path, PathBuf};

const COMPILES: &[&str] = &[
    "direct::compile(",
    "direct::compile_gen_host(",
    "direct::wat(",
];

const GENERATES: &str = "vyrn_genwasm::install()";

/// What fills `vyrn_frontend::gen`'s engine slot: the wasmtime engine, or a
/// host's own (the playground's, a test's).
const ENGINES: &[&str] = &[GENERATES, "set_gen_engine("];

/// The hosts that install no generation engine, each with the reason. Any
/// other host that compiles a `derive` or a generator import runs it.
const NO_ENGINE: &[(&str, &str)] = &[
    (
        "compiler/vyrn-cli/src/wasmrun.rs",
        "a module of the `vyrn` binary, whose `main` installs the engine",
    ),
    (
        "compiler/vyrn-frontend/tests/common/mod.rs",
        "`vyrn-frontend` cannot depend on `vyrn-genwasm`, which depends on it;          a `derive` there answers that no generation engine is installed",
    ),
];

/// This census names every marker above and is not a host.
const SELF: &str = "compiler/vyrn-cli/tests/hosts.rs";

#[test]
fn every_host_but_the_listed_ones_installs_a_generation_engine() {
    let without: Vec<String> = hosts()
        .into_iter()
        .filter_map(|(p, engine)| (!engine).then_some(p))
        .collect();
    let pinned: Vec<&str> = NO_ENGINE.iter().map(|(p, _)| *p).collect();
    assert_eq!(
        without, pinned,
        "a host compiles programs with no generation engine, so a `derive` or a          generator import in them fails; install one or list it with the reason"
    );
}

/// Every host under `compiler/`, in path order, with whether it installs a
/// generation engine.
fn hosts() -> Vec<(String, bool)> {
    let root = repo_root();
    let mut out = Vec::new();
    walk(&root.join("compiler"), &mut |p| {
        if p.extension().is_none_or(|e| e != "rs") {
            return;
        }
        let src = std::fs::read_to_string(p).expect("read a source file");
        let body: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        if !COMPILES.iter().any(|n| body.contains(n)) && !body.contains(GENERATES) {
            return;
        }
        let rel = p
            .strip_prefix(&root)
            .expect("a path under the root")
            .to_string_lossy()
            .replace('\\', "/");
        if rel == SELF {
            return;
        }
        let engine = ENGINES.iter().any(|e| body.contains(e));
        out.push((rel, engine));
    });
    out.sort();
    out
}

/// Every file under `dir`, skipping build output.
fn walk(dir: &Path, f: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .map(|e| e.expect("a directory entry").path())
        .collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            walk(&p, f);
        } else {
            f(&p);
        }
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}
