//! RFC-0103 M1 — artifacts: what a project builds, and where each one runs.
//!
//! An artifact is an entry point and a target. The target is a CAPABILITY
//! declaration, not a build-target selection: M0's census found two build
//! targets (`native` and `wasm`) and three answers about what the code can
//! reach, because `wasi` and `browser` run the identical bytes under hosts that
//! answer the WASI imports differently. A browser has no filesystem, and no edit
//! to `vyrn.json` can give it one.
//!
//! ```json
//! { "artifacts": {
//!     "api": { "entry": "server/main.vyrn", "target": "native" },
//!     "app": { "entry": "client/boot.vyrn", "target": "browser" } } }
//! ```
//!
//! The keys every fullstack project has written since RFC-0013 are sugar for
//! exactly this: `main` and `server` name native artifacts, `client` names a
//! browser one, each under its own key's name. A project that writes only those
//! keys is already declaring artifacts and never sees the new spelling — the
//! same trick RFC-0072 plays to read an entry point's audience off the key that
//! names it.
//!
//! Opt-in is absolute, as it is for [`crate::audience`]: no `artifacts` map and
//! no entry-point key gets [`None`], which is the signal that this project
//! declares nothing to check. M1 parsed and exposed; [`crate::floor`] is the M2
//! rule that reads it.

use crate::audience::RealPath;
use crate::schema::Json;

/// Where an artifact runs — the capability set it gets, in the vocabulary the
/// manifest writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A binary for the machine that built it.
    Native,
    /// Wasm under a WASI host: a filesystem, stdin, args.
    Wasi,
    /// The same wasm in a page: a clock and a CSPRNG, and no filesystem.
    Browser,
}

/// The values `artifacts.<name>.target` accepts, for diagnostics. One list, so
/// the error cannot drift from [`Target::parse`].
pub const TARGETS: &str = "native, wasi, browser";

impl Target {
    pub fn parse(s: &str) -> Option<Target> {
        Some(match s {
            "native" => Target::Native,
            "wasi" => Target::Wasi,
            "browser" => Target::Browser,
            _ => return None,
        })
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Target::Native => "native",
            Target::Wasi => "wasi",
            Target::Browser => "browser",
        })
    }
}

/// One declared artifact: the name it is called by, the entry point it is built
/// from, and where it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// The `artifacts` key, or the sugar key (`main`, `server`, `client`).
    pub name: String,
    /// The entry point as a slash path, resolved against the manifest's
    /// directory exactly as an audience entry point is.
    pub entry: String,
    pub target: Target,
}

/// What a project builds, and the two things the floor needs to decide on it:
/// the directory the entries hang off, and the consumer's reading of file
/// IDENTITY. Exactly the shape [`crate::audience::AudienceMap`] has, for exactly
/// the same reason — an artifact entry point and an audience entry point are the
/// same paths, and one file must not be two.
#[derive(Debug, Clone, Default)]
pub struct ArtifactMap {
    /// The declared artifacts, sugar first, in manifest order.
    pub list: Vec<Artifact>,
    /// The project directory as a slash path. Empty means "no base" — every
    /// path is inside it, which is what the in-process tests want.
    pub base: String,
    /// The consumer's file-identity function ([`crate::manifest::real_path`] in
    /// both real ones), or `None` when paths are compared as written.
    pub realpath: Option<RealPath>,
}

/// Two maps are equal when they DECLARE the same thing; `realpath` is the
/// consumer's reading of the disk, not part of the declaration.
impl PartialEq for ArtifactMap {
    fn eq(&self, other: &Self) -> bool {
        self.list == other.list && self.base == other.base
    }
}

impl Eq for ArtifactMap {}

impl ArtifactMap {
    /// This map, deciding on file identity as `f` reports it. The base and every
    /// entry are put into the form a module key will be, or nothing matches.
    pub fn with_realpath(mut self, f: RealPath) -> Self {
        self.base = f(&self.base).unwrap_or(self.base);
        for a in self.list.iter_mut() {
            a.entry = f(&a.entry).unwrap_or_else(|| a.entry.clone());
        }
        self.realpath = Some(f);
        self
    }

    /// `path` as file identity: what the filesystem calls it, or the path itself
    /// when nothing on disk answers to it (a generated module's banner, a remote
    /// key, an in-memory test module).
    fn identity(&self, path: &str) -> String {
        match self.realpath {
            Some(f) => f(path).unwrap_or_else(|| path.to_string()),
            None => path.to_string(),
        }
    }

    /// The artifact whose ENTRY is `root`, or `None` when this root is nobody's
    /// entry point.
    ///
    /// That `None` is the whole blast radius of the floor. A file that no
    /// artifact names gets no capability check even with a manifest above it —
    /// `examples/externdemo.vyrn` is built natively by the parity suite and is
    /// not an entry, so the census's "one real cost" of refusing `extern` off
    /// the browser is not paid by it.
    pub fn artifact_for(&self, root: &str) -> Option<&Artifact> {
        let root = self.identity(root);
        self.list
            .iter()
            .find(|a| crate::audience::same_path(&a.entry, &root))
    }

    /// `path` as a reader of the project would type it — relative to the project
    /// directory when it is inside one, else unchanged. A generated module's
    /// banner is kept as a banner: it already names the call site that produced
    /// it, and that call site is a path, so it is spelled the same way.
    pub fn display_path(&self, path: &str) -> String {
        let path = self.identity(path);
        // A banner keeps its banner shape — it already names the call site
        // that produced it. The importer side spells like any other path, and
        // both banner generations (the `\u{1f}` separator and the legacy
        // `" at "`) re-display as the readable form.
        if let Some(at) = crate::loader::generated_importer(&path) {
            let head = &path[..path.len() - at.len()];
            let head = head
                .strip_suffix(crate::loader::GEN_SEP)
                .or_else(|| head.strip_suffix(" at "))
                .unwrap_or(head);
            return format!("{head} at {}", self.display_path(at));
        }
        crate::audience::relative_to(&path, &self.base).unwrap_or(path)
    }
}

/// The artifacts a manifest declares — the `artifacts` map plus the entry-point
/// keys that are sugar for it — or `None` when it declares neither.
///
/// Like [`crate::audience::from_manifest`] this takes the PARSED document, so
/// "this manifest declares no artifacts" cannot be reached by a manifest that
/// failed to parse. `base` is the manifest's directory; entry paths are joined
/// onto it and it is what a diagnostic names.
///
/// The `Err` arm is a manifest that declares artifacts and says something
/// contradictory about them. It is NOT the floor check (that is M2): nothing
/// here asks whether an entry file exists, only whether the declaration is one.
pub fn from_manifest(doc: &Json, base: &str) -> Result<Option<ArtifactMap>, String> {
    let base = base.replace('\\', "/").trim_end_matches('/').to_string();
    let at = format!("{base}/vyrn.json");
    let entry_path = |rel: &str| -> String {
        crate::loader::normalize(&if base.is_empty() {
            rel.to_string()
        } else {
            format!("{base}/{rel}")
        })
    };

    // The sugar. `main` and `server` are programs built for the machine that
    // runs them; `client` is the half that reaches a browser.
    let map = |list: Vec<Artifact>| ArtifactMap {
        list,
        base: base.clone(),
        realpath: None,
    };
    let mut out: Vec<Artifact> = Vec::new();
    for (key, target) in [
        ("main", Target::Native),
        ("server", Target::Native),
        ("client", Target::Browser),
    ] {
        if let Some(Json::Str(rel)) = doc.get(key) {
            out.push(Artifact {
                name: key.to_string(),
                entry: entry_path(rel),
                target,
            });
        }
    }

    // Everything up to here came from a sugar key, which is what lets the loop
    // below tell "this repeats the `client` key" from "this key is written
    // twice" — two different mistakes with two different fixes.
    let sugar = out.len();
    let declared = match doc.get("artifacts") {
        None => {
            return Ok(if out.is_empty() { None } else { Some(map(out)) });
        }
        Some(Json::Obj(entries)) => entries,
        Some(_) => return Err(format!("`artifacts` in {at} is not an object")),
    };

    for (name, value) in declared {
        let Json::Obj(fields) = value else {
            return Err(format!("artifact `{name}` in {at} is not an object"));
        };
        let field = |k: &str| match fields.iter().find(|(f, _)| f == k).map(|(_, v)| v) {
            Some(Json::Str(s)) => Some(s.clone()),
            _ => None,
        };
        let Some(entry) = field("entry") else {
            return Err(format!("artifact `{name}` in {at} has no `entry` string"));
        };
        let Some(target) = field("target") else {
            return Err(format!(
                "artifact `{name}` in {at} has no `target` (expected one of: {TARGETS})"
            ));
        };
        let Some(target) = Target::parse(&target) else {
            return Err(format!(
                "unknown target `{target}` for artifact `{name}` in {at} \
                 (expected one of: {TARGETS})"
            ));
        };
        let artifact = Artifact {
            name: name.clone(),
            entry: entry_path(&entry),
            target,
        };
        if let Some(i) = out.iter().position(|a| a.name == artifact.name) {
            // Two entries under one name inside `artifacts` say two things
            // about one artifact, and which one wins is whichever the reader
            // happened to keep. Refused whether or not they agree: one name,
            // one declaration.
            if i >= sugar {
                return Err(format!("artifact `{name}` is declared twice in {at}"));
            }
            // Against a sugar key it is a redeclaration, not a duplicate.
            // Writing a project's artifacts out in full is how it stops using
            // sugar, so repeating what the key already says is accepted — and
            // only repeating it.
            if out[i] == artifact {
                continue;
            }
            return Err(format!(
                "artifact `{name}` in {at} disagrees with the `{name}` key: \
                 `{}` ({}) against `{}` ({})",
                artifact.entry, artifact.target, out[i].entry, out[i].target
            ));
        }
        out.push(artifact);
    }
    Ok(Some(map(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_text(json: &str, base: &str) -> Result<Option<ArtifactMap>, String> {
        from_manifest(&crate::schema::parse_json(json).unwrap(), base)
    }

    fn ok(json: &str) -> Vec<Artifact> {
        from_text(json, "/p").unwrap().unwrap().list
    }

    #[test]
    fn no_artifacts_and_no_entry_keys_is_no_map() {
        assert_eq!(from_text(r#"{"name":"x"}"#, "/p").unwrap(), None);
    }

    /// The floor fires for a declared entry point and for nothing else — the
    /// answer to "does this root have a target?" is this lookup, and a file no
    /// artifact names has none.
    #[test]
    fn only_a_declared_entry_point_has_an_artifact() {
        let m = from_text(
            r#"{"artifacts":{"app":{"entry":"client/boot.vyrn","target":"browser"}}}"#,
            "/p",
        )
        .unwrap()
        .unwrap();
        assert_eq!(m.artifact_for("/p/client/boot.vyrn").unwrap().name, "app");
        // The same file, spelled from inside the project.
        assert_eq!(m.artifact_for("client/boot.vyrn").unwrap().name, "app");
        assert!(m.artifact_for("/p/client/other.vyrn").is_none());
        assert!(m.artifact_for("/p/examples/externdemo.vyrn").is_none());
        assert_eq!(m.display_path("/p/server/db.vyrn"), "server/db.vyrn");
    }

    #[test]
    fn the_shape_from_the_rfc() {
        let a = ok(r#"{"artifacts":{
            "api":{"entry":"server/main.vyrn","target":"native"},
            "app":{"entry":"client/boot.vyrn","target":"browser"}}}"#);
        assert_eq!(
            a,
            vec![
                Artifact {
                    name: "api".into(),
                    entry: "/p/server/main.vyrn".into(),
                    target: Target::Native,
                },
                Artifact {
                    name: "app".into(),
                    entry: "/p/client/boot.vyrn".into(),
                    target: Target::Browser,
                },
            ]
        );
    }

    /// The keys every project already writes ARE artifacts, under their own
    /// names: `main`/`server` native, `client` browser.
    #[test]
    fn the_entry_point_keys_are_sugar() {
        let a = ok(r#"{"server":"server.vyrn","client":"client/boot.vyrn"}"#);
        assert_eq!(
            a,
            vec![
                Artifact {
                    name: "server".into(),
                    entry: "/p/server.vyrn".into(),
                    target: Target::Native,
                },
                Artifact {
                    name: "client".into(),
                    entry: "/p/client/boot.vyrn".into(),
                    target: Target::Browser,
                },
            ]
        );
        assert_eq!(ok(r#"{"main":"src/main.vyrn"}"#)[0].target, Target::Native);
    }

    /// Writing the sugar out in full is how a project stops using sugar, so the
    /// two spellings coexist when they agree — and only then.
    #[test]
    fn an_explicit_artifact_may_repeat_a_key_but_not_contradict_it() {
        let both = r#"{"server":"server.vyrn","client":"client/boot.vyrn",
            "artifacts":{"server":{"entry":"server.vyrn","target":"native"},
                         "client":{"entry":"client/boot.vyrn","target":"browser"}}}"#;
        let a = ok(both);
        assert_eq!(a.len(), 2, "the redeclaration is one artifact, not two");
        assert_eq!(a[0].name, "server");

        let e = from_text(
            r#"{"client":"client/boot.vyrn",
                "artifacts":{"client":{"entry":"client/other.vyrn","target":"browser"}}}"#,
            "/p",
        )
        .unwrap_err();
        assert!(e.contains("disagrees with the `client` key"), "{e}");
        assert!(e.contains("client/other.vyrn"), "{e}");
        assert!(e.contains("client/boot.vyrn"), "{e}");

        // Same entry, different target is the same contradiction.
        let e = from_text(
            r#"{"client":"boot.vyrn",
                "artifacts":{"client":{"entry":"boot.vyrn","target":"native"}}}"#,
            "/p",
        )
        .unwrap_err();
        assert!(e.contains("disagrees"), "{e}");
    }

    /// One name, one declaration. The refusal a manifest actually meets is the
    /// JSON reader's, which sees the second key before this module sees any of
    /// them; the check here covers a document that reader did not build.
    #[test]
    fn a_name_declared_twice_is_refused() {
        let e = crate::schema::parse_json(
            r#"{"artifacts":{"app":{"entry":"a.vyrn","target":"native"},
                             "app":{"entry":"b.vyrn","target":"native"}}}"#,
        )
        .unwrap_err();
        assert!(e.contains("`app` is defined twice"), "{e}");

        let one = || {
            Json::Obj(vec![
                ("entry".into(), Json::Str("a.vyrn".into())),
                ("target".into(), Json::Str("native".into())),
            ])
        };
        let doc = Json::Obj(vec![(
            "artifacts".into(),
            Json::Obj(vec![("app".into(), one()), ("app".into(), one())]),
        )]);
        let e = from_manifest(&doc, "/p")
            .err()
            .expect("two declarations of one name is not one artifact");
        assert!(e.contains("artifact `app` is declared twice"), "{e}");
    }

    #[test]
    fn a_declaration_that_is_not_one_names_what_is_missing() {
        for (json, want) in [
            (r#"{"artifacts":[]}"#, "`artifacts` in /p/vyrn.json"),
            (r#"{"artifacts":{"app":"x.vyrn"}}"#, "is not an object"),
            (r#"{"artifacts":{"app":{"target":"native"}}}"#, "`entry`"),
            (r#"{"artifacts":{"app":{"entry":"x.vyrn"}}}"#, "`target`"),
        ] {
            let e = from_text(json, "/p").unwrap_err();
            assert!(e.contains(want), "missing {want:?} in: {e}");
        }
    }

    #[test]
    fn an_unknown_target_names_the_three_valid_ones() {
        let e = from_text(
            r#"{"artifacts":{"app":{"entry":"x.vyrn","target":"wasm"}}}"#,
            "/p",
        )
        .unwrap_err();
        assert!(e.contains("unknown target `wasm`"), "{e}");
        assert!(e.contains("artifact `app`"), "{e}");
        assert!(e.contains("/p/vyrn.json"), "{e}");
        assert!(e.contains("native, wasi, browser"), "{e}");
    }

    /// M1 parses a declaration; it does not walk it. A missing entry file is
    /// M2's question, and refusing it here would break every project whose
    /// manifest names a file it has not written yet.
    #[test]
    fn a_missing_entry_file_is_not_this_milestones_business() {
        let a = ok(r#"{"artifacts":{"app":{"entry":"nope/never.vyrn","target":"wasi"}}}"#);
        assert_eq!(a[0].entry, "/p/nope/never.vyrn");
        assert_eq!(a[0].target, Target::Wasi);
    }
}
