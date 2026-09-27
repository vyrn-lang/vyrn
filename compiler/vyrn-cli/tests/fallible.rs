//! `?` on a user type, where the corpus does not reach it; parity runs
//! `examples/fallible.vyrn`. The protocol is declared inline: the compiler knows only
//! the name `Fallible` and its two method names, so declaring it must equal importing
//! `std/fallible`.

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

/// Two successes sharing one `Output`, and two failures, one carrying a payload the
/// protocol never mentions.
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

/// Asserts `vyrn check` refuses `src` with a diagnostic containing `needle`.
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

/// `?` copies the whole sum, so Vyrn needs no residual type (Rust's `FromResidual`).
/// The failing path returns the operand and `h` is a `read` parameter, so the
/// ownership rules require `consume h` or `h.copy()`.
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

/// `Output` is the impl head's `T`, and `?` monomorphizes per payload type. Rule 3
/// asks for `s.copy()` at both types because it asks of the parameter, not the
/// payload. `success` takes `read self`, so `Full(v) => v.copy()`: a generic impl
/// reached from `?` alone is judged too.
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
        return match self { Full(v) => v.copy(), Gone(m) => panic(\"unreachable\") }
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

/// There is no error half to check separately, as `Result`'s is, so the two types
/// must be equal.
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

/// `??` desugars to a `match` over `Success` and `Failure`. On a sum with
/// more than two variants the failure side is a wildcard over N-1 of them, and
/// `Pattern` has no wildcard; `?` never pattern-matches.
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
