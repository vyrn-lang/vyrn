//! The structural census of the CLI crate: `main.rs`, `wasmrun.rs`, `remote.rs`
//! and `lib.rs`, tiled by kind and again by command.
//!
//! A section is an anchor item plus every item after it up to the next anchor,
//! from the anchor's own doc comment, so the sections tile each file. The second
//! column counts `eprintln!` sites: what the driver says in its own words. All
//! four files count together, so a rule moving between them is not a deletion.

mod common;

use std::path::{Path, PathBuf};

/// What a section of the CLI is.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Kind {
    /// A command's own path: arguments, the passes, the output, the exit code.
    ///
    /// The payload names the command, so this kind tiles a second time. Commands
    /// that share a path share an entry (`"serve, dev"`). `"(global)"` is the
    /// flags read before the subcommand; `"(dispatch)"` is `real_main`, which
    /// holds the `check`, `run` and `emit-*` arms inline.
    Cmd(&'static str),
    /// A rule the CLI states that the frontend, the lowering or the emitter also
    /// states. The deletion candidates.
    Restated,
    /// A path only a deleted route reached. Empty: `cargo check` reports no
    /// `dead_code` under `src/`.
    Dead,
    /// Machinery that belongs in a library crate because two commands and
    /// another binary each carry a copy. Each names the copies.
    Library,
    /// The WASI host and the wasmtime embedding: `wasmrun.rs`, and the library
    /// face that lets `vyrn-frontend`'s tests run a program the way the driver
    /// runs it.
    Host,
    /// Shared machinery: the dispatcher's helpers, the load site, the HTTP host
    /// both `serve` and `dev` answer on, the remote resolver.
    Shared,
    /// The crate's own unit tests.
    Tests,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Cmd(_) => "a command's own path",
            Kind::Restated => "a rule another pass also states",
            Kind::Dead => "a path only a deleted route reached",
            Kind::Library => "machinery with a copy elsewhere",
            Kind::Host => "the WASI host and the wasmtime embedding",
            Kind::Shared => "shared machinery",
            Kind::Tests => "tests",
        }
    }
}

/// One section: the head of the item that starts it, and its kind.
struct Section {
    at: &'static str,
    kind: Kind,
}

const fn sec(at: &'static str, kind: Kind) -> Section {
    Section { at, kind }
}

/// The files this census tiles, in the order the table prints them.
fn files() -> Vec<(&'static str, Vec<Section>)> {
    vec![
        ("compiler/vyrn-cli/src/main.rs", main_sections()),
        ("compiler/vyrn-cli/src/wasmrun.rs", wasmrun_sections()),
        ("compiler/vyrn-cli/src/remote.rs", remote_sections()),
        ("compiler/vyrn-cli/src/lib.rs", lib_sections()),
    ]
}

/// `main.rs`, in file order. The first section starts at line 1.
fn main_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec("mod remote", Shared),
        sec("fn wants_version", Cmd("(global)")),
        sec("enum NativeTarget", Cmd("build, bench")),
        sec("fn main", Shared),
        sec("fn real_main", Cmd("(dispatch)")),
        sec("fn failed", Shared),
        sec("fn emit_gen", Cmd("emit-gen")),
        sec("fn generated", Shared),
        sec("fn scaffold", Cmd("new")),
        sec("fn why_cmd", Cmd("why")),
        sec("fn routes_cmd", Cmd("routes")),
        sec("fn routes_json", Cmd("routes")),
        sec("fn json_str", Shared),
        sec("fn why_memory", Cmd("why")),
        sec("fn why_audience", Cmd("why")),
        sec("fn why_capability", Cmd("why")),
        sec("const MAX_CHAINS", Shared),
        sec("fn chains_from", Shared),
        sec("fn rel_to", Shared),
        sec("fn project_imports", Cmd("why")),
        sec("type ToolRow", Cmd("deps")),
        sec("fn deps", Cmd("deps")),
        sec("fn fmt_cmd", Cmd("fmt")),
        sec("const FROM_JSON_SRC", Cmd("fmt")),
        sec("struct DocModule", Cmd("doc")),
        sec("fn closure_doc_modules", Cmd("doc")),
        sec("fn render_doc_index", Cmd("doc")),
        sec("fn fix_cmd", Cmd("fix")),
        sec("fn synth_fn", Shared),
        sec("fn add", Cmd("add")),
        sec("fn update_tool", Cmd("update")),
        sec("fn update", Cmd("update")),
        sec("fn vendor", Cmd("vendor")),
        sec("fn json_pretty", Shared),
        sec("fn test_cmd", Cmd("test")),
        sec("fn bench_cmd", Cmd("bench")),
        sec("fn bench_native", Cmd("bench")),
        sec("fn bench_ungate_list", Cmd("bench")),
        sec("fn bench_compare", Cmd("bench")),
        sec("struct ServeRequest", Shared),
        sec("const SERVE_SHIM", Cmd("serve, dev")),
        sec("fn serve_rewrite", Cmd("serve, dev")),
        sec("fn serve_cmd", Cmd("serve")),
        sec("fn has_served_handle", Cmd("serve, dev")),
        sec("fn serve_pool_wasm", Cmd("serve, dev")),
        sec("fn dev_cmd", Cmd("dev")),
        sec("struct DevAssets", Cmd("dev")),
        sec("fn request_header", Shared),
        sec("fn serve_one", Shared),
        sec("fn reason_phrase", Shared),
        sec("fn pump_stream", Shared),
        sec("enum WsIn", Shared),
        sec("fn write_response", Shared),
        sec("fn run_wasm", Cmd("run")),
        sec("struct Body", Cmd("test, bench")),
        sec("fn build", Cmd("build")),
        sec("fn build_wasm2c", Cmd("build")),
        sec("mod tests", Tests),
    ]
}

fn wasmrun_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec("struct Outcome", Host),
        sec("struct Run", Host),
        sec("struct Meter", Host),
        sec("const SUCCESS", Host),
        sec("struct Exit", Host),
        sec("struct Host", Host),
        sec("fn engine", Host),
        sec("fn run", Host),
        sec("fn open", Host),
        sec("struct Compiled", Host),
        sec("struct Resident", Host),
        sec("fn first_line", Host),
        sec("fn link_wasi", Host),
        sec("mod tests", Tests),
    ]
}

fn remote_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec("pub use vyrn_frontend::hash::sha256_hex;", Shared),
        sec("fn resolve_to_url", Shared),
        sec("fn upstream_changed", Shared),
        sec("struct RemoteResolver", Shared),
        sec("mod tests", Tests),
    ]
}

fn lib_sections() -> Vec<Section> {
    vec![sec("mod wasmrun", Kind::Host)]
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn source(rel: &str) -> Vec<String> {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("read {rel}: {e}"))
        .replace("\r\n", "\n")
        .lines()
        .map(str::to_string)
        .collect()
}

fn spans(rel: &str, lines: &[String], secs: &[Section]) -> Vec<(usize, usize, usize)> {
    common::census_spans(rel, lines, secs.iter().map(|s| s.at))
}

/// How many sentences of its own a span states on stderr.
fn messages(lines: &[String], a: usize, b: usize) -> usize {
    lines[a - 1..b]
        .iter()
        .filter(|l| l.contains("eprintln!("))
        .count()
}

#[test]
fn the_structural_census_covers_the_crate() {
    for (rel, secs) in files() {
        let lines = source(rel);
        let mut next = 1;
        for (_, a, b) in spans(rel, &lines, &secs) {
            assert_eq!(next, a, "a gap or an overlap at line {a} of {rel}");
            next = b + 1;
        }
        assert_eq!(
            next - 1,
            lines.len(),
            "the last section does not reach the end of {rel}"
        );
    }
}

/// The line and stderr-sentence counts per kind, pinned.
#[test]
fn the_structural_census_matches_its_pin() {
    let mut by_kind = std::collections::BTreeMap::new();
    let mut msg_by_kind = std::collections::BTreeMap::new();
    let mut total = 0usize;
    let mut total_msgs = 0usize;
    for (rel, secs) in files() {
        let lines = source(rel);
        total += lines.len();
        total_msgs += messages(&lines, 1, lines.len());
        for (i, a, b) in spans(rel, &lines, &secs) {
            *by_kind.entry(secs[i].kind.label()).or_insert(0usize) += b - a + 1;
            *msg_by_kind.entry(secs[i].kind.label()).or_insert(0usize) += messages(&lines, a, b);
        }
    }
    let got: Vec<(&'static str, usize, usize)> = [
        Kind::Cmd(""),
        Kind::Restated,
        Kind::Dead,
        Kind::Library,
        Kind::Host,
        Kind::Shared,
        Kind::Tests,
    ]
    .iter()
    .map(|k| {
        (
            k.label(),
            by_kind.get(k.label()).copied().unwrap_or(0),
            msg_by_kind.get(k.label()).copied().unwrap_or(0),
        )
    })
    .collect();
    common::pin(
        "cli-census",
        "kind\tlines\tsentences",
        got.iter().map(|(k, n, m)| format!("{k}\t{n}\t{m}")),
    );
    assert_eq!(
        got.iter().map(|(_, n, _)| n).sum::<usize>(),
        total,
        "the kinds do not add up to the crate"
    );
    assert_eq!(
        got.iter().map(|(_, _, n)| n).sum::<usize>(),
        total_msgs,
        "the sentence counts do not add up to the crate"
    );
}

/// The lines and stderr sentences of every entry of the per-command tile, in the
/// order the table prints them: most lines first, then by name.
fn per_command() -> Vec<(&'static str, usize, usize)> {
    let mut by_cmd: std::collections::BTreeMap<&'static str, (usize, usize)> =
        std::collections::BTreeMap::new();
    for (rel, secs) in files() {
        let lines = source(rel);
        for (i, a, b) in spans(rel, &lines, &secs) {
            let Kind::Cmd(name) = secs[i].kind else {
                continue;
            };
            let row = by_cmd.entry(name).or_insert((0, 0));
            row.0 += b - a + 1;
            row.1 += messages(&lines, a, b);
        }
    }
    let mut got: Vec<(&'static str, usize, usize)> =
        by_cmd.into_iter().map(|(k, (n, m))| (k, n, m)).collect();
    got.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    got
}

/// Lines and stderr sentences per command, so a deletion is ranked before it
/// is made.
#[test]
fn the_per_command_census_matches_its_pin() {
    common::pin(
        "cli-commands",
        "command\tlines\tsentences",
        per_command()
            .iter()
            .map(|(c, n, m)| format!("{c}\t{n}\t{m}")),
    );
}
