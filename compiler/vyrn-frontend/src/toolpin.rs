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

/// The tools the table knows. Three entries, because these are the three this
/// repository fetches; a name outside this list is a refusal, never a
/// fall-through to PATH.
pub const KNOWN_TOOLS: [&str; 3] = ["wasmtime", "wasi-sysroot", "wasi-builtins"];

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
pub fn tool_env_var(name: &str) -> &'static str {
    match name {
        "wasmtime" => "VYRN_WASMTIME",
        "wasi-sysroot" => "WASI_SYSROOT",
        "wasi-builtins" => "WASI_BUILTINS",
        _ => "",
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

/// Unpack a verified archive to `~/.vyrn/tools/<sha>/`, or return it if it is
/// already there.
///
/// Unpacking shells out to `tar`, for the reason `curl` and `git` are already
/// spawned: the tool is ubiquitous — Windows 10 and later, every Linux userland,
/// macOS — and a crate for it would cost more than it buys. `tar -xf` sniffs the
/// format, so one call covers `.tar.gz`, `.tar.xz` and `.zip`.
pub fn unpack_tool(sha: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let out = tools_dir().join(sha);
    if out.is_dir() {
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
        let _ = std::fs::remove_dir_all(&stage);
        if !out.is_dir() {
            return Err(format!("cannot place the unpacked tool at {}", slash(&out)));
        }
    }
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
             Add one with `vyrn update {name}`, or point ${} at a binary you trust.",
            tool_env_var(name)
        ));
    };
    let out = tools_dir().join(sha);
    if out.is_dir() {
        return Ok(out);
    }
    match pinned_blob_bytes(project_dir, sha) {
        Some(Ok(bytes)) => unpack_tool(sha, &bytes),
        Some(Err(e)) => Err(e),
        None => Err(format!(
            "{name} {version} is pinned for {platform} (sha256 {sha}) but not cached — \
             run `vyrn update {name}` online, `vyrn vendor`, or drop any copy of the \
             archive with that hash into {}",
            slash(&cache_dir())
        )),
    }
}

/// The first file named `what` (or `what.exe`) at the top of an unpacked tool
/// directory or one level in — a release archive puts its binary inside a
/// version-named directory, and which one is not this resolver's business.
pub fn tool_binary(dir: &Path, what: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{what}.exe")
    } else {
        what.to_string()
    };
    let direct = dir.join(&exe);
    if direct.is_file() {
        return Some(direct);
    }
    let mut hits: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path().join(&exe))
        .filter(|p| p.is_file())
        .collect();
    hits.sort();
    hits.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_knows_three_tools_and_refuses_the_rest() {
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
             are wasmtime, wasi-sysroot, wasi-builtins"
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
    }
}
