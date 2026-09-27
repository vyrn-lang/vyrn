//! A generator reads a manifest-aliased file, and `std/icons` turns one into a
//! module. A `gen fn` path resolves through the same import map a module specifier uses;
//! the bytes come from the lock and the vendor or cache; refusals keep their existing
//! wordings; and a re-pinned collection misses the generator cache.
//!
//! Every row is offline: a remote key's bytes come out of the project's own
//! `vyrn_vendor/`, where an air-gapped build reads them.

use std::path::{Path, PathBuf};
use std::process::Command;

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("vyrn-icons-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A two-glyph Iconify collection, exercising the header, the box and the body. The
/// emitter's own cases are `std/icons`'s `test` blocks; this is only what must arrive
/// through a pin.
fn collection(prefix: &str, body: &str) -> String {
    format!(
        r#"{{"prefix":"{prefix}",
"info":{{"license":{{"title":"MIT","url":"https://example.test/L"}}}},
"icons":{{"mark":{{"body":"{body}"}}}},
"width":16,"height":16}}"#
    )
}

fn write(path: &Path, text: &str) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

/// Stdout of a successful run, or a panic naming what the compiler said.
fn run_ok(dir: &Path, args: &[&str]) -> String {
    let out = vyrn().current_dir(dir).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "`vyrn {}` failed:\n{}{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Stderr of a run that must not succeed.
fn run_err(dir: &Path, args: &[&str]) -> String {
    let out = vyrn().current_dir(dir).args(args).output().unwrap();
    assert!(
        !out.status.success(),
        "`vyrn {}` succeeded and should not have:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// The manifest maps `coll` to a file beside the manifest; the program sits in `src/`,
/// so path arithmetic alone (`src/` + `coll`) reaches nothing. Only the import map
/// answers, and an alias is what `vyrn add` writes.
#[test]
fn a_generator_reads_a_manifest_aliased_collection() {
    let dir = scratch("alias-read");
    write(
        &dir.join("vyrn.json"),
        r#"{"name":"t","main":"src/main.vyrn","dependencies":{"coll":"./data/toy.json"}}"#,
    );
    write(&dir.join("data/toy.json"), &collection("toy", "<a/>"));
    write(
        &dir.join("src/main.vyrn"),
        "import { icons } from \"std/icons\"\n\
         import * as ic from icons(\"coll\", \"mark\")\n\
         import { toHtmlString } from \"std/html\"\n\
         fn main() -> Int64 { print(toHtmlString(ic.mark())) return 0 }\n",
    );
    let out = run_ok(&dir, &["run"]);
    assert_eq!(
        out.trim(),
        "<svg viewBox=\"0 0 16 16\" width=\"1em\" height=\"1em\" aria-hidden=\"true\"><a/></svg>"
    );
}

/// A name the manifest does not declare is not an alias, so it stays a path and fails as
/// one, in the canonical `readFile` wording. `std/icons` says which of the two
/// it could have been.
#[test]
fn an_undeclared_alias_is_refused() {
    let dir = scratch("alias-unknown");
    write(&dir.join("vyrn.json"), r#"{"name":"t","main":"main.vyrn"}"#);
    write(
        &dir.join("main.vyrn"),
        "import { icons } from \"std/icons\"\n\
         import * as ic from icons(\"coll\", \"mark\")\n\
         fn main() -> Int64 { return 0 }\n",
    );
    let err = run_err(&dir, &["run"]);
    for want in [
        "std/icons cannot read the collection `coll`",
        "cannot read `coll`",
        "a relative path, or a dependency in vyrn.json",
    ] {
        assert!(err.contains(want), "missing {want:?} in: {err}");
    }
}

/// The sandbox rule holds: an alias resolves because it was one of the generator's own
/// constant arguments, and a path built from one that climbs out of the declared roots
/// is refused.
#[test]
fn the_input_root_rule_still_decides() {
    let dir = scratch("alias-escape");
    write(
        &dir.join("vyrn.json"),
        r#"{"name":"t","main":"main.vyrn","dependencies":{"coll":"./data/toy.json"}}"#,
    );
    write(&dir.join("data/toy.json"), &collection("toy", "<a/>"));
    write(&dir.join("secret.txt"), "not yours");
    write(
        &dir.join("peek.vyrn"),
        "export gen fn peek(p: String) -> String {\n\
         \x20   let s = match readFile(p + \"/../../secret.txt\") { Ok(t) => t, Err(e) => \"\" }\n\
         \x20   return \"export fn s() -> Int64 { return 0 }\\n\"\n\
         }\n",
    );
    write(
        &dir.join("main.vyrn"),
        "import { peek } from \"./peek\"\n\
         import * as p from peek(\"coll\")\n\
         fn main() -> Int64 { return p.s() }\n",
    );
    let err = run_err(&dir, &["run"]);
    assert!(
        err.contains("escapes its declared inputs")
            && err.contains("a generator may only read under its constant path arguments"),
        "{err}"
    );
}

/// The generator cache is keyed on the generator's sources, its arguments and its
/// resolved inputs, and an alias's resolved key is one of those, so re-pinning the
/// dependency misses. Otherwise the entry's recorded input is the old file, which still
/// hashes the same, and the build serves the old glyph.
#[test]
fn re_pinning_a_collection_misses_the_generator_cache() {
    let dir = scratch("alias-repin");
    let cache = dir.join("gencache");
    write(&dir.join("a.json"), &collection("toy", "<a/>"));
    write(&dir.join("b.json"), &collection("toy", "<b/>"));
    write(
        &dir.join("main.vyrn"),
        "import { icons } from \"std/icons\"\n\
         import * as ic from icons(\"coll\", \"mark\")\n\
         import { toHtmlString } from \"std/html\"\n\
         fn main() -> Int64 { print(toHtmlString(ic.mark())) return 0 }\n",
    );

    let manifest = |target: &str| {
        format!(r#"{{"name":"t","main":"main.vyrn","dependencies":{{"coll":"{target}"}}}}"#)
    };
    let run = |dir: &Path| {
        let out = vyrn()
            .current_dir(dir)
            .env("VYRN_GEN_CACHE_DIR", &cache)
            .arg("run")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    write(&dir.join("vyrn.json"), &manifest("./a.json"));
    assert!(run(&dir).contains("<a/>"));
    // Same generator, same arguments, same bytes on disk for `a.json`; only the pin
    // moved.
    write(&dir.join("vyrn.json"), &manifest("./b.json"));
    let after = run(&dir);
    assert!(
        after.contains("<b/>"),
        "the re-pinned collection served a stale generation: {after}"
    );
}

/// A REMOTE collection, the shape `vyrn add` writes: the lock names the bytes and
/// the project vendors them, so the read is offline, hash-verified, and takes the
/// module resolver's own path rather than a second one.
#[test]
fn a_pinned_remote_collection_reads_offline_from_the_vendor() {
    let dir = scratch("alias-remote");
    let spec = "github:o/r@".to_string() + &"a".repeat(40) + "/json/toy.json";
    let text = collection("toy", "<a/>");
    let sha = vyrn_frontend::hash::sha256_hex(text.as_bytes());
    write(&dir.join(format!("vyrn_vendor/sha256/{sha}")), &text);
    write(
        &dir.join("vyrn.lock"),
        &format!("{spec}\thttps://example.invalid/toy.json\t{sha}\n"),
    );
    write(
        &dir.join("vyrn.json"),
        &format!(r#"{{"name":"t","main":"main.vyrn","dependencies":{{"coll":"{spec}"}}}}"#),
    );
    write(
        &dir.join("main.vyrn"),
        "import { icons } from \"std/icons\"\n\
         import * as ic from icons(\"coll\", \"mark\")\n\
         import { toHtmlString } from \"std/html\"\n\
         fn main() -> Int64 { print(toHtmlString(ic.mark())) return 0 }\n",
    );
    let out = run_ok(&dir, &["run", "--offline"]);
    assert!(out.contains("<a/>"), "{out}");

    // With the bytes gone, the refusal is the pinning one, with its remedy, not the
    // canonical "cannot read", which says nothing about the lock.
    std::fs::remove_dir_all(dir.join("vyrn_vendor")).unwrap();
    let err = vyrn()
        .current_dir(&dir)
        // A home of its own, so the user's real `~/.vyrn/cache` cannot answer.
        .env("USERPROFILE", dir.join("home"))
        .env("HOME", dir.join("home"))
        .env("VYRN_GEN_CACHE_DIR", dir.join("gencache"))
        .args(["run", "--offline"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&err.stderr).to_string();
    assert!(!err.status.success(), "an uncached pin must not build");
    for want in [
        "is locked (sha256",
        "but not cached, and this is an offline build",
        "run once online, `vyrn vendor`",
    ] {
        assert!(text.contains(want), "missing {want:?} in: {text}");
    }
}

/// The whole surface from a plain `.vyrn` file, with no template language
/// anywhere: two collections in one program, `import * as`, the licence in the
/// generated header, and a misspelled glyph refused with the nearest name.
#[test]
fn two_collections_in_one_program_and_a_misspelled_glyph() {
    let dir = scratch("two-collections");
    write(&dir.join("one.json"), &collection("one", "<a/>"));
    write(&dir.join("two.json"), &collection("two", "<b/>"));
    write(
        &dir.join("vyrn.json"),
        r#"{"name":"t","main":"main.vyrn","dependencies":{"one":"./one.json","two":"./two.json"}}"#,
    );
    write(
        &dir.join("main.vyrn"),
        "import { icons } from \"std/icons\"\n\
         import * as a from icons(\"one\", \"mark\")\n\
         import * as b from icons(\"two\", \"mark\")\n\
         import { toHtmlString } from \"std/html\"\n\
         fn main() -> Int64 {\n\
         \x20   print(toHtmlString(a.mark()))\n\
         \x20   print(toHtmlString(b.mark()))\n\
         \x20   return 0\n\
         }\n",
    );
    let out = run_ok(&dir, &["run"]);
    assert!(out.contains("<a/>") && out.contains("<b/>"), "{out}");

    // The licence rides with the glyphs, in the generated module's own header.
    let emitted = run_ok(&dir, &["emit-gen", "main.vyrn"]);
    assert_eq!(
        emitted
            .matches("/// License: MIT — https://example.test/L")
            .count(),
        2,
        "each collection's terms belong in its own module: {emitted}"
    );

    write(
        &dir.join("main.vyrn"),
        "import { icons } from \"std/icons\"\n\
         import * as a from icons(\"one\", \"marc\")\n\
         fn main() -> Int64 { return 0 }\n",
    );
    let err = run_err(&dir, &["run"]);
    assert!(
        err.contains("the collection `one` has no glyph `marc` — nearest is `mark`"),
        "{err}"
    );
}
