//! Hand-written lexer: [`scan`] reads the source once, and [`lex`] and the formatter read the scan.

use crate::diagnostics::Diagnostic;

/// An interpolation hole: its raw source and where that source starts, so the
/// nodes parsed from it carry their own lines and columns (#471).
#[derive(Debug, Clone, PartialEq)]
pub struct Hole {
    pub src: String,
    pub line: usize,
    pub col: usize,
}

/// A lexical token kind.
#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Int(i64),
    /// A byte literal `'c'`: one ASCII byte, an integer literal the checker
    /// defaults to `UInt8`.
    Byte(u8),
    Float(f64),
    /// A string literal with its escapes decoded.
    Str(String),
    /// An interpolated string `"a\{e}b"`. `parts` are the decoded fragments,
    /// always `exprs.len() + 1` of them; the parser parses `exprs`.
    TemplateStr {
        parts: Vec<String>,
        exprs: Vec<Hole>,
    },
    /// A `///` doc line as markdown, one leading space stripped. The parser attaches
    /// it to the next declaration.
    Doc(String),
    Ident(String),

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
    Import,
    Export,
    Impl,
    Vself,
    Return,
    True,
    False,
    Type,
    Where,
    Match,
    Region,
    Break,
    Continue,

    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Colon,
    Dot,
    Arrow,
    FatArrow,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eq,
    EqEq,
    TildeMatch,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    AndAnd,
    OrOr,
    Bang,
    Question,
    /// `??`. Maximal munch, so two postfix `?` are written `(x?)?`.
    QuestionQuestion,
    Pipe,
    Amp,
    Caret,
    Tilde,
    // `>>` lexes greedily; in type position the parser splits it into two `>`,
    // so `Array<Array<T>>` parses.
    Shl,
    Shr,

    Eof,
}

/// A token and the 1-based line and column it starts on.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub line: usize,
    pub col: usize,
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
        t => match keyword_text(t) {
            Some(w) => ("keyword".to_string(), w.to_string()),
            None => (
                "punct".to_string(),
                punct_text(t)
                    .expect("every token that is not a literal, a keyword or `Eof` is punctuation")
                    .to_string(),
            ),
        },
    }
}

/// One lexical item, a token or a comment, with its raw source text and the
/// lines it spans. [`lex`] drops the comments and keeps the payloads. The
/// formatter keeps the comments and prints the raw `text`, changing only the
/// whitespace between items, so it never re-escapes a literal.
#[derive(Debug, Clone)]
pub struct Triv {
    pub kind: TrivKind,
    /// Raw source text. A comment keeps its slashes; trailing whitespace and CR
    /// are stripped.
    pub text: String,
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
    /// raw `text`.
    Tok(Tok),
    /// A `//` line comment, `////+` included.
    Comment,
    /// A `///` doc line. The payload is the markdown [`lex`] attaches to the next
    /// declaration: the line minus `///`, a trailing CR and one leading space.
    Doc(String),
}

/// One scan of a source file: every item in order, and the end position where
/// [`lex`] puts `Eof`.
pub struct Scan {
    pub items: Vec<Triv>,
    pub end_line: usize,
    pub end_col: usize,
}

/// Every keyword with its spelling: the one keyword table. It is a macro so one
/// table answers both directions.
///
/// `tests/forms.rs` and `editor/vscode/test/grammar.test.mjs` parse the
/// `"word" => Tok::Name` rows below as text. Keep one row per line, spelled
/// that way.
macro_rules! keywords {
    ($($w:literal => Tok::$t:ident),* $(,)?) => {
        fn keyword_or_ident(text: &str) -> Tok {
            match text {
                $($w => Tok::$t,)*
                _ => Tok::Ident(text.to_string()),
            }
        }

        fn keyword_text(tok: &Tok) -> Option<&'static str> {
            Some(match tok {
                $(Tok::$t => $w,)*
                _ => return None,
            })
        }
    };
}

keywords! {
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
    "import" => Tok::Import,
    "export" => Tok::Export,
    "impl" => Tok::Impl,
    "self" => Tok::Vself,
    "return" => Tok::Return,
    "true" => Tok::True,
    "false" => Tok::False,
    "type" => Tok::Type,
    "where" => Tok::Where,
    "match" => Tok::Match,
    "region" => Tok::Region,
    "break" => Tok::Break,
    "continue" => Tok::Continue,
}

/// Every punctuation token with its spelling: the one punctuation table. It is a
/// macro so one table answers both directions.
macro_rules! punctuation {
    (
        two { $(($a:literal, $b:literal) => $tt:ident),* $(,)? }
        one { $($c:literal => $ot:ident),* $(,)? }
    ) => {
        fn two_char_op(a: char, b: char) -> Option<Tok> {
            match (a, b) {
                $(($a, $b) => Some(Tok::$tt),)*
                _ => None,
            }
        }

        fn single_char_op(c: char) -> Option<Tok> {
            match c {
                $($c => Some(Tok::$ot),)*
                _ => None,
            }
        }

        /// Returns the spelling of a punctuation token, or `None`.
        pub fn punct_text(tok: &Tok) -> Option<&'static str> {
            Some(match tok {
                $(Tok::$tt => concat!($a, $b),)*
                $(Tok::$ot => concat!($c),)*
                _ => return None,
            })
        }

        /// Returns the punctuation token a spelling names, or `None`.
        pub(crate) fn punct_tok(text: &str) -> Option<Tok> {
            match text {
                $(concat!($a, $b) => Some(Tok::$tt),)*
                $(concat!($c) => Some(Tok::$ot),)*
                _ => None,
            }
        }

        /// Every punctuation spelling, two-character forms first. The form
        /// census (`tests/forms.rs`) counts operators against it.
        pub const PUNCT_SPELLINGS: &[&str] = &[$(concat!($a, $b),)* $(concat!($c),)*];
    };
}

punctuation! {
    two {
        ('-', '>') => Arrow,
        ('=', '>') => FatArrow,
        ('=', '~') => TildeMatch,
        ('=', '=') => EqEq,
        ('!', '=') => NotEq,
        ('<', '=') => LtEq,
        ('>', '=') => GtEq,
        ('&', '&') => AndAnd,
        ('|', '|') => OrOr,
        ('?', '?') => QuestionQuestion,
        ('<', '<') => Shl,
        ('>', '>') => Shr,
    }
    one {
        '(' => LParen,
        ')' => RParen,
        '{' => LBrace,
        '}' => RBrace,
        '[' => LBracket,
        ']' => RBracket,
        ',' => Comma,
        ';' => Semi,
        ':' => Colon,
        '.' => Dot,
        '+' => Plus,
        '-' => Minus,
        '*' => Star,
        '/' => Slash,
        '%' => Percent,
        '=' => Eq,
        '<' => Lt,
        '>' => Gt,
        '!' => Bang,
        '?' => Question,
        '|' => Pipe,
        '&' => Amp,
        '^' => Caret,
        '~' => Tilde,
    }
}

/// Scans `src` once into every lexical item and the end position.
///
/// This is the one statement of the lexical grammar. [`lex`] and the formatter
/// both read it, so `vyrn fmt` formats only files `vyrn check` lexes. A literal's
/// payload is decoded here and its raw text kept beside it, so neither reader
/// scans again.
pub fn scan(src: &str) -> Result<Scan, Diagnostic> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0usize;
    let mut line = 1usize;
    // Index of the current line's first character; a column is `i - line_start + 1`.
    let mut line_start = 0usize;
    let mut out: Vec<Triv> = Vec::new();
    // The file start counts as space before the first token.
    let mut space_before = true;

    // Strips a trailing CR and blanks, so a printed comment carries no trailing
    // whitespace. A doc's payload comes from the untrimmed line.
    let trim_comment = |s: &[char]| -> String {
        let mut t: String = s.iter().collect();
        while t.ends_with('\r') || t.ends_with(' ') || t.ends_with('\t') {
            t.pop();
        }
        t
    };

    while i < chars.len() {
        let c = chars[i];
        let col = i - line_start + 1;

        if c == '\n' {
            line += 1;
            i += 1;
            line_start = i;
            space_before = true;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            space_before = true;
            continue;
        }

        let start = i;
        let start_line = line;

        // A `//` comment, or a `///` doc line; `////+` is a plain comment.
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            let is_doc = i + 2 < chars.len()
                && chars[i + 2] == '/'
                && !(i + 3 < chars.len() && chars[i + 3] == '/');
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            let kind = if is_doc {
                let text: String = chars[start + 3..i].iter().collect();
                // A CRLF file leaves a trailing `\r`; strip it so it never reaches markdown.
                let text = text.strip_suffix('\r').unwrap_or(&text);
                TrivKind::Doc(text.strip_prefix(' ').unwrap_or(text).to_string())
            } else {
                TrivKind::Comment
            };
            out.push(Triv {
                kind,
                text: trim_comment(&chars[start..i]),
                start_line,
                col,
                end_line: start_line,
                space_before,
            });
            space_before = true;
            continue;
        }

        // A string literal with escapes and `\{ expr }` holes. Only `\{` opens a hole;
        // a bare `{` is text.
        if c == '"' {
            let start_col = col; // column of the opening quote
                                 // In a `"""..."""` string a lone `"` or `""` is text,
                                 // so emitted code needs no `\"`. Only the terminator differs from
                                 // a plain string.
            let triple = i + 2 < chars.len() && chars[i + 1] == '"' && chars[i + 2] == '"';
            i += if triple { 3 } else { 1 }; // opening quote(s)
                                             // A String is NUL-terminated natively, so it cannot
                                             // hold a NUL. Refuse one at both entrances: a raw NUL
                                             // byte and `\u{0}` (there is no `\0` escape).
            let nul = |line: usize| {
                Diagnostic::error(
                    line,
                    start_col,
                    "lex",
                    "string literal contains a NUL byte; a Vyrn String is NUL-terminated and \
                     cannot hold one"
                        .to_string(),
                )
            };
            let mut parts: Vec<String> = Vec::new();
            let mut exprs: Vec<Hole> = Vec::new();
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
                if ch == '\0' {
                    return Err(nul(line));
                }
                if ch == '"' {
                    if triple {
                        // Only a run of three closes a triple-quoted string; a lone
                        // `"` or `""` is literal text.
                        if i + 2 < chars.len() && chars[i + 1] == '"' && chars[i + 2] == '"' {
                            i += 3; // closing `"""`
                            break;
                        }
                        cur.push('"');
                        i += 1;
                        continue;
                    }
                    i += 1; // closing quote
                    break;
                }
                // Normalize CRLF to LF inside a literal, so a multi-line string has the same
                // bytes on every checkout. The raw text keeps the CR for the formatter.
                if ch == '\r' && i + 1 < chars.len() && chars[i + 1] == '\n' {
                    i += 1; // drop the CR; the LF is handled next iteration
                    continue;
                }
                // A raw newline is part of the string; keep counting lines.
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
                    // `\{` opens a hole: scan its raw source to the matching `}`.
                    if chars[i + 1] == '{' {
                        parts.push(std::mem::take(&mut cur));
                        i += 2; // skip `\{`
                        let hole = i;
                        let (hole_line, hole_col) = (line, i - line_start + 1);
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
                                    // Skip a nested string and its escapes. Its raw newlines count
                                    // lines as the `\n` arm does.
                                    i += 1;
                                    while i < chars.len() && chars[i] != '"' {
                                        if chars[i] == '\n' {
                                            line += 1;
                                            i += 1;
                                            line_start = i;
                                        } else {
                                            i += if chars[i] == '\\' { 2 } else { 1 };
                                        }
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
                                // A `}` in a nested byte literal (`'}'`) does not close the hole.
                                '\'' => {
                                    i += 1;
                                    while i < chars.len() && chars[i] != '\'' && chars[i] != '\n' {
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
                                // A `}` in a `//` comment is text. The `\n` stays for the newline
                                // arm.
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
                        let hole_src: String = chars[hole..i].iter().collect();
                        if hole_src.trim().is_empty() {
                            return Err(Diagnostic::error(
                                line,
                                start_col,
                                "lex",
                                "empty `\\{ }` interpolation".to_string(),
                            ));
                        }
                        exprs.push(Hole {
                            src: hole_src,
                            line: hole_line,
                            col: hole_col,
                        });
                        i += 1; // skip closing `}`
                        continue;
                    }
                    // `\u{XXXX}`: a Unicode scalar by hex code point.
                    if chars[i + 1] == 'u' {
                        let (ch, next) = parse_unicode_escape(&chars, i, line, start_col)?;
                        // `\u{0}` is the same NUL spelled another way.
                        if ch == '\0' {
                            return Err(nul(line));
                        }
                        cur.push(ch);
                        i = next;
                        continue;
                    }
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
            // Anchor the token at the opening quote, so a diagnostic on a multi-line
            // string points where it starts.
            let tok = if exprs.is_empty() {
                Tok::Str(cur)
            } else {
                parts.push(cur); // the trailing fragment after the last hole
                Tok::TemplateStr { parts, exprs }
            };
            out.push(Triv {
                kind: TrivKind::Tok(tok),
                text: chars[start..i].iter().collect(),
                start_line,
                col: start_col,
                end_line: line,
                space_before,
            });
            space_before = false;
            continue;
        }

        // A byte literal: one ASCII byte, an integer literal that defaults to `UInt8`.
        // Vyrn has no char type.
        if c == '\'' {
            let (val, next) = lex_byte_literal(&chars, i, line, col)?;
            let text: String = chars[start..next].iter().collect();
            i = next; // past the closing quote
            out.push(Triv {
                kind: TrivKind::Tok(Tok::Byte(val)),
                text,
                start_line,
                col,
                end_line: line,
                space_before,
            });
            space_before = false;
            continue;
        }

        if c.is_ascii_digit() {
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
            }
            let text: String = chars[start..i].iter().collect();
            let tok = if is_float {
                let value: f64 = text.parse().map_err(|_| {
                    Diagnostic::error(line, col, "lex", format!("invalid float literal: {text}"))
                })?;
                Tok::Float(value)
            } else {
                // A literal above `i64::MAX`, reachable only for `UInt64`, keeps its `u64` bit
                // pattern.
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
                Tok::Int(value)
            };
            out.push(Triv {
                kind: TrivKind::Tok(tok),
                text,
                start_line,
                col,
                end_line: line,
                space_before,
            });
            space_before = false;
            continue;
        }

        if c.is_alphabetic() || c == '_' {
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            let tok = keyword_or_ident(&text);
            out.push(Triv {
                kind: TrivKind::Tok(tok),
                text,
                start_line,
                col,
                end_line: line,
                space_before,
            });
            space_before = false;
            continue;
        }

        // Two-character operators first: maximal munch.
        let two = i
            .checked_add(1)
            .filter(|&j| j < chars.len())
            .and_then(|j| two_char_op(c, chars[j]));
        let (tok, width) = match two {
            Some(tok) => (tok, 2usize),
            None => match single_char_op(c) {
                Some(tok) => (tok, 1usize),
                None => {
                    return Err(Diagnostic::error(
                        line,
                        col,
                        "lex",
                        format!("unexpected character {c:?}"),
                    ))
                }
            },
        };
        i += width;
        out.push(Triv {
            kind: TrivKind::Tok(tok),
            text: chars[start..i].iter().collect(),
            start_line,
            col,
            end_line: line,
            space_before,
        });
        space_before = false;
    }

    Ok(Scan {
        items: out,
        end_line: line,
        end_col: i - line_start + 1,
    })
}

/// Returns every item [`scan`] finds, comments included, for the formatter.
pub fn lex_with_trivia(src: &str) -> Result<Vec<Triv>, Diagnostic> {
    Ok(scan(src)?.items)
}

/// Parses a `\u{HEX}` escape whose backslash is `chars[at]`. Returns the
/// character and the index past the closing `}`.
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
    let cp = u32::from_str_radix(hex.trim(), 16).map_err(|_| err("`\\u{}` needs hex digits"))?;
    let ch = char::from_u32(cp).ok_or_else(|| err("invalid Unicode scalar in `\\u{}`"))?;
    Ok((ch, j + 1)) // past the closing `}`
}

/// Lexes a byte literal whose opening quote is `chars[start]`. Returns the byte
/// and the index past the closing quote.
///
/// The content is one printable ASCII character (`0x20..=0x7E`; `'` and `\`
/// escaped) or one of the escapes `\n \t \r \' \\ \0 \xNN`.
fn lex_byte_literal(
    chars: &[char],
    start: usize,
    line: usize,
    col: usize,
) -> Result<(u8, usize), Diagnostic> {
    let err = |m: &str| Diagnostic::error(line, col, "lex", m.to_string());
    let i = start + 1; // first char of the content (past the opening `'`)
    if i >= chars.len() {
        return Err(err("unterminated byte literal"));
    }
    let (val, after): (u8, usize) = if chars[i] == '\'' {
        return Err(err(
            "empty byte literal; a byte literal holds exactly one byte, e.g. 'a' or '\\x0a'",
        ));
    } else if chars[i] == '\\' {
        if i + 1 >= chars.len() {
            return Err(err("unterminated byte escape"));
        }
        match chars[i + 1] {
            'n' => (0x0A, i + 2),
            't' => (0x09, i + 2),
            'r' => (0x0D, i + 2),
            '0' => (0x00, i + 2),
            '\\' => (0x5C, i + 2),
            '\'' => (0x27, i + 2),
            'x' => {
                if i + 3 >= chars.len() {
                    return Err(err("`\\x` needs two hex digits"));
                }
                match (chars[i + 2].to_digit(16), chars[i + 3].to_digit(16)) {
                    (Some(h), Some(l)) => ((h * 16 + l) as u8, i + 4),
                    _ => return Err(err("`\\x` needs two hex digits")),
                }
            }
            other => return Err(err(&format!("unknown byte escape `\\{other}`"))),
        }
    } else {
        let ch = chars[i];
        if ch == '\n' {
            return Err(err("raw newline in byte literal; write '\\n'"));
        }
        let cp = ch as u32;
        if !(0x20..=0x7E).contains(&cp) {
            return Err(err(
                "byte literal must be a single ASCII byte; write the UTF-8 bytes explicitly",
            ));
        }
        (cp as u8, i + 1)
    };
    if after >= chars.len() {
        return Err(err("unterminated byte literal"));
    }
    if chars[after] != '\'' {
        return Err(err(
            "single-quoted strings are not allowed: '…' is a single byte (e.g. 'a', '\\n', '\\x41'); use \"…\" for text",
        ));
    }
    Ok((val, after + 1))
}

/// Tokenizes `src`: [`scan`]'s items without comments, closed by `Eof`. Returns
/// an error diagnostic for the first malformed token.
pub fn lex(src: &str) -> Result<Vec<Token>, Diagnostic> {
    let scanned = scan(src)?;
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
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

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
