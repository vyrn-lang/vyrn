//! The structural census of the driver — RFC-0125 §3 M5, the size strand.
//!
//! `compiler/vyrn-cli/src/main.rs` was 7,276 lines and 118 top-level functions
//! when this census was written.
//! §2.7 counts the compiler toward 40,000–45,000 lines, and since that estimate
//! was written the interpreter went (M5), the text-IR native route, `emit-ir`,
//! `--route` and the parity harness went (M3/M4), the runtime became Vyrn (M4)
//! and one emitter was left standing. Nobody had counted the driver since.
//! `own.rs` was censused by a reader against every part, `movecheck.rs` by
//! `tests/frontend_census.rs`, `checker.rs` by `tests/checker_census.rs` and
//! `direct.rs` by `tests/emitter_census.rs`; this is the same measurement for
//! the CLI.
//!
//! # The method, which is `checker_census.rs`'s
//!
//! A section is one item — a `fn`, a `struct`, an `enum`, an `impl`, a `mod`, a
//! `const` — together with every item after it up to the next section's anchor.
//! The span runs from the anchor's own doc comment to the line before the next
//! anchor's, so every line of the file belongs to exactly one section and the
//! counts add up to the file. The test computes the spans; the table below
//! records the anchor, the kind and a reader, so an edit to the file moves the
//! numbers and the classification stays where a reader put it.
//!
//! # Four files, not one
//!
//! The other four censuses each tile ONE file, because each measures one pass.
//! This one measures a crate: `main.rs` is the driver, `wasmrun.rs` is the WASI
//! host the compiled route runs in, `remote.rs` is the remote-module resolver
//! and `lib.rs` is the library face `vyrn-frontend`'s tests reach the host
//! through. A rule that leaves `main.rs` for `wasmrun.rs` has not left the CLI,
//! and a census of `main.rs` alone would record that move as a deletion.
//!
//! # The command tile, and why it is a second tiling
//!
//! "A command's own path" is 5,101 lines, two thirds of the crate's 7,930
//! non-test lines, and one heading that size says nothing about where to cut. So
//! [`Kind::Cmd`] carries the command it belongs to and the same lines tile a
//! second time, one entry per command. A section two commands share is one entry
//! naming both, because splitting it would be a guess. The ranking that follows
//! is lines per rule: an entry is worth cutting when its lines state something
//! another entry, or another crate, already states.
//!
//! # The second column, and why
//!
//! The checker's census counts refusals beside lines and the emitter's counts
//! `Instruction::` sites, because that is what each pass produces. What a driver
//! produces is an exit code and a sentence on stderr, so the column beside lines
//! is the number of `eprintln!` sites a section holds: every place the CLI says
//! something in its own words. That is the reading kind [`Kind::Restated`] needs
//! — a rule stated twice is a sentence written twice — and it separates a
//! section that got shorter from a section whose rule left.

use std::path::{Path, PathBuf};

/// What a section of the CLI is.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Kind {
    /// A command's own path: parsing its arguments, calling the frontend, the
    /// lowering or the emitter, printing what came back, and choosing the exit
    /// code. Nothing replaces this — it is what a driver is.
    ///
    /// The payload names the command, so this kind tiles a second time: every
    /// line of it belongs to exactly one entry of the per-command table. Two
    /// commands that share a path share an entry (`"serve, dev"`), which is the
    /// only honest way to tile a section both of them reach. `"(global)"` is the
    /// flags read before the subcommand and `"(dispatch)"` is `real_main`, which
    /// is no one command's — it holds the `check`, `run` and `emit-*` arms
    /// inline, so those four commands have no section of their own.
    Cmd(&'static str),
    /// A rule the CLI states that `vyrn-frontend`, `vyrn-lower` or the emitter
    /// also states: a re-check, a second wording of a refusal, or a second walk
    /// over the program. Each names the other site. These are the deletion
    /// candidates.
    Restated,
    /// A path only a deleted route reached — the interpreter's, the text-IR
    /// route's, `emit-ir`, the profiler's per-function rows, the three-engine
    /// flags. **This kind is empty**, and the emptiness is the census's first
    /// finding: `cargo check -p vyrn-cli --all-targets` reports no `dead_code`
    /// anywhere under `src/`, and every candidate the brief named is live —
    /// `--native-target` feeds `bench_native` and `build_wasm2c`'s clang,
    /// `emit-wat`/`emit-lowered`/`emit-gen` are three commands `real_main`
    /// dispatches, and `prof.rs` is the build-phase table M4 added, not the
    /// tree-walker's rows M5 deleted. What the deleted routes left behind is
    /// PROSE, not code, and prose is filed under the kind it decorates.
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

/// One section: the exact source line that starts it, its kind, and what it is.
struct Section {
    at: &'static str,
    kind: Kind,
    what: &'static str,
}

const fn sec(at: &'static str, kind: Kind, what: &'static str) -> Section {
    Section { at, kind, what }
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
        sec(
            "mod remote;",
            Shared,
            "the module head: the usage doc every command is described in, the \
             two imports the driver needs beyond the frontend, and `USAGE` — \
             one string, printed by every argument refusal in the file",
        ),
        sec(
            "fn offline(args: &[String]) -> bool {",
            Cmd("(global)"),
            "the global flags `--offline`, `--version` and `--deny-warnings`: \
             read before the subcommand, normalized into the environment so \
             every nested construction sees them, and stripped",
        ),
        sec(
            "enum NativeTarget {",
            Cmd("build, bench"),
            "`--native-target` and `vyrn.json`'s `nativeTarget`: the curated \
             `-march` set, its default, its resolution and the clang flags. LIVE \
             — `bench_native` and `build_wasm2c` are the two native clang \
             invocations, and both read it",
        ),
        sec(
            "fn main() -> ExitCode {",
            Shared,
            "the worker thread with the compiler's own stack reserve, and the \
             build-phase table printed on the way out",
        ),
        sec(
            "fn real_main() -> ExitCode {",
            Cmd("(dispatch)"),
            "the dispatcher: the global flags, then one branch per subcommand, \
             then the four that read the file themselves (`fix`, `check`, `run`, \
             the three `emit-*`)",
        ),
        sec(
            "fn emit_gen(path: &str, source: &str, maps: bool) -> ExitCode {",
            Cmd("emit-gen"),
            "`vyrn emit-gen [--maps]` (RFC-0021, RFC-0073 M1)",
        ),
        sec(
            "fn nearest_manifest(start: &Path) -> Option<Manifest> {",
            Shared,
            "the manifest lookup with the CLI's answer to an unreadable one, its \
             `main`, and the `LoadOptions` every command builds",
        ),
        sec(
            "fn scaffold(name: &str) -> ExitCode {",
            Cmd("new"),
            "`vyrn new <name>`",
        ),
        sec(
            "fn why_cmd(args: &[String]) -> ExitCode {",
            Cmd("why"),
            "`vyrn why --contract <file>` (RFC-0071 M4), and the argument \
             parsing of the other three `why` questions",
        ),
        sec(
            "fn routes_cmd(file: Option<&str>, json: bool) -> ExitCode {",
            Cmd("routes"),
            "`vyrn routes` (RFC-0072 M3): the derived channel out of the \
             mounting generator's `//@route` directives, and the hand-written \
             channel read by running every `mount(..)` as wasm",
        ),
        sec(
            "fn routes_json(",
            Cmd("routes"),
            "`vyrn routes --json` (RFC-0073 M4): the merged wire table, each \
             route carrying the declaration its symbol map names",
        ),
        sec(
            "fn json_str(s: &str) -> String {",
            Shared,
            "the one JSON string literal this driver writes, for \
             `vyrn routes --json` and for every manifest the pretty printer \
             rewrites. It quotes and calls `codec::escape_into`; the table \
             itself is RFC-0018's, stated in `vyrn-frontend/src/codec.rs:432`. \
             It was two copies of that table until RFC-0125 §3 M5",
        ),
        sec(
            "fn why_memory(file: &str) -> ExitCode {",
            Cmd("why"),
            "`vyrn why --memory <file>` (RFC-0087 U1): a printer over \
             `own::Ownership::memory`, which the core writes",
        ),
        sec(
            "fn why_audience(file: &str) -> ExitCode {",
            Cmd("why"),
            "`vyrn why <file>` (RFC-0072 M1): the audience, and the path segment \
             that decided it, from the same `audience` the loader enforces with",
        ),
        sec(
            "fn why_capability(cap: &str, name: &str) -> ExitCode {",
            Cmd("why"),
            "`vyrn why --capability <cap> <artifact>` (RFC-0103 M3): every \
             import chain that pulls a capability into one artifact's closure, \
             where the floor's refusal shows only the shortest",
        ),
        sec(
            "const MAX_CHAINS: usize = 24;",
            Shared,
            "what either import walk answers with. Both walks stated it and had              drifted — one stopped at thirteen modules and the other at twelve              (RFC-0125 §3 M5)",
        ),
        sec(
            "fn chains_from(entry: &str, target: &str, edges: &[(String, String)]) -> Vec<Vec<String>> {",
            Shared,
            "the bounded forward path enumeration the capability report prints.              The backward one is in `project_imports`'s section below; the two              differ in direction, in where they stop and in the order they              answer in, and merging them was measured at +13 lines",
        ),
        sec(
            "fn rel_to(path: &str, base: &str) -> String {",
            Shared,
            "a path relative to a base, for printing",
        ),
        sec(
            "fn project_imports(app_dir: &Path) -> Vec<(String, String)> {",
            Restated,
            "the CLI's own walk over a project's import graph — a second \
             construction of the edge set `vyrn-frontend/src/loader.rs` builds \
             on every load. It resolves with the loader's `resolve_spec` and \
             `audience::generator_inputs`, so the two agree on each edge; what \
             is stated twice is the WALK, and the CLI's exists only because \
             `why` answers about a project that need not load",
        ),
        sec(
            "type ToolRow = (String, String, String, String);",
            Cmd("deps"),
            "the `toolchain:` section of `vyrn deps` (RFC-0102 M3): one row per \
             tool, with the path that would be used, its version, and why that \
             path was chosen",
        ),
        sec(
            "fn deps(name: Option<&str>) -> ExitCode {",
            Cmd("deps"),
            "`vyrn deps [artifact]`: every declared artifact's module graph",
        ),
        sec(
            "fn fmt_cmd(rest: &[String]) -> ExitCode {",
            Cmd("fmt"),
            "`vyrn fmt [file ...] [--check]` (RFC-0017)",
        ),
        sec(
            "const FROM_JSON_SRC: &str = r#\"import { parseJson } from \"std/jsonread\"",
            Cmd("fmt"),
            "`vyrn fmt --from-json` (RFC-0097 M1) — the converter is Vyrn, run \
             as wasm, so there is no second JSON reader and no second VON writer \
             in Rust. The CLI carries bytes and nothing else",
        ),
        sec(
            "fn fmt_project_files() -> Result<Vec<String>, ExitCode> {",
            Cmd("fmt"),
            "the default target set for a bare `vyrn fmt`: the project `main` \
             plus its local imports",
        ),
        sec(
            "struct DocModule {",
            Cmd("doc"),
            "`vyrn doc` (RFC-0065) and the module-set discovery its four \
             argument shapes select",
        ),
        sec(
            "fn closure_doc_modules(root_file: &str, with_std: bool) -> Result<Vec<DocModule>, ExitCode> {",
            Cmd("doc"),
            "the local-import closure `vyrn doc` documents, and the module names \
             it gives the files in it",
        ),
        sec(
            "fn render_doc_index(modules: &[DocModule]) -> String {",
            Cmd("doc"),
            "the Markdown renderers, the writer that prunes what it did not \
             write, and `--verify`, the drift gate",
        ),
        sec(
            "fn lock_home(root_key: &str) -> (PathBuf, Option<String>) {",
            Shared,
            "the lock: where it lives, the resolver built over it, the CLI's \
             answer to a damaged one, and the save every load's new pins need",
        ),
        sec(
            "fn fix_cmd(path: &str, source: &str) -> ExitCode {",
            Cmd("fix"),
            "`vyrn fix` — it applies the `.copy()` a move diagnostic already \
             names, by reading the menu `movecheck::menu` wrote, and refuses \
             rather than chooses when the line cannot say which occurrence",
        ),
        sec(
            "fn synth_fn(",
            Shared,
            "the one `ast::Function` this driver builds: no parameters, no type              parameters, no doc, in the root module. `routes`, `bench` twice and              `test` each wrote out all fifteen fields until RFC-0125 §3 M5",
        ),
        sec(
            "fn load_program(path: &str, source: &str) -> Result<vyrn_frontend::ast::Program, ExitCode> {",
            Shared,
            "the toolchain's single load site — every command that builds a \
             program arrives here — with the one ownership analysis the command \
             then adopts (RFC-0125 §3 M3) and the one warning printer",
        ),
        sec(
            "fn add(rest: &[String], _offline: bool) -> ExitCode {",
            Cmd("add"),
            "`vyrn add <specifier>`",
        ),
        sec(
            "fn update_tool(name: &str, version: &str, lock: &mut remote::Lock) -> Result<(), String> {",
            Cmd("update"),
            "the pinned-tool half of `vyrn update` (RFC-0102 M1/M4): fetch and \
             pin every platform's artifact, or make this machine hold what the \
             lock already pins and change nothing",
        ),
        sec(
            "fn update(alias: Option<&str>, locked: bool) -> ExitCode {",
            Cmd("update"),
            "`vyrn update [--locked] [alias]`",
        ),
        sec(
            "fn vendor(check: bool) -> ExitCode {",
            Cmd("vendor"),
            "`vyrn vendor [--check]`",
        ),
        sec(
            "fn json_pretty(j: &vyrn_frontend::schema::Json, depth: usize) -> String {",
            Shared,
            "the manifest writer `vyrn add` and `vyrn update` rewrite \
             `vyrn.json` through. Its own escape was `codec::escape_into`'s rule \
             with two more short forms — the same sentence said longer — and \
             RFC-0125 §3 M5 deleted it for `json_str`",
        ),
        sec(
            "fn test_cmd(path: &str, rest: &[String]) -> ExitCode {",
            Cmd("test"),
            "`vyrn test [--name <substring>]` (RFC-0015)",
        ),
        sec(
            "fn bench_cmd(path: &str, rest: &[String]) -> ExitCode {",
            Cmd("bench"),
            "`vyrn bench` (RFC-0055 + RFC-0063) and its four modes",
        ),
        sec(
            "fn bench_native(",
            Cmd("bench"),
            "the default bench mode: lift each selected body to a function, \
             synthesize the harness `main` over `std/bench`, build native, run \
             it. The harness is Vyrn; the CLI drives clang",
        ),
        sec(
            "fn bench_ungate_list(text: &str) -> Vec<String> {",
            Cmd("bench"),
            "the readers of a `--json` report and a baseline",
        ),
        sec(
            "fn bench_compare(",
            Cmd("bench"),
            "`vyrn bench --compare` (RFC-0063 §2): the verdicts, the host-scale \
             correction and its quorum",
        ),
        sec(
            "pub struct ServeRequest {",
            Shared,
            "the four shapes the HTTP host and the engine speak in: a request, a \
             response, what the host asks and what the engine answers",
        ),
        sec(
            "const SERVE_SHIM: &str = r#\"",
            Cmd("serve, dev"),
            "the Vyrn `vyrn serve` appends to a served program before it loads \
             it, so the checker, the move checker and the release planner judge \
             every line of it as they judge the program",
        ),
        sec(
            "fn serve_rewrite(program: &mut vyrn_frontend::ast::Program) {",
            Cmd("serve, dev"),
            "the two name substitutions that turn a loaded program into the \
             served one, and the one resident instance that answers a request",
        ),
        sec(
            "fn serve_cmd(path: &str, rest: &[String]) -> ExitCode {",
            Cmd("serve"),
            "`vyrn serve [--port N] [--workers N]` (RFC-0016)",
        ),
        sec(
            "fn has_served_handle(program: &vyrn_frontend::ast::Program) -> bool {",
            Cmd("serve, dev"),
            "the one serving loop and the one signature a served root must              have. `vyrn serve` and `vyrn dev` each wrote both out until              RFC-0125 §3 M5; what differs between them is the greeting and the              static tree in front of the doors, and both are arguments now",
        ),
        sec(
            "fn serve_pool_wasm<W, A>(",
            Cmd("serve, dev"),
            "`--workers N` (RFC-0025): N resident instances over one Cranelift \
             compile, and the isolation gate that refuses the flag when `handle` \
             touches module state — the analysis is the frontend's, the refusal \
             is this",
        ),
        sec(
            "fn dev_cmd(rest: &[String]) -> ExitCode {",
            Cmd("dev"),
            "`vyrn dev [--port N]` (RFC-0019): build the client to wasm, serve \
             the server root with static assets in front",
        ),
        sec(
            "struct DevAssets {",
            Cmd("dev"),
            "static asset resolution and its traversal refusals: no `..` on \
             either separator, no absolute or drive-letter target, and the join \
             confirmed by canonicalization",
        ),
        sec(
            "fn request_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {",
            Shared,
            "the browser-origin gate every served request passes before Vyrn's \
             `handle`, and the body limit the loopback bind does not give",
        ),
        sec(
            "fn serve_one(",
            Shared,
            "one connection, parsed and answered: the hand-rolled HTTP/1.1 \
             reader on `std::net`, no crates",
        ),
        sec(
            "fn reason_phrase(status: i64) -> &'static str {",
            Shared,
            "the status-code to reason-phrase table",
        ),
        sec(
            "fn pump_stream(",
            Shared,
            "the two streaming adapters (RFC-0074 M3a/M3b) and the one frame \
             pull they share — the disconnect signal is the write, on both",
        ),
        sec(
            "enum WsIn {",
            Shared,
            "the WebSocket half: the non-blocking inbound drain, the frame \
             writer, and the fragmentation rule",
        ),
        sec(
            "fn write_response(stream: &mut std::net::TcpStream, status: i64, content_type: &str, body: &[u8]) {",
            Shared,
            "the three response writers: GET, HEAD, and the one that carries a \
             `Vary` field and the response's own headers",
        ),
        sec(
            "fn run_wasm(",
            Cmd("run"),
            "`vyrn run`: the program compiled by the one emitter and run in the \
             embedded wasmtime, with the arguments, streams and exit code, and \
             the profile of a compiled run",
        ),
        sec(
            "struct Body {",
            Cmd("test, bench"),
            "`vyrn test` and `vyrn bench --check`: one module, one instance, one \
             RFC-0012 door per body, the store left open behind `_start`",
        ),
        sec(
            "fn build(path: &str, rest: &[String]) -> ExitCode {",
            Cmd("build"),
            "`vyrn build [-o out] [--target wasm]`",
        ),
        sec(
            "fn build_wasm2c(",
            Cmd("build"),
            "the native route (RFC-0125 §2.5): the same wasm `--target wasm` \
             writes, through wasm2c and clang at the native target's flags",
        ),
        sec("mod tests {", Tests, "the driver's own unit tests"),
    ]
}

/// `wasmrun.rs`, in file order.
fn wasmrun_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec(
            "pub struct Outcome {",
            Host,
            "the module head, and what one run produced",
        ),
        sec(
            "pub struct Run {",
            Host,
            "one run's inputs beyond the module",
        ),
        sec(
            "pub struct Meter {",
            Host,
            "what a metered run measured: wall time per phase the host owns, and \
             one operation count. There is no per-function row and there cannot \
             be one",
        ),
        sec(
            "const SUCCESS: i32 = 0;",
            Host,
            "the WASI preview1 constant tables, from the witx: errnos, \
             `path_open` bits, rights, the preopen fd, filetypes",
        ),
        sec(
            "struct Exit(i32);",
            Host,
            "`proc_exit`'s argument, carried out of the guest as an error so the \
             stack unwinds the way a trap's does",
        ),
        sec(
            "struct Host {",
            Host,
            "the host state one store carries, and the generator imports served \
             by the crate that emits the generators they belong to",
        ),
        sec(
            "fn engine(metered: bool) -> &'static Engine {",
            Host,
            "one Cranelift engine for the process, and a second for metered runs",
        ),
        sec(
            "pub fn run(bytes: &[u8], run: Run) -> Result<Outcome, String> {",
            Host,
            "compile and run a WASI command — the entry `vyrn run`, `vyrn \
             routes` and `vyrn fmt --from-json` reach the guest through",
        ),
        sec(
            "fn open(",
            Host,
            "everything a run does before `_start`: compile, link this host, \
             instantiate",
        ),
        sec(
            "pub struct Compiled {",
            Host,
            "one module translated once and instantiable many times — what \
             `--workers N` costs an instantiation per worker rather than a \
             translation",
        ),
        sec(
            "pub struct Resident {",
            Host,
            "one instance that outlives `_start`, and every RFC-0012 door \
             `vyrn serve`, `vyrn test` and `vyrn bench --check` call through it",
        ),
        sec(
            "fn first_line(s: &str) -> &str {",
            Host,
            "the guest-memory primitives every import call reads and writes \
             through, and the capability rule for a guest path under the preopen",
        ),
        sec(
            "fn link_wasi(linker: &mut Linker<Host>) -> wasmtime::Result<()> {",
            Host,
            "the `wasi_snapshot_preview1` import table, one closure per import — \
             the largest single section of the crate, and format rather than \
             language",
        ),
        sec("mod tests {", Tests, "the host's own unit tests"),
    ]
}

/// `remote.rs`, in file order.
fn remote_sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec(
            "pub use vyrn_frontend::hash::sha256_hex;",
            Shared,
            "the module head and the re-exports: the lock, the caches and the \
             content-addressed blob read live in `vyrn_frontend::manifest`, \
             because the LSP reads them too and a second reader of a pin is a \
             second answer about what is pinned",
        ),
        sec(
            "pub fn resolve_to_url(spec: &str) -> Result<String, String> {",
            Shared,
            "a remote specifier to an immutable URL, pinning a floating github \
             ref through `git ls-remote` and refusing an ambiguous one",
        ),
        sec(
            "pub fn upstream_changed(spec: &str, url: &str, got: &str, pinned: &str, remedy: &str) -> String {",
            Shared,
            "the refusal for bytes that are not the pinned bytes, spelled once \
             for both readers, and the fetch that only ever hands curl an \
             `https://` URL",
        ),
        sec(
            "pub struct RemoteResolver {",
            Shared,
            "the CLI's resolver: local paths from disk, remote keys through lock \
             then vendor then cache then network",
        ),
        sec("mod tests {", Tests, "the resolver's own unit tests"),
    ]
}

/// `lib.rs`.
fn lib_sections() -> Vec<Section> {
    vec![sec(
        "pub mod wasmrun;",
        Kind::Host,
        "the library face: `vyrn-frontend`'s loader and decoder tests run their \
         linked programs through the driver's own host rather than write a \
         second one",
    )]
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

/// Where a section's doc comment starts: the run of comment and attribute lines
/// straight above the anchor.
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

/// The sections tile every file: each line is in one, in file order.
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

/// The line count and the stderr-sentence count per kind, as RFC-0125 §3 M5
/// records them. The prose quotes these numbers, so they are asserted rather
/// than described: a change to the CLI moves one, and the RFC's table moves with
/// it.
#[test]
fn the_structural_census_is_what_the_rfc_records() {
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
        ("a command's own path", 5101, 167),
        ("a rule another pass also states", 165, 0),
        ("a path only a deleted route reached", 0, 0),
        ("machinery with a copy elsewhere", 0, 0),
        ("the WASI host and the wasmtime embedding", 1133, 0),
        ("shared machinery", 1529, 19),
        ("tests", 856, 2),
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

/// What each command's own path costs, as RFC-0125 §3 M5 records it. The kind
/// table above says the driver spends most of itself on commands; this says
/// which ones, so a deletion is ranked before it is made rather than after.
#[test]
fn the_per_command_census_is_what_the_rfc_records() {
    let want = vec![
        ("bench", 651, 17),
        ("why", 566, 19),
        ("serve, dev", 501, 7),
        ("doc", 391, 13),
        ("routes", 357, 4),
        ("fmt", 289, 13),
        ("build", 277, 17),
        ("(dispatch)", 269, 12),
        ("deps", 263, 5),
        ("fix", 247, 1),
        ("dev", 238, 18),
        ("update", 226, 7),
        ("build, bench", 163, 0),
        ("test, bench", 145, 2),
        ("serve", 97, 8),
        ("run", 79, 3),
        ("emit-gen", 77, 3),
        ("add", 73, 6),
        ("vendor", 65, 6),
        ("test", 51, 2),
        ("new", 41, 4),
        ("(global)", 35, 0),
    ];
    assert_eq!(per_command(), want, "the per-command census has moved");
    let total: usize = per_command().iter().map(|(_, n, _)| n).sum();
    assert_eq!(
        total, 5101,
        "the per-command tile does not add up to its kind"
    );
}

/// The per-command table for RFC-0125 §3 M5:
/// `cargo test -p vyrn-cli --test cli_census -- --ignored --nocapture
/// the_per_command_census_as_a_table`.
#[test]
#[ignore]
fn the_per_command_census_as_a_table() {
    println!("| command | lines | stderr |");
    println!("|---|---|---|");
    for (name, lines, msgs) in per_command() {
        println!("| `{name}` | {lines} | {msgs} |");
    }
}

/// The table for RFC-0125 §3 M5, printed from the sections above:
/// `cargo test -p vyrn-cli --test cli_census -- --ignored --nocapture
/// the_structural_census_as_a_table`.
#[test]
#[ignore]
fn the_structural_census_as_a_table() {
    println!("| file | section | lines | stderr | kind | command | what it is |");
    println!("|---|---|---|---|---|---|---|");
    for (rel, secs) in files() {
        let lines = source(rel);
        let short = rel.rsplit('/').next().unwrap_or(rel);
        for (i, a, b) in spans(rel, &lines, &secs) {
            let name = secs[i]
                .at
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(64)
                .collect::<String>()
                .trim_end_matches(" {")
                .trim_end_matches('(')
                .to_string();
            println!(
                "| `{}` | `{}` | {} | {} | {} | {} | {} |",
                short,
                name,
                b - a + 1,
                messages(&lines, a, b),
                secs[i].kind.label(),
                match secs[i].kind {
                    Kind::Cmd(c) => c,
                    _ => "—",
                },
                secs[i].what
            );
        }
    }
}
