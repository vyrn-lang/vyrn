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

/// A move-check error (use-after-consume) is reported with the right stage and
/// accumulates with a type error in another function.
#[test]
fn movecheck_accumulates_with_check() {
    let src = "type T = { id: Int64 };\n\
               fn take(t: consume T) -> Int64 { return t.id; }\n\
               fn bad() -> Int64 { return true; }\n\
               fn main() -> Int64 { let x = T { id: 1 }; let a = take(x); return take(x); }";
    let diags = diagnostics(src);
    // One check error (bad returns Bool) and one movecheck error (x used twice).
    let check_errs = diags.iter().filter(|d| d.stage == "check").count();
    let move_errs = diags.iter().filter(|d| d.stage == "movecheck").count();
    assert_eq!(check_errs, 1, "{:?}", diags);
    assert_eq!(move_errs, 1, "{:?}", diags);
    assert!(
        diags
            .iter()
            .any(|d| d.stage == "movecheck" && d.message.contains("already consumed")),
        "{diags:?}"
    );
}

/// Inside-body accumulation for movecheck (RFC-0006): two *independent*
/// use-after-consume bugs in ONE function body are both reported — `block`
/// push-and-continues at each statement, so the `y` bug is still found after the
/// `x` bug. Each is `movecheck`-stage; lines follow the source (the second
/// `take(x)` on line 5, the second `take(y)` on line 6).
#[test]
fn movecheck_accumulates_within_function_body() {
    let src = "type T = { id: Int64 };\n\
               fn take(t: consume T) -> Int64 { return t.id; }\n\
               fn main() -> Int64 {\n  let x = T { id: 1 }; let a = take(x); let b = take(x);\n  let y = T { id: 2 }; let c = take(y); let d = take(y);\n  return a + c;\n}";
    let diags = diagnostics(src);
    let move_errs: Vec<_> = diags.iter().filter(|d| d.stage == "movecheck").collect();
    assert_eq!(move_errs.len(), 2, "{:?}", diags);
    assert!(
        move_errs
            .iter()
            .all(|d| d.message.contains("already consumed")),
        "{:?}",
        move_errs
    );
    assert_eq!(move_errs[0].line, 4);
    assert_eq!(move_errs[1].line, 5);
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
