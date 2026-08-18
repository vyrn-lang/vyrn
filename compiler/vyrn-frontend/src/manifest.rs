//! The project context on disk: `vyrn.json`, `vyrn.lock`, the caches beside
//! them, and the two roots (`std/`, `web/`) a toolchain binary walks up to find.
//!
//! **Why this is in the frontend.** Two programs read a Vyrn project: the `vyrn`
//! driver and the language server. `vyrn-cli` is a binary crate, so the LSP
//! could not call its reader — and answered that by keeping a second copy. The
//! copy drifted, in the way a second copy always does: it did not verify a
//! cached blob's hash, it accepted a `vyrn.lock` the build refuses, and it
//! canonicalized the audience base at a different point in the sequence than the
//! CLI did. An editor that answers a different question from the build is worse
//! than an editor that answers none.
//!
//! `vyrn-frontend` is the one crate both already depend on, so the reader lives
//! here and both are consumers. What does NOT live here is the network: fetching
//! a remote module, resolving a floating ref, writing the lock back. Those are
//! the driver's, and the editor must never do them.
//!
//! Nothing in this module is reachable from the compiler proper — the frontend
//! still does not touch the disk to compile a program. This is the *toolchain's*
//! reader, kept in the shared crate for the only reason that matters: so there
//! is one of it.

use crate::schema::Json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// paths
// ---------------------------------------------------------------------------

/// What the filesystem calls `path`: the one canonicalization the whole
/// toolchain decides file identity by (RFC-0072).
///
/// `vyrn why` canonicalized and `vyrn check` did not, so one file could be
/// server-only to the tool a developer asks and universal to the checker that
/// gates the build — a second spelling (a different case on Windows, a directory
/// junction) walked a server module into a client bundle with no diagnostic.
/// `None` for a path the OS cannot resolve: a remote key, a module that exists
/// only in memory, a file that is not there.
pub fn real_path(path: &str) -> Option<String> {
    let p = Path::new(path).canonicalize().ok()?;
    Some(
        p.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .replace('\\', "/"),
    )
}

/// A sibling directory of the running executable's repo root: `$var` if it names
/// one that exists, else `name/` found by walking up at most five levels from
/// the executable.
///
/// Five is not arbitrary: the bundled LSP lives at `<repo>/editor/vscode/server/`
/// and dev builds at `<repo>/compiler/target/<profile>/`, and both are within
/// five levels of the repo root.
fn root_near_exe(var: &str, name: &str) -> Option<String> {
    if let Ok(p) = std::env::var(var) {
        if Path::new(&p).exists() {
            return Some(p.replace('\\', "/"));
        }
    }
    let mut dir = std::env::current_exe().ok()?;
    for _ in 0..5 {
        dir = dir.parent()?.to_path_buf();
        let cand = dir.join(name);
        if cand.is_dir() {
            return Some(cand.to_string_lossy().replace('\\', "/"));
        }
    }
    None
}

/// The std-library root: `$VYRN_STD`, or `std/` beside the executable's repo.
/// `None` if not found — only an error if a program actually imports `std/...`.
pub fn std_root() -> Option<String> {
    root_near_exe("VYRN_STD", "std")
}

/// The `web/` root holding the browser runtimes (`wasi-min.js`, `vyrn-rpc.js`,
/// `vyrn-query.js`): `$VYRN_WEB`, or `web/` beside the executable's repo.
pub fn web_root() -> Option<String> {
    root_near_exe("VYRN_WEB", "web")
}

// ---------------------------------------------------------------------------
// vyrn.json
// ---------------------------------------------------------------------------

/// The project manifest (`vyrn.json`), parsed with the frontend's own JSON
/// parser. All fields optional; unknown keys are ignored (forward compat).
pub struct Manifest {
    /// Directory the manifest lives in (slash-separated), as walked up to.
    pub dir: String,
    /// The parsed document. Every rule the manifest carries is read from this
    /// one parse: `audience`, `roles`, `dependencies`, and the rewrite `vyrn
    /// add` performs. Re-reading the file to re-parse it is how a second reader
    /// gets a different answer from the first.
    pub doc: Json,
    pub main: Option<String>,
    pub dependencies: Vec<(String, String)>,
    /// RFC-0102 M1: the `toolchain` object — tool name to version string —
    /// empty when the manifest declares none, which leaves every tool
    /// discovered exactly as before this key existed. A tool is a dependency,
    /// so this is read beside `dependencies` and resolved through the same
    /// lock, the same content-addressed cache and the same `--offline`.
    pub toolchain: Vec<(String, String)>,
    /// RFC-0072 M1: the declared audience vocabulary, or `None` when the
    /// manifest has no `audience` key — which leaves every module universal and
    /// every import legal, exactly as before this key existed.
    pub audience: Option<crate::audience::AudienceMap>,
    /// RFC-0103 M1: what this project builds — the `artifacts` map, plus the
    /// `main` / `server` / `client` keys that are sugar for it — or `None` when
    /// the manifest declares neither, which is the same absolute opt-in
    /// `audience` has. M2's floor reads it: the map carries the project base and
    /// the identity function, because an artifact entry and an audience entry
    /// are the same paths and one file must not be two.
    pub artifacts: Option<crate::artifacts::ArtifactMap>,
    /// The `nativeTarget` key, unvalidated. Kept as written so a diagnostic can
    /// quote it back and name the file.
    pub native_target: Option<String>,
}

/// Find `vyrn.json` by walking up from `start` (a directory).
///
/// Three outcomes, and they are three: `Ok(None)` is "this project declares
/// nothing", `Ok(Some)` is what it declares, and `Err` is "it declares
/// something and I cannot read it". Collapsing the third into the first is how a
/// trailing comma silently switched RFC-0072's audience boundary off while the
/// build printed `ok` — every rule the manifest carries evaporated with the
/// manifest, and an unreadable policy is not the empty policy.
pub fn find(start: &Path) -> Result<Option<Manifest>, String> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join("vyrn.json");
        if candidate.is_file() {
            let text = std::fs::read_to_string(&candidate)
                .map_err(|e| format!("cannot read {}: {e}", candidate.display()))?;
            let doc = crate::schema::parse_json(&text)
                .map_err(|e| format!("{} is not valid JSON: {e}", candidate.display()))?;
            let slash_dir = dir.to_string_lossy().replace('\\', "/");
            return from_doc(doc, slash_dir).map(Some);
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => return Ok(None),
        }
    }
}

/// A [`Manifest`] from an already-parsed document rooted at `slash_dir`. Split
/// out so the rules read off a manifest can be tested without a file, and so
/// there is exactly one place that decides how the audience base is formed.
///
/// `Err` is a manifest that declares something contradictory — today only
/// RFC-0103's artifacts. It travels the same channel as an unparseable manifest
/// because it has the same meaning: this project declares a rule and it cannot
/// be read, which is not the empty rule.
fn from_doc(doc: Json, slash_dir: String) -> Result<Manifest, String> {
    let str_key = |k: &str| match doc.get(k) {
        Some(Json::Str(s)) => Some(s.clone()),
        _ => None,
    };
    let str_map = |k: &str| -> Vec<(String, String)> {
        match doc.get(k) {
            Some(Json::Obj(entries)) => entries
                .iter()
                .filter_map(|(k, v)| match v {
                    Json::Str(s) => Some((k.clone(), s.clone())),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    };
    let dependencies = str_map("dependencies");
    // A tool name the table cannot resolve is refused where it is WRITTEN, not
    // where it is looked up: a typo that silently declared nothing would leave
    // the build on PATH with a `toolchain` key in the file saying otherwise,
    // which is the silent fallback RFC-0102 exists to forbid.
    let toolchain = str_map("toolchain");
    for (name, _) in &toolchain {
        if !crate::toolpin::KNOWN_TOOLS.contains(&name.as_str()) {
            return Err(crate::toolpin::unknown_tool(name));
        }
    }
    // The audience map decides on file identity, so its base is the project
    // directory as the FILESYSTEM names it — an empty walk-up result is the
    // working directory, not the root of everything. The base is canonicalized
    // HERE, before the entry points are joined onto it, because `with_realpath`
    // can only repair an entry whose file exists.
    let audience_base = real_path(if slash_dir.is_empty() {
        "."
    } else {
        &slash_dir
    })
    .unwrap_or_else(|| slash_dir.clone());
    let audience =
        crate::audience::from_manifest(&doc, &audience_base).map(|m| m.with_realpath(real_path));
    // Artifact entry points are joined onto the same canonical base as audience
    // entry points, because they are the same paths: `client` names one file,
    // and the two rules must not read it as two.
    let artifacts =
        crate::artifacts::from_manifest(&doc, &audience_base)?.map(|m| m.with_realpath(real_path));
    Ok(Manifest {
        main: str_key("main"),
        native_target: str_key("nativeTarget"),
        dependencies,
        toolchain,
        audience,
        artifacts,
        dir: slash_dir,
        doc,
    })
}

/// The parsed `vyrn.json` governing `dir`, for the readers that want one key out
/// of it. `None` covers both "no manifest" and "unreadable"; the reader that
/// REPORTS the unreadable one is [`find`], on the path every build and every
/// analyzed buffer takes.
pub fn doc_in(dir: &Path) -> Option<Json> {
    let text = std::fs::read_to_string(dir.join("vyrn.json")).ok()?;
    crate::schema::parse_json(&text).ok()
}

// ---------------------------------------------------------------------------
// vyrn.lock
// ---------------------------------------------------------------------------

/// `vyrn.lock`: `specifier ⇥ resolved-url ⇥ sha256` per line, sorted by
/// specifier. Line-based and diff-friendly by design.
#[derive(Debug)]
pub struct Lock {
    pub path: PathBuf,
    pub entries: BTreeMap<String, (String, String)>,
    pub dirty: bool,
}

impl Lock {
    /// Read the lock, or say which line stopped it.
    ///
    /// A lock file that exists and does not parse is not an unpinned project.
    /// Every way of damaging one — tabs turned to spaces by an editor or a CI
    /// checkout, a truncated write, a merge that appended instead of replacing —
    /// used to read as "this specifier was never pinned", and an unpinned
    /// specifier is fetched from the network and re-pinned to whatever arrives.
    /// The one artifact whose whole job is that it cannot drift failed toward
    /// the network in every direction. A missing file is still `Ok`: never
    /// pinned and never claimed otherwise.
    pub fn load(path: PathBuf) -> Result<Lock, String> {
        let mut entries: BTreeMap<String, (String, String)> = BTreeMap::new();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        for (i, line) in text.lines().enumerate() {
            let no = i + 1;
            if line.trim().is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            let [spec, url, sha] = fields[..] else {
                return Err(format!(
                    "{}:{no}: expected `specifier<TAB>url<TAB>sha256`, found {} \
                     tab-separated field(s)",
                    path.display(),
                    fields.len()
                ));
            };
            // Two pins for one specifier: the second used to replace the first
            // in silence, so appending a line changed what was built while
            // leaving the reviewed line in place, and the next `save` erased the
            // evidence. A specifier has exactly one pin.
            if entries.contains_key(spec) {
                return Err(format!(
                    "{}:{no}: `{spec}` is pinned twice; a specifier has exactly one pin",
                    path.display()
                ));
            }
            entries.insert(spec.to_string(), (url.to_string(), sha.to_string()));
        }
        Ok(Lock {
            path,
            entries,
            dirty: false,
        })
    }

    /// The lock beside the manifest in `project_dir`.
    pub fn in_project(project_dir: &str) -> Result<Lock, String> {
        Lock::load(Path::new(project_dir).join("vyrn.lock"))
    }

    pub fn save(&self) -> Result<(), String> {
        let mut out = String::new();
        for (spec, (url, sha)) in &self.entries {
            // The format is one line of three tab-separated fields, so a field
            // carrying a tab or a newline is a line the reader would split
            // differently from the writer. Specifiers come from source strings
            // the compiler does not constrain, so this is refused where it is
            // written rather than discovered where it is read.
            for field in [spec, url, sha] {
                if field.contains('\t') || field.contains('\n') || field.contains('\r') {
                    return Err(format!(
                        "`{field}` contains a tab or a line break and cannot be written to \
                         {}",
                        self.path.display()
                    ));
                }
            }
            out.push_str(&format!("{spec}\t{url}\t{sha}\n"));
        }
        std::fs::write(&self.path, out).map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
// content-addressed caches
// ---------------------------------------------------------------------------

fn home() -> String {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string())
}

/// The user's module cache: `~/.vyrn/cache/sha256/<hex>`.
pub fn cache_dir() -> PathBuf {
    Path::new(&home()).join(".vyrn/cache/sha256")
}

/// A project's committed, air-gapped copy of the same blobs.
pub fn vendor_dir(project_dir: &str) -> PathBuf {
    Path::new(project_dir).join("vyrn_vendor/sha256")
}

/// The generator cache directory (RFC-0021): `~/.vyrn/cache/gen`, overridable
/// with `VYRN_GEN_CACHE_DIR` (used by tests + air-gapped setups). Shared by the
/// CLI and the LSP so a build's generation is reused per keystroke.
pub fn gen_cache_dir() -> PathBuf {
    if let Ok(d) = std::env::var("VYRN_GEN_CACHE_DIR") {
        return PathBuf::from(d);
    }
    Path::new(&home()).join(".vyrn/cache/gen")
}

/// Read a cached generator output by content-address key (a hex sha256).
pub fn gen_cache_get(key: &str) -> Option<String> {
    std::fs::read_to_string(gen_cache_dir().join(key)).ok()
}

/// Store a generator output; failures are swallowed (the cache is optional).
pub fn gen_cache_put(key: &str, value: &str) {
    let dir = gen_cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(key), value);
}

/// Read a content-addressed blob, verifying its hash (tamper-evident).
///
/// `None` is "no copy here, look elsewhere"; `Some(Err)` is "a copy is here and
/// it is not the pinned content", which is never a reason to keep looking.
///
/// The hash check lives HERE, in the bytes core, rather than in the UTF-8
/// wrapper below: a pinned toolchain archive (RFC-0102) is a tarball, and
/// "a tampered cache fails loudly" has to stay a property of the cache rather
/// than of whichever caller happened to want text.
fn read_blob_bytes(dir: &Path, sha: &str) -> Option<Result<Vec<u8>, String>> {
    let path = dir.join(sha);
    let bytes = std::fs::read(&path).ok()?;
    if crate::hash::sha256_hex(&bytes) != sha {
        return Some(Err(format!(
            "cached copy at `{}` does not match its recorded sha256 — delete it and \
             re-fetch (or restore a good copy: any file hashing {sha} works)",
            path.display()
        )));
    }
    Some(Ok(bytes))
}

/// The UTF-8 wrapper the module readers keep using. It sits one level above the
/// bytes core rather than beside it, because the core's two call sites are the
/// two halves of [`pinned_blob_bytes`]' lookup and wrapping it twice would be
/// two answers about what a cached module is.
fn as_module_text(bytes: Vec<u8>) -> Result<String, String> {
    String::from_utf8(bytes).map_err(|_| "cached module is not UTF-8".to_string())
}

/// The pinned content of `sha`, from this project's vendor directory or the user
/// cache, hash-verified. `None` means neither holds a copy — the caller decides
/// whether that is a fetch or a refusal, and only the driver may fetch.
///
/// The verification is the point. Both readers of a cached module go through
/// here, so "a tampered cache fails loudly" is a property of the cache rather
/// than of whichever program happened to open it.
pub fn pinned_blob(project_dir: Option<&str>, sha: &str) -> Option<Result<String, String>> {
    Some(pinned_blob_bytes(project_dir, sha)?.and_then(as_module_text))
}

/// [`pinned_blob`] without the UTF-8 requirement: a pinned toolchain archive is
/// not text (RFC-0102). Same lookup, same verification.
pub fn pinned_blob_bytes(project_dir: Option<&str>, sha: &str) -> Option<Result<Vec<u8>, String>> {
    if let Some(dir) = project_dir {
        if let Some(r) = read_blob_bytes(&vendor_dir(dir), sha) {
            return Some(r);
        }
    }
    read_blob_bytes(&cache_dir(), sha)
}

pub fn write_blob(dir: &Path, sha: &str, bytes: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(sha), bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audience::{audience_of, Audience};

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vyrn-manifest-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // --- the lock: what the editor's second copy used to accept --------------

    #[test]
    fn lock_round_trips() {
        let path = tmp("lock-roundtrip").join("vyrn.lock");
        let mut lock = Lock::load(path.clone()).unwrap();
        lock.entries.insert(
            "github:a/b@v1/x.vyrn".into(),
            (
                "https://raw.githubusercontent.com/a/b/deadbeef/x.vyrn".into(),
                "abc123".into(),
            ),
        );
        lock.save().unwrap();
        let reloaded = Lock::load(path).unwrap();
        assert_eq!(lock.entries, reloaded.entries);
    }

    /// A lock file that exists and does not parse is not an unpinned project.
    /// Every one of these used to read as "this specifier was never pinned",
    /// which fetches from the network and re-pins to whatever arrives — and in
    /// the editor's own copy of this reader, every one of them still did.
    #[test]
    fn a_lock_file_that_does_not_parse_stops_the_build() {
        let dir = tmp("lock-damaged");
        let good = "github:a/b@v1/x.vyrn\thttps://x.dev/x.vyrn\tabc123\n";

        // Absent is still `Ok`: never pinned, and never claimed otherwise.
        let absent = Lock::load(dir.join("absent.lock")).unwrap();
        assert!(absent.entries.is_empty());

        // A duplicate specifier: the second line used to replace the first in
        // silence, so appending a line changed what was built while leaving the
        // reviewed line in place.
        let path = dir.join("dupe.lock");
        std::fs::write(
            &path,
            format!("{good}github:a/b@v1/x.vyrn\thttps://evil.dev/x.vyrn\tdef456\n"),
        )
        .unwrap();
        let e = Lock::load(path).unwrap_err();
        assert!(e.contains("dupe.lock:2"), "names the line: {e}");
        assert!(e.contains("github:a/b@v1/x.vyrn"), "names the pin: {e}");

        // Tabs turned to spaces by an editor, a merge tool or a CI checkout.
        let path = dir.join("spaces.lock");
        std::fs::write(&path, good.replace('\t', " ")).unwrap();
        let e = Lock::load(path).unwrap_err();
        assert!(e.contains("spaces.lock:1"), "{e}");

        // A truncated write.
        let path = dir.join("cut.lock");
        std::fs::write(&path, "github:a/b@v1/x.vyrn\thttps://x.dev\n").unwrap();
        assert!(Lock::load(path).is_err());

        // A blank line is not damage.
        let path = dir.join("blank.lock");
        std::fs::write(&path, format!("{good}\n")).unwrap();
        assert_eq!(Lock::load(path).unwrap().entries.len(), 1);
    }

    /// The write side of the same invariant: a field carrying the separator is a
    /// line the reader would split differently from the writer, and specifiers
    /// come from source strings the compiler does not constrain.
    #[test]
    fn a_specifier_carrying_a_separator_is_never_written() {
        let path = tmp("lock-separator").join("vyrn.lock");
        let mut lock = Lock::load(path.clone()).unwrap();
        lock.entries.insert(
            "https://x.dev/a.vyrn\tb\tc".into(),
            ("https://x.dev/a.vyrn".into(), "abc123".into()),
        );
        assert!(lock.save().is_err(), "a tab in a specifier is not writable");
        assert!(!path.is_file(), "and nothing was written");
    }

    // --- the cache: the guarantee remote.rs's module doc states --------------

    #[test]
    fn a_blob_whose_bytes_do_not_hash_to_its_name_is_refused() {
        let d = tmp("blob-tamper");
        let sha = crate::hash::sha256_hex(b"the reviewed module");
        std::fs::write(d.join(&sha), b"something else entirely").unwrap();
        let r = read_blob_bytes(&d, &sha).expect("a copy is there");
        let e = r.unwrap_err();
        assert!(e.contains("does not match its recorded sha256"), "{e}");
    }

    /// The split RFC-0102 M1 needed: the core returns bytes (a pinned toolchain
    /// archive is a tarball), and text is what the module readers ask of it.
    #[test]
    fn a_good_blob_reads_back_as_bytes_and_as_text() {
        let d = tmp("blob-good");
        let sha = crate::hash::sha256_hex(b"fn main() {}");
        std::fs::write(d.join(&sha), b"fn main() {}").unwrap();
        let bytes = read_blob_bytes(&d, &sha).unwrap().unwrap();
        assert_eq!(bytes, b"fn main() {}");
        assert_eq!(as_module_text(bytes).unwrap(), "fn main() {}");

        // Not-UTF-8 is the wrapper's verdict, never the core's: the same bytes
        // read fine, and only the text reader refuses them.
        let sha = crate::hash::sha256_hex(&[0xff, 0xfe]);
        std::fs::write(d.join(&sha), [0xff, 0xfe]).unwrap();
        let bytes = read_blob_bytes(&d, &sha).unwrap().unwrap();
        assert_eq!(bytes, vec![0xff, 0xfe]);
        assert!(as_module_text(bytes).unwrap_err().contains("not UTF-8"));
    }

    // --- the manifest: one canonicalization, at one point in the sequence ----

    /// The audience base is the project directory AS THE FILESYSTEM NAMES IT,
    /// whatever spelling reached the reader — and it is canonical BEFORE the
    /// entry points are joined onto it, because `with_realpath` can only repair
    /// an entry whose file already exists.
    ///
    /// The editor and the build reach a project by different spellings: one from
    /// the URI a client sent, one from the shell. One project has one base, or a
    /// module is server-only to the checker and universal to the editor.
    #[test]
    fn the_audience_base_is_canonical_however_the_directory_was_spelled() {
        let d = tmp("audience-base");
        std::fs::create_dir_all(d.join("server")).unwrap();
        std::fs::write(d.join("server/store.vyrn"), "fn f() {}").unwrap();
        let doc = crate::schema::parse_json(
            r#"{"audience":{"server":["server"]},"client":"client/boot.vyrn"}"#,
        )
        .unwrap();

        let canon = real_path(&d.to_string_lossy().replace('\\', "/")).unwrap();
        let m = from_doc(doc, format!("{canon}/server/.."))
            .unwrap()
            .audience
            .unwrap();

        assert_eq!(m.base, canon, "the base is what the filesystem calls it");
        assert_eq!(
            m.entries[0].0,
            format!("{canon}/client/boot.vyrn"),
            "an entry point that is not written yet still hangs off that base"
        );
        let key = format!("{canon}/server/store.vyrn");
        assert_eq!(audience_of(&key, &m).audience, Audience::Server);
    }

    /// RFC-0103 M1: an artifact's entry point is the same path an audience entry
    /// point is, so it hangs off the same canonical base — and a manifest that
    /// contradicts itself about one stops the read, on the channel an
    /// unparseable manifest already travels.
    #[test]
    fn artifacts_hang_off_the_same_base_and_a_contradiction_stops_the_read() {
        let d = tmp("artifacts-base");
        std::fs::create_dir_all(d.join("server")).unwrap();
        let canon = real_path(&d.to_string_lossy().replace('\\', "/")).unwrap();
        let parse = |src: &str| crate::schema::parse_json(src).unwrap();

        let m = from_doc(
            parse(
                r#"{"client":"client/boot.vyrn",
                    "artifacts":{"api":{"entry":"server/main.vyrn","target":"native"}}}"#,
            ),
            format!("{canon}/server/.."),
        )
        .unwrap();
        let a = m.artifacts.unwrap();
        assert_eq!(a.base, canon, "the base is what the filesystem calls it");
        assert_eq!(a.list[0].entry, format!("{canon}/client/boot.vyrn"));
        assert_eq!(a.list[1].entry, format!("{canon}/server/main.vyrn"));

        // A manifest with neither declares nothing, exactly as with `audience`.
        assert!(from_doc(parse(r#"{"name":"x"}"#), canon.clone())
            .unwrap()
            .artifacts
            .is_none());

        let e = from_doc(
            parse(r#"{"artifacts":{"app":{"entry":"x.vyrn","target":"wasm"}}}"#),
            canon.clone(),
        )
        .err()
        .expect("a target nobody can build for is not a manifest this reads");
        assert!(e.contains("unknown target `wasm`"), "{e}");

        // RFC-0102 M1: a tool nothing can resolve is the same kind of
        // contradiction, on the same channel — a `toolchain` key that declared
        // nothing would leave the build on PATH with the file saying otherwise.
        let m = from_doc(
            parse(r#"{"toolchain":{"wasmtime":"46.0.1","wasi-sysroot":"25.0"}}"#),
            canon.clone(),
        )
        .unwrap();
        assert_eq!(
            m.toolchain,
            vec![
                ("wasmtime".to_string(), "46.0.1".to_string()),
                ("wasi-sysroot".to_string(), "25.0".to_string())
            ]
        );
        assert!(from_doc(parse(r#"{"main":"m.vyrn"}"#), canon.clone())
            .unwrap()
            .toolchain
            .is_empty());
        let e = from_doc(parse(r#"{"toolchain":{"wasmtimee":"46.0.1"}}"#), canon)
            .err()
            .expect("a tool the table cannot resolve is not a manifest this reads");
        assert!(e.contains("unknown tool `wasmtimee`"), "{e}");
        assert!(e.contains("wasmtime, wasi-sysroot, wasi-builtins"), "{e}");
    }
}
