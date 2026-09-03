//! RFC-0103 M2 — the floor: what an artifact's target can reach.
//!
//! An artifact is an entry point and a target ([`crate::artifacts`]), and a
//! target is a CAPABILITY SET — a fact about where the code runs. A browser page
//! has no filesystem. No edit to `vyrn.json` can give it one, which is what
//! separates this rule from RFC-0072's audience fence: the fence is a declared
//! boundary and can be relabelled by whoever declares it; the floor cannot.
//!
//! ```text
//! requirement(closure(entry)) ⊆ capabilities(target)
//! ```
//!
//! **Presence, not reachability.** A module REQUIRES `fs` because a call to
//! `readFile` is written in it, not because that call runs. The check must not
//! depend on control flow, and M0's census found the shipped runtime already
//! behaves this way: under wasmtime an `extern` import fails at INSTANTIATION,
//! so a program that never reaches the call still cannot start.
//!
//! **The vocabulary is four capabilities** — [`Capability::Fs`],
//! [`Capability::Stdin`], [`Capability::Args`], [`Capability::Extern`]. The
//! universal reaches of M0's table (stdout/stderr, the clock, entropy, threads)
//! are not tracked at all: every target answers yes, so a row for them would say
//! nothing. `serveStream` is not tracked either, and for the opposite reason —
//! M0's finding 5 — no compiled target has it, so it keeps the runtime trap it
//! already has rather than becoming a per-target row that is `no` three times.
//! `listDir` was in the same case until RFC-0125 M5 lowered it over
//! `fd_readdir`: a WASI host lists, a page answers `BADF`, so it is an `fs`
//! carrier like `readFile` (RFC-0125 §3 M6 finding 6).

use crate::artifacts::{Artifact, ArtifactMap, Target};
use crate::ast::{LogSink, Program};
use crate::diagnostics::Diagnostic;

/// A way out of the program that some target lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    /// The filesystem: `readFile`, `readFileBytes`, `writeFile`,
    /// `writeFileBytes`, `renameFile`, `fsyncFile`, `listDir`, `listDirKinds`,
    /// and the `logging { sink: file(..) }` declaration.
    Fs,
    /// Standard input: `readLine`.
    Stdin,
    /// The command line: `args`.
    Args,
    /// A host function imported by name: an `extern fn` DECLARATION.
    Extern,
}

/// The whole vocabulary, for the diagnostic that has to name it back. One list,
/// so a refusal cannot drift from [`Capability::parse`].
pub const CAPABILITIES: &str = "fs, stdin, args, extern";

impl Capability {
    /// How the manifest-facing vocabulary spells it.
    pub fn name(self) -> &'static str {
        match self {
            Capability::Fs => "fs",
            Capability::Stdin => "stdin",
            Capability::Args => "args",
            Capability::Extern => "extern",
        }
    }

    /// The capability `s` names, or `None`. The inverse of [`Capability::name`],
    /// and the reading `vyrn why --capability` does of its argument.
    pub fn parse(s: &str) -> Option<Capability> {
        Some(match s {
            "fs" => Capability::Fs,
            "stdin" => Capability::Stdin,
            "args" => Capability::Args,
            "extern" => Capability::Extern,
            _ => return None,
        })
    }

    /// What a module that carries it DOES, for the first line of the diagnostic.
    ///
    /// Per capability rather than per carrier, and deliberately not §3's
    /// illustrative "it reads files": a `writeFile` is not a reader, and one
    /// phrase that covers both beats two that are each half right.
    fn does(self) -> &'static str {
        match self {
            Capability::Fs => "it reaches the filesystem",
            Capability::Stdin => "it reads stdin",
            Capability::Args => "it reads the command line",
            Capability::Extern => "it imports a host function",
        }
    }

    /// What the target has none of, for the "= …" line — and for
    /// `vyrn why --capability`, which asks the same question at the shell.
    pub fn absence(self) -> &'static str {
        match self {
            Capability::Fs => "no filesystem",
            Capability::Stdin => "no stdin",
            Capability::Args => "no command line",
            Capability::Extern => "no host to import from",
        }
    }
}

/// The capabilities a target HAS. Rust constants, not configuration: this is the
/// floor, and nothing in `vyrn.json` may alter it.
///
/// `wasi` and `browser` are the identical bytes under two hosts (M0's finding
/// 1), and this is where the two differ: a WASI host answers `path_open`,
/// `fd_read` and `args_get`; a page answers `NOENT`, EOF and an empty list, and
/// IS the `vyrn` import namespace an `extern` needs.
pub fn capabilities(target: Target) -> &'static [Capability] {
    match target {
        Target::Native | Target::Wasi => &[Capability::Fs, Capability::Stdin, Capability::Args],
        Target::Browser => &[Capability::Extern],
    }
}

/// One capability a module carries, and what carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Carried {
    pub cap: Capability,
    /// The builtin, the `extern fn`'s name, or the declaration — what the
    /// diagnostic quotes.
    pub carrier: String,
    /// 1-based line of the carrier, or `0` for a declaration the AST keeps no
    /// line for (the `logging` block).
    pub line: usize,
}

/// `(builtin, capability)` — the calls that reach out of the program.
///
/// `fsyncFile` is in the `fs` row rather than a row of its own. M0 split it out
/// because it behaves differently (the direct backend has no lowering for it, so
/// a wasm build is refused outright), but that is a missing lowering — a filed
/// regression — and not a second capability. The floor names the capability;
/// the backend keeps its own refusal. `listDir` and `listDirKinds` are the same
/// case on the native target (`NATIVE_UNSUPPORTED`), and on a page they degrade
/// to the canonical `Err` the floor exists to refuse.
pub const CALLS: &[(&str, Capability)] = &[
    ("readFile", Capability::Fs),
    ("readFileBytes", Capability::Fs),
    ("writeFile", Capability::Fs),
    ("writeFileBytes", Capability::Fs),
    ("renameFile", Capability::Fs),
    ("fsyncFile", Capability::Fs),
    ("listDir", Capability::Fs),
    ("listDirKinds", Capability::Fs),
];

/// The rows a judgment answers — RFC-0125 §3 M6, fourth slice.
///
/// A row here is out of [`CALLS`]: the pass no longer decides it. The module
/// scan below reads both lists, so a moved row is still FOUND by the scan —
/// the carrier and its line are the diagnostic's own words and the sentence is
/// written once. What moved is the VERDICT. When a judgment is installed
/// ([`install_judge`]) the effect judgment of RFC-0125 §2.2 says which module
/// reaches the capability, and [`decide`] drops the rows it does not confirm.
/// When none is installed — the LSP, `VYRN_NO_JUDGE=1` — the pass answers for
/// them exactly as it did.
pub const JUDGED: &[(&str, Capability)] =
    &[("readLine", Capability::Stdin), ("args", Capability::Args)];

/// Every capability `program` carries, in a stable order.
///
/// The `&mut` is the walker's, not this function's: [`crate::project::walk_block`]
/// is the one exhaustive expression walk in the frontend, and reusing it is worth
/// more than a second walk that goes stale the next time the AST grows an arm.
/// Nothing here changes a node.
///
/// What is NOT scanned is as load-bearing as what is: a `gen fn` body runs at
/// GENERATION time against the compiler's filesystem and is never compiled into
/// the artifact (the checker already fences it), and `test` / `bench` blocks are
/// separate `Program` fields that no build walks. A shipped binary contains
/// neither, so neither can make one need a capability.
pub fn carried(program: &mut Program) -> Vec<Carried> {
    fn visit(e: &mut crate::ast::Expr, out: &mut Vec<Carried>) {
        let crate::ast::Expr::Call { name, line, .. } = e else {
            return;
        };
        if let Some((_, cap)) = CALLS.iter().chain(JUDGED).find(|(n, _)| n == name) {
            out.push(Carried {
                cap: *cap,
                carrier: name.clone(),
                line: *line,
            });
        }
    }

    let mut out: Vec<Carried> = Vec::new();

    // An `extern fn` IMPORT is carried by the DECLARATION, not by the call:
    // under wasmtime the module with an unanswered import never instantiates, so
    // a program that never calls it still cannot start (M0's finding 3). An
    // `export extern fn` carries nothing — it has a body and is an ordinary
    // function that is additionally callable from a page.
    //
    // RFC-0043's host-boundary externs carry nothing either, and M0's census
    // missed it: `hostNowMillis` and its two neighbours are not host imports at
    // all — the C runtime shim implements them on every target, which is why
    // `std/time` is in every native server's closure and a clock example is a
    // three-way parity citizen. `extern fn` is two things, and only one of them
    // is a capability.
    for f in &program.functions {
        if f.is_extern && crate::trap::host_boundary_extern(&f.name).is_none() {
            out.push(Carried {
                cap: Capability::Extern,
                carrier: f.name.clone(),
                line: f.line,
            });
        }
    }

    // The one capability carried by a DECLARATION (M0's finding 4). It is here
    // because it is the one `fs` reach that degrades SILENTLY in a page: the
    // line vanishes, nothing is printed, the exit code is 0.
    if let LogSink::File(path) = &program.log_sink {
        out.push(Carried {
            cap: Capability::Fs,
            carrier: format!("logging {{ sink: file(\"{path}\") }}"),
            line: 0,
        });
    }

    for f in &mut program.functions {
        if f.is_gen {
            continue;
        }
        crate::project::walk_block(&mut f.body, &mut |e| visit(e, &mut out));
    }
    for im in &mut program.impls {
        for m in im.methods.iter_mut().chain(im.places.iter_mut()) {
            crate::project::walk_block(&mut m.body, &mut |e| visit(e, &mut out));
        }
    }
    for g in &mut program.globals {
        crate::project::walk_bare(&mut g.init, &mut |e| visit(e, &mut out));
    }
    for t in &mut program.type_decls {
        if let Some(p) = &mut t.predicate {
            crate::project::walk_bare(p, &mut |e| visit(e, &mut out));
        }
    }
    out
}

/// What the floor decides on: `(module key, resolved import targets, what that
/// module carries)` per module a load linked.
///
/// Named because two commands walk it. [`objection`] refuses over it at the end
/// of a load, and `vyrn why --capability` reports over the same triples through
/// [`crate::loader::capability_graph`] — M3 claimed the report could not drift
/// from the check, and M4 found that true of the vocabulary and false of the
/// GRAPH, because the report read the project's files while the check read the
/// load. A generated module is on nobody's disk.
pub type Graph = Vec<(String, Vec<String>, Vec<Carried>)>;

/// The floor's objection, if any, to building `root` as the artifact that
/// declares it.
///
/// `modules` is the load's own [`Graph`] and `root` is the key the load started
/// from.
/// `None` whenever this root is nobody's declared entry point, which is every
/// project that has not opted in and every file inside one that is not an entry.
///
/// The chain is breadth-first from the entry, so the reported path is the
/// SHORTEST one that reaches the offending module: the author never saw hop
/// three, and showing them the longest way round would not help.
pub fn objection(modules: &Graph, root: &str, map: &ArtifactMap) -> Option<Diagnostic> {
    let (artifact, key, c, parent) = locate(modules, root, map)?;
    Some(refusal(artifact, key, c, &parent, map))
}

/// The capability the floor would object to, without writing the diagnostic.
///
/// The loader asks before it refuses (RFC-0125 §3 M6, fourth slice): a row a
/// judgment answers cannot be decided inside the load, because the judgment
/// needs a checked program, so that objection is deferred to [`decide`] and
/// every other one is made where it always was.
pub fn objected(modules: &Graph, root: &str, map: &ArtifactMap) -> Option<Capability> {
    locate(modules, root, map).map(|(_, _, c, _)| c.cap)
}

/// The artifact, the module, the carrier and each module's first parent — what
/// both [`objection`] and [`objected`] read, walked once.
#[allow(clippy::type_complexity)]
fn locate<'a>(
    modules: &'a Graph,
    root: &'a str,
    map: &'a ArtifactMap,
) -> Option<(
    &'a Artifact,
    &'a str,
    &'a Carried,
    std::collections::HashMap<&'a str, &'a str>,
)> {
    let artifact = map.artifact_for(root)?;
    let has = capabilities(artifact.target);

    // Breadth-first from the root, recording each module's first parent.
    let mut parent: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    let mut order: Vec<&str> = vec![root];
    let mut seen: std::collections::HashSet<&str> = [root].into_iter().collect();
    let mut i = 0;
    while i < order.len() {
        let key = order[i];
        i += 1;
        let Some((_, imports, _)) = modules.iter().find(|(k, _, _)| k == key) else {
            continue;
        };
        for t in imports {
            if seen.insert(t) {
                parent.insert(t, key);
                order.push(t);
            }
        }
    }
    // Everything else the load linked — the runtime modules a builtin's desugar
    // injects (RFC-0078) enter with no importer, and they are in the artifact
    // just the same. Their chain is the root and them.
    for (k, _, _) in modules {
        if seen.insert(k) {
            order.push(k);
        }
    }

    for key in order {
        let Some((_, _, carried)) = modules.iter().find(|(k, _, _)| k == key) else {
            continue;
        };
        let Some(c) = carried.iter().find(|c| !has.contains(&c.cap)) else {
            continue;
        };
        return Some((artifact, key, c, parent));
    }
    None
}

/// A judgment that says which of [`JUDGED`]'s capabilities each module of a
/// CHECKED program reaches — RFC-0125 §3 M6, fourth slice.
///
/// The effect judgment (RFC-0125 §2.2) is in `vyrn-lower`, which depends on
/// this crate and cannot be named from it, so the shape is the placer's
/// ([`crate::own::Placer`]): a function pointer the CLI installs at start-up.
/// The module key is the load's, `""` for the root.
pub type Judge = fn(&Program) -> Vec<(String, Capability)>;

static JUDGE: std::sync::OnceLock<Judge> = std::sync::OnceLock::new();

/// Install the judgment. The first installation wins; a second is ignored.
pub fn install_judge(f: Judge) {
    let _ = JUDGE.set(f);
}

/// The installed judgment, unless `VYRN_NO_JUDGE=1` stood it aside — the knob
/// that puts every row back in the pass for a bisect.
fn judge() -> Option<Judge> {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let off = *OFF.get_or_init(|| std::env::var("VYRN_NO_JUDGE").is_ok_and(|v| v == "1"));
    if off {
        return None;
    }
    JUDGE.get().copied()
}

/// Whether a judgment is installed and answers `cap`.
pub fn is_judged(cap: Capability) -> bool {
    judge().is_some() && JUDGED.iter().any(|(_, c)| *c == cap)
}

/// A floor decision the load could not make, held until the program is checked.
struct Pending {
    graph: Graph,
    root: String,
    map: ArtifactMap,
    origins: crate::origin::OriginMaps,
}

thread_local! {
    static PENDING: std::cell::RefCell<Option<Pending>> = const {
        std::cell::RefCell::new(None)
    };
}

/// Hold this load's floor decision for [`decide`]. Called by the loader in
/// place of the refusal, and only when the objection is on a judged row.
pub fn defer(graph: Graph, root: String, map: ArtifactMap, origins: crate::origin::OriginMaps) {
    PENDING.with(|p| {
        *p.borrow_mut() = Some(Pending {
            graph,
            root,
            map,
            origins,
        })
    });
}

/// Forget a held decision. The loader calls it at the start of every outermost
/// load, so a deferral nobody checked cannot answer for the next program.
pub fn forget() {
    PENDING.with(|p| *p.borrow_mut() = None);
}

/// The floor's objection to a program whose row a judgment answers, made after
/// the check — RFC-0125 §3 M6, fourth slice.
///
/// `None` for every load that decided for itself, which is every load whose
/// first objection is not a judged row.
///
/// The judgment can only CLEAR a row: a module keeps its carrier when the
/// judgment confirms the module reaches the capability, and loses it when no
/// instance of that module does. That is presence giving way to reachability
/// (RFC-0125 §3 M6 finding 8) for these rows and for nothing else, and the
/// words of the refusal are still the scan's — the carrier it found and the
/// line it found it on.
pub fn decide(program: &Program) -> Option<Diagnostic> {
    let mut p = PENDING.with(|p| p.borrow_mut().take())?;
    let judge = judge()?;
    let reached = judge(program);
    for (key, _, carried) in &mut p.graph {
        carried.retain(|c| {
            !JUDGED.iter().any(|(_, jc)| *jc == c.cap)
                || reached
                    .iter()
                    .any(|(m, rc)| *rc == c.cap && (m == key || (m.is_empty() && *key == p.root)))
        });
    }
    let mut d = objection(&p.graph, &p.root, &p.map)?;
    if d.file.as_deref() == Some(p.root.as_str()) {
        d.file = None;
    }
    if !p.origins.is_empty() {
        p.origins.remap(&mut d);
    }
    Some(d)
}

/// The diagnostic RFC-0103 §3 specifies: what was refused, the chain that
/// reaches it, why, and — for the one crossing that has an answer today — what
/// to write instead.
fn refusal(
    artifact: &Artifact,
    module: &str,
    c: &Carried,
    parent: &std::collections::HashMap<&str, &str>,
    map: &ArtifactMap,
) -> Diagnostic {
    // The chain, entry first. A module the loader injected has no parent, so its
    // chain is the entry and it.
    let mut chain: Vec<&str> = vec![module];
    while let Some(p) = parent.get(chain[0]) {
        chain.insert(0, p);
    }
    if chain[0] != artifact.entry
        && !crate::audience::same_path(chain[0], &artifact.entry, &map.base)
    {
        chain.insert(0, &artifact.entry);
    }
    let shown: Vec<String> = chain.iter().map(|k| map.display_path(k)).collect();

    let mut note = format!(
        "{}\n   = `{}` needs `{}`; target `{}` has {}",
        shown.join(" → "),
        c.carrier,
        c.cap.name(),
        artifact.target,
        c.cap.absence()
    );
    // The one remedy that exists today, and M3 made it the only spelling of one:
    // the fence quotes the same [`crossing`], so no diagnostic in the tree names
    // a path the project does not contain.
    if c.cap == Capability::Fs && artifact.target == Target::Browser {
        let importer = chain[chain.len().saturating_sub(2)];
        note.push_str(&format!(
            "\n   = call it through the wire instead: {}",
            crossing(importer, module)
        ));
    }
    let mut d = Diagnostic::error(
        c.line,
        0,
        "floor",
        format!(
            "artifact `{}` ({}) cannot include `{}`: {}",
            artifact.name,
            artifact.target,
            map.display_path(module),
            c.cap.does()
        ),
    );
    d.file = Some(module.to_string());
    d.note = Some(note);
    d
}

/// The concrete crossing: the call `importer` writes to reach `module` through
/// the wire instead of importing it.
///
/// One function because there is one rule. The floor's diagnostic and
/// RFC-0072's fence both end with this line, and the fence used to end with a
/// FIXED path (`client("./server/api")`) that named a module most projects do
/// not have — the remedy pointed at a file the reader could not open. Every
/// remedy in the tree now spells a module the project contains, because it is
/// spelled from the module that actually imports it.
pub fn crossing(importer: &str, module: &str) -> String {
    format!("connect(\"{}\")", spec_from(importer, module))
}

/// How `importer` would spell an import of `module`: a relative specifier, no
/// extension.
///
/// Counted from the IMPORTER — the module that would write the call — and from
/// nothing else. It used to require the two keys to share a FIRST segment and to
/// fall back to the module key as spelled otherwise, and a module key is as
/// relative as the path the CLI was handed ([`crate::audience::relative_to`]
/// says so): `cd examples/leak && vyrn check client/boot.vyrn` advised
/// `connect("server/db")` to a reader of `shared/format.vyrn`, which names
/// `shared/server/db` — a module the project does not have — while the same
/// check one directory up advised the correct `connect("../server/db")`
/// (RFC-0103 M4 finding 4). Two keys from one load are relative to one working
/// directory, so counting `..` between them needs no shared prefix and the
/// answer no longer depends on where `vyrn` was invoked.
///
/// The fallback is now only for a pair that cannot be counted between: a
/// generated banner, a remote key or a `std/` module is a KEY rather than a
/// path, and an absolute path beside a relative one — or beside another
/// absolute path on a different Windows drive — shares no root.
fn spec_from(importer: &str, module: &str) -> String {
    let strip = |s: &str| s.trim_end_matches(".vyrn").to_string();
    let (from, to): (Vec<&str>, Vec<&str>) =
        (importer.split('/').collect(), module.split('/').collect());
    let not_a_path = |s: &str| {
        s.starts_with("generated by ") || crate::loader::is_remote(s) || s.starts_with("std/")
    };
    let rooted = crate::audience::is_absolute(importer);
    if not_a_path(importer)
        || not_a_path(module)
        || rooted != crate::audience::is_absolute(module)
        || (rooted && from[0] != to[0])
    {
        return strip(module);
    }
    let shared = from
        .iter()
        .zip(&to)
        .take_while(|(a, b)| a == b)
        .count()
        .min(from.len() - 1);
    let up = from.len() - 1 - shared;
    let mut out = String::new();
    for _ in 0..up {
        out.push_str("../");
    }
    if up == 0 {
        out.push_str("./");
    }
    out.push_str(&to[shared..].join("/"));
    strip(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(src: &str) -> Program {
        crate::parser::parse(crate::lexer::lex(src).expect("lexes")).expect("parses")
    }

    fn caps(src: &str) -> Vec<(Capability, String)> {
        carried(&mut program(src))
            .into_iter()
            .map(|c| (c.cap, c.carrier))
            .collect()
    }

    #[test]
    fn every_carrier_in_the_vocabulary() {
        assert_eq!(
            caps("fn f() -> Int64 {\n    match readFile(\"a\") { Ok(s) => 0, Err(e) => 1 }\n}"),
            vec![(Capability::Fs, "readFile".into())]
        );
        for (src, want) in [
            ("writeFile(\"a\", \"b\")", Capability::Fs),
            ("readFileBytes(\"a\")", Capability::Fs),
            ("renameFile(\"a\", \"b\")", Capability::Fs),
            ("fsyncFile(\"a\")", Capability::Fs),
            ("listDir(\"a\")", Capability::Fs),
            ("listDirKinds(\"a\")", Capability::Fs),
            ("readLine()", Capability::Stdin),
            ("args()", Capability::Args),
        ] {
            let src = format!("fn f() -> Int64 {{\n    let x = {src}\n    return 0\n}}");
            assert_eq!(caps(&src).first().map(|c| c.0), Some(want), "{src}");
        }
    }

    /// M0's finding 4: the one capability carried by a DECLARATION, and the one
    /// `fs` reach that degrades silently in a page.
    #[test]
    fn the_logging_file_sink_is_a_carrier() {
        let c = caps("logging { sink: file(\"app.log\") }\nfn main() -> Int64 {\n    return 0\n}");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].0, Capability::Fs);
        assert!(c[0].1.contains("app.log"), "{:?}", c[0].1);
        // A sink that is not a file reaches nothing.
        assert!(caps("logging { sink: stdout }\nfn main() -> Int64 {\n    return 0\n}").is_empty());
    }

    /// The DECLARATION carries `extern`, not the call: an unanswered import
    /// stops instantiation before any line runs. An `export extern fn` has a
    /// body and carries nothing.
    #[test]
    fn an_extern_import_is_carried_by_its_declaration() {
        assert_eq!(
            caps("extern fn jsAdd(a: Int64, b: Int64) -> Int64\nfn main() -> Int64 {\n    return 0\n}"),
            vec![(Capability::Extern, "jsAdd".into())]
        );
        assert!(caps(
            "export extern fn twice(a: Int64) -> Int64 {\n    return a + a\n}\n\
             fn main() -> Int64 {\n    return 0\n}"
        )
        .is_empty());
    }

    /// A `gen fn` runs at generation time against the COMPILER's filesystem and
    /// is never compiled into the artifact; `test` blocks are never built at all.
    #[test]
    fn generation_and_test_bodies_are_not_in_the_artifact() {
        assert!(caps(
            "gen fn mod(p: String) -> String {\n    match readFile(p) { Ok(s) => s, Err(e) => e }\n}"
        )
        .is_empty());
        assert!(caps(
            "fn main() -> Int64 {\n    return 0\n}\n\
             test \"reads\" {\n    let x = readLine()\n}"
        )
        .is_empty());
    }

    fn map(target: Target) -> ArtifactMap {
        ArtifactMap {
            list: vec![Artifact {
                name: "app".into(),
                entry: "/p/client/boot.vyrn".into(),
                target,
            }],
            base: "/p".into(),
            realpath: None,
        }
    }

    fn graph(
        caps: &[(&str, &[&str], &[(Capability, &str)])],
    ) -> Vec<(String, Vec<String>, Vec<Carried>)> {
        caps.iter()
            .map(|(k, imports, carried)| {
                (
                    k.to_string(),
                    imports.iter().map(|s| s.to_string()).collect(),
                    carried
                        .iter()
                        .map(|(cap, carrier)| Carried {
                            cap: *cap,
                            carrier: carrier.to_string(),
                            line: 7,
                        })
                        .collect(),
                )
            })
            .collect()
    }

    /// The union over the import closure, and the chain the author never saw.
    #[test]
    fn the_closure_is_the_requirement_and_the_chain_is_the_diagnostic() {
        let g = graph(&[
            ("/p/client/boot.vyrn", &["/p/shared/format.vyrn"], &[]),
            ("/p/shared/format.vyrn", &["/p/server/db.vyrn"], &[]),
            ("/p/server/db.vyrn", &[], &[(Capability::Fs, "readFile")]),
        ]);
        let d = objection(&g, "/p/client/boot.vyrn", &map(Target::Browser)).expect("refused");
        assert_eq!(
            d.message,
            "artifact `app` (browser) cannot include `server/db.vyrn`: it reaches the filesystem"
        );
        let note = d.note.unwrap();
        assert!(
            note.starts_with("client/boot.vyrn → shared/format.vyrn → server/db.vyrn"),
            "{note}"
        );
        assert!(
            note.contains("`readFile` needs `fs`; target `browser` has no filesystem"),
            "{note}"
        );
        assert!(note.contains("connect(\"../server/db\")"), "{note}");
        assert_eq!(d.file.as_deref(), Some("/p/server/db.vyrn"));
        assert_eq!(d.line, 7);

        // The same tree under a target that HAS a filesystem is fine.
        assert!(objection(&g, "/p/client/boot.vyrn", &map(Target::Native)).is_none());
    }

    /// Per target, per capability — the subset test, stated as a table.
    #[test]
    fn each_target_refuses_exactly_what_it_lacks() {
        for (target, refused) in [
            (Target::Native, vec![Capability::Extern]),
            (Target::Wasi, vec![Capability::Extern]),
            (
                Target::Browser,
                vec![Capability::Fs, Capability::Stdin, Capability::Args],
            ),
        ] {
            for cap in [
                Capability::Fs,
                Capability::Stdin,
                Capability::Args,
                Capability::Extern,
            ] {
                let g = graph(&[("/p/client/boot.vyrn", &[], &[(cap, "x")])]);
                let got = objection(&g, "/p/client/boot.vyrn", &map(target)).is_some();
                assert_eq!(got, refused.contains(&cap), "{target} / {}", cap.name());
            }
        }
    }

    /// M4's finding 4: the remedy is counted from the IMPORTER and from nothing
    /// else. A module key is as relative as the path the CLI was handed, so the
    /// same edge arrives here spelled three ways — and used to get two answers,
    /// the wrong one being the invocation a developer is most likely to make.
    #[test]
    fn the_crossing_is_counted_from_the_importer_whatever_the_cwd() {
        for (importer, module) in [
            // `cd examples/leak && vyrn check client/boot.vyrn` — no shared
            // first segment, and the old fallback advised `connect("server/db")`
            // to a reader of `shared/`, naming `shared/server/db`.
            ("shared/format.vyrn", "server/db.vyrn"),
            ("leak/shared/format.vyrn", "leak/server/db.vyrn"),
            ("/p/leak/shared/format.vyrn", "/p/leak/server/db.vyrn"),
            ("N:/e/leak/shared/format.vyrn", "N:/e/leak/server/db.vyrn"),
        ] {
            assert_eq!(
                crossing(importer, module),
                "connect(\"../server/db\")",
                "{importer} -> {module}"
            );
        }
        // An importer at the project root has no `..` to climb, and a specifier
        // with no `./` names a package rather than a sibling.
        assert_eq!(
            crossing("boot.vyrn", "server/db.vyrn"),
            "connect(\"./server/db\")"
        );
        assert_eq!(
            crossing("client/boot.vyrn", "db.vyrn"),
            "connect(\"../db\")"
        );
    }

    /// The fallback that remains: a pair with nothing to count between. A key
    /// that is not a path, and two roots that are not one root.
    #[test]
    fn a_key_that_is_not_a_path_is_quoted_whole() {
        for module in [
            "github:acme/x@v1/src/a",
            "std/rpc",
            "generated by client(\"./server/api\") at client/boot",
        ] {
            assert_eq!(
                crossing("client/boot.vyrn", &format!("{module}.vyrn")),
                format!("connect(\"{module}\")"),
                "{module}"
            );
        }
        assert_eq!(
            crossing("client/boot.vyrn", "/p/server/db.vyrn"),
            "connect(\"/p/server/db\")"
        );
        assert_eq!(
            crossing("C:/a/boot.vyrn", "D:/b/db.vyrn"),
            "connect(\"D:/b/db\")"
        );
    }

    /// A root no artifact names gets no floor, whatever it carries. That is what
    /// leaves `examples/externdemo.vyrn` — built natively and asserted on by the
    /// parity suite — exactly as it was.
    #[test]
    fn a_root_that_is_no_artifacts_entry_gets_no_floor() {
        let g = graph(&[(
            "/p/examples/externdemo.vyrn",
            &[],
            &[(Capability::Extern, "jsAdd")],
        )]);
        assert!(objection(&g, "/p/examples/externdemo.vyrn", &map(Target::Native)).is_none());
    }
}
