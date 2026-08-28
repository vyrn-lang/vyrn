//! VON — Vyrn Object Notation (RFC-0097 M1).
//!
//! Four claims, each pinned through the real `vyrn` binary:
//!
//! 1. `std/von`'s own inline suite — the strictness rules, the verbatim number
//!    text, the canonical writer, and the JSON conversion — runs green.
//! 2. **A `.von` file is Vyrn tokens.** `vyrn fmt --check` passes on
//!    `examples/vondemo.von` with no formatter change of any kind (RFC-0097 M0).
//! 3. **The canonical text is a fixed point of the formatter.** What `toVon`
//!    writes is what `vyrn fmt` would leave behind, so a document written by a
//!    program and a document written by a person are the same bytes.
//! 4. **A malformed document fails the build, positioned in the `.von` file.**
//!    Nothing in that message names a token kind, a generated module, or any
//!    other word the reader cannot type.
//!
//! `examples/vondemo.vyrn` covers the reader end to end under all three
//! backends: the parity harness builds it, and the document's canonical text is
//! its output.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn repo_file(rel: &str) -> PathBuf {
    repo_root().join(rel)
}

fn vyrn() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
    // So `std/` is found by walking up, exactly as it is for a user in the repo.
    c.current_dir(repo_root());
    c
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("vyrn-von-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn combined(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr)
}

#[test]
fn std_von_unit_tests_run_green() {
    let out = vyrn()
        .arg("test")
        .arg(repo_file("std/von.vyrn"))
        .output()
        .expect("vyrn test");
    let text = combined(&out);
    assert!(out.status.success(), "std/von unit tests failed:\n{text}");
    assert!(text.contains("17 passed, 0 failed"), "{text}");
}

/// RFC-0097 M0, and the whole thesis in one command: the formatter that formats
/// `.vyrn` files formats a `.von` file, because there is only one grammar.
#[test]
fn a_von_document_is_already_canonically_formatted_vyrn() {
    let out = vyrn()
        .arg("fmt")
        .arg("--check")
        .arg(repo_file("examples/vondemo.von"))
        .output()
        .expect("vyrn fmt");
    assert_eq!(
        out.status.code(),
        Some(0),
        "examples/vondemo.von is not canonically formatted:\n{}",
        combined(&out)
    );
}

/// What `toVon` writes, `vyrn fmt` leaves alone. The example prints the
/// canonical text of its own `.von` file after two summary lines.
#[test]
fn the_canonical_text_is_a_fixed_point_of_fmt() {
    let out = vyrn()
        .arg("run")
        .arg(repo_file("examples/vondemo.vyrn"))
        .output()
        .expect("vyrn run");
    assert!(out.status.success(), "{}", combined(&out));
    let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    // `print` adds the newline the document already ends with; one file ends
    // with one.
    let printed: String = stdout.split_inclusive('\n').skip(2).collect();
    let canonical = printed.trim_end_matches('\n').to_string() + "\n";
    assert!(
        canonical.starts_with("import type { AppConfig } from \"./vondemo\"\n\nAppConfig {\n"),
        "unexpected canonical text:\n{canonical}"
    );
    let dir = scratch("fixed-point");
    let file = dir.join("canonical.von");
    std::fs::write(&file, canonical.as_bytes()).unwrap();
    let out = vyrn()
        .arg("fmt")
        .arg("--check")
        .arg(&file)
        .output()
        .expect("vyrn fmt");
    assert_eq!(
        out.status.code(),
        Some(0),
        "`toVon` output is not what `vyrn fmt` writes:\n{}",
        combined(&out)
    );
}

/// A configuration mistake is a BUILD error, and it names the line of the
/// `.von` file it is on — not a position in generated text, and not a token
/// kind.
#[test]
fn a_malformed_document_fails_the_build_in_the_von_file() {
    let dir = scratch("malformed");
    std::fs::copy(
        repo_file("examples/lib/gen_von.vyrn"),
        dir.join("gen_von.vyrn"),
    )
    .unwrap();
    // The YAML octal ambiguity, refused: `0777` is 511 under YAML 1.1 and 777
    // under 1.2, so VON reads it as neither.
    std::fs::write(
        dir.join("bad.von"),
        "import type { C } from \"./c\"\n\nC {\n    mode: 0777,\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("app.vyrn"),
        "import { vonModule } from \"./gen_von\"\n\
         import { configTypeName } from vonModule(\"./bad.von\")\n\n\
         fn main() -> Int64 {\n    print(configTypeName())\n    return 0\n}\n",
    )
    .unwrap();
    let out = vyrn()
        .arg("check")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("vyrn check");
    let text = combined(&out);
    assert!(!out.status.success(), "a bad document built:\n{text}");
    assert!(
        text.contains("./bad.von: line 4, col 11: `0777` has a leading zero"),
        "the error is not anchored in the .von file:\n{text}"
    );
    // Nothing the reader cannot type: no token kinds, no synthesized module key.
    for internal in ["punct", "ident token", "generated by"] {
        assert!(!text.contains(internal), "`{internal}` leaked:\n{text}");
    }
}

/// `vyrn fmt --from-json` — RFC-0097 M1's migration path, and RFC-0097 §6's
/// worked example: `examples/shelf/vyrn.json` as VON.
#[test]
fn from_json_prints_a_manifest_as_von() {
    let out = vyrn()
        .arg("fmt")
        .arg("--from-json")
        .arg(repo_file("examples/shelf/vyrn.json"))
        .arg("--as")
        .arg("Manifest")
        .arg("--from")
        .arg("std/manifest")
        .output()
        .expect("vyrn fmt --from-json");
    assert!(out.status.success(), "{}", combined(&out));
    let text = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    assert_eq!(
        text,
        "import type { Manifest } from \"std/manifest\"\n\n\
         Manifest {\n    \
         name: \"shelf\",\n    \
         server: \"server.vyrn\",\n    \
         client: \"client/boot.vyrn\",\n    \
         public: \"public\",\n    \
         artifacts: [\n        \
         \"server\": [\"entry\": \"server.vyrn\", \"target\": \"native\"],\n        \
         \"client\": [\"entry\": \"client/boot.vyrn\", \"target\": \"browser\"],\n    \
         ],\n    \
         audience: [\n        \
         \"server\": [\"server\"],\n        \
         \"client\": [\"client\"],\n        \
         \"universal\": [\"app\", \"shared\"],\n    \
         ],\n}\n"
    );
    // And the printed document is a canonical `.von` file.
    let dir = scratch("from-json");
    let file = dir.join("vyrn.von");
    std::fs::write(&file, text.as_bytes()).unwrap();
    let out = vyrn()
        .arg("fmt")
        .arg("--check")
        .arg(&file)
        .output()
        .expect("vyrn fmt");
    assert_eq!(out.status.code(), Some(0), "{}", combined(&out));
}

/// The conversion refuses what VON has no spelling for, and says so against the
/// INPUT file's name. `std/json`'s strict reader catches the duplicate key
/// before the walk runs — there is only one JSON reader in the toolchain.
#[test]
fn from_json_refuses_null_and_a_duplicate_key() {
    let dir = scratch("refusals");
    let nul = dir.join("nul.json");
    std::fs::write(&nul, "{\"main\": null}").unwrap();
    let out = vyrn()
        .arg("fmt")
        .arg("--from-json")
        .arg(&nul)
        .output()
        .expect("vyrn fmt --from-json");
    assert!(!out.status.success());
    let text = combined(&out);
    assert!(text.contains("VON has no null"), "{text}");
    assert!(text.contains("nul.json"), "{text}");
    // The converter's own module is never named — the user cannot open it.
    assert!(!text.contains("from-json.vyrn"), "{text}");

    let dup = dir.join("dup.json");
    std::fs::write(&dup, "{\"a\": 1, \"a\": 2}").unwrap();
    let out = vyrn()
        .arg("fmt")
        .arg("--from-json")
        .arg(&dup)
        .output()
        .expect("vyrn fmt --from-json");
    assert!(!out.status.success());
    assert!(
        combined(&out).contains("duplicate object key: a"),
        "{}",
        combined(&out)
    );
}

/// The writer's own awkward-input round trip: `parseVon(toVon(v))` through the
/// real toolchain. `vyrn fmt --from-json` runs the writer over data a reasonable
/// test never feeds it — map keys holding a quote, a backslash, the target's own
/// punctuation, a control byte and a character above the BMP, two keys differing
/// only by case, empty containers, and an integer no f64 holds — and the emitted
/// document is then READ BACK by `std/von`'s own reader (through `vonModule`),
/// which is the strongest cheap oracle available: what the writer produces, the
/// reader accepts, byte for byte.
///
/// What this cannot check: the value is compared as canonical TEXT, since the
/// reader's tree does not cross the process boundary. A writer and a reader that
/// agreed on the same wrong bytes would pass — which is why the strictness rules
/// have their own suite.
#[test]
fn the_writer_round_trips_the_awkward_keys_and_values() {
    let dir = scratch("awkward-round-trip");
    let json = dir.join("awkward.json");
    std::fs::write(
        &json,
        r#"{"port": 8080,
           "ratio": -0.5,
           "big": 9007199254740993,
           "nothing": {},
           "none": [],
           "labels": {
             "a \"quoted\" key": "v",
             "back\\slash": "b",
             "punctuation: {}, [] ,": "c",
             "\u0001control": "d",
             "𝄞 above the BMP": "e",
             "Case": "upper",
             "case": "lower"}}"#,
    )
    .unwrap();
    let out = vyrn()
        .arg("fmt")
        .arg("--from-json")
        .arg(&json)
        .arg("--as")
        .arg("Awkward")
        .arg("--from")
        .arg("./awkward")
        .output()
        .expect("vyrn fmt --from-json");
    assert!(out.status.success(), "{}", combined(&out));
    let von = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");

    // It is canonically formatted Vyrn — the formatter is the lexer's verdict.
    let doc = dir.join("awkward.von");
    std::fs::write(&doc, von.as_bytes()).unwrap();
    let check = vyrn()
        .arg("fmt")
        .arg("--check")
        .arg(&doc)
        .output()
        .expect("vyrn fmt --check");
    assert_eq!(check.status.code(), Some(0), "{}", combined(&check));

    // And the reader takes it back unchanged: `parseVon(toVon(v))` re-emits the
    // same bytes.
    std::fs::copy(
        repo_file("examples/lib/gen_von.vyrn"),
        dir.join("gen_von.vyrn"),
    )
    .unwrap();
    std::fs::write(
        dir.join("app.vyrn"),
        "import { vonModule } from \"./gen_von\"\n\
         import { configText } from vonModule(\"./awkward.von\")\n\n\
         fn main() -> Int64 {\n    print(configText())\n    return 0\n}\n",
    )
    .unwrap();
    let run = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("vyrn run");
    assert!(
        run.status.success(),
        "the reader refused the writer's document:\n{}",
        combined(&run)
    );
    let printed = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    assert_eq!(
        printed.trim_end_matches('\n').to_string() + "\n",
        von,
        "the document did not survive parse -> emit"
    );
}

/// `VInt`/`VFloat` are public, unvalidated `String` constructors and `emitVon`
/// copies their contents out VERBATIM, as it does a record, field or variant
/// NAME — so an ordinary call used to write a document the reader refuses, or
/// text that is not even Vyrn tokens. The writer now checks a name and a number
/// where it escapes a value, the way `std/html` checks a tag.
///
/// What this validates: every refusal fires, names the text it refused, and
/// fails the program. What it cannot: that a document the writer ACCEPTS is
/// readable — that is the round trip above.
#[test]
fn emit_von_refuses_a_name_or_a_number_it_cannot_spell() {
    let dir = scratch("writer-refusals");
    for (name, value, wanted) in [
        // A name that is not a Vyrn identifier: the target's own punctuation and
        // a newline, an empty name, a digit-led name, a keyword, a space.
        (
            "record-punctuation",
            "VRecord(\"Cfg }\\ndrop x\", [])",
            "is not a usable record name",
        ),
        (
            "record-empty",
            "VRecord(\"\", [])",
            "is not a usable record name",
        ),
        (
            "record-digit-led",
            "VRecord(\"2fa\", [])",
            "is not a usable record name",
        ),
        (
            "variant-keyword",
            "VVariant(\"match\", [])",
            "is not a usable variant name",
        ),
        (
            "field-space",
            "VRecord(\"Cfg\", [VonField { name: \"a b\", value: VBool(true), line: 0 }])",
            "is not a usable field name",
        ),
        // A number VON has no spelling for: hex, a leading zero, an exponent, a
        // digit separator, an empty text, and a float that is not one.
        ("int-hex", "VInt(\"0x1f\")", "is not a usable integer"),
        (
            "int-leading-zero",
            "VInt(\"007\")",
            "is not a usable integer",
        ),
        ("int-exponent", "VInt(\"1e9\")", "is not a usable integer"),
        (
            "int-separator",
            "VInt(\"1_000\")",
            "is not a usable integer",
        ),
        ("int-empty", "VInt(\"\")", "is not a usable integer"),
        ("float-no-point", "VFloat(\"1\")", "is not a usable float"),
        (
            "float-trailing-point",
            "VFloat(\"1.\")",
            "is not a usable float",
        ),
    ] {
        let file = dir.join(format!("{name}.vyrn"));
        std::fs::write(
            &file,
            format!(
                "import {{ Von, VonField, emitVon }} from \"std/von\"\n\
                 fn main() -> Int64 {{\n    print(emitVon({value}))\n    return 0\n}}\n"
            ),
        )
        .unwrap();
        let out = vyrn().arg("run").arg(&file).output().expect("vyrn run");
        let text = combined(&out);
        assert!(!out.status.success(), "{name} emitted a document:\n{text}");
        assert!(text.contains(wanted), "{name}: unexpected failure:\n{text}");
    }
}
