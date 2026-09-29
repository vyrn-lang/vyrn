//! The structural census of the CLI crate: `main.rs`, `wasmrun.rs`, `remote.rs`
//! and `lib.rs`, tiled by kind and again by command.
//!
//! A section is an anchor item plus every item after it up to the next anchor,
//! from the anchor's own doc comment, so the sections tile each file. The second
//! column counts `eprintln!` sites: what the driver says in its own words. All
//! four files count together, so a rule moving between them is not a deletion.

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

/// One section: the exact source line that starts it and its kind.
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
        sec("mod remote;", Shared),
        sec("fn offline(args: &[String]) -> bool {", Cmd("(global)")),
        sec("enum NativeTarget {", Cmd("build, bench")),
        sec("fn main() -> ExitCode {", Shared),
        sec("fn real_main() -> ExitCode {", Cmd("(dispatch)")),
        sec("fn emit_gen(path: &str, source: &str, maps: bool) -> ExitCode {", Cmd("emit-gen")),
        sec("fn nearest_manifest(start: &Path) -> Option<Manifest> {", Shared),
        sec("fn scaffold(name: &str) -> ExitCode {", Cmd("new")),
        sec("fn why_cmd(args: &[String]) -> ExitCode {", Cmd("why")),
        sec("fn routes_cmd(file: Option<&str>, json: bool) -> ExitCode {", Cmd("routes")),
        sec("fn routes_json(", Cmd("routes")),
        sec("fn json_str(s: &str) -> String {", Shared),
        sec("fn why_memory(file: &str) -> ExitCode {", Cmd("why")),
        sec("fn why_audience(file: &str) -> ExitCode {", Cmd("why")),
        sec("fn why_capability(cap: &str, name: &str) -> ExitCode {", Cmd("why")),
        sec("const MAX_CHAINS: usize = 24;", Shared),
        sec("fn chains_from(entry: &str, target: &str, edges: &[(String, String)]) -> Vec<Vec<String>> {", Shared),
        sec("fn rel_to(path: &str, base: &str) -> String {", Shared),
        sec("fn project_imports(app_dir: &Path) -> Vec<(String, String)> {", Restated),
        sec("type ToolRow = (String, String, String, String);", Cmd("deps")),
        sec("fn deps(name: Option<&str>) -> ExitCode {", Cmd("deps")),
        sec("fn fmt_cmd(rest: &[String]) -> ExitCode {", Cmd("fmt")),
        sec("const FROM_JSON_SRC: &str = r#\"import { parseJson } from \"std/jsonread\"", Cmd("fmt")),
        sec("fn fmt_project_files() -> Result<Vec<String>, ExitCode> {", Cmd("fmt")),
        sec("struct DocModule {", Cmd("doc")),
        sec("fn closure_doc_modules(root_file: &str, with_std: bool) -> Result<Vec<DocModule>, ExitCode> {", Cmd("doc")),
        sec("fn render_doc_index(modules: &[DocModule]) -> String {", Cmd("doc")),
        sec("fn lock_home(root_key: &str) -> (PathBuf, Option<String>) {", Shared),
        sec("fn fix_cmd(path: &str, source: &str) -> ExitCode {", Cmd("fix")),
        sec("fn synth_fn(", Shared),
        sec("fn load_program(path: &str, source: &str) -> Result<vyrn_frontend::ast::Program, ExitCode> {", Shared),
        sec("fn add(rest: &[String], _offline: bool) -> ExitCode {", Cmd("add")),
        sec("fn update_tool(name: &str, version: &str, lock: &mut remote::Lock) -> Result<(), String> {", Cmd("update")),
        sec("fn update(alias: Option<&str>, locked: bool) -> ExitCode {", Cmd("update")),
        sec("fn vendor(check: bool) -> ExitCode {", Cmd("vendor")),
        sec("fn json_pretty(j: &vyrn_frontend::schema::Json, depth: usize) -> String {", Shared),
        sec("fn test_cmd(path: &str, rest: &[String]) -> ExitCode {", Cmd("test")),
        sec("fn bench_cmd(path: &str, rest: &[String]) -> ExitCode {", Cmd("bench")),
        sec("fn bench_native(", Cmd("bench")),
        sec("fn bench_ungate_list(text: &str) -> Vec<String> {", Cmd("bench")),
        sec("fn bench_compare(", Cmd("bench")),
        sec("pub struct ServeRequest {", Shared),
        sec("const SERVE_SHIM: &str = r#\"", Cmd("serve, dev")),
        sec("fn serve_rewrite(program: &mut vyrn_frontend::ast::Program) {", Cmd("serve, dev")),
        sec("fn serve_cmd(path: &str, rest: &[String]) -> ExitCode {", Cmd("serve")),
        sec("fn has_served_handle(program: &vyrn_frontend::ast::Program) -> bool {", Cmd("serve, dev")),
        sec("fn serve_pool_wasm<W, A>(", Cmd("serve, dev")),
        sec("fn dev_cmd(rest: &[String]) -> ExitCode {", Cmd("dev")),
        sec("struct DevAssets {", Cmd("dev")),
        sec("fn request_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {", Shared),
        sec("fn serve_one(", Shared),
        sec("fn reason_phrase(status: i64) -> &'static str {", Shared),
        sec("fn pump_stream(", Shared),
        sec("enum WsIn {", Shared),
        sec("fn write_response(stream: &mut std::net::TcpStream, status: i64, content_type: &str, body: &[u8]) {", Shared),
        sec("fn run_wasm(", Cmd("run")),
        sec("struct Body {", Cmd("test, bench")),
        sec("fn build(path: &str, rest: &[String]) -> ExitCode {", Cmd("build")),
        sec("fn build_wasm2c(", Cmd("build")),
        sec("mod tests {", Tests),
    ]
}

fn wasmrun_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec("pub struct Outcome {", Host),
        sec("pub struct Run {", Host),
        sec("pub struct Meter {", Host),
        sec("const SUCCESS: i32 = 0;", Host),
        sec("struct Exit(i32);", Host),
        sec("struct Host {", Host),
        sec("fn engine(metered: bool) -> &'static Engine {", Host),
        sec(
            "pub fn run(bytes: &[u8], run: Run) -> Result<Outcome, String> {",
            Host,
        ),
        sec("fn open(", Host),
        sec("pub struct Compiled {", Host),
        sec("pub struct Resident {", Host),
        sec("fn first_line(s: &str) -> &str {", Host),
        sec(
            "fn link_wasi(linker: &mut Linker<Host>) -> wasmtime::Result<()> {",
            Host,
        ),
        sec("mod tests {", Tests),
    ]
}

fn remote_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec("pub use vyrn_frontend::hash::sha256_hex;", Shared),
        sec("pub fn resolve_to_url(spec: &str) -> Result<String, String> {", Shared),
        sec("pub fn upstream_changed(spec: &str, url: &str, got: &str, pinned: &str, remedy: &str) -> String {", Shared),
        sec("pub struct RemoteResolver {", Shared),
        sec("mod tests {", Tests),
    ]
}

fn lib_sections() -> Vec<Section> {
    vec![sec("pub mod wasmrun;", Kind::Host)]
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

/// Where a section's doc starts: the run of comment and attribute lines above
/// the anchor.
fn doc_start(lines: &[String], anchor: usize) -> usize {
    let mut i = anchor;
    while i > 0 {
        let t = lines[i - 1].trim_start();
        if t.starts_with("//") || t.starts_with("#[") {
            i -= 1;
        } else {
            break;
        }
    }
    i
}

/// The sections of one file, with the span each holds: `(index, first, last)`,
/// one-based and inclusive. Every line of the file is in exactly one span.
fn spans(rel: &str, lines: &[String], secs: &[Section]) -> Vec<(usize, usize, usize)> {
    let mut anchors = Vec::new();
    for s in secs {
        let want: String = s.at.split_whitespace().collect::<Vec<_>>().join(" ");
        let hits: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.split_whitespace().collect::<Vec<_>>().join(" ") == want)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "the anchor `{}` names {} lines of {rel}; a section's anchor must name one",
            s.at,
            hits.len()
        );
        anchors.push(doc_start(lines, hits[0]));
    }
    let mut out = Vec::new();
    for i in 0..secs.len() {
        let first = if i == 0 { 0 } else { anchors[i] };
        let last = if i + 1 == secs.len() {
            lines.len()
        } else {
            anchors[i + 1]
        };
        assert!(
            first < last,
            "section `{}` of {rel} is empty or out of order",
            secs[i].at
        );
        out.push((i, first + 1, last));
    }
    out
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
    let want = vec![
        ("a command's own path", 4461, 167),
        ("a rule another pass also states", 150, 0),
        ("a path only a deleted route reached", 0, 0),
        ("machinery with a copy elsewhere", 0, 0),
        ("the WASI host and the wasmtime embedding", 1073, 0),
        ("shared machinery", 1271, 19),
        ("tests", 755, 2),
    ];
    assert_eq!(got, want, "the structural census has moved");
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
    let want = vec![
        ("bench", 567, 17),
        ("why", 504, 19),
        ("serve, dev", 458, 7),
        ("doc", 370, 13),
        ("routes", 303, 4),
        ("build", 260, 17),
        ("fmt", 248, 13),
        ("(dispatch)", 226, 12),
        ("dev", 222, 18),
        ("deps", 210, 5),
        ("update", 195, 7),
        ("fix", 192, 1),
        ("build, bench", 127, 0),
        ("test, bench", 124, 2),
        ("serve", 87, 8),
        ("add", 70, 6),
        ("vendor", 66, 6),
        ("run", 61, 3),
        ("emit-gen", 57, 3),
        ("test", 46, 2),
        ("new", 41, 4),
        ("(global)", 27, 0),
    ];
    assert_eq!(per_command(), want, "the per-command census has moved");
    let total: usize = per_command().iter().map(|(_, n, _)| n).sum();
    assert_eq!(
        total, 4461,
        "the per-command tile does not add up to its kind"
    );
}
