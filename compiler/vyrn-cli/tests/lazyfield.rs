//! `lazy` record fields. `examples/lazyfield.vyrn` pins the semantics
//! in its `test` blocks and runs in the fixture corpus. This file holds what the
//! corpus does not reach: the refusals, `lazy` as a contextual word, and the cost
//! claim that a lazy field lowers exactly as a stored nullary closure.

use std::path::PathBuf;
use std::process::Command;

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

fn norm(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

fn write(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("vyrn-lazyfield");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.vyrn"));
    std::fs::write(&path, src).unwrap();
    path
}

/// `vyrn check` on `src`, as one string (stdout + stderr).
fn check(name: &str, src: &str) -> String {
    let path = write(name, src);
    let out = vyrn().arg("check").arg(&path).output().unwrap();
    format!("{}{}", norm(&out.stdout), norm(&out.stderr))
}

fn run(name: &str, src: &str) -> String {
    let path = write(name, src);
    let out = vyrn().arg("run").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "run failed:\n{}{}",
        norm(&out.stdout),
        norm(&out.stderr)
    );
    norm(&out.stdout)
}

/// The fixture corpus runs an example's `main`, never its tests. The count has a
/// floor because a suite that stops being discovered otherwise passes.
#[test]
fn the_corpus_examples_own_suite_runs() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/lazyfield.vyrn")
        .canonicalize()
        .unwrap();
    let out = vyrn().arg("test").arg(&path).output().unwrap();
    let text = format!("{}{}", norm(&out.stdout), norm(&out.stderr));
    assert!(out.status.success(), "{text}");
    let ran = text.matches("... ok").count();
    assert!(ran >= 4, "only {ran} blocks ran:\n{text}");
}

/// The deferral is a fact about a declared field, as an inline field `where` is.
#[test]
fn an_anonymous_record_may_not_declare_a_lazy_field() {
    let out = check(
        "anon",
        "fn f(b: { x: lazy String }) -> Int64 { return 1 }\nfn main() -> Int64 { return 0 }\n",
    );
    assert!(
        out.contains("a `lazy` field needs a named record type"),
        "{out}"
    );
}

/// An inline `where` rewrites the field's type into a synthetic named one, which
/// would hide the marker and make a read stop forcing.
#[test]
fn a_lazy_field_may_not_carry_an_inline_where() {
    let out = check(
        "wherecl",
        "type B = { x: lazy String where x.byteLength > 0 }\nfn main() -> Int64 { return 0 }\n",
    );
    assert!(
        out.contains("a `lazy` field may not carry an inline `where`"),
        "{out}"
    );
}

#[test]
fn a_lazy_field_is_not_built_from_an_eager_value() {
    let out = check(
        "eager",
        "type B = { body: lazy String }\nfn main() -> Int64 { let b = B { body: \"x\" }\n return 0 }\n",
    );
    assert!(out.contains("lazy String"), "{out}");
}

#[test]
fn a_read_cannot_recover_the_thunk() {
    let out = check(
        "steal",
        "type B = { body: lazy String }\n\
         fn main() -> Int64 { let b = B { body: () -> \"x\" }\n \
         let f: fn() -> String = b.body\n return 0 }\n",
    );
    assert!(
        out.contains("declared fn() -> String but initializer is String"),
        "{out}"
    );
}

/// A decoded value arrives as data with no thunk behind it, so there is nothing
/// to defer.
#[test]
fn a_lazy_field_encodes_but_does_not_decode() {
    let out = check(
        "decode",
        "type B = { title: String, body: lazy String }\n\
         fn main() -> Int64 { let r = fromJson<B>(\"{}\")\n return 0 }\n",
    );
    assert!(out.contains("cannot decode into `B`"), "{out}");

    let stdout = run(
        "encode",
        "type B = { title: String, body: lazy String }\n\
         fn main() -> Int64 { let b = B { title: \"t\", body: () -> \"deferred\" }\n \
         print(toJson(b))\n return 0 }\n",
    );
    assert_eq!(stdout.trim(), "{\"title\":\"t\",\"body\":\"deferred\"}");
}

/// The schema describes what `toJson` writes, and `toJson` forces, so a client
/// never sees the deferral.
#[test]
fn the_schema_shows_the_forced_type() {
    let stdout = run(
        "schema",
        "type B = { title: String, body: lazy Int64 }\n\
         fn main() -> Int64 { print(jsonSchema<B>())\n return 0 }\n",
    );
    assert!(
        stdout.contains("\"body\":{\"type\":\"integer\"}"),
        "{stdout}"
    );
}

/// `lazy` is contextual, read only where a record field's type begins, so
/// `std/ui`'s `lazy(..)` function and ordinary bindings keep the name.
#[test]
fn lazy_is_still_an_ordinary_identifier_everywhere_else() {
    let stdout = run(
        "contextual",
        "type B = { body: lazy String }\n\
         fn lazy(n: Int64) -> Int64 { return n + 1 }\n\
         fn main() -> Int64 { let lazy = lazy(1)\n \
         let b = B { body: () -> \"v\" }\n \
         print(\"\\{lazy} \\{b.body}\")\n return 0 }\n",
    );
    assert_eq!(stdout.trim(), "2 v");
}

/// The cost claim: a `lazy T` field is `fn() -> T` and nothing else,
/// so the same program written both ways emits identical wasm, down to the call
/// through the synthesized dispatcher (#452). A record without a lazy field
/// therefore pays nothing.
#[test]
fn a_lazy_field_lowers_exactly_as_the_stored_closure_it_is() {
    let wat_of = |name: &str, src: &str| -> String {
        let path = write(name, src);
        let out = vyrn()
            .env("VYRN_WASM_NAMES", "1")
            .arg("emit-wat")
            .arg(&path)
            .output()
            .expect("vyrn emit-wat");
        assert!(out.status.success(), "{name}: {}", norm(&out.stderr));
        norm(&out.stdout)
    };
    let deferred = wat_of(
        "wat_lazy",
        "type B = { tag: String, body: lazy String }\n\
         fn make(n: Int64) -> B {\n\
         \x20   let pre = \"prefix-\\{n}\"\n\
         \x20   return B { tag: \"t\\{n}\", body: () -> \"\\{pre}!\" }\n\
         }\n\
         fn main() -> Int64 {\n\
         \x20   let b = make(1)\n\
         \x20   print(b.body)\n\
         \x20   return 0\n\
         }\n",
    );
    let explicit = wat_of(
        "wat_fnfield",
        "type B = { tag: String, body: fn() -> String }\n\
         fn make(n: Int64) -> B {\n\
         \x20   let pre = \"prefix-\\{n}\"\n\
         \x20   return B { tag: \"t\\{n}\", body: () -> \"\\{pre}!\" }\n\
         }\n\
         fn main() -> Int64 {\n\
         \x20   let b = make(1)\n\
         \x20   let f = b.body\n\
         \x20   print(f())\n\
         \x20   return 0\n\
         }\n",
    );
    assert_eq!(
        deferred, explicit,
        "a lazy field must lower as the stored closure it is, and nothing else"
    );
}

/// A generic record binds `T` through a `lazy T` field.
#[test]
fn a_generic_lazy_field_binds_its_parameter() {
    let out = run(
        "generic",
        "type Cell<T> = { name: String, v: lazy T }\n\
         fn get<T>(c: Cell<T>) -> T {\n\
         \x20   return c.v\n\
         }\n\
         fn main() -> Int64 {\n\
         \x20   let c: Cell<Int64> = Cell { name: \"a\", v: () -> 4 }\n\
         \x20   print(get(c))\n\
         \x20   return 0\n\
         }\n",
    );
    assert_eq!(out, "4\n");
}

/// A `fn() -> T` value binds a generic `lazy T` field, as it fills a concrete one.
#[test]
fn a_function_value_binds_a_generic_lazy_field() {
    let out = run(
        "generic_fn",
        "type Cell<T> = { name: String, v: lazy T }\n\
         fn main() -> Int64 {\n\
         \x20   let f: fn() -> Int64 = () -> 4\n\
         \x20   let c: Cell<Int64> = Cell { name: \"a\", v: f }\n\
         \x20   print(c.v)\n\
         \x20   return 0\n\
         }\n",
    );
    assert_eq!(out, "4\n");
}
