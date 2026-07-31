//! `??` — handle-or-default in one token (RFC-0079 M2).
//!
//! `??` is not an operator in any backend: it desugars in the PARSER to a
//! `match` over two type-agnostic patterns (`Pattern::Success`/`Pattern::Failure`,
//! unspellable in source), so drops, ownership, validation and short-circuiting
//! are inherited rather than restated. This file therefore tests the two things
//! a desugar can still get wrong — the shape it builds, and the precedence it
//! is parsed at — plus the one property the desugar has to preserve rather than
//! establish: that `match` never evaluates the arm it did not take.
//!
//! The three-engine byte-parity case lives with the others, in `parity.rs`
//! (`nullish_and_panic_say_the_same_bytes_on_all_three_engines`); everything
//! here runs on the interpreter, which is where the parse is decided.

use std::path::PathBuf;
use std::process::Command;

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

fn norm(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

fn write(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("vyrn-nullish");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.vyrn"));
    std::fs::write(&path, src).unwrap();
    path
}

/// The two sums under test, plus a loud helper whose call is observable.
const PRELUDE: &str = "\
fn half(n: Int64) -> Option<Int64> {
    if n % 2 == 0 {
        return Some(n / 2)
    }
    return None
}

fn toNum(s: String) -> Result<Int64, String> {
    if s == \"one\" {
        return Ok(1)
    }
    return Err(\"bad: \" + s)
}

fn flag(b: Bool) -> Option<Bool> {
    return Some(b)
}

fn loud(n: Int64) -> Int64 {
    print(\"loud\")
    return n
}
";

/// Run `PRELUDE` plus a `main` printing each expression, and return stdout.
fn prints(name: &str, exprs: &[&str]) -> String {
    let body: String =
        exprs.iter().map(|e| format!("    print({e})\n")).collect();
    let src = format!("{PRELUDE}\nfn main() -> Int64 {{\n{body}    return 0\n}}\n");
    let path = write(name, &src);
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
fn rejects(name: &str, main: &str, needle: &str) {
    let src = format!("{PRELUDE}\nfn main() -> Int64 {{\n{main}\n    return 0\n}}\n");
    let path = write(name, &src);
    let out = vyrn().arg("check").arg(&path).output().expect("vyrn check");
    let all = norm(&out.stdout) + &norm(&out.stderr);
    assert!(!out.status.success(), "{name} was accepted:\n{all}");
    assert!(all.contains(needle), "{name}: expected {needle:?}, got:\n{all}");
}

#[test]
fn nullish_unwraps_an_option() {
    assert_eq!(prints("opt", &["half(10) ?? -1", "half(7) ?? -1"]), "5\n-1\n");
}

/// The `Result` path takes the same two arms — and the error payload never
/// reaches stdout, because `Failure`'s binder is read by nothing.
#[test]
fn nullish_unwraps_a_result_and_discards_the_error() {
    let out = prints("res", &["toNum(\"one\") ?? -1", "toNum(\"two\") ?? -1"]);
    assert_eq!(out, "1\n-1\n");
    assert!(!out.contains("bad:"), "the error payload leaked into stdout: {out}");
}

/// `a ?? b ?? c` is `a ?? (b ?? c)`. That it compiles at all is the proof: `??`
/// yields an unwrapped `T`, so the left-associative grouping would apply `??` to
/// a non-sum and the checker would refuse it.
#[test]
fn nullish_chains_right_associatively() {
    assert_eq!(
        prints("chain", &["half(7) ?? half(4) ?? -1", "half(7) ?? half(3) ?? -1"]),
        "2\n-1\n"
    );
}

/// Tighter than `&&` and `||`. Both cases are chosen to *disagree* under the
/// other grouping: `Some(true) ?? true && false` is `false` bound this way and
/// `true` bound the other, and `Some(false) ?? false || true` is `true` here and
/// `false` there.
#[test]
fn nullish_binds_tighter_than_the_logical_operators() {
    assert_eq!(
        prints("logic", &["flag(true) ?? true && false", "flag(false) ?? false || true"]),
        "false\ntrue\n"
    );
}

/// Looser than comparison — `x ?? 0 == 5` is `x ?? (0 == 5)`, NOT `(x ?? 0) == 5`.
/// (RFC-0079's M2 section asserted the second in one clause while specifying the
/// first in two others; the two won. See the as-landed note there.)
#[test]
fn nullish_binds_tighter_than_comparison_and_looser_than_arithmetic() {
    // The case that decided the binding power. Both readings typecheck when the
    // option is a `Bool`, so the parse is not protected by the type checker:
    //   (flag(false) ?? true) == false  ->  false == false  ->  true
    //    flag(false) ?? (true == false) ->  Some(false)     ->  false
    // `??` first shipped tied with `==`, which took the second reading silently.
    assert_eq!(prints("cmp_bool", &["flag(false) ?? true == false"]), "true\n");

    // The same shape on an `Int64` option, which is the spelling a reader
    // actually writes: default it, *then* compare.
    assert_eq!(prints("cmp_int", &["half(7) ?? 0 == 5"]), "false\n");
    assert_eq!(prints("cmp_some", &["half(10) ?? 0 == 5"]), "true\n");

    // The other neighbour, unchanged and deliberately so: the right-hand side is
    // the fallback *value*, so it takes its own arithmetic with it.
    assert_eq!(prints("arith_none", &["half(7) ?? 1 + 1"]), "2\n");
    assert_eq!(prints("arith_some", &["half(10) ?? 1 + 1"]), "5\n");
}

/// The point of the whole design: the right-hand side is not evaluated when the
/// left succeeds. `loud` prints, so a lost short-circuit is visible rather than
/// merely slow.
#[test]
fn the_right_hand_side_is_not_evaluated_when_the_left_succeeds() {
    assert_eq!(prints("lazy_ok", &["half(10) ?? loud(-1)"]), "5\n");
    assert_eq!(prints("lazy_none", &["half(7) ?? loud(-1)"]), "loud\n-1\n");
}

/// `??` is maximal munch: `a??b` with no spaces is one token, not two postfix
/// `?`. RFC-0079 recorded the cost of that as "the old `x??` spelling must now
/// be written `(x?)?`", but the spelling it worried about needed a
/// `Result<Option<T>, E>` — and nested sums are refused outright ("nested
/// Option/Result is not supported in v0.1"), so `Try(Try(x))` was never
/// reachable on any type. The munch takes nothing away; only that it happens
/// needs pinning.
#[test]
fn double_question_is_one_token_even_unspaced() {
    assert_eq!(prints("munch", &["half(7)??-1"]), "-1\n");
}

/// `??` inherits `match`'s reclamation exactly — which for an arm binder means
/// none, in both spellings. `own.rs` makes only `let` bindings droppable, so a
/// hand-written `Err(e)` arm does not free `e` either; the invariant worth
/// pinning is that the desugar adds no free and drops no free relative to the
/// `match` a user would write by hand.
#[test]
fn the_desugar_frees_exactly_what_the_handwritten_match_frees() {
    let frees = |name: &str, expr: &str| -> usize {
        let src = format!(
            "{PRELUDE}\nfn main() -> Int64 {{\n    let n = {expr}\n    return n\n}}\n"
        );
        let path = write(name, &src);
        let out = vyrn().arg("emit-ir").arg(&path).output().expect("vyrn emit-ir");
        assert!(out.status.success(), "{name}: {}", norm(&out.stderr));
        norm(&out.stdout).matches("call void @free(ptr").count()
    };
    assert_eq!(
        frees("free_sugar", "toNum(\"two\") ?? -1"),
        frees("free_match", "match toNum(\"two\") { Ok(v) => v, Err(e) => -1 }"),
        "`??` must reclaim exactly what the `match` it desugars to reclaims"
    );
}
