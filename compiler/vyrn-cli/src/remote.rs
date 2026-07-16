//! Reproducible remote imports (RFC-0010 M4) — the CLI half.
//!
//! The frontend treats `github:` / `gist:` / `https:` specifiers as opaque
//! module keys; this module turns them into content with three guarantees:
//!
//!   * **Pinned**: every fetch is recorded in `vela.lock`
//!     (`specifier ⇥ resolved-immutable-url ⇥ sha256`, sorted, tab-separated).
//!     Once locked, only `velac update` changes an entry. Floating refs
//!     (`@main`, `@v1`) are resolved to a commit once, then frozen.
//!   * **Content-addressed**: bytes live in `~/.vela/cache/sha256/<hex>`
//!     (and optionally `./vela_vendor/sha256/<hex>` for committed, air-gapped
//!     repos). The hash is verified on EVERY load — a tampered cache fails
//!     loudly, and any copy of the file obtained anywhere can restore a
//!     vanished upstream (the left-pad scenario).
//!   * **Offline-capable**: `--offline` / `VELA_OFFLINE=1` forbids network;
//!     a lock+cache hit needs none.
//!
//! Zero new crates: SHA-256 is implemented below (FIPS 180-4, tested against
//! NIST vectors), fetching shells out to `curl -sL --fail`, and git refs
//! resolve via `git ls-remote` — both tools are ubiquitous.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// SHA-256 (FIPS 180-4)
// ---------------------------------------------------------------------------

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
    0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
    0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
    0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
    0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
    0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
    0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
    0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
    0xc67178f2,
];

/// The SHA-256 digest of `data`, lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    // Pad: 0x80, zeros, then the bit length as big-endian u64.
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

// ---------------------------------------------------------------------------
// lockfile
// ---------------------------------------------------------------------------

/// `vela.lock`: `specifier ⇥ resolved-url ⇥ sha256` per line, sorted by
/// specifier. Line-based and diff-friendly by design.
pub struct Lock {
    pub path: PathBuf,
    pub entries: BTreeMap<String, (String, String)>,
    pub dirty: bool,
}

impl Lock {
    pub fn load(path: PathBuf) -> Lock {
        let mut entries = BTreeMap::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                let mut parts = line.split('\t');
                if let (Some(spec), Some(url), Some(sha)) =
                    (parts.next(), parts.next(), parts.next())
                {
                    entries.insert(spec.to_string(), (url.to_string(), sha.to_string()));
                }
            }
        }
        Lock { path, entries, dirty: false }
    }

    pub fn save(&self) -> Result<(), String> {
        let mut out = String::new();
        for (spec, (url, sha)) in &self.entries {
            out.push_str(&format!("{spec}\t{url}\t{sha}\n"));
        }
        std::fs::write(&self.path, out).map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
// cache / vendor
// ---------------------------------------------------------------------------

pub fn cache_dir() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    Path::new(&home).join(".vela/cache/sha256")
}

pub fn vendor_dir(project_dir: &str) -> PathBuf {
    Path::new(project_dir).join("vela_vendor/sha256")
}

/// Read a content-addressed blob, verifying its hash (tamper-evident).
fn read_blob(dir: &Path, sha: &str) -> Option<Result<String, String>> {
    let path = dir.join(sha);
    let bytes = std::fs::read(&path).ok()?;
    if sha256_hex(&bytes) != sha {
        return Some(Err(format!(
            "cached copy at `{}` does not match its recorded sha256 — delete it and \
             re-fetch (or restore a good copy: any file hashing {sha} works)",
            path.display()
        )));
    }
    Some(String::from_utf8(bytes).map_err(|_| "cached module is not UTF-8".to_string()))
}

fn write_blob(dir: &Path, sha: &str, bytes: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(sha), bytes).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// specifier resolution + fetching
// ---------------------------------------------------------------------------

/// Turn a remote specifier into an immutable URL. Floating github refs are
/// pinned to a commit via `git ls-remote` (network!); 40-hex refs and https
/// URLs are already immutable.
pub fn resolve_to_url(spec: &str) -> Result<String, String> {
    if let Some(rest) = spec.strip_prefix("github:") {
        // github:owner/repo@ref/path(.vela)
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
                .ok_or_else(|| {
                    format!("cannot resolve ref `{r}` in github.com/{owner_repo}")
                })?
                .to_string()
        };
        return Ok(format!(
            "https://raw.githubusercontent.com/{owner_repo}/{sha}{path}"
        ));
    }
    if let Some(rest) = spec.strip_prefix("gist:") {
        // gist:user/id[@rev]/file(.vela)
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

/// Fetch a URL's bytes with `curl -sL --fail`.
pub fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let out = Command::new("curl")
        .args(["-sL", "--fail", url])
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("fetch failed for {url} (curl exit {:?})", out.status.code()));
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
            if let Some(dir) = &self.project_dir {
                if let Some(r) = read_blob(&vendor_dir(dir), &sha) {
                    return r;
                }
            }
            if let Some(r) = read_blob(&cache_dir(), &sha) {
                return r;
            }
            if self.offline {
                return Err(format!(
                    "`{spec}` is locked (sha256 {sha}) but not cached, and this is an \
                     offline build — run once online, `velac vendor`, or drop any copy \
                     of the file with that hash into the cache"
                ));
            }
            let bytes = fetch(&url)?;
            let got = sha256_hex(&bytes);
            if got != sha {
                return Err(format!(
                    "`{spec}` fetched from {url} hashes {got}, but vela.lock pins {sha} — \
                     the upstream changed under an immutable URL; refusing to build \
                     (run `velac update` to accept the new content deliberately)"
                ));
            }
            write_blob(&cache_dir(), &sha, &bytes)?;
            return String::from_utf8(bytes).map_err(|_| "module is not UTF-8".into());
        }
        // 2. Unlocked: first resolution (network), then pin.
        if self.offline {
            return Err(format!(
                "`{spec}` is not in vela.lock and this is an offline build"
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

impl vela_frontend::loader::ModuleResolver for RemoteResolver {
    fn read(&self, resolved: &str) -> Result<String, String> {
        if vela_frontend::loader::is_remote(resolved) {
            self.read_remote(resolved)
        } else {
            std::fs::read_to_string(resolved).map_err(|e| e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_nist_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // A length crossing the 55-byte padding boundary.
        assert_eq!(
            sha256_hex(&[b'a'; 64]),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    #[test]
    fn lock_round_trips() {
        let dir = std::env::temp_dir().join("vela-lock-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vela.lock");
        let mut lock = Lock::load(path.clone());
        lock.entries.insert(
            "github:a/b@v1/x.vela".into(),
            ("https://raw.githubusercontent.com/a/b/deadbeef/x.vela".into(), "abc123".into()),
        );
        lock.save().unwrap();
        let reloaded = Lock::load(path);
        assert_eq!(lock.entries, reloaded.entries);
    }

    #[test]
    fn resolve_to_url_shapes() {
        // A 40-hex ref needs no network.
        let sha = "a".repeat(40);
        assert_eq!(
            resolve_to_url(&format!("github:o/r@{sha}/src/x.vela")).unwrap(),
            format!("https://raw.githubusercontent.com/o/r/{sha}/src/x.vela")
        );
        assert_eq!(
            resolve_to_url("gist:u/abc123/f.vela").unwrap(),
            "https://gist.githubusercontent.com/u/abc123/raw/f.vela"
        );
        assert_eq!(
            resolve_to_url("gist:u/abc123@rev9/f.vela").unwrap(),
            "https://gist.githubusercontent.com/u/abc123/raw/rev9/f.vela"
        );
        assert_eq!(
            resolve_to_url("https://x.dev/m.vela").unwrap(),
            "https://x.dev/m.vela"
        );
    }

    #[test]
    fn locked_content_loads_offline_from_cache_and_rejects_tampering() {
        let text = b"export fn one() -> Int64 { return 1 }\n";
        let sha = sha256_hex(text);
        write_blob(&cache_dir(), &sha, text).unwrap();

        let dir = std::env::temp_dir().join("vela-remote-test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut lock = Lock::load(dir.join("vela.lock"));
        lock.entries
            .insert("https://x.dev/one.vela".into(), ("https://x.dev/one.vela".into(), sha.clone()));
        let r = RemoteResolver { lock: RefCell::new(lock), project_dir: None, offline: true };
        let got = r.read_remote("https://x.dev/one.vela").unwrap();
        assert_eq!(got.as_bytes(), text);

        // Tamper with the cached blob: the hash check must fail loudly.
        std::fs::write(cache_dir().join(&sha), b"evil").unwrap();
        let e = r.read_remote("https://x.dev/one.vela").unwrap_err();
        assert!(e.contains("does not match its recorded sha256"), "{e}");
        // Restore for other test runs.
        write_blob(&cache_dir(), &sha, text).unwrap();
    }

    #[test]
    fn offline_without_lock_is_a_clear_error() {
        let dir = std::env::temp_dir().join("vela-remote-test2");
        std::fs::create_dir_all(&dir).unwrap();
        let lock = Lock::load(dir.join("vela.lock"));
        let r = RemoteResolver { lock: RefCell::new(lock), project_dir: None, offline: true };
        let e = r.read_remote("https://x.dev/never-seen.vela").unwrap_err();
        assert!(e.contains("not in vela.lock"), "{e}");
    }
}
