// Written by vyrn_syntax::lexer from compiler/vyrn-syntax/vyrn.syntax. Do not
// edit: the test the_lexer_is_fresh fails when this file differs from a fresh
// run, and VYRN_PIN=write rewrites it.

//! The lexical rules of `vyrn.syntax` as Rust: one method of [`Scanner`] per
//! rule that `scan` reaches. A method returns `Ok(false)` with the cursor
//! unmoved when its rule does not match, and the host's error when the rule
//! refuses. The host's [`Hooks`] build the tokens.

use crate::diagnostics::Diagnostic;
use crate::lexer::Tok;
use crate::rules::{rule, Rule};

/// What the host does when a lexical rule matches. A rule calls its hook once
/// it has matched `start..end`, a byte range of the source, with the labels it
/// reads from its callers, each the byte where that label matched. The
/// scanner never undoes a match it has reported, and it reports the matches
/// inside a rule before the rule's own.
#[allow(unused_variables)]
pub(super) trait Hooks {
    /// The error that states `rule` at byte `at`.
    fn refuse(&mut self, at: usize, rule: Rule) -> Box<Diagnostic>;
    /// The longest punctuation spelling of the token table matched `start..end`.
    fn punct(&mut self, start: usize, end: usize, tok: Tok) -> Result<(), Box<Diagnostic>> {
        Ok(())
    }
    fn trivia(&mut self, start: usize, end: usize) -> Result<(), Box<Diagnostic>> {
        Ok(())
    }
    fn comment(&mut self, start: usize, end: usize) -> Result<(), Box<Diagnostic>> {
        Ok(())
    }
    fn doc(&mut self, start: usize, end: usize) -> Result<(), Box<Diagnostic>> {
        Ok(())
    }
    fn scan(&mut self, start: usize, end: usize) -> Result<(), Box<Diagnostic>> {
        Ok(())
    }
    fn token(&mut self, start: usize, end: usize) -> Result<(), Box<Diagnostic>> {
        Ok(())
    }
    fn word(&mut self, start: usize, end: usize) -> Result<(), Box<Diagnostic>> {
        Ok(())
    }
    fn number(&mut self, start: usize, end: usize) -> Result<(), Box<Diagnostic>> {
        Ok(())
    }
    fn string(&mut self, start: usize, end: usize) -> Result<(), Box<Diagnostic>> {
        Ok(())
    }
    fn str_item(&mut self, start: usize, end: usize, q: usize) -> Result<(), Box<Diagnostic>> {
        Ok(())
    }
    fn esc(&mut self, start: usize, end: usize, q: usize) -> Result<(), Box<Diagnostic>> {
        Ok(())
    }
    fn uni(&mut self, start: usize, end: usize, q: usize) -> Result<(), Box<Diagnostic>> {
        Ok(())
    }
    fn hole(&mut self, start: usize, end: usize, q: usize) -> Result<(), Box<Diagnostic>> {
        Ok(())
    }
    fn hole_text(&mut self, start: usize, end: usize, q: usize) -> Result<(), Box<Diagnostic>> {
        Ok(())
    }
    fn hole_item(&mut self, start: usize, end: usize, q: usize) -> Result<(), Box<Diagnostic>> {
        Ok(())
    }
    fn byte(&mut self, start: usize, end: usize) -> Result<(), Box<Diagnostic>> {
        Ok(())
    }
    fn byte_body(&mut self, start: usize, end: usize, q: usize) -> Result<(), Box<Diagnostic>> {
        Ok(())
    }
}

/// Returns the reserved word `word` spells, or `None`.
pub(super) fn keyword(word: &str) -> Option<Tok> {
    Some(match word {
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
        "enum" => Tok::Enum,
        "where" => Tok::Where,
        "match" => Tok::Match,
        "region" => Tok::Region,
        "break" => Tok::Break,
        "continue" => Tok::Continue,
        _ => return None,
    })
}

/// A cursor over the source and the hooks it reports to.
pub(super) struct Scanner<'s, H> {
    src: &'s str,
    /// A byte offset on a character boundary.
    pos: usize,
    /// The `nest` rules running.
    depth: u32,
    pub(super) hooks: H,
}

impl<'s, H: Hooks> Scanner<'s, H> {
    pub(super) fn new(src: &'s str, hooks: H) -> Self {
        Scanner { src, pos: 0, depth: 0, hooks }
    }

    fn peek_byte(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos).copied()
    }

    fn at_eof(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn eat_byte(&mut self, b: u8) -> bool {
        let hit = self.peek_byte() == Some(b);
        self.pos += hit as usize;
        hit
    }

    fn eat_lit(&mut self, s: &[u8]) -> bool {
        let hit = self.src.as_bytes()[self.pos..].starts_with(s);
        if hit {
            self.pos += s.len();
        }
        hit
    }

    /// Consumes one character that satisfies `ok`.
    fn eat_one(&mut self, ok: impl Fn(char) -> bool) -> bool {
        let c = match self.peek_byte() {
            None => return false,
            Some(b) if b < 0x80 => b as char,
            Some(_) => match self.src[self.pos..].chars().next() {
                Some(c) => c,
                None => return false,
            },
        };
        let hit = ok(c);
        if hit {
            self.pos += c.len_utf8();
        }
        hit
    }

    /// Consumes characters while they satisfy `ok`.
    fn eat_while(&mut self, ok: impl Fn(char) -> bool) {
        while self.eat_one(&ok) {}
    }

    /// Moves the cursor to the next byte `b`, or to the end.
    fn skip_to(&mut self, b: u8) {
        let rest = &self.src.as_bytes()[self.pos..];
        self.pos += rest.iter().position(|&c| c == b).unwrap_or(rest.len());
    }

    /// The character at the cursor, as Rust's `Debug` spells it if `debug`,
    /// or empty at the end.
    fn char_text(&self, debug: bool) -> String {
        match self.src[self.pos..].chars().next() {
            Some(c) if debug => format!("{c:?}"),
            Some(c) => c.to_string(),
            None => String::new(),
        }
    }

    /// Matches the longest punctuation spelling at the cursor.
    fn punct(&mut self) -> Result<bool, Box<Diagnostic>> {
        let (tok, n) = match &self.src.as_bytes()[self.pos..] {
            [b'-', b'>', ..] => (Tok::Arrow, 2),
            [b'=', b'>', ..] => (Tok::FatArrow, 2),
            [b'=', b'~', ..] => (Tok::TildeMatch, 2),
            [b'=', b'=', ..] => (Tok::EqEq, 2),
            [b'!', b'=', ..] => (Tok::NotEq, 2),
            [b'<', b'=', ..] => (Tok::LtEq, 2),
            [b'>', b'=', ..] => (Tok::GtEq, 2),
            [b'&', b'&', ..] => (Tok::AndAnd, 2),
            [b'|', b'|', ..] => (Tok::OrOr, 2),
            [b'?', b'?', ..] => (Tok::QuestionQuestion, 2),
            [b'<', b'<', ..] => (Tok::Shl, 2),
            [b'>', b'>', ..] => (Tok::Shr, 2),
            [b'(', ..] => (Tok::LParen, 1),
            [b')', ..] => (Tok::RParen, 1),
            [b'{', ..] => (Tok::LBrace, 1),
            [b'}', ..] => (Tok::RBrace, 1),
            [b'[', ..] => (Tok::LBracket, 1),
            [b']', ..] => (Tok::RBracket, 1),
            [b',', ..] => (Tok::Comma, 1),
            [b';', ..] => (Tok::Semi, 1),
            [b':', ..] => (Tok::Colon, 1),
            [b'.', ..] => (Tok::Dot, 1),
            [b'+', ..] => (Tok::Plus, 1),
            [b'-', ..] => (Tok::Minus, 1),
            [b'*', ..] => (Tok::Star, 1),
            [b'/', ..] => (Tok::Slash, 1),
            [b'%', ..] => (Tok::Percent, 1),
            [b'=', ..] => (Tok::Eq, 1),
            [b'<', ..] => (Tok::Lt, 1),
            [b'>', ..] => (Tok::Gt, 1),
            [b'!', ..] => (Tok::Bang, 1),
            [b'?', ..] => (Tok::Question, 1),
            [b'|', ..] => (Tok::Pipe, 1),
            [b'&', ..] => (Tok::Amp, 1),
            [b'^', ..] => (Tok::Caret, 1),
            [b'~', ..] => (Tok::Tilde, 1),
            _ => return Ok(false),
        };
        let start = self.pos;
        self.pos += n;
        self.hooks.punct(start, self.pos, tok)?;
        Ok(true)
    }

    fn trivia(&mut self) -> Result<(), Box<Diagnostic>> {
        let start = self.pos;
        { loop { let p = self.pos; if !(
            self.eat_one(|c| c.is_whitespace())
            || (matches!(self.peek_byte(), Some(b'/')) && self.comment()?)
        ) || self.pos == p { break; } } }
        self.hooks.trivia(start, self.pos)
    }

    fn comment(&mut self) -> Result<bool, Box<Diagnostic>> {
        let start = self.pos;
        let hit = {
            let p0 = self.pos;
            (self.eat_lit(b"//") && !{ let p = self.pos; let m = {
                let p0 = self.pos;
                (self.eat_byte(b'/') && !{ let p = self.pos; let m = self.eat_byte(b'/'); self.pos = p; m })
                || { self.pos = p0; false }
            }; self.pos = p; m } && { self.skip_to(b'\n'); true })
            || { self.pos = p0; false }
        };
        if !hit {
            return Ok(false);
        }
        self.hooks.comment(start, self.pos)?;
        Ok(true)
    }

    fn doc(&mut self) -> Result<bool, Box<Diagnostic>> {
        let start = self.pos;
        let hit = {
            let p0 = self.pos;
            (self.eat_lit(b"//") && self.eat_byte(b'/') && !{ let p = self.pos; let m = self.eat_byte(b'/'); self.pos = p; m } && { self.skip_to(b'\n'); true })
            || { self.pos = p0; false }
        };
        if !hit {
            return Ok(false);
        }
        self.hooks.doc(start, self.pos)?;
        Ok(true)
    }

    pub(super) fn scan(&mut self) -> Result<(), Box<Diagnostic>> {
        let start = self.pos;
        { self.trivia()?; loop { let p = self.pos; if !{
            let p0 = self.pos;
            (self.token()? && { self.trivia()?; true })
            || { self.pos = p0; false }
        } || self.pos == p { break; } } if !(self.at_eof()) { { let rule = rule!(UnexpectedChar, c = self.char_text(true)); return Err(self.hooks.refuse(self.pos, rule)); } }  }
        self.hooks.scan(start, self.pos)
    }

    fn token(&mut self) -> Result<bool, Box<Diagnostic>> {
        let start = self.pos;
        let hit = (matches!(self.peek_byte(), Some(b'\"')) && self.string()?)
            || (matches!(self.peek_byte(), Some(b'\'')) && self.byte()?)
            || (matches!(self.peek_byte(), Some(b'0'..=b'9')) && self.number()?)
            || (matches!(self.peek_byte(), Some(b'A'..=b'Z' | b'_' | b'a'..=b'z' | b'\x80'..=b'\xff')) && self.word()?)
            || (matches!(self.peek_byte(), Some(b'/')) && self.doc()?)
            || self.punct()?;
        if !hit {
            return Ok(false);
        }
        self.hooks.token(start, self.pos)?;
        Ok(true)
    }

    fn word(&mut self) -> Result<bool, Box<Diagnostic>> {
        let start = self.pos;
        let hit = {
            let p0 = self.pos;
            (self.eat_one(|c| c.is_alphabetic() || c == '_') && { self.eat_while(|c| c.is_alphanumeric() || c == '_'); true })
            || { self.pos = p0; false }
        };
        if !hit {
            return Ok(false);
        }
        self.hooks.word(start, self.pos)?;
        Ok(true)
    }

    fn number(&mut self) -> Result<bool, Box<Diagnostic>> {
        let start = self.pos;
        let hit = {
            let p0 = self.pos;
            ((self.eat_one(|c| c.is_ascii_digit()) && { self.eat_while(|c| c.is_ascii_digit()); true }) && { let _ = {
                let p0 = self.pos;
                (self.eat_byte(b'.') && (self.eat_one(|c| c.is_ascii_digit()) && { self.eat_while(|c| c.is_ascii_digit()); true }))
                || { self.pos = p0; false }
            }; true })
            || { self.pos = p0; false }
        };
        if !hit {
            return Ok(false);
        }
        self.hooks.number(start, self.pos)?;
        Ok(true)
    }

    fn string(&mut self) -> Result<bool, Box<Diagnostic>> {
        let start = self.pos;
        let hit = (matches!(self.peek_byte(), Some(b'\"')) && {
                let p0 = self.pos;
                ({ let q = self.pos; self.eat_lit(b"\"\"\"") && { loop { let p = self.pos; if !{
                    let p0 = self.pos;
                    (!{ let p = self.pos; let m = self.eat_lit(b"\"\"\""); self.pos = p; m } && self.str_item(q)?)
                    || { self.pos = p0; false }
                } || self.pos == p { break; } } true } && { if !((matches!(self.peek_byte(), Some(b'\"')) && self.eat_lit(b"\"\"\""))) { { let rule = rule!(UnterminatedString); return Err(self.hooks.refuse(q, rule)); } } true } })
                || { self.pos = p0; false }
            })
            || (matches!(self.peek_byte(), Some(b'\"')) && {
                let p0 = self.pos;
                ({ let q = self.pos; self.eat_byte(b'\"') && { loop { let p = self.pos; if !{
                    let p0 = self.pos;
                    (!{ let p = self.pos; let m = self.eat_byte(b'\"'); self.pos = p; m } && self.str_item(q)?)
                    || { self.pos = p0; false }
                } || self.pos == p { break; } } true } && { if !((matches!(self.peek_byte(), Some(b'\"')) && self.eat_byte(b'\"'))) { { let rule = rule!(UnterminatedString); return Err(self.hooks.refuse(q, rule)); } } true } })
                || { self.pos = p0; false }
            });
        if !hit {
            return Ok(false);
        }
        self.hooks.string(start, self.pos)?;
        Ok(true)
    }

    fn str_item(&mut self, q: usize) -> Result<bool, Box<Diagnostic>> {
        let start = self.pos;
        let hit = (matches!(self.peek_byte(), Some(b'\x00')) && {
                let p0 = self.pos;
                (self.eat_byte(b'\x00') && { let rule = rule!(NulInString); return Err(self.hooks.refuse(q, rule)); })
                || { self.pos = p0; false }
            })
            || (matches!(self.peek_byte(), Some(b'\\')) && {
                let p0 = self.pos;
                (self.eat_byte(b'\\') && { self.esc(q)?; true })
                || { self.pos = p0; false }
            })
            || self.eat_one(|_| true);
        if !hit {
            return Ok(false);
        }
        self.hooks.str_item(start, self.pos, q)?;
        Ok(true)
    }

    fn esc(&mut self, q: usize) -> Result<(), Box<Diagnostic>> {
        let start = self.pos;
        { if !({
                let p0 = self.pos;
                (self.at_eof() && { let rule = rule!(UnterminatedEscape); return Err(self.hooks.refuse(q, rule)); })
                || { self.pos = p0; false }
            } || (matches!(self.peek_byte(), Some(b'{')) && {
                let p0 = self.pos;
                (self.eat_byte(b'{') && { self.hole(q)?; true })
                || { self.pos = p0; false }
            }) || (matches!(self.peek_byte(), Some(b'u')) && {
                let p0 = self.pos;
                (self.eat_byte(b'u') && { self.uni(q)?; true })
                || { self.pos = p0; false }
            }) || self.eat_one(|c| matches!(c, 'n' | 't' | 'r' | '\\' | '"'))) { { let rule = rule!(UnknownEscape, other = self.char_text(false)); return Err(self.hooks.refuse(q, rule)); } } }
        self.hooks.esc(start, self.pos, q)
    }

    fn uni(&mut self, q: usize) -> Result<(), Box<Diagnostic>> {
        let start = self.pos;
        { if !((matches!(self.peek_byte(), Some(b'{')) && self.eat_byte(b'{'))) { { let rule = rule!(UnicodeEscapeBrace); return Err(self.hooks.refuse(q, rule)); } } self.skip_to(b'}'); if !((matches!(self.peek_byte(), Some(b'}')) && self.eat_byte(b'}'))) { { let rule = rule!(UnterminatedUnicodeEscape); return Err(self.hooks.refuse(q, rule)); } }  }
        self.hooks.uni(start, self.pos, q)
    }

    fn hole(&mut self, q: usize) -> Result<(), Box<Diagnostic>> {
        let start = self.pos;
        { if !({
                let p0 = self.pos;
                ({ self.eat_while(|c| c.is_whitespace()); true } && { let p = self.pos; let m = self.eat_byte(b'}'); self.pos = p; m } && { let rule = rule!(EmptyInterp); return Err(self.hooks.refuse(q, rule)); })
                || { self.pos = p0; false }
            }) { self.hole_text(q)?; if !((matches!(self.peek_byte(), Some(b'}')) && self.eat_byte(b'}'))) { { let rule = rule!(UnterminatedInterp); return Err(self.hooks.refuse(q, rule)); } }  } }
        self.hooks.hole(start, self.pos, q)
    }

    fn hole_text(&mut self, q: usize) -> Result<(), Box<Diagnostic>> {
        let start = self.pos;
        if self.depth >= 1024 { let rule = rule!(NestingTooDeep, max = "1024"); return Err(self.hooks.refuse(self.pos, rule)); }
        self.depth += 1;
        { loop { let p = self.pos; if !{
            let p0 = self.pos;
            (!{ let p = self.pos; let m = self.eat_byte(b'}'); self.pos = p; m } && self.hole_item(q)?)
            || { self.pos = p0; false }
        } || self.pos == p { break; } } }
        self.depth -= 1;
        self.hooks.hole_text(start, self.pos, q)
    }

    fn hole_item(&mut self, q: usize) -> Result<bool, Box<Diagnostic>> {
        let start = self.pos;
        let hit = (matches!(self.peek_byte(), Some(b'\"')) && {
                let p0 = self.pos;
                (self.eat_byte(b'\"') && { loop { let p = self.pos; if !{
                    let p0 = self.pos;
                    (!{ let p = self.pos; let m = self.eat_byte(b'\"'); self.pos = p; m } && (
                        (matches!(self.peek_byte(), Some(b'\\')) && {
                            let p0 = self.pos;
                            (self.eat_byte(b'\\') && self.eat_one(|_| true))
                            || { self.pos = p0; false }
                        })
                        || self.eat_one(|_| true)
                    ))
                    || { self.pos = p0; false }
                } || self.pos == p { break; } } true } && { if !((matches!(self.peek_byte(), Some(b'\"')) && self.eat_byte(b'\"'))) { { let rule = rule!(UnterminatedInterpString); return Err(self.hooks.refuse(q, rule)); } } true })
                || { self.pos = p0; false }
            })
            || (matches!(self.peek_byte(), Some(b'\'')) && {
                let p0 = self.pos;
                (self.eat_byte(b'\'') && { loop { let p = self.pos; if !{
                    let p0 = self.pos;
                    (!{ let p = self.pos; let m = self.eat_one(|c| matches!(c, '\'' | '\n')); self.pos = p; m } && (
                        (matches!(self.peek_byte(), Some(b'\\')) && {
                            let p0 = self.pos;
                            (self.eat_byte(b'\\') && self.eat_one(|_| true))
                            || { self.pos = p0; false }
                        })
                        || self.eat_one(|_| true)
                    ))
                    || { self.pos = p0; false }
                } || self.pos == p { break; } } true } && { if !((matches!(self.peek_byte(), Some(b'\'')) && self.eat_byte(b'\''))) { { let rule = rule!(UnterminatedInterpChar); return Err(self.hooks.refuse(q, rule)); } } true })
                || { self.pos = p0; false }
            })
            || (matches!(self.peek_byte(), Some(b'/')) && {
                let p0 = self.pos;
                (self.eat_lit(b"//") && { self.skip_to(b'\n'); true })
                || { self.pos = p0; false }
            })
            || (matches!(self.peek_byte(), Some(b'{')) && {
                let p0 = self.pos;
                (self.eat_byte(b'{') && { self.hole_text(q)?; true } && { let _ = self.eat_byte(b'}'); true })
                || { self.pos = p0; false }
            })
            || self.eat_one(|_| true);
        if !hit {
            return Ok(false);
        }
        self.hooks.hole_item(start, self.pos, q)?;
        Ok(true)
    }

    fn byte(&mut self) -> Result<bool, Box<Diagnostic>> {
        let start = self.pos;
        let hit = {
            let p0 = self.pos;
            ({ let q = self.pos; self.eat_byte(b'\'') && { self.byte_body(q)?; true } && { if !((matches!(self.peek_byte(), Some(b'\'')) && self.eat_byte(b'\'')) || {
                    let p0 = self.pos;
                    (self.at_eof() && { let rule = rule!(UnterminatedByte); return Err(self.hooks.refuse(q, rule)); })
                    || { self.pos = p0; false }
                }) { { let rule = rule!(SingleQuotedString); return Err(self.hooks.refuse(q, rule)); } } true } })
            || { self.pos = p0; false }
        };
        if !hit {
            return Ok(false);
        }
        self.hooks.byte(start, self.pos)?;
        Ok(true)
    }

    fn byte_body(&mut self, q: usize) -> Result<(), Box<Diagnostic>> {
        let start = self.pos;
        { if !({
                let p0 = self.pos;
                (self.at_eof() && { let rule = rule!(UnterminatedByte); return Err(self.hooks.refuse(q, rule)); })
                || { self.pos = p0; false }
            } || (matches!(self.peek_byte(), Some(b'\'')) && {
                let p0 = self.pos;
                (self.eat_byte(b'\'') && { let rule = rule!(EmptyByte); return Err(self.hooks.refuse(q, rule)); })
                || { self.pos = p0; false }
            }) || (matches!(self.peek_byte(), Some(b'\\')) && {
                let p0 = self.pos;
                (self.eat_byte(b'\\') && { if !({
                        let p0 = self.pos;
                        (self.at_eof() && { let rule = rule!(UnterminatedByteEscape); return Err(self.hooks.refuse(q, rule)); })
                        || { self.pos = p0; false }
                    } || self.eat_one(|c| matches!(c, 'n' | 't' | 'r' | '0' | '\\' | '\'')) || (matches!(self.peek_byte(), Some(b'x')) && {
                        let p0 = self.pos;
                        (self.eat_byte(b'x') && { if !((matches!(self.peek_byte(), Some(b'0'..=b'9' | b'A'..=b'F' | b'a'..=b'f')) && {
                                let p0 = self.pos;
                                (self.eat_one(|c| c.is_ascii_hexdigit()) && self.eat_one(|c| c.is_ascii_hexdigit()))
                                || { self.pos = p0; false }
                            })) { { let rule = rule!(ByteHexDigits); return Err(self.hooks.refuse(q, rule)); } } true })
                        || { self.pos = p0; false }
                    })) { { let rule = rule!(UnknownByteEscape, other = self.char_text(false)); return Err(self.hooks.refuse(q, rule)); } } true })
                || { self.pos = p0; false }
            }) || (matches!(self.peek_byte(), Some(b'\n')) && {
                let p0 = self.pos;
                (self.eat_byte(b'\n') && { let rule = rule!(ByteNewline); return Err(self.hooks.refuse(q, rule)); })
                || { self.pos = p0; false }
            }) || self.eat_one(|c| matches!(c, ' '..='~'))) { { let rule = rule!(ByteNotAscii); return Err(self.hooks.refuse(q, rule)); } } }
        self.hooks.byte_body(start, self.pos, q)
    }
}
