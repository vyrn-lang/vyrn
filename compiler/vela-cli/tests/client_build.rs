//! `velac build --client` (RFC-0019): the client-role wasm artifact must import
//! the browser RPC host `vela`.`__rpc` and export the `onRpc` handler, while the
//! server-only procedure bodies are absent. Needs clang + the wasi-libc sysroot,
//! so it is `#[ignore]`d like the parity harness:
//!
//!     cargo test -p vela-cli --test client_build -- --ignored

use std::path::PathBuf;
use std::process::Command;

const CLIENT_SRC: &str = r#"
type GetUserReq = { id: Int64 }
type User = { name: String, active: Bool }

rpc fn getUser(req: GetUserReq) -> User {
    return User { name: "server", active: true }
}

export extern fn refresh(id: Int64) {
    let reqId = rpc(getUser, GetUserReq { id: id })
    print("requested \{reqId}")
}

export extern fn onRpc(id: Int64, status: Int64, body: String) {
    match fromJson(User, body) {
        Valid(u) => print("ok"),
        Invalid(errs) => print("bad"),
    }
}
"#;

/// The wasi-libc sysroot, from `$WASI_SYSROOT` or the repo's `tools/`.
fn wasi_sysroot() -> Option<PathBuf> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::env::var("WASI_SYSROOT")
        .map(PathBuf::from)
        .ok()
        .filter(|p| p.exists())
        .or_else(|| Some(root.join("tools/wasi-sysroot-25.0")).filter(|p| p.exists()))
}

fn wasi_builtins() -> Option<PathBuf> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::env::var("WASI_BUILTINS")
        .map(PathBuf::from)
        .ok()
        .filter(|p| p.exists())
        .or_else(|| {
            Some(root.join(
                "tools/libclang_rt.builtins-wasm32-wasi-25.0/libclang_rt.builtins-wasm32.a",
            ))
            .filter(|p| p.exists())
        })
}

/// A minimal LEB128 reader over a wasm module: returns the (module, name) pairs
/// of the import section and the names of the function exports.
fn wasm_imports_exports(data: &[u8]) -> (Vec<(String, String)>, Vec<String>) {
    assert_eq!(&data[..4], b"\0asm", "not a wasm module");
    let mut p = 8usize;
    let u = |p: &mut usize| -> u64 {
        let mut r = 0u64;
        let mut s = 0;
        loop {
            let b = data[*p];
            *p += 1;
            r |= ((b & 0x7f) as u64) << s;
            if b & 0x80 == 0 {
                break;
            }
            s += 7;
        }
        r
    };
    let mut imports = Vec::new();
    let mut exports = Vec::new();
    while p < data.len() {
        let sid = data[p];
        p += 1;
        let size = u(&mut p) as usize;
        let end = p + size;
        if sid == 2 {
            let n = u(&mut p);
            for _ in 0..n {
                let l = u(&mut p) as usize;
                let m = String::from_utf8_lossy(&data[p..p + l]).into_owned();
                p += l;
                let l = u(&mut p) as usize;
                let nm = String::from_utf8_lossy(&data[p..p + l]).into_owned();
                p += l;
                let kind = data[p];
                p += 1;
                match kind {
                    0 => {
                        u(&mut p);
                    }
                    1 => {
                        p += 1;
                        let fl = u(&mut p);
                        u(&mut p);
                        if fl & 1 == 1 {
                            u(&mut p);
                        }
                    }
                    2 => {
                        let fl = u(&mut p);
                        u(&mut p);
                        if fl & 1 == 1 {
                            u(&mut p);
                        }
                    }
                    3 => {
                        p += 2;
                    }
                    _ => {}
                }
                imports.push((m, nm));
            }
            p = end;
        } else if sid == 7 {
            let n = u(&mut p);
            for _ in 0..n {
                let l = u(&mut p) as usize;
                let nm = String::from_utf8_lossy(&data[p..p + l]).into_owned();
                p += l;
                let kind = data[p];
                p += 1;
                u(&mut p);
                if kind == 0 {
                    exports.push(nm);
                }
            }
            p = end;
        } else {
            p = end;
        }
    }
    (imports, exports)
}

#[test]
#[ignore = "needs clang + wasi sysroot; run: cargo test -p vela-cli --test client_build -- --ignored"]
fn client_build_imports_rpc_host_and_exports_handlers() {
    let (Some(sysroot), Some(builtins)) = (wasi_sysroot(), wasi_builtins()) else {
        eprintln!("NOTE: wasm toolchain not found — skipping client build test");
        return;
    };
    let unique = format!("{}", std::process::id());
    let src = std::env::temp_dir().join(format!("vela-client-{unique}.vela"));
    let wasm = std::env::temp_dir().join(format!("vela-client-{unique}.wasm"));
    std::fs::write(&src, CLIENT_SRC).unwrap();

    let build = Command::new(env!("CARGO_BIN_EXE_velac"))
        .arg("build")
        .arg(&src)
        .arg("--client")
        .arg("-o")
        .arg(&wasm)
        .env("WASI_SYSROOT", &sysroot)
        .env("WASI_BUILTINS", &builtins)
        .output()
        .expect("run velac build --client");
    assert!(
        build.status.success(),
        "client build failed:\n{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let data = std::fs::read(&wasm).expect("read wasm");
    let (imports, exports) = wasm_imports_exports(&data);
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&wasm);

    assert!(
        imports.iter().any(|(m, n)| m == "vela" && n == "__rpc"),
        "client wasm must import vela.__rpc; imports: {imports:?}"
    );
    assert!(
        exports.iter().any(|e| e == "onRpc"),
        "client wasm must export onRpc; exports: {exports:?}"
    );
    assert!(
        exports.iter().any(|e| e == "refresh"),
        "client wasm must export refresh; exports: {exports:?}"
    );
    // The procedure is NOT an export (its body is a remote stub — only the wire
    // name survives, as a data-section string for the `__rpc` call).
    assert!(
        !exports.iter().any(|e| e == "getUser"),
        "the procedure must not be exported from the client artifact; exports: {exports:?}"
    );
}
