//! Remote imports (`github:`, `gist:`, `https:`): resolving floating refs,
//! fetching with `curl`, and pinning in `vyrn.lock`. Reading the pin and the
//! cache lives in [`vyrn_frontend::manifest`], because the LSP must reach the
//! same verdict without the network. The sha256 is verified on every load.

use std::cell::RefCell;
use std::process::Command;

// The lock and cache readers live in `vyrn_frontend::manifest`: the LSP reads
// them too, and a second reader of a pin is a second answer.
pub use vyrn_frontend::hash::sha256_hex;
use vyrn_frontend::loader::DiskResolver;
pub use vyrn_frontend::manifest::{cache_dir, pinned_blob, vendor_dir, write_blob, Lock};

/// Turns a remote specifier into an immutable URL. A floating github ref is
/// pinned to a commit via `git ls-remote`, which uses the network.
pub fn resolve_to_url(spec: &str) -> Result<String, String> {
    if let Some(rest) = spec.strip_prefix("github:") {
        // github:owner/repo@ref/path(.vyrn)
        let at = rest.find('@').ok_or("github specifier needs `@ref`")?;
        let (owner_repo, rest) = rest.split_at(at);
        let rest = &rest[1..];
        // The ref and the path share `/`: `feature/2/api/src/x.vyrn` may name
        // ref `feature` or `feature/2`. `@ref=<ref>/<path>` names the split;
        // without it every `/` boundary is a candidate ref, shortest first.
        let (r, path, candidates) = if let Some(explicit) = rest.strip_prefix("ref=") {
            let slash = explicit
                .find('/')
                .ok_or("`@ref=<ref>/<path>` needs the file path after the ref")?;
            let (r, path) = (&explicit[..slash], &explicit[slash..]);
            (r, path, vec![r])
        } else {
            let slash = rest.find('/').ok_or("github specifier needs a file path")?;
            let (r, path) = (&rest[..slash], &rest[slash..]);
            let candidates: Vec<&str> = rest.match_indices('/').map(|(i, _)| &rest[..i]).collect();
            (r, path, candidates)
        };
        let sha = if r.len() == 40 && r.bytes().all(|b| b.is_ascii_hexdigit()) {
            // Already a commit: immutable, no network.
            r.to_string()
        } else {
            ls_remote_ref(
                &format!("https://github.com/{owner_repo}"),
                &candidates,
                spec,
                ls_remote_once,
            )?
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

/// Resolves a floating github ref to a commit by probing each candidate.
/// Exactly one live ref wins; several refuse as ambiguous, because a guess
/// pins an arbitrary branch; none reports the shortest reading's failure.
/// `probe` is the network edge, so the choice is testable offline.
fn ls_remote_ref(
    url: &str,
    candidates: &[&str],
    spec: &str,
    mut probe: impl FnMut(&str, &str) -> Result<String, String>,
) -> Result<String, String> {
    let mut live: Vec<(&str, String)> = Vec::new();
    let mut first_err = String::new();
    for r in candidates {
        match probe(url, r) {
            Ok(sha) => live.push((r, sha)),
            Err(e) => {
                if first_err.is_empty() {
                    first_err = e;
                }
            }
        }
    }
    match live.len() {
        1 => Ok(live.remove(0).1),
        0 => Err(first_err),
        _ => Err(format!(
            "`{spec}` is ambiguous: refs {} all exist in {url} — \
             write `@ref=<ref>/<path>` to name the ref and the file path explicitly",
            live.iter()
                .map(|(r, _)| format!("`{r}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn ls_remote_once(url: &str, r: &str) -> Result<String, String> {
    let out = Command::new("git")
        .args(["ls-remote", url, r])
        .output()
        .map_err(|e| format!("cannot run git ls-remote: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace()
        .next()
        .filter(|s| s.len() == 40)
        .map(str::to_string)
        .ok_or_else(|| format!("cannot resolve ref `{r}` in {url}"))
}

/// The refusal for fetched bytes that differ from the pinned bytes, shared by
/// [`RemoteResolver`] and `vyrn update --locked`. `remedy` is the command that
/// accepts the new content.
pub fn upstream_changed(spec: &str, url: &str, got: &str, pinned: &str, remedy: &str) -> String {
    format!(
        "`{spec}` fetched from {url} hashes {got}, but vyrn.lock pins {pinned} — \
         the upstream changed under an immutable URL; refusing to build \
         (run `{remedy}` to accept the new content deliberately)"
    )
}

/// Fetches a URL's bytes with `curl -sL --fail`.
///
/// A lock file is data, so a crafted entry like `-K/etc/passwd` must never
/// become a curl option. Only `https://` and local `file://` URLs are fetched;
/// the sha256 gate still decides what the build accepts, and `--` ends curl's
/// option parsing.
pub fn fetch(url: &str) -> Result<Vec<u8>, String> {
    if !url.starts_with("https://") && !url.starts_with("file://") {
        return Err(format!("refusing to fetch a non-https URL: {url}"));
    }
    let out = Command::new("curl")
        .args(["-sL", "--fail", "--", url])
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

/// The CLI's module resolver: local paths from disk; remote keys through
/// lock -> vendor -> cache -> network (unless offline). A new resolution marks
/// the lock dirty; the caller saves it after a successful load.
pub struct RemoteResolver {
    pub lock: RefCell<Lock>,
    /// The vendor location; `None` means no manifest.
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

/// [`DiskResolver`] with `read` overridden for remote keys.
impl vyrn_frontend::loader::ModuleResolver for RemoteResolver {
    fn read(&self, resolved: &str) -> Result<String, String> {
        if vyrn_frontend::loader::is_remote(resolved) {
            self.read_remote(resolved)
        } else {
            DiskResolver.read(resolved)
        }
    }
    fn list_kinds(&self, resolved: &str) -> Result<Vec<String>, String> {
        // A remote key has no directory to enumerate.
        DiskResolver.list_kinds(resolved)
    }
    fn gen_cache_get(&self, key: &str) -> Option<String> {
        DiskResolver.gen_cache_get(key)
    }
    fn gen_cache_put(&self, key: &str, value: &str) {
        DiskResolver.gen_cache_put(key, value)
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
    fn explicit_ref_form_names_both_sides() {
        let sha = "b".repeat(40);
        assert_eq!(
            resolve_to_url(&format!("github:o/r@ref={sha}/src/x.vyrn")).unwrap(),
            format!("https://raw.githubusercontent.com/o/r/{sha}/src/x.vyrn")
        );
        let e = resolve_to_url("github:o/r@ref=main").unwrap_err();
        assert!(e.contains("needs the file path"), "{e}");
    }

    #[test]
    fn slashed_refs_disambiguate_shortest_first_or_refuse() {
        let url = "https://github.com/o/r";
        // One live ref wins, whatever its length.
        let sha = ls_remote_ref(
            url,
            &["feature", "feature/2"],
            "github:o/r@feature/2/x.vyrn",
            |_, r| match r {
                "feature/2" => Ok("b".repeat(40)),
                _ => Err(format!("cannot resolve ref `{r}`")),
            },
        )
        .unwrap();
        assert_eq!(sha, "b".repeat(40));
        // Two live refs refuse, naming both and the way out.
        let e = ls_remote_ref(
            url,
            &["feature", "feature/2"],
            "github:o/r@feature/2/x.vyrn",
            |_, _| Ok("c".repeat(40)),
        )
        .unwrap_err();
        assert!(e.contains("`feature`"), "{e}");
        assert!(e.contains("`feature/2`"), "{e}");
        assert!(e.contains("@ref="), "{e}");
        // None lives: the shortest reading's failure is what is reported.
        let e = ls_remote_ref(url, &["main", "main/dev"], "spec", |_, r| {
            Err(format!("cannot resolve ref `{r}`"))
        })
        .unwrap_err();
        assert!(e.contains("cannot resolve ref `main`"), "{e}");
    }

    #[test]
    fn fetch_refuses_non_https_and_option_looking_urls() {
        // A crafted URL never becomes a curl option, and plaintext is never
        // fetched.
        for url in [
            "http://x.dev/m.vyrn",
            "-K/etc/passwd",
            "--output=/tmp/evil",
            "ftp://x.dev/m.vyrn",
        ] {
            let e = fetch(url).unwrap_err();
            assert!(e.contains("refusing to fetch"), "{url}: {e}");
        }
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
