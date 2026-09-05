//! `?` on a user type — RFC-0080 M3.
//!
//! The three-engine byte-parity case is `examples/fallible.vyrn`, swept by
//! `parity.rs`. What is left for this file is what the corpus does not reach:
//! the refusals, the generic impl at two payload types, and the one property
//! the whole milestone rests on — that a `?` on an `Option` or a `Result` did
//! not move.
//!
//! The protocol is declared inline rather than imported from `std/fallible`,
//! which is not a shortcut: the compiler knows only the NAME `Fallible` and its
//! two method names, so a program that declares it is indistinguishable from
//! one that imports it, and this file is where that stays true.

use std::path::PathBuf;
use std::process::Command;

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

fn norm(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

fn write(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("vyrn-fallible");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.vyrn"));
    std::fs::write(&path, src).unwrap();
    path
}

/// A four-variant sum: two successes sharing one `Output`, two failures, one of
/// them carrying a payload nothing in the protocol mentions.
const PRELUDE: &str = "\
protocol Fallible {
    type Output
    fn isSuccess(self) -> Bool
    fn success(self) -> Output
}

type Http = | Body(String) | Created(String) | NotFound | ServerError(String)

impl Fallible for Http {
    type Output = String
    fn isSuccess(self) -> Bool {
        return match self {
            Body(b) => true,
            Created(b) => true,
            NotFound => false,
            ServerError(m) => false,
        }
    }
    fn success(self) -> Output {
        return match self {
            Body(b) => b.copy(),
            Created(b) => b.copy(),
            NotFound => panic(\"unreachable\"),
            ServerError(m) => panic(\"unreachable\"),
        }
    }
}

fn say(h: Http) -> String {
    return match h {
        Body(b) => \"body \" + b,
        Created(b) => \"created \" + b,
        NotFound => \"not found\",
        ServerError(m) => \"server error: \" + m,
    }
}
";

fn run(name: &str, src: &str) -> String {
    let path = write(name, src);
    let out = vyrn().arg("run").arg(&path).output().expect("vyrn run");
    assert!(
        out.status.success(),
        "{name} did not run:\n{}{}",
        norm(&out.stdout),
        norm(&out.stderr)
    );
    norm(&out.stdout)
}

/// Compile-only, expecting a diagnostic containing `needle`.
fn rejects(name: &str, src: &str, needle: &str) {
    let path = write(name, src);
    let out = vyrn().arg("check").arg(&path).output().expect("vyrn check");
    let all = norm(&out.stdout) + &norm(&out.stderr);
    assert!(!out.status.success(), "{name} was accepted:\n{all}");
    assert!(
        all.contains(needle),
        "{name}: expected {needle:?}, got:\n{all}"
    );
}

/// The claim RFC-0080 makes and never executed: Vyrn needs no residual type
/// because `?` copies the whole sum instead of taking it apart, so a failing
/// variant with a payload arrives at the caller as itself. Rust routes exactly
/// this through `FromResidual` and rebuilds it.
///
/// `h.copy()?` rather than `h?`, and the `.copy()` is the rule rather than a
/// detour: the failing path RETURNS the operand, and `h` is a `read` parameter
/// the caller still owns, so RFC-0089 rule 3 offers `consume h` or `h.copy()`
/// and nothing else. The copy is what `examples/fallible.vyrn` gets for free by
/// writing `fetch(code)?` over a temporary. What the test asserts is untouched:
/// the sum still propagates whole, payload and all (RFC-0125 §3 M3, the
/// by-default sweep, the programs tests write).
#[test]
fn a_failing_variant_propagates_with_its_payload_intact() {
    let src = format!(
        "{PRELUDE}
fn pass(h: Http) -> Http {{
    let b = h.copy()?
    return Body(\"[\" + b + \"]\")
}}

fn main() -> Int64 {{
    print(say(pass(Body(\"one\"))))
    print(say(pass(Created(\"two\"))))
    print(say(pass(NotFound)))
    print(say(pass(ServerError(\"upstream\"))))
    return 0
}}
"
    );
    assert_eq!(
        run("payload", &src),
        "body [one]\nbody [two]\nnot found\nserver error: upstream\n",
        "both successes unwrap to one Output; both failures propagate as themselves"
    );
}

/// M1 and M2 compose with M3 without anything being said about it: the impl head
/// binds `T`, `Output` is that same `T`, and the operator monomorphizes per
/// payload type the way any generic call does.
///
/// `s.copy()?` for the reason above, at both payload types: `Slot<String>` owns
/// heap and `Slot<Int64>` does not, and rule 3 asks the question of the
/// parameter rather than of the payload.
#[test]
fn a_generic_impl_serves_every_payload_type() {
    let src = "\
protocol Fallible {
    type Output
    fn isSuccess(self) -> Bool
    fn success(self) -> Output
}

type Slot<T> = | Full(T) | Gone(String)

impl<T> Fallible for Slot<T> {
    type Output = T
    fn isSuccess(self) -> Bool {
        return match self { Full(v) => true, Gone(m) => false }
    }
    fn success(self) -> Output {
        return match self { Full(v) => v, Gone(m) => panic(\"unreachable\") }
    }
}

fn twice(s: Slot<Int64>) -> Slot<Int64> {
    let v = s.copy()?
    return Full(v * 2)
}

fn shout(s: Slot<String>) -> Slot<String> {
    let v = s.copy()?
    return Full(v + \"!\")
}

fn main() -> Int64 {
    print(match twice(Full(21)) { Full(v) => v.toString(), Gone(m) => \"gone \" + m })
    print(match twice(Gone(\"nope\")) { Full(v) => v.toString(), Gone(m) => \"gone \" + m })
    print(match shout(Full(\"hi\")) { Full(v) => v, Gone(m) => \"gone \" + m })
    print(match shout(Gone(\"bye\")) { Full(v) => v, Gone(m) => \"gone \" + m })
    return 0
}
";
    assert_eq!(run("generic", src), "42\ngone nope\nhi!\ngone bye\n");
}

/// Propagation is a copy of the whole value, so there is no error half to check
/// separately the way `Result`'s `assignable(e, re)` is checked — the two sides
/// have to be the same type outright, and the message says that rather than
/// naming a protocol the operand does implement.
#[test]
fn the_whole_value_is_propagated_so_the_return_type_must_be_the_same_one() {
    let src = format!(
        "{PRELUDE}
fn takes(h: Http) -> String {{
    let b = h?
    return b
}}

fn main() -> Int64 {{
    print(takes(NotFound))
    return 0
}}
"
    );
    rejects(
        "same_type",
        &src,
        "`?` propagates the whole Http, but the function returns String",
    );
}

/// Without an impl the operator refuses, and the message names the third option
/// rather than repeating the two nominal ones.
#[test]
fn a_type_with_no_impl_is_refused_and_the_message_names_the_protocol() {
    rejects(
        "no_impl",
        "\
type Http = | Body(String) | NotFound

fn pass(h: Http) -> Http {
    let b = h?
    return Body(b)
}

fn main() -> Int64 { return 0 }
",
        "`?` needs an Option, a Result, or a type that implements `Fallible`, found Http",
    );
}

/// `??` does NOT follow `?` here, and the diagnostic is the milestone's honest
/// edge. `??` desugars in the parser to a `match` over `Success`/`Failure`
/// (RFC-0079 M2); on a sum with more than two variants "the failure side" is a
/// wildcard over N-1 of them, and `Pattern` has no wildcard. `?` reaches a
/// `Fallible` enum precisely because it never pattern-matches at all.
#[test]
fn nullish_does_not_follow_and_says_so_in_the_sources_own_words() {
    let src = format!(
        "{PRELUDE}
fn main() -> Int64 {{
    let h: Http = NotFound
    print(h ?? \"fallback\")
    return 0
}}
"
    );
    rejects(
        "nullish",
        &src,
        "`??` works on an Option or a Result, not on Http",
    );
}

/// The whole point of M3's shape: `Option` and `Result` did not move. Their `?`
/// is still the inline tag test and `extractvalue` it was — no call to an impl
/// method appears in the IR, and the label the lowering emits is still
/// `try.ok`/`try.prop` from `gen_try` rather than the `Fallible` path's.
///
/// Measured over the whole corpus rather than asserted: emitting IR for every
/// example before and after this milestone produced 106 of 106 byte-identical
/// modules, 238 `?` lowerings among them. This test is the part of that a
/// regression would trip over.
#[test]
fn the_nominal_operator_is_untouched() {
    let src = "\
fn half(n: Int64) -> Option<Int64> {
    if n % 2 == 0 { return Some(n / 2) }
    return None
}

fn quarter(n: Int64) -> Option<Int64> {
    let h = half(n)?
    return half(h)
}

fn main() -> Int64 {
    return quarter(8) ?? -1
}
";
    let path = write("nominal", src);
    let out = vyrn()
        .arg("emit-ir")
        .arg(&path)
        .output()
        .expect("vyrn emit-ir");
    assert!(out.status.success(), "{}", norm(&out.stderr));
    let ir = norm(&out.stdout);
    assert!(
        ir.contains("try.ok"),
        "`?` on an Option still lowers through `gen_try`"
    );
    assert!(
        !ir.contains("Fallible__"),
        "`?` on an Option must not call an impl method:\n{ir}"
    );
}
