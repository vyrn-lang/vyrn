//! Under `vyrn check` a non-generic std function is given: its stored row
//! stands for its body (`loader::std_given`), and a written impl method's
//! row is served in place of its build (`stdrows::Given::serve`). Under
//! `Host::Emit` a std body no run reaches is given (`loader::parse_reached`).
//! A `Host::Report` load gives nothing and builds every body it links. Each
//! corpus root `scripts/check-corpus.sh` walks is loaded under the three
//! hosts, and each giving load must print the built load's diagnostics,
//! warnings and verdict: a given body's stored verdict is the verdict its
//! build gives in that program.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use vyrn_frontend::movecheck::{declare, Host};

mod common;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Every `.vyrn` file under `dir`, recursively, but a build's `target/`.
fn roots_in(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).expect("a corpus directory reads") {
        let p = e.expect("an entry reads").path();
        if p.is_dir() && !p.ends_with("target") {
            roots_in(&p, out);
        } else if p.extension().is_some_and(|x| x == "vyrn") {
            out.push(p);
        }
    }
}

/// What `vyrn check` decides of `path` under `host`: each diagnostic and
/// warning of the load, then the instantiation check's error, then `ok` or
/// `refused`.
fn verdict(path: &Path, host: Host) -> String {
    declare(host);
    let src = std::fs::read_to_string(path).expect("the root reads");
    let mut opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(slash(&repo_root().join("std"))),
        expansions: vyrn_frontend::project::Expansions::shared(),
        ..Default::default()
    };
    let dir = path.parent().expect("a file has a directory");
    if let Ok(Some(m)) = vyrn_frontend::manifest::find(dir) {
        m.load_options(&mut opts);
    }
    let engine = vyrn_genwasm::engine();
    let resolver = vyrn_frontend::loader::DiskResolver;
    let (loaded, warnings) =
        vyrn_lower::load_warned(&src, &slash(path), &opts, &resolver, Some(&*engine));
    let mut out: String = warnings.iter().map(|w| format!("{w:?}\n")).collect();
    match loaded {
        Ok((_, world)) => match vyrn_codegen::check_instantiations(&world) {
            Ok(()) => out += "ok\n",
            Err(e) => out += &format!("{e}\nrefused\n"),
        },
        Err(ds) => {
            out.extend(ds.iter().map(|d| format!("{d:?}\n")));
            out += "refused\n";
        }
    }
    out
}

/// The roots whose `Host::Check` and built loads may print different
/// diagnostics, each with its reason. Both loads still reach one verdict, and a root that agrees again
/// fails until it leaves this list.
const KNOWN: &[(&str, &str)] = &[(
    "compiler/vyrn-cli/tests/check/a_protocol_method_named_like_a_std_function_is_refused_at_the_method.vyrn",
    "a given std line's follow-on refusals are not printed (docs/decisions.md, given bodies)",
)];

/// The last line of a [`verdict`]: `ok` or `refused`.
fn last(v: &str) -> &str {
    v.lines().last().unwrap_or("")
}

#[test]
#[ignore = "checks every corpus root three times; run explicitly: cargo test -p vyrn-cli --test given -- --ignored"]
fn every_given_verdict_is_the_built_one() {
    let mut roots = Vec::new();
    for dir in ["examples", "std", "site", "compiler/vyrn-cli/tests"] {
        roots_in(&repo_root().join(dir), &mut roots);
    }
    roots.sort();
    for (k, _) in KNOWN {
        assert!(
            roots.contains(&repo_root().join(k)),
            "KNOWN names no root: {k}"
        );
    }
    let next = AtomicUsize::new(0);
    let moved = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..threads {
            // A debug build lowers the deepest roots past the default thread
            // stack; `fnverdicts` reserves the same.
            let deep =
                std::thread::Builder::new().stack_size(vyrn_frontend::trap::DEEP_STACK_BYTES);
            deep.spawn_scoped(s, || {
                while let Some(path) = roots.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let built = verdict(path, Host::Report);
                    let rel = slash(path.strip_prefix(repo_root()).expect("under the repo"));
                    let unrun = verdict(path, Host::Emit);
                    let unrun = common::first_diff(&rel, "unrun given", &unrun, "built", &built);
                    let given = verdict(path, Host::Check);
                    let known = KNOWN.iter().any(|(k, _)| *k == rel);
                    let d = common::first_diff(&rel, "given", &given, "built", &built);
                    let d = match (known, d) {
                        (true, Some(_)) if last(&given) == last(&built) => None,
                        (true, None) => Some(format!("  {rel}: agrees; remove it from KNOWN\n")),
                        (_, d) => d,
                    };
                    moved
                        .lock()
                        .expect("no worker panicked")
                        .extend(d.into_iter().chain(unrun));
                }
            })
            .expect("a worker starts");
        }
    });
    let mut moved = moved.into_inner().expect("no worker panicked");
    moved.sort();
    assert!(
        moved.is_empty(),
        "{} of {} roots print another verdict with every body built\n{}",
        moved.len(),
        roots.len(),
        moved.join("\n")
    );
}
