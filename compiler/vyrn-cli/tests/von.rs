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
    assert!(text.contains("15 passed, 0 failed"), "{text}");
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
