//! What a natively built program that sends requests links beside its host:
//! rustls-ffi's static library and header, and the Mozilla roots of
//! `webpki-root-certs` as one PEM bundle. `vyrn` embeds the three
//! (`nativec::Native::net`) and writes them into `~/.vyrn/cache/native`, so
//! `vyrn build` needs no cargo.
//!
//! The library comes from a cargo of its own, run on this workspace with its
//! Cargo.lock into a target directory under `OUT_DIR`, so its path is known.
//! `cargo rustc --crate-type staticlib` builds it alone: cargo applies LTO to a
//! staticlib only when the crate has no other crate type, and rustls-ffi also
//! declares `lib`. Fat LTO keeps only the code its C functions reach, std's
//! included. rustls-ffi catches panics at its C boundary, so it unwinds.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.join("../Cargo.toml");
    let lock = manifest.join("../Cargo.lock");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", lock.display());
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let target = out.join("rustls-ffi");
    let cargo = std::env::var_os("CARGO").expect("cargo sets CARGO");
    // A jobserver or rustflags of the outer build are not this build's: the
    // first could starve one build of the other's tokens, the second could
    // change what the library holds.
    let built = Command::new(cargo)
        .args([
            "rustc",
            "--release",
            "--locked",
            "-p",
            "rustls-ffi",
            "--lib",
        ])
        .args(["--crate-type", "staticlib", "--manifest-path"])
        .arg(&workspace)
        .env("CARGO_TARGET_DIR", &target)
        .env("CARGO_PROFILE_RELEASE_LTO", "fat")
        .env("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "1")
        .env("CARGO_PROFILE_RELEASE_OPT_LEVEL", "3")
        .env("CARGO_PROFILE_RELEASE_PANIC", "unwind")
        .env("CARGO_PROFILE_RELEASE_INCREMENTAL", "false")
        .env_remove("CARGO_MAKEFLAGS")
        .env_remove("MAKEFLAGS")
        .env_remove("MFLAGS")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status()
        .expect("run cargo for rustls-ffi");
    assert!(built.success(), "cargo could not build rustls-ffi");
    let name = if cfg!(windows) {
        "rustls_ffi.lib"
    } else {
        "librustls_ffi.a"
    };
    let lib = target.join("release").join(name);
    // rustls-ffi's build script names its include directory (`links`).
    let include = std::env::var_os("DEP_RUSTLS_FFI_INCLUDE").expect("rustls-ffi names its headers");
    let header = Path::new(&include).join("rustls.h");
    let pem = out.join("roots.pem");
    std::fs::write(&pem, bundle()).expect("write the PEM bundle");
    let mut key = 0xcbf2_9ce4_8422_2325u64;
    for (name, path) in [
        ("VYRN_TLS_LIB", &lib),
        ("VYRN_TLS_H", &header),
        ("VYRN_TLS_ROOTS", &pem),
    ] {
        println!("cargo:rustc-env={name}={}", path.display());
        for b in std::fs::read(path).expect("read a TLS input") {
            key = (key ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
        }
    }
    // Names the cache entries, so a changed library, header or bundle names other ones.
    println!("cargo:rustc-env=VYRN_TLS_KEY={key:016x}");
}

/// Every root of `webpki-root-certs`, PEM-encoded, one after another.
fn bundle() -> String {
    let mut out = String::new();
    for der in webpki_root_certs::TLS_SERVER_ROOT_CERTS {
        out.push_str("-----BEGIN CERTIFICATE-----\n");
        let b64 = base64(der);
        for line in b64.as_bytes().chunks(64) {
            out.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
            out.push('\n');
        }
        out.push_str("-----END CERTIFICATE-----\n");
    }
    out
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for k in 0..4 {
            if k <= c.len() {
                out.push(A[(n >> (18 - 6 * k)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
