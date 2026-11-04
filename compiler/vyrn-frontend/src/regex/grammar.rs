// Written by vyrn_syntax::bytes from std/regex.syntax. Do not edit: the test
// the_regex_parser_is_fresh fails when this file differs from a fresh run, and
// VYRN_PIN=write rewrites it.

//! The rules of `std/regex.syntax` as Rust: one method of [`Scanner`] per rule, and the
//! rows they refuse with. A rule that matches calls its hook in [`Hooks`].

/// A row a rule refuses with, and the text of each of its holes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    ClassBackslash {  },
    ClassUnclosed {  },
    CountForm {  },
    CountNumber {  },
    CountUnclosed {  },
    GroupUnclosed {  },
    NoCountTerm {  },
    NoTerm {  },
    NonGreedy { op: String },
    PatternBackslash {  },
    StrayBrace {  },
    StrayParen {  },
    TooDeep {  },
}

macro_rules! rule {
    ($row:ident $(, $hole:ident = $text:expr)*) => {
        Rule::$row { $($hole: $text.to_string()),* }
    };
}

/// What the host does when a rule matches `start..end`, a byte range of the
/// source, and the refusal it answers with for a row.
#[allow(unused_variables)]
pub(super) trait Hooks {
    fn refuse(&mut self, at: usize, rule: Rule) -> String;
    fn _regex_pattern(&mut self, start: usize, end: usize) -> Result<(), String> {
        Ok(())
    }
    fn _regex_alt(&mut self, start: usize, end: usize) -> Result<(), String> {
        Ok(())
    }
    fn _regex_branch(&mut self, start: usize, end: usize) -> Result<(), String> {
        Ok(())
    }
    fn _regex_piece(&mut self, start: usize, end: usize) -> Result<(), String> {
        Ok(())
    }
    fn _regex_count(&mut self, start: usize, end: usize) -> Result<(), String> {
        Ok(())
    }
    fn _regex_atom(&mut self, start: usize, end: usize) -> Result<(), String> {
        Ok(())
    }
    fn _regex_group(&mut self, start: usize, end: usize) -> Result<(), String> {
        Ok(())
    }
    fn _regex_escape(&mut self, start: usize, end: usize) -> Result<(), String> {
        Ok(())
    }
    fn _regex_dot(&mut self, start: usize, end: usize) -> Result<(), String> {
        Ok(())
    }
    fn _regex_literal(&mut self, start: usize, end: usize) -> Result<(), String> {
        Ok(())
    }
    fn _regex_class(&mut self, start: usize, end: usize) -> Result<(), String> {
        Ok(())
    }
    fn _regex_item(&mut self, start: usize, end: usize) -> Result<(), String> {
        Ok(())
    }
    fn _regex_item_escape(&mut self, start: usize, end: usize) -> Result<(), String> {
        Ok(())
    }
    fn _regex_member(&mut self, start: usize, end: usize) -> Result<(), String> {
        Ok(())
    }
    fn regex_char(&mut self, start: usize, end: usize) -> Result<(), String> {
        Ok(())
    }
    fn regex_hi(&mut self, start: usize, end: usize) -> Result<(), String> {
        Ok(())
    }
    fn regex_negate(&mut self, start: usize, end: usize) -> Result<(), String> {
        Ok(())
    }
    fn regex_int(&mut self, start: usize, end: usize) -> Result<(), String> {
        Ok(())
    }
    fn regex_op(&mut self, start: usize, end: usize) -> Result<(), String> {
        Ok(())
    }
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


    pub(super) fn _regex_pattern(&mut self) -> Result<(), String> {
        let start = self.pos;
        { self._regex_alt()?; if !(self.at_eof()) { { let rule = rule!(StrayParen); return Err(self.hooks.refuse(self.pos, rule)); } }  }
        self.hooks._regex_pattern(start, self.pos)
    }

    fn _regex_alt(&mut self) -> Result<(), String> {
        let start = self.pos;
        if self.depth >= 250 { let rule = rule!(TooDeep); return Err(self.hooks.refuse(self.pos, rule)); }
        self.depth += 1;
        { let branches = self.pos; self._regex_branch()?; loop { let p = self.pos; if !{
            let p0 = self.pos;
            (self.eat_byte(b'|') && { self._regex_branch()?; true })
            || { self.pos = p0; false }
        } || self.pos == p { break; } }  }
        self.depth -= 1;
        self.hooks._regex_alt(start, self.pos)
    }

    fn _regex_branch(&mut self) -> Result<(), String> {
        let start = self.pos;
        { loop { let p = self.pos; if !{
            let p0 = self.pos;
            (!{ let p = self.pos; let m = (
                self.eat_byte(b'|')
                || self.eat_byte(b')')
                || self.at_eof()
            ); self.pos = p; m } && self._regex_piece()?)
            || { self.pos = p0; false }
        } || self.pos == p { break; } } }
        self.hooks._regex_branch(start, self.pos)
    }

    fn _regex_piece(&mut self) -> Result<bool, String> {
        let start = self.pos;
        let hit = {
            let p0 = self.pos;
            ({ let atom = self.pos; self._regex_atom()? && { let _ = (
                self.regex_op()?
                || self._regex_count()?
            ); true } })
            || { self.pos = p0; false }
        };
        if !hit {
            return Ok(false);
        }
        self.hooks._regex_piece(start, self.pos)?;
        Ok(true)
    }

    fn _regex_count(&mut self) -> Result<bool, String> {
        let start = self.pos;
        let hit = {
            let p0 = self.pos;
            (self.eat_byte(b'{') && { if !(self.regex_int()?) { { let rule = rule!(CountNumber); return Err(self.hooks.refuse(self.pos, rule)); } } true } && { if !(self.eat_byte(b'}') || {
                    let p0 = self.pos;
                    (self.eat_byte(b',') && { if !(self.eat_byte(b'}')) { if !(self.regex_int()?) { { let rule = rule!(CountNumber); return Err(self.hooks.refuse(self.pos, rule)); } } if !(self.eat_byte(b'}')) { { let rule = rule!(CountUnclosed); return Err(self.hooks.refuse(self.pos, rule)); } }  } true })
                    || { self.pos = p0; false }
                }) { { let rule = rule!(CountForm); return Err(self.hooks.refuse(self.pos, rule)); } } true })
            || { self.pos = p0; false }
        };
        if !hit {
            return Ok(false);
        }
        self.hooks._regex_count(start, self.pos)?;
        Ok(true)
    }

    fn _regex_atom(&mut self) -> Result<bool, String> {
        let start = self.pos;
        let hit = self._regex_group()?
            || self._regex_class()?
            || self._regex_escape()?
            || self._regex_dot()?
            || {
                let p0 = self.pos;
                ({ let p = self.pos; let m = self.eat_one(|c| c == '*' || c == '+' || c == '?'); self.pos = p; m } && { let rule = rule!(NoTerm); return Err(self.hooks.refuse(self.pos, rule)); })
                || { self.pos = p0; false }
            }
            || {
                let p0 = self.pos;
                ({ let p = self.pos; let m = self.eat_byte(b'{'); self.pos = p; m } && { let rule = rule!(NoCountTerm); return Err(self.hooks.refuse(self.pos, rule)); })
                || { self.pos = p0; false }
            }
            || {
                let p0 = self.pos;
                ({ let p = self.pos; let m = self.eat_byte(b'}'); self.pos = p; m } && { let rule = rule!(StrayBrace); return Err(self.hooks.refuse(self.pos, rule)); })
                || { self.pos = p0; false }
            }
            || self._regex_literal()?;
        if !hit {
            return Ok(false);
        }
        self.hooks._regex_atom(start, self.pos)?;
        Ok(true)
    }

    fn _regex_group(&mut self) -> Result<bool, String> {
        let start = self.pos;
        let hit = {
            let p0 = self.pos;
            (self.eat_byte(b'(') && { let alt = self.pos; ({ self._regex_alt()?; true }) && { if !(self.eat_byte(b')')) { { let rule = rule!(GroupUnclosed); return Err(self.hooks.refuse(self.pos, rule)); } } true } })
            || { self.pos = p0; false }
        };
        if !hit {
            return Ok(false);
        }
        self.hooks._regex_group(start, self.pos)?;
        Ok(true)
    }

    fn _regex_escape(&mut self) -> Result<bool, String> {
        let start = self.pos;
        let hit = {
            let p0 = self.pos;
            (self.eat_byte(b'\\') && { if !(self.regex_char()?) { { let rule = rule!(PatternBackslash); return Err(self.hooks.refuse(self.pos, rule)); } } true })
            || { self.pos = p0; false }
        };
        if !hit {
            return Ok(false);
        }
        self.hooks._regex_escape(start, self.pos)?;
        Ok(true)
    }

    fn _regex_dot(&mut self) -> Result<bool, String> {
        let start = self.pos;
        let hit = self.eat_byte(b'.');
        if !hit {
            return Ok(false);
        }
        self.hooks._regex_dot(start, self.pos)?;
        Ok(true)
    }

    fn _regex_literal(&mut self) -> Result<bool, String> {
        let start = self.pos;
        let hit = self.regex_char()?;
        if !hit {
            return Ok(false);
        }
        self.hooks._regex_literal(start, self.pos)?;
        Ok(true)
    }

    fn _regex_class(&mut self) -> Result<bool, String> {
        let start = self.pos;
        let hit = {
            let p0 = self.pos;
            (self.eat_byte(b'[') && { let negate = self.pos; ({ let _ = self.regex_negate()?; true }) && { loop { let p = self.pos; if !{
                let p0 = self.pos;
                (!{ let p = self.pos; let m = (
                    self.eat_byte(b']')
                    || self.at_eof()
                ); self.pos = p; m } && self._regex_item()?)
                || { self.pos = p0; false }
            } || self.pos == p { break; } } true } && { if !(self.eat_byte(b']')) { { let rule = rule!(ClassUnclosed); return Err(self.hooks.refuse(self.pos, rule)); } } true } })
            || { self.pos = p0; false }
        };
        if !hit {
            return Ok(false);
        }
        self.hooks._regex_class(start, self.pos)?;
        Ok(true)
    }

    fn _regex_item(&mut self) -> Result<bool, String> {
        let start = self.pos;
        let hit = self._regex_item_escape()?
            || self._regex_member()?;
        if !hit {
            return Ok(false);
        }
        self.hooks._regex_item(start, self.pos)?;
        Ok(true)
    }

    fn _regex_item_escape(&mut self) -> Result<bool, String> {
        let start = self.pos;
        let hit = {
            let p0 = self.pos;
            (self.eat_byte(b'\\') && { if !(self.regex_char()?) { { let rule = rule!(ClassBackslash); return Err(self.hooks.refuse(self.pos, rule)); } } true })
            || { self.pos = p0; false }
        };
        if !hit {
            return Ok(false);
        }
        self.hooks._regex_item_escape(start, self.pos)?;
        Ok(true)
    }

    fn _regex_member(&mut self) -> Result<bool, String> {
        let start = self.pos;
        let hit = {
            let p0 = self.pos;
            (self.regex_char()? && { let _ = self.regex_hi()?; true })
            || { self.pos = p0; false }
        };
        if !hit {
            return Ok(false);
        }
        self.hooks._regex_member(start, self.pos)?;
        Ok(true)
    }

    fn regex_char(&mut self) -> Result<bool, String> {
        let start = self.pos;
        let hit = self.eat_one(|_| true);
        if !hit {
            return Ok(false);
        }
        self.hooks.regex_char(start, self.pos)?;
        Ok(true)
    }

    fn regex_hi(&mut self) -> Result<bool, String> {
        let start = self.pos;
        let hit = {
            let p0 = self.pos;
            (self.eat_byte(b'-') && !{ let p = self.pos; let m = self.eat_byte(b']'); self.pos = p; m } && self.eat_one(|_| true))
            || { self.pos = p0; false }
        };
        if !hit {
            return Ok(false);
        }
        self.hooks.regex_hi(start, self.pos)?;
        Ok(true)
    }

    fn regex_negate(&mut self) -> Result<bool, String> {
        let start = self.pos;
        let hit = self.eat_byte(b'^');
        if !hit {
            return Ok(false);
        }
        self.hooks.regex_negate(start, self.pos)?;
        Ok(true)
    }

    fn regex_int(&mut self) -> Result<bool, String> {
        let start = self.pos;
        let hit = (self.eat_one(|c| matches!(c, '0'..='9')) && { self.eat_while(|c| matches!(c, '0'..='9')); true });
        if !hit {
            return Ok(false);
        }
        self.hooks.regex_int(start, self.pos)?;
        Ok(true)
    }

    fn regex_op(&mut self) -> Result<bool, String> {
        let start = self.pos;
        let hit = (matches!(self.peek_byte(), Some(b'*')) && {
                let p0 = self.pos;
                (self.eat_byte(b'*') && { let _ = {
                    let p0 = self.pos;
                    ({ let p = self.pos; let m = self.eat_byte(b'?'); self.pos = p; m } && { let rule = rule!(NonGreedy, op = "*"); return Err(self.hooks.refuse(self.pos, rule)); })
                    || { self.pos = p0; false }
                }; true })
                || { self.pos = p0; false }
            })
            || (matches!(self.peek_byte(), Some(b'+')) && {
                let p0 = self.pos;
                (self.eat_byte(b'+') && { let _ = {
                    let p0 = self.pos;
                    ({ let p = self.pos; let m = self.eat_byte(b'?'); self.pos = p; m } && { let rule = rule!(NonGreedy, op = "+"); return Err(self.hooks.refuse(self.pos, rule)); })
                    || { self.pos = p0; false }
                }; true })
                || { self.pos = p0; false }
            })
            || (matches!(self.peek_byte(), Some(b'?')) && self.eat_byte(b'?'));
        if !hit {
            return Ok(false);
        }
        self.hooks.regex_op(start, self.pos)?;
        Ok(true)
    }
}
