//! Which hosts compile with a core, and which compile with none.
//!
//! `vyrn_lower::install()` puts `core::augment` into `own::analyze`
//! (`own::install_placer`), the kernel's and the must-use refusals into a file's
//! one refusal list, and the effect judgment into the floor. A process that does
//! not call it runs a different compiler: `core::BODIES` stays empty,
//! `Fn_::core` is `None` for every function, the emitter refuses every body, and
//! the refusals are `movecheck.rs`'s, not the kernel's. This census finds a host
//! by what it calls, so a host that forgets the line fails here.
//!
//! Every `.rs` file under `compiler/`, with its comment lines dropped, is a host
//! if it names a compile entry (`direct::compile`, `direct::compile_gen_host`,
//! `direct::wat`) or `vyrn_genwasm::install()` (it assembles an engine of its
//! own, so it must assemble the whole one). [`HOSTS`] pins every host and its
//! core.
//!
//! A file that reaches a backend only through `common::run_compiled` is not a
//! host: the helper is, and it installs. A per-file marker cannot tell apart
//! several run paths in one file that install on only some of them; one helper
//! that installs removes the case.

use std::path::{Path, PathBuf};

/// Whether a host installs the lowering before it compiles.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Core {
    Installed,
    /// The host's emitter has no core to read, with the reason.
    None_(&'static str),
}
use Core::{Installed, None_};

const HOSTS: &[(&str, Core)] = &[
    ("compiler/vyrn-cli/src/main.rs", Installed),
    ("compiler/vyrn-cli/src/wasmrun.rs", Installed),
    ("compiler/vyrn-cli/tests/coredrive.rs", Installed),
    ("compiler/vyrn-cli/tests/coretables.rs", Installed),
    ("compiler/vyrn-cli/tests/effects.rs", Installed),
    ("compiler/vyrn-cli/tests/kernel.rs", Installed),
    ("compiler/vyrn-cli/tests/lowered.rs", Installed),
    ("compiler/vyrn-cli/tests/reproducible.rs", Installed),
    ("compiler/vyrn-cli/tests/typed.rs", Installed),
    ("compiler/vyrn-frontend/tests/common/mod.rs", Installed),
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

const COMPILES: &[&str] = &[
    "direct::compile(",
    "direct::compile_gen_host(",
    "direct::wat(",
];

const GENERATES: &str = "vyrn_genwasm::install()";

const INSTALL: &str = "vyrn_lower::install()";

/// This census names every marker above and is not a host.
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
        "a host compiles with no core. Its emitter refuses every body and its \
         refusals are not the kernel's, so it is a second compiler with a \
         second rule"
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
