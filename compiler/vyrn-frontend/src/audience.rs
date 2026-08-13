//! RFC-0072 M1 — audience: who runs a module.
//!
//! Every module has exactly one audience, and it is read off the module's own
//! path. `server/store.vyrn` is server-only; `app/routes/index.vyx` is universal;
//! `client/boot.vyrn` is client-only. Nothing is declared per file — the
//! directory tree already says it, and this module is the reading.
//!
//! The vocabulary is a project's own, declared once in `vyrn.json`:
//!
//! ```json
//! { "audience": { "server": ["server"], "client": ["client"],
//!                 "universal": ["app", "shared"] } }
//! ```
//!
//! A project with no `audience` key gets [`None`] from [`from_manifest`], which
//! is the signal to skip the whole mechanism: every module is universal and no
//! import is ever rejected. Adoption is opt-in per project, which is what lets
//! this land without touching a single existing example.
//!
//! **Nearest wins.** The deciding segment is the LAST audience segment on the
//! path, so `server/api/pastes.vyrn` and `src/pastes/server/api/pastes.vyrn`
//! resolve identically, and one checker covers both the audience-outer layout
//! (Nuxt's vocabulary) and the feature-outer one. The same rule is what
//! [`crate::contracts::role_for`] uses for role scopes, so the two path axes —
//! audience outside, role inside — compose instead of competing.

use crate::schema::Json;

/// Who runs a module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// Server-only: never in the client bundle.
    Server,
    /// Client-only: never in the server binary.
    Client,
    /// SSR and client bundle both, or no UI at all — the conservative default,
    /// since a universal module is legal to import from anywhere.
    Universal,
}

impl Audience {
    /// How a diagnostic names it (`server-only`, `universal`).
    pub fn phrase(self) -> &'static str {
        match self {
            Audience::Server => "server-only",
            Audience::Client => "client-only",
            Audience::Universal => "universal",
        }
    }

    /// The `vyrn.json` key that declares this audience's segments.
    pub fn key(self) -> &'static str {
        match self {
            Audience::Server => "audience.server",
            Audience::Client => "audience.client",
            Audience::Universal => "audience.universal",
        }
    }
}

impl std::fmt::Display for Audience {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Audience::Server => "server",
            Audience::Client => "client",
            Audience::Universal => "universal",
        })
    }
}

/// A project's declared audience vocabulary, plus the directory it is rooted at.
///
/// `base` is the manifest's directory: only paths UNDER it are subject to the
/// rule. Without it a `std/` file living under some ancestor directory named
/// `client/` would acquire an audience nobody declared, and the std library has
/// no business having one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudienceMap {
    pub server: Vec<String>,
    pub client: Vec<String>,
    pub universal: Vec<String>,
    /// The project's ENTRY POINTS and the audience each one has by virtue of
    /// being one: `(slash path, audience, manifest key)`.
    ///
    /// A composition root is the one module that legitimately names both sides,
    /// and no path segment can say so — `server.vyrn` sits at the project root by
    /// design (RFC-0072's own migration table leaves it there). But the manifest
    /// ALREADY says which file is the server and which is the client, in keys
    /// every fullstack example has written since RFC-0013. So the entry point's
    /// audience is read off the key that names it, and nothing new is declared.
    pub entries: Vec<(String, Audience, String)>,
    /// Slash-separated project directory. Empty means "no base" — every path is
    /// subject to the rule, which is what the in-process tests want.
    pub base: String,
}

/// What decided a module's audience.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// The nearest audience segment on its path.
    Segment(String),
    /// The `vyrn.json` key naming it as an entry point (`server`, `client`,
    /// `main`).
    Entry(String),
    /// Nothing did: universal is the default.
    Default,
}

/// One resolved audience, with what decided it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub audience: Audience,
    pub reason: Reason,
}

impl Verdict {
    /// The universal default: nothing on the path said otherwise.
    pub fn universal() -> Self {
        Verdict {
            audience: Audience::Universal,
            reason: Reason::Default,
        }
    }

    /// `path segment `server` (vyrn.json audience.server)`, or the default's
    /// phrasing — the "why" half of every diagnostic and of `vyrn why`.
    pub fn because(&self) -> String {
        match &self.reason {
            Reason::Segment(s) => {
                format!("path segment `{s}` (vyrn.json {})", self.audience.key())
            }
            Reason::Entry(k) => {
                format!("being this project's `{k}` entry point (vyrn.json:{k})")
            }
            Reason::Default => {
                "no audience segment on its path (universal is the default)".to_string()
            }
        }
    }
}

/// The `"audience"` map of a `vyrn.json` document, or `None` when the manifest
/// has no such key.
///
/// `None` is load-bearing: it is the entire compatibility story. A project that
/// has not opted in gets no map, the loader runs no check, and nothing it
/// compiles today can start failing.
///
/// It takes the PARSED document rather than the text, so that "this manifest
/// declares no audience" cannot be reached by a manifest that failed to parse.
/// A parse failure belongs to whoever read the file, and it is that reader's job
/// to refuse; this function only reads a rule out of a document that already
/// exists.
pub fn from_manifest(doc: &Json, base: &str) -> Option<AudienceMap> {
    let Some(Json::Obj(entries)) = doc.get("audience") else {
        return None;
    };
    let list = |key: &str| -> Vec<String> {
        match entries.iter().find(|(k, _)| k == key).map(|(_, v)| v) {
            Some(Json::Arr(items)) => items
                .iter()
                .filter_map(|i| match i {
                    Json::Str(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            Some(Json::Str(s)) => vec![s.clone()],
            _ => Vec::new(),
        }
    };
    let base = base.replace('\\', "/").trim_end_matches('/').to_string();
    // The entry points, from the manifest's own top-level keys. `main` is a
    // program that runs on the machine it was built for, so it is server-side in
    // the only sense the rule cares about: it may reach server modules and must
    // not reach client-only ones.
    let mut entries = Vec::new();
    for (key, audience) in [
        ("server", Audience::Server),
        ("client", Audience::Client),
        ("main", Audience::Server),
    ] {
        if let Some(Json::Str(rel)) = doc.get(key) {
            let path = if base.is_empty() {
                rel.clone()
            } else {
                format!("{base}/{rel}")
            };
            entries.push((crate::loader::normalize(&path), audience, key.to_string()));
        }
    }
    Some(AudienceMap {
        server: list("server"),
        client: list("client"),
        universal: list("universal"),
        entries,
        base,
    })
}

/// The audience of `path`, and the segment that decided it.
///
/// `path` is a module key — a slash path, a `.vyx` (or other generator input),
/// or a generated module's banner, which is resolved back to the real file that
/// triggered generation first. A page's generated server and client modules
/// therefore inherit the page's own audience, which is the only answer that
/// could be right: the audience is a property of the file a person wrote.
pub fn audience_of(key: &str, map: &AudienceMap) -> Verdict {
    let verdict = declared_audience_of(key, map);
    if verdict.audience != Audience::Universal {
        return verdict;
    }
    // A generated module has no independent existence: it is compiled for
    // whoever generated it, and the root that mounts it is the one that runs it.
    // A `.vyx` page is the case that forces this — `vyxPage` and `vyxPageClient`
    // compile the SAME file into two modules that go to opposite sides of the
    // wire, and reading both as the file's own (universal) audience makes the
    // SSR half — the half whose whole job is to reach the server — illegal for
    // doing exactly that. The half that reaches a browser is still checked,
    // against the client root that mounts it, so nothing is exempted: a page
    // whose VIEW touches a server module is rejected there (RFC-0072 M5).
    if let Some(importer) = crate::loader::generated_importer(key) {
        let caller = audience_of(importer, map);
        if caller.audience != Audience::Universal {
            return caller;
        }
    }
    verdict
}

/// The audience `path` declares for itself: the manifest key naming it as an
/// entry point, else the nearest audience segment on its path.
fn declared_audience_of(path: &str, map: &AudienceMap) -> Verdict {
    let path = source_file(path);
    // An entry point's audience is DECLARED (by the key that names it), so it
    // beats anything read off the path. There is nothing above a composition
    // root for a segment to be nearer than.
    if let Some((_, a, key)) = map.entries.iter().find(|(p, _, _)| same_path(p, &path)) {
        return Verdict {
            audience: *a,
            reason: Reason::Entry(key.clone()),
        };
    }
    let rel = match relative_to(&path, &map.base) {
        Some(r) => r,
        // Outside the project: std, a remote module, a vendored dependency.
        // Nothing declared its audience, so it is universal and importable.
        None => return Verdict::universal(),
    };
    // Directory components only: a FILE named `server.vyrn` is a composition
    // root, not a server-only module, and reading it as one would make every
    // existing example's entry point server-only overnight.
    let mut comps: Vec<&str> = rel.split('/').collect();
    comps.pop();
    let mut out = Verdict::universal();
    for c in comps {
        if let Some(a) = classify(c, map) {
            out = Verdict {
                audience: a,
                reason: Reason::Segment(c.to_string()),
            };
        }
    }
    out
}

/// Whether two slash paths name the same module. An entry point is spelled
/// relative to the manifest while a module key may be spelled relative to the
/// working directory, so a suffix match on a full component boundary is the
/// honest comparison — the same allowance [`relative_to`] makes.
fn same_path(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (long, short) = if a.len() > b.len() { (a, b) } else { (b, a) };
    long.ends_with(short) && long.as_bytes()[long.len() - short.len() - 1] == b'/'
}

/// The file a module key's audience should be read from — itself for an
/// ordinary module, and for a GENERATED one the source a person actually wrote.
///
/// A generated module has no path, so it has to borrow one, and there are two
/// candidates: the generator's INPUT (`vyxPage("./app/routes/index.vyx")`) and
/// the module that called the generator. The input is the right answer whenever
/// there is one — a page compiled out of a `.vyx` is that page, wherever the
/// root that mounted it happens to live — and RFC-0072's whole claim is that the
/// audience is a property of the file, not of the build. A generator pointed at
/// a DIRECTORY (`pages("./app/routes")`) has no single input file; its module is
/// router glue, and it inherits the calling module's audience.
pub fn source_file(key: &str) -> String {
    let importer = crate::loader::generated_importer(key)
        .unwrap_or(key)
        .replace('\\', "/");
    let Some(arg) = first_generator_arg(key) else {
        return importer;
    };
    // A directory argument names no file: `./app/routes` has no extension on its
    // last component, so it is not the module's origin.
    let last = arg.rsplit('/').next().unwrap_or(&arg);
    if !last.contains('.') || arg.starts_with("std/") {
        return importer;
    }
    let dir = match importer.rfind('/') {
        Some(i) => importer[..i].to_string(),
        None => return importer,
    };
    join_normalized(&dir, &arg)
}

/// The first string argument of the INNERMOST generator call in a banner key
/// (`generated by vyxPage("./x.vyx") at …`).
fn first_generator_arg(key: &str) -> Option<String> {
    let rest = key.strip_prefix("generated by ")?;
    let open = rest.find('(')?;
    let after = &rest[open + 1..];
    let q = after.find('"')?;
    let tail = &after[q + 1..];
    let end = tail.find('"')?;
    Some(tail[..end].to_string())
}

/// `dir` + `rel`, with `.`/`..` components folded away.
fn join_normalized(dir: &str, rel: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for c in dir.split('/').chain(rel.split('/')) {
        match c {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    let joined = out.join("/");
    if dir.starts_with('/') {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Which audience `segment` names, if any. A segment listed under several keys
/// takes the first match in server / client / universal order; declaring one
/// twice is a manifest mistake, not a language question.
fn classify(segment: &str, map: &AudienceMap) -> Option<Audience> {
    if map.server.iter().any(|s| s == segment) {
        return Some(Audience::Server);
    }
    if map.client.iter().any(|s| s == segment) {
        return Some(Audience::Client);
    }
    if map.universal.iter().any(|s| s == segment) {
        return Some(Audience::Universal);
    }
    None
}

/// `path` with `base` stripped, or `None` when it is not under `base`. An empty
/// base matches everything (there is no project directory to be outside of).
///
/// The one wrinkle is that module keys are as RELATIVE as the path the CLI was
/// handed, while the manifest's directory can be absolute (it is found by
/// walking up from the working directory when the root file has no parent
/// component). A relative key against an absolute base is therefore not "outside
/// the project" — it is the same project, spelled from inside it — so it is
/// taken as already project-relative rather than silently losing its audience.
fn relative_to(path: &str, base: &str) -> Option<String> {
    if base.is_empty() {
        return Some(path.trim_start_matches('/').to_string());
    }
    if let Some(rest) = path.strip_prefix(base) {
        if rest.is_empty() {
            return None;
        }
        return rest.strip_prefix('/').map(|r| r.to_string());
    }
    if is_absolute(base) && !is_absolute(path) {
        return Some(path.to_string());
    }
    None
}

/// Whether a slash path is absolute: rooted, or drive-qualified on Windows.
fn is_absolute(path: &str) -> bool {
    path.starts_with('/') || {
        let b = path.as_bytes();
        b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic()
    }
}

/// Whether `importer` importing `imported` WIDENS audience — the one illegal
/// edge shape.
///
/// Legal: anything → universal (a universal module is legal everywhere),
/// server → server, client → client. Illegal: reaching a server-only module
/// from anywhere but the server, or a client-only module from anywhere but the
/// client. Stated once, because it is the whole rule.
pub fn widens(importer: Audience, imported: Audience) -> bool {
    match imported {
        Audience::Universal => false,
        other => other != importer,
    }
}

/// The advice line a rejected import ends with — what to do instead.
pub fn remedy(imported: Audience) -> &'static str {
    match imported {
        Audience::Server => "call it through `client(\"./server/api\")` instead",
        Audience::Client => {
            "move the shared part into a universal module (`shared/`) and import that instead"
        }
        Audience::Universal => "",
    }
}

/// `path` as a reader of the project would type it — relative to the project
/// directory when it is inside one, else unchanged. Diagnostics quote paths, and
/// an absolute one from a temp directory is noise around the two names that
/// matter.
pub fn display_path(path: &str, map: &AudienceMap) -> String {
    let path = source_file(path);
    relative_to(&path, &map.base).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> AudienceMap {
        AudienceMap {
            server: vec!["server".into()],
            client: vec!["client".into()],
            universal: vec!["app".into(), "shared".into()],
            entries: Vec::new(),
            base: "/p".into(),
        }
    }

    /// The reader takes a parsed document; these tests are written as JSON.
    fn from_text(json: &str, base: &str) -> Option<AudienceMap> {
        from_manifest(&crate::schema::parse_json(json).unwrap(), base)
    }

    #[test]
    fn no_audience_key_is_no_map() {
        assert!(from_text("{\"name\":\"x\"}", "/p").is_none());
    }

    #[test]
    fn manifest_shape_from_the_rfc() {
        let m = from_text(
            r#"{"audience":{"server":["server"],"client":["client"],"universal":["app","shared"]}}"#,
            "/p/",
        )
        .unwrap();
        assert_eq!(m.server, vec!["server".to_string()]);
        assert_eq!(m.universal, vec!["app".to_string(), "shared".to_string()]);
        assert_eq!(m.base, "/p");
    }

    #[test]
    fn audience_outer_and_feature_outer_agree() {
        let m = map();
        assert_eq!(
            audience_of("/p/server/api/pastes.vyrn", &m).audience,
            Audience::Server
        );
        assert_eq!(
            audience_of("/p/src/pastes/server/api/pastes.vyrn", &m).audience,
            Audience::Server
        );
    }

    #[test]
    fn nearest_segment_wins() {
        let m = map();
        // A universal directory nested under a server one is universal.
        let v = audience_of("/p/server/app/widget.vyrn", &m);
        assert_eq!(v.audience, Audience::Universal);
        assert_eq!(v.reason, Reason::Segment("app".into()));
        // And the other way round.
        let v = audience_of("/p/app/server/secret.vyrn", &m);
        assert_eq!(v.audience, Audience::Server);
        assert_eq!(v.reason, Reason::Segment("server".into()));
    }

    #[test]
    fn a_file_named_server_is_not_a_server_module_unless_the_manifest_says_so() {
        let m = map();
        let v = audience_of("/p/server.vyrn", &m);
        assert_eq!(v.audience, Audience::Universal);
        assert_eq!(v.reason, Reason::Default);

        // …but the manifest naming it as the server entry point does say so.
        let m = from_text(
            r#"{"server":"server.vyrn","client":"client.vyrn",
                "audience":{"server":["server"],"client":["client"],"universal":["app"]}}"#,
            "/p",
        )
        .unwrap();
        let v = audience_of("/p/server.vyrn", &m);
        assert_eq!(v.audience, Audience::Server);
        assert_eq!(v.reason, Reason::Entry("server".into()));
        assert_eq!(audience_of("/p/client.vyrn", &m).audience, Audience::Client);
    }

    #[test]
    fn outside_the_project_has_no_audience() {
        let m = map();
        assert_eq!(
            audience_of("/elsewhere/server/x.vyrn", &m).audience,
            Audience::Universal
        );
    }

    #[test]
    fn the_legal_edges() {
        use Audience::*;
        assert!(!widens(Universal, Universal));
        assert!(!widens(Server, Universal));
        assert!(!widens(Client, Universal));
        assert!(!widens(Server, Server));
        assert!(!widens(Client, Client));
        assert!(widens(Universal, Server));
        assert!(widens(Universal, Client));
        assert!(widens(Client, Server));
        assert!(widens(Server, Client));
    }
}
