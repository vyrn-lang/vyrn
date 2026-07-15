//! A tiny regular-expression engine for `String` refinements (`value =~ "…"`).
//!
//! The supported subset is deliberately small so it can be matched *identically*
//! by the interpreter and the native backend (the sacred `interp == native`
//! invariant): the compiler builds a DFA here once, the interpreter runs it, and
//! the code generator emits the same transition table plus a fixed runner. There
//! is no runtime pattern parsing.
//!
//! Grammar (anchored full match):
//!   alt    = concat ( `|` concat )*
//!   concat = quant*
//!   quant  = atom ( `*` | `+` | `?` | `{m}` | `{m,}` | `{m,n}` )?
//!   atom   = `(` alt `)` | literal | `.` | `\d \w \s \D \W \S` | `\<c>` | `[ … ]`
//!   class  = `[` `^`? ( range | char )+ `]`   (range = `a-z`; reversed = error)
//! Supports alternation `|`, grouping `()` (no backreferences, so the language
//! stays regular and the DFA handles it), and counted repetition expanded
//! structurally (bounds capped at 255 to keep the DFA small). Every accepted
//! construct means the same thing as the equivalent ECMA-262 regex on ASCII —
//! `.` excludes `\n`/`\r` like ECMA — so a pattern round-trips to a JSON
//! Schema `pattern` faithfully. Caveat, documented rather than hidden: the
//! engine is byte-wise, so on multi-byte UTF-8 input `.` and negated classes
//! count bytes, not code points (a code-point-exact engine would need UTF-8
//! range compilation; refinement patterns are ASCII in practice).

/// A set of bytes (256 bits) — a single character class.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteClass([u64; 4]);

impl ByteClass {
    fn empty() -> Self {
        ByteClass([0; 4])
    }
    fn set(&mut self, b: u8) {
        self.0[(b >> 6) as usize] |= 1u64 << (b & 63);
    }
    fn add_range(&mut self, lo: u8, hi: u8) {
        for b in lo..=hi {
            self.set(b);
        }
    }
    fn contains(&self, b: u8) -> bool {
        self.0[(b >> 6) as usize] & (1u64 << (b & 63)) != 0
    }
    fn negated(self) -> Self {
        ByteClass([!self.0[0], !self.0[1], !self.0[2], !self.0[3]])
    }
}

/// A parsed regular expression (regular, so no backreferences).
#[derive(Clone)]
enum Re {
    /// Matches the empty string (an empty group/branch).
    Empty,
    /// A single character from a class.
    Class(ByteClass),
    /// A sequence, matched in order.
    Concat(Vec<Re>),
    /// An alternation (`a|b|c`) — any branch matches.
    Alt(Vec<Re>),
    Star(Box<Re>),
    Plus(Box<Re>),
    Opt(Box<Re>),
}

// ---- parsing: pattern → `Re` (recursive descent) ----------------------------

struct ReParser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> ReParser<'a> {
    fn parse(pat: &str) -> Result<Re, String> {
        let mut p = ReParser { b: pat.as_bytes(), i: 0 };
        let re = p.alt()?;
        if p.i < p.b.len() {
            // Only a stray `)` can be left over (concat stops at `|`/`)`).
            return Err(format!("unmatched `{}` in pattern", p.b[p.i] as char));
        }
        Ok(re)
    }

    fn alt(&mut self) -> Result<Re, String> {
        let mut branches = vec![self.concat()?];
        while self.i < self.b.len() && self.b[self.i] == b'|' {
            self.i += 1;
            branches.push(self.concat()?);
        }
        Ok(if branches.len() == 1 { branches.pop().unwrap() } else { Re::Alt(branches) })
    }

    fn concat(&mut self) -> Result<Re, String> {
        let mut parts = Vec::new();
        while self.i < self.b.len() && self.b[self.i] != b'|' && self.b[self.i] != b')' {
            parts.push(self.quant()?);
        }
        Ok(match parts.len() {
            0 => Re::Empty,
            1 => parts.pop().unwrap(),
            _ => Re::Concat(parts),
        })
    }

    fn quant(&mut self) -> Result<Re, String> {
        let atom = self.atom()?;
        // Read an optional trailing quantifier before moving `atom` into it.
        let q = if self.i < self.b.len() {
            match self.b[self.i] {
                b'*' | b'+' | b'?' => {
                    let c = self.b[self.i];
                    self.i += 1;
                    Some(c)
                }
                b'{' => return self.counted(atom),
                _ => None,
            }
        } else {
            None
        };
        Ok(match q {
            Some(b'*') => Re::Star(Box::new(atom)),
            Some(b'+') => Re::Plus(Box::new(atom)),
            Some(b'?') => Re::Opt(Box::new(atom)),
            _ => atom,
        })
    }

    /// Counted repetition `{m}` / `{m,}` / `{m,n}`, expanded structurally:
    /// `m` mandatory copies, then `n − m` optional ones (or a `*` for an open
    /// upper bound). The DFA grows with `n`, so bounds are capped at 255 —
    /// far above any real wire-format pattern, far below pathological.
    fn counted(&mut self, atom: Re) -> Result<Re, String> {
        self.i += 1; // past `{`
        let m = self.int_in_braces()?;
        let n = match self.b.get(self.i) {
            Some(b'}') => Some(m),
            Some(b',') => {
                self.i += 1;
                match self.b.get(self.i) {
                    Some(b'}') => None, // `{m,}` — open upper bound
                    _ => Some(self.int_in_braces()?),
                }
            }
            _ => return Err("malformed counted repetition (expected `}` or `,`)".into()),
        };
        if self.b.get(self.i) != Some(&b'}') {
            return Err("unclosed counted repetition `{`".into());
        }
        self.i += 1;
        if let Some(n) = n {
            if n < m {
                return Err(format!("counted repetition `{{{m},{n}}}` has max < min"));
            }
        }
        let mut parts: Vec<Re> = std::iter::repeat_with(|| atom.clone()).take(m as usize).collect();
        match n {
            None => parts.push(Re::Star(Box::new(atom))),
            Some(n) => {
                for _ in m..n {
                    parts.push(Re::Opt(Box::new(atom.clone())));
                }
            }
        }
        Ok(match parts.len() {
            0 => Re::Empty,
            1 => parts.pop().unwrap(),
            _ => Re::Concat(parts),
        })
    }

    /// A decimal integer inside `{..}`, capped to keep the expanded DFA small.
    fn int_in_braces(&mut self) -> Result<u32, String> {
        let start = self.i;
        while self.b.get(self.i).is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        if self.i == start {
            return Err("counted repetition needs a number (`{m}`, `{m,}`, `{m,n}`)".into());
        }
        let text = std::str::from_utf8(&self.b[start..self.i]).unwrap();
        let v: u32 = text.parse().map_err(|_| format!("repetition count `{text}` too large"))?;
        if v > 255 {
            return Err(format!("repetition count {v} exceeds the maximum (255)"));
        }
        Ok(v)
    }

    fn atom(&mut self) -> Result<Re, String> {
        match self.b[self.i] {
            b'(' => {
                self.i += 1;
                let inner = self.alt()?;
                if self.i >= self.b.len() || self.b[self.i] != b')' {
                    return Err("unclosed group `(`".into());
                }
                self.i += 1;
                Ok(inner)
            }
            b'*' | b'+' | b'?' => Err("nothing to repeat before quantifier".into()),
            // A quantifier brace with nothing before it (a literal brace is
            // written `\{` / `\}`, as in ECMA-262 strict mode).
            b'{' => Err("nothing to repeat before counted repetition `{..}`".into()),
            b'}' => Err("unmatched `}` (a literal brace is `\\}`)".into()),
            b')' => Err("unmatched `)`".into()),
            _ => {
                let (cl, ni) = parse_atom(self.b, self.i)?;
                self.i = ni;
                Ok(Re::Class(cl))
            }
        }
    }
}

fn parse_atom(b: &[u8], i: usize) -> Result<(ByteClass, usize), String> {
    match b[i] {
        b'\\' => {
            if i + 1 >= b.len() {
                return Err("dangling `\\` in pattern".into());
            }
            Ok((escape_class(b[i + 1]), i + 2))
        }
        // `.` matches any byte except line terminators, matching ECMA-262
        // (LF/CR; ECMA's U+2028/U+2029 are multi-byte in UTF-8 and thus
        // never matched a single `.` here anyway). `[^..]` stays byte-wise.
        b'.' => {
            let mut nl = ByteClass::empty();
            nl.set(b'\n');
            nl.set(b'\r');
            Ok((nl.negated(), i + 1))
        }
        b'[' => parse_class(b, i),
        c => {
            let mut cl = ByteClass::empty();
            cl.set(c);
            Ok((cl, i + 1))
        }
    }
}

/// A `\d`/`\w`/`\s` shorthand (and their negations), or an escaped literal.
fn escape_class(e: u8) -> ByteClass {
    let mut cl = ByteClass::empty();
    match e {
        b'd' => cl.add_range(b'0', b'9'),
        b'D' => {
            cl.add_range(b'0', b'9');
            return cl.negated();
        }
        b'w' => {
            cl.add_range(b'a', b'z');
            cl.add_range(b'A', b'Z');
            cl.add_range(b'0', b'9');
            cl.set(b'_');
        }
        b'W' => {
            cl.add_range(b'a', b'z');
            cl.add_range(b'A', b'Z');
            cl.add_range(b'0', b'9');
            cl.set(b'_');
            return cl.negated();
        }
        b's' => {
            for &w in &[b' ', b'\t', b'\n', b'\r', 0x0b, 0x0c] {
                cl.set(w);
            }
        }
        b'S' => {
            for &w in &[b' ', b'\t', b'\n', b'\r', 0x0b, 0x0c] {
                cl.set(w);
            }
            return cl.negated();
        }
        // Any other escaped byte is that literal (`\.`, `\*`, `\\`, `\[`, …).
        other => cl.set(other),
    }
    cl
}

fn parse_class(b: &[u8], start: usize) -> Result<(ByteClass, usize), String> {
    let mut i = start + 1; // past `[`
    let mut cl = ByteClass::empty();
    let mut negate = false;
    if i < b.len() && b[i] == b'^' {
        negate = true;
        i += 1;
    }
    while i < b.len() && b[i] != b']' {
        // An escape inside a class contributes its class/literal.
        if b[i] == b'\\' {
            if i + 1 >= b.len() {
                return Err("dangling `\\` in character class".into());
            }
            let ec = escape_class(b[i + 1]);
            for byte in 0..=255u8 {
                if ec.contains(byte) {
                    cl.set(byte);
                }
            }
            i += 2;
            continue;
        }
        // A range `a-z` (the `-` must be between two literals, not at the edge).
        if i + 2 < b.len() && b[i + 1] == b'-' && b[i + 2] != b']' {
            if b[i] > b[i + 2] {
                return Err(format!(
                    "reversed range `{}-{}` in character class",
                    b[i] as char, b[i + 2] as char
                ));
            }
            cl.add_range(b[i], b[i + 2]);
            i += 3;
        } else {
            cl.set(b[i]);
            i += 1;
        }
    }
    if i >= b.len() {
        return Err("unterminated character class `[`".into());
    }
    i += 1; // past `]`
    if negate {
        cl = cl.negated();
    }
    Ok((cl, i))
}

// ---- NFA (Thompson construction over the `Re` tree) -------------------------

struct NfaState {
    eps: Vec<usize>,
    edge: Option<(ByteClass, usize)>,
}

struct Nfa {
    states: Vec<NfaState>,
    start: usize,
    accept: usize,
}

impl Nfa {
    fn add(states: &mut Vec<NfaState>) -> usize {
        states.push(NfaState { eps: Vec::new(), edge: None });
        states.len() - 1
    }

    fn build(re: &Re) -> Nfa {
        let mut states: Vec<NfaState> = Vec::new();
        let (start, accept) = Nfa::frag(&mut states, re);
        Nfa { states, start, accept }
    }

    /// Build a Thompson fragment for `re`, returning its `(in, out)` states.
    fn frag(states: &mut Vec<NfaState>, re: &Re) -> (usize, usize) {
        match re {
            Re::Empty => {
                let a = Nfa::add(states);
                let b = Nfa::add(states);
                states[a].eps.push(b);
                (a, b)
            }
            Re::Class(class) => {
                let a = Nfa::add(states);
                let b = Nfa::add(states);
                states[a].edge = Some((*class, b));
                (a, b)
            }
            Re::Concat(parts) => {
                let a = Nfa::add(states);
                let mut cur = a;
                for p in parts {
                    let (fin, fout) = Nfa::frag(states, p);
                    states[cur].eps.push(fin);
                    cur = fout;
                }
                (a, cur)
            }
            Re::Alt(branches) => {
                let s = Nfa::add(states);
                let e = Nfa::add(states);
                for br in branches {
                    let (fin, fout) = Nfa::frag(states, br);
                    states[s].eps.push(fin);
                    states[fout].eps.push(e);
                }
                (s, e)
            }
            Re::Star(inner) => {
                let (fin, fout) = Nfa::frag(states, inner);
                let s = Nfa::add(states);
                let ex = Nfa::add(states);
                states[s].eps.push(fin);
                states[s].eps.push(ex);
                states[fout].eps.push(s);
                (s, ex)
            }
            Re::Plus(inner) => {
                let (fin, fout) = Nfa::frag(states, inner);
                let ex = Nfa::add(states);
                states[fout].eps.push(fin);
                states[fout].eps.push(ex);
                (fin, ex)
            }
            Re::Opt(inner) => {
                let (fin, fout) = Nfa::frag(states, inner);
                let s = Nfa::add(states);
                let ex = Nfa::add(states);
                states[s].eps.push(fin);
                states[s].eps.push(ex);
                states[fout].eps.push(ex);
                (s, ex)
            }
        }
    }

    fn eps_closure(&self, seed: &[usize]) -> Vec<usize> {
        let mut seen = vec![false; self.states.len()];
        let mut stack: Vec<usize> = seed.to_vec();
        let mut out = Vec::new();
        while let Some(s) = stack.pop() {
            if seen[s] {
                continue;
            }
            seen[s] = true;
            out.push(s);
            for &n in &self.states[s].eps {
                if !seen[n] {
                    stack.push(n);
                }
            }
        }
        out.sort_unstable();
        out
    }
}

// ---- DFA (subset construction) ----------------------------------------------

/// A compiled, deterministic matcher. `table[state * 256 + byte]` is the next
/// state (the table is complete — a dead state absorbs non-matches), and
/// `accepting[state]` marks a full-match state.
pub struct Dfa {
    pub start: u32,
    pub accepting: Vec<bool>,
    pub table: Vec<u32>,
}

impl Dfa {
    pub fn num_states(&self) -> usize {
        self.accepting.len()
    }

    /// Whether `s` matches the pattern in full (anchored start and end).
    pub fn matches(&self, s: &str) -> bool {
        let mut st = self.start as usize;
        for &byte in s.as_bytes() {
            st = self.table[st * 256 + byte as usize] as usize;
        }
        self.accepting[st]
    }
}

/// Compile a pattern to a DFA, or return a human-readable error for unsupported
/// or malformed syntax.
pub fn compile(pattern: &str) -> Result<Dfa, String> {
    let re = ReParser::parse(pattern)?;
    let nfa = Nfa::build(&re);

    use std::collections::HashMap;
    let mut index: HashMap<Vec<usize>, u32> = HashMap::new();
    let mut sets: Vec<Vec<usize>> = Vec::new();
    let mut table: Vec<u32> = Vec::new();
    let mut accepting: Vec<bool> = Vec::new();

    let intern = |set: Vec<usize>,
                  index: &mut HashMap<Vec<usize>, u32>,
                  sets: &mut Vec<Vec<usize>>|
     -> u32 {
        if let Some(&i) = index.get(&set) {
            return i;
        }
        let i = sets.len() as u32;
        index.insert(set.clone(), i);
        sets.push(set);
        i
    };

    let start_set = nfa.eps_closure(&[nfa.start]);
    let start = intern(start_set, &mut index, &mut sets);

    let mut processed = 0usize;
    while processed < sets.len() {
        let cur = sets[processed].clone();
        let dfa_state = processed as u32;
        // Grow the per-state rows.
        if table.len() < (dfa_state as usize + 1) * 256 {
            table.resize((dfa_state as usize + 1) * 256, 0);
        }
        if accepting.len() < dfa_state as usize + 1 {
            accepting.resize(dfa_state as usize + 1, false);
        }
        accepting[dfa_state as usize] = cur.contains(&nfa.accept);
        for byte in 0..256u32 {
            let mut moved: Vec<usize> = Vec::new();
            for &s in &cur {
                if let Some((class, to)) = &nfa.states[s].edge {
                    if class.contains(byte as u8) {
                        moved.push(*to);
                    }
                }
            }
            let closure = nfa.eps_closure(&moved); // empty set → the dead state
            let next = intern(closure, &mut index, &mut sets);
            table[dfa_state as usize * 256 + byte as usize] = next;
        }
        processed += 1;
    }
    // Final grow (last states may have been interned late).
    table.resize(sets.len() * 256, 0);
    accepting.resize(sets.len(), false);
    for (i, set) in sets.iter().enumerate() {
        accepting[i] = set.contains(&nfa.accept);
    }

    Ok(Dfa { start, accepting, table })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pat: &str, s: &str) -> bool {
        compile(pat).unwrap().matches(s)
    }

    #[test]
    fn literals_full_match() {
        assert!(m("abc", "abc"));
        assert!(!m("abc", "ab"));
        assert!(!m("abc", "abcd"));
    }

    #[test]
    fn char_class_and_plus() {
        assert!(m("[a-z]+", "hello"));
        assert!(!m("[a-z]+", "Hello"));
        assert!(!m("[a-z]+", "")); // + needs at least one
    }

    #[test]
    fn digit_shorthand_and_star() {
        assert!(m("\\d*", ""));
        assert!(m("\\d*", "0042"));
        assert!(!m("\\d*", "0a"));
    }

    #[test]
    fn word_class() {
        assert!(m("\\w+", "id_42"));
        assert!(!m("\\w+", "no space"));
    }

    #[test]
    fn dot_and_optional() {
        assert!(m("a.c", "abc"));
        assert!(m("a.c", "axc"));
        assert!(m("colou?r", "color"));
        assert!(m("colou?r", "colour"));
        assert!(!m("colou?r", "colouur"));
    }

    #[test]
    fn negated_class() {
        assert!(m("[^0-9]+", "abc"));
        assert!(!m("[^0-9]+", "ab9"));
    }

    #[test]
    fn escaped_metachars_are_literal() {
        assert!(m("a\\.b", "a.b"));
        assert!(!m("a\\.b", "axb"));
    }

    #[test]
    fn email_ish() {
        let p = "[a-z0-9_.]+@[a-z0-9]+\\.[a-z]+";
        assert!(m(p, "user.name_1@example.com"));
        assert!(!m(p, "no-at-sign"));
        assert!(!m(p, "user@@x.com"));
    }

    #[test]
    fn alternation() {
        assert!(m("cat|dog", "cat"));
        assert!(m("cat|dog", "dog"));
        assert!(!m("cat|dog", "cow"));
        assert!(m("(yes|no|maybe)", "maybe"));
    }

    #[test]
    fn groups_with_quantifiers() {
        // "ab" repeated.
        assert!(m("(ab)+", "ababab"));
        assert!(!m("(ab)+", "aba"));
        assert!(m("(ab)*", ""));
        // Optional group.
        assert!(m("a(bc)?d", "ad"));
        assert!(m("a(bc)?d", "abcd"));
    }

    #[test]
    fn every_second_char_is_a() {
        // `(.a)*` — even length, every char at an odd index is 'a'.
        let p = "(.a)*";
        assert!(m(p, ""));
        assert!(m(p, "xa"));
        assert!(m(p, "xaya1a"));
        assert!(!m(p, "xayb")); // 4th char is not 'a'
        assert!(!m(p, "xay")); // odd length
    }

    #[test]
    fn nested_groups_and_alternation() {
        let p = "(ab|cd)+";
        assert!(m(p, "abcdab"));
        assert!(!m(p, "abc"));
    }

    #[test]
    fn rejects_unsupported_syntax() {
        assert!(compile("*a").is_err()); // nothing to repeat
        assert!(compile("{2,3}").is_err()); // nothing to repeat (counted)
        assert!(compile("[a-z").is_err()); // unterminated class
        assert!(compile("(ab").is_err()); // unclosed group
        assert!(compile("ab)").is_err()); // unmatched close
        assert!(compile("a}").is_err()); // unmatched brace (literal is \})
        assert!(compile("[z-a]").is_err()); // reversed class range
        assert!(compile("a{3,2}").is_err()); // max < min
        assert!(compile("a{999}").is_err()); // over the 255 cap
        assert!(compile("a{2,").is_err()); // unclosed counted repetition
    }

    #[test]
    fn counted_repetition() {
        assert!(m("a{3}", "aaa"));
        assert!(!m("a{3}", "aa"));
        assert!(!m("a{3}", "aaaa"));
        assert!(m("a{2,4}", "aa"));
        assert!(m("a{2,4}", "aaaa"));
        assert!(!m("a{2,4}", "a"));
        assert!(!m("a{2,4}", "aaaaa"));
        assert!(m("a{2,}", "aaaaaaa"));
        assert!(!m("a{2,}", "a"));
        assert!(m("(ab){2}", "abab"));
        assert!(!m("(ab){2}", "ab"));
        assert!(m("[0-9]{3}-[0-9]{4}", "555-0123"));
        assert!(!m("[0-9]{3}-[0-9]{4}", "55-0123"));
        assert!(m("a{0,2}", "")); // zero minimum matches empty
    }

    #[test]
    fn dot_excludes_line_terminators() {
        // ECMA-262 `.`: any character except a line terminator.
        assert!(m("a.b", "axb"));
        assert!(!m("a.b", "a\nb"));
        assert!(!m("a.b", "a\rb"));
        // An explicit negated class stays byte-wise and CAN match `\n`.
        assert!(m("a[^x]b", "a\nb"));
    }
}
