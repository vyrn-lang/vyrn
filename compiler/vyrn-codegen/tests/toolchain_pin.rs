//! RFC-0102 M1: the discovery order, end to end, with no network.
//!
//! A fabricated archive is hashed, dropped into the content-addressed cache the
//! way a fetch would leave it, and named by a `tool:` line in a `vyrn.lock`. The
//! resolver then has to reach the unpacked binary through the pin alone — and
//! has to prefer an environment override to it, and has to FAIL rather than fall
//! through to PATH when a pin cannot be resolved.
//!
//! One `#[test]`, deliberately: the order's first step is an environment
//! variable, and two tests disagreeing about `VYRN_WASMTIME` in one process is
//! the flake that would follow from splitting it.

use std::path::{Path, PathBuf};
use vyrn_codegen::toolchain::wasmtime_from;
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

/// A `.tar` holding `wasmtime-v<ver>-<platform>/wasmtime[.exe]`, the shape a
/// release archive has. Built with `tar` because unpacking uses `tar`, and an
/// archive this repository cannot produce is not evidence about one it fetches.
fn fake_release(version: &str) -> Vec<u8> {
    let stage = tmp("stage");
    let inner = stage.join(format!("wasmtime-v{version}-{}", host_platform()));
    std::fs::create_dir_all(&inner).unwrap();
    let exe = if cfg!(windows) {
        "wasmtime.exe"
    } else {
        "wasmtime"
    };
    std::fs::write(inner.join(exe), b"not really a runtime").unwrap();
    let archive = stage.join("release.tar");
    let st = std::process::Command::new("tar")
        .args(["-cf", &slash(&archive), "-C", &slash(&stage), "."])
        .status()
        .expect("tar is on PATH (Windows 10+, every Linux userland, macOS)");
    assert!(st.success(), "tar -cf failed");
    std::fs::read(&archive).unwrap()
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
    assert!(
        found.is_none(),
        "no `tools/` above a temp directory, so nothing is found: {found:?}"
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
}
