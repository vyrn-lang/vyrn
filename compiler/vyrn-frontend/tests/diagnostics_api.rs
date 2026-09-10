//! Integration tests for the structured-diagnostics core API
//! (`vyrn_frontend::diagnostics`).
//!
//! These guard the contract the CLI and the LSP both rely on: how many
//! diagnostics are produced, with what stage/position, and that the historical
//! `check()` shim stays byte-identical to `diagnostics()[0].render()`.

use vyrn_frontend::diagnostics;

/// A valid program produces no diagnostics.
#[test]
fn valid_program_is_clean() {
    let src = "fn main() -> Int64 { let x = 2 + 3; print(x); return x; }";
    assert!(diagnostics(src).is_empty(), "{:?}", diagnostics(src));
}

/// Two independent type errors (in two functions) are BOTH reported — the
/// bounded accumulation the LSP and `vyrn check` rely on. Order follows the
/// source: f before g.
#[test]
fn accumulates_across_functions() {
    let src = "fn f() -> Int64 { return true; }\nfn g() -> Int64 { let y = \"s\" + 1; return y; }\nfn main() -> Int64 { return f(); }";
    let diags = diagnostics(src);
    assert_eq!(diags.len(), 2, "{:?}", diags);
    assert_eq!(diags[0].stage, "check");
    assert_eq!(diags[1].stage, "check");
    assert!(
        diags[0].message.contains("return type mismatch"),
        "{:?}",
        diags[0]
    );
    assert!(
        diags[1].message.contains("`+` concatenates two Strings"),
        "{:?}",
        diags[1]
    );
    // f is on line 1, g on line 2.
    assert_eq!(diags[0].line, 1);
    assert_eq!(diags[1].line, 2);
}

/// A lex error is reported alone (the lexer stops at the first illegal token).
#[test]
fn lex_error_is_single_and_has_a_column() {
    let src = "fn main() -> Int64 { let x = @; return x; }";
    let diags = diagnostics(src);
    assert_eq!(diags.len(), 1, "{:?}", diags);
    assert_eq!(diags[0].stage, "lex");
    // `@` is the 30th character (1-based) of `fn main() -> Int64 { let x = @; ...`.
    assert_eq!(diags[0].col, 30);
    assert!(
        diags[0].message.contains("unexpected character"),
        "{:?}",
        diags[0]
    );
}

/// A parse error is reported alone (the parser stops at the first problem);
/// nothing downstream runs.
#[test]
fn parse_error_suppresses_downstream() {
    let src = "fn f() -> Int64 { return true; }\nfn main() -> Int64 { let x = ; return x; }";
    let diags = diagnostics(src);
    assert_eq!(diags.len(), 1, "{:?}", diags);
    assert_eq!(diags[0].stage, "parse");
}

/// The historical `check()` shim renders exactly the first diagnostic, so
/// existing callers/tests see the same string they always did.
#[test]
fn check_shim_matches_first_rendered() {
    let src = "fn f() -> Int64 { return true; }\nfn g() -> Int64 { let y = \"s\" + 1; return y; }\nfn main() -> Int64 { return f(); }";
    let diags = diagnostics(src);
    let via_shim = vyrn_frontend::check(src).unwrap_err();
    // The shim returns the FIRST diagnostic rendered, which is f's error on line 1.
    assert_eq!(via_shim, diags[0].render());
    assert_eq!(
        via_shim,
        "line 1: return type mismatch: expected Int64, found Bool"
    );
}

/// RFC-0006's accumulation, for the two passes that still refuse here.
///
/// It read a use-after-consume until that rule left this pass (RFC-0125 §3 M3,
/// row 06), then a hand-over to a `consume` parameter until that one left too
/// (rows 13 and 14), then a store until rule 2 left (rows 01, 02, 03, 27 and
/// 34), then a `for .. in consume` of a `read` parameter until rows 10, 11 and
/// 29 left, then a closure's capture until row 24 left. `movecheck.rs` states
/// NO refusal now, and this crate cannot reach the kernel — the memory
/// judgment lives above it and is installed through a slot — so what
/// accumulates here is the checker's own, and the shapes the memory judgment
/// refuses are pinned where the kernel is reachable
/// (`vyrn-cli/tests/refusals.rs`, the accumulation tests below the census rows
/// and `the_shapes_the_last_three_rules_unit_tests_pinned_are_still_refused`).
#[test]
fn the_checker_accumulates_across_declarations() {
    let src = "fn bad() -> Int64 { return true; }
               fn worse() -> String { return 1; }
               fn main() -> Int64 { return 0; }";
    let diags = diagnostics(src);
    assert_eq!(diags.len(), 2, "{:?}", diags);
    assert!(diags.iter().all(|d| d.stage == "check"), "{:?}", diags);
    assert_eq!(diags[0].line, 1);
    assert_eq!(diags[1].line, 2);
}

/// Parser error recovery (RFC-0006): two bad top-level declarations are BOTH
/// reported in one pass. The first bad `fn` (a `let x = ;` missing its
/// initializer) no longer hides the later bad `type` (a `where` with no
/// predicate) — `program_accum` records the diagnostic, synchronizes to the
/// next top-level starter, and continues. Both are `parse`-stage; the clean
/// `helper` between them still parses (it just produces no diagnostic).
#[test]
fn parse_recovers_across_declarations() {
    let src = "fn main() -> Int64 { let x = ; return x; }\n\
               fn helper() -> Int64 { return 1; }\n\
               type Bad = Int64 where;";
    let diags = diagnostics(src);
    assert_eq!(diags.len(), 2, "{:?}", diags);
    assert!(diags.iter().all(|d| d.stage == "parse"), "{:?}", diags);
    // The bad `fn` is on line 1, the bad `type` on line 3.
    assert_eq!(diags[0].line, 1);
    assert_eq!(diags[1].line, 3);
}

/// A parse error suppresses downstream checks even under recovery: a source
/// with a parse error *and* an unrelated type error (`bad` returns Bool) reports
/// ONLY the parse error(s) — the partial program is not type-checked (running
/// the checker on a malformed program would only cascade). This preserves the
/// existing "parse error suppresses downstream" contract, now with recovery.
#[test]
fn parse_recovery_skips_downstream_checks() {
    let src = "fn main() -> Int64 { let x = ; return x; }\n\
               fn bad() -> Int64 { return true; }";
    let diags = diagnostics(src);
    assert!(diags.iter().all(|d| d.stage == "parse"), "{:?}", diags);
    assert_eq!(diags.len(), 1, "{:?}", diags);
    assert!(!diags.iter().any(|d| d.stage == "check"), "{:?}", diags);
}

/// Inside-body accumulation (RFC-0006): two *independent* type errors in ONE
/// function body are both reported — the per-statement push-and-continue in
/// `block` (each `let` is its own statement boundary) means the second bad `let`
/// is still checked after the first fails. Both are `check`-stage; lines follow
/// the source. The trailing `return a` is fine because a failed `let` binds the
/// name to `Type::Err`, which is assignable to the declared `Int` return.
#[test]
fn accumulates_within_function_body() {
    let src = "fn main() -> Int64 {\n  let a = \"s\" + 1;\n  let b = true + 2;\n  return a;\n}";
    let diags = diagnostics(src);
    assert_eq!(diags.len(), 2, "{:?}", diags);
    assert_eq!(diags[0].stage, "check");
    assert_eq!(diags[1].stage, "check");
    assert!(
        diags[0].message.contains("`+` concatenates two Strings"),
        "{:?}",
        diags[0]
    );
    assert!(
        diags[1]
            .message
            .contains("arithmetic needs matching numeric"),
        "{:?}",
        diags[1]
    );
    assert_eq!(diags[0].line, 2);
    assert_eq!(diags[1].line, 3);
}

/// Cascade-free recovery: a failed `let a = "s" + 1` binds `a` to `Type::Err`,
/// so the later `let b = a + 1` does NOT produce a second diagnostic — `binop_type`
/// short-circuits on an `Err` operand. Only the first, real error is reported.
#[test]
fn failed_let_does_not_cascade_through_binop() {
    let src = "fn main() -> Int64 {\n  let a = \"s\" + 1;\n  let b = a + 1;\n  return b;\n}";
    let diags = diagnostics(src);
    assert_eq!(diags.len(), 1, "{:?}", diags);
    assert!(
        diags[0].message.contains("`+` concatenates two Strings"),
        "{:?}",
        diags[0]
    );
    assert_eq!(diags[0].line, 2);
}

/// Cascade-free recovery through a built-in: a failed `let a` (Err-typed) flows
/// into `a.length` without a spurious "cannot access field" — the `Field`
/// `Type::Err` guard returns `Err`, so only the original error survives.
#[test]
fn failed_let_does_not_cascade_through_builtin() {
    let src = "fn main() -> Int64 {\n  let a = \"s\" + 1;\n  let n = a.length;\n  return 0;\n}";
    let diags = diagnostics(src);
    assert_eq!(diags.len(), 1, "{:?}", diags);
    assert!(
        diags[0].message.contains("`+` concatenates two Strings"),
        "{:?}",
        diags[0]
    );
    assert_eq!(diags[0].line, 2);
}
