//! Which hosts compile with a core, and which compile with none — RFC-0125 §3 M3.
//!
//! `vyrn_lower::install()` is what puts `core::augment` into `own::analyze`
//! (`own::install_placer`), the kernel's refusals into the one list a file's
//! refusals come out in, the must-use judgment into the same list, and the
//! effect judgment into the floor and the isolation rule. A process that does
//! not call it runs a DIFFERENT compiler: the placer never runs, `core::BODIES`
//! stays empty, `Fn_::core` is `None` for every function, and the emitter's AST
//! dispatch is the whole of it. Its refusals are the ones `movecheck.rs` still
//! states and not the kernel's.
//!
//! `vyrn-frontend/tests/semantics.rs` was such a host. 193 tests over the
//! emitter, none of them over the walk the emitter has taken since the driver
//! slice, and four live defects behind the one missing line. That is what this
//! census is for: a host is found by what it CALLS, not by what a reader
//! remembers, so a new one that forgets the line fails here.
//!
//! # The method
//!
//! Every `.rs` file under `compiler/`, with its comment lines dropped, is
//! searched for two families of marker:
//!
//!   - it COMPILES: it names `direct::compile`, `direct::compile_gen_host` or
//!     `direct::wat`.
//!   - it RUNS GENERATORS: it names `vyrn_genwasm::install()`, so it is a
//!     process that assembles an engine of its own and has to assemble the
//!     whole one.
//!
//! A file matching either is a host. [`HOSTS`] pins every host and the core it
//! compiles with, so a host that appears, disappears or changes side fails
//! [`the_host_table_is_what_the_sources_say`].
//!
//! A file that reaches a backend only through `common::run_compiled` is NOT a
//! host: the helper is, and it installs, so its callers cannot forget. That is
//! also the answer to what a per-FILE marker cannot see. `loader_run.rs` had
//! three run paths and installed on one — `tests::run_multi` did,
//! `remote_tests`' two callers and `run_with` did not — and no marker short of
//! a parser separates them. One helper that installs does.

use std::path::{Path, PathBuf};

/// Whether a host installs the lowering before it compiles.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Core {
    /// The host names `vyrn_lower::install()`.
    Installed,
    /// The host does not, with the reason. Every entry of this kind is a
    /// process that compiles with the AST dispatch alone.
    None_(&'static str),
}
use Core::{Installed, None_};

/// Every host, and the core it compiles with.
const HOSTS: &[(&str, Core)] = &[
    ("compiler/vyrn-cli/src/main.rs", Installed),
    ("compiler/vyrn-cli/src/wasmrun.rs", Installed),
    ("compiler/vyrn-cli/tests/coredrive.rs", Installed),
    ("compiler/vyrn-cli/tests/coretables.rs", Installed),
    ("compiler/vyrn-cli/tests/effects.rs", Installed),
    ("compiler/vyrn-cli/tests/kernel.rs", Installed),
    ("compiler/vyrn-cli/tests/lowered.rs", Installed),
    ("compiler/vyrn-cli/tests/projections.rs", Installed),
    ("compiler/vyrn-cli/tests/typed.rs", Installed),
    ("compiler/vyrn-frontend/tests/common/mod.rs", Installed),
    ("compiler/vyrn-frontend/tests/isolation.rs", Installed),
    ("compiler/vyrn-frontend/tests/loader_run.rs", Installed),
    ("compiler/vyrn-frontend/tests/semantics.rs", Installed),
    (
        "compiler/vyrn-genwasm/src/lib.rs",
        None_(
            "the generation engine, not a host: it compiles a generator inside \
             the process that installed it, and that process is a host of its own",
        ),
    ),
    ("compiler/vyrn-lsp/src/main.rs", Installed),
    ("compiler/vyrn-play/src/lib.rs", Installed),
];

/// A file compiles a program in this process.
const COMPILES: &[&str] = &[
    "direct::compile(",
    "direct::compile_gen_host(",
    "direct::wat(",
];

/// A file assembles a generation engine, so it assembles a compiler.
const GENERATES: &str = "vyrn_genwasm::install()";

/// The line every host needs.
const INSTALL: &str = "vyrn_lower::install()";

/// This census, which names every marker above and is not a host.
const SELF: &str = "compiler/vyrn-cli/tests/hosts.rs";

#[test]
fn the_only_thing_that_compiles_without_a_core_is_not_a_process() {
    let without: Vec<&str> = HOSTS
        .iter()
        .filter_map(|(p, c)| matches!(c, None_(_)).then_some(*p))
        .collect();
    assert_eq!(
        without,
        ["compiler/vyrn-genwasm/src/lib.rs"],
        "a host compiles with no core. Its emitter is the AST dispatch alone and \
         its refusals are not the kernel's, so it is a second compiler with a \
         second, weaker rule"
    );
}

#[test]
fn the_host_table_is_what_the_sources_say() {
    let found: Vec<(String, bool)> = hosts();
    let pinned: Vec<(String, bool)> = HOSTS
        .iter()
        .map(|(p, c)| ((*p).to_string(), *c == Installed))
        .collect();
    assert_eq!(
        found, pinned,
        "the hosts moved: a file that compiles or runs a generator was added, \
         removed, or changed side"
    );
}

/// Every host under `compiler/`, in path order, with whether it installs.
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
        out.push((rel, body.contains(INSTALL)));
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
