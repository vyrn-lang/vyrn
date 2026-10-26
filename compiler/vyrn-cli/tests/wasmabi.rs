//! The JS boundary's types. The wasm ABI is lossy: `String`, `Bool`,
//! `Int32` and `UInt32` all cross as `i32`, and a `String` argument to an
//! import occupies `(i32, i64)`, the same slots as an `(Int32, Int64)` pair. So
//! the compiler writes every declared signature into the `vyrn:exports` custom
//! section and `web/wasi-min.js` reads it. The shim tests skip without node;
//! `VYRN_REQUIRE_TOOLS` makes a missing node fail.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Decodes the `vyrn:exports` payload into `(name, ret, params)` rows for the
/// exports and for the `vyrn.*` imports, and the exports that wait. A second reader written against the
/// documented format, not a call into the emitter, so a format change must be
/// made on both sides.
fn read_abi(
    wasm: &[u8],
) -> (
    Vec<(String, String, Vec<String>)>,
    Vec<(String, String, Vec<String>)>,
    Vec<String>,
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
    assert_eq!(b[i], 4, "section version");
    i += 1;
    let mut sides = Vec::new();
    let mut waiting = Vec::new();
    for side in 0..2 {
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
            if side == 0 && b[i] == 1 {
                waiting.push(name.clone());
            }
            i += usize::from(side == 0);
            rows.push((name, ret, params));
        }
        sides.push(rows);
    }
    assert_eq!(i, b.len(), "the whole payload was consumed");
    let imports = sides.pop().unwrap();
    let exports = sides.pop().unwrap();
    (exports, imports, waiting)
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

/// Holds the four types that share the `i32` slot and the `(String)` /
/// `(Int32, Int64)` collision.
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
    let (exports, imports, waiting) = read_abi(&wasm);
    assert!(waiting.is_empty(), "no export waits: {waiting:?}");

    let row = |rows: &[(String, String, Vec<String>)], name: &str| {
        rows.iter()
            .find(|(n, _, _)| n == name)
            .unwrap_or_else(|| panic!("{name} is declared: {rows:?}"))
            .clone()
    };
    let sig = |r: (String, String, Vec<String>)| (r.1, r.2);

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

    // One `String` argument and an `(Int32, Int64)` pair occupy the same slots.
    assert_eq!(
        sig(row(&imports, "hostLog")),
        ("unit".into(), vec!["string".to_string()])
    );
    assert_eq!(
        sig(row(&imports, "hostPair")),
        ("i64".into(), vec!["i32".to_string(), "i64".to_string()])
    );
    assert_eq!(sig(row(&imports, "hostNow")), ("f64".into(), vec![]));

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

/// Calls an import taking one `String` and one taking `(Int32, Int64)`, so the
/// driver sees what each hook received.
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
    let Some(node) = common::node() else {
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

    // Identical wasm slots: a shim guessing from the shape would decode
    // hostPair's (7, 900) as a string at address 7 of length 900.
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

const RUN: &str = r#"import { readFile } from "node:fs/promises";
import { runVyrn } from "./wasi-min.mjs";

const r = await runVyrn(await readFile(process.argv[2]), {});
process.stdout.write(r.stdout);
process.stderr.write(r.stderr);
process.exitCode = r.exitCode;
"#;

/// A stack overflow and a program that waits end in the shim as under `vyrn run`.
/// The shim hands V8's `RangeError`, or the shadow stack's out-of-bounds trap,
/// back to the module, which flushes stdout and traps with one sentence; it
/// delivers each timer to `__vyrn_wake` before `runVyrn` resolves.
#[test]
fn the_shim_ends_each_program_as_vyrn_run_does() {
    let Some(node) = common::node() else {
        eprintln!("NOTE: no node, so the shim's traps and waits are unverified on this machine");
        return;
    };
    let dir = tmp("ends");
    std::fs::write(dir.join("run.mjs"), RUN).unwrap();
    std::fs::copy(repo("web/wasi-min.js"), dir.join("wasi-min.mjs")).unwrap();
    let waiting = common::waiting();
    let traps = common::ENGINE_TRAP.iter().map(|(n, _)| *n);
    for name in traps.chain(waiting.iter().map(String::as_str)) {
        let example = repo(&format!("examples/{name}"));
        let stem = name.trim_end_matches(".vyrn");
        let wasm = build_wasm(&dir, stem, &std::fs::read_to_string(&example).unwrap());
        let shim = Command::new(&node)
            .arg(dir.join("run.mjs"))
            .arg(&wasm)
            .output()
            .expect("node");
        let engine = Command::new(env!("CARGO_BIN_EXE_vyrn"))
            .arg("run")
            .arg(&example)
            .output()
            .expect("vyrn run");
        let say = |o: &std::process::Output| {
            format!(
                "exit {:?}\n{}{}",
                o.status.code(),
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        };
        assert_eq!(say(&shim), say(&engine), "{name}");
    }
}

/// Exports that wait, flagged in the section; a String argument lives as long as
/// its task, and each task's result reaches the shim when the task ends.
const WAITING_EXPORT: &str = r#"import { sleep } from "std/task"

let mut total: Int64 = 0

export extern fn later(name: String) -> Int64 {
    await sleep(5)
    total = total + Int64(name.byteLength)
    return total * 10
}

export extern fn label(n: Int64) -> String {
    await sleep(1)
    return "n=\{n}"
}

export extern fn ratio(n: Int64) -> Float32 {
    await sleep(1)
    return Float32(n) / Float32(4)
}

export extern fn quick(n: UInt32) -> UInt32 {
    if n < 1 {
        await sleep(1)
    }
    return n
}

export extern fn sum() -> Int64 { return total }

fn main() -> Int64 { return 0 }
"#;

const LATER: &str = r#"import { readFile } from "node:fs/promises";
import { runVyrn } from "./wasi-min.mjs";

const r = await runVyrn(await readFile(process.argv[2]), {});
const task = r.exports.later("abc");
console.log(task instanceof Promise, String(r.exports.sum()));
console.log(String(await task), String(r.exports.sum()), await r.exports.label(7n));
console.log(await r.exports.ratio(3n), await r.exports.quick(3000000000));
"#;

/// The section flags each export that waits, and the shim's wrapper for one
/// returns a promise of the result its task returned: an Int64, a String the
/// shim frees, a Float32 by its bits. A waiting export whose body reaches no
/// wait ends in its first step, and its promise holds the result.
#[test]
fn the_shim_settles_the_task_an_export_starts() {
    let dir = tmp("later");
    let wasm = build_wasm(&dir, "later", WAITING_EXPORT);
    let (_, _, waiting) = read_abi(&std::fs::read(&wasm).unwrap());
    assert_eq!(waiting, ["later", "label", "ratio", "quick"]);
    let Some(node) = common::node() else {
        eprintln!("NOTE: no node, so the shim's waiting exports are unverified on this machine");
        return;
    };
    std::fs::write(dir.join("later.mjs"), LATER).unwrap();
    std::fs::copy(repo("web/wasi-min.js"), dir.join("wasi-min.mjs")).unwrap();
    let out = Command::new(&node)
        .arg(dir.join("later.mjs"))
        .arg(&wasm)
        .output()
        .expect("node");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "true 0\n30 3 n=7\n0.75 3000000000\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Two waiting exports, each a task: the one that sleeps less ends first, though it
/// started second.
const TWO_TASKS: &str = r#"import { sleep } from "std/task"

let mut seen: Int64 = 0

export extern fn later(tag: Int64, ms: Int64) {
    await sleep(ms)
    seen = seen * 10 + tag
}

export extern fn order() -> Int64 { return seen }

fn main() -> Int64 { return 0 }
"#;

const BOTH_LATER: &str = r#"import { readFile } from "node:fs/promises";
import { runVyrn } from "./wasi-min.mjs";

const r = await runVyrn(await readFile(process.argv[2]), {});
await Promise.all([r.exports.later(1n, 60n), r.exports.later(2n, 5n)]);
console.log(String(r.exports.order()));
"#;

/// Each timer has its own `setTimeout`: a timer set while another task waits for a later
/// one fires at its own deadline, so two tasks end in timer order.
#[test]
fn the_shim_ends_two_tasks_in_timer_order() {
    let Some(node) = common::node() else {
        eprintln!("NOTE: no node, so the shim's timers are unverified on this machine");
        return;
    };
    let dir = tmp("two-tasks");
    let wasm = build_wasm(&dir, "two", TWO_TASKS);
    std::fs::write(dir.join("two.mjs"), BOTH_LATER).unwrap();
    std::fs::copy(repo("web/wasi-min.js"), dir.join("wasi-min.mjs")).unwrap();
    let out = Command::new(&node)
        .arg(dir.join("two.mjs"))
        .arg(&wasm)
        .output()
        .expect("node");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "21\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
