//! Integration tests for JSON's two writers.
//!
//! `std/json` + `std/jsonread` (RFC-0059, split by RFC-0078 M2a): the two
//! modules' own inline unit suites — the writer's escaping and indentation on one
//! side, and on the other the strict-parse rejections with pinned `line N, col M`
//! wording, the full escape set including surrogate pairs, the round-trip law and
//! field-order preservation — run green through the real `vyrn` binary. Two rows
//! rather than one because `vyrn test` runs one module's blocks, and the point of
//! the split is that the writer links without the reader.
//!
//! `examples/jsonbytes.vyrn` (RFC-0078 M2): the bytes `toJson` produces, pinned as
//! literals BEFORE its serializer half is rewired from the C shim to `std/json`.
//! The example carries the pins in `test` blocks, and nothing else in the suite
//! runs an example's `test` blocks, so without this row the pins are decoration.

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

/// Run one std module's inline `test` blocks and assert the green count.
fn unit_tests_green(rel: &str, expected: &str) {
    let module = repo_file(rel);
    let out = vyrn().arg("test").arg(&module).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{rel} unit tests failed:\n{combined}");
    assert!(
        combined.contains(expected),
        "expected `{expected}`:\n{combined}"
    );
}

#[test]
fn std_json_writer_unit_tests_run_green() {
    unit_tests_green("std/json.vyrn", "3 passed, 0 failed");
}

#[test]
fn std_jsonread_unit_tests_run_green() {
    // The 14th is the awkward-input round trip: a key holding the document's own
    // punctuation, a quote, a backslash and a control byte; two keys differing
    // only by case; a character above the BMP; empty containers; and every
    // number form `JNum` may carry. `read(emit(x))` is the oracle — the writer's
    // output is checked by the toolchain's own strict reader, not by eye.
    unit_tests_green("std/jsonread.vyrn", "16 passed, 0 failed");
}

/// `JNum` is a public, unvalidated `String` constructor and `emit` copies its
/// contents out VERBATIM, so `emit(JNum("0x1f"))` used to print `0x1f` and exit
/// 0 — a document no JSON reader accepts, produced by an ordinary call. The
/// writer now refuses a raw number that is not one, where it escapes a value
/// (the rule `std/html` follows for a name), so both writers and every caller of
/// either are covered by the one check.
///
/// What this test validates: the refusal fires, says which text was refused, and
/// fails the program. What it cannot: that `numberOk` accepts exactly the JSON
/// grammar — the round trip in `std/jsonread`'s suite is what pins that end.
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

/// RFC-0078 M2b: the injected import is invisible. A program that mentions
/// `toJson` links `std/json` without saying so, and that module's declarations
/// must be unable to collide with the program's own or to capture the desugar's
/// calls.
///
/// Every name in here is one `std/json` also declares — `emit`, `hex2`,
/// `emitString`, the type `Json`, and an enum variant `JStr`, which is the one that
/// bites hardest: before the reserved spellings, a variant-name clash with a module
/// in the link was rejected outright ("function `JStr` is defined in
/// `std/json.vyrn` but not imported here"), so injection would have turned a legal
/// program into an error naming a module the user never mentioned.
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

/// `toJson` was O(N²) in array length — 40k `Int64` took 2.5 s, 80k took 23.5 s,
/// and 50k three-float records ran the machine out of memory producing 2.5 MB of
/// JSON. All of it was in `std/json`'s writer: `emitArr`/`emitObj` end with
/// `return out + "]"`, and codegen's in-place String append banned any name that
/// appeared under a `+`, so every element re-`malloc`'d and re-copied the whole
/// result so far and leaked the previous buffer.
///
/// The pin is structural — a COUNT of copying concatenations, not a duration, so
/// it cannot go flaky on a loaded machine. Each writer may copy exactly once, in
/// its tail; a second `strcat` means the per-element append went back to
/// allocating, which is the complexity class regressing. (`vyrn-codegen`'s
/// `accumulator_returned_through_a_concat_still_appends_in_place` pins the
/// compiler rule; this pins that `std/json` is actually written in the shape the
/// rule recognizes, which is the half a library edit could silently undo.)
#[test]
fn the_json_writer_does_not_copy_once_per_element() {
    let dir = std::env::temp_dir().join("vyrn-json-linear");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("linear.vyrn");
    std::fs::write(
        &file,
        "fn main() -> Int64 {\n\
         let mut a: Array<Int64> = []\n\
         a.push(1)\n\
         print(toJson(a).byteLength)\n\
         return 0\n\
         }\n",
    )
    .unwrap();
    let out = vyrn()
        .arg("emit-ir")
        .arg(&file)
        .output()
        .expect("vyrn emit-ir");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ir = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    for writer in ["vyrn_json$emitArr", "vyrn_json$emitObj"] {
        let start = ir
            .find(&format!("define ptr @{writer}("))
            .unwrap_or_else(|| panic!("no `{writer}` in the emitted IR"));
        let body = &ir[start..start + ir[start..].find("\n}\n").expect("unterminated body")];
        assert!(
            body.contains("call ptr @__vyrn_realloc"),
            "`{writer}` must grow its accumulator in place:\n{body}"
        );
        assert_eq!(
            body.matches("call ptr @__vyrn_str_concat(").count(),
            1,
            "`{writer}` may copy only in its tail — a second copy is one per \
             element, which is the O(N²) `toJson` had:\n{body}"
        );
    }
}

/// The same, for the other direction (RFC-0078 M3): `examples/jsondecbytes.vyrn`
/// pins what `fromJson` produces — weighted towards the accumulated `Issue`s,
/// their order, and the parse-error wording, which is where two READERS differ and
/// which nothing else in the suite looks at.
///
/// One of its blocks pins three rows where `std/jsonread` reads the same input
/// differently, two of them changing which documents parse at all. That block is
/// the evidence M3 is a semantic ruling rather than a byte-formatting one, so it
/// has to fail loudly if either reader moves under it.
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

/// RFC-0096 M2 — an `impl` DECLARED in an injected module is reached in both link
/// modes, because a link has ONE type key for `Json` rather than two.
///
/// `std/json` is linked two ways. A program that says `toJson` gets it INJECTED,
/// and the linker renames its every declaration to a reserved spelling, so the
/// type key is `json$Json`. A program that only imports it by hand gets the
/// unrenamed `Json`. A declared row — `impl Owned for Json`, `impl Copy for Json`
/// — is keyed by ONE type key, so a rename that reached the type and not the
/// impl would bind the row to a spelling nothing looks up.
///
/// The rename does reach it, by the patch RFC-0092 M3 landed in `loader.rs` for
/// `Copy`: a flattened impl method follows its TYPE's rename rather than taking
/// the module prefix in front of the mangling, and `rewrite_module_refs` rewrites
/// the impl HEAD along with every other reference. That patch is general over
/// protocols, so `Owned` needed nothing added — and nothing pinned it either.
/// This is the pin. Reverting the patch makes the injected halves fail.
///
/// The third program is the one that makes a single key necessary rather than
/// merely tidy: it does BOTH — a hand import beside a `toJson` — and the reserved
/// spellings apply to the module either way.
#[test]
fn a_declared_impl_in_an_injected_module_is_reached_in_both_link_modes() {
    let dir = std::env::temp_dir().join("vyrn-0096-keys");
    std::fs::create_dir_all(&dir).unwrap();
    // (file, source, the release the binding must be reclaimed by).
    // ONE key for both link modes: a runtime module's reserved spellings
    // (`json$Json`) apply however it was linked — hand import or builtin
    // mention. Bare constructor spellings are program-global, so an unprefixed
    // hand-imported `Json` sat one consumer enum away from a `JStr` collision.
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
        // It runs: an unresolved flattened `release` is a check failure, and a
        // release that frees the wrong thing is a trap.
        let run = vyrn().arg("run").arg(&file).output().expect("vyrn run");
        assert!(
            run.status.success(),
            "{name} did not run:\n{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        // And the row the binding got is the DECLARED one, under the key this
        // link mode gives the type.
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
