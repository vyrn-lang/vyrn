//! Hand-written lexer for the Vela v0 subset.

use crate::diagnostics::Diagnostic;

/// A lexical token kind.
#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    // literals & identifiers
    Int(i64),
    /// A floating-point literal, e.g. `1.5` (`Float64`).
    Float(f64),
    /// A string literal, already decoded (escapes resolved).
    Str(String),
    /// An interpolated string `"a\{e}b\{f}c"` (RFC-0007). `parts` are the decoded
    /// literal fragments (always `exprs.len() + 1` of them); `exprs` are the raw,
    /// un-lexed source of each `\{ .. }` hole, re-parsed by the parser.
    TemplateStr { parts: Vec<String>, exprs: Vec<String> },
    /// A `///` documentation comment line (markdown). One leading space after the
    /// slashes is stripped. Attached to the following declaration by the parser.
    Doc(String),
    Ident(String),

    // keywords
    Fn,
    Let,
    Mut,
    If,
    Else,
    While,
    For,
    In,
    Drop,
    Protocol,
    Impl,
    Vself,
    Return,
    True,
    False,
    Type,
    Where,
    Match,
    Region,
    Spawn,

    // punctuation & operators
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket, // [
    RBracket, // ]
    Comma,
    Semi,
    Colon,
    Dot,      // .
    Arrow,    // ->
    FatArrow, // =>
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eq,     // =
    EqEq,   // ==
    TildeMatch, // =~
    NotEq,  // !=
    Lt,     // <
    LtEq,   // <=
    Gt,     // >
    GtEq,   // >=
    AndAnd,   // &&
    OrOr,     // ||
    Bang,     // !
    Question, // ?
    Pipe,     // |
    Amp,      // &

    Eof,
}

/// A token plus the 1-based line and column it appeared on (for diagnostics).
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub line: usize,
    pub col: usize,
}

/// Parse a `\u{HEX}` Unicode-scalar escape starting at the backslash (`chars[at]`
/// is `\`, `chars[at+1]` is `u`). Returns the decoded character and the index
/// just past the closing `}`.
fn parse_unicode_escape(
    chars: &[char],
    at: usize,
    line: usize,
    col: usize,
) -> Result<(char, usize), Diagnostic> {
    let err = |m: &str| Diagnostic::error(line, col, "lex", m.to_string());
    if at + 2 >= chars.len() || chars[at + 2] != '{' {
        return Err(err("`\\u` must be followed by `{HEX}`"));
    }
    let mut j = at + 3;
    let mut hex = String::new();
    while j < chars.len() && chars[j] != '}' {
        hex.push(chars[j]);
        j += 1;
    }
    if j >= chars.len() {
        return Err(err("unterminated `\\u{` escape"));
    }
    let cp = u32::from_str_radix(hex.trim(), 16)
        .map_err(|_| err("`\\u{}` needs hex digits"))?;
    let ch = char::from_u32(cp).ok_or_else(|| err("invalid Unicode scalar in `\\u{}`"))?;
    Ok((ch, j + 1)) // past the closing `}`
}

/// Tokenize `src`. Returns an error [`Diagnostic`] on the first illegal
/// character.
pub fn lex(src: &str) -> Result<Vec<Token>, Diagnostic> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut line = 1;
    // Index of the first character of the current line; column is derived as
    // `i - line_start + 1`, which stays correct regardless of how `i` advances.
    let mut line_start = 0;
    let mut out = Vec::new();

    while i < chars.len() {
        let c = chars[i];
        // 1-based column of the current position.
        let col = i - line_start + 1;

        // whitespace
        if c == '\n' {
            line += 1;
            i += 1;
            line_start = i;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        // line comment `// ..` (skipped) or doc comment `/// ..` (captured as
        // markdown, attached to the next declaration). `////+` is a plain comment.
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            let is_doc = i + 2 < chars.len()
                && chars[i + 2] == '/'
                && !(i + 3 < chars.len() && chars[i + 3] == '/');
            let start = i;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            if is_doc {
                let text: String = chars[start + 3..i].iter().collect();
                // CRLF files leave a trailing `\r` (the scan stops at `\n`
                // only) — strip it so it never leaks into rendered markdown.
                let text = text.strip_suffix('\r').unwrap_or(&text);
                let text = text.strip_prefix(' ').unwrap_or(text).to_string();
                out.push(Token { tok: Tok::Doc(text), line, col });
            }
            continue;
        }

        // string literal: "..." with \n \t \\ \" escapes, and `\{ expr }`
        // interpolation holes (RFC-0007). A hole leaves `{`/`}` as ordinary
        // characters, so only `\{` opens interpolation.
        if c == '"' {
            let start_col = col; // column of the opening quote
            let start_line = line; // a multi-line string is anchored at its start
            i += 1; // opening quote
            let mut parts: Vec<String> = Vec::new();
            let mut exprs: Vec<String> = Vec::new();
            let mut cur = String::new();
            loop {
                if i >= chars.len() {
                    return Err(Diagnostic::error(
                        line,
                        start_col,
                        "lex",
                        "unterminated string literal".to_string(),
                    ));
                }
                let ch = chars[i];
                if ch == '"' {
                    i += 1; // closing quote
                    break;
                }
                // Multi-line strings (RFC-0007): a raw newline is part of the
                // string. Track the line so later diagnostics stay accurate.
                if ch == '\n' {
                    line += 1;
                    i += 1;
                    line_start = i;
                    cur.push('\n');
                    continue;
                }
                if ch == '\\' {
                    if i + 1 >= chars.len() {
                        return Err(Diagnostic::error(
                            line,
                            start_col,
                            "lex",
                            "unterminated escape in string".to_string(),
                        ));
                    }
                    // `\{` opens an interpolation hole; scan its raw source up to
                    // the matching `}` (tracking nested braces and strings).
                    if chars[i + 1] == '{' {
                        parts.push(std::mem::take(&mut cur));
                        i += 2; // skip `\{`
                        let start = i;
                        let mut depth = 1usize;
                        while i < chars.len() && depth > 0 {
                            match chars[i] {
                                '\n' => {
                                    // A hole may span lines too; keep counting.
                                    line += 1;
                                    i += 1;
                                    line_start = i;
                                }
                                '"' => {
                                    // Skip a nested string, respecting its escapes.
                                    i += 1;
                                    while i < chars.len() && chars[i] != '"' {
                                        i += if chars[i] == '\\' { 2 } else { 1 };
                                    }
                                    if i >= chars.len() {
                                        return Err(Diagnostic::error(
                                            line,
                                            start_col,
                                            "lex",
                                            "unterminated string in interpolation".to_string(),
                                        ));
                                    }
                                    i += 1; // closing nested quote
                                }
                                // Skip a nested char literal — a `}` inside one
                                // (`'}'`, `'\u{1F600}'`) must not close the hole.
                                '\'' => {
                                    i += 1;
                                    while i < chars.len() && chars[i] != '\'' && chars[i] != '\n'
                                    {
                                        i += if chars[i] == '\\' { 2 } else { 1 };
                                    }
                                    if i >= chars.len() || chars[i] == '\n' {
                                        return Err(Diagnostic::error(
                                            line,
                                            start_col,
                                            "lex",
                                            "unterminated character literal in interpolation"
                                                .to_string(),
                                        ));
                                    }
                                    i += 1; // closing nested quote
                                }
                                // Skip a `//` comment — a `}` inside it is text,
                                // not the end of the hole. The `\n` stays for the
                                // newline arm to count.
                                '/' if i + 1 < chars.len() && chars[i + 1] == '/' => {
                                    while i < chars.len() && chars[i] != '\n' {
                                        i += 1;
                                    }
                                }
                                '{' => {
                                    depth += 1;
                                    i += 1;
                                }
                                '}' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                    i += 1;
                                }
                                _ => i += 1,
                            }
                        }
                        if depth != 0 {
                            return Err(Diagnostic::error(
                                line,
                                start_col,
                                "lex",
                                "unterminated `\\{` interpolation".to_string(),
                            ));
                        }
                        let src: String = chars[start..i].iter().collect();
                        if src.trim().is_empty() {
                            return Err(Diagnostic::error(
                                line,
                                start_col,
                                "lex",
                                "empty `\\{ }` interpolation".to_string(),
                            ));
                        }
                        exprs.push(src);
                        i += 1; // skip closing `}`
                        continue;
                    }
                    // `\u{XXXX}` — a Unicode scalar by hex code point.
                    if chars[i + 1] == 'u' {
                        let (ch, next) = parse_unicode_escape(&chars, i, line, start_col)?;
                        cur.push(ch);
                        i = next;
                        continue;
                    }
                    // Ordinary escape.
                    let esc = match chars[i + 1] {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        '\\' => '\\',
                        '"' => '"',
                        other => {
                            return Err(Diagnostic::error(
                                line,
                                start_col,
                                "lex",
                                format!("unknown escape `\\{other}`"),
                            ))
                        }
                    };
                    cur.push(esc);
                    i += 2;
                    continue;
                }
                cur.push(ch);
                i += 1;
            }
            // The token is anchored at the OPENING quote: a diagnostic on a
            // multi-line string must point where it starts, not where it ends
            // (and `(end_line, start_col)` would be internally inconsistent).
            if exprs.is_empty() {
                out.push(Token {
                    tok: Tok::Str(cur),
                    line: start_line,
                    col: start_col,
                });
            } else {
                parts.push(cur); // the trailing fragment after the last hole
                out.push(Token {
                    tok: Tok::TemplateStr { parts, exprs },
                    line: start_line,
                    col: start_col,
                });
            }
            continue;
        }

        // character literal: `'a'` / `'é'` / `'\u{1F600}'` is the Unicode scalar
        // value (code point) as an Int — Vela has no distinct char type.
        // Escapes: `\n \t \r \0 \\ \'` and `\u{HEX}`.
        if c == '\'' {
            let start_col = col;
            if i + 1 >= chars.len() {
                return Err(Diagnostic::error(line, start_col, "lex", "unterminated character literal".into()));
            }
            let (cp, consumed) = if chars[i + 1] == '\\' {
                if i + 2 >= chars.len() {
                    return Err(Diagnostic::error(line, start_col, "lex", "unterminated character escape".into()));
                }
                if chars[i + 2] == 'u' {
                    // `'\u{HEX}'` — a Unicode scalar by code point.
                    let (ch, next) = parse_unicode_escape(&chars, i + 1, line, start_col)?;
                    (ch as u32, next - i) // `next` is past `}`; consumed chars after `'`
                } else {
                    let e: u32 = match chars[i + 2] {
                        'n' => u32::from('\n'),
                        't' => u32::from('\t'),
                        'r' => u32::from('\r'),
                        '0' => 0,
                        '\\' => u32::from('\\'),
                        '\'' => u32::from('\''),
                        other => {
                            return Err(Diagnostic::error(
                                line,
                                start_col,
                                "lex",
                                format!("unknown character escape `\\{other}`"),
                            ))
                        }
                    };
                    (e, 3) // '\x'
                }
            } else {
                // A raw newline is almost certainly a typo (and would silently
                // desync line counting) — require the `'\n'` escape.
                if chars[i + 1] == '\n' {
                    return Err(Diagnostic::error(
                        line,
                        start_col,
                        "lex",
                        "raw newline in character literal; write '\\n'".into(),
                    ));
                }
                (chars[i + 1] as u32, 2) // 'x' — any Unicode scalar
            };
            if i + consumed >= chars.len() || chars[i + consumed] != '\'' {
                return Err(Diagnostic::error(line, start_col, "lex", "unterminated character literal".into()));
            }
            out.push(Token { tok: Tok::Int(cp as i64), line, col: start_col });
            i += consumed + 1; // include closing quote
            continue;
        }

        // integer literal
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            // A `.` followed by a digit makes this a float literal (`1.5`); a `.`
            // followed by anything else is field/method access (`x.foo`, `1.max`).
            let is_float = i + 1 < chars.len() && chars[i] == '.' && chars[i + 1].is_ascii_digit();
            if is_float {
                i += 1; // consume the `.`
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                let value: f64 = text.parse().map_err(|_| {
                    Diagnostic::error(line, col, "lex", format!("invalid float literal: {text}"))
                })?;
                out.push(Token { tok: Tok::Float(value), line, col });
                continue;
            }
            let text: String = chars[start..i].iter().collect();
            // Integer literals are stored as `i64` bits. A value above `i64::MAX`
            // (only reachable for `UInt64`) is accepted by reinterpreting its
            // `u64` bit pattern, so e.g. `10000000000000000000` round-trips.
            let value: i64 = match text.parse::<i64>() {
                Ok(v) => v,
                Err(_) => text.parse::<u64>().map(|u| u as i64).map_err(|_| {
                    Diagnostic::error(
                        line,
                        col,
                        "lex",
                        format!("integer literal out of range: {text}"),
                    )
                })?,
            };
            out.push(Token {
                tok: Tok::Int(value),
                line,
                col,
            });
            continue;
        }

        // identifier or keyword
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            let tok = match text.as_str() {
                "fn" => Tok::Fn,
                "let" => Tok::Let,
                "mut" => Tok::Mut,
                "if" => Tok::If,
                "else" => Tok::Else,
                "while" => Tok::While,
                "for" => Tok::For,
                "in" => Tok::In,
                "drop" => Tok::Drop,
                "protocol" => Tok::Protocol,
                "impl" => Tok::Impl,
                "self" => Tok::Vself,
                "return" => Tok::Return,
                "true" => Tok::True,
                "false" => Tok::False,
                "type" => Tok::Type,
                "where" => Tok::Where,
                "match" => Tok::Match,
                "region" => Tok::Region,
                "spawn" => Tok::Spawn,
                _ => Tok::Ident(text),
            };
            out.push(Token { tok, line, col });
            continue;
        }

        // multi-char operators, then single-char
        let two: Option<Tok> = if i + 1 < chars.len() {
            match (c, chars[i + 1]) {
                ('-', '>') => Some(Tok::Arrow),
                ('=', '>') => Some(Tok::FatArrow),
                ('=', '~') => Some(Tok::TildeMatch),
                ('=', '=') => Some(Tok::EqEq),
                ('!', '=') => Some(Tok::NotEq),
                ('<', '=') => Some(Tok::LtEq),
                ('>', '=') => Some(Tok::GtEq),
                ('&', '&') => Some(Tok::AndAnd),
                ('|', '|') => Some(Tok::OrOr),
                _ => None,
            }
        } else {
            None
        };
        if let Some(tok) = two {
            out.push(Token { tok, line, col });
            i += 2;
            continue;
        }

        let single = match c {
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            '{' => Tok::LBrace,
            '}' => Tok::RBrace,
            '[' => Tok::LBracket,
            ']' => Tok::RBracket,
            ',' => Tok::Comma,
            ';' => Tok::Semi,
            ':' => Tok::Colon,
            '.' => Tok::Dot,
            '+' => Tok::Plus,
            '-' => Tok::Minus,
            '*' => Tok::Star,
            '/' => Tok::Slash,
            '%' => Tok::Percent,
            '=' => Tok::Eq,
            '<' => Tok::Lt,
            '>' => Tok::Gt,
            '!' => Tok::Bang,
            '?' => Tok::Question,
            '|' => Tok::Pipe,
            '&' => Tok::Amp,
            other => {
                return Err(Diagnostic::error(
                    line,
                    col,
                    "lex",
                    format!("unexpected character {other:?}"),
                ))
            }
        };
        out.push(Token {
            tok: single,
            line,
            col,
        });
        i += 1;
    }

    out.push(Token {
        tok: Tok::Eof,
        line,
        col: i - line_start + 1,
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexes_operators_and_keywords() {
        let toks = lex("fn main() -> Int { let x = 1 + 2; }").unwrap();
        let kinds: Vec<Tok> = toks.into_iter().map(|t| t.tok).collect();
        assert_eq!(
            kinds,
            vec![
                Tok::Fn,
                Tok::Ident("main".into()),
                Tok::LParen,
                Tok::RParen,
                Tok::Arrow,
                Tok::Ident("Int".into()),
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
    fn tracks_lines_and_skips_comments() {
        let toks = lex("// c\nlet\n  x").unwrap();
        assert_eq!(toks[0].tok, Tok::Let);
        assert_eq!(toks[0].line, 2);
        assert_eq!(toks[1].line, 3);
    }

    #[test]
    fn multiline_string_token_is_anchored_at_its_start() {
        // A string spanning lines 2-4 must carry line 2 (where it opens), so a
        // diagnostic about it points at the right place.
        let toks = lex("let\n\"a\nb\nc\"\nx").unwrap();
        assert!(matches!(toks[1].tok, Tok::Str(_)));
        assert_eq!(toks[1].line, 2, "anchored at the opening quote");
        assert_eq!(toks[2].line, 5, "counting still advances past it");
    }

    #[test]
    fn doc_comment_strips_trailing_cr() {
        // CRLF files must not leak `\r` into rendered markdown.
        let toks = lex("/// hello\r\nfn").unwrap();
        assert_eq!(toks[0].tok, Tok::Doc("hello".into()));
    }

    #[test]
    fn hole_scanner_skips_comments_and_char_literals() {
        // A `}` inside a char literal or a `//` comment must not close the hole.
        let toks = lex("\"\\{'}'}\"").unwrap();
        assert!(
            matches!(&toks[0].tok, Tok::TemplateStr { exprs, .. } if exprs[0] == "'}'"),
            "{:?}",
            toks[0].tok
        );
        let toks = lex("\"\\{ 1 + // } not the end\n 2 }\"").unwrap();
        assert!(
            matches!(&toks[0].tok, Tok::TemplateStr { exprs, .. } if exprs[0].contains("2")),
            "{:?}",
            toks[0].tok
        );
    }

    #[test]
    fn raw_newline_in_char_literal_is_rejected() {
        let e = lex("'\n'").unwrap_err();
        assert!(e.message.contains("raw newline"), "{}", e.message);
    }
}
