//! The lexer: [`scan`] reads the source once, and [`lex`] and the formatter read the scan.

#[rustfmt::skip]
mod grammar;

pub(crate) use grammar::punct_tok;
pub use grammar::{Tok, CONTEXTUAL, KEYWORDS, PUNCT_SPELLINGS};

use crate::diagnostics::Diagnostic;
use crate::rules::{refuse, Rule};

/// An interpolation hole: its raw source and where that source starts, so the
/// nodes parsed from it carry their own lines and columns (#471).
#[derive(Debug, Clone, PartialEq)]
pub struct Hole {
    pub src: String,
    pub line: usize,
    pub col: usize,
}

/// A token and the 1-based line and column it starts on.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub line: usize,
    pub col: usize,
}

/// Drops a `vyrn"""` quote's closing indentation, Swift's rule: when the text
/// opens with a newline and its last line holds only whitespace, that
/// whitespace leaves the start of every line, and the opening newline and the
/// last line with the newline before it go. A part that starts after a hole
/// starts mid-line, so only text after a newline is a line start.
fn dedent(parts: &mut [String]) {
    let Some(last) = parts.last() else { return };
    let Some(nl) = last.rfind('\n') else { return };
    let close = last[nl + 1..].to_string();
    let opens = parts.first().is_some_and(|p| p.starts_with('\n'));
    if opens && close.trim().is_empty() {
        let n = parts.len();
        parts[n - 1].truncate(nl);
        for (i, part) in parts.iter_mut().enumerate() {
            let mut out = String::with_capacity(part.len());
            for (j, line) in part.split_inclusive('\n').enumerate() {
                let start = j > 0 || i == 0;
                out.push_str(match start {
                    true => line.strip_prefix(close.as_str()).unwrap_or(line),
                    false => line,
                });
            }
            *part = out;
        }
        if let Some(first) = parts.first_mut() {
            if first.starts_with('\n') {
                first.remove(0);
            }
        }
    }
}

/// Returns the `(kind, text)` pair the `lex()` builtin gives generators: `kind` is
/// a stable category name, `text` a literal's decoded value or the spelling.
pub fn token_name_and_text(tok: &Tok) -> (String, String) {
    match tok {
        Tok::Int(n) => ("int".to_string(), n.to_string()),
        // A byte literal reads as its integer value.
        Tok::Byte(n) => ("int".to_string(), n.to_string()),
        Tok::Float(f) => ("float".to_string(), format!("{f:?}")),
        Tok::Str(s) => ("string".to_string(), s.clone()),
        Tok::TemplateStr { parts, .. } => ("template".to_string(), parts.join("")),
        Tok::Doc(s) => ("doc".to_string(), s.clone()),
        Tok::Ident(s) => ("ident".to_string(), s.clone()),
        Tok::Eof => ("eof".to_string(), String::new()),
        t => match t.keyword_text() {
            Some(w) => ("keyword".to_string(), w.to_string()),
            None => (
                "punct".to_string(),
                t.punct_text()
                    .expect("every token that is not a literal, a keyword or `Eof` is punctuation")
                    .to_string(),
            ),
        },
    }
}

/// One lexical item, a token or a comment, with the span of its raw source
/// text and the lines it spans. [`lex`] drops the comments and keeps the
/// payloads. The formatter keeps the comments and prints the raw text
/// ([`Triv::text`]), changing only the whitespace between items, so it never
/// re-escapes a literal.
#[derive(Debug, Clone)]
pub struct Triv {
    pub kind: TrivKind,
    /// The raw source text, as a byte range of the scanned source
    /// ([`Triv::text`]). A comment keeps its slashes; its trailing whitespace
    /// and CR fall outside the range.
    pub span: std::ops::Range<usize>,
    /// 1-based line the item starts on.
    pub start_line: usize,
    /// 1-based start column. The formatter never reads it: it re-indents everything.
    pub col: usize,
    /// 1-based end line; it differs from `start_line` only for a multi-line string.
    pub end_line: usize,
    /// Whether whitespace, a newline or a comment precedes this item. It tells a
    /// generic `<`/`>`, which is tight, from a comparison.
    pub space_before: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TrivKind {
    /// A token with its payload. The formatter reads only its kind and prints the
    /// raw text.
    Tok(Tok),
    /// A `//` line comment, `////+` included.
    Comment,
    /// A `///` doc line. The payload is the markdown [`lex`] attaches to the next
    /// declaration: the line minus `///`, a trailing CR and one leading space.
    Doc(String),
}

impl Triv {
    /// The raw source text of the item in `src`, the source [`scan`] read.
    pub fn text<'s>(&self, src: &'s str) -> &'s str {
        &src[self.span.clone()]
    }
}

/// One scan of a source file: every item in order, and the end position where
/// [`lex`] puts `Eof`.
pub struct Scan {
    pub items: Vec<Triv>,
    pub end_line: usize,
    pub end_col: usize,
}

/// Scans `src` once into every lexical item and the end position.
///
/// The rules are `vyrn.syntax`'s, as [`grammar`] states them; this module builds
/// the items and decodes each literal's value. [`lex`] and the formatter both
/// read the scan, so `vyrn fmt` formats only files `vyrn check` lexes. A
/// literal's payload is decoded here and its raw text kept beside it, so
/// neither reader scans again.
pub fn scan(src: &str) -> Result<Scan, Diagnostic> {
    let items = Items {
        src,
        items: Vec::new(),
        escapes: Vec::new(),
        lines: LineCol::START,
        end: 0,
    };
    let mut scanner = grammar::Scanner::new(src, items);
    scanner.scan().map_err(|d| *d)?;
    let mut items = scanner.hooks;
    let (end_line, end_col) = items.lines.of(src, src.len());
    Ok(Scan {
        items: items.items,
        end_line,
        end_col,
    })
}

/// The 1-based line and character column of a byte offset, counted on from
/// the offset asked before.
#[derive(Clone, Copy)]
struct LineCol {
    at: usize,
    line: usize,
    col: usize,
}

impl LineCol {
    const START: LineCol = LineCol {
        at: 0,
        line: 1,
        col: 1,
    };

    /// Returns the line and column of byte `to` of `src`, a character boundary.
    /// An offset before the last one asked recounts from the start.
    fn of(&mut self, src: &str, to: usize) -> (usize, usize) {
        if to < self.at {
            *self = LineCol::START;
        }
        // A character starts at every byte but a UTF-8 continuation byte.
        let chars = |b: &[u8]| b.iter().filter(|&&b| b & 0xC0 != 0x80).count();
        let seg = &src.as_bytes()[self.at..to];
        // Most segments are a blank and a token: ASCII on one line.
        if seg.iter().all(|&b| b != b'\n' && b < 0x80) {
            self.col += seg.len();
        } else if let Some(nl) = seg.iter().rposition(|&b| b == b'\n') {
            self.line += seg[..=nl].iter().filter(|&&b| b == b'\n').count();
            self.col = 1 + chars(&seg[nl + 1..]);
        } else {
            self.col += chars(seg);
        }
        self.at = to;
        (self.line, self.col)
    }
}

/// What an escape of a string literal decodes to.
enum Escape {
    Char(char),
    /// A `\{ expr }` hole.
    Hole,
}

/// The items of one scan, built as the scanner reports its rules.
struct Items<'s> {
    src: &'s str,
    items: Vec<Triv>,
    /// The escapes of the string literal being scanned, each with its byte
    /// range from the backslash; the `string` hook empties it.
    escapes: Vec<(std::ops::Range<usize>, Escape)>,
    lines: LineCol,
    /// The end of the last item, so the next knows whether space precedes it.
    end: usize,
}

impl Items<'_> {
    /// Pushes an item that starts at `at`, a line and column, and ends on
    /// `end_line`. `span` is its printed text, and its rule matched up to `end`.
    fn push(
        &mut self,
        kind: TrivKind,
        at: (usize, usize),
        end_line: usize,
        span: std::ops::Range<usize>,
        end: usize,
    ) {
        let space_before = self.items.is_empty() || self.end < span.start;
        self.items.push(Triv {
            kind,
            span,
            start_line: at.0,
            col: at.1,
            end_line,
            space_before,
        });
        self.end = end;
    }

    /// Pushes a line comment or doc line; its printed text drops trailing
    /// blanks and the CR of a CRLF line.
    fn push_line(&mut self, kind: TrivKind, start: usize, end: usize) {
        let at = self.lines.of(self.src, start);
        let text = self.src[start..end].trim_end_matches(['\r', ' ', '\t']);
        self.push(kind, at, at.0, start..start + text.len(), end);
    }
}

impl grammar::Hooks for Items<'_> {
    fn refuse(&mut self, at: usize, rule: Rule) -> Box<Diagnostic> {
        let (line, col) = self.lines.of(self.src, at);
        Box::new(Diagnostic::refusal(line, col, "lex", rule))
    }

    fn punct(&mut self, start: usize, end: usize, tok: Tok) -> Result<(), Box<Diagnostic>> {
        let at = self.lines.of(self.src, start);
        self.push(TrivKind::Tok(tok), at, at.0, start..end, end);
        Ok(())
    }

    fn comment(&mut self, start: usize, end: usize) -> Result<(), Box<Diagnostic>> {
        self.push_line(TrivKind::Comment, start, end);
        Ok(())
    }

    fn doc(&mut self, start: usize, end: usize) -> Result<(), Box<Diagnostic>> {
        // A CRLF file leaves a trailing `\r`; strip it so it never reaches markdown.
        let text = &self.src[start + 3..end];
        let text = text.strip_suffix('\r').unwrap_or(text);
        let text = text.strip_prefix(' ').unwrap_or(text).to_string();
        self.push_line(TrivKind::Doc(text), start, end);
        Ok(())
    }

    fn word(&mut self, start: usize, end: usize) -> Result<(), Box<Diagnostic>> {
        let at = self.lines.of(self.src, start);
        let text = &self.src[start..end];
        let tok = grammar::keyword(text).unwrap_or_else(|| Tok::Ident(text.to_string()));
        self.push(TrivKind::Tok(tok), at, at.0, start..end, end);
        Ok(())
    }

    fn number(&mut self, start: usize, end: usize) -> Result<(), Box<Diagnostic>> {
        let at = self.lines.of(self.src, start);
        let text = &self.src[start..end];
        let tok = if text.contains('.') {
            // Every `digits.digits` text is in f64::from_str's grammar, which rounds a
            // text too large to infinity, so the parse never fails.
            Tok::Float(text.parse().expect("digits.digits parses as f64"))
        } else {
            // A literal above `i64::MAX`, reachable only for `UInt64`, keeps its `u64` bit
            // pattern.
            match text.parse::<i64>() {
                Ok(v) => Tok::Int(v),
                Err(_) => Tok::Int(
                    text.parse::<u64>()
                        .map_err(|_| refuse!("lex", at.0, at.1, IntOutOfRange, text))?
                        as i64,
                ),
            }
        };
        self.push(TrivKind::Tok(tok), at, at.0, start..end, end);
        Ok(())
    }

    fn byte(&mut self, start: usize, end: usize) -> Result<(), Box<Diagnostic>> {
        let at = self.lines.of(self.src, start);
        let value = match &self.src.as_bytes()[start + 1..end - 1] {
            [b'\\', b'n'] => Some(b'\n'),
            [b'\\', b't'] => Some(b'\t'),
            [b'\\', b'r'] => Some(b'\r'),
            [b'\\', b'0'] => Some(0),
            [b'\\', b'x', ..] => u8::from_str_radix(&self.src[start + 3..end - 1], 16).ok(),
            [b'\\', c @ (b'\\' | b'\'')] | [c] => Some(*c),
            _ => None,
        };
        let value = value.expect("vyrn.syntax's byteBody admits one byte or one escape");
        self.push(TrivKind::Tok(Tok::Byte(value)), at, at.0, start..end, end);
        Ok(())
    }

    /// An escape after its backslash, at `start`; `q` is its string's opening quote.
    fn esc(&mut self, start: usize, end: usize, q: usize) -> Result<(), Box<Diagnostic>> {
        let src = self.src;
        let value = match src.as_bytes()[start] {
            b'n' => Escape::Char('\n'),
            b't' => Escape::Char('\t'),
            b'r' => Escape::Char('\r'),
            c @ (b'\\' | b'"') => Escape::Char(c as char),
            b'{' => Escape::Hole,
            // `\u{XXXX}`: a Unicode scalar by hex code point.
            b'u' => {
                let hex = src[start + 2..end - 1].trim();
                let c = u32::from_str_radix(hex, 16)
                    .map_err(|_| self.refuse(q, Rule::UnicodeEscapeDigits {}))?;
                let c = char::from_u32(c)
                    .ok_or_else(|| self.refuse(q, Rule::UnicodeEscapeScalar {}))?;
                // A String is NUL-terminated natively, so it cannot hold `\u{0}`.
                if c == '\0' {
                    return Err(self.refuse(q, Rule::NulInString {}));
                }
                Escape::Char(c)
            }
            other => unreachable!("vyrn.syntax's esc rule admits no {:?}", other as char),
        };
        self.escapes.push((start - 1..end, value));
        Ok(())
    }

    fn string(&mut self, start: usize, end: usize) -> Result<(), Box<Diagnostic>> {
        let src = self.src;
        let at = self.lines.of(src, start);
        // In a `"""` string a lone `"` or `""` is text; only the delimiters differ.
        let quotes = if src[start..].starts_with("\"\"\"") {
            3
        } else {
            1
        };
        let mut escapes = std::mem::take(&mut self.escapes);
        let mut parts: Vec<String> = Vec::new();
        let mut exprs: Vec<Hole> = Vec::new();
        let mut cur = String::new();
        let mut from = start + quotes;
        for (range, value) in escapes.drain(..) {
            push_text(&mut cur, &src[from..range.start]);
            match value {
                Escape::Char(c) => cur.push(c),
                Escape::Hole => {
                    parts.push(std::mem::take(&mut cur));
                    let inner = range.start + 2..range.end - 1;
                    let (line, col) = self.lines.of(src, inner.start);
                    exprs.push(Hole {
                        src: src[inner].to_string(),
                        line,
                        col,
                    });
                }
            }
            from = range.end;
        }
        self.escapes = escapes;
        push_text(&mut cur, &src[from..end - quotes]);
        // A `vyrn"""` code quote, its tag the item before it on its line as the
        // parser reads it, drops its closing indentation. Only here are the raw
        // delimiters known: `vyrn"\n"` is no multi-line quote.
        let quote = quotes == 3
            && self.items.last().is_some_and(|t| {
                t.end_line == at.0 && matches!(&t.kind, TrivKind::Tok(Tok::Ident(n)) if n == "vyrn")
            });
        let tok = if exprs.is_empty() {
            if quote {
                dedent(std::slice::from_mut(&mut cur));
            }
            Tok::Str(cur)
        } else {
            parts.push(cur); // the trailing fragment after the last hole
            if quote {
                dedent(&mut parts);
            }
            Tok::TemplateStr { parts, exprs }
        };
        // The token sits at the opening quote, so a diagnostic on a multi-line
        // string points where it starts. Only a string spans lines.
        let (end_line, _) = self.lines.of(src, end);
        self.push(TrivKind::Tok(tok), at, end_line, start..end, end);
        Ok(())
    }
}

/// Appends the raw text of a string literal with each CRLF as LF, so a
/// multi-line string has the same bytes on every checkout. The raw text keeps
/// the CR for the formatter.
fn push_text(cur: &mut String, raw: &str) {
    let mut lines = raw.split("\r\n");
    if let Some(first) = lines.next() {
        cur.push_str(first);
    }
    for line in lines {
        cur.push('\n');
        cur.push_str(line);
    }
}

/// Returns every item [`scan`] finds, comments included, for the formatter.
pub fn lex_with_trivia(src: &str) -> Result<Vec<Triv>, Diagnostic> {
    Ok(scan(src)?.items)
}

/// Tokenizes `src`: [`scan`]'s items without comments, closed by `Eof`. Returns
/// an error diagnostic for the first malformed token.
pub fn lex(src: &str) -> Result<Vec<Token>, Diagnostic> {
    Ok(tokens(scan(src)?))
}

/// The tokens of a [`scan`], comments dropped and `Eof` last.
pub fn tokens(scanned: Scan) -> Vec<Token> {
    let mut out = Vec::with_capacity(scanned.items.len() + 1);
    for it in scanned.items {
        let tok = match it.kind {
            TrivKind::Comment => continue,
            TrivKind::Doc(text) => Tok::Doc(text),
            TrivKind::Tok(t) => t,
        };
        out.push(Token {
            tok,
            line: it.start_line,
            col: it.col,
        });
    }
    out.push(Token {
        tok: Tok::Eof,
        line: scanned.end_line,
        col: scanned.end_col,
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A string literal's error sits where the literal starts, as its token does:
    /// the line it reports holds the column it reports.
    #[test]
    fn a_string_error_points_at_the_opening_quote() {
        for (src, why) in [
            ("  \"\n", "unterminated"),
            ("  \"a\nb\\q\"", "unknown escape"),
            ("  \"a\nb\\u{0}\"", "NUL"),
            ("  \"a\nb\\{\"", "unterminated interpolation"),
        ] {
            let d = scan(src)
                .err()
                .unwrap_or_else(|| panic!("{why}: {src:?} lexes"));
            assert_eq!((d.line, d.col), (1, 3), "{why}: {src:?}");
        }
    }

    #[test]
    fn lexes_operators_and_keywords() {
        let toks = lex("fn main() -> Int64 { let x = 1 + 2; }").unwrap();
        let kinds: Vec<Tok> = toks.into_iter().map(|t| t.tok).collect();
        assert_eq!(
            kinds,
            vec![
                Tok::Fn,
                Tok::Ident("main".into()),
                Tok::LParen,
                Tok::RParen,
                Tok::Arrow,
                Tok::Ident("Int64".into()),
                Tok::LBrace,
                Tok::Let,
                Tok::Ident("x".into()),
                Tok::Eq,
                Tok::Int(1),
                Tok::Plus,
                Tok::Int(2),
                Tok::Semi,
                Tok::RBrace,
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn lexes_bitwise_and_shift_tokens() {
        // `<<` and `>>` are greedy; the type parser splits a `>>`.
        let kinds: Vec<Tok> = lex("a & b | c ^ ~d << e >> f")
            .unwrap()
            .into_iter()
            .map(|t| t.tok)
            .collect();
        assert_eq!(
            kinds,
            vec![
                Tok::Ident("a".into()),
                Tok::Amp,
                Tok::Ident("b".into()),
                Tok::Pipe,
                Tok::Ident("c".into()),
                Tok::Caret,
                Tok::Tilde,
                Tok::Ident("d".into()),
                Tok::Shl,
                Tok::Ident("e".into()),
                Tok::Shr,
                Tok::Ident("f".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn tracks_lines_and_skips_comments() {
        let toks = lex("// c\nlet\n  x").unwrap();
        assert_eq!(toks[0].tok, Tok::Let);
        assert_eq!(toks[0].line, 2);
        assert_eq!(toks[1].line, 3);
    }

    #[test]
    fn multiline_string_token_is_anchored_at_its_start() {
        let toks = lex("let\n\"a\nb\nc\"\nx").unwrap();
        assert!(matches!(toks[1].tok, Tok::Str(_)));
        assert_eq!(toks[1].line, 2, "anchored at the opening quote");
        assert_eq!(toks[2].line, 5, "counting still advances past it");
    }

    #[test]
    fn doc_comment_strips_trailing_cr() {
        let toks = lex("/// hello\r\nfn").unwrap();
        assert_eq!(toks[0].tok, Tok::Doc("hello".into()));
    }

    #[test]
    fn hole_scanner_skips_comments_and_char_literals() {
        let toks = lex("\"\\{'}'}\"").unwrap();
        assert!(
            matches!(&toks[0].tok, Tok::TemplateStr { exprs, .. } if exprs[0].src == "'}'"),
            "{:?}",
            toks[0].tok
        );
        let toks = lex("\"\\{ 1 + // } not the end\n 2 }\"").unwrap();
        assert!(
            matches!(&toks[0].tok, Tok::TemplateStr { exprs, .. } if exprs[0].src.contains("2")),
            "{:?}",
            toks[0].tok
        );
    }

    #[test]
    fn hole_nested_string_newlines_keep_line_counting_current() {
        let toks = lex("let\n\"\\{ \"x\ny\" }\"\nlet").unwrap();
        assert_eq!(toks[2].line, 4, "counting resumes correctly after the hole");
        assert_eq!(toks.last().unwrap().line, 4);
    }

    #[test]
    fn raw_newline_in_byte_literal_is_rejected() {
        let e = lex("'\n'").unwrap_err();
        assert!(e.message.contains("raw newline"), "{}", e.message);
    }

    // Byte literals.

    fn byte_of(src: &str) -> u8 {
        match lex(src).unwrap()[0].tok {
            Tok::Byte(b) => b,
            ref other => panic!("expected a byte literal, got {other:?}"),
        }
    }

    #[test]
    fn byte_literal_printable_ascii_and_escapes() {
        assert_eq!(byte_of("'a'"), 97);
        assert_eq!(byte_of("'{'"), 123);
        assert_eq!(byte_of("'0'"), 48);
        assert_eq!(byte_of("' '"), 32); // space is printable ASCII
        assert_eq!(byte_of("'~'"), 126); // top of the printable range
        assert_eq!(byte_of("'\\n'"), 0x0A);
        assert_eq!(byte_of("'\\t'"), 0x09);
        assert_eq!(byte_of("'\\r'"), 0x0D);
        assert_eq!(byte_of("'\\0'"), 0x00);
        assert_eq!(byte_of("'\\''"), 0x27);
        assert_eq!(byte_of("'\\\\'"), 0x5C);
        assert_eq!(byte_of("'\\x41'"), 0x41);
        assert_eq!(byte_of("'\\xff'"), 0xFF);
        assert_eq!(byte_of("'\\x00'"), 0x00);
    }

    #[test]
    fn byte_literal_error_cases_have_pinned_wording() {
        assert!(lex("''")
            .unwrap_err()
            .message
            .contains("empty byte literal"));
        let e = lex("'ab'").unwrap_err();
        assert!(
            e.message.contains("single-quoted strings are not allowed"),
            "{}",
            e.message
        );
        let e = lex("'é'").unwrap_err();
        assert!(
            e.message.contains(
                "byte literal must be a single ASCII byte; write the UTF-8 bytes explicitly"
            ),
            "{}",
            e.message
        );
        assert!(lex("'a")
            .unwrap_err()
            .message
            .contains("unterminated byte literal"));
        assert!(lex("'")
            .unwrap_err()
            .message
            .contains("unterminated byte literal"));
        assert!(lex("'\\u{41}'")
            .unwrap_err()
            .message
            .contains("unknown byte escape"));
        assert!(lex("'\\xzz'")
            .unwrap_err()
            .message
            .contains("two hex digits"));
    }

    #[test]
    fn quote_inside_string_and_template_stays_plain_text() {
        assert_eq!(lex("\"it's\"").unwrap()[0].tok, Tok::Str("it's".into()));
        assert_eq!(
            lex("\"\"\"a'b\"\"\"").unwrap()[0].tok,
            Tok::Str("a'b".into())
        );
        assert!(matches!(
            &lex("\"x's \\{y}\"").unwrap()[0].tok,
            Tok::TemplateStr { parts, .. } if parts[0] == "x's "
        ));
    }

    #[test]
    fn a_nul_byte_in_a_string_literal_is_rejected() {
        const MSG: &str = "string literal contains a NUL byte; a Vyrn String is NUL-terminated \
                           and cannot hold one";
        for src in [
            "\"a\0b\"",                   // plain string
            "\"\"\"a\0b\"\"\"",           // triple-quoted
            "\"a\0b \\{x}\"",             // template literal part
            "vyrn\"\"\"let s = \0\"\"\"", // inside a code quote
        ] {
            let e = lex(src).unwrap_err();
            assert_eq!(e.message, MSG, "{src:?}");
        }
        // `\u{0}` spells the same NUL; there is no `\0` escape.
        assert_eq!(lex("\"a\\u{0}b\"").unwrap_err().message, MSG);
        assert_eq!(lex("\"\\u{00}\"").unwrap_err().message, MSG);
        // The formatter's reader refuses it too.
        assert_eq!(lex_with_trivia("\"a\0b\"").unwrap_err().message, MSG);
        // Every other control character is fine: only NUL is unrepresentable.
        assert_eq!(
            lex("\"a\\u{1}b\"").unwrap()[0].tok,
            Tok::Str("a\u{1}b".into())
        );
        assert_eq!(lex("\"a\\nb\"").unwrap()[0].tok, Tok::Str("a\nb".into()));
        // A NUL byte literal is fine: a byte is not a String, and `std/text`'s
        // `decodeUtf8` depends on it.
        assert!(lex("'\\x00'").is_ok());
    }
}
