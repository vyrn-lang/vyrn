//! `??`, handle-or-default. The parser desugars it to a `match` over
//! `Pattern::Success` and `Pattern::Failure`, which source cannot spell, so drops,
//! ownership, validation and short-circuiting are inherited. These tests pin what a
//! desugar can still get wrong: the shape, the precedence, and the arm not taken. The
//! three-engine parity case is in `parity.rs`.

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

/// The sums under test, plus `loud`, whose call is observable.
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

/// Runs `PRELUDE` plus a `main` printing each expression, and returns stdout.
fn prints(name: &str, exprs: &[&str]) -> String {
    let body: String = exprs.iter().map(|e| format!("    print({e})\n")).collect();
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

/// `Failure`'s binder is read by nothing, so the error payload never reaches stdout.
#[test]
fn nullish_unwraps_a_result_and_discards_the_error() {
    let out = prints("res", &["toNum(\"one\") ?? -1", "toNum(\"two\") ?? -1"]);
    assert_eq!(out, "1\n-1\n");
    assert!(
        !out.contains("bad:"),
        "the error payload leaked into stdout: {out}"
    );
}

/// `??` yields an unwrapped `T`, so the left grouping would apply `??` to a non-sum
/// and the checker would refuse it.
#[test]
fn nullish_chains_right_associatively() {
    assert_eq!(
        prints(
            "chain",
            &["half(7) ?? half(4) ?? -1", "half(7) ?? half(3) ?? -1"]
        ),
        "2\n-1\n"
    );
}

/// Each case gives a different answer under the other grouping.
#[test]
fn nullish_binds_tighter_than_the_logical_operators() {
    assert_eq!(
        prints(
            "logic",
            &[
                "flag(true) ?? true && false",
                "flag(false) ?? false || true"
            ]
        ),
        "false\ntrue\n"
    );
}

#[test]
fn nullish_binds_tighter_than_comparison_and_looser_than_arithmetic() {
    // Both readings type-check on a `Bool` option, so only the parse decides:
    //   (flag(false) ?? true) == false  ->  false == false  ->  true
    //    flag(false) ?? (true == false) ->  Some(false)     ->  false
    assert_eq!(
        prints("cmp_bool", &["flag(false) ?? true == false"]),
        "true\n"
    );

    // The spelling a reader writes: default it, then compare.
    assert_eq!(prints("cmp_int", &["half(7) ?? 0 == 5"]), "false\n");
    assert_eq!(prints("cmp_some", &["half(10) ?? 0 == 5"]), "true\n");

    // The right-hand side is the fallback value, so it takes its arithmetic with it.
    assert_eq!(prints("arith_none", &["half(7) ?? 1 + 1"]), "2\n");
    assert_eq!(prints("arith_some", &["half(10) ?? 1 + 1"]), "5\n");
}

/// `loud` prints, so a lost short-circuit is visible rather than merely slow.
#[test]
fn the_right_hand_side_is_not_evaluated_when_the_left_succeeds() {
    assert_eq!(prints("lazy_ok", &["half(10) ?? loud(-1)"]), "5\n");
    assert_eq!(prints("lazy_none", &["half(7) ?? loud(-1)"]), "loud\n-1\n");
}

/// Maximal munch costs nothing: two postfix `?` would need a nested sum, which the
/// checker refuses.
#[test]
fn double_question_is_one_token_even_unspaced() {
    assert_eq!(prints("munch", &["half(7)??-1"]), "-1\n");
}

/// The two spellings compile to the same module, byte for byte, so the desugar adds
/// or drops no release.
#[test]
fn the_desugar_is_the_match_it_desugars_to() {
    let module = |name: &str, expr: &str| -> String {
        let src =
            format!("{PRELUDE}\nfn main() -> Int64 {{\n    let n = {expr}\n    return n\n}}\n");
        let path = write(name, &src);
        let out = vyrn()
            .arg("emit-wat")
            .arg(&path)
            .output()
            .expect("vyrn emit-wat");
        assert!(out.status.success(), "{name}: {}", norm(&out.stderr));
        norm(&out.stdout)
    };
    assert_eq!(
        module("sugar", "toNum(\"two\") ?? -1"),
        module("hand", "match toNum(\"two\") { Ok(v) => v, Err(e) => -1 }"),
        "`??` must be the `match` a user would write, and nothing else"
    );
}
