//! The one classifier of Vyrn source: the coloured runs of a text, for the
//! playground (`vyrn-play`) and the site (the `codeRuns` builtin).
//!
//! [`code_runs`] lexes the text and runs the recognizer over its tokens. The token
//! kind decides a run's class, and the recognizer's [`Mark::Word`]s say which
//! identifiers a rule consumed as a word. A file that does not parse still has
//! runs: recovery resumes at the next declaration or statement, and a token in the
//! text it skips gets its kind's class and no word's.

use std::ops::Range;

use crate::diagnostics::Diagnostic;
use crate::lexer::{self, Hole, Tok, Token, TrivKind};
use crate::recognizer::{recognize, recognize_hole, Mark, Recognized};

/// What colours a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Keyword,
    Str,
    Number,
    Comment,
    Type,
    Function,
    /// The `\{` and the `}` around an interpolation hole.
    HoleMark,
}

impl Class {
    /// Returns the stylesheet's class name.
    pub fn css(self) -> &'static str {
        match self {
            Class::Keyword => "k",
            Class::Str => "s",
            Class::Number => "n",
            Class::Comment => "c",
            Class::Type => "t",
            Class::Function => "f",
            Class::HoleMark => "i",
        }
    }
}

/// A coloured byte range of the source. Text in no run has no colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub start: usize,
    pub stop: usize,
    pub class: Class,
}

/// Returns the runs of `src`, holes of interpolated strings included, in source
/// order and apart.
///
/// # Errors
///
/// Returns the lexer's refusal when `src`, or a hole in it, does not lex.
pub fn code_runs(src: &str) -> Result<Vec<Run>, Diagnostic> {
    let mut out = Vec::new();
    runs_in(src, 0, recognize, &mut out)?;
    Ok(out)
}

/// Pushes the runs of `src`, which starts `base` bytes into the text, with `rule`
/// as what `src` is. A hole's source is shorter than the token that holds it, so
/// the recursion ends.
fn runs_in(
    src: &str,
    base: usize,
    rule: fn(&[Token]) -> Recognized,
    out: &mut Vec<Run>,
) -> Result<(), Diagnostic> {
    let scan = lexer::scan(src)?;
    let items: Vec<(Range<usize>, bool)> = (scan.items.iter())
        .map(|i| (i.span.clone(), matches!(i.kind, TrivKind::Comment)))
        .collect();
    let toks = lexer::tokens(scan);
    // A rule consumes tokens in order, so the marks are in order.
    let mut words = (rule(&toks).marks.into_iter())
        .filter_map(|m| match m {
            Mark::Word(at) => Some(at),
            Mark::Decl(..) => None,
        })
        .peekable();
    let push = |start: usize, stop: usize, class: Class, out: &mut Vec<Run>| {
        out.push(Run {
            start: base + start,
            stop: base + stop,
            class,
        });
    };
    let mut at = 0;
    for (span, comment) in items {
        if comment {
            push(span.start, span.end, Class::Comment, out);
            continue;
        }
        let word = words.next_if_eq(&at).is_some();
        if let Tok::TemplateStr { exprs, .. } = &toks[at].tok {
            let mut from = span.start;
            for hole in exprs {
                let start = hole_start(src, span.start, &toks[at], hole);
                let stop = start + hole.src.len();
                if from < start - 2 {
                    push(from, start - 2, Class::Str, out);
                }
                push(start - 2, start, Class::HoleMark, out);
                runs_in(&hole.src, base + start, recognize_hole, out)?;
                push(stop, stop + 1, Class::HoleMark, out);
                from = stop + 1;
            }
            push(from, span.end, Class::Str, out);
        } else if let Some(class) = class_of(&toks, at, word) {
            push(span.start, span.end, class, out);
        }
        at += 1;
    }
    Ok(())
}

/// The class of token `at` of `toks`, which ends in `Eof`. `word` says a rule
/// consumed it as a word. A capital name is a type, whether a rule spells it, as
/// `Array`, or not.
fn class_of(toks: &[Token], at: usize, word: bool) -> Option<Class> {
    Some(match &toks[at].tok {
        Tok::Str(_) | Tok::TemplateStr { .. } => Class::Str,
        Tok::Int(_) | Tok::Byte(_) | Tok::Float(_) => Class::Number,
        Tok::Doc(_) => Class::Comment,
        Tok::Ident(name) if name.starts_with(char::is_uppercase) => Class::Type,
        Tok::Ident(_) if word => Class::Keyword,
        Tok::Ident(_) => {
            let called = matches!(toks[at + 1].tok, Tok::LParen);
            if !called && (at == 0 || !matches!(toks[at - 1].tok, Tok::Fn)) {
                return None;
            }
            Class::Function
        }
        tok => {
            tok.keyword_text()?;
            Class::Keyword
        }
    })
}

/// The byte of `src` where `hole` starts. The string `tok` starts at byte `start`
/// and holds the hole. A hole's line and column count from `src`, and a column
/// counts characters.
fn hole_start(src: &str, start: usize, tok: &Token, hole: &Hole) -> usize {
    let text = &src[start..];
    let (line, chars) = match hole.line - tok.line {
        0 => (0, hole.col - tok.col),
        n => {
            let newline = text.match_indices('\n').nth(n - 1);
            (newline.map_or(text.len(), |(i, _)| i + 1), hole.col - 1)
        }
    };
    let bytes: usize = text[line..].chars().take(chars).map(char::len_utf8).sum();
    start + line + bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use Class::*;

    /// Each run as its text and class.
    fn runs(src: &str) -> Vec<(&str, Class)> {
        (code_runs(src).expect("the source lexes").into_iter())
            .map(|r| (&src[r.start..r.stop], r.class))
            .collect()
    }

    /// The class of the first run that spells `text`.
    fn class(src: &str, text: &str) -> Option<Class> {
        runs(src).into_iter().find(|r| r.0 == text).map(|r| r.1)
    }

    #[test]
    fn a_number_ends_where_the_lexer_ends_it() {
        assert_eq!(
            runs("let a = 1.0.toString()\n")[1..],
            [("1.0", Number), ("toString", Function)]
        );
        assert_eq!(runs("let a = 'A'\n")[1], ("'A'", Number));
    }

    #[test]
    fn a_string_run_is_its_quotes_not_its_decoded_text() {
        assert_eq!(runs(r#"let a = "a//b""#)[1], (r#""a//b""#, Str));
        assert_eq!(runs(r#"let a = "a\"b""#)[1], (r#""a\"b""#, Str));
    }

    #[test]
    fn a_triple_quoted_string_holds_a_quote() {
        let src = "let a = \"\"\"x \"y\" z\"\"\" + b\n";
        assert_eq!(runs(src)[1], ("\"\"\"x \"y\" z\"\"\"", Str));
    }

    #[test]
    fn a_hole_is_coloured_as_code_inside_the_string() {
        assert_eq!(
            runs(r#"let a = "a\{f("x")}b""#)[1..],
            [
                (r#""a"#, Str),
                (r"\{", HoleMark),
                ("f", Function),
                (r#""x""#, Str),
                ("}", HoleMark),
                (r#"b""#, Str),
            ]
        );
    }

    #[test]
    fn a_hole_after_a_newline_and_wide_text_finds_its_byte() {
        let src = "let a = \"\u{e9}\n\u{e9}\\{x.y()}\"\n";
        assert_eq!(class(src, "y"), Some(Function));
    }

    #[test]
    fn a_word_a_rule_consumes_is_a_keyword_and_an_ordinary_name_is_not() {
        let src = "fn f(a: read T, read: Int64) -> Int64 { return read + a }\n";
        let got = runs(src);
        assert_eq!(got.iter().filter(|r| r.0 == "read").count(), 1);
        assert_eq!(class(src, "read"), Some(Keyword));
        assert_eq!(class(src, "T"), Some(Type));
        assert_eq!(class(src, "a"), None);
        assert_eq!(class(src, "return"), Some(Keyword));
    }

    #[test]
    fn await_and_consume_are_keywords_only_before_their_operand() {
        assert_eq!(class("fn f() { await g() }\n", "await"), Some(Keyword));
        assert_eq!(class("fn f() { return await }\n", "await"), None);
        assert_eq!(class("fn f() { g(consume x) }\n", "consume"), Some(Keyword));
    }

    #[test]
    fn a_callee_is_a_function_and_a_capital_one_stays_a_type() {
        let src = "fn f() { print(1) Some(2) panic(3) }\n";
        assert_eq!(class(src, "f"), Some(Function));
        assert_eq!(class(src, "print"), Some(Function));
        assert_eq!(class(src, "Some"), Some(Type));
        assert_eq!(class(src, "panic"), Some(Function));
    }

    #[test]
    fn a_type_constructor_stays_a_type() {
        assert_eq!(class("let a: Array<Int64> = []\n", "Array"), Some(Type));
    }

    #[test]
    fn a_comment_and_a_doc_line_are_comments() {
        assert_eq!(
            runs("// a\n/// b\nlet c = 1\n")[..2],
            [("// a", Comment), ("/// b", Comment)]
        );
    }

    #[test]
    fn a_syntax_error_leaves_the_runs_after_it_and_no_word_in_skipped_text() {
        let src = "fn a(x: read T) { let = 1 }\n) read (\nfn b(y: read T) { }\n";
        let got = runs(src);
        let reads: Vec<Class> = (got.iter().filter(|r| r.0 == "read"))
            .map(|r| r.1)
            .collect();
        assert_eq!(reads, [Keyword, Function, Keyword]);
        assert_eq!(class(src, "b"), Some(Function));
    }

    #[test]
    fn a_lex_error_is_an_error() {
        assert!(code_runs("let a = \"open\n").is_err());
        assert!(code_runs("let a = \"\\{ \"open }\"\n").is_err());
    }

    #[test]
    fn nothing_has_no_runs() {
        assert_eq!(code_runs("").expect("empty lexes"), []);
    }
}
