//! Hand-written lexer for the Vyrn v0 subset.

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
    TemplateStr {
        parts: Vec<String>,
        exprs: Vec<String>,
    },
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
    /// `import` — bring exported names from another module into scope.
    Import,
    /// `export` — mark a top-level declaration importable (RFC-0010).
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
    Eq,         // =
    EqEq,       // ==
    TildeMatch, // =~
    NotEq,      // !=
    Lt,         // <
    LtEq,       // <=
    Gt,         // >
    GtEq,       // >=
    AndAnd,     // &&
    OrOr,       // ||
    Bang,       // !
    Question,   // ?
    Pipe,       // |
    Amp,        // &
    Caret,      // ^  (bitwise xor, RFC-0045)
    Tilde,      // ~  (bitwise complement, RFC-0045)
    // Shift tokens (RFC-0045). Lexed greedily from `<<`/`>>` in every position;
    // in *type* position a `>>` is split back into two `>` by the parser's
    // generic-closing `eat` (mirroring the `>=` split) — so `Array<Array<T>>`
    // still parses while `a >> b` shifts.
    Shl, // <<
    Shr, // >>

    Eof,
}

/// A token plus the 1-based line and column it appeared on (for diagnostics).
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub line: usize,
    pub col: usize,
}

/// The canonical `(kind, text)` pair for a token, exposed to generators through
/// the `lex()` builtin (RFC-0054). `kind` is a stable category name; `text` is
/// the token's source spelling (a literal's decoded value, a keyword/punct's
/// spelling). `Eof` is included for completeness but callers filter it out.
pub fn token_name_and_text(tok: &Tok) -> (String, String) {
    let kw = |s: &str| ("keyword".to_string(), s.to_string());
    let p = |s: &str| ("punct".to_string(), s.to_string());
    match tok {
        Tok::Int(n) => ("int".to_string(), n.to_string()),
        Tok::Float(f) => ("float".to_string(), format!("{f:?}")),
        Tok::Str(s) => ("string".to_string(), s.clone()),
        Tok::TemplateStr { parts, .. } => ("template".to_string(), parts.join("")),
        Tok::Doc(s) => ("doc".to_string(), s.clone()),
        Tok::Ident(s) => ("ident".to_string(), s.clone()),
        Tok::Fn => kw("fn"),
        Tok::Let => kw("let"),
        Tok::Mut => kw("mut"),
        Tok::If => kw("if"),
        Tok::Else => kw("else"),
        Tok::While => kw("while"),
        Tok::For => kw("for"),
        Tok::In => kw("in"),
        Tok::Drop => kw("drop"),
        Tok::Protocol => kw("protocol"),
        Tok::Import => kw("import"),
        Tok::Export => kw("export"),
        Tok::Impl => kw("impl"),
        Tok::Vself => kw("self"),
        Tok::Return => kw("return"),
        Tok::True => kw("true"),
        Tok::False => kw("false"),
        Tok::Type => kw("type"),
        Tok::Where => kw("where"),
        Tok::Match => kw("match"),
        Tok::Region => kw("region"),
        Tok::Spawn => kw("spawn"),
        Tok::LParen => p("("),
        Tok::RParen => p(")"),
        Tok::LBrace => p("{"),
        Tok::RBrace => p("}"),
        Tok::LBracket => p("["),
        Tok::RBracket => p("]"),
        Tok::Comma => p(","),
        Tok::Semi => p(";"),
        Tok::Colon => p(":"),
        Tok::Dot => p("."),
        Tok::Arrow => p("->"),
        Tok::FatArrow => p("=>"),
        Tok::Plus => p("+"),
        Tok::Minus => p("-"),
        Tok::Star => p("*"),
        Tok::Slash => p("/"),
        Tok::Percent => p("%"),
        Tok::Eq => p("="),
        Tok::EqEq => p("=="),
        Tok::TildeMatch => p("=~"),
        Tok::NotEq => p("!="),
        Tok::Lt => p("<"),
        Tok::LtEq => p("<="),
        Tok::Gt => p(">"),
        Tok::GtEq => p(">="),
        Tok::AndAnd => p("&&"),
        Tok::OrOr => p("||"),
        Tok::Bang => p("!"),
        Tok::Question => p("?"),
        Tok::Pipe => p("|"),
        Tok::Amp => p("&"),
        Tok::Caret => p("^"),
        Tok::Tilde => p("~"),
        Tok::Shl => p("<<"),
        Tok::Shr => p(">>"),
        Tok::Eof => ("eof".to_string(), String::new()),
    }
}

/// A lexical *item* for the formatter (RFC-0017): a token or a comment, carrying
/// its **raw source text** (verbatim) and the source lines it spans. This is the
/// additive, comment-preserving view of the token stream that [`lex_with_trivia`]
/// produces; the normal [`lex`] and its callers are untouched.
///
/// The formatter prints raw `text` for every item and only ever changes the
/// whitespace *between* items — so a literal (string, char, number) can never be
/// re-escaped or mangled, which is what makes the safety invariant cheap.
#[derive(Debug, Clone)]
pub struct Triv {
    pub kind: TrivKind,
    /// Raw source slice, verbatim. For comments/docs it includes the leading
    /// slashes; trailing whitespace/CR is stripped.
    pub text: String,
    /// 1-based line the item starts on.
    pub start_line: usize,
    /// 1-based line the item ends on (equal to `start_line` except for multi-line
    /// string literals, whose internal newlines are part of the token).
    pub end_line: usize,
    /// Whether whitespace, a newline, or a comment separated this item from the
    /// previous token in the source. Drives the `<`/`>` generic-vs-comparison
    /// disambiguation (a generic bracket is *tight* against its neighbours).
    pub space_before: bool,
}

/// What a [`Triv`] item is.
#[derive(Debug, Clone, PartialEq)]
pub enum TrivKind {
    /// A real token. The payload is the token *kind* (used for spacing
    /// decisions); its inner value is a dummy for literals — the raw `text` is
    /// what gets printed, never a re-synthesized spelling.
    Tok(Tok),
    /// A `//` line comment (including `////+` plain comments).
    Comment,
    /// A `///` documentation line.
    Doc,
}

/// Map an identifier's text to its keyword token, or [`Tok::Ident`]. Shared
/// spelling table so [`lex`] and [`lex_with_trivia`] agree exactly.
fn keyword_or_ident(text: &str) -> Tok {
    match text {
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
        "spawn" => Tok::Spawn,
        _ => Tok::Ident(text.to_string()),
    }
}

/// Two-character operator table (returns `None` if `(a, b)` is not one).
fn two_char_op(a: char, b: char) -> Option<Tok> {
    match (a, b) {
        ('-', '>') => Some(Tok::Arrow),
        ('=', '>') => Some(Tok::FatArrow),
        ('=', '~') => Some(Tok::TildeMatch),
        ('=', '=') => Some(Tok::EqEq),
        ('!', '=') => Some(Tok::NotEq),
        ('<', '=') => Some(Tok::LtEq),
        ('>', '=') => Some(Tok::GtEq),
        ('&', '&') => Some(Tok::AndAnd),
        ('|', '|') => Some(Tok::OrOr),
        ('<', '<') => Some(Tok::Shl),
        ('>', '>') => Some(Tok::Shr),
        _ => None,
    }
}

/// Single-character operator/punctuation table.
fn single_char_op(c: char) -> Option<Tok> {
    Some(match c {
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
        '^' => Tok::Caret,
        '~' => Tok::Tilde,
        _ => return None,
    })
}

/// Comment-preserving tokenizer (RFC-0017). Yields the same tokens as [`lex`]
/// **plus** `//` comments and blank-line structure (via each item's raw text and
/// line span), so the formatter can reconstruct the source faithfully.
///
/// Token *kinds* are accurate (keywords vs identifiers, each operator), but
/// literal payloads are dummy — the raw `text` slice is authoritative and is all
/// the printer ever emits. Boundaries mirror [`lex`] exactly (strings, char
/// literals, `\{..}` interpolation holes, `///` docs vs `//`/`////+` comments),
/// so re-lexing the printer's output with [`lex`] round-trips modulo semicolons.
pub fn lex_with_trivia(src: &str) -> Result<Vec<Triv>, Diagnostic> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0usize;
    let mut line = 1usize;
    let mut out: Vec<Triv> = Vec::new();
    // No token precedes the first one; the file start counts as "space before".
    let mut space_before = true;

    // Trim a comment/doc slice: strip a trailing CR and any trailing blanks
    // (verbatim content otherwise, so no-trailing-whitespace holds).
    let trim_comment = |s: &[char]| -> String {
        let mut t: String = s.iter().collect();
        while t.ends_with('\r') || t.ends_with(' ') || t.ends_with('\t') {
            t.pop();
        }
        t
    };

    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            i += 1;
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

        // `//` comment or `///` doc (`////+` is a plain comment, like `lex`).
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            let is_doc = i + 2 < chars.len()
                && chars[i + 2] == '/'
                && !(i + 3 < chars.len() && chars[i + 3] == '/');
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            out.push(Triv {
                kind: if is_doc {
                    TrivKind::Doc
                } else {
                    TrivKind::Comment
                },
                text: trim_comment(&chars[start..i]),
                start_line,
                end_line: start_line,
                space_before,
            });
            space_before = true;
            continue;
        }

        // string / interpolated string — one item spanning any internal newlines.
        if c == '"' {
            i += 1;
            loop {
                if i >= chars.len() {
                    return Err(Diagnostic::error(
                        line,
                        0,
                        "lex",
                        "unterminated string literal".into(),
                    ));
                }
                let ch = chars[i];
                if ch == '"' {
                    i += 1;
                    break;
                }
                if ch == '\n' {
                    line += 1;
                    i += 1;
                    continue;
                }
                if ch == '\\' {
                    if i + 1 >= chars.len() {
                        return Err(Diagnostic::error(
                            line,
                            0,
                            "lex",
                            "unterminated escape in string".into(),
                        ));
                    }
                    if chars[i + 1] == '{' {
                        // interpolation hole: scan raw to matching `}` (mirroring
                        // `lex` — nested strings, char literals, comments, braces).
                        i += 2;
                        let mut depth = 1usize;
                        while i < chars.len() && depth > 0 {
                            match chars[i] {
                                '\n' => {
                                    line += 1;
                                    i += 1;
                                }
                                '"' => {
                                    i += 1;
                                    while i < chars.len() && chars[i] != '"' {
                                        if chars[i] == '\n' {
                                            line += 1;
                                        }
                                        i += if chars[i] == '\\' { 2 } else { 1 };
                                    }
                                    if i >= chars.len() {
                                        return Err(Diagnostic::error(
                                            line,
                                            0,
                                            "lex",
                                            "unterminated string in interpolation".into(),
                                        ));
                                    }
                                    i += 1;
                                }
                                '\'' => {
                                    i += 1;
                                    while i < chars.len() && chars[i] != '\'' && chars[i] != '\n' {
                                        i += if chars[i] == '\\' { 2 } else { 1 };
                                    }
                                    if i >= chars.len() || chars[i] == '\n' {
                                        return Err(Diagnostic::error(
                                            line,
                                            0,
                                            "lex",
                                            "unterminated character literal in interpolation"
                                                .into(),
                                        ));
                                    }
                                    i += 1;
                                }
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
                                0,
                                "lex",
                                "unterminated `\\{` interpolation".into(),
                            ));
                        }
                        i += 1; // closing `}`
                        continue;
                    }
                    // Any other escape: skip the backslash and its next char. `\u{..}`
                    // leaves `{HEX}` as ordinary string chars (harmless here — only
                    // `\{` opens a hole), matching `lex`.
                    i += 2;
                    continue;
                }
                i += 1;
            }
            out.push(Triv {
                kind: TrivKind::Tok(Tok::Str(String::new())),
                text: chars[start..i].iter().collect(),
                start_line,
                end_line: line,
                space_before,
            });
            space_before = false;
            continue;
        }

        // character literal `'a'` / `'\n'` / `'\u{HEX}'` — a single Int token.
        if c == '\'' {
            i += 1;
            if i < chars.len() && chars[i] == '\\' {
                if i + 1 < chars.len() && chars[i + 1] == 'u' {
                    i += 2;
                    while i < chars.len() && chars[i] != '}' && chars[i] != '\n' {
                        i += 1;
                    }
                    if i < chars.len() && chars[i] == '}' {
                        i += 1;
                    }
                } else {
                    i += 2; // `\x`
                }
            } else if i < chars.len() && chars[i] != '\n' {
                i += 1; // any single scalar
            }
            if i < chars.len() && chars[i] == '\'' {
                i += 1;
            } else {
                return Err(Diagnostic::error(
                    start_line,
                    0,
                    "lex",
                    "unterminated character literal".into(),
                ));
            }
            out.push(Triv {
                kind: TrivKind::Tok(Tok::Int(0)),
                text: chars[start..i].iter().collect(),
                start_line,
                end_line: line,
                space_before,
            });
            space_before = false;
            continue;
        }

        // number (int or `1.5` float — same rule as `lex`).
        if c.is_ascii_digit() {
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let mut kind = Tok::Int(0);
            if i + 1 < chars.len() && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
                i += 1;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                kind = Tok::Float(0.0);
            }
            out.push(Triv {
                kind: TrivKind::Tok(kind),
                text: chars[start..i].iter().collect(),
                start_line,
                end_line: line,
                space_before,
            });
            space_before = false;
            continue;
        }

        // identifier or keyword.
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
                end_line: line,
                space_before,
            });
            space_before = false;
            continue;
        }

        // multi-char then single-char operators.
        if let Some(tok) = i
            .checked_add(1)
            .filter(|&j| j < chars.len())
            .and_then(|j| two_char_op(c, chars[j]))
        {
            i += 2;
            out.push(Triv {
                kind: TrivKind::Tok(tok),
                text: chars[start..i].iter().collect(),
                start_line,
                end_line: line,
                space_before,
            });
            space_before = false;
            continue;
        }
        match single_char_op(c) {
            Some(tok) => {
                i += 1;
                out.push(Triv {
                    kind: TrivKind::Tok(tok),
                    text: chars[start..i].iter().collect(),
                    start_line,
                    end_line: line,
                    space_before,
                });
                space_before = false;
            }
            None => {
                return Err(Diagnostic::error(
                    line,
                    0,
                    "lex",
                    format!("unexpected character {c:?}"),
                ));
            }
        }
    }

    Ok(out)
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
    let cp = u32::from_str_radix(hex.trim(), 16).map_err(|_| err("`\\u{}` needs hex digits"))?;
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
                out.push(Token {
                    tok: Tok::Doc(text),
                    line,
                    col,
                });
            }
            continue;
        }

        // string literal: "..." with \n \t \\ \" escapes, and `\{ expr }`
        // interpolation holes (RFC-0007). A hole leaves `{`/`}` as ordinary
        // characters, so only `\{` opens interpolation.
        if c == '"' {
            let start_col = col; // column of the opening quote
            let start_line = line; // a multi-line string is anchored at its start
            // A `"""…"""` triple-quoted string (RFC-0054): inside it a single `"`
            // and `""` are ordinary characters, so emitted Vyrn code carrying
            // string literals needs no `\"` escaping. `\{` interpolation and every
            // other escape work exactly as in a plain string; only the terminator
            // differs (`"""` instead of `"`).
            let triple = i + 2 < chars.len() && chars[i + 1] == '"' && chars[i + 2] == '"';
            i += if triple { 3 } else { 1 }; // opening quote(s)
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
        // value (code point) as an Int — Vyrn has no distinct char type.
        // Escapes: `\n \t \r \0 \\ \'` and `\u{HEX}`.
        if c == '\'' {
            let start_col = col;
            if i + 1 >= chars.len() {
                return Err(Diagnostic::error(
                    line,
                    start_col,
                    "lex",
                    "unterminated character literal".into(),
                ));
            }
            let (cp, consumed) = if chars[i + 1] == '\\' {
                if i + 2 >= chars.len() {
                    return Err(Diagnostic::error(
                        line,
                        start_col,
                        "lex",
                        "unterminated character escape".into(),
                    ));
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
                return Err(Diagnostic::error(
                    line,
                    start_col,
                    "lex",
                    "unterminated character literal".into(),
                ));
            }
            out.push(Token {
                tok: Tok::Int(cp as i64),
                line,
                col: start_col,
            });
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
                out.push(Token {
                    tok: Tok::Float(value),
                    line,
                    col,
                });
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
                ('<', '<') => Some(Tok::Shl),
                ('>', '>') => Some(Tok::Shr),
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
            '^' => Tok::Caret,
            '~' => Tok::Tilde,
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
        // RFC-0045: `& | ^ ~ << >>` are all tokens; `<<`/`>>` are greedy shift
        // tokens (the type parser splits a `>>` when closing generics).
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
