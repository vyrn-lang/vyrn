//! `vyrn fix`. A move diagnostic is a menu: the offending line, then one
//! `fix:` per way out. `vyrn fix` applies the one entry that is an edit rather than a
//! decision, `.copy()`, and refuses the rest by name; each refusal has its own test.

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

/// Writes `src`, runs `vyrn fix` on it, and returns `(stdout, the file afterwards)`.
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
    // The menu names `for x in consume xs` first, but that entry decides nothing after
    // the loop wants the container, which is the author's call, so the second is applied.
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
fn it_copies_each_of_two_occurrences_on_one_line() {
    // The diagnostic carries the column of each path, so a line that takes the
    // same name twice needs no choice between the two.
    let src = "fn twice(s: read String) -> Array<String> {\n\
                   let mut out: Array<String> = []\n\
                   out.push(s) out.push(s)\n\
                   return out\n\
               }\n\
               fn main() -> Int64 { return 0 }\n";
    let (log, after) = fix("twice", src);
    assert!(
        after.contains("out.push(s.copy()) out.push(s.copy())"),
        "{after}"
    );
    assert!(log.contains("2 fix(es) applied, 0 left"), "{log}");
    assert!(checks("twice-after", &after), "{after}");
}

#[test]
fn it_copies_a_name_on_a_later_line_than_the_statement() {
    let src = "type Box = { a: String, b: String }\n\
               fn g(s: read String) -> Box {\n\
                   return Box {\n\
                       a: \"x\",\n\
                       b: s,\n\
                   }\n\
               }\n\
               fn main() -> Int64 { return 0 }\n";
    let (_, after) = fix("later-line", src);
    assert!(after.contains("b: s.copy(),"), "{after}");
    assert!(checks("later-line-after", &after), "{after}");
}

#[test]
fn it_copies_a_value_where_it_was_moved_rather_than_where_it_is_used_again() {
    let src = "fn f() -> Array<String> {\n\
                   let s = \"a\" + \"b\"\n\
                   let mut out: Array<String> = []\n\
                   out.push(s)\n\
                   out.push(s)\n\
                   return out\n\
               }\n\
               fn main() -> Int64 { return 0 }\n";
    let (_, after) = fix("moved", src);
    assert!(
        after.contains("out.push(s.copy())\nout.push(s)\n"),
        "{after}"
    );
    assert!(checks("moved-after", &after), "{after}");
}

#[test]
fn it_copies_the_place_a_binding_reads_where_it_is_bound() {
    // The refusal is at the write, the edit at the `let` that made the alias.
    let src = "type Tbl = { xs: Array<Int64> }\n\
               fn main() -> Int64 {\n\
                   let mut t = Tbl { xs: [1, 2] }\n\
                   let before = t.xs\n\
                   t.xs[0] = 99\n\
                   print(before[0])\n\
                   return 0\n\
               }\n";
    let (log, after) = fix("alias-read", src);
    assert!(after.contains("let before = t.xs.copy()\n"), "{after}");
    assert!(log.contains("1 fix(es) applied, 0 left"), "{log}");
    assert!(checks("alias-read-after", &after), "{after}");
}

#[test]
fn it_copies_the_place_a_rebuilt_binding_reads_where_it_is_bound() {
    let src = "type Head = { meta: Array<String> }\n\
               fn withOne(h: Head, s: String) -> Head {\n\
                   let mut mt = h.meta\n\
                   mt.push(s.copy())\n\
                   return Head { meta: mt }\n\
               }\n\
               fn main() -> Int64 {\n\
                   let h = Head { meta: [\"a\"] }\n\
                   let g = withOne(h, \"b\".copy())\n\
                   print(g.meta.length + h.meta.length)\n\
                   return 0\n\
               }\n";
    let (_, after) = fix("rebuilt-borrow", src);
    assert!(after.contains("let mut mt = h.meta.copy()\n"), "{after}");
    assert!(checks("rebuilt-borrow-after", &after), "{after}");
}

#[test]
fn it_copies_the_consumed_argument_that_a_place_of_it_overlaps() {
    let src = "type Cell = { name: String, v: Float64 }\n\
               fn g(a: consume Cell, b: String) -> Int64 {\n\
                   let n = consume a.name\n\
                   drop n\n\
                   return b.byteLength\n\
               }\n\
               fn main() -> Int64 {\n\
                   let x = Cell { name: 1234567.toString(), v: 1.0 }\n\
                   let n = g(x, x.name)\n\
                   print(n)\n\
                   return 0\n\
               }\n";
    let (_, after) = fix("consumed-and-passed", src);
    assert!(after.contains("g(x.copy(), x.name)"), "{after}");
    assert!(checks("consumed-and-passed-after", &after), "{after}");
}

#[test]
fn it_copies_the_name_an_arm_hands_out_of_a_loop() {
    let head = "fn size(xs: Array<String>) -> Int64 { return xs.length }\n\
                fn main() -> Int64 {\n\
                    let names: Array<String> = [\"a\", \"b\"]\n\
                    let opts: Array<Option<Int64>> = [None, Some(1)]\n\
                    let mut n = 0\n\
                    let mut i = 0\n\
                    while i < 2 {\n";
    let tail = "i = i + 1\n}\nreturn n\n}\n";
    let cases = [
        (
            "then",
            "let p: Array<String> = if i > 0 { names } else { [\"z\"] }\nn = n + p.length\n",
        ),
        (
            "else",
            "let p: Array<String> = if i > 0 { [\"z\"] } else { names }\nn = n + p.length\n",
        ),
        (
            "match",
            "let p: Array<String> = match opts[i] { None => [\"z\"], Some(_) => names }\n\
             n = n + p.length\n",
        ),
        (
            "argument",
            "n = n + size(if i > 0 { names } else { [\"z\"] })\n",
        ),
    ];
    for (name, body) in cases {
        let src = format!("{head}{body}{tail}");
        let (_, after) = fix(&format!("loop-arm-{name}"), &src);
        assert!(after.contains("names.copy()"), "{name}:\n{after}");
        assert!(!after.contains("names.copy().copy()"), "{name}:\n{after}");
        let again = format!("loop-arm-{name}-after");
        assert!(checks(&again, &after), "{name}:\n{after}");
    }
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

const PERSON: &str = "type Person = { name: String, age: Int64 }\n\
                      fn take(s: consume String) -> Int64 { return s.byteLength }\n\
                      fn keep(p: consume Person) -> Int64 { return p.age }\n";

/// Runs `vyrn fix` on `PERSON` and `main`'s `body`: the text must be refused
/// before, hold `want` after (and no `consume` in its place), and check.
fn replaces_consume(name: &str, body: &str, want: &str) {
    let src = format!("{PERSON}fn main() -> Int64 {{\n{body}return 0\n}}\n");
    assert!(!checks(&format!("{name}-before"), &src));
    let (log, after) = fix(name, &src);
    assert!(after.contains(want), "{after}");
    assert!(log.contains("2 fix(es) applied, 0 left"), "{log}");
    assert!(checks(&format!("{name}-after"), &after), "{after}");
}

#[test]
fn it_replaces_the_consume_that_left_a_hole_a_whole_use_meets() {
    let body = "let p = Person { name: \"n\", age: 1 }\n\
                let a = take(consume p.name)\n\
                let b = keep(p)\n\
                print(a + b)\n";
    replaces_consume("whole-with-hole", body, "take(p.name.copy())");
}

#[test]
fn it_replaces_the_consume_whose_place_is_read_again() {
    let body = "let p = Person { name: \"n\", age: 1 }\n\
                let a = take(consume   p.name)\n\
                print(a + p.name.byteLength)\n";
    replaces_consume("read-in-hole", body, "take(p.name.copy())");
}

#[test]
fn it_replaces_the_consume_a_loop_repeats() {
    let body = "let p = Person { name: \"n\", age: 1 }\n\
                let mut n = 0\n\
                for i in [1, 2] {\n\
                    n = n + take(consume p.name)\n\
                }\n\
                print(n)\n";
    replaces_consume("loop-hole", body, "take(p.name.copy())");
}

#[test]
fn it_copies_the_first_of_two_takes_of_one_name_in_a_statement() {
    let cases = [
        (
            "literal",
            "let p = P { a: x, b: x }\nprint(p.a)\n",
            "P { a: x.copy(), b: x }",
        ),
        ("array", "let p = [x, x]\nprint(p[0])\n", "[x.copy(), x]"),
        ("call", "let n = g(x, x)\nprint(n)\n", "g(x.copy(), x)"),
    ];
    for (name, body, want) in cases {
        let src = format!(
            "type P = {{ a: String, b: String }}\n\
             fn g(a: String, b: consume String) -> Int64 {{ return a.byteLength + b.byteLength }}\n\
             fn main() -> Int64 {{\nlet x = \"ab\" + \"c\"\n{body}return 0\n}}\n"
        );
        assert!(!checks(&format!("twice-{name}-before"), &src));
        let (log, after) = fix(&format!("twice-{name}"), &src);
        assert!(after.contains(want), "{name}:\n{after}");
        assert!(log.contains("1 fix(es) applied, 0 left"), "{name}:\n{log}");
        assert!(
            checks(&format!("twice-{name}-after"), &after),
            "{name}:\n{after}"
        );
    }
}
