//! Pinned toolchains: the table that turns a tool name and version
//! into a URL, and the resolver that turns a `tool:` line in `vyrn.lock` into
//! an unpacked directory under `~/.vyrn/tools/<sha256>/`.
//!
//! A tool is a dependency: named in `vyrn.json`, frozen in `vyrn.lock` under
//! the opaque specifier `tool:<name>@<version>/<platform>`, stored by content
//! hash and verified on every load, as [`crate::manifest`] does for a module.
//! Resolution never touches the network: fetching is `vyrn update <tool>`'s,
//! in the driver, because the editor reads pins too.

use crate::manifest::{cache_dir, pinned_blob_bytes, Lock};
use std::path::{Path, PathBuf};

/// The platforms a native tool artifact is published for, in `install.sh`'s
/// vocabulary.
const PLATFORMS: [&str; 4] = [
    "x86_64-linux",
    "aarch64-linux",
    "aarch64-macos",
    "x86_64-windows",
];

/// The tools the table knows. Any other name is refused, never looked up on
/// PATH.
pub const KNOWN_TOOLS: [&str; 4] = [
    "wasmtime",
    "cargo-nextest",
    // The native route's two: `wasm2c` from a wabt release, and the
    // SIMD header its output includes.
    "wabt",
    "simde",
];

/// Returns this machine in [`PLATFORMS`]' vocabulary, which Rust's `ARCH` and
/// `OS` constants already spell. A host outside it (`aarch64-windows`) still
/// gets a name, so the refusal can say which platform has no entry.
pub fn host_platform() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

/// Returns the lock specifier for one tool artifact.
pub fn tool_spec(name: &str, version: &str, platform: &str) -> String {
    format!("tool:{name}@{version}/{platform}")
}

/// Returns the platforms a tool publishes one artifact each for. `any` is a
/// real value: simde's headers are the same files on every host.
pub fn tool_platforms(name: &str) -> &'static [&'static str] {
    match name {
        "simde" => &["any"],
        // wabt publishes one binary release per platform, each pinned by sha256.
        // `vyrn update --locked` checks the sha256 the release publishes beside each
        // asset.
        _ => &PLATFORMS,
    }
}

/// Returns the environment variable that overrides a tool, so a refusal can
/// name it. Empty for a tool no compiler code resolves; see [`escape_hatch`].
fn tool_env_var(name: &str) -> &'static str {
    match name {
        "wasmtime" => "VYRN_WASMTIME",
        "wabt" => "VYRN_WASM2C",
        "simde" => "VYRN_SIMDE",
        _ => "",
    }
}

/// Returns the clause that names a refusal's escape hatch, or nothing for a
/// tool without one. `cargo-nextest` is CI's test runner, put on PATH by the
/// workflow; nothing here looks for it, so the refusal invents no variable
/// that no reader honours.
fn escape_hatch(name: &str) -> String {
    match tool_env_var(name) {
        "" => String::new(),
        v => format!(", or point ${v} at a binary you trust"),
    }
}

/// Returns the refusal for a tool name the table does not know.
pub fn unknown_tool(name: &str) -> String {
    format!(
        "unknown tool `{name}` in vyrn.json's `toolchain` — the tools vyrn can pin are {}",
        KNOWN_TOOLS.join(", ")
    )
}

/// Returns the published artifact's URL for a name, version and platform. An
/// unknown name is [`unknown_tool`].
pub fn tool_url(name: &str, version: &str, platform: &str) -> Result<String, String> {
    match name {
        // Windows ships a zip and every other platform a tar.xz; `tar` reads both.
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
        // One `.tar.gz` per Rust target triple, Windows included. macOS has one
        // universal binary, so `aarch64-macos` maps to `universal-apple-darwin`.
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
        // wabt names its assets `<os>-<arch>`, so the mapping is a table and an
        // unknown platform is refused.
        "wabt" => {
            let asset = match platform {
                "x86_64-linux" => "linux-x64",
                "aarch64-linux" => "linux-arm64",
                "aarch64-macos" => "macos-arm64",
                "x86_64-windows" => "windows-x64",
                other => return Err(format!("this table records no wabt asset for {other}")),
            };
            Ok(format!(
                "https://github.com/WebAssembly/wabt/releases/download/{version}/wabt-{version}-{asset}.tar.gz"
            ))
        }
        // A source tag: simde is a header library, and the route needs only
        // `simde/wasm/simd128.h`.
        "simde" => Ok(format!(
            "https://github.com/simd-everywhere/simde/archive/refs/tags/v{version}.tar.gz"
        )),
        _ => Err(unknown_tool(name)),
    }
}

/// Returns where unpacked tools live: `~/.vyrn/tools/<sha256>/`. Each is
/// derived from a verified blob and may be deleted at any time.
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

/// The file that certifies which sha an unpacked tool directory holds,
/// written after the archive is fully placed. Any process of the user can
/// write the tools directory, so an uncertified directory counts as absent.
const SHA_MARKER: &str = ".vyrn-sha";

/// Returns whether `sha` is full-width lowercase hex. The sha joins into a
/// path, so a corrupt or hand-edited lock must refuse, never traverse.
fn is_sha256(sha: &str) -> bool {
    sha.len() == 64 && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Returns whether `dir` is a completed unpack of `sha`: it carries the marker
/// [`unpack_tool`] writes last.
fn verified(dir: &Path, sha: &str) -> bool {
    std::fs::read_to_string(dir.join(SHA_MARKER))
        .map_or(false, |recorded| recorded.trim_end() == sha)
}

/// Unpacks a verified archive to `~/.vyrn/tools/<sha>/`, or returns the
/// directory if it already certifies itself (see [`verified`]).
///
/// It runs `tar`, which every supported host ships (Windows 10 and later,
/// Linux, macOS); `tar -xf` detects `.tar.gz`, `.tar.xz` and `.zip`.
fn unpack_tool(sha: &str, bytes: &[u8]) -> Result<PathBuf, String> {
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
    // A `.gate` file held across place and certify serializes them per hash:
    // without it, a loser could see the winner's rename before its marker and
    // delete the winner's directory. If the gate cannot be created, this process
    // places without it.
    let gate = tools_dir().join(format!("{sha}.gate"));
    let gated = hold_gate(&gate);
    let placed = place_staged_tool(&stage, &out, sha);
    if gated {
        let _ = std::fs::remove_file(&gate);
    }
    placed?;
    Ok(out)
}

/// Creates `gate`, waiting up to 60 s while another process holds it. Returns
/// whether this process holds it; a gate left by a killed process costs one
/// wait, then the caller places without it.
fn hold_gate(gate: &Path) -> bool {
    for _ in 0..600 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(gate)
        {
            Ok(_) => return true,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(_) => return false,
        }
    }
    false
}

/// Moves the staged copy to `out` and writes the certification marker,
/// replacing an uncertified directory. The caller serializes per hash (see
/// [`unpack_tool`]).
fn place_staged_tool(stage: &Path, out: &Path, sha: &str) -> Result<(), String> {
    if std::fs::rename(stage, out).is_err() {
        if verified(out, sha) {
            // Another process placed the same bytes first.
            let _ = std::fs::remove_dir_all(stage);
        } else {
            // An uncertified directory holds the name: an interrupted unpack or a
            // hand-made one. Replace it with the staged copy, which this process can
            // certify.
            let _ = std::fs::remove_dir_all(out);
            if std::fs::rename(stage, out).is_err() {
                let _ = std::fs::remove_dir_all(stage);
                return Err(format!("cannot place the unpacked tool at {}", slash(out)));
            }
        }
    }
    // The marker goes last, so a reader that finds it can trust the tree beside
    // it.
    std::fs::write(out.join(SHA_MARKER), sha)
        .map_err(|e| format!("cannot mark {} as verified: {e}", slash(out)))
}

/// Returns the unpacked directory of a pinned tool, resolved through the lock,
/// then the vendor directory, then the user cache; never the network or
/// PATH.
///
/// Each refusal says what to do: no lock entry for this platform, no bytes
/// for the locked hash, or an archive that does not unpack.
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
    // The sha joins into a path below, so a bad one refuses before any join.
    if !is_sha256(sha) {
        return Err(format!(
            "{name} {version} is pinned with sha `{sha}`, which is not a sha256 digest — \
             vyrn.lock is corrupt or hand-edited."
        ));
    }
    let out = tools_dir().join(sha);
    // An uncertified directory counts as absent and is re-unpacked from the
    // pinned bytes.
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

/// Returns `name` at the top of an unpacked tool directory or one level in,
/// where release archives put a version-named directory. Sorted, so several
/// candidates pick deterministically.
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

/// Returns the first file named `what` (or `what.exe` on Windows) at the top
/// of an unpacked tool directory or one level in.
pub fn tool_binary(dir: &Path, what: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{what}.exe")
    } else {
        what.to_string()
    };
    at_top_or_one_in(dir, &exe).filter(|p| p.is_file())
}

/// Returns the directory that contains `marker`, such as
/// `~/.vyrn/tools/<sha>/wabt-1.0.41`: a consumer wants the tree with `bin/`
/// in it.
pub fn tool_root(dir: &Path, marker: &str) -> Option<PathBuf> {
    at_top_or_one_in(dir, marker)?
        .parent()
        .map(|p| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// cargo-nextest names a target triple, uses `.tar.gz` everywhere, and has one
    /// universal macOS artifact.
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
        // Every host in the vocabulary is covered, and a host outside it is refused.
        assert_eq!(tool_platforms("cargo-nextest"), &PLATFORMS);
        assert_eq!(
            tool_url("cargo-nextest", "0.9.143", "aarch64-windows").unwrap_err(),
            "cargo-nextest publishes no artifact for aarch64-windows"
        );
        // No Rust code resolves it, so the refusal names no variable.
        assert_eq!(tool_env_var("cargo-nextest"), "");
        assert_eq!(escape_hatch("cargo-nextest"), "");
        assert_eq!(
            escape_hatch("wasmtime"),
            ", or point $VYRN_WASMTIME at a binary you trust"
        );
    }

    #[test]
    fn the_table_knows_its_tools_and_refuses_the_rest() {
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

        // An unknown name lists the known ones and never falls through to PATH.
        let e = tool_url("wasm-opt", "1", "x86_64-linux").unwrap_err();
        assert_eq!(
            e,
            "unknown tool `wasm-opt` in vyrn.json's `toolchain` — the tools vyrn can pin \
             are wasmtime, cargo-nextest, wabt, simde"
        );

        // wabt names its assets `<os>-<arch>`, and a platform outside the table is
        // refused.
        assert_eq!(
            tool_url("wabt", "1.0.41", "x86_64-windows").unwrap(),
            "https://github.com/WebAssembly/wabt/releases/download/1.0.41/wabt-1.0.41-windows-x64.tar.gz"
        );
        assert_eq!(
            tool_url("wabt", "1.0.41", "x86_64-linux").unwrap(),
            "https://github.com/WebAssembly/wabt/releases/download/1.0.41/wabt-1.0.41-linux-x64.tar.gz"
        );
        assert!(tool_url("wabt", "1.0.41", "riscv64-linux").is_err());
        assert_eq!(tool_platforms("wabt"), PLATFORMS);
        assert_eq!(
            tool_url("simde", "0.8.2", "any").unwrap(),
            "https://github.com/simd-everywhere/simde/archive/refs/tags/v0.8.2.tar.gz"
        );
        assert_eq!(tool_platforms("simde"), ["any"]);
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

    /// A `tool:` line rides the existing lock reader and writer with no format
    /// change: the specifier is opaque to `Lock::load`.
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
    /// lock covers, the command that adds one, and the escape hatch.
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
        // On a pinned host the pin resolves as far as the cache, which the next test
        // covers.
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

    /// A pin whose bytes are nowhere fails and names the hash that would satisfy
    /// it.
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
        // Both refusals name the escape hatch.
        assert!(e.contains("$VYRN_WASMTIME"), "{e}");
    }
    /// A sha that is not a sha256 refuses before any path join.
    #[test]
    fn a_lock_sha_that_is_not_a_sha256_refuses_before_any_path_join() {
        assert!(is_sha256(&"a".repeat(64)));
        assert!(is_sha256(&"0f".repeat(32)));
        assert!(
            !is_sha256(&"A".repeat(64)),
            "uppercase is not the lock's spelling"
        );
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

    /// An unpacked directory is trusted only when its marker certifies it.
    #[test]
    fn an_unpacked_tool_directory_is_trusted_only_when_it_certifies_itself() {
        let root = std::env::temp_dir().join(format!("vyrn-toolpin-marker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let sha = "b".repeat(64);

        // Absent.
        assert!(!verified(&root, &sha));
        // Present but uncertified.
        assert!(std::fs::write(root.join("wasmtime.exe"), b"x").is_ok());
        assert!(!verified(&root, &sha));
        // Certified for another sha.
        assert!(std::fs::write(root.join(SHA_MARKER), &"c".repeat(64)).is_ok());
        assert!(!verified(&root, &sha));
        // Certified.
        assert!(std::fs::write(root.join(SHA_MARKER), format!("{sha}\n")).is_ok());
        assert!(verified(&root, &sha));

        // `unpack_tool` refuses a sha that cannot form a key.
        assert!(unpack_tool("../../x", b"bytes").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A second unpack of the same hash waits for the first to release the gate.
    #[test]
    fn a_held_gate_makes_the_next_unpack_wait() {
        let dir = std::env::temp_dir().join("vyrn-toolpin-gate");
        std::fs::create_dir_all(&dir).unwrap();
        let gate = dir.join("x.gate");
        std::fs::write(&gate, "").unwrap();
        let held = gate.clone();
        // Taken before the spawn: a delay between the spawn and this line
        // would otherwise shorten the measured wait below 300 ms.
        let start = std::time::Instant::now();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            std::fs::remove_file(held).unwrap();
        });
        assert!(hold_gate(&gate));
        assert!(start.elapsed() >= std::time::Duration::from_millis(300));
        release.join().unwrap();
        std::fs::remove_file(&gate).unwrap();
    }
}
