//! `vyrn_frontend::fmt` — the canonical formatter (RFC-0017).
//!
//! One style, no options. The printer works over the comment-preserving token
//! stream ([`crate::lexer::lex_with_trivia`]) and only ever decides the
//! whitespace *between* raw token texts — it never re-synthesizes a token's
//! spelling, so a string/char/number literal is reproduced byte-for-byte. That
//! is what makes the **safety invariant** cheap: after printing, we re-lex both
//! the input and the output with the normal [`crate::lexer::lex`] and require the
//! token sequences to be equal *modulo removed `Semi` tokens*. If they ever
//! differ (a formatter bug), `fmt` returns an error and the caller leaves the
//! file untouched — a formatter must never corrupt source.
//!
//! Line structure is the author's: v1 **never joins or splits lines**. It
//! normalizes indentation (4 spaces × brace/bracket depth), intra-line spacing
//! (the RFC's table), drops semicolons, collapses 2+ blank lines to one, and
//! trims trailing whitespace to a single trailing newline.

use crate::diagnostics::Diagnostic;
use crate::lexer::{self, Tok, Triv, TrivKind};

/// Format `source` into its canonical form. Requires *lexable* (not necessarily
/// parseable) input — a file with a parse error still formats, which matters for
/// format-on-save. On a lex error, returns that error verbatim.
pub fn fmt(source: &str) -> Result<String, Diagnostic> {
    // A lex error is the caller's error (fmt requires lexable input). Compute the
    // baseline token sequence up front so the safety check can compare against it.
    let before = lexer::lex(source)?;
    let items = lexer::lex_with_trivia(source)?;
    let output = print(&items);

    // Safety invariant (1): lex(fmt(src)) == lex(src) modulo removed Semi tokens.
    let after = lexer::lex(&output)?;
    if strip_semi(&before) != strip_semi(&after) {
        return Err(Diagnostic::error(
            0,
            0,
            "fmt",
            "internal formatter error: output would change the token sequence \
             (source left unchanged)"
                .to_string(),
        ));
    }
    Ok(output)
}

/// The token kinds of a stream with `Semi` and the trailing `Eof` removed — the
/// equivalence used by the safety invariant.
fn strip_semi(toks: &[lexer::Token]) -> Vec<&Tok> {
    toks.iter()
        .map(|t| &t.tok)
        .filter(|t| !matches!(t, Tok::Semi | Tok::Eof))
        .collect()
}

/// Per-token role for the ambiguous operators, precomputed over the token
/// subsequence (comments do not affect these decisions).
#[derive(Clone, Copy, Default)]
struct Roles {
    /// A `<`/`>` that is a generic-argument bracket (no surrounding spaces),
    /// rather than a comparison.
    generic_angle: bool,
    /// A `-` used as a unary prefix (`-x`) rather than a binary subtraction.
    unary_minus: bool,
    /// A `|` that opens a lambda parameter list (RFC-0023): tight *after* it
    /// (`|x`), so no space precedes the first parameter.
    lambda_open: bool,
    /// A `|` that closes a lambda parameter list (RFC-0023): tight *before* it
    /// (`x|`), with the normal one space *after* it before the body.
    lambda_close: bool,
}

/// A token can be the *end* of an operand — i.e. a binary operator that follows
/// it is genuinely binary, and a `(`/`[` that follows it is a call/index.
fn is_value_end(t: &Tok) -> bool {
    matches!(
        t,
        Tok::Ident(_)
            | Tok::Int(_)
            | Tok::Float(_)
            | Tok::Str(_)
            | Tok::TemplateStr { .. }
            | Tok::True
            | Tok::False
            | Tok::Vself
            | Tok::RParen
            | Tok::RBracket
            | Tok::Question
    )
}

/// Compute the [`Roles`] for every item (indexed to match `items`).
///
/// `<`/`>` disambiguation is source-driven: a generic bracket is *tight* against
/// its neighbours (`Box<T>`, `Array<Int64>`), whereas a comparison is spaced
/// (`a < b`, `i < 1`). This mirrors the entire corpus and needs no type
/// information (fmt does not build an AST).
fn compute_roles(items: &[Triv]) -> Vec<Roles> {
    let mut roles = vec![Roles::default(); items.len()];
    // Indices of the real tokens (skip comments), so `prev`/`next` mean the
    // previous/next *token*.
    let toks: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, t)| matches!(t.kind, TrivKind::Tok(_)))
        .map(|(i, _)| i)
        .collect();
    let kind = |idx: usize| -> &Tok {
        match &items[idx].kind {
            TrivKind::Tok(t) => t,
            _ => unreachable!("toks holds only Tok items"),
        }
    };
    let mut in_lambda_params = false;
    // Whether we are lexically inside a `type` declaration's right-hand side —
    // its `|`s separate enum variants, never open a lambda (RFC-0037 lambda
    // positions are expression contexts, which a type RHS is not).
    let mut in_type_decl = false;
    // How many generic `<` are currently open (balanced by their `>` closes). A
    // closing `>` is a generic bracket ONLY while one is open — otherwise a lone
    // tight `>` (`x>0`, prev = operand) is a comparison, not a stray generic
    // close. Source-tightness alone cannot tell `x>0` from `Int64>`; the open-`<`
    // context does, and needs no type information.
    let mut generic_depth: i32 = 0;
    for k in 0..toks.len() {
        let idx = toks[k];
        let prev = if k > 0 { Some(kind(toks[k - 1])) } else { None };
        let next_idx = toks.get(k + 1).copied();
        let next = next_idx.map(kind);
        match kind(idx) {
            Tok::Type => in_type_decl = true,
            Tok::Fn | Tok::Let | Tok::Return | Tok::Import | Tok::Export => in_type_decl = false,
            _ => {}
        }
        match kind(idx) {
            Tok::Lt => {
                // A generic bracket is tight on both sides in the source; a
                // comparison is spaced. The first argument after `<` is always a
                // type name (`Box<T>`, `Array<Int64, 3>` — the const-generic size
                // is a *later* argument, never right after `<`), so a `<` directly
                // before an integer literal (`n<10`) is a comparison, not a generic.
                let tight_before = !items[idx].space_before;
                let tight_after = next_idx.map(|n| !items[n].space_before).unwrap_or(false);
                let prev_ok = matches!(prev, Some(Tok::Ident(_)) | Some(Tok::Gt));
                let next_ok = matches!(next, Some(Tok::Ident(_)));
                if tight_before && tight_after && prev_ok && next_ok {
                    roles[idx].generic_angle = true;
                    generic_depth += 1;
                }
            }
            Tok::Gt => {
                // The closing `>` is tight against a type name (`Int64>`), a nested
                // close (`>>` = a `Shr` then a `>`), or a const-generic integer size
                // (`Array<Int64, 3>`). It is generic ONLY while a generic `<` is
                // open — a lone tight `>` whose left side is an operand (`x>0`) is a
                // comparison.
                let tight_before = !items[idx].space_before;
                let prev_ok = matches!(
                    prev,
                    Some(Tok::Ident(_)) | Some(Tok::Gt) | Some(Tok::Shr) | Some(Tok::Int(_))
                );
                if generic_depth > 0 && tight_before && prev_ok {
                    roles[idx].generic_angle = true;
                    generic_depth -= 1;
                }
            }
            // A `>>` (RFC-0045 shift token) closes TWO nested generics when a
            // generic is open (`Array<Array<T>>`); otherwise it is a shift
            // operator, spaced like any binary op. Source-tightness plus the
            // open-`<` depth disambiguate, exactly as for a lone `>`.
            Tok::Shr => {
                let tight_before = !items[idx].space_before;
                let prev_ok = matches!(
                    prev,
                    Some(Tok::Ident(_)) | Some(Tok::Gt) | Some(Tok::Shr) | Some(Tok::Int(_))
                );
                if generic_depth >= 2 && tight_before && prev_ok {
                    roles[idx].generic_angle = true;
                    generic_depth -= 2;
                }
            }
            Tok::Minus => {
                if !prev.map(is_value_end).unwrap_or(false) {
                    roles[idx].unary_minus = true;
                }
            }
            // A `|` opening or closing a lambda parameter list. A lambda is a
            // call argument (RFC-0023: after `(`/`,`) or a storage-position
            // source (RFC-0037: after `=`, a record/map `:`, a match arm's
            // `=>`, `return`, or an opening `[`/`{`). Enum-variant `|` also
            // follows `=`, but only on a `type` declaration's RHS — the
            // `in_type_decl` guard keeps those unmarked (spaced).
            Tok::Pipe => {
                if in_lambda_params {
                    roles[idx].lambda_close = true;
                    in_lambda_params = false;
                } else if matches!(prev, Some(Tok::LParen) | Some(Tok::Comma))
                    || (!in_type_decl
                        && matches!(
                            prev,
                            Some(Tok::Eq)
                                | Some(Tok::Colon)
                                | Some(Tok::FatArrow)
                                | Some(Tok::Return)
                                | Some(Tok::LBracket)
                                | Some(Tok::LBrace)
                        ))
                {
                    roles[idx].lambda_open = true;
                    in_lambda_params = true;
                }
            }
            _ => {}
        }
    }
    roles
}

/// Whether a single space belongs between adjacent same-line tokens `prev`→`next`
/// (the RFC's spacing table). `*_generic` mark `<`/`>` as generic brackets;
/// `prev_unary_minus` marks a `-` as a unary prefix.
#[allow(clippy::too_many_arguments)]
fn wants_space(
    prev: &Tok,
    next: &Tok,
    prev_generic: bool,
    next_generic: bool,
    prev_unary_minus: bool,
    prev_lambda_open: bool,
    next_lambda_close: bool,
) -> bool {
    use Tok::*;
    // A lambda parameter list is tight inside (RFC-0023): no space after the
    // opening `|` and no space before the closing `|` (`|x|`, `|x, y|`). The one
    // space AFTER the closing `|` (before the body) follows the normal rules.
    if prev_lambda_open || next_lambda_close {
        return false;
    }
    // No space just inside `(`/`[`.
    if matches!(prev, LParen | LBracket) {
        return false;
    }
    if matches!(next, RParen | RBracket) {
        return false;
    }
    // Method chains / field access: no space around `.`.
    if matches!(prev, Dot) || matches!(next, Dot) {
        return false;
    }
    // No space before `,` `;` `:` or a postfix `?`.
    if matches!(next, Comma | Semi | Colon | Question) {
        return false;
    }
    // Generic angle brackets are tight: no space *before* a generic `<`/`>` (or a
    // generic-closing `>>`), and no space *after* a generic `<`. (Space *after* a
    // generic `>`/`>>` follows the normal rules below — so `Box<T> =` keeps its
    // space and never fuses to `>=`.)
    if matches!(next, Lt | Gt | Shr) && next_generic {
        return false;
    }
    if matches!(prev, Lt) && prev_generic {
        return false;
    }
    // Call / index: `foo(`, `arr[`, `f()[`, `x?(` attach with no space. A generic
    // close `>` (or `>>`) also attaches (`fn id<T>(x)`, `foo<Int64>()`,
    // `f<Array<T>>()`). A function-value type's `fn(` (RFC-0023) attaches too.
    if matches!(next, LParen | LBracket)
        && (matches!(prev, Ident(_) | RParen | RBracket | Question | Fn)
            || (matches!(prev, Gt | Shr) && prev_generic))
    {
        return false;
    }
    // Empty braces print as `{}`.
    if matches!(prev, LBrace) && matches!(next, RBrace) {
        return false;
    }
    // Prefix operators bind tight to their operand.
    if matches!(prev, Minus) && prev_unary_minus {
        return false;
    }
    if matches!(prev, Bang) {
        return false;
    }
    // `~` (bitwise complement, RFC-0045) hugs its operand like unary `-`/`!`.
    if matches!(prev, Tilde) {
        return false;
    }
    // Everything else — words, binary operators, `=`/`->`/`=>`, one space before
    // `{`, one space after `,`/`:`, spaces inside record braces — gets one space.
    true
}

/// Indentation level (in 4-space units) for a line whose first token is `first`
/// at running bracket `depth`. A line starting with a closer dedents first; a
/// line starting with a leading `|` (enum-variant style) indents one extra.
fn indent_level(depth: i32, first: Option<&Tok>) -> usize {
    let d = match first {
        Some(Tok::RParen | Tok::RBracket | Tok::RBrace) => depth - 1,
        Some(Tok::Pipe) => depth + 1,
        _ => depth,
    };
    d.max(0) as usize
}

/// Update the running bracket depth as a token is emitted.
fn bump_depth(depth: &mut i32, tok: &Tok) {
    match tok {
        Tok::LParen | Tok::LBracket | Tok::LBrace => *depth += 1,
        Tok::RParen | Tok::RBracket | Tok::RBrace => *depth -= 1,
        _ => {}
    }
}

/// Print the item stream to canonical text.
fn print(items: &[Triv]) -> String {
    let roles = compute_roles(items);
    let mut out = String::new();
    let mut depth: i32 = 0;
    // The previous *token* item (index into `items`) actually emitted, for
    // spacing and role lookups. Comments/dropped semicolons don't update it.
    let mut prev_tok: Option<usize> = None;
    let mut prev_end_line: Option<usize> = None;

    for (idx, it) in items.iter().enumerate() {
        let tok = match &it.kind {
            TrivKind::Tok(t) => Some(t),
            _ => None,
        };

        // Semicolons are dropped: emit nothing, but advance the line cursor so the
        // following token's blank-line math is measured from the semicolon's line
        // (its spacing continues as if the `;` were absent).
        if matches!(tok, Some(Tok::Semi)) {
            prev_end_line = Some(it.end_line);
            continue;
        }

        let is_doc = matches!(it.kind, TrivKind::Doc);
        let is_comment = matches!(it.kind, TrivKind::Comment);

        match prev_end_line {
            // First emitted item: no leading newlines, just its line's indent.
            None => {
                let indent = if is_comment || is_doc {
                    depth.max(0) as usize
                } else {
                    indent_level(depth, tok)
                };
                out.push_str(&"    ".repeat(indent));
                out.push_str(&it.text);
            }
            Some(prev_line) => {
                let gap = it.start_line.saturating_sub(prev_line);
                if gap == 0 {
                    // Same source line as the previous item.
                    if is_comment || is_doc {
                        // Trailing comment: exactly one space before it.
                        out.push(' ');
                        out.push_str(&it.text);
                    } else {
                        let t = tok.expect("non-comment item is a token");
                        let sp = match prev_tok {
                            Some(p) => {
                                let pt = match &items[p].kind {
                                    TrivKind::Tok(x) => x,
                                    _ => unreachable!(),
                                };
                                // Tagged template (`sql"..\{}.."`, RFC-0007): a
                                // string written tight against the tag identifier
                                // stays tight. The parser keys on same-line
                                // adjacency, so this is cosmetic — but tight is the
                                // canonical form. `return "x"` / `from "path"` have a
                                // source space, so they are unaffected.
                                if matches!(t, Tok::Str(_) | Tok::TemplateStr { .. })
                                    && !it.space_before
                                    && matches!(pt, Tok::Ident(_))
                                {
                                    false
                                } else {
                                    wants_space(
                                        pt,
                                        t,
                                        roles[p].generic_angle,
                                        roles[idx].generic_angle,
                                        roles[p].unary_minus,
                                        roles[p].lambda_open,
                                        roles[idx].lambda_close,
                                    )
                                }
                            }
                            None => false,
                        };
                        if sp {
                            out.push(' ');
                        }
                        out.push_str(&it.text);
                    }
                } else {
                    // New line(s): collapse 2+ blank lines to one. A blank line
                    // between a `///` block and a declaration is NOT removed — the
                    // parser treats it as *detaching* the doc (a file-header block
                    // belongs to the file, not the next decl), which is observable
                    // via hover and `schemaOf(T).doc`, so removing it would change
                    // meaning. Preserving line structure keeps attachment intact.
                    let newlines = gap.min(2);
                    for _ in 0..newlines {
                        out.push('\n');
                    }
                    let indent = if is_comment || is_doc {
                        depth.max(0) as usize
                    } else {
                        indent_level(depth, tok)
                    };
                    out.push_str(&"    ".repeat(indent));
                    out.push_str(&it.text);
                }
            }
        }

        // Bookkeeping: depth follows real tokens; `prev_tok` skips comments.
        if let Some(t) = tok {
            bump_depth(&mut depth, t);
            prev_tok = Some(idx);
        }
        prev_end_line = Some(it.end_line);
    }

    // Exactly one trailing newline (and never a trailing blank line — newlines are
    // only ever emitted *before* an item, so the last line has no trailing space).
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(s: &str) -> String {
        fmt(s).expect("should format")
    }

    #[test]
    fn drops_semicolons_and_normalizes_spacing() {
        assert_eq!(f("fn main()->Int64{let x=1+2;return x;}"), {
            // No reflow: everything stays on one line, but spacing is canonical
            // and the semicolons are gone.
            "fn main() -> Int64 { let x = 1 + 2 return x }\n"
        });
    }

    #[test]
    fn formats_gen_fn_and_generator_imports() {
        // RFC-0021: `gen fn` and `import { .. } from gen(args)` are token-based
        // constructs the formatter renders canonically.
        assert_eq!(
            f("gen  fn   make(dir:String)->String{return \"x\"}\n"),
            "gen fn make(dir: String) -> String { return \"x\" }\n"
        );
        assert_eq!(
            f("import { t } from i18n(\"./x\")\n"),
            "import { t } from i18n(\"./x\")\n"
        );
    }

    #[test]
    fn formats_namespace_imports() {
        // RFC-0027: `import * as ns from ..` — a header line like any other import.
        assert_eq!(
            f("import   *   as   api   from   \"./api\"\n"),
            "import * as api from \"./api\"\n"
        );
        assert_eq!(
            f("import * as ui from pages(\"./pages\")\n"),
            "import * as ui from pages(\"./pages\")\n"
        );
    }

    #[test]
    fn indents_by_brace_depth() {
        let src = "fn main() -> Int64 {\nlet x = 1\nreturn x\n}\n";
        assert_eq!(
            f(src),
            "fn main() -> Int64 {\n    let x = 1\n    return x\n}\n"
        );
    }

    #[test]
    fn collapses_blank_lines_and_trims_trailing() {
        let src = "fn a() -> Int64 { return 1 }\n\n\n\nfn b() -> Int64 { return 2 }\n";
        assert_eq!(
            f(src),
            "fn a() -> Int64 { return 1 }\n\nfn b() -> Int64 { return 2 }\n"
        );
    }

    #[test]
    fn lambdas_and_fn_types(/* RFC-0023 */) {
        // `fn(T) -> R` type: `fn(` tight, arrow spaced.
        assert_eq!(
            f("fn g(f:fn(Int64)->Int64)->Int64{return f(1)}\n"),
            "fn g(f: fn(Int64) -> Int64) -> Int64 { return f(1) }\n"
        );
        // Lambda pipes tight inside, one space after the closing pipe.
        assert_eq!(
            f("fn m()->Int64{let a=g(|x|x*2)  return 0}\n"),
            "fn m() -> Int64 { let a = g(|x| x * 2) return 0 }\n"
        );
        // Multi-parameter and zero-parameter forms.
        assert_eq!(
            f("fn m()->Int64{let a=z(|x,y|x+y)  let b=n(||7)  return 0}\n"),
            "fn m() -> Int64 { let a = z(|x, y| x + y) let b = n(|| 7) return 0 }\n"
        );
    }

    #[test]
    fn generic_angles_have_no_spaces() {
        assert_eq!(
            f("type Box<T> = { value: T }\n"),
            "type Box<T> = { value: T }\n"
        );
        assert_eq!(
            f("let p: Pair<Int64, String> = x\n"),
            "let p: Pair<Int64, String> = x\n"
        );
        assert_eq!(
            f("fn id<T>(x: T) -> T { return x }\n"),
            "fn id<T>(x: T) -> T { return x }\n"
        );
        // Nested generics fuse `>>` without a comparison being inferred.
        assert_eq!(
            f("let a: Array<Array<Int64>> = x\n"),
            "let a: Array<Array<Int64>> = x\n"
        );
    }

    #[test]
    fn comparisons_keep_spaces() {
        assert_eq!(f("let b = a < c\n"), "let b = a < c\n");
        assert_eq!(f("if i < 1 { return a }\n"), "if i < 1 { return a }\n");
        assert_eq!(f("let b = a > c\n"), "let b = a > c\n");
    }

    #[test]
    fn tight_comparisons_re_space() {
        // A tight comparison (no matching generic `<`) must re-space, not be
        // mistaken for a stray generic close (`x>0` -> `x > 0`, never `x> 0`).
        assert_eq!(f("if x>0 { return a }\n"), "if x > 0 { return a }\n");
        assert_eq!(f("if n<10 { return a }\n"), "if n < 10 { return a }\n");
        assert_eq!(f("let b = i>=0\n"), "let b = i >= 0\n");
        assert_eq!(f("let b = a<=b\n"), "let b = a <= b\n");
        // A comparison against a literal on both sides.
        assert_eq!(f("let b = 3>0\n"), "let b = 3 > 0\n");
        // A generic still fuses when a real `<` opened, even next to a comparison.
        assert_eq!(
            f("let m: Map<String, Int64> = x\nlet b = i>0\n"),
            "let m: Map<String, Int64> = x\nlet b = i > 0\n"
        );
    }

    #[test]
    fn bitwise_ops_are_spaced_and_tilde_hugs(/* RFC-0045 */) {
        // Binary bitwise ops are spaced like other binaries; `~` hugs its operand.
        assert_eq!(f("let x=a&b\n"), "let x = a & b\n");
        assert_eq!(f("let x=a|b\n"), "let x = a | b\n");
        assert_eq!(f("let x=a^b\n"), "let x = a ^ b\n");
        assert_eq!(f("let x=a<<b\n"), "let x = a << b\n");
        assert_eq!(f("let x=a>>b\n"), "let x = a >> b\n");
        assert_eq!(f("let x = ~ a\n"), "let x = ~a\n");
        // `~` hugs even after another binary op (`= ~a` needs the space so it is
        // not the `=~` regex-match token, but `& ~a` is unambiguous).
        assert_eq!(f("let x = b & ~a\n"), "let x = b & ~a\n");
        assert_eq!(f("return ~a & b\n"), "return ~a & b\n");
        // A masked-flags comparison keeps the RFC precedence readable.
        assert_eq!(f("let b = x&mask==0\n"), "let b = x & mask == 0\n");
    }

    #[test]
    fn shift_and_generic_close_coexist(/* RFC-0045 */) {
        // A shift `>>` and a generic-closing `>>` in the SAME program: the shift
        // stays spaced, the generic close stays tight.
        assert_eq!(
            f("fn f(a: Array<Array<Int64>>) -> Int64 { return n >> 2 }\n"),
            "fn f(a: Array<Array<Int64>>) -> Int64 { return n >> 2 }\n"
        );
        // Triple-nested generic close (`>>>` = `Shr` then `>`).
        assert_eq!(
            f("let a: Array<Array<Array<Int64>>> = x\n"),
            "let a: Array<Array<Array<Int64>>> = x\n"
        );
    }

    #[test]
    fn unary_vs_binary_minus() {
        assert_eq!(f("let x = -1\n"), "let x = -1\n");
        assert_eq!(f("let x = a - 1\n"), "let x = a - 1\n");
        assert_eq!(f("let x = (-1)\n"), "let x = (-1)\n");
        assert_eq!(
            f("return match s { Err(e) => -1, }\n"),
            "return match s { Err(e) => -1, }\n"
        );
    }

    #[test]
    fn leading_pipe_enum_indents() {
        let src = "type Shape =\n| Circle(Int64)\n| Rect(Int64, Int64)\n| Unit\n";
        assert_eq!(
            f(src),
            "type Shape =\n    | Circle(Int64)\n    | Rect(Int64, Int64)\n    | Unit\n"
        );
    }

    #[test]
    fn trailing_comment_one_space_ownline_indented() {
        let src = "fn main() -> Int64 {\nlet x = 1      // note\n// own line\nreturn x\n}\n";
        assert_eq!(
            f(src),
            "fn main() -> Int64 {\n    let x = 1 // note\n    // own line\n    return x\n}\n"
        );
    }

    #[test]
    fn attached_doc_stays_attached() {
        // A doc directly above its decl stays adjacent (canonical, and the
        // parser attaches it).
        let src = "/// doc\nfn main() -> Int64 { return 0 }\n";
        assert_eq!(f(src), "/// doc\nfn main() -> Int64 { return 0 }\n");
    }

    #[test]
    fn detaching_blank_after_doc_is_preserved() {
        // A blank line between a `///` header and a decl DETACHES the doc in the
        // parser (observable via hover / schemaOf). fmt must NOT remove it, or it
        // would silently change meaning. The blank is kept (collapsed to one).
        assert_eq!(
            f("/// header\n\n\nfn main() -> Int64 { return 0 }\n"),
            "/// header\n\nfn main() -> Int64 { return 0 }\n"
        );
    }

    #[test]
    fn string_literals_reproduced_exactly() {
        // Interpolation holes and escapes are not reformatted.
        let src = "fn f() -> String { return \"n=\\{n + n}, x\\t\" }\n";
        assert_eq!(
            f(src),
            "fn f() -> String { return \"n=\\{n + n}, x\\t\" }\n"
        );
    }

    #[test]
    fn multiline_string_internal_newlines_preserved() {
        let src = "fn f() -> Int64 {\nlet b = \"a\nc\"\nreturn 0\n}\n";
        assert_eq!(
            f(src),
            "fn f() -> Int64 {\n    let b = \"a\nc\"\n    return 0\n}\n"
        );
    }

    #[test]
    fn tagged_template_stays_tight() {
        assert_eq!(
            f("let q = sql\"a \\{x} b\"\n"),
            "let q = sql\"a \\{x} b\"\n"
        );
        // A normal keyword/ident with a source space before a string keeps it.
        assert_eq!(f("return \"hi \\{n}\"\n"), "return \"hi \\{n}\"\n");
        assert_eq!(f("import { x } from \"p\"\n"), "import { x } from \"p\"\n");
    }

    #[test]
    fn vyrn_code_quote_survives_formatting() {
        // A `vyrn"…"` code quote (RFC-0054) is an ordinary tagged-template token to
        // fmt — the tag stays tight against the string and the raw text (incl. its
        // `\{…}` holes) is preserved verbatim, so the safety invariant holds.
        assert_eq!(
            f("let c = vyrn\"fn f() { \\{x} }\"\n"),
            "let c = vyrn\"fn f() { \\{x} }\"\n"
        );
        // A `"""…"""` triple-quoted skeleton (inner `\"` and newlines) round-trips
        // byte-for-byte; only leading indentation is normalized.
        let src = "fn g() -> Int64 {\nlet c = vyrn\"\"\"fn f() -> String {\n  return \"hi\"\n}\"\"\"\nreturn 0\n}\n";
        let out = f(src);
        assert!(out.contains("vyrn\"\"\"fn f() -> String {\n  return \"hi\"\n}\"\"\""));
        // Idempotent: re-formatting the output is a no-op (re-lex equality).
        assert_eq!(f(&out), out);
    }

    #[test]
    fn sized_array_const_generic() {
        assert_eq!(
            f("let fixed: Array<Int64, 5> = x\n"),
            "let fixed: Array<Int64, 5> = x\n"
        );
        assert_eq!(
            f("fn w() -> Array<String, 3> { return x }\n"),
            "fn w() -> Array<String, 3> { return x }\n"
        );
    }

    #[test]
    fn record_braces_get_inner_spaces() {
        assert_eq!(f("let n = Box{value: 41}\n"), "let n = Box { value: 41 }\n");
    }

    #[test]
    fn method_chains_and_calls_tight() {
        assert_eq!(f("print( t . values [ i ] )\n"), "print(t.values[i])\n");
    }

    #[test]
    fn idempotent_on_messy_input() {
        let src = "fn  main( )->Int64{let  x=1+2*3;return x}\n";
        let once = f(src);
        let twice = f(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn parse_error_still_formats() {
        // Not parseable (dangling `let`), but lexable — must still format.
        let src = "fn main() -> Int64 {\nlet x =\nreturn 0\n}\n";
        let out = fmt(src).expect("lexable input formats even if unparseable");
        assert!(out.contains("let x ="));
    }
}
