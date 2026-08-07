//! `vyrn fix` integration tests (RFC-0087 U2).
//!
//! Every move diagnostic since RFC-0089 Phase 4b is a menu: the offending line,
//! then one `fix:` per way out. `vyrn fix` applies the one entry that is an edit
//! rather than a decision — `.copy()` — and refuses the rest by name.
//!
//! These tests are about the refusals as much as the fixes. A tool that rewrites
//! source is only worth having if it declines the cases it cannot be sure of,
//! so each refusal has a test of its own.

use std::path::PathBuf;
use std::process::Command;

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("vyrn-fix-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write `src`, run `vyrn fix` on it, return `(stdout, the file afterwards)`.
fn fix(name: &str, src: &str) -> (String, String) {
    let dir = scratch(name);
    let file = dir.join("a.vyrn");
    std::fs::write(&file, src).unwrap();
    let out = vyrn().arg("fix").arg(&file).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        std::fs::read_to_string(&file).unwrap(),
    )
}

/// `vyrn check` on the same text, so a fix is proved by the compiler and not by
/// the test's reading of it.
fn checks(name: &str, src: &str) -> bool {
    let dir = scratch(name);
    let file = dir.join("b.vyrn");
    std::fs::write(&file, src).unwrap();
    vyrn()
        .arg("check")
        .arg(&file)
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn it_copies_a_projection_that_may_not_be_stored() {
    let src = "type Person = { name: String, age: Int64 }\n\
               fn names(ps: read Array<Person>) -> Array<String> {\n\
                   let mut out: Array<String> = []\n\
                   for p in ps {\n\
                       out.push(p.name)\n\
                   }\n\
                   return out\n\
               }\n\
               fn main() -> Int64 { return 0 }\n";
    assert!(!checks("proj-before", src));
    let (log, after) = fix("proj", src);
    assert!(after.contains("out.push(p.name.copy())"), "{after}");
    assert!(log.contains("1 fix(es) applied, 0 left"), "{log}");
    assert!(checks("proj-after", &after), "{after}");
}

#[test]
fn it_copies_a_loop_variable_rather_than_consuming_the_container() {
    // The menu names `for x in consume xs` FIRST. That entry decides that
    // nothing after the loop wants the container, which is the author's call and
    // not the tool's, so the second entry is the one applied.
    let src = "fn dup(xs: read Array<String>) -> Array<String> {\n\
                   let mut out: Array<String> = []\n\
                   for x in xs {\n\
                       out.push(x)\n\
                   }\n\
                   return out\n\
               }\n\
               fn main() -> Int64 { return 0 }\n";
    let (_, after) = fix("loop", src);
    assert!(after.contains("out.push(x.copy())"), "{after}");
    assert!(
        !after.contains("consume"),
        "the container must not be taken:\n{after}"
    );
    assert!(checks("loop-after", &after), "{after}");
}

#[test]
fn it_refuses_a_use_after_consume_because_the_menu_names_no_edit() {
    let src = "type T = { id: Int64 }\n\
               fn useUp(t: consume T) -> Int64 { return t.id }\n\
               fn main() -> Int64 {\n\
                   let x = T { id: 1 }\n\
                   let a = useUp(x)\n\
                   let b = useUp(x)\n\
                   return a + b\n\
               }\n";
    let (log, after) = fix("consumed", src);
    assert_eq!(after, src, "the file must be untouched");
    assert!(log.contains("not fixed:"), "{log}");
    assert!(log.contains("already consumed"), "{log}");
    assert!(log.contains("0 fix(es) applied"), "{log}");
}

#[test]
fn it_refuses_a_line_where_the_path_appears_twice() {
    // The diagnostic carries a line and no column, so two occurrences of the
    // path on one line means the tool cannot say which one it is about.
    let src = "fn twice(s: read String) -> Array<String> {\n\
                   let mut out: Array<String> = []\n\
                   out.push(s) out.push(s)\n\
                   return out\n\
               }\n\
               fn main() -> Int64 { return 0 }\n";
    let (log, after) = fix("twice", src);
    assert_eq!(after, src, "the file must be untouched");
    assert!(log.contains("appears 2 times on the line"), "{log}");
}

#[test]
fn a_clean_file_is_left_exactly_as_it_was() {
    let src = "fn main() -> Int64 {\n    let s = \"a\" + \"b\"\n    print(s)\n    return 0\n}\n";
    let (log, after) = fix("clean", src);
    assert_eq!(after, src);
    assert!(log.contains("0 fix(es) applied, 0 left"), "{log}");
}

#[test]
fn running_it_twice_changes_nothing_the_second_time() {
    let src = "fn label(s: read String) -> String {\n\
                   let t = s\n\
                   return t\n\
               }\n\
               fn main() -> Int64 { print(label(\"hi\")) return 0 }\n";
    let dir = scratch("idempotent");
    let file = dir.join("a.vyrn");
    std::fs::write(&file, src).unwrap();
    vyrn().arg("fix").arg(&file).output().unwrap();
    let once = std::fs::read_to_string(&file).unwrap();
    assert!(once.contains("return t.copy()"), "{once}");
    let out = vyrn().arg("fix").arg(&file).output().unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), once);
    assert!(String::from_utf8_lossy(&out.stdout).contains("0 fix(es) applied"),);
}
