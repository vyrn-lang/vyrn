//! Tests JSON's two writers. The inline suites of `std/json` and `std/jsonread`
//! run in `std_suite.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_file(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
        .canonicalize()
        .unwrap()
}

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

/// `JNum` is a public, unvalidated `String` constructor and `emit` copies it
/// verbatim, so the writer checks it where it escapes a value. That `numberOk`
/// accepts exactly the JSON grammar is pinned by `std/jsonread`'s round trip.
#[test]
fn emit_refuses_a_jnum_that_is_not_a_json_number() {
    let dir = std::env::temp_dir().join("vyrn-json-badnum");
    std::fs::create_dir_all(&dir).unwrap();
    for (name, raw) in [
        ("hex", "0x1f"),
        ("leading-zero", "007"),
        ("plus", "+1"),
        ("bare-word", "NaN"),
        ("trailing-dot", "1."),
        ("empty", ""),
        ("punctuation", "1,2"),
    ] {
        let file = dir.join(format!("{name}.vyrn"));
        std::fs::write(
            &file,
            format!(
                "import {{ Json, emit }} from \"std/json\"\n\
                 fn main() -> Int64 {{\n    print(emit(JNum(\"{raw}\")))\n    return 0\n}}\n"
            ),
        )
        .unwrap();
        let out = vyrn().arg("run").arg(&file).output().expect("vyrn run");
        let text = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "`JNum(\"{raw}\")` emitted a document:\n{text}"
        );
        assert!(
            text.contains(&format!("json: `{raw}` is not a usable number")),
            "`JNum(\"{raw}\")`: unexpected failure:\n{text}"
        );
    }
}

/// A program that mentions `toJson` links `std/json` without saying so.
/// Every name here is one `std/json` also declares; the variant `JStr` is the
/// sharpest, since a variant clash with a linked module is otherwise refused.
#[test]
fn an_injected_runtime_module_cannot_collide_with_the_users_names() {
    let dir = std::env::temp_dir().join("vyrn-m2b-inject");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("collide.vyrn");
    std::fs::write(
        &file,
        "type Json = | Mine | JStr(String)\n\
         type P = { n: Int64 }\n\
         fn emit(x: Int64) -> String { return \"user emit \" + x.toString() }\n\
         fn hex2(b: Int64) -> String { return \"user hex2\" }\n\
         fn emitString(s: String) -> String { return \"user emitString\" }\n\
         fn main() -> Int64 {\n\
         print(emit(7))\n\
         print(hex2(3))\n\
         print(emitString(\"q\"))\n\
         print(match JStr(\"v\") { Mine => \"mine\", JStr(s) => s })\n\
         print(toJson(P { n: 5 }))\n\
         return 0\n\
         }\n",
    )
    .unwrap();
    let out = vyrn().arg("run").arg(&file).output().expect("vyrn run");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "collision program failed:\n{combined}"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        "user emit 7\nuser hex2\nuser emitString\nv\n{\"n\":5}\n",
        "the user's own names must win, and `toJson` must still work:\n{combined}"
    );
}

/// Nothing else runs an example's `test` blocks, so without this row the pins in
/// `examples/jsonbytes.vyrn` are decoration.
#[test]
fn tojson_byte_pins_hold() {
    let example = repo_file("examples/jsonbytes.vyrn");
    let out = vyrn()
        .arg("test")
        .arg(&example)
        .output()
        .expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "toJson byte pins failed:\n{combined}");
    assert!(
        combined.contains("7 passed, 0 failed"),
        "expected 7 green pins:\n{combined}"
    );
}

/// `emitArr`/`emitObj` end with `return out + "]"`; if the in-place append
/// refuses that name, every element copies the whole result so far.
///
/// The pin is a ratio between two sizes, not a duration, so a loaded machine
/// slows both sides. Four times the elements costs four times the work when
/// appending in place and sixteen when copying; the threshold sits between.
#[test]
fn the_json_writer_does_not_copy_once_per_element() {
    const N: usize = 20_000;
    let dir = std::env::temp_dir().join("vyrn-json-linear");
    std::fs::create_dir_all(&dir).unwrap();
    let best_of_3 = |name: &str, n: usize| -> std::time::Duration {
        let src = format!(
            "fn main() -> Int64 {{\n\
             let mut a: Array<Int64> = []\n\
             let mut i = 0\n\
             while i < {n} {{ a.push(i)  i = i + 1 }}\n\
             print(toJson(a).byteLength)\n\
             return 0\n\
             }}\n"
        );
        let file = dir.join(format!("{name}.vyrn"));
        std::fs::write(&file, src).unwrap();
        (0..3)
            .map(|_| {
                let t = std::time::Instant::now();
                let out = vyrn().arg("run").arg(&file).output().expect("vyrn run");
                assert!(
                    out.status.success(),
                    "{}",
                    String::from_utf8_lossy(&out.stderr)
                );
                t.elapsed()
            })
            .min()
            .unwrap()
    };
    let small = best_of_3("linear-small", N);
    let big = best_of_3("linear-big", 4 * N);
    assert!(
        big.as_secs_f64() < 8.0 * small.as_secs_f64(),
        "`toJson` is copying its accumulator per element: {big:?} for {} \
         elements against {small:?} for {N}",
        4 * N
    );
}

/// `examples/jsondecbytes.vyrn` pins `fromJson`'s `Issue`s, their order and the
/// parse-error wording, where two readers differ. One block pins three rows where
/// `std/jsonread` reads the same input differently.
#[test]
fn fromjson_byte_pins_hold() {
    let example = repo_file("examples/jsondecbytes.vyrn");
    let out = vyrn()
        .arg("test")
        .arg(&example)
        .output()
        .expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "fromJson byte pins failed:\n{combined}"
    );
    assert!(
        combined.contains("10 passed, 0 failed"),
        "expected 10 green pins:\n{combined}"
    );
}

/// A synthesized decoder is judged like source, and one for a scalar alias
/// answers in the alias, which is its base (#527).
#[test]
fn a_decoder_for_a_scalar_alias_is_typed_as_written() {
    let dir = std::env::temp_dir().join("vyrn-json-alias");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("alias.vyrn");
    std::fs::write(
        &file,
        r#"type I = Int64
type U = UInt64
type F = Float64
type B = Bool
type S = String
type R = { is: Array<I>, u: U, f: F, b: B, s: S }
fn main() -> Int64 {
    match fromJson<R>("{\"is\":[1,2],\"u\":3,\"f\":0.5,\"b\":true,\"s\":\"x\"}") {
        Valid(r) => print(toJson(r)),
        Invalid(_) => print("invalid"),
    }
    return 0
}
"#,
    )
    .unwrap();
    let out = vyrn().arg("run").arg(&file).output().expect("vyrn run");
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{text}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        r#"{"is":[1,2],"u":3,"f":0.500000,"b":true,"s":"x"}"#,
        "{text}"
    );
}

/// A runtime module's declarations take a reserved spelling (`json$Json`)
/// however it is linked, and a declared impl row is keyed by that one type key.
/// In `loader.rs` a flattened impl method follows its type's rename, and
/// `rewrite_module_refs` rewrites the impl head; if either stops, the row binds
/// to a spelling nothing looks up.
#[test]
fn a_declared_impl_in_an_injected_module_is_reached_in_both_link_modes() {
    let dir = std::env::temp_dir().join("vyrn-0096-keys");
    std::fs::create_dir_all(&dir).unwrap();
    // (file, source, the release the binding must be reclaimed by).
    let cases = [
        (
            "handonly.vyrn",
            "import { Json, emit } from \"std/json\"\n\
             fn main() -> Int64 {\n\
             let v: Json = JStr(\"a\" + \"b\")\n\
             print(emit(v))\n\
             return 0\n\
             }\n",
            "Owned__json$Json__release",
        ),
        (
            "both.vyrn",
            "import { Json, emit } from \"std/json\"\n\
             type P = { n: Int64 }\n\
             fn main() -> Int64 {\n\
             let v: Json = JStr(\"a\" + \"b\")\n\
             print(emit(v))\n\
             print(toJson(P { n: 5 }))\n\
             return 0\n\
             }\n",
            "Owned__json$Json__release",
        ),
    ];
    for (name, src, release) in cases {
        let file = dir.join(name);
        std::fs::write(&file, src).unwrap();
        // An unresolved flattened `release` fails the check; a release that
        // frees the wrong thing traps.
        let run = vyrn().arg("run").arg(&file).output().expect("vyrn run");
        assert!(
            run.status.success(),
            "{name} did not run:\n{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        let why = vyrn()
            .arg("why")
            .arg("--memory")
            .arg(&file)
            .output()
            .expect("vyrn why");
        let report = String::from_utf8_lossy(&why.stdout).to_string();
        assert!(
            report.contains(&format!("calling `{release}`")),
            "{name}: expected the declared release `{release}`:\n{report}"
        );
    }
}
