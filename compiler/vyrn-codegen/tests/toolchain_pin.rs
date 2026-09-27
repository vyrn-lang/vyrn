//! The tool discovery order, end to end, with no network.
//!
//! A fabricated archive is cached the way a fetch leaves it and named by a
//! `tool:` line in a `vyrn.lock`. The resolver must reach it through the pin
//! alone, prefer an environment override to it, and fail rather than fall
//! through to PATH when a pin cannot be resolved.
//!
//! One `#[test]` on purpose: step 1 is an environment variable, and two tests
//! setting `VYRN_WASMTIME` in one process would race.

use std::path::{Path, PathBuf};
use vyrn_codegen::toolchain::{clang_from, wasmtime_from};
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

/// A `.tar` holding `files`, each with `content`. Built with `tar` because
/// unpacking uses `tar`.
fn fake_archive(tag: &str, files: &[String], content: &[u8]) -> Vec<u8> {
    let stage = tmp(tag);
    for f in files {
        let p = stage.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
    }
    // Beside the staging directory: inside it, `tar -C stage .` would archive
    // the archive it is writing.
    let archive = std::env::temp_dir().join(format!("vyrn-toolpin-{tag}.tar"));
    let st = std::process::Command::new("tar")
        .args(["-cf", &slash(&archive), "-C", &slash(&stage), "."])
        .status()
        .expect("tar is on PATH (Windows 10+, every Linux userland, macOS)");
    assert!(st.success(), "tar -cf failed");
    std::fs::read(&archive).unwrap()
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
    // The parity harness exports this; the pin steps run without it.
    let saved = std::env::var("VYRN_WASMTIME").ok();
    std::env::remove_var("VYRN_WASMTIME");

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
        // The lock covers this host, so the bytes are missing, not the platform.
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

    let unpinned = tmp("unpinned");
    std::fs::write(unpinned.join("vyrn.json"), r#"{"main":"src/main.vyrn"}"#).unwrap();
    let found = wasmtime_from(&unpinned).expect("no pin is not an error");
    // Without a pin the resolver falls through to the `tools/` walk. What the walk
    // finds depends on the machine: a scratch under `compiler/target` sees this
    // checkout's `tools/`.
    assert!(
        found
            .as_ref()
            .map_or(true, |(_, why)| *why == "discovered: tools/"),
        "no pin must fall through to the `tools/` walk: {found:?}"
    );

    let hatch = project.join("my-own-wasmtime");
    std::fs::write(&hatch, b"whatever the developer trusts").unwrap();
    std::env::set_var("VYRN_WASMTIME", &hatch);
    let (path, why) = wasmtime_from(&project).unwrap().unwrap();
    assert_eq!(why, "override: environment");
    assert_eq!(path, hatch);
    // It also beats a pin that would refuse: an escape hatch, not a preference.
    assert_eq!(wasmtime_from(&unresolvable).unwrap().unwrap().0, hatch);

    match saved {
        Some(v) => std::env::set_var("VYRN_WASMTIME", v),
        None => std::env::remove_var("VYRN_WASMTIME"),
    }

    clang_is_recorded_not_pinned();
}

/// Clang is discovered, not pinned, but still reports its version, its path and
/// why that path.
///
/// Called from the one `#[test]`: `$CLANG` is step 1 here too, so a second test
/// would race on it.
fn clang_is_recorded_not_pinned() {
    match clang_from() {
        Some((path, version, why)) => {
            assert!(!version.is_empty());
            // The vendor's first line, trimmed; vendors word it differently.
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
            assert_eq!(Some(path), vyrn_codegen::toolchain::find_clang());
        }
        // Without clang the native route fails at the link step, not here.
        None => assert!(vyrn_codegen::toolchain::find_clang().is_none()),
    }
}
