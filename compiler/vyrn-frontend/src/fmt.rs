//! The canonical formatter: one style, no options.
//!
//! The printer reads [`crate::lexer::lex_with_trivia`] and decides only the whitespace
//! between raw token texts, so a literal is reproduced byte for byte. Safety
//! invariant: `lex(output)` equals `lex(input)` without `Semi` tokens, or [`fmt`]
//! returns an error and the caller leaves the file untouched.
//!
//! Line structure is the author's: the printer never joins or splits lines. It
//! sets indentation, intra-line spacing ([`wants_space`]), drops semicolons,
//! collapses blank-line runs to one and ends the file with one newline.

use crate::diagnostics::Diagnostic;
use crate::lexer::{self, Tok, Triv, TrivKind};

/// Formats `source` into its canonical form. The input must lex but need not
/// parse, so format-on-save works on a broken file. Returns a lex error verbatim.
pub fn fmt(source: &str) -> Result<String, Diagnostic> {
    let before = lexer::lex(source)?;
    let items = lexer::lex_with_trivia(source)?;
    let output = print(&items);

    // Safety invariant: lex(fmt(src)) == lex(src) without Semi tokens.
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

/// The token kinds of a stream without `Semi` and `Eof`: the safety invariant's
/// equivalence.
fn strip_semi(toks: &[lexer::Token]) -> Vec<&Tok> {
    toks.iter()
        .map(|t| &t.tok)
        .filter(|t| !matches!(t, Tok::Semi | Tok::Eof))
        .collect()
}

/// The role of each ambiguous operator, computed over tokens only.
#[derive(Clone, Copy, Default)]
struct Roles {
    /// A tight `<`/`>` that brackets generic arguments, not a comparison.
    generic_angle: bool,
    unary_minus: bool,
    /// The `*` of a contract's open rule `fn *(..)`, tight after: it lexes as multiply.
    open_rule_star: bool,
}

/// Whether `t` can end a type, so a tight `>`/`>>` after it can close a generic
/// list: a name, a generic close, a const-generic size, a Unit-return function
/// type's `)`, or a record type's `}`.
fn is_type_end(t: Option<&Tok>) -> bool {
    matches!(
        t,
        Some(Tok::Ident(_) | Tok::Gt | Tok::Shr | Tok::Int(_) | Tok::RParen | Tok::RBrace)
    )
}

/// Whether `t` can end an operand, so an operator after it is binary.
///
/// `}` ends a `match`, `if` or record-literal operand, but also a statement block,
/// after which a `-` would start a statement. The token stream cannot tell them
/// apart; the parser never reads spacing, so this picks the spelling the corpus
/// writes: `} - 1`.
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
            | Tok::RBrace
            | Tok::Question
    )
}

/// Computes the [`Roles`] of every item, indexed like `items`.
///
/// A generic bracket is tight against its neighbours (`Box<T>`), a comparison is
/// spaced (`a < b`). The source spacing decides; the formatter builds no AST.
fn compute_roles(items: &[Triv]) -> Vec<Roles> {
    let mut roles = vec![Roles::default(); items.len()];
    // Indices of the tokens, so `prev`/`next` skip comments.
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
    // Open generic `<` count. A tight `>` closes a generic only while one is open,
    // so `x>0` stays a comparison while `Int64>` closes.
    let mut generic_depth: i32 = 0;
    for k in 0..toks.len() {
        let idx = toks[k];
        let prev = if k > 0 { Some(kind(toks[k - 1])) } else { None };
        let next_idx = toks.get(k + 1).copied();
        let next = next_idx.map(kind);
        match kind(idx) {
            Tok::Lt => {
                // A generic `<` is tight on both sides, and its first argument starts a type:
                // a name, `fn` or `{`. No expression follows `<` with `fn` or `{`. A const size
                // is never the first argument, so `n<10` is a comparison.
                let tight_before = !items[idx].space_before;
                let tight_after = next_idx.map(|n| !items[n].space_before).unwrap_or(false);
                // `impl<T>` opens a generic after a keyword; no comparison follows `impl`.
                let prev_ok = matches!(prev, Some(Tok::Ident(_)) | Some(Tok::Gt) | Some(Tok::Impl));
                let next_ok = matches!(
                    next,
                    Some(Tok::Ident(_)) | Some(Tok::Fn) | Some(Tok::LBrace)
                );
                if tight_before && tight_after && prev_ok && next_ok {
                    roles[idx].generic_angle = true;
                    generic_depth += 1;
                }
            }
            Tok::Gt => {
                // A tight `>` after a type end closes a generic while one is open. The opener
                // and closer lists must agree, or the count stays raised and a later comparison
                // prints tight.
                let tight_before = !items[idx].space_before;
                let prev_ok = is_type_end(prev);
                if generic_depth > 0 && tight_before && prev_ok {
                    roles[idx].generic_angle = true;
                    generic_depth -= 1;
                }
            }
            // `>>` closes two generics when two are open (`Array<Array<T>>`); otherwise it
            // is a shift.
            Tok::Shr => {
                let tight_before = !items[idx].space_before;
                let prev_ok = is_type_end(prev);
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
            // The `*` of `fn *(..)` lexes as multiply; recognize it by position, where no
            // multiply can appear, so it does not print as `fn * (..)`.
            Tok::Star => {
                if matches!(prev, Some(Tok::Fn)) && matches!(next, Some(Tok::LParen)) {
                    roles[idx].open_rule_star = true;
                }
            }
            _ => {}
        }
    }
    roles
}

/// Whether one space goes between same-line tokens `prev` and `next`.
#[allow(clippy::too_many_arguments)]
fn wants_space(
    prev: &Tok,
    next: &Tok,
    prev_generic: bool,
    next_generic: bool,
    prev_unary_minus: bool,
    prev_open_rule_star: bool,
) -> bool {
    use Tok::*;
    if prev_open_rule_star {
        return false;
    }
    if matches!(prev, LParen | LBracket) {
        return false;
    }
    if matches!(next, RParen | RBracket) {
        return false;
    }
    if matches!(prev, Dot) || matches!(next, Dot) {
        return false;
    }
    if matches!(next, Comma | Semi | Colon | Question) {
        return false;
    }
    // Generic brackets are tight. The space after a generic `>` follows the rules
    // below, so `Box<T> =` never fuses to `>=`.
    if matches!(next, Lt | Gt | Shr) && next_generic {
        return false;
    }
    if matches!(prev, Lt) && prev_generic {
        return false;
    }
    // A call or index attaches: `foo(`, `arr[`, `x?(`, `id<T>(`, `fn(`.
    if matches!(next, LParen | LBracket)
        && (matches!(prev, Ident(_) | RParen | RBracket | Question | Fn)
            || (matches!(prev, Gt | Shr) && prev_generic))
    {
        return false;
    }
    if matches!(prev, LBrace) && matches!(next, RBrace) {
        return false;
    }
    if matches!(prev, Minus) && prev_unary_minus {
        return false;
    }
    if matches!(prev, Bang) {
        return false;
    }
    if matches!(prev, Tilde) {
        return false;
    }
    // Everything else gets one space.
    true
}

/// How deep the open brackets in `opens` indent a line, in 4-space units.
///
/// A level is a line that opened something still open, not a bracket, so in
/// `take(R {` and `print(match e {` the brace continues the call.
fn open_levels(opens: &[usize]) -> i32 {
    opens.windows(2).filter(|w| w[0] != w[1]).count() as i32 + i32::from(!opens.is_empty())
}

/// Indentation, in 4-space units, of a line whose first token is `first`. A
/// leading closer dedents; a leading `|` (enum variant) indents one more.
fn indent_level(opens: &[usize], first: Option<&Tok>) -> usize {
    let d = open_levels(opens);
    let d = match first {
        Some(Tok::RParen | Tok::RBracket | Tok::RBrace) => d - 1,
        Some(Tok::Pipe) => d + 1,
        _ => d,
    };
    d.max(0) as usize
}

/// Pushes an opener with its source line, or pops on a closer.
fn bump_depth(opens: &mut Vec<usize>, tok: &Tok, line: usize) {
    match tok {
        Tok::LParen | Tok::LBracket | Tok::LBrace => opens.push(line),
        Tok::RParen | Tok::RBracket | Tok::RBrace => {
            opens.pop();
        }
        _ => {}
    }
}

fn print(items: &[Triv]) -> String {
    let roles = compute_roles(items);
    let mut out = String::new();
    // The source line of each open bracket.
    let mut opens: Vec<usize> = Vec::new();
    // The last emitted token; comments and dropped semicolons do not update it.
    let mut prev_tok: Option<usize> = None;
    let mut prev_end_line: Option<usize> = None;

    for (idx, it) in items.iter().enumerate() {
        let tok = match &it.kind {
            TrivKind::Tok(t) => Some(t),
            _ => None,
        };

        // Drop a semicolon but advance the line cursor, so the next item's blank-line
        // count starts from the semicolon's line.
        if matches!(tok, Some(Tok::Semi)) {
            prev_end_line = Some(it.end_line);
            continue;
        }

        let is_doc = matches!(it.kind, TrivKind::Doc(_));
        let is_comment = matches!(it.kind, TrivKind::Comment);

        match prev_end_line {
            None => {
                let indent = if is_comment || is_doc {
                    open_levels(&opens).max(0) as usize
                } else {
                    indent_level(&opens, tok)
                };
                out.push_str(&"    ".repeat(indent));
                out.push_str(&it.text);
            }
            Some(prev_line) => {
                let gap = it.start_line.saturating_sub(prev_line);
                if gap == 0 {
                    if is_comment || is_doc {
                        // Trailing comment: one space before it.
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
                                // A string tight against an identifier is a tagged template
                                // (`sql".."`) and stays tight.
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
                                        roles[p].open_rule_star,
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
                    // Collapse blank-line runs to one, but keep one: a blank line after a `///`
                    // block detaches the doc, which hover and `schemaOf(T).doc` observe.
                    let newlines = gap.min(2);
                    for _ in 0..newlines {
                        out.push('\n');
                    }
                    let indent = if is_comment || is_doc {
                        open_levels(&opens).max(0) as usize
                    } else {
                        indent_level(&opens, tok)
                    };
                    out.push_str(&"    ".repeat(indent));
                    out.push_str(&it.text);
                }
            }
        }

        if let Some(t) = tok {
            bump_depth(&mut opens, t, it.start_line);
            prev_tok = Some(idx);
        }
        prev_end_line = Some(it.end_line);
    }

    // Newlines go only before an item, so there is no trailing blank line.
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
            "fn main() -> Int64 { let x = 1 + 2 return x }\n"
        });
    }

    #[test]
    fn formats_gen_fn_and_generator_imports() {
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
    fn lambdas_and_fn_types() {
        assert_eq!(
            f("fn g(f:fn(Int64)->Int64)->Int64{return f(1)}\n"),
            "fn g(f: fn(Int64) -> Int64) -> Int64 { return f(1) }\n"
        );
        // A lambda's arrow is spaced.
        assert_eq!(
            f("fn m()->Int64{let a=g(x ->x*2)  return 0}\n"),
            "fn m() -> Int64 { let a = g(x -> x * 2) return 0 }\n"
        );
        assert_eq!(
            f("fn m()->Int64{let a=z((x, y) ->x+y)  let b=n(||7)  return 0}\n"),
            "fn m() -> Int64 { let a = z((x, y) -> x + y) let b = n(|| 7) return 0 }\n"
        );
    }

    #[test]
    fn a_generic_argument_that_is_a_function_type() {
        // The argument starts with `fn` and holds `(`, `)` and `->`.
        assert_eq!(
            f("type Many<P, T> = { runs: Array<fn(P) -> T> }\n"),
            "type Many<P, T> = { runs: Array<fn(P) -> T> }\n"
        );
        assert_eq!(
            f("type Maybe<P, T> = { run: Option<fn(P) -> T> }\n"),
            "type Maybe<P, T> = { run: Option<fn(P) -> T> }\n"
        );
        assert_eq!(
            f("let a: Array<fn(Int64)> = x\n"),
            "let a: Array<fn(Int64)> = x\n"
        );
        assert_eq!(
            f("let m: Map<String, fn(Int64) -> Int64> = x\n"),
            "let m: Map<String, fn(Int64) -> Int64> = x\n"
        );
        assert_eq!(
            f("let a: Array<Array<fn(P) -> T>> = x\n"),
            "let a: Array<Array<fn(P) -> T>> = x\n"
        );
    }

    #[test]
    fn a_generic_argument_that_is_a_record_type() {
        // A record type starts with `{` and ends with `}`, neither a name.
        assert_eq!(
            f("let a: Array<{ x: Int64 }> = v\n"),
            "let a: Array<{ x: Int64 }> = v\n"
        );
    }

    #[test]
    fn an_unclosed_generic_never_tightens_a_later_comparison() {
        // An opener the closer does not match leaves the count raised and tightens
        // the next comparison.
        assert_eq!(
            f("type A = { r: Array<fn(P) -> T> }\nfn m() -> Int64 { if n>0 { return 1 } return 0 }\n"),
            "type A = { r: Array<fn(P) -> T> }\nfn m() -> Int64 { if n > 0 { return 1 } return 0 }\n"
        );
        assert_eq!(
            f("let a: Array<{ x: Int64 }> = v\nlet b = i>0\n"),
            "let a: Array<{ x: Int64 }> = v\nlet b = i > 0\n"
        );
    }

    #[test]
    fn tight_comparisons_re_space() {
        // A tight comparison re-spaces: `x>0` prints `x > 0`, never `x> 0`.
        assert_eq!(f("if x>0 { return a }\n"), "if x > 0 { return a }\n");
        assert_eq!(f("if n<10 { return a }\n"), "if n < 10 { return a }\n");
        assert_eq!(f("let b = i>=0\n"), "let b = i >= 0\n");
        assert_eq!(f("let b = a<=b\n"), "let b = a <= b\n");
        assert_eq!(f("let b = 3>0\n"), "let b = 3 > 0\n");
        assert_eq!(
            f("let m: Map<String, Int64> = x\nlet b = i>0\n"),
            "let m: Map<String, Int64> = x\nlet b = i > 0\n"
        );
    }

    #[test]
    fn bitwise_ops_are_spaced_and_tilde_hugs() {
        assert_eq!(f("let x=a&b\n"), "let x = a & b\n");
        assert_eq!(f("let x=a|b\n"), "let x = a | b\n");
        assert_eq!(f("let x=a^b\n"), "let x = a ^ b\n");
        assert_eq!(f("let x=a<<b\n"), "let x = a << b\n");
        assert_eq!(f("let x=a>>b\n"), "let x = a >> b\n");
        assert_eq!(f("let x = ~ a\n"), "let x = ~a\n");
        // `= ~a` keeps its space (not `=~`); `& ~a` hugs.
        assert_eq!(f("let x = b & ~a\n"), "let x = b & ~a\n");
        assert_eq!(f("return ~a & b\n"), "return ~a & b\n");
        assert_eq!(f("let b = x&mask==0\n"), "let b = x & mask == 0\n");
    }

    #[test]
    fn a_binary_operator_after_a_block_expression_stays_binary() {
        // `is_value_end` lists `}`, so `-` after a block stays binary like every
        // other operator. The re-lex invariant cannot see this: both spellings lex the
        // same.
        for op in ["-", "+", "*", "/", "%", "==", "<<", "&"] {
            let src = format!("let x = match n {{ 0 => 1, _ => 2, }} {op} 1\n");
            assert_eq!(f(&src), src, "operator `{op}` after a match block");
        }
        // The other two shapes a `}` can end: an `if` expression and a record literal.
        assert_eq!(
            f("let x = if c { 1 } else { 2 } - 1\n"),
            "let x = if c { 1 } else { 2 } - 1\n"
        );
        assert_eq!(
            f("let x = Box { value: 41 }.value - 1\n"),
            "let x = Box { value: 41 }.value - 1\n"
        );
        // A unary minus still hugs, including right after an arm's `=>`.
        assert_eq!(
            f("let x = match n { 0 => -1, _ => a - 1, }\n"),
            "let x = match n { 0 => -1, _ => a - 1, }\n"
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
    fn detaching_blank_after_doc_is_preserved() {
        // A blank line after a `///` detaches the doc; fmt keeps it, collapsed to one.
        assert_eq!(
            f("/// header\n\n\nfn main() -> Int64 { return 0 }\n"),
            "/// header\n\nfn main() -> Int64 { return 0 }\n"
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
    fn vyrn_code_quote_survives_formatting() {
        // A `vyrn"..."` code quote is a tagged template: the tag stays tight and the
        // raw text, holes included, is kept.
        assert_eq!(
            f("let c = vyrn\"fn f() { \\{x} }\"\n"),
            "let c = vyrn\"fn f() { \\{x} }\"\n"
        );
        // A `"""..."""` skeleton round-trips byte for byte; only indentation moves.
        let src = "fn g() -> Int64 {\nlet c = vyrn\"\"\"fn f() -> String {\n  return \"hi\"\n}\"\"\"\nreturn 0\n}\n";
        let out = f(src);
        assert!(out.contains("vyrn\"\"\"fn f() -> String {\n  return \"hi\"\n}\"\"\""));
        assert_eq!(f(&out), out);
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
    fn contract_declarations_round_trip() {
        // `fn *(..)`'s `*` lexes as a multiply and would otherwise print spaced.
        let src = "export contract Page {\n\
                   \x20   /// The page's data.\n\
                   \x20   let data: Query<T>\n\
                   \x20   let head: Head = Head {}\n\
                   \x20   fn load(id: Int64) -> String\n\
                   \x20   fn *(input: String) -> String\n\
                   }\n";
        assert_eq!(f(src), src);
    }

    #[test]
    fn a_messy_contract_formats_to_the_canonical_form() {
        // The printer never splits lines; spacing and the open rule's `*` normalize.
        assert_eq!(
            f("contract P{fn * ( a : String )->String}\n"),
            "contract P { fn *(a: String) -> String }\n"
        );
    }
}
