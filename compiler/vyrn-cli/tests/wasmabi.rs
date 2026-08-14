//! What crosses the JS boundary, and who says so (RFC-0012 M3).
//!
//! The wasm ABI is lossy in the one direction a host needs. `String`, `Bool`,
//! `Int32` and `UInt32` all cross as `i32`; a `String` argument to an imported
//! extern occupies two slots, `(i32, i64)`, which is the same shape an
//! `(Int32, Int64)` pair has. `web/wasi-min.js` used to recover the difference
//! by walking the module's own type/import/function/export sections and guessing
//! — an `i32` followed by an `i64` was taken to BE a String, with the collision
//! written down as a caveat in `web/README.md` — and it decided each export
//! ARGUMENT by the JS runtime type of whatever the caller happened to pass.
//!
//! Both guesses are gone: the compiler writes every declared signature into the
//! module's `vyrn:exports` custom section and the shim reads it. This file is
//! the gate on that, in three parts:
//!
//!   1. the section round-trips — a Rust reader of the emitted bytes agrees with
//!      the source declarations, for both directions;
//!   2. the collision the caveat named produces the right values through the
//!      real shim under node;
//!   3. a wrong-typed export argument is refused instead of being handed to the
//!      module as a pointer.
//!
//! Skips without node, loudly under `VYRN_REQUIRE_TOOLS` — the same posture as
//! `memory.rs` and `wasmio.rs`.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

fn find_node() -> Option<PathBuf> {
    let node = std::env::var("VYRN_NODE").unwrap_or_else(|_| "node".into());
    let found = Command::new(&node)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| PathBuf::from(&node));
    if found.is_none() && std::env::var_os("VYRN_REQUIRE_TOOLS").is_some() {
        panic!(
            "VYRN_REQUIRE_TOOLS is set and `node` was not found — this run would have \
             silently skipped the extern-boundary check, the only thing here that runs \
             the real shim. Point `VYRN_NODE` at the binary, or unset VYRN_REQUIRE_TOOLS."
        );
    }
    found
}

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vyrn-abi-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn build_wasm(dir: &Path, stem: &str, source: &str) -> PathBuf {
    std::fs::write(dir.join(format!("{stem}.vyrn")), source).unwrap();
    let out = dir.join(format!("{stem}.wasm"));
    let build = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg("build")
        .arg(dir.join(format!("{stem}.vyrn")))
        .args(["--target", "wasm", "-o"])
        .arg(&out)
        .output()
        .expect("vyrn build");
    assert!(
        build.status.success(),
        "build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    out
}

// ---------------------------------------------------------------------------
// 1. the section round-trips
// ---------------------------------------------------------------------------

/// The `vyrn:exports` payload, decoded: `(name, ret, params)` for the exports,
/// then the same for the `vyrn.*` imports. Deliberately a SECOND reader written
/// against the documented format rather than a call into the emitter, so that a
/// change to either side has to be made in both.
fn read_abi(
    wasm: &[u8],
) -> (
    Vec<(String, String, Vec<String>)>,
    Vec<(String, String, Vec<String>)>,
) {
    let mut i = 8; // magic + version
    let mut payload: Option<&[u8]> = None;
    while i < wasm.len() {
        let id = wasm[i];
        i += 1;
        let (len, used) = uleb(wasm, i);
        i += used;
        let end = i + len as usize;
        if id == 0 {
            let mut j = i;
            let (n, used) = uleb(wasm, j);
            j += used;
            let name = std::str::from_utf8(&wasm[j..j + n as usize]).unwrap();
            j += n as usize;
            if name == "vyrn:exports" {
                payload = Some(&wasm[j..end]);
            }
        }
        i = end;
    }
    let b = payload.expect("the module carries a vyrn:exports section");
    let mut i = 0;
    assert_eq!(b[i], 2, "section version");
    i += 1;
    let mut sides = Vec::new();
    for _ in 0..2 {
        let (count, used) = uleb(b, i);
        i += used;
        let mut rows = Vec::new();
        for _ in 0..count {
            let (name, used) = string(b, i);
            i += used;
            let (ret, used) = string(b, i);
            i += used;
            let (pc, used) = uleb(b, i);
            i += used;
            let mut params = Vec::new();
            for _ in 0..pc {
                let (p, used) = string(b, i);
                i += used;
                params.push(p);
            }
            rows.push((name, ret, params));
        }
        sides.push(rows);
    }
    assert_eq!(i, b.len(), "the whole payload was consumed");
    let imports = sides.pop().unwrap();
    let exports = sides.pop().unwrap();
    (exports, imports)
}

fn uleb(b: &[u8], mut i: usize) -> (u32, usize) {
    let (mut r, mut s, start) = (0u32, 0u32, i);
    loop {
        let x = b[i];
        i += 1;
        r |= ((x & 0x7f) as u32) << s;
        if x & 0x80 == 0 {
            return (r, i - start);
        }
        s += 7;
    }
}

fn string(b: &[u8], i: usize) -> (String, usize) {
    let (n, used) = uleb(b, i);
    let at = i + used;
    (
        String::from_utf8(b[at..at + n as usize].to_vec()).unwrap(),
        used + n as usize,
    )
}

/// Every declared extern, in both directions, with the type the SOURCE wrote —
/// not the wasm slot it happens to occupy. The four types that share the `i32`
/// slot are all here, and so is the `(String)` / `(Int32, Int64)` collision.
const BOUNDARY: &str = r#"
extern fn hostLog(msg: String) -> Unit
extern fn hostPair(a: Int32, b: Int64) -> Int64
extern fn hostNow() -> Float64

export extern fn addI64(a: Int64, b: Int64) -> Int64 { return a + b }
export extern fn greet(name: String) -> String { return "Hello, \{name}!" }
export extern fn isBig(n: Int32) -> Bool { return n > 100 }
export extern fn wide(n: UInt32) -> UInt32 { return n }
export extern fn small(a: Int8, b: UInt16) -> Int32 { return 0 }
export extern fn ratio(x: Float32) -> Float64 { return 1.0 }
export extern fn nothing(flag: Bool) -> Unit { }

fn main() -> Int64 { return 0 }
"#;

#[test]
fn the_section_states_every_declared_signature_on_both_sides() {
    let dir = tmp("section");
    let wasm = std::fs::read(build_wasm(&dir, "boundary", BOUNDARY)).unwrap();
    let (exports, imports) = read_abi(&wasm);

    let row = |rows: &[(String, String, Vec<String>)], name: &str| {
        rows.iter()
            .find(|(n, _, _)| n == name)
            .unwrap_or_else(|| panic!("{name} is declared: {rows:?}"))
            .clone()
    };
    let sig = |r: (String, String, Vec<String>)| (r.1, r.2);

    // The four types that share the wasm `i32` slot are four different answers.
    assert_eq!(
        sig(row(&exports, "greet")),
        ("string".into(), vec!["string".to_string()])
    );
    assert_eq!(
        sig(row(&exports, "isBig")),
        ("bool".into(), vec!["i32".to_string()])
    );
    assert_eq!(
        sig(row(&exports, "wide")),
        ("u32".into(), vec!["u32".to_string()])
    );
    assert_eq!(
        sig(row(&exports, "small")),
        ("i32".into(), vec!["i32".to_string(), "u32".to_string()])
    );
    assert_eq!(
        sig(row(&exports, "addI64")),
        ("i64".into(), vec!["i64".to_string(), "i64".to_string()])
    );
    assert_eq!(
        sig(row(&exports, "ratio")),
        ("f64".into(), vec!["f32".to_string()])
    );
    assert_eq!(
        sig(row(&exports, "nothing")),
        ("unit".into(), vec!["bool".to_string()])
    );

    // The import side, and the collision by name: one `String` argument and an
    // `(Int32, Int64)` pair occupy the same three wasm slots in the same order.
    assert_eq!(
        sig(row(&imports, "hostLog")),
        ("unit".into(), vec!["string".to_string()])
    );
    assert_eq!(
        sig(row(&imports, "hostPair")),
        ("i64".into(), vec!["i32".to_string(), "i64".to_string()])
    );
    assert_eq!(sig(row(&imports, "hostNow")), ("f64".into(), vec![]));

    // Nothing on the boundary, nothing written down.
    let bare = tmp("bare");
    let wasm = std::fs::read(build_wasm(
        &bare,
        "bare",
        "fn main() -> Int64 { return 0 }\n",
    ))
    .unwrap();
    assert!(
        !String::from_utf8_lossy(&wasm).contains("vyrn:exports"),
        "a module with no externs carries no section"
    );
}

// ---------------------------------------------------------------------------
// 2 + 3. the real shim, under node
// ---------------------------------------------------------------------------

/// An import taking one `String` and one taking `(Int32, Int64)` — the pair the
/// old shape-guess could not tell apart. Both are CALLED, so the driver sees
/// what each hook actually received.
const COLLIDE: &str = r#"
extern fn hostLog(msg: String) -> Unit
extern fn hostPair(a: Int32, b: Int64) -> Int64

export extern fn greet(name: String) -> String { return "Hello, \{name}!" }
export extern fn wide(n: UInt32) -> UInt32 { return n }

fn main() -> Int64 {
    hostLog("from vyrn")
    let n = hostPair(7, 900)
    print("pair=\{n}")
    return 0
}
"#;

const DRIVER: &str = r#"import { readFile } from "node:fs/promises";
import { runVyrn } from "./wasi-min.mjs";

const bytes = await readFile(new URL("./collide.wasm", import.meta.url));
const seen = [];
const r = await runVyrn(bytes, {
  onStdout: () => {},
  onStderr: () => {},
  extern: {
    hostLog: (msg) => { seen.push(["hostLog", typeof msg, msg]); },
    hostPair: (a, b) => { seen.push(["hostPair", typeof a, a, typeof b, String(b)]); return 42n; },
  },
});
console.log(JSON.stringify(seen));

// A String parameter takes a JS string, and gives one back.
console.log(JSON.stringify(r.exports.greet("world")));

// An unsigned result is unsigned. 3000000000 is over 2^31, so a wasm `i32`
// reaches JS sign-extended (-1294967296) unless something says it is a UInt32.
console.log(JSON.stringify(r.exports.wide(3000000000)));

// A String parameter handed a number is refused, not passed as a pointer.
let refused = "no";
try { r.exports.greet(42); } catch (e) { refused = e.message; }
console.log(JSON.stringify(refused));
"#;

#[test]
fn the_shim_reads_the_declaration_instead_of_the_instruction_shape() {
    let Some(node) = find_node() else {
        eprintln!("NOTE: no node — the extern boundary is unverified on this machine");
        return;
    };
    let dir = tmp("shim");
    build_wasm(&dir, "collide", COLLIDE);
    std::fs::write(dir.join("drive.mjs"), DRIVER).unwrap();
    std::fs::copy(repo("web/wasi-min.js"), dir.join("wasi-min.mjs")).unwrap();

    let out = Command::new(&node)
        .arg(dir.join("drive.mjs"))
        .output()
        .expect("node");
    assert!(
        out.status.success(),
        "node failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();

    // `hostLog(msg: String)` is one decoded string; `hostPair(Int32, Int64)` is
    // two numbers. Identical wasm slots, opposite answers. The shape-guess read
    // hostPair's two arguments as ONE string decoded from linear memory at
    // address 7 with length 900 — 900 bytes of whatever the heap held.
    let seen = lines.next().unwrap_or_default();
    assert_eq!(
        seen, r#"[["hostLog","string","from vyrn"],["hostPair","number",7,"bigint","900"]]"#,
        "each import saw its declared arguments: {text}"
    );

    assert_eq!(
        lines.next().unwrap_or_default(),
        r#""Hello, world!""#,
        "a String parameter and a String return: {text}"
    );
    assert_eq!(
        lines.next().unwrap_or_default(),
        "3000000000",
        "a UInt32 result is unsigned: {text}"
    );
    let refused = lines.next().unwrap_or_default();
    assert!(
        refused.contains("declared `String`"),
        "a number for a String parameter is refused: {text}"
    );
}
