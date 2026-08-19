//! Reproducible remote imports (RFC-0010 M4) — the CLI half.
//!
//! The frontend treats `github:` / `gist:` / `https:` specifiers as opaque
//! module keys; this module turns them into content with three guarantees:
//!
//!   * **Pinned**: every fetch is recorded in `vyrn.lock`
//!     (`specifier ⇥ resolved-immutable-url ⇥ sha256`, sorted, tab-separated).
//!     Once locked, only `vyrn update` changes an entry. Floating refs
//!     (`@main`, `@v1`) are resolved to a commit once, then frozen.
//!   * **Content-addressed**: bytes live in `~/.vyrn/cache/sha256/<hex>`
//!     (and optionally `./vyrn_vendor/sha256/<hex>` for committed, air-gapped
//!     repos). The hash is verified on EVERY load — a tampered cache fails
//!     loudly, and any copy of the file obtained anywhere can restore a
//!     vanished upstream (the left-pad scenario).
//!   * **Offline-capable**: `--offline` / `VYRN_OFFLINE=1` forbids network;
//!     a lock+cache hit needs none.
//!
//! Zero new crates: SHA-256 is `vyrn_frontend::hash` (FIPS 180-4, tested against
//! NIST vectors), fetching shells out to `curl -sL --fail`, and git refs
//! resolve via `git ls-remote` — both tools are ubiquitous.
//!
//! What is HERE is the network: resolving a floating ref, fetching, pinning.
//! What is not is reading the pin and the cache — that is
//! [`vyrn_frontend::manifest`], because the editor reads them too and must reach
//! the same verdict without ever reaching the network.

use std::cell::RefCell;
use std::process::Command;

// The lock, the caches and the content-addressed blob read live in
// `vyrn_frontend::manifest`, because the LSP reads them too and a second reader
// of a pin is a second answer about what is pinned. Re-exported here so this
// module still reads as the one place remote imports are handled.
pub use vyrn_frontend::hash::sha256_hex;
pub use vyrn_frontend::manifest::{
    cache_dir, gen_cache_get, gen_cache_put, pinned_blob, vendor_dir, write_blob, Lock,
};

/// List the entry names directly under `dir` (generation-time `listDir`,
/// RFC-0021), sorted for determinism.
pub fn list_dir(dir: &str) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(dir).map_err(|_| vyrn_frontend::trap::io_at("listerr", dir))?;
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Ok(names)
}

// ---------------------------------------------------------------------------
// specifier resolution + fetching
// ---------------------------------------------------------------------------

/// Turn a remote specifier into an immutable URL. Floating github refs are
/// pinned to a commit via `git ls-remote` (network!); 40-hex refs and https
/// URLs are already immutable.
pub fn resolve_to_url(spec: &str) -> Result<String, String> {
    if let Some(rest) = spec.strip_prefix("github:") {
        // github:owner/repo@ref/path(.vyrn)
        let at = rest.find('@').ok_or("github specifier needs `@ref`")?;
        let (owner_repo, rest) = rest.split_at(at);
        let rest = &rest[1..];
        let slash = rest.find('/').ok_or("github specifier needs a file path")?;
        let (r, path) = rest.split_at(slash);
        let sha = if r.len() == 40 && r.bytes().all(|b| b.is_ascii_hexdigit()) {
            r.to_string()
        } else {
            let out = Command::new("git")
                .args(["ls-remote", &format!("https://github.com/{owner_repo}"), r])
                .output()
                .map_err(|e| format!("cannot run git ls-remote: {e}"))?;
            let text = String::from_utf8_lossy(&out.stdout);
            text.split_whitespace()
                .next()
                .filter(|s| s.len() == 40)
                .ok_or_else(|| format!("cannot resolve ref `{r}` in github.com/{owner_repo}"))?
                .to_string()
        };
        return Ok(format!(
            "https://raw.githubusercontent.com/{owner_repo}/{sha}{path}"
        ));
    }
    if let Some(rest) = spec.strip_prefix("gist:") {
        // gist:user/id[@rev]/file(.vyrn)
        let mut segs = rest.splitn(3, '/');
        let user = segs.next().ok_or("gist specifier needs user/id/file")?;
        let id_rev = segs.next().ok_or("gist specifier needs user/id/file")?;
        let file = segs.next().ok_or("gist specifier needs a file name")?;
        let (id, rev) = match id_rev.split_once('@') {
            Some((i, r)) => (i, Some(r)),
            None => (id_rev, None),
        };
        return Ok(match rev {
            Some(r) => format!("https://gist.githubusercontent.com/{user}/{id}/raw/{r}/{file}"),
            None => format!("https://gist.githubusercontent.com/{user}/{id}/raw/{file}"),
        });
    }
    if spec.starts_with("https://") {
        return Ok(spec.to_string());
    }
    Err(format!("not a remote specifier: {spec}"))
}

/// The refusal for bytes that arrived from a pinned URL and are not the pinned
/// bytes, spelled once: a module reads it through [`RemoteResolver`] and a
/// pinned tool reads it through `vyrn update --locked`, and a rule with two
/// copies is a rule with two answers. `remedy` is the command that would accept
/// the new content deliberately, which differs by what was fetched.
pub fn upstream_changed(spec: &str, url: &str, got: &str, pinned: &str, remedy: &str) -> String {
    format!(
        "`{spec}` fetched from {url} hashes {got}, but vyrn.lock pins {pinned} — \
         the upstream changed under an immutable URL; refusing to build \
         (run `{remedy}` to accept the new content deliberately)"
    )
}

/// Fetch a URL's bytes with `curl -sL --fail`.
pub fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let out = Command::new("curl")
        .args(["-sL", "--fail", url])
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "fetch failed for {url} (curl exit {:?})",
            out.status.code()
        ));
    }
    Ok(out.stdout)
}

// ---------------------------------------------------------------------------
// the resolver
// ---------------------------------------------------------------------------

/// The CLI's module resolver: local paths from disk; remote keys through
/// lock → vendor → cache → network (unless offline). New resolutions mark the
/// lock dirty; the caller saves it after a successful load.
pub struct RemoteResolver {
    pub lock: RefCell<Lock>,
    /// The project directory (vendor location); `None` = no manifest.
    pub project_dir: Option<String>,
    pub offline: bool,
}

impl RemoteResolver {
    fn read_remote(&self, spec: &str) -> Result<String, String> {
        // 1. Locked: content by hash, wherever it lives.
        let locked = self.lock.borrow().entries.get(spec).cloned();
        if let Some((url, sha)) = locked {
            if let Some(r) = pinned_blob(self.project_dir.as_deref(), &sha) {
                return r;
            }
            if self.offline {
                return Err(format!(
                    "`{spec}` is locked (sha256 {sha}) but not cached, and this is an \
                     offline build — run once online, `vyrn vendor`, or drop any copy \
                     of the file with that hash into the cache"
                ));
            }
            let bytes = fetch(&url)?;
            let got = sha256_hex(&bytes);
            if got != sha {
                return Err(upstream_changed(spec, &url, &got, &sha, "vyrn update"));
            }
            write_blob(&cache_dir(), &sha, &bytes)?;
            return String::from_utf8(bytes).map_err(|_| "module is not UTF-8".into());
        }
        // 2. Unlocked: first resolution (network), then pin.
        if self.offline {
            return Err(format!(
                "`{spec}` is not in vyrn.lock and this is an offline build"
            ));
        }
        let url = resolve_to_url(spec)?;
        let bytes = fetch(&url)?;
        let sha = sha256_hex(&bytes);
        write_blob(&cache_dir(), &sha, &bytes)?;
        if let Some(dir) = &self.project_dir {
            // Auto-vendor keeps committed repos self-contained when enabled.
            if vendor_dir(dir).parent().is_some_and(|p| p.exists()) {
                let _ = write_blob(&vendor_dir(dir), &sha, &bytes);
            }
        }
        let mut lock = self.lock.borrow_mut();
        lock.entries.insert(spec.to_string(), (url, sha));
        lock.dirty = true;
        String::from_utf8(bytes).map_err(|_| "module is not UTF-8".into())
    }
}

impl vyrn_frontend::loader::ModuleResolver for RemoteResolver {
    fn read(&self, resolved: &str) -> Result<String, String> {
        if vyrn_frontend::loader::is_remote(resolved) {
            self.read_remote(resolved)
        } else {
            std::fs::read_to_string(resolved).map_err(|e| e.to_string())
        }
    }
    fn list(&self, resolved: &str) -> Result<Vec<String>, String> {
        // Generation-time `listDir` reads local directories only (inputs are
        // local or lock-pinned; a remote key has no directory to enumerate).
        list_dir(resolved)
    }
    fn gen_cache_get(&self, key: &str) -> Option<String> {
        gen_cache_get(key)
    }
    fn gen_cache_put(&self, key: &str, value: &str) {
        gen_cache_put(key, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_to_url_shapes() {
        // A 40-hex ref needs no network.
        let sha = "a".repeat(40);
        assert_eq!(
            resolve_to_url(&format!("github:o/r@{sha}/src/x.vyrn")).unwrap(),
            format!("https://raw.githubusercontent.com/o/r/{sha}/src/x.vyrn")
        );
        assert_eq!(
            resolve_to_url("gist:u/abc123/f.vyrn").unwrap(),
            "https://gist.githubusercontent.com/u/abc123/raw/f.vyrn"
        );
        assert_eq!(
            resolve_to_url("gist:u/abc123@rev9/f.vyrn").unwrap(),
            "https://gist.githubusercontent.com/u/abc123/raw/rev9/f.vyrn"
        );
        assert_eq!(
            resolve_to_url("https://x.dev/m.vyrn").unwrap(),
            "https://x.dev/m.vyrn"
        );
    }

    #[test]
    fn locked_content_loads_offline_from_cache_and_rejects_tampering() {
        let text = b"export fn one() -> Int64 { return 1 }\n";
        let sha = sha256_hex(text);
        write_blob(&cache_dir(), &sha, text).unwrap();

        let dir = std::env::temp_dir().join("vyrn-remote-test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut lock = Lock::load(dir.join("vyrn.lock")).unwrap();
        lock.entries.insert(
            "https://x.dev/one.vyrn".into(),
            ("https://x.dev/one.vyrn".into(), sha.clone()),
        );
        let r = RemoteResolver {
            lock: RefCell::new(lock),
            project_dir: None,
            offline: true,
        };
        let got = r.read_remote("https://x.dev/one.vyrn").unwrap();
        assert_eq!(got.as_bytes(), text);

        // Tamper with the cached blob: the hash check must fail loudly.
        std::fs::write(cache_dir().join(&sha), b"evil").unwrap();
        let e = r.read_remote("https://x.dev/one.vyrn").unwrap_err();
        assert!(e.contains("does not match its recorded sha256"), "{e}");
        // Restore for other test runs.
        write_blob(&cache_dir(), &sha, text).unwrap();
    }

    #[test]
    fn offline_without_lock_is_a_clear_error() {
        let dir = std::env::temp_dir().join("vyrn-remote-test2");
        std::fs::create_dir_all(&dir).unwrap();
        let lock = Lock::load(dir.join("vyrn.lock")).unwrap();
        let r = RemoteResolver {
            lock: RefCell::new(lock),
            project_dir: None,
            offline: true,
        };
        let e = r.read_remote("https://x.dev/never-seen.vyrn").unwrap_err();
        assert!(e.contains("not in vyrn.lock"), "{e}");
    }
}
