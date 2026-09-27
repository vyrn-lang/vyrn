//! Tests `vyrn_frontend::diagnostics`, the contract the CLI and the LSP share:
//! how many diagnostics, at what stage and position, and that `check()` renders
//! exactly the first.

use vyrn_frontend::diagnostics;

#[test]
fn valid_program_is_clean() {
    let src = "fn main() -> Int64 { let x = 2 + 3; print(x); return x; }";
    assert!(diagnostics(src).is_empty(), "{:?}", diagnostics(src));
}

#[test]
fn accumulates_across_functions() {
    let src = "fn f() -> Int64 { let a = None; return 0; }\nfn g() -> Int64 { let b = []; return 0; }\nfn main() -> Int64 { return f(); }";
    let diags = diagnostics(src);
    assert_eq!(diags.len(), 2, "{:?}", diags);
    assert_eq!(diags[0].stage, "check");
    assert_eq!(diags[1].stage, "check");
    assert!(diags[0].message.contains("`None`"), "{:?}", diags[0]);
    assert!(diags[1].message.contains("`[]`"), "{:?}", diags[1]);
    assert_eq!(diags[0].line, 1);
    assert_eq!(diags[1].line, 2);
}

/// The lexer stops at the first illegal token.
#[test]
fn lex_error_is_single_and_has_a_column() {
    let src = "fn main() -> Int64 { let x = @; return x; }";
    let diags = diagnostics(src);
    assert_eq!(diags.len(), 1, "{:?}", diags);
    assert_eq!(diags[0].stage, "lex");
    // 1-based.
    assert_eq!(diags[0].col, 30);
    assert!(
        diags[0].message.contains("unexpected character"),
        "{:?}",
        diags[0]
    );
}

#[test]
fn check_shim_matches_first_rendered() {
    let src = "fn f() -> Int64 { let a = None; return 0; }\nfn g() -> Int64 { let b = []; return 0; }\nfn main() -> Int64 { return f(); }";
    let diags = diagnostics(src);
    let via_shim = vyrn_frontend::check(src).unwrap_err();
    assert_eq!(via_shim, diags[0].render());
    assert_eq!(
        via_shim,
        "line 1: cannot infer the type of `None`; add an annotation (e.g. `let x: Option<Int64> = None;`)"
    );
}

/// `program_accum` records a parse diagnostic, synchronizes to the next
/// top-level starter, and continues.
#[test]
fn parse_recovers_across_declarations() {
    let src = "fn main() -> Int64 { let x = ; return x; }\n\
               fn helper() -> Int64 { return 1; }\n\
               type Bad = Int64 where;";
    let diags = diagnostics(src);
    assert_eq!(diags.len(), 2, "{:?}", diags);
    assert!(diags.iter().all(|d| d.stage == "parse"), "{:?}", diags);
    assert_eq!(diags[0].line, 1);
    assert_eq!(diags[1].line, 3);
}

/// Checking a malformed program would only cascade, so `bad`'s type error is
/// not reported.
#[test]
fn parse_recovery_skips_downstream_checks() {
    let src = "fn main() -> Int64 { let x = ; return x; }\n\
               fn bad() -> Int64 { return true; }";
    let diags = diagnostics(src);
    assert!(diags.iter().all(|d| d.stage == "parse"), "{:?}", diags);
    assert_eq!(diags.len(), 1, "{:?}", diags);
    assert!(!diags.iter().any(|d| d.stage == "check"), "{:?}", diags);
}

/// `block` pushes a diagnostic per statement and continues. The
/// `return a` is clean because a failed `let` binds `a` to `Type::Err`.
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

/// `binop_type` short-circuits on a `Type::Err` operand.
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

/// The `Field` guard returns `Type::Err` for an `Err` receiver, so `a.length`
/// adds no "cannot access field".
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
