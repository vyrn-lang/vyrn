//! The project context on disk: `vyrn.json`, `vyrn.lock`, the caches beside
//! them, and the `std/` and `web/` roots a toolchain binary walks up to find.
//!
//! The `vyrn` driver and the language server both read a project, and a second
//! copy of this reader drifted (it skipped hash checks and accepted a lock the
//! build refuses), so the one reader lives in the crate both depend on. The
//! network stays out: fetching, resolving a floating ref and writing the lock
//! back are the driver's, and the editor never does them. The compiler proper
//! never reaches this module.

use crate::schema::Json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Returns what the filesystem calls `path`: the one canonicalization that
/// decides file identity. A second spelling (another case on
/// Windows, a junction) must not make one file two audiences. `None` for a
/// path the OS cannot resolve: a remote key, an in-memory module, a missing
/// file.
pub fn real_path(path: &str) -> Option<String> {
    let p = Path::new(path).canonicalize().ok()?;
    Some(dos_to_slash(&p.to_string_lossy()))
}

/// Returns a canonical Windows path slash-separated and prefix-free.
/// `canonicalize` yields `\\?\C:\..` for a drive and `\\?\UNC\server\share\..`
/// for a network location, which spells back to `//server/share/..`, the form
/// the rest of the toolchain uses.
pub fn dos_to_slash(s: &str) -> String {
    let stripped = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!("//{rest}")
    } else {
        s.trim_start_matches(r"\\?\").to_string()
    };
    stripped.replace('\\', "/")
}

/// Names the kind of a JSON value, for a diagnostic.
fn json_kind(v: &Json) -> &'static str {
    match v {
        Json::Null => "null",
        Json::Bool(_) => "a boolean",
        Json::Num(_) => "a number",
        Json::Str(_) => "a string",
        Json::Arr(_) => "an array",
        Json::Obj(_) => "an object",
    }
}

/// Returns a directory beside the executable's repo root: `$var` if it names
/// one that exists, else `name/` found by walking up at most five levels. The
/// bundled LSP (`<repo>/editor/vscode/server/`) and dev builds
/// (`<repo>/compiler/target/<profile>/`) are within five levels of the root.
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
/// `None` is an error only for a program that imports `std/...`.
pub fn std_root() -> Option<String> {
    root_near_exe("VYRN_STD", "std")
}

/// The `web/` root holding the browser runtimes (`wasi-min.js`, `vyrn-rpc.js`,
/// `vyrn-query.js`): `$VYRN_WEB`, or `web/` beside the executable's repo.
pub fn web_root() -> Option<String> {
    root_near_exe("VYRN_WEB", "web")
}

/// The project manifest, `vyrn.json`. Every field is optional; unknown keys are
/// ignored.
pub struct Manifest {
    /// Directory the manifest lives in (slash-separated), as walked up to.
    pub dir: String,
    /// The parsed document. Every rule the manifest carries (`audience`, `roles`,
    /// `dependencies`, the rewrite `vyrn add` performs) reads this one parse, so
    /// no second reader can get a different answer.
    pub doc: Json,
    pub main: Option<String>,
    pub dependencies: Vec<(String, String)>,
    /// The `toolchain` object: tool name to version string, empty when
    /// absent. A tool is a dependency, resolved through the same lock, cache and
    /// `--offline`.
    pub toolchain: Vec<(String, String)>,
    /// The declared audience vocabulary; `None` without an `audience`
    /// key, which leaves every module universal.
    pub audience: Option<crate::audience::AudienceMap>,
    /// What the project builds: the `artifacts` map plus the `main`,
    /// `server` and `client` keys that are sugar for it; `None` when it declares
    /// neither. It shares the audience base and identity function, because an
    /// artifact entry and an audience entry are the same paths.
    pub artifacts: Option<crate::artifacts::ArtifactMap>,
    /// The `nativeTarget` key as written, so a diagnostic can quote it.
    pub native_target: Option<String>,
}

/// Returns the nearest directory at or above `start` that holds `vyrn.json`.
/// The one walk of the project-root rule: one marker, a stat per level, up to
/// the filesystem root, no depth limit.
fn nearest_manifest_dir(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|d| d.join("vyrn.json").is_file())
        .map(Path::to_path_buf)
}

/// Finds `vyrn.json` by walking up from the directory `start`.
///
/// `Ok(None)`: the project declares nothing. `Ok(Some)`: what it declares.
/// `Err`: it declares something unreadable. An unreadable policy is not the
/// empty policy; a trailing comma must not switch the audience boundary off.
pub fn find(start: &Path) -> Result<Option<Manifest>, String> {
    let Some(dir) = nearest_manifest_dir(start) else {
        return Ok(None);
    };
    let candidate = dir.join("vyrn.json");
    let text = std::fs::read_to_string(&candidate)
        .map_err(|e| format!("cannot read {}: {e}", candidate.display()))?;
    let doc = crate::schema::parse_json(&text)
        .map_err(|e| format!("{} is not valid JSON: {e}", candidate.display()))?;
    let slash_dir = dir.to_string_lossy().replace('\\', "/");
    from_doc(doc, slash_dir).map(Some)
}

/// Returns the project root for the directory `start`: the directory of the
/// nearest `vyrn.json`, else `start`. It walks as [`find`] does, so the editor,
/// `vyrn why --contract` and the build name one root.
pub fn app_root(start: &Path) -> PathBuf {
    nearest_manifest_dir(start).unwrap_or_else(|| start.to_path_buf())
}

/// Builds a [`Manifest`] from a parsed document rooted at `slash_dir`: the
/// one place the audience base is formed, testable without a file. `Err` is a
/// contradictory declaration (an artifact), which travels with an unparseable
/// manifest because it means the same: a rule that cannot be read.
fn from_doc(doc: Json, slash_dir: String) -> Result<Manifest, String> {
    let str_key = |k: &str| match doc.get(k) {
        Some(Json::Str(s)) => Some(s.clone()),
        _ => None,
    };
    // A present `dependencies` or `toolchain` that is not an object of strings is
    // refused, not read as empty; empty would surface later as an unrelated
    // "cannot resolve import".
    let str_map = |k: &str| -> Result<Vec<(String, String)>, String> {
        match doc.get(k) {
            None => Ok(Vec::new()),
            Some(Json::Obj(entries)) => {
                let mut out = Vec::new();
                for (name, v) in entries {
                    match v {
                        Json::Str(s) => out.push((name.clone(), s.clone())),
                        other => {
                            return Err(format!(
                                "`{k}.{name}` must be a string, found {} — vyrn.json's \
                                 `{k}` maps names to version strings",
                                json_kind(other)
                            ))
                        }
                    }
                }
                Ok(out)
            }
            Some(other) => Err(format!(
                "`{k}` must be an object of strings, found {}",
                json_kind(other)
            )),
        }
    };
    let dependencies = str_map("dependencies")?;
    // An unknown tool name is refused where it is written: a typo that declared
    // nothing would leave the build on PATH, a silent fallback.
    let toolchain = str_map("toolchain")?;
    for (name, _) in &toolchain {
        if !crate::toolpin::KNOWN_TOOLS.contains(&name.as_str()) {
            return Err(crate::toolpin::unknown_tool(name));
        }
    }
    // The base is the project directory as the filesystem names it; an empty
    // walk-up result is the working directory. It is canonical before the entry
    // points join it, because `with_realpath` repairs only an existing file.
    let audience_base = real_path(if slash_dir.is_empty() {
        "."
    } else {
        &slash_dir
    })
    .unwrap_or_else(|| slash_dir.clone());
    let audience =
        crate::audience::from_manifest(&doc, &audience_base).map(|m| m.with_realpath(real_path));
    // Artifact entry points join the same base: `client` names one file for both
    // rules.
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

/// Returns the modules role discovery reads, as `(slash path,
/// source)` pairs: the manifest's entry points and every `.vyrn` directly in
/// the app directory.
///
/// Generator imports live in an app's root modules, never in a page, so a
/// shallow scan suffices. `doc` is the manifest the caller already read: a
/// second read could answer "no entry points" about a file the first refused.
pub fn role_roots(app_dir: &Path, doc: Option<&Json>) -> Vec<(String, String)> {
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(doc) = doc {
        for key in ["main", "server", "client"] {
            if let Some(Json::Str(p)) = doc.get(key) {
                paths.push(app_dir.join(p));
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(app_dir) {
        for e in entries.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("vyrn") {
                paths.push(p);
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .filter_map(|p| {
            let src = std::fs::read_to_string(&p).ok()?;
            Some((p.to_string_lossy().replace('\\', "/"), src))
        })
        .collect()
}

/// `vyrn.lock`: `specifier<TAB>resolved-url<TAB>sha256` per line, sorted by
/// specifier.
#[derive(Debug)]
pub struct Lock {
    pub path: PathBuf,
    pub entries: BTreeMap<String, (String, String)>,
    pub dirty: bool,
    /// `entries` as read: [`Lock::save`] applies only what changed since.
    loaded: BTreeMap<String, (String, String)>,
}

impl Lock {
    /// Reads the lock, or names the line that stops it.
    ///
    /// A damaged lock (tabs turned to spaces, a truncated write, a bad merge) is
    /// not an unpinned project: an unpinned specifier is fetched and re-pinned to
    /// whatever arrives. A missing file is `Ok`: never pinned.
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
            // Two pins for one specifier would let an appended line change the build
            // while the reviewed line stays in place.
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
            loaded: entries.clone(),
            entries,
            dirty: false,
        })
    }

    /// Returns the lock beside the manifest in `project_dir`.
    pub fn in_project(project_dir: &str) -> Result<Lock, String> {
        Lock::load(Path::new(project_dir).join("vyrn.lock"))
    }

    /// Writes this lock's changes over the file as it is now, so a pin another
    /// process saved since [`Lock::load`] survives, and replaces the file whole.
    pub fn save(&self) -> Result<(), String> {
        let dir = self.path.parent().filter(|d| !d.as_os_str().is_empty());
        let _held = lock_project_files(dir.unwrap_or(Path::new(".")))?;
        let mut merged = Lock::load(self.path.clone())?.entries;
        merged.retain(|spec, _| self.entries.contains_key(spec) || !self.loaded.contains_key(spec));
        for (spec, pin) in &self.entries {
            if self.loaded.get(spec) != Some(pin) {
                merged.insert(spec.clone(), pin.clone());
            }
        }
        let mut out = String::new();
        for (spec, (url, sha)) in &merged {
            // A tab or newline in a field would split differently on read. Specifiers
            // come from unconstrained source strings, so refuse them here.
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
        write_whole(&self.path, out.as_bytes()).map_err(|e| e.to_string())
    }
}

fn home() -> String {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string())
}

/// Takes this machine's lock on the `vyrn.lock` and `vyrn.json` of
/// `project_dir`, waiting while another process holds it. The lock goes with
/// the returned file. Hold it across one reload, merge and rename, never
/// across a build.
pub fn lock_project_files(project_dir: &Path) -> Result<std::fs::File, String> {
    let file = project_lock_file(project_dir, "files")?;
    file.lock()
        .map_err(|e| format!("cannot lock {}: {e}", project_dir.display()))?;
    Ok(file)
}

/// Takes the lock `vyrn dev` holds on `project_dir` for its life, or `None`
/// while another process holds it. Its own file, not [`lock_project_files`]'s:
/// a lock held for a server's life would stop every build's save.
pub fn try_lock_dev(project_dir: &Path) -> Result<Option<std::fs::File>, String> {
    let file = project_lock_file(project_dir, "dev")?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => {
            Err(format!("cannot lock {}: {e}", project_dir.display()))
        }
    }
}

/// Where the holder of [`try_lock_dev`] records its port. Not in the locked
/// file: on Windows another process cannot read a locked range.
pub fn dev_port_file(project_dir: &Path) -> PathBuf {
    project_lock_path(project_dir, "port")
}

/// Opens `~/.vyrn/locks/<hash of the canonical directory>.<what>`: one file
/// per project and purpose, outside the project, so a checkout gains no file.
/// An OS lock on it goes with a killed holder.
fn project_lock_file(project_dir: &Path, what: &str) -> Result<std::fs::File, String> {
    let path = project_lock_path(project_dir, what);
    path.parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            let mut open = std::fs::OpenOptions::new();
            open.create(true).truncate(false).write(true).open(&path)
        })
        .map_err(|e| format!("cannot open the project lock {}: {e}", path.display()))
}

fn project_lock_path(project_dir: &Path, what: &str) -> PathBuf {
    let key = std::fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    let name = crate::hash::sha256_hex(key.to_string_lossy().as_bytes());
    Path::new(&home()).join(format!(".vyrn/locks/{name}.{what}"))
}

/// How far a filesystem's mtime can trail the clock: the coarsest common
/// timestamp (FAT's 2 s; HFS+ keeps 1 s, Linux a scheduler tick). A file whose
/// mtime lies within it of a check can change again without moving its mtime.
pub const MTIME_SLACK: std::time::Duration = std::time::Duration::from_secs(2);

/// The user's module cache: `~/.vyrn/cache/sha256/<hex>`.
pub fn cache_dir() -> PathBuf {
    Path::new(&home()).join(".vyrn/cache/sha256")
}

/// The native route's cache: `~/.vyrn/cache/native`. Each subdirectory holds
/// the objects and the precompiled header of one toolchain and flag set.
pub fn native_cache_dir() -> PathBuf {
    Path::new(&home()).join(".vyrn/cache/native")
}

/// A project's committed, air-gapped copy of the same blobs.
pub fn vendor_dir(project_dir: &str) -> PathBuf {
    Path::new(project_dir).join("vyrn_vendor/sha256")
}

/// The generator cache directory: `~/.vyrn/cache/gen`, or
/// `VYRN_GEN_CACHE_DIR`. The CLI and the LSP share it, so a build's generation
/// is reused per keystroke.
pub fn gen_cache_dir() -> PathBuf {
    if let Ok(d) = std::env::var("VYRN_GEN_CACHE_DIR") {
        return PathBuf::from(d);
    }
    Path::new(&home()).join(".vyrn/cache/gen")
}

/// Where `vyrn run --profile` saves the counts of its last run: `~/.vyrn/cache/profile`, or
/// `VYRN_PROFILE_DIR`. One file per root.
pub fn profile_dir() -> PathBuf {
    if let Ok(d) = std::env::var("VYRN_PROFILE_DIR") {
        return PathBuf::from(d);
    }
    Path::new(&home()).join(".vyrn/cache/profile")
}

/// Reads a cached generator output by its hex sha256 key.
pub fn gen_cache_get(key: &str) -> Option<String> {
    std::fs::read_to_string(gen_cache_dir().join(key)).ok()
}

/// Stores a generator output; a failure is ignored, as the cache is optional.
pub fn gen_cache_put(key: &str, value: &str) {
    let dir = gen_cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = write_whole(&dir.join(key), value.as_bytes());
}

/// Writes `bytes` to `path` so that a concurrent reader sees the old file or
/// the new one, never a part: every running compiler shares the caches.
///
/// The bytes go to a name no other writer uses, then a rename replaces
/// `path`. On a failed rename the staged file is removed.
pub fn write_whole(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let stage = stage_beside(path);
    std::fs::write(&stage, bytes)?;
    std::fs::rename(&stage, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&stage);
    })
}

/// A name beside `path` that no other writer, process or thread, uses. It
/// keeps `path`'s extension last, so a linker that reads it writes the same
/// kind of file.
pub fn stage_beside(path: &Path) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut name = path.file_stem().unwrap_or_default().to_os_string();
    name.push(format!(".{}.{seq}.tmp", std::process::id()));
    if let Some(ext) = path.extension() {
        name.push(".");
        name.push(ext);
    }
    path.with_file_name(name)
}

/// Replaces the user's file at `path` with `bytes` if it still holds `read`,
/// the bytes the caller read from it.
///
/// The write goes to the canonical path, so a symlink keeps pointing at the
/// file, through a staged file beside it with the target's permissions, so a
/// reader sees the old bytes or the new ones.
///
/// # Errors
///
/// The file changed since the caller read it (another writer, such as an
/// editor's save, came between), or an I/O step failed. Either way nothing is
/// written. The check and the rename are two steps: a write between them is
/// lost, and no lock can stop a writer that takes none.
pub fn rewrite_whole(path: &Path, read: &[u8], bytes: &[u8]) -> Result<(), String> {
    let shown = path.display();
    let target = std::fs::canonicalize(path).map_err(|e| format!("cannot resolve {shown}: {e}"))?;
    let perms = std::fs::metadata(&target)
        .map_err(|e| format!("cannot read {shown}: {e}"))?
        .permissions();
    let stage = stage_beside(&target);
    let staged =
        std::fs::write(&stage, bytes).and_then(|()| std::fs::set_permissions(&stage, perms));
    let placed = match staged {
        Err(e) => Err(format!("cannot write {shown}: {e}")),
        Ok(()) if std::fs::read(&target).ok().as_deref() != Some(read) => Err(format!(
            "{shown} changed after it was read; nothing was written, run the command again"
        )),
        Ok(()) => {
            std::fs::rename(&stage, &target).map_err(|e| format!("cannot write {shown}: {e}"))
        }
    };
    if placed.is_err() {
        let _ = std::fs::remove_file(&stage);
    }
    placed
}

/// Reads a content-addressed blob and verifies its hash.
///
/// `None`: no copy here, look elsewhere. `Some(Err)`: a copy that is not the
/// pinned content, which is never a reason to keep looking. The check sits in
/// the bytes core because a pinned toolchain archive is a tarball.
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

/// Decodes a verified blob as a module's UTF-8 text.
fn as_module_text(bytes: Vec<u8>) -> Result<String, String> {
    String::from_utf8(bytes).map_err(|_| "cached module is not UTF-8".to_string())
}

/// Returns the pinned content of `sha` from the project's vendor directory or
/// the user cache, hash-verified. `None`: neither holds a copy; only the
/// driver may fetch.
pub fn pinned_blob(project_dir: Option<&str>, sha: &str) -> Option<Result<String, String>> {
    Some(pinned_blob_bytes(project_dir, sha)?.and_then(as_module_text))
}

/// [`pinned_blob`] without the UTF-8 requirement, for a pinned toolchain
/// archive.
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
    write_whole(&dir.join(sha), bytes).map_err(|e| e.to_string())
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

    /// The project root is the nearest `vyrn.json` however deep the start is,
    /// else the start itself.
    #[test]
    fn the_project_root_is_the_nearest_manifest_at_any_depth() {
        let d = tmp("approot");
        let deep = (0..12).fold(d.clone(), |p, i| p.join(format!("d{i}")));
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(app_root(&deep), deep, "no vyrn.json: the start directory");
        std::fs::write(d.join("vyrn.json"), "{}").unwrap();
        assert_eq!(app_root(&deep), d, "twelve levels up");
        let found = find(&deep).unwrap().unwrap();
        assert_eq!(found.dir, d.to_string_lossy().replace('\\', "/"));
        let inner = d.join("d0");
        std::fs::write(inner.join("vyrn.json"), "{}").unwrap();
        assert_eq!(app_root(&deep), inner, "the nearest wins");
    }

    /// A reader racing a writer on one cache entry reads a whole version. A
    /// write in place let a parallel `vyrn check` read an empty or cut entry,
    /// which it reports as foreign (record `0125-m11-gencache`).
    #[test]
    fn a_reader_racing_a_writer_reads_a_whole_entry() {
        const LEN: usize = 4 << 20;
        let path = tmp("write-whole").join("entry");
        let versions = [vec![b'a'; LEN], vec![b'b'; LEN]];
        write_whole(&path, &versions[0]).unwrap();
        let done = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| {
                for i in 0..100 {
                    // A rename can lose to the reader's open on Windows; the
                    // cache ignores that, and so does this writer.
                    let _ = write_whole(&path, &versions[i % 2]);
                }
                done.store(true, std::sync::atomic::Ordering::Relaxed);
            });
            let mut reads = 0;
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok(got) = std::fs::read(&path) {
                    assert!(
                        versions.contains(&got),
                        "read {} bytes, not a whole version",
                        got.len()
                    );
                    reads += 1;
                }
            }
            assert!(reads > 0);
        });
    }

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

    #[test]
    fn a_save_keeps_the_pin_another_process_saved_since_the_load() {
        let path = tmp("lock-merge").join("vyrn.lock");
        let pin = |sha: &str| ("https://x.dev/m.vyrn".to_string(), sha.to_string());
        let mut first = Lock::load(path.clone()).unwrap();
        let mut second = Lock::load(path.clone()).unwrap();
        first
            .entries
            .insert("https://x.dev/a.vyrn".into(), pin("aa"));
        first.save().unwrap();
        second
            .entries
            .insert("https://x.dev/b.vyrn".into(), pin("bb"));
        second.save().unwrap();
        let on_disk = Lock::load(path).unwrap().entries;
        assert_eq!(
            on_disk.keys().collect::<Vec<_>>(),
            ["https://x.dev/a.vyrn", "https://x.dev/b.vyrn"]
        );
    }

    #[test]
    fn a_project_lock_excludes_a_second_holder_until_it_drops() {
        let dir = tmp("project-lock");
        let held = lock_project_files(&dir).unwrap();
        let other = project_lock_file(&dir, "files").unwrap();
        assert!(matches!(
            other.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(held);
        assert!(other.try_lock().is_ok());
    }

    #[test]
    fn a_rewrite_refuses_a_file_that_changed_after_the_read() {
        let path = tmp("rewrite-changed").join("a.vyrn");
        std::fs::write(&path, "saved by the editor").unwrap();
        let e = rewrite_whole(&path, b"read by fmt", b"formatted").unwrap_err();
        assert!(e.contains("changed after it was read"), "{e}");
        assert_eq!(std::fs::read(&path).unwrap(), b"saved by the editor");
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1,
            "no staged file is left"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_rewrite_keeps_the_symlink_and_the_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp("rewrite-link");
        let (real, link) = (dir.join("real.vyrn"), dir.join("link.vyrn"));
        std::fs::write(&real, "old").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        rewrite_whole(&link, b"old", b"new").unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read(&real).unwrap(), b"new");
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    /// A damaged lock stops the build instead of reading as unpinned.
    #[test]
    fn a_lock_file_that_does_not_parse_stops_the_build() {
        let dir = tmp("lock-damaged");
        let good = "github:a/b@v1/x.vyrn\thttps://x.dev/x.vyrn\tabc123\n";

        // Absent is `Ok`: never pinned.
        let absent = Lock::load(dir.join("absent.lock")).unwrap();
        assert!(absent.entries.is_empty());

        // A duplicate specifier.
        let path = dir.join("dupe.lock");
        std::fs::write(
            &path,
            format!("{good}github:a/b@v1/x.vyrn\thttps://evil.dev/x.vyrn\tdef456\n"),
        )
        .unwrap();
        let e = Lock::load(path).unwrap_err();
        assert!(e.contains("dupe.lock:2"), "names the line: {e}");
        assert!(e.contains("github:a/b@v1/x.vyrn"), "names the pin: {e}");

        // Tabs turned to spaces.
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

    /// A field carrying the separator is never written.
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

    #[test]
    fn a_blob_whose_bytes_do_not_hash_to_its_name_is_refused() {
        let d = tmp("blob-tamper");
        let sha = crate::hash::sha256_hex(b"the reviewed module");
        std::fs::write(d.join(&sha), b"something else entirely").unwrap();
        let r = read_blob_bytes(&d, &sha).expect("a copy is there");
        let e = r.unwrap_err();
        assert!(e.contains("does not match its recorded sha256"), "{e}");
    }

    /// The core returns bytes (a toolchain archive is a tarball); text is what
    /// module readers ask for.
    #[test]
    fn a_good_blob_reads_back_as_bytes_and_as_text() {
        let d = tmp("blob-good");
        let sha = crate::hash::sha256_hex(b"fn main() {}");
        std::fs::write(d.join(&sha), b"fn main() {}").unwrap();
        let bytes = read_blob_bytes(&d, &sha).unwrap().unwrap();
        assert_eq!(bytes, b"fn main() {}");
        assert_eq!(as_module_text(bytes).unwrap(), "fn main() {}");

        // Not UTF-8 is the wrapper's verdict, not the core's.
        let sha = crate::hash::sha256_hex(&[0xff, 0xfe]);
        std::fs::write(d.join(&sha), [0xff, 0xfe]).unwrap();
        let bytes = read_blob_bytes(&d, &sha).unwrap().unwrap();
        assert_eq!(bytes, vec![0xff, 0xfe]);
        assert!(as_module_text(bytes).unwrap_err().contains("not UTF-8"));
    }

    /// The audience base is the project directory as the filesystem names it,
    /// however the directory was spelled, and is canonical before the entry points
    /// join it. The editor and the build reach a project by different spellings.
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
        assert_eq!(audience_of(&key, &m, None).audience, Audience::Server);
    }

    /// An artifact entry hangs off the audience base, and a contradictory
    /// manifest stops the read.
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

        // A manifest with neither declares nothing, as with `audience`.
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

        // An unknown tool stops the read the same way.
        let m = from_doc(
            parse(r#"{"toolchain":{"wasmtime":"46.0.1","simde":"0.8.2"}}"#),
            canon.clone(),
        )
        .unwrap();
        assert_eq!(
            m.toolchain,
            vec![
                ("wasmtime".to_string(), "46.0.1".to_string()),
                ("simde".to_string(), "0.8.2".to_string())
            ]
        );
        assert!(from_doc(parse(r#"{"main":"m.vyrn"}"#), canon.clone())
            .unwrap()
            .toolchain
            .is_empty());
        let e = from_doc(
            parse(r#"{"toolchain":{"wasmtimee":"46.0.1"}}"#),
            canon.clone(),
        )
        .err()
        .expect("a tool the table cannot resolve is not a manifest this reads");
        assert!(e.contains("unknown tool `wasmtimee`"), "{e}");
        assert!(
            e.contains("wasmtime, cargo-nextest, wabt, simde, binaryen"),
            "{e}"
        );
        // The two wasi-sdk tools left the table with the C shim: nothing reads them.
        for gone in ["wasi-sysroot", "wasi-builtins"] {
            let doc = format!(r#"{{"toolchain":{{"{gone}":"25.0"}}}}"#);
            let e = from_doc(parse(&doc), canon.clone())
                .err()
                .expect("a tool nothing reads is not a manifest this reads");
            assert!(e.contains(&format!("unknown tool `{gone}`")), "{e}");
        }
    }

    /// A present but mistyped map is refused, not read as no dependencies.
    #[test]
    fn a_mistyped_dependencies_or_toolchain_value_stops_the_read() {
        let parse = |src: &str| crate::schema::parse_json(src).unwrap();
        let e = from_doc(
            parse(r#"{"dependencies":{"pad":{"ref":"v1"}}}"#),
            String::new(),
        )
        .err()
        .expect("an object-of-objects dependencies is not silently nothing");
        assert!(
            e.contains("`dependencies.pad`") && e.contains("must be a string"),
            "{e}"
        );
        let e = from_doc(parse(r#"{"dependencies":["pad"]}"#), String::new())
            .err()
            .expect("an array of names declares nothing readable");
        assert!(
            e.contains("`dependencies` must be an object of strings, found an array"),
            "{e}"
        );
        let e = from_doc(parse(r#"{"toolchain":{"wasmtime":46}}"#), String::new())
            .err()
            .expect("a number is not a version string");
        assert!(
            e.contains("`toolchain.wasmtime`") && e.contains("found a number"),
            "{e}"
        );
    }

    /// A `\\?\UNC\` path spells back to `//server/share`, or audience and artifact
    /// rules would miss share-hosted projects.
    #[test]
    fn a_verbatim_unc_path_spells_back_to_its_share_form() {
        assert_eq!(
            dos_to_slash(r"\\?\UNC\server\share\proj"),
            "//server/share/proj"
        );
        assert_eq!(dos_to_slash(r"\\?\C:\repo\proj"), "C:/repo/proj");
        assert_eq!(dos_to_slash("/already/slashy"), "/already/slashy");
    }
}
