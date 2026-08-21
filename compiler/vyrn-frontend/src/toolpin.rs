//! Pinned toolchains (RFC-0102 M1): the table that turns a tool name and a
//! version into a URL, and the resolver that turns a `tool:` line in
//! `vyrn.lock` into an unpacked directory under `~/.vyrn/tools/<sha256>/`.
//!
//! A tool is a dependency. It is named in `vyrn.json`, frozen in `vyrn.lock` as
//! `specifier ⇥ url ⇥ sha256`, stored by content hash in `~/.vyrn`, and verified
//! on every load — the four properties [`crate::manifest`] already gives a
//! module. Nothing here is a new mechanism; the specifier is
//! `tool:<name>@<version>/<platform>`, which `Lock::load` reads as an opaque
//! string like any other.
//!
//! **What is NOT here is the network.** Resolution reads the lock, the vendor
//! directory and the user cache, and refuses when none of them holds the pinned
//! bytes. Fetching them is `vyrn update <tool>`'s, in the driver, for the reason
//! `vyrn-cli::remote` already gives: the editor reads a pin too and must reach
//! the same verdict without ever reaching the network.

use crate::manifest::{cache_dir, pinned_blob_bytes, Lock};
use std::path::{Path, PathBuf};

/// The platforms a native tool artifact is published for — `install.sh`'s
/// vocabulary, unchanged. One word, one meaning: a second spelling of a platform
/// would be a second answer about which artifact to fetch.
pub const PLATFORMS: [&str; 4] = [
    "x86_64-linux",
    "aarch64-linux",
    "aarch64-macos",
    "x86_64-windows",
];

/// The tools the table knows. Four entries, because these are the four this
/// repository fetches; a name outside this list is a refusal, never a
/// fall-through to PATH.
pub const KNOWN_TOOLS: [&str; 4] = ["wasmtime", "wasi-sysroot", "wasi-builtins", "cargo-nextest"];

/// This machine, in [`PLATFORMS`]' vocabulary. Rust's own `ARCH`/`OS` constants
/// already spell it that way (`x86_64`, `aarch64`; `linux`, `macos`, `windows`),
/// so this is a join rather than a table. A host outside the vocabulary — say
/// `aarch64-windows` — still gets a name here, because the refusal has to be
/// able to say which platform had no entry.
pub fn host_platform() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

/// The lock specifier for one tool artifact.
pub fn tool_spec(name: &str, version: &str, platform: &str) -> String {
    format!("tool:{name}@{version}/{platform}")
}

/// The platforms one tool publishes an artifact per.
///
/// `any` is a real platform value, not a placeholder: the wasi sysroot and the
/// builtins archive are wasm32 **target** libraries, the same file on every
/// host. One entry, every machine.
pub fn tool_platforms(name: &str) -> &'static [&'static str] {
    match name {
        "wasi-sysroot" | "wasi-builtins" => &["any"],
        _ => &PLATFORMS,
    }
}

/// The environment variable that overrides a tool, so a refusal can name the
/// escape hatch it is refusing to take silently.
///
/// Empty for a tool no code in this compiler resolves — see [`escape_hatch`].
pub fn tool_env_var(name: &str) -> &'static str {
    match name {
        "wasmtime" => "VYRN_WASMTIME",
        "wasi-sysroot" => "WASI_SYSROOT",
        "wasi-builtins" => "WASI_BUILTINS",
        _ => "",
    }
}

/// The clause a refusal appends to name the escape hatch — and nothing at all
/// for a tool that has none.
///
/// `cargo-nextest` is the case that needed this: it is CI's test runner, pinned
/// by this table and put on PATH by the workflow, and no Rust code here ever
/// looks for it. A variable nothing reads is the exact defect the three
/// `WASI_*` exports were (RFC-0076 M7 left them behind for a build that had
/// stopped reading them), so the refusal says `vyrn update cargo-nextest` and
/// stops there rather than inventing a `$VYRN_NEXTEST` no reader honours.
pub fn escape_hatch(name: &str) -> String {
    match tool_env_var(name) {
        "" => String::new(),
        v => format!(", or point ${v} at a binary you trust"),
    }
}

/// A tool name the table does not know.
pub fn unknown_tool(name: &str) -> String {
    format!(
        "unknown tool `{name}` in vyrn.json's `toolchain` — the tools vyrn can pin are {}",
        KNOWN_TOOLS.join(", ")
    )
}

/// Name, version and platform in; the published artifact's URL out.
///
/// This is the whole of "where the URL comes from". Inventing a URL scheme for
/// arbitrary third-party tools is future work; today an unknown name is
/// [`unknown_tool`].
pub fn tool_url(name: &str, version: &str, platform: &str) -> Result<String, String> {
    match name {
        // Windows ships a zip and every other platform a tar.xz; `tar` reads
        // both, so the extension matters to the URL and to nothing else.
        "wasmtime" => {
            let ext = if platform.ends_with("-windows") {
                "zip"
            } else {
                "tar.xz"
            };
            Ok(format!(
                "https://github.com/bytecodealliance/wasmtime/releases/download/v{version}/\
                 wasmtime-v{version}-{platform}.{ext}"
            ))
        }
        // wasi-sdk names its release by the major version and its assets by the
        // full one: `wasi-sdk-25` holds `wasi-sysroot-25.0.tar.gz`.
        "wasi-sysroot" | "wasi-builtins" => {
            let major = version.split('.').next().unwrap_or(version);
            let asset = if name == "wasi-sysroot" {
                format!("wasi-sysroot-{version}.tar.gz")
            } else {
                format!("libclang_rt.builtins-wasm32-wasi-{version}.tar.gz")
            };
            Ok(format!(
                "https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-{major}/{asset}"
            ))
        }
        // One archive per Rust target triple, and a `.tar.gz` for every one of
        // them — Windows included, unlike wasmtime — so there is no extension
        // to choose here. macOS publishes ONE artifact, a universal binary,
        // which is why `aarch64-macos` maps to `universal-apple-darwin`: the
        // vocabulary above names the host, and this arm names the asset.
        "cargo-nextest" => {
            let triple = match platform {
                "x86_64-linux" => "x86_64-unknown-linux-gnu",
                "aarch64-linux" => "aarch64-unknown-linux-gnu",
                "aarch64-macos" => "universal-apple-darwin",
                "x86_64-windows" => "x86_64-pc-windows-msvc",
                _ => {
                    return Err(format!(
                        "cargo-nextest publishes no artifact for {platform}"
                    ))
                }
            };
            Ok(format!(
                "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-{version}/\
                 cargo-nextest-{version}-{triple}.tar.gz"
            ))
        }
        _ => Err(unknown_tool(name)),
    }
}

/// Where unpacked tools live: `~/.vyrn/tools/<sha256>/`, beside `bin`, `std`,
/// `web` and `cache`. Derived from a verified blob, and deletable at any time.
pub fn tools_dir() -> PathBuf {
    cache_dir()
        .parent() // ~/.vyrn/cache
        .and_then(|p| p.parent()) // ~/.vyrn
        .map(|p| p.join("tools"))
        .unwrap_or_else(|| PathBuf::from(".vyrn/tools"))
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// The file an unpacked tool directory carries recording which sha it holds,
/// written only after the archive is fully placed. The tools directory is
/// content-addressed storage any process running as the user can write, so a
/// directory that is there but does not certify itself is treated as absent.
const SHA_MARKER: &str = ".vyrn-sha";

/// A lock sha the tools directory will store: lowercase hex, full width. The
/// sha joins into a path everywhere below, so it is validated before any join —
/// a corrupt or hand-edited lock must refuse, never traverse.
fn is_sha256(sha: &str) -> bool {
    sha.len() == 64 && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether `dir` is a completed unpack of `sha`: present, and carrying the
/// marker [`unpack_tool`] writes last.
fn verified(dir: &Path, sha: &str) -> bool {
    std::fs::read_to_string(dir.join(SHA_MARKER))
        .map_or(false, |recorded| recorded.trim_end() == sha)
}

/// Unpack a verified archive to `~/.vyrn/tools/<sha>/`, or return it if it is
/// already there and certifies itself (see [`verified`]).
///
/// Unpacking shells out to `tar`, for the reason `curl` and `git` are already
/// spawned: the tool is ubiquitous — Windows 10 and later, every Linux userland,
/// macOS — and a crate for it would cost more than it buys. `tar -xf` sniffs the
/// format, so one call covers `.tar.gz`, `.tar.xz` and `.zip`.
pub fn unpack_tool(sha: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    if !is_sha256(sha) {
        return Err(format!(
            "`{sha}` is not a sha256 digest — the tools directory is keyed by content hash"
        ));
    }
    let out = tools_dir().join(sha);
    if verified(&out, sha) {
        return Ok(out);
    }
    let pid = std::process::id();
    let stage = tools_dir().join(format!("{sha}.{pid}.tmp"));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(|e| format!("cannot create {}: {e}", slash(&stage)))?;
    let archive = tools_dir().join(format!("{sha}.{pid}.archive"));
    std::fs::write(&archive, bytes)
        .map_err(|e| format!("cannot write {}: {e}", slash(&archive)))?;

    let st = std::process::Command::new("tar")
        .args(["-xf", &slash(&archive), "-C", &slash(&stage)])
        .status();
    let _ = std::fs::remove_file(&archive);
    match st {
        Ok(s) if s.success() => {}
        Ok(s) => {
            let _ = std::fs::remove_dir_all(&stage);
            return Err(format!(
                "cannot unpack the pinned archive {sha} (tar exit {:?})",
                s.code()
            ));
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&stage);
            return Err(format!("cannot run tar: {e}"));
        }
    }
    // Staged, then renamed: two builds may unpack the same tool at once, and a
    // reader must never see half an archive.
    if std::fs::rename(&stage, &out).is_err() {
        if verified(&out, sha) {
            // Another process placed the same bytes first; ours is redundant.
            let _ = std::fs::remove_dir_all(&stage);
        } else {
            // An uncertified directory holds the name — an interrupted unpack,
            // a hand-made one, or a pre-marker layout. Replace it with the
            // staged copy, which is the one this process built and can certify.
            let _ = std::fs::remove_dir_all(&out);
            if std::fs::rename(&stage, &out).is_err() {
                let _ = std::fs::remove_dir_all(&stage);
                return Err(format!("cannot place the unpacked tool at {}", slash(&out)));
            }
        }
    }
    // The marker is written LAST, after the tree is fully in place: a reader
    // that finds it can trust everything beside it — "verified on every load",
    // the invariant this module's doc claims.
    std::fs::write(out.join(SHA_MARKER), sha)
        .map_err(|e| format!("cannot mark {} as verified: {e}", slash(&out)))?;
    Ok(out)
}

/// The unpacked directory of a pinned tool, resolved through the lock, then the
/// vendor directory, then the user cache. Never the network, and never PATH.
///
/// The refusals are three, and each says what to do about it: no lock entry for
/// this platform, no bytes for a hash the lock names, or an archive that will
/// not unpack. A pinned tool that cannot be resolved **fails** — the whole value
/// of a pin is that its absence is loud.
pub fn pinned_tool(
    project_dir: Option<&str>,
    lock: &Lock,
    name: &str,
    version: &str,
) -> Result<PathBuf, String> {
    let platform = if tool_platforms(name) == ["any"] {
        "any".to_string()
    } else {
        host_platform()
    };
    let spec = tool_spec(name, version, &platform);
    let Some((_, sha)) = lock.entries.get(&spec) else {
        let prefix = format!("tool:{name}@{version}/");
        let covered: Vec<&str> = lock
            .entries
            .keys()
            .filter_map(|k| k.strip_prefix(&prefix))
            .collect();
        let covered = if covered.is_empty() {
            "none".to_string()
        } else {
            covered.join(", ")
        };
        return Err(format!(
            "{name} {version} is pinned, and vyrn.lock has no entry for {platform}.\n  \
             Pinned platforms: {covered}.\n  \
             Add one with `vyrn update {name}`{}.",
            escape_hatch(name),
        ));
    };
    // The sha joins into a path below, so it is checked before any join: a
    // corrupt or hand-edited lock refuses rather than reaching outside
    // ~/.vyrn/tools.
    if !is_sha256(sha) {
        return Err(format!(
            "{name} {version} is pinned with sha `{sha}`, which is not a sha256 digest — \
             vyrn.lock is corrupt or hand-edited."
        ));
    }
    let out = tools_dir().join(sha);
    // A directory that is there but does not certify itself (no marker: an
    // interrupted unpack, a hand-made one, a pre-marker layout) is treated as
    // absent and re-unpacked from the pinned bytes below — never trusted.
    if verified(&out, sha) {
        return Ok(out);
    }
    match pinned_blob_bytes(project_dir, sha) {
        Some(Ok(bytes)) => unpack_tool(sha, &bytes),
        Some(Err(e)) => Err(e),
        None => Err(format!(
            "{name} {version} is pinned for {platform} (sha256 {sha}) but not cached — \
             run `vyrn update {name}` online, `vyrn vendor`, or drop any copy of the \
             archive with that hash into {}{}",
            slash(&cache_dir()),
            escape_hatch(name)
        )),
    }
}

/// `name` at the top of an unpacked tool directory or one level in — a release
/// archive puts its payload inside a version-named directory, and which one is
/// not this resolver's business. Sorted, so several unpacked side by side pick
/// deterministically.
fn at_top_or_one_in(dir: &Path, name: &str) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.exists() {
        return Some(direct);
    }
    let mut hits: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path().join(name))
        .filter(|p| p.exists())
        .collect();
    hits.sort();
    hits.into_iter().next()
}

/// The first file named `what` (or `what.exe`) at the top of an unpacked tool
/// directory or one level in.
pub fn tool_binary(dir: &Path, what: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{what}.exe")
    } else {
        what.to_string()
    };
    at_top_or_one_in(dir, &exe).filter(|p| p.is_file())
}

/// The first file named exactly `name`, same two levels. A library archive is
/// not an executable and gets no `.exe`.
pub fn tool_file(dir: &Path, name: &str) -> Option<PathBuf> {
    at_top_or_one_in(dir, name).filter(|p| p.is_file())
}

/// The directory a consumer actually points at, found by a `marker` it must
/// contain: `~/.vyrn/tools/<sha>/wasi-sysroot-25.0` rather than the `<sha>`
/// above it, because `--sysroot=` wants the tree with `include/` in it.
pub fn tool_root(dir: &Path, marker: &str) -> Option<PathBuf> {
    at_top_or_one_in(dir, marker)?
        .parent()
        .map(|p| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One archive per Rust target triple, `.tar.gz` on every platform, and one
    /// universal binary for macOS — the three ways cargo-nextest's naming
    /// differs from wasmtime's, each pinned here.
    #[test]
    fn cargo_nextest_names_a_target_triple_and_one_universal_mac_artifact() {
        let base = "https://github.com/nextest-rs/nextest/releases/download/\
                    cargo-nextest-0.9.143/cargo-nextest-0.9.143-";
        for (platform, triple) in [
            ("x86_64-linux", "x86_64-unknown-linux-gnu"),
            ("aarch64-linux", "aarch64-unknown-linux-gnu"),
            ("aarch64-macos", "universal-apple-darwin"),
            ("x86_64-windows", "x86_64-pc-windows-msvc"),
        ] {
            assert_eq!(
                tool_url("cargo-nextest", "0.9.143", platform).unwrap(),
                format!("{base}{triple}.tar.gz"),
            );
        }
        // Every host in the vocabulary is covered, so the table needs no
        // fall-through — and a host outside it is a refusal that says so.
        assert_eq!(tool_platforms("cargo-nextest"), &PLATFORMS);
        assert_eq!(
            tool_url("cargo-nextest", "0.9.143", "aarch64-windows").unwrap_err(),
            "cargo-nextest publishes no artifact for aarch64-windows"
        );
        // No Rust code here resolves it, so the refusal names no variable.
        assert_eq!(tool_env_var("cargo-nextest"), "");
        assert_eq!(escape_hatch("cargo-nextest"), "");
        assert_eq!(
            escape_hatch("wasmtime"),
            ", or point $VYRN_WASMTIME at a binary you trust"
        );
    }

    #[test]
    fn the_table_knows_four_tools_and_refuses_the_rest() {
        assert_eq!(
            tool_url("wasmtime", "46.0.1", "x86_64-linux").unwrap(),
            "https://github.com/bytecodealliance/wasmtime/releases/download/v46.0.1/\
             wasmtime-v46.0.1-x86_64-linux.tar.xz"
        );
        assert_eq!(
            tool_url("wasmtime", "46.0.1", "x86_64-windows").unwrap(),
            "https://github.com/bytecodealliance/wasmtime/releases/download/v46.0.1/\
             wasmtime-v46.0.1-x86_64-windows.zip"
        );
        assert_eq!(
            tool_url("wasi-sysroot", "25.0", "any").unwrap(),
            "https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-25/\
             wasi-sysroot-25.0.tar.gz"
        );
        assert_eq!(
            tool_url("wasi-builtins", "25.0", "any").unwrap(),
            "https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-25/\
             libclang_rt.builtins-wasm32-wasi-25.0.tar.gz"
        );

        // A name the table does not know names the ones it does. It is never a
        // fall-through to PATH.
        let e = tool_url("wasm-opt", "1", "x86_64-linux").unwrap_err();
        assert_eq!(
            e,
            "unknown tool `wasm-opt` in vyrn.json's `toolchain` — the tools vyrn can pin \
             are wasmtime, wasi-sysroot, wasi-builtins, cargo-nextest"
        );
    }

    #[test]
    fn the_host_platform_is_the_published_vocabulary() {
        let p = host_platform();
        assert!(
            PLATFORMS.contains(&p.as_str()) || p.contains('-'),
            "a host outside the vocabulary still has a name: {p}"
        );
        assert_eq!(
            tool_spec("wasmtime", "46.0.1", "x86_64-linux"),
            "tool:wasmtime@46.0.1/x86_64-linux"
        );
    }

    /// The specifier is opaque to `Lock::load`, so a `tool:` line rides the
    /// existing reader, the existing sort and the existing refusals with no
    /// format change at all.
    #[test]
    fn a_tool_line_round_trips_through_the_lock_that_already_exists() {
        let dir = std::env::temp_dir().join("vyrn-toolpin-lock");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vyrn.lock");
        let _ = std::fs::remove_file(&path);
        let mut lock = Lock::load(path.clone()).unwrap();
        let sha = "9".repeat(64);
        for p in PLATFORMS {
            lock.entries.insert(
                tool_spec("wasmtime", "46.0.1", p),
                (tool_url("wasmtime", "46.0.1", p).unwrap(), sha.clone()),
            );
        }
        lock.entries.insert(
            "github:a/b@v1/x.vyrn".into(),
            ("https://x.dev/x.vyrn".into(), "abc123".into()),
        );
        lock.save().unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains(
                "tool:wasmtime@46.0.1/x86_64-windows\thttps://github.com/bytecodealliance/\
                 wasmtime/releases/download/v46.0.1/wasmtime-v46.0.1-x86_64-windows.zip\t"
            ),
            "{text}"
        );
        assert_eq!(Lock::load(path).unwrap().entries, lock.entries);
    }

    /// The refusal names the tool, the version, this platform, the platforms the
    /// lock does cover, the command that adds one, and the escape hatch.
    #[test]
    fn no_entry_for_this_platform_is_a_refusal_that_says_all_six_things() {
        let dir = std::env::temp_dir().join("vyrn-toolpin-refusal");
        std::fs::create_dir_all(&dir).unwrap();
        let mut lock = Lock::load(dir.join("vyrn.lock")).unwrap();
        // Pinned for two platforms, neither of them this one.
        for p in ["x86_64-linux", "aarch64-macos"] {
            lock.entries.insert(
                tool_spec("wasmtime", "46.0.1", p),
                ("https://x.dev/w".into(), "f".repeat(64)),
            );
        }
        let host = host_platform();
        // The test host is one of the two pinned platforms on exactly two of the
        // four; there the pin resolves as far as the cache, which the next test
        // covers. Everywhere else this is the refusal.
        if host == "x86_64-linux" || host == "aarch64-macos" {
            return;
        }
        let e = pinned_tool(None, &lock, "wasmtime", "46.0.1").unwrap_err();
        assert!(e.contains("wasmtime 46.0.1 is pinned"), "{e}");
        assert!(e.contains(&format!("no entry for {host}")), "{e}");
        assert!(
            // The lock is sorted, so the covered platforms are too.
            e.contains("Pinned platforms: aarch64-macos, x86_64-linux."),
            "{e}"
        );
        assert!(e.contains("`vyrn update wasmtime`"), "{e}");
        assert!(e.contains("$VYRN_WASMTIME"), "{e}");
    }

    /// A pin whose hash is in the lock but whose bytes are nowhere fails, and
    /// says which hash would satisfy it. It never falls back to anything.
    #[test]
    fn pinned_but_uncached_fails_and_names_the_hash() {
        let dir = std::env::temp_dir().join("vyrn-toolpin-uncached");
        std::fs::create_dir_all(&dir).unwrap();
        let mut lock = Lock::load(dir.join("vyrn.lock")).unwrap();
        let sha = "a".repeat(64);
        lock.entries.insert(
            tool_spec("wasmtime", "9.9.9", &host_platform()),
            ("https://x.dev/w".into(), sha.clone()),
        );
        let e = pinned_tool(None, &lock, "wasmtime", "9.9.9").unwrap_err();
        assert!(e.contains(&sha), "{e}");
        assert!(e.contains("not cached"), "{e}");
        assert!(e.contains("`vyrn update wasmtime`"), "{e}");
        // Both refusals name the escape hatch: the integration test asserts it
        // outside its platform branch, and x86_64-linux is where that bit.
        assert!(e.contains("$VYRN_WASMTIME"), "{e}");
    }
    /// The sha joins into `~/.vyrn/tools/<sha>/` on every path below, so a
    /// corrupt or hand-edited lock refuses before any join — it never reaches
    /// outside the tools directory, and never falls through to the cache.
    #[test]
    fn a_lock_sha_that_is_not_a_sha256_refuses_before_any_path_join() {
        assert!(is_sha256(&"a".repeat(64)));
        assert!(is_sha256(&"0f".repeat(32)));
        assert!(!is_sha256(&"A".repeat(64)), "uppercase is not the lock's spelling");
        assert!(!is_sha256(&"a".repeat(63)), "short is not full width");
        assert!(!is_sha256("../../escape"));
        assert!(!is_sha256(""));

        let dir = std::env::temp_dir().join("vyrn-toolpin-traversal");
        std::fs::create_dir_all(&dir).unwrap();
        let mut lock = Lock::load(dir.join("vyrn.lock")).unwrap();
        lock.entries.insert(
            tool_spec("wasmtime", "9.9.9", &host_platform()),
            ("https://x.dev/w".into(), "../../escape".into()),
        );
        let e = pinned_tool(None, &lock, "wasmtime", "9.9.9").unwrap_err();
        assert!(e.contains("not a sha256 digest"), "{e}");
        assert!(e.contains("corrupt or hand-edited"), "{e}");
    }

    /// A directory in the tools cache is trusted only when it certifies itself:
    /// the marker `unpack_tool` writes after the archive is fully placed. A
    /// directory that is merely there — interrupted unpack, hand-made,
    /// pre-marker layout — reads as absent and gets re-unpacked.
    #[test]
    fn an_unpacked_tool_directory_is_trusted_only_when_it_certifies_itself() {
        let root = std::env::temp_dir().join(format!("vyrn-toolpin-marker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let sha = "b".repeat(64);

        // Not there at all.
        assert!(!verified(&root, &sha));
        // There, but uncertified.
        assert!(std::fs::write(root.join("wasmtime.exe"), b"x").is_ok());
        assert!(!verified(&root, &sha));
        // Certified for another sha: still not this one's unpack.
        assert!(std::fs::write(root.join(SHA_MARKER), &"c".repeat(64)).is_ok());
        assert!(!verified(&root, &sha));
        // Certified.
        assert!(std::fs::write(root.join(SHA_MARKER), format!("{sha}\n")).is_ok());
        assert!(verified(&root, &sha));

        // And unpack_tool refuses a sha that cannot even form a key.
        assert!(unpack_tool("../../x", b"bytes").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
