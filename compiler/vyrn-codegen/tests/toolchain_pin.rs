//! RFC-0102 M1 and M2: the discovery order, end to end, with no network.
//!
//! A fabricated archive is hashed, dropped into the content-addressed cache the
//! way a fetch would leave it, and named by a `tool:` line in a `vyrn.lock`. The
//! resolver then has to reach the unpacked binary through the pin alone — and
//! has to prefer an environment override to it, and has to FAIL rather than fall
//! through to PATH when a pin cannot be resolved.
//!
//! One `#[test]`, deliberately: the order's first step is an environment
//! variable, and two tests disagreeing about `VYRN_WASMTIME` in one process is
//! the flake that would follow from splitting it. M2's two tools joined the same
//! test for the same reason, one variable each.
//!
//! Nothing here has a host-only branch that skips the check: `wasi-sysroot` and
//! `wasi-builtins` are pinned `/any`, so the pinned path below executes on all
//! four platforms.

use std::path::{Path, PathBuf};
use vyrn_codegen::toolchain::{
    clang_from, shim_key_clang_component, wasi_builtins_from, wasi_sysroot_from, wasmtime_from,
    BUILTINS_A,
};
use vyrn_frontend::manifest::{cache_dir, write_blob};
use vyrn_frontend::toolpin::{host_platform, tool_spec, tools_dir};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("vyrn-toolpin-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// A `.tar` holding `files`, each with `content` in it — the shape a release
/// archive has, payload inside one version-named directory. Built with `tar`
/// because unpacking uses `tar`, and an archive this repository cannot produce is
/// not evidence about one it fetches.
fn fake_archive(tag: &str, files: &[String], content: &[u8]) -> Vec<u8> {
    let stage = tmp(tag);
    for f in files {
        let p = stage.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
    }
    // Beside the staging directory, not inside it: `tar -C stage .` would
    // otherwise archive the archive it is writing.
    let archive = std::env::temp_dir().join(format!("vyrn-toolpin-{tag}.tar"));
    let st = std::process::Command::new("tar")
        .args(["-cf", &slash(&archive), "-C", &slash(&stage), "."])
        .status()
        .expect("tar is on PATH (Windows 10+, every Linux userland, macOS)");
    assert!(st.success(), "tar -cf failed");
    std::fs::read(&archive).unwrap()
}

/// Hash a fabricated archive and leave it where a fetch would: the
/// content-addressed cache, plus the `tool:` line naming it.
fn seed(lock: &mut String, name: &str, version: &str, platform: &str, bytes: &[u8]) -> String {
    let sha = vyrn_frontend::hash::sha256_hex(bytes);
    write_blob(&cache_dir(), &sha, bytes).unwrap();
    lock.push_str(&format!(
        "{}\thttps://example.invalid/{name}-{version}\t{sha}\n",
        tool_spec(name, version, platform)
    ));
    sha
}

/// A `wasmtime-v<ver>-<platform>/wasmtime[.exe]` release archive.
fn fake_release(version: &str) -> Vec<u8> {
    let exe = if cfg!(windows) {
        "wasmtime.exe"
    } else {
        "wasmtime"
    };
    fake_archive(
        "stage",
        &[format!("wasmtime-v{version}-{}/{exe}", host_platform())],
        b"not really a runtime",
    )
}

#[test]
fn the_pin_resolves_offline_and_the_order_is_env_then_pin_then_walk() {
    // The parity harness exports this; the pin steps below have to be measured
    // without it, and step 1 puts it back.
    let saved = std::env::var("VYRN_WASMTIME").ok();
    std::env::remove_var("VYRN_WASMTIME");

    // --- a pinned tool, seeded the way a fetch would leave it ----------------
    let project = tmp("project");
    std::fs::write(
        project.join("vyrn.json"),
        r#"{"toolchain":{"wasmtime":"9.9.9"}}"#,
    )
    .unwrap();
    let bytes = fake_release("9.9.9");
    let sha = vyrn_frontend::hash::sha256_hex(&bytes);
    write_blob(&cache_dir(), &sha, &bytes).unwrap();
    std::fs::write(
        project.join("vyrn.lock"),
        format!(
            "{}\thttps://example.invalid/wasmtime-9.9.9\t{sha}\n",
            tool_spec("wasmtime", "9.9.9", &host_platform())
        ),
    )
    .unwrap();

    let (path, why) = wasmtime_from(&project)
        .expect("a pinned tool whose bytes are cached resolves with no network")
        .expect("and it is found");
    assert_eq!(why, "pinned");
    assert!(path.is_file(), "{}", path.display());
    assert!(
        slash(&path).starts_with(&slash(&tools_dir().join(&sha))),
        "the pin resolves INSIDE ~/.vyrn/tools/<sha>/: {}",
        path.display()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"not really a runtime");

    // --- a pin that cannot be resolved FAILS; it never falls back to PATH ----
    let unresolvable = tmp("unresolvable");
    std::fs::write(
        unresolvable.join("vyrn.json"),
        r#"{"toolchain":{"wasmtime":"46.0.1"}}"#,
    )
    .unwrap();
    std::fs::write(
        unresolvable.join("vyrn.lock"),
        format!(
            "{}\thttps://example.invalid/w\t{}\n",
            tool_spec("wasmtime", "46.0.1", "x86_64-linux"),
            "b".repeat(64)
        ),
    )
    .unwrap();
    let e = wasmtime_from(&unresolvable).unwrap_err();
    if host_platform() == "x86_64-linux" {
        // The one host the fabricated lock does cover: the bytes are missing,
        // not the platform, so the OTHER refusal is the one to prove.
        assert!(e.contains("not cached"), "{e}");
        assert!(e.contains(&"b".repeat(64)), "{e}");
    } else {
        assert!(e.contains("wasmtime 46.0.1 is pinned"), "{e}");
        assert!(
            e.contains(&format!("no entry for {}", host_platform())),
            "{e}"
        );
        assert!(e.contains("Pinned platforms: x86_64-linux."), "{e}");
    }
    assert!(e.contains("$VYRN_WASMTIME"), "{e}");

    // --- no pin: the `tools/` walk, exactly as before this key existed -------
    let unpinned = tmp("unpinned");
    std::fs::write(unpinned.join("vyrn.json"), r#"{"main":"src/main.vyrn"}"#).unwrap();
    let found = wasmtime_from(&unpinned).expect("no pin is not an error");
    // What this row proves is that no pin is no refusal and no pinned path: the
    // resolver falls through to the walk. What the WALK then answers is a fact
    // about the machine, not about the resolver — the test scratch moved under
    // `compiler/target` so that two worktrees can gate at once, and this
    // checkout's own `tools/` is above it, where a system temp directory had
    // none.
    assert!(
        found
            .as_ref()
            .map_or(true, |(_, why)| *why == "discovered: tools/"),
        "no pin must fall through to the `tools/` walk: {found:?}"
    );

    // --- step 1: the environment override beats the pin ----------------------
    let hatch = project.join("my-own-wasmtime");
    std::fs::write(&hatch, b"whatever the developer trusts").unwrap();
    std::env::set_var("VYRN_WASMTIME", &hatch);
    let (path, why) = wasmtime_from(&project).unwrap().unwrap();
    assert_eq!(why, "override: environment");
    assert_eq!(path, hatch);
    // And it beats a pin that would otherwise refuse, which is what makes it an
    // escape hatch rather than a preference.
    assert_eq!(wasmtime_from(&unresolvable).unwrap().unwrap().0, hatch);

    match saved {
        Some(v) => std::env::set_var("VYRN_WASMTIME", v),
        None => std::env::remove_var("VYRN_WASMTIME"),
    }

    sysroot_and_builtins();
    clang_is_recorded_not_pinned();
}

/// RFC-0102 M3: the one tool that is discovered rather than pinned still has to
/// say what it is — the version, the path, and why that path.
///
/// Called from the one `#[test]` for the reason [`sysroot_and_builtins`] is:
/// `$CLANG` is step 1 here too, and a second test setting it beside this one is
/// the same race.
///
/// No host-only branch: a machine with clang and one without both get an
/// assertion, and the key component is a pure function of a string.
fn clang_is_recorded_not_pinned() {
    // The key component IS the version: two versions, two keys, and the same
    // version twice is the same key — which is what makes the cache hit correct
    // and the upgrade a miss (Exhibit 5).
    let a = shim_key_clang_component("clang version 22.1.0");
    let b = shim_key_clang_component("clang version 23.0.0");
    assert_ne!(a, b, "a clang upgrade has to change the key");
    assert_eq!(a, shim_key_clang_component("clang version 22.1.0"));
    assert!(a.starts_with("clang"), "{a}");

    match clang_from() {
        Some((path, version, why)) => {
            assert!(!version.is_empty());
            // Whatever the vendor prints, trimmed to its first line: Apple,
            // Ubuntu and upstream word it differently and none is normalized.
            assert_eq!(version, version.trim());
            assert!(!version.contains('\n'), "{version}");
            assert!(
                version.contains("clang") || version == "unknown",
                "the probe reports its own first line: {version}"
            );
            assert!(
                why.starts_with("discovered: ") || why == "override: environment",
                "{why}"
            );
            assert!(!path.as_os_str().is_empty());
            // The path this reports is the path `find_clang` runs.
            assert_eq!(Some(path), vyrn_codegen::toolchain::find_clang());
        }
        // A machine with no clang is a machine where the shim does not compile,
        // and that is unchanged: `shim_wasm` answers `None`, as it did when the
        // key had no compiler in it.
        None => assert!(vyrn_codegen::toolchain::find_clang().is_none()),
    }
}

/// RFC-0102 M2: the same order, for the two `/any` tools, and the answer each
/// consumer actually needs — a DIRECTORY to point `--sysroot=` at, and the `.a`
/// FILE the link line names.
///
/// Called from the one `#[test]` rather than being a second: `cargo test` runs
/// tests in threads of one process, and `set_var` beside another thread's
/// `getenv` — `cache_dir()` reads `HOME` on every resolve — is a race whether or
/// not the two agree about which variable they are setting.
fn sysroot_and_builtins() {
    // CI's codegen step exports both; the pin steps have to be measured without
    // them, and the last step puts them back.
    let saved = [
        std::env::var("WASI_SYSROOT").ok(),
        std::env::var("WASI_BUILTINS").ok(),
    ];
    std::env::remove_var("WASI_SYSROOT");
    std::env::remove_var("WASI_BUILTINS");

    let project = tmp("wasi");
    std::fs::write(
        project.join("vyrn.json"),
        r#"{"toolchain":{"wasi-sysroot":"9.9.9","wasi-builtins":"9.9.9"}}"#,
    )
    .unwrap();
    let mut lock = String::new();
    // `/any`, because both are wasm32 TARGET libraries: the same file on every
    // host, and one lock line that every platform reads.
    let sys_sha = seed(
        &mut lock,
        "wasi-sysroot",
        "9.9.9",
        "any",
        &fake_archive(
            "sysroot",
            &[
                "wasi-sysroot-9.9.9/include/stdio.h".into(),
                "wasi-sysroot-9.9.9/lib/wasm32-wasip1/libc.a".into(),
            ],
            b"not really a sysroot",
        ),
    );
    seed(
        &mut lock,
        "wasi-builtins",
        "9.9.9",
        "any",
        &fake_archive(
            "builtins",
            &[format!(
                "libclang_rt.builtins-wasm32-wasi-9.9.9/{BUILTINS_A}"
            )],
            b"not really an archive",
        ),
    );
    std::fs::write(project.join("vyrn.lock"), &lock).unwrap();

    let (sysroot, why) = wasi_sysroot_from(&project).unwrap().unwrap();
    assert_eq!(why, "pinned");
    // The directory a consumer points clang at, one level INSIDE the blob's
    // `<sha>` — the unpacked-layout answer.
    assert!(
        slash(&sysroot).starts_with(&slash(&tools_dir().join(&sys_sha))),
        "{}",
        sysroot.display()
    );
    assert_eq!(sysroot.file_name().unwrap(), "wasi-sysroot-9.9.9");
    assert!(sysroot.join("include").is_dir(), "{}", sysroot.display());

    let (builtins, why) = wasi_builtins_from(&project, &sysroot).unwrap().unwrap();
    assert_eq!(why, "pinned");
    assert!(builtins.is_file(), "{}", builtins.display());
    assert_eq!(builtins.file_name().unwrap(), BUILTINS_A);
    // And it came from its OWN blob, not from beside the sysroot: two pins, two
    // hashes, and `builtins_near_sysroot` is step 3 only.
    assert!(!slash(&builtins).starts_with(&slash(&tools_dir().join(&sys_sha))));

    // --- a pin that cannot be resolved FAILS, and names its escape hatch ------
    let uncached = tmp("wasi-uncached");
    std::fs::write(
        uncached.join("vyrn.json"),
        r#"{"toolchain":{"wasi-sysroot":"8.8.8","wasi-builtins":"8.8.8"}}"#,
    )
    .unwrap();
    std::fs::write(
        uncached.join("vyrn.lock"),
        format!(
            "{}\thttps://example.invalid/s\t{}\n",
            tool_spec("wasi-sysroot", "8.8.8", "any"),
            "c".repeat(64)
        ),
    )
    .unwrap();
    let e = wasi_sysroot_from(&uncached).unwrap_err();
    assert!(e.contains("not cached"), "{e}");
    assert!(e.contains(&"c".repeat(64)), "{e}");
    assert!(e.contains("`vyrn update wasi-sysroot`"), "{e}");
    assert!(e.contains("$WASI_SYSROOT"), "{e}");
    // The builtins are pinned with no lock line at all: the other refusal.
    let e = wasi_builtins_from(&uncached, &sysroot).unwrap_err();
    assert!(e.contains("wasi-builtins 8.8.8 is pinned"), "{e}");
    assert!(e.contains("no entry for any"), "{e}");
    assert!(e.contains("Pinned platforms: none."), "{e}");
    assert!(e.contains("$WASI_BUILTINS"), "{e}");

    // --- no pin: the `tools/` walk, exactly as before this key existed -------
    let unpinned = tmp("wasi-unpinned");
    std::fs::write(unpinned.join("vyrn.json"), r#"{"main":"src/main.vyrn"}"#).unwrap();
    assert!(wasi_sysroot_from(&unpinned).unwrap().is_none());
    assert!(wasi_builtins_from(&unpinned, &sysroot).unwrap().is_none());

    // --- step 1: the environment override beats the pin, and beats a refusal --
    std::env::set_var("WASI_SYSROOT", &sysroot);
    std::env::set_var("WASI_BUILTINS", &builtins);
    for dir in [&project, &uncached, &unpinned] {
        assert_eq!(
            wasi_sysroot_from(dir).unwrap().unwrap(),
            (sysroot.clone(), "override: environment")
        );
        assert_eq!(
            wasi_builtins_from(dir, &sysroot).unwrap().unwrap(),
            (builtins.clone(), "override: environment")
        );
    }
    // Both spellings of the override reach the same `.a`: CI exports the file,
    // and the directory the tool unpacks to is what a developer has in hand.
    std::env::set_var("WASI_BUILTINS", builtins.parent().unwrap());
    assert_eq!(
        wasi_builtins_from(&unpinned, &sysroot).unwrap().unwrap(),
        (builtins.clone(), "override: environment")
    );

    for (var, v) in ["WASI_SYSROOT", "WASI_BUILTINS"].iter().zip(saved) {
        match v {
            Some(v) => std::env::set_var(var, v),
            None => std::env::remove_var(var),
        }
    }
}
