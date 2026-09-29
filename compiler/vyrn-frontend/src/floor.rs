//! The floor: what an artifact's target can reach.
//!
//! An artifact is an entry point and a target ([`crate::artifacts`]), and a
//! target is a capability set, a fact about where the code runs: no
//! `vyrn.json` edit gives a browser page a filesystem. That separates the
//! floor from the audience fence, which is a declared boundary.
//!
//! ```text
//! requirement(closure(entry)) is a subset of capabilities(target)
//! ```
//!
//! The scan here finds every carrier and writes every refusal, quoting the
//! carrier and its line. The effect judgment decides a call: it
//! can clear a carrier no instance reaches, never add one, because the direct
//! backend emits an `extern` import only for a call it reaches. The one
//! carrier no effect holds is the `logging { sink: file(..) }` declaration,
//! which [`carried`] decides alone.
//!
//! The vocabulary is the lattice's rows read through [`Capability::of`].
//! Reaches every target has (stdout, the clock, entropy) are not tracked, and
//! neither is `serveStream`, which no compiled target has and which keeps its
//! runtime trap.

use crate::artifacts::{Artifact, ArtifactMap, Target};
use crate::ast::{LogSink, Program};
use crate::diagnostics::Diagnostic;

/// A way out of the program that some target lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    /// The filesystem: the file builtins, including `listDir`, `listDirKinds`
    /// and `fsyncFile`, and the `logging { sink: file(..) }` declaration.
    /// `fsyncFile` has no direct-backend lowering, but that is the emitter's
    /// refusal, not a second capability.
    Fs,
    /// Standard input: `readLine`.
    Stdin,
    /// The command line: `args`.
    Args,
    /// A host function imported by name: a call to an `extern fn` import.
    Extern,
}

/// The vocabulary, for a diagnostic that names it back; it must match
/// [`Capability::parse`].
pub const CAPABILITIES: &str = "fs, stdin, args, extern";

impl Capability {
    /// The manifest-facing spelling.
    pub fn name(self) -> &'static str {
        match self {
            Capability::Fs => "fs",
            Capability::Stdin => "stdin",
            Capability::Args => "args",
            Capability::Extern => "extern",
        }
    }

    /// Returns the capability `s` names: the inverse of [`Capability::name`], used
    /// by `vyrn why --capability`.
    pub fn parse(s: &str) -> Option<Capability> {
        Some(match s {
            "fs" => Capability::Fs,
            "stdin" => Capability::Stdin,
            "args" => Capability::Args,
            "extern" => Capability::Extern,
            _ => return None,
        })
    }

    /// What a module that carries it does, for the first line of the diagnostic.
    /// One phrase per capability covers both readers and writers.
    fn does(self) -> &'static str {
        match self {
            Capability::Fs => "it reaches the filesystem",
            Capability::Stdin => "it reads stdin",
            Capability::Args => "it reads the command line",
            Capability::Extern => "it imports a host function",
        }
    }

    /// What the target has none of, for the note and for
    /// `vyrn why --capability`.
    pub fn absence(self) -> &'static str {
        match self {
            Capability::Fs => "no filesystem",
            Capability::Stdin => "no stdin",
            Capability::Args => "no command line",
            Capability::Extern => "no host to import from",
        }
    }

    /// Returns the capability an effect needs, or `None`.
    ///
    /// This is the floor's whole vocabulary, read from the lattice rows. `None`
    /// covers an effect every target has (`alloc`, output, the clock, entropy,
    /// module state, `trap`), one no compiled target has (`serve`), and one that
    /// exists only in the generator (`gen-only`).
    pub fn of(e: crate::effects::Effect) -> Option<Capability> {
        use crate::effects::Effect;
        Some(match e {
            Effect::FsRead | Effect::FsWrite | Effect::FsList => Capability::Fs,
            Effect::ReadInput => Capability::Stdin,
            Effect::Args => Capability::Args,
            Effect::Extern => Capability::Extern,
            Effect::Alloc
            | Effect::WriteOutput
            | Effect::Clock
            | Effect::Random
            | Effect::Serve
            | Effect::ModuleState
            | Effect::Trap
            | Effect::GenOnly => return None,
        })
    }
}

/// Returns the capabilities a target has: Rust constants, because nothing in
/// `vyrn.json` may alter the floor.
///
/// `wasi` and `browser` run identical bytes under two hosts. A WASI host
/// answers `path_open`, `fd_read` and `args_get`; a page answers `NOENT`, EOF
/// and an empty list, and is the `vyrn` import namespace an `extern` needs.
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
    /// The builtin, the `extern fn`'s name, or the declaration, as the diagnostic
    /// quotes it.
    pub carrier: String,
    /// 1-based line of the carrier, or `0` for the `logging` declaration, which
    /// the AST keeps no line for.
    pub line: usize,
    /// Whether the effect judgment answers for this carrier: true for a call,
    /// false for a declaration. Only the scan knows which it found.
    pub judged: bool,
}

/// Returns the host imports `program` declares: each `extern fn` that is not a
/// host-boundary extern.
///
/// `hostNowMillis` and its neighbours are no host imports: the runtime
/// implements them on every target. An `export extern fn` has a body and is
/// an ordinary function a page may also call.
pub fn extern_imports(program: &Program) -> std::collections::HashSet<String> {
    program
        .functions
        .iter()
        .filter(|f| f.is_extern && crate::trap::host_boundary_extern(&f.name).is_none())
        .map(|f| f.name.clone())
        .collect()
}

/// Returns the capability a call to `name` carries: the one reading of a call
/// site, shared by [`carried`] and `vyrn_lower::effects::reaches`. A builtin
/// carries its lattice row; a call to a host import carries `extern`.
/// `externs` is [`extern_imports`] of the program the call is in.
pub fn call_carrier(name: &str, externs: &std::collections::HashSet<String>) -> Option<Capability> {
    if let Some(c) = crate::effects::atom(name).and_then(Capability::of) {
        return Some(c);
    }
    externs.contains(name).then_some(Capability::Extern)
}

/// Returns every capability `program` carries, in a stable order.
///
/// The `&mut` belongs to [`crate::project::walk_block`], the frontend's one
/// exhaustive expression walk; nothing here changes a node. A `gen fn` body
/// runs at generation time and never enters the artifact, and `test` and
/// `bench` blocks are never built, so neither is scanned.
pub fn carried(program: &mut Program) -> Vec<Carried> {
    let externs = extern_imports(program);
    let visit = |e: &mut crate::ast::Expr, out: &mut Vec<Carried>| {
        let crate::ast::Expr::Call { name, line, .. } = e else {
            return;
        };
        if let Some(cap) = call_carrier(name, &externs) {
            out.push(Carried {
                cap,
                carrier: name.clone(),
                line: *line,
                judged: true,
            });
        }
    };

    let mut out: Vec<Carried> = Vec::new();

    // The one capability a declaration carries. It degrades silently in a page:
    // the line vanishes and the exit code is 0.
    if let LogSink::File(path) = &program.log_sink {
        out.push(Carried {
            cap: Capability::Fs,
            carrier: format!("logging {{ sink: file(\"{path}\") }}"),
            line: 0,
            judged: false,
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

/// What the floor decides on: `(module key, resolved import targets, carried)`
/// per linked module. [`objection`] refuses over it, and `vyrn why
/// --capability` reports over the same triples through
/// [`crate::loader::capability_graph`], so the report and the check read the
/// same load, generated modules included.
pub type Graph = Vec<(String, Vec<String>, Vec<Carried>)>;

/// Returns the floor's objection to building `root` as the artifact that
/// declares it. `modules` is the load's [`Graph`]; `None` when `root` is no
/// artifact's entry point.
///
/// The chain is breadth-first from the entry, so the diagnostic shows the
/// shortest path to the offending module.
pub fn objection(modules: &Graph, root: &str, map: &ArtifactMap) -> Option<Diagnostic> {
    let (artifact, key, c, parent) = locate(modules, root, map)?;
    Some(refusal(artifact, key, c, &parent, map))
}

/// Returns what the floor would object to, without the diagnostic. The loader
/// asks first: the judgment needs a checked program, so an objection on a
/// judged row is deferred to [`decide`].
pub fn objected(modules: &Graph, root: &str, map: &ArtifactMap) -> Option<Carried> {
    locate(modules, root, map).map(|(_, _, c, _)| c.clone())
}

/// Returns the artifact, the module, the carrier and each module's first
/// parent, for [`objection`] and [`objected`].
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
    // Modules linked with no importer, like the runtime modules a builtin's
    // desugar injects, are in the artifact too.
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

/// A judgment of which capability each module of a checked program reaches.
///
/// The effect judgment lives in `vyrn-lower`, which depends on this crate, so
/// the CLI installs it as a function pointer at start-up, as with
/// [`crate::own::Placer`]. The module key is the load's, `""` for the root.
pub type Judge = fn(&Program) -> Vec<(String, Capability)>;

static JUDGE: std::sync::OnceLock<Judge> = std::sync::OnceLock::new();

/// Installs the judgment. The first installation wins.
pub fn install_judge(f: Judge) {
    let _ = JUDGE.set(f);
}

/// `VYRN_NO_JUDGE=1` is a bisect knob that sets the judgments aside: the floor
/// refuses every scanned carrier, reached or not, inside the load and before
/// every type error. `tests/floor.rs` pins it.
pub fn no_judge() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var("VYRN_NO_JUDGE").is_ok_and(|v| v == "1"))
}

/// The installed judgment, unless [`no_judge`] set it aside.
fn judge() -> Option<Judge> {
    if no_judge() {
        return None;
    }
    JUDGE.get().copied()
}

/// Returns whether an installed judgment answers for this carrier.
///
/// It asks per carrier, not per capability: `fs` has calls the judgment
/// decides and the `logging` declaration, which it does not. Judging by
/// capability would defer the declaration's refusal for no gain.
pub fn is_judged(c: &Carried) -> bool {
    judge().is_some() && c.judged
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

/// Holds this load's floor decision for [`decide`]. The loader calls it in
/// place of the refusal when the objection is on a judged row.
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

/// Forgets a held decision. The loader calls it at the start of every
/// outermost load, so a stale deferral cannot answer for the next program.
pub fn forget() {
    PENDING.with(|p| *p.borrow_mut() = None);
}

/// Runs `f` with this load's held decision set aside, so a nested check (a
/// derived-code generator's program, `gen::derive`) neither answers it nor
/// drops it.
pub fn aside<R>(f: impl FnOnce() -> R) -> R {
    let held = PENDING.with(|p| p.borrow_mut().take());
    let r = f();
    PENDING.with(|p| *p.borrow_mut() = held);
    r
}

/// Returns the floor's objection to a checked program whose objection was
/// deferred; `None` for every load that decided for itself.
///
/// The judgment only clears a row: a module keeps a judged carrier when some
/// instance of it reaches the capability. The refusal still quotes the scan's
/// carrier and line.
pub fn decide(program: &Program) -> Option<Diagnostic> {
    let mut p = PENDING.with(|p| p.borrow_mut().take())?;
    let judge = judge()?;
    let reached = judge(program);
    for (key, _, carried) in &mut p.graph {
        carried.retain(|c| {
            !c.judged
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

/// Builds the floor's diagnostic: what was refused, the chain that reaches
/// it, why, and, for fs in a browser, what to write instead.
fn refusal(
    artifact: &Artifact,
    module: &str,
    c: &Carried,
    parent: &std::collections::HashMap<&str, &str>,
    map: &ArtifactMap,
) -> Diagnostic {
    // The chain, entry first. An injected module has no parent, so its chain is
    // the entry and itself.
    let mut chain: Vec<&str> = vec![module];
    while let Some(p) = parent.get(chain[0]) {
        chain.insert(0, p);
    }
    // Asked through the map's file identity: the entry is stored realpathed and a key is as
    // relative as the CLI's argument, so a raw comparison would name the entry twice (#588).
    if !map
        .artifact_for(chain[0])
        .is_some_and(|a| a.name == artifact.name)
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
    // The fence quotes the same [`crossing`], so every remedy names a module the
    // project contains.
    if c.cap == Capability::Fs && artifact.target == Target::Browser {
        let importer = chain[chain.len().saturating_sub(2)];
        note.push_str(&format!(
            "\n   = call it through the wire instead: {}",
            crossing(importer, module)
        ));
    }
    Diagnostic::error(
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
    )
    .in_file(Some(module.to_string()))
    .with_note(note)
}

/// Returns the call `importer` writes to reach `module` through the wire
/// instead of importing it. The floor and the audience fence both end with it,
/// spelled from the module that imports, so the remedy names a file the
/// project contains.
pub fn crossing(importer: &str, module: &str) -> String {
    format!("connect(\"{}\")", spec_from(importer, module))
}

/// Returns how `importer` would spell an import of `module`: a relative
/// specifier without extension.
///
/// Two keys from one load are relative to one working directory, so counting
/// `..` from the importer needs no shared prefix, and the answer does not
/// depend on where `vyrn` was invoked. A pair that cannot be counted between
/// gets the module key: a generated banner, a remote key or a `std/` module,
/// and an absolute path beside a relative one or on another Windows drive.
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

    /// The one capability a declaration carries.
    #[test]
    fn the_logging_file_sink_is_a_carrier() {
        let c = caps("logging { sink: file(\"app.log\") }\nfn main() -> Int64 {\n    return 0\n}");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].0, Capability::Fs);
        assert!(c[0].1.contains("app.log"), "{:?}", c[0].1);
        // A sink that is not a file reaches nothing.
        assert!(caps("logging { sink: stdout }\nfn main() -> Int64 {\n    return 0\n}").is_empty());
    }

    /// The call carries `extern`, not the declaration: the direct backend imports
    /// only what a reached call needs. An `export extern fn` carries nothing.
    #[test]
    fn an_extern_import_is_carried_by_the_call() {
        assert_eq!(
            caps(
                "extern fn jsAdd(a: Int64, b: Int64) -> Int64\n\
                 fn main() -> Int64 {\n    return jsAdd(1, 2)\n}"
            ),
            vec![(Capability::Extern, "jsAdd".into())]
        );
        assert!(caps(
            "extern fn jsAdd(a: Int64, b: Int64) -> Int64\n\
             fn main() -> Int64 {\n    return 0\n}"
        )
        .is_empty());
        assert!(caps(
            "export extern fn twice(a: Int64) -> Int64 {\n    return a + a\n}\n\
             fn main() -> Int64 {\n    return twice(2)\n}"
        )
        .is_empty());
    }

    /// A `gen fn` runs at generation time and `test` blocks are never built.
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
                            judged: true,
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

        // The same tree under a target with a filesystem is fine.
        assert!(objection(&g, "/p/client/boot.vyrn", &map(Target::Native)).is_none());
    }

    /// Per target and capability: the subset test as a table.
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

    /// The remedy is counted from the importer, so the same edge spelled three
    /// ways by the working directory gets one answer.
    #[test]
    fn the_crossing_is_counted_from_the_importer_whatever_the_cwd() {
        for (importer, module) in [
            // `cd examples/leak && vyrn check client/boot.vyrn`: no shared first
            // segment.
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
        // An importer at the root has no `..` to climb, and a specifier without
        // `./` would name a package.
        assert_eq!(
            crossing("boot.vyrn", "server/db.vyrn"),
            "connect(\"./server/db\")"
        );
        assert_eq!(
            crossing("client/boot.vyrn", "db.vyrn"),
            "connect(\"../db\")"
        );
    }

    /// A key that is not a path, or two different roots, is quoted whole.
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

    /// A root no artifact names gets no floor, whatever it carries.
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
