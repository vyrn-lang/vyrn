//! Artifacts: what a project builds, and where each one runs.
//!
//! An artifact is an entry point and a target. The target declares
//! capabilities, not a build: `wasi` and `browser` run identical wasm under
//! hosts that answer the WASI imports differently, and no `vyrn.json` edit
//! gives a browser a filesystem.
//!
//! ```json
//! { "artifacts": {
//!     "api": { "entry": "server/main.vyrn", "target": "native" },
//!     "app": { "entry": "client/boot.vyrn", "target": "browser" } } }
//! ```
//!
//! The `main` and `server` keys are sugar for native artifacts and `client`
//! for a browser one, each under its key's name. With neither the map nor a
//! sugar key, [`from_manifest`] returns `None` and nothing is checked;
//! [`crate::floor`] reads the map.

use crate::audience::RealPath;
use crate::schema::Json;

/// Where an artifact runs: the capability set it gets, in the manifest's
/// vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A binary for the machine that built it.
    Native,
    /// Wasm under a WASI host: a filesystem, stdin, args.
    Wasi,
    /// The same wasm in a page: a clock and a CSPRNG, and no filesystem.
    Browser,
}

/// The values `artifacts.<name>.target` accepts, for diagnostics; it must
/// match [`Target::parse`].
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

/// One declared artifact: its name, the entry point it is built from, and
/// where it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// The `artifacts` key, or the sugar key (`main`, `server`, `client`).
    pub name: String,
    /// The entry point as a slash path, resolved against the manifest's directory
    /// as an audience entry point is.
    pub entry: String,
    pub target: Target,
}

/// What a project builds, with the base directory and file-identity function
/// the floor decides on. It mirrors [`crate::audience::AudienceMap`], because
/// artifact and audience entry points are the same paths.
#[derive(Debug, Clone, Default)]
pub struct ArtifactMap {
    /// The declared artifacts, sugar first, in manifest order.
    pub list: Vec<Artifact>,
    /// The project directory as a slash path. Empty means every path is inside.
    pub base: String,
    /// The consumer's file-identity function ([`crate::manifest::real_path`] in
    /// both real ones), or `None` when paths are compared as written.
    pub realpath: Option<RealPath>,
}

/// Two maps are equal when they declare the same thing; `realpath` is the
/// consumer's reading of the disk.
impl PartialEq for ArtifactMap {
    fn eq(&self, other: &Self) -> bool {
        self.list == other.list && self.base == other.base
    }
}

impl Eq for ArtifactMap {}

impl ArtifactMap {
    /// Decides on file identity as `f` reports it; the base and every entry are
    /// converted too, or nothing matches.
    pub fn with_realpath(mut self, f: RealPath) -> Self {
        self.base = f(&self.base).unwrap_or(self.base);
        for a in self.list.iter_mut() {
            a.entry = f(&a.entry).unwrap_or_else(|| a.entry.clone());
        }
        self.realpath = Some(f);
        self
    }

    /// Returns `path` as file identity: what the filesystem calls it, or `path`
    /// for a banner, a remote key or an in-memory module.
    fn identity(&self, path: &str) -> String {
        match self.realpath {
            Some(f) => f(path).unwrap_or_else(|| path.to_string()),
            None => path.to_string(),
        }
    }

    /// Returns the artifact whose entry is `root`. `None` means no floor: a file
    /// no artifact names gets no capability check, even under a manifest.
    pub fn artifact_for(&self, root: &str) -> Option<&Artifact> {
        let root = self.identity(root);
        self.list
            .iter()
            .find(|a| crate::audience::same_path(&a.entry, &root, &self.base))
    }

    /// Returns `path` as a project reader types it: relative to the project
    /// directory when inside it.
    pub fn display_path(&self, path: &str) -> String {
        let path = self.identity(path);
        // A banner keeps its shape and displays its importer like any path. Both
        // separators (`\u{1f}` and `" at "`) render as ` at `. Nested generation
        // nests banners, so every inner separator is replaced too, and no invisible
        // U+001F reaches a printed diagnostic.
        if let Some(at) = crate::loader::generated_importer(&path) {
            let head = &path[..path.len() - at.len()];
            let head = head
                .strip_suffix(crate::loader::GEN_SEP)
                .or_else(|| head.strip_suffix(" at "))
                .unwrap_or(head);
            let head = crate::loader::readable(head);
            return format!("{head} at {}", self.display_path(at));
        }
        crate::audience::relative_to(&path, &self.base).unwrap_or(path)
    }
}

/// Returns the artifacts a manifest declares (the `artifacts` map and the
/// sugar keys), or `None` when it declares neither.
///
/// It takes the parsed document, as [`crate::audience::from_manifest`] does.
/// Entry paths join `base`, the manifest's directory. `Err` is a contradictory
/// declaration; whether an entry file exists is not checked here.
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

    // The sugar: `main` and `server` build for the machine that runs them;
    // `client` reaches a browser.
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

    // Artifacts before `sugar` came from sugar keys, so the loop can tell a
    // repeated key from a name written twice.
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
            // Two entries under one name in `artifacts` are refused, even when they
            // agree: one name, one declaration.
            if i >= sugar {
                return Err(format!("artifact `{name}` is declared twice in {at}"));
            }
            // Against a sugar key, an identical redeclaration is accepted: writing
            // artifacts out in full is how a project leaves the sugar.
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

    /// Only a declared entry point has an artifact.
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

    /// A deeper file sharing the entry's tail is not that entry point.
    #[test]
    fn a_shared_tail_does_not_resolve_to_an_artifact() {
        let m = from_text(
            r#"{"artifacts":{"app":{"entry":"main.vyrn","target":"native"}}}"#,
            "",
        )
        .unwrap()
        .unwrap();
        assert!(m.artifact_for("/p/screens/main.vyrn").is_none());
        // The genuine spelling still answers.
        assert_eq!(m.artifact_for("main.vyrn").unwrap().name, "app");
    }

    /// Every separator of nested banners displays as ` at `.
    #[test]
    fn a_nested_banners_display_path_renders_every_separator() {
        let m = from_text(
            r#"{"artifacts":{"app":{"entry":"client/boot.vyrn","target":"browser"}}}"#,
            "/p",
        )
        .unwrap()
        .unwrap();
        let sep = crate::loader::GEN_SEP;
        let nested = format!(
            "generated by i18n(\"../app/strings\"){sep}generated by \
             components(\"./widgets\"){sep}/p/client/boot.vyrn"
        );
        assert_eq!(
            m.display_path(&nested),
            "generated by i18n(\"../app/strings\") at generated by \
             components(\"./widgets\") at client/boot.vyrn"
        );
    }

    #[test]
    fn the_documented_shape() {
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

    /// The entry-point keys are artifacts under their own names: `main` and
    /// `server` native, `client` browser.
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

    /// An explicit artifact may repeat a sugar key, but not contradict it.
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

    /// The JSON reader refuses a repeated key first; this covers a document that
    /// reader did not build.
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

    /// A missing entry file is not refused here, so a manifest may name a file not
    /// written yet.
    #[test]
    fn a_missing_entry_file_is_not_the_manifests_business() {
        let a = ok(r#"{"artifacts":{"app":{"entry":"nope/never.vyrn","target":"wasi"}}}"#);
        assert_eq!(a[0].entry, "/p/nope/never.vyrn");
        assert_eq!(a[0].target, Target::Wasi);
    }
}
