//! `lazy` record fields — RFC-0085 M4a.
//!
//! The three-engine byte-parity case is `examples/lazyfield.vyrn`, swept by
//! `parity.rs`, and its own `test` blocks are what pin the semantics: a field
//! never read is never computed, a field read twice is computed twice, and
//! `toJson` forces. What is left for this file is what the corpus does not
//! reach — the refusals, the two words that must not collide, and the one
//! property the milestone's cost claim rests on: that the deferral IS RFC-0037's
//! stored nullary closure and nothing else, so a record with a lazy field lowers
//! exactly as a record with an ordinary `fn`-typed field does.

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

/// The declaration needs a name, for the same reason an inline field `where`
/// does: the deferral is a fact about a DECLARED field, and there is nothing to
/// hang it on otherwise.
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
/// would bury the marker where nothing looks for it — a read that quietly stopped
/// forcing. Refused by name rather than mis-lowered.
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

/// The construction site writes the thunk and is meant to see that it is one.
/// An eager value in a deferred field is a type error naming the modifier.
#[test]
fn a_lazy_field_is_not_built_from_an_eager_value() {
    let out = check(
        "eager",
        "type B = { body: lazy String }\nfn main() -> Int64 { let b = B { body: \"x\" }\n return 0 }\n",
    );
    assert!(out.contains("lazy String"), "{out}");
}

/// Nothing in the surface can name the thunk: a read is the forced value, so a
/// `fn`-typed binding cannot capture the deferral and defeat it.
#[test]
fn a_read_cannot_recover_the_thunk() {
    let out = check(
        "steal",
        "type B = { body: lazy String }\n\
         fn main() -> Int64 { let b = B { body: || \"x\" }\n \
         let f: fn() -> String = b.body\n return 0 }\n",
    );
    assert!(
        out.contains("declared fn() -> String but initializer is String"),
        "{out}"
    );
}

/// `toJson` forces (pinned in the corpus), and `fromJson` refuses: a decoded
/// value arrives as data with no thunk behind it, so there is nothing to defer.
/// A decoder that manufactured a constant thunk would be laziness that had
/// already done the work.
#[test]
fn a_lazy_field_encodes_but_does_not_decode() {
    let out = check(
        "decode",
        "type B = { title: String, body: lazy String }\n\
         fn main() -> Int64 { let r = fromJson(B, \"{}\")\n return 0 }\n",
    );
    assert!(out.contains("cannot decode into `B`"), "{out}");

    // The encode half compiles and answers with the forced value.
    let stdout = run(
        "encode",
        "type B = { title: String, body: lazy String }\n\
         fn main() -> Int64 { let b = B { title: \"t\", body: || \"deferred\" }\n \
         print(toJson(b))\n return 0 }\n",
    );
    assert_eq!(stdout.trim(), "{\"title\":\"t\",\"body\":\"deferred\"}");
}

/// The schema describes what `toJson` writes, and `toJson` forces — so nothing
/// about the deferral is visible to a client. That is the point of putting the
/// fact on the field: one declaration, and no projection restates it.
#[test]
fn the_schema_shows_the_forced_type() {
    let stdout = run(
        "schema",
        "type B = { title: String, body: lazy Int64 }\n\
         fn main() -> Int64 { print(jsonSchema(B))\n return 0 }\n",
    );
    assert!(
        stdout.contains("\"body\":{\"type\":\"integer\"}"),
        "{stdout}"
    );
}

/// `lazy` is CONTEXTUAL, exactly as `mut fn` is: it is read only where a record
/// field's type begins, so `std/ui`'s `lazy(..)` FUNCTION (RFC-0070 — lazy
/// PAGES, a different mechanism one layer up, with a `Loading` state and a
/// network behind it) keeps its name, and so does an ordinary binding.
#[test]
fn lazy_is_still_an_ordinary_identifier_everywhere_else() {
    let stdout = run(
        "contextual",
        "type B = { body: lazy String }\n\
         fn lazy(n: Int64) -> Int64 { return n + 1 }\n\
         fn main() -> Int64 { let lazy = lazy(1)\n \
         let b = B { body: || \"v\" }\n \
         print(\"\\{lazy} \\{b.body}\")\n return 0 }\n",
    );
    assert_eq!(stdout.trim(), "2 v");
}

/// The cost claim, checked rather than asserted in prose: a `lazy T` field IS
/// `fn() -> T` (RFC-0037) and nothing else. The same program written both ways —
/// deferred, and an ordinary fn-typed field read into a binding and called —
/// emits **identical IR everywhere outside the one function that reads it**: the
/// record lowers to the same aggregate, the constructor is the same function,
/// and the force goes through the same synthesized dispatcher.
///
/// Which is also the answer to what a lazy field costs a record that has none.
/// Nothing: no shape changed, so nothing that is not deferred can pay for
/// something that is. Inside the reader the deferred spelling is *shorter* — it
/// skips the intermediate binding's spill, which is the only thing the explicit
/// form needs and the implicit one does not.
#[test]
fn a_lazy_field_lowers_exactly_as_the_stored_closure_it_is() {
    let deferred = write(
        "ir_lazy",
        "type B = { tag: String, body: lazy String }\n\
         fn make(n: Int64) -> B {\n\
         \x20   let pre = \"prefix-\\{n}\"\n\
         \x20   return B { tag: \"t\\{n}\", body: || \"\\{pre}!\" }\n\
         }\n\
         fn main() -> Int64 {\n\
         \x20   let b = make(1)\n\
         \x20   print(b.body)\n\
         \x20   return 0\n\
         }\n",
    );
    let explicit = write(
        "ir_fnfield",
        "type B = { tag: String, body: fn() -> String }\n\
         fn make(n: Int64) -> B {\n\
         \x20   let pre = \"prefix-\\{n}\"\n\
         \x20   return B { tag: \"t\\{n}\", body: || \"\\{pre}!\" }\n\
         }\n\
         fn main() -> Int64 {\n\
         \x20   let b = make(1)\n\
         \x20   let f = b.body\n\
         \x20   print(f())\n\
         \x20   return 0\n\
         }\n",
    );
    // `vyrn_main` is the user's `main`; everything else is the shared surface —
    // the record's aggregate, `vyrn_make`, and the dispatcher.
    const READER: &str = "define i64 @vyrn_main() {";
    let split = |p: &PathBuf| -> (String, String) {
        let out = vyrn().arg("emit-ir").arg(p).output().unwrap();
        assert!(out.status.success(), "emit-ir failed: {}", norm(&out.stderr));
        let text = norm(&out.stdout);
        let start = text.find(READER).expect("the user's main");
        let end = text[start..].find("\n}\n").expect("its end") + start + 3;
        (
            format!("{}{}", &text[..start], &text[end..]),
            text[start..end].to_string(),
        )
    };
    let (rest_lazy, main_lazy) = split(&deferred);
    let (rest_fn, main_fn) = split(&explicit);
    assert_eq!(rest_lazy, rest_fn, "the deferral changed a shape");

    // The same dispatcher, called the same way — the whole force.
    let call = main_fn
        .lines()
        .find(|l| l.contains("call ptr @__vyrn_fndispatch_"))
        .expect("the explicit call goes through a dispatcher");
    let sym = call.split_once("@__vyrn_fndispatch_").unwrap().1;
    let sym = &sym[..sym.find('(').unwrap()];
    assert_eq!(
        main_lazy
            .matches(&format!("call ptr @__vyrn_fndispatch_{sym}("))
            .count(),
        1,
        "{main_lazy}"
    );
    // Shorter by exactly the binding the implicit read does not need.
    assert!(
        main_lazy.lines().count() < main_fn.lines().count(),
        "{main_lazy}"
    );
}
