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
//!   quant  = atom ( `*` | `+` | `?` )?
//!   atom   = `(` alt `)` | literal | `.` | `\d \w \s \D \W \S` | `\<c>` | `[ … ]`
//!   class  = `[` `^`? ( range | char )+ `]`   (range = `a-z`)
//! Supports alternation `|` and grouping `()` (no backreferences, so the language
//! stays regular and the DFA handles it). Unsupported (rejected clearly): counted
//! repetition `{m,n}`. Every accepted construct means the same thing as the
//! equivalent ECMA-262 regex, so it round-trips to a JSON Schema `pattern`.

/// A set of bytes (256 bits) — a single character class.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteClass([u64; 4]);

impl ByteClass {
    fn empty() -> Self {
        ByteClass([0; 4])
    }
    fn all() -> Self {
        ByteClass([u64::MAX; 4])
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
            b'{' | b'}' => Err("counted repetition `{..}` is not supported".into()),
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
        b'.' => Ok((ByteClass::all(), i + 1)),
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
        assert!(compile("a{2,3}").is_err()); // counted repetition
        assert!(compile("*a").is_err()); // nothing to repeat
        assert!(compile("[a-z").is_err()); // unterminated class
        assert!(compile("(ab").is_err()); // unclosed group
        assert!(compile("ab)").is_err()); // unmatched close
    }
}
