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
        let mut p = ReParser {
            b: pat.as_bytes(),
            i: 0,
        };
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
        Ok(if branches.len() == 1 {
            branches.pop().unwrap()
        } else {
            Re::Alt(branches)
        })
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
        let mut parts: Vec<Re> = std::iter::repeat_with(|| atom.clone())
            .take(m as usize)
            .collect();
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
        let v: u32 = text
            .parse()
            .map_err(|_| format!("repetition count `{text}` too large"))?;
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
                    b[i] as char,
                    b[i + 2] as char
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
        states.push(NfaState {
            eps: Vec::new(),
            edge: None,
        });
        states.len() - 1
    }

    fn build(re: &Re) -> Nfa {
        let mut states: Vec<NfaState> = Vec::new();
        let (start, accept) = Nfa::frag(&mut states, re);
        Nfa {
            states,
            start,
            accept,
        }
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

    /// The set of states reachable from `start` over any byte transition.
    fn reachable_from_start(&self) -> Vec<bool> {
        let n = self.num_states();
        let mut seen = vec![false; n];
        let mut stack = vec![self.start as usize];
        seen[self.start as usize] = true;
        while let Some(s) = stack.pop() {
            for b in 0..256usize {
                let t = self.table[s * 256 + b] as usize;
                if !seen[t] {
                    seen[t] = true;
                    stack.push(t);
                }
            }
        }
        seen
    }

    /// The set of states that can reach an accepting state (reverse reachability
    /// from every accepting state over the transition graph).
    fn can_reach_accept(&self) -> Vec<bool> {
        let n = self.num_states();
        // Reverse adjacency: rev[t] holds every predecessor state `s` with an
        // edge `s -b-> t`.
        let mut rev: Vec<Vec<usize>> = vec![Vec::new(); n];
        for s in 0..n {
            for b in 0..256usize {
                let t = self.table[s * 256 + b] as usize;
                rev[t].push(s);
            }
        }
        let mut seen = vec![false; n];
        let mut stack = Vec::new();
        for (s, &acc) in self.accepting.iter().enumerate() {
            if acc {
                seen[s] = true;
                stack.push(s);
            }
        }
        while let Some(t) = stack.pop() {
            for &s in &rev[t] {
                if !seen[s] {
                    seen[s] = true;
                    stack.push(s);
                }
            }
        }
        seen
    }

    /// Whether the language is **finite**: no cycle lies on any accepting path.
    /// A DFA denotes a finite language iff, among the states that are both
    /// reachable from the start AND able to reach an accepting state ("live"
    /// states), there is no cycle. The empty language (nothing accepting is
    /// reachable) is finite vacuously. Decidable in one DFS over the live
    /// subgraph. This is the defining test of a *finite string type* (RFC-0020).
    pub fn is_finite(&self) -> bool {
        let n = self.num_states();
        let reach = self.reachable_from_start();
        let acc = self.can_reach_accept();
        let live: Vec<bool> = (0..n).map(|i| reach[i] && acc[i]).collect();
        // Cycle detection (three-colour DFS) over the subgraph induced by live
        // states, following distinct successor states (a self-loop counts).
        #[derive(Clone, Copy, PartialEq)]
        enum Colour {
            White,
            Grey,
            Black,
        }
        let mut colour = vec![Colour::White; n];
        // Iterative DFS with an explicit stack of (state, next-successor-index)
        // over the deduplicated live successors of each state.
        let succ = |s: usize| -> Vec<usize> {
            let mut out: Vec<usize> = Vec::new();
            for b in 0..256usize {
                let t = self.table[s * 256 + b] as usize;
                if live[t] && !out.contains(&t) {
                    out.push(t);
                }
            }
            out
        };
        for start in 0..n {
            if !live[start] || colour[start] != Colour::White {
                continue;
            }
            let mut stack: Vec<(usize, Vec<usize>, usize)> = vec![(start, succ(start), 0)];
            colour[start] = Colour::Grey;
            while let Some((s, succs, i)) = stack.last_mut() {
                if *i < succs.len() {
                    let t = succs[*i];
                    *i += 1;
                    match colour[t] {
                        Colour::Grey => return false, // back-edge → cycle → infinite
                        Colour::White => {
                            colour[t] = Colour::Grey;
                            let ts = succ(t);
                            stack.push((t, ts, 0));
                        }
                        Colour::Black => {}
                    }
                } else {
                    colour[*s] = Colour::Black;
                    stack.pop();
                }
            }
        }
        true
    }

    /// Enumerate every string in the language in lexicographic (byte) order, up
    /// to `cap` strings. Returns `None` if the language has more than `cap`
    /// members (or is infinite). Deterministic. Non-UTF-8 members (impossible for
    /// the ASCII refinement patterns in practice) are rendered with `\xNN`
    /// escapes; see [`escape_bytes`].
    pub fn enumerate(&self, cap: usize) -> Option<Vec<String>> {
        if !self.is_finite() {
            return None;
        }
        let acc = self.can_reach_accept();
        // BFS by length keeps the queue bounded (a finite language over a DFA has
        // no live-state repeat on any accepting path, so lengths are bounded by
        // the live-state count). Collect raw bytes, then sort for a stable
        // lexicographic result.
        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut frontier: Vec<(usize, Vec<u8>)> = vec![(self.start as usize, Vec::new())];
        let max_len = self.num_states() + 1; // safety bound; never hit when finite
        let mut depth = 0;
        while !frontier.is_empty() && depth <= max_len {
            let mut next: Vec<(usize, Vec<u8>)> = Vec::new();
            for (st, s) in &frontier {
                if self.accepting[*st] {
                    out.push(s.clone());
                    if out.len() > cap {
                        return None;
                    }
                }
                for b in 0..256usize {
                    let t = self.table[*st * 256 + b] as usize;
                    // Only descend into states that can still reach an accept —
                    // prunes the dead state and dead branches.
                    if acc[t] {
                        let mut ns = s.clone();
                        ns.push(b as u8);
                        next.push((t, ns));
                    }
                }
            }
            frontier = next;
            depth += 1;
        }
        out.sort();
        Some(out.into_iter().map(|b| escape_bytes(&b)).collect())
    }

    /// Enumerate every accepted string that contains **no** `exclude` byte, up to
    /// `cap` strings, in lexicographic order. Returns `None` if there are more
    /// than `cap` such strings (or the sublanguage is unbounded).
    ///
    /// This is how a *sequence* validated-string type yields its **alphabet**
    /// (RFC-0042): `Tw`'s language is `token( token)*` — infinite — but the strings
    /// it accepts that contain no space are exactly the single tokens (`= L(TwClass)`),
    /// which is finite. Excluding the separator byte and enumerating the residual
    /// finite sublanguage gives the completion/checking alphabet straight from the
    /// same DFA the compiler validates against (one enumeration, no drift).
    pub fn enumerate_without(&self, exclude: u8, cap: usize) -> Option<Vec<String>> {
        let acc = self.can_reach_accept();
        let mut out: Vec<Vec<u8>> = Vec::new();
        // BFS by length. Excluding `exclude` prunes the transition that restarts a
        // fresh token, so the residual paths are bounded by the token DFA — the
        // safety bound below is never the terminating condition when the sublanguage
        // is finite, but guards against an `exclude`-free cycle (unbounded) by
        // returning `None` via the `cap` overflow first.
        let mut frontier: Vec<(usize, Vec<u8>)> = vec![(self.start as usize, Vec::new())];
        let max_len = self.num_states() + 1;
        let mut depth = 0;
        while !frontier.is_empty() && depth <= max_len {
            let mut next: Vec<(usize, Vec<u8>)> = Vec::new();
            for (st, s) in &frontier {
                if self.accepting[*st] {
                    out.push(s.clone());
                    if out.len() > cap {
                        return None;
                    }
                }
                for b in 0..256usize {
                    if b as u8 == exclude {
                        continue;
                    }
                    let t = self.table[*st * 256 + b] as usize;
                    if acc[t] {
                        let mut ns = s.clone();
                        ns.push(b as u8);
                        next.push((t, ns));
                    }
                }
            }
            frontier = next;
            depth += 1;
        }
        out.sort();
        out.dedup();
        Some(out.into_iter().map(|b| escape_bytes(&b)).collect())
    }

    /// The complement DFA: same transitions, accepting bits flipped. Correct
    /// because the DFA is **total** (the subset construction always interns the
    /// empty set as a dead sink with all-self transitions), so every byte string
    /// lands in exactly one state and flipping acceptance yields the complement
    /// language over all byte strings.
    pub fn complement(&self) -> Dfa {
        Dfa {
            start: self.start,
            accepting: self.accepting.iter().map(|a| !a).collect(),
            table: self.table.clone(),
        }
    }
}

/// Render bytes for a diagnostic: the UTF-8 string if valid, otherwise each
/// offending byte as `\xNN` (with valid runs kept verbatim). Refinement
/// languages are ASCII in practice, so the escape path is defensive.
pub fn escape_bytes(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            let mut out = String::new();
            for &b in bytes {
                if b.is_ascii_graphic() || b == b' ' {
                    out.push(b as char);
                } else {
                    out.push_str(&format!("\\x{b:02x}"));
                }
            }
            out
        }
    }
}

/// The intersection DFA (product automaton, accepting iff both accept). Used to
/// realise a `where value =~ "a" && value =~ "b"` conjunction as a single DFA.
pub fn intersect(a: &Dfa, b: &Dfa) -> Dfa {
    product(a, b, |x, y| x && y)
}

/// `sub ⊆ sup`? Returns `Ok(())` when every string `sub` accepts, `sup` also
/// accepts; otherwise `Err(witness)` where `witness` is a **shortest** string in
/// `sub \ sup` (BFS over the product of `sub` and the complement of `sup`, so
/// the counterexample the containment fails on is produced for free). This is
/// the automaton-theoretic core of RFC-0020's interpolation containment.
pub fn contains(sup: &Dfa, sub: &Dfa) -> Result<(), String> {
    // sub ⊆ sup  ⇔  sub ∩ ¬sup = ∅. BFS the product; a reachable state that is
    // accepting in `sub` and non-accepting in `sup` is a witness.
    use std::collections::VecDeque;
    let ns = sup.num_states();
    let start = (sub.start as usize, sup.start as usize);
    let idx = |a: usize, c: usize| a * ns + c;
    let total = sub.num_states() * ns;
    let mut seen = vec![false; total];
    let mut prev: Vec<Option<(usize, u8)>> = vec![None; total];
    let mut queue = VecDeque::new();
    seen[idx(start.0, start.1)] = true;
    queue.push_back(start);
    let mut witness_state: Option<(usize, usize)> = None;
    'bfs: while let Some((a, c)) = queue.pop_front() {
        if sub.accepting[a] && !sup.accepting[c] {
            witness_state = Some((a, c));
            break 'bfs;
        }
        for b in 0..256usize {
            let na = sub.table[a * 256 + b] as usize;
            let nc = sup.table[c * 256 + b] as usize;
            let id = idx(na, nc);
            if !seen[id] {
                seen[id] = true;
                prev[id] = Some((idx(a, c), b as u8));
                queue.push_back((na, nc));
            }
        }
    }
    match witness_state {
        None => Ok(()),
        Some((a, c)) => {
            // Reconstruct the shortest path bytes by walking `prev` back to start.
            let mut bytes: Vec<u8> = Vec::new();
            let mut cur = idx(a, c);
            let start_id = idx(start.0, start.1);
            while cur != start_id {
                let (p, b) = prev[cur].expect("product BFS parent");
                bytes.push(b);
                cur = p;
            }
            bytes.reverse();
            Err(escape_bytes(&bytes))
        }
    }
}

/// Product automaton over two DFAs, `accept` combining the two acceptance bits.
/// Only reachable product states are materialised (keeps the table small).
fn product(a: &Dfa, b: &Dfa, accept: impl Fn(bool, bool) -> bool) -> Dfa {
    use std::collections::HashMap;
    let mut index: HashMap<(usize, usize), u32> = HashMap::new();
    let mut work: Vec<(usize, usize)> = Vec::new();
    let start_pair = (a.start as usize, b.start as usize);
    index.insert(start_pair, 0);
    work.push(start_pair);
    let mut table: Vec<u32> = Vec::new();
    let mut accepting: Vec<bool> = Vec::new();
    let mut processed = 0usize;
    while processed < work.len() {
        let (sa, sb) = work[processed];
        if table.len() < (processed + 1) * 256 {
            table.resize((processed + 1) * 256, 0);
        }
        accepting.resize(processed + 1, false);
        accepting[processed] = accept(a.accepting[sa], b.accepting[sb]);
        for byte in 0..256usize {
            let na = a.table[sa * 256 + byte] as usize;
            let nb = b.table[sb * 256 + byte] as usize;
            let next = *index.entry((na, nb)).or_insert_with(|| {
                let id = work.len() as u32;
                work.push((na, nb));
                id
            });
            table[processed * 256 + byte] = next;
        }
        processed += 1;
    }
    table.resize(work.len() * 256, 0);
    accepting.resize(work.len(), false);
    for (i, &(sa, sb)) in work.iter().enumerate() {
        accepting[i] = accept(a.accepting[sa], b.accepting[sb]);
    }
    Dfa {
        start: 0,
        accepting,
        table,
    }
}

// ---- concatenation of literals and hole DFAs (RFC-0020) ---------------------

/// One piece of an interpolation's concatenation language: a literal byte run
/// (a string constant) or the language of a hole (a finite string type's DFA).
pub enum ConcatPiece<'a> {
    Lit(&'a [u8]),
    Dfa(&'a Dfa),
}

/// A general ε-NFA (multiple labelled edges per state) used to assemble a
/// concatenation of literal runs and hole DFAs, then subset-construct to a DFA.
/// The `Re`-based [`Nfa`] cannot express an arbitrary hole DFA, so this is its
/// sibling for the containment feature.
struct GenNfa {
    states: Vec<GenState>,
}

struct GenState {
    eps: Vec<usize>,
    edges: Vec<(ByteClass, usize)>,
}

impl GenNfa {
    fn new() -> GenNfa {
        GenNfa { states: Vec::new() }
    }
    fn add(&mut self) -> usize {
        self.states.push(GenState {
            eps: Vec::new(),
            edges: Vec::new(),
        });
        self.states.len() - 1
    }

    /// A fragment matching exactly the literal bytes `s`, returning `(in, out)`.
    fn literal(&mut self, s: &[u8]) -> (usize, usize) {
        let start = self.add();
        let mut cur = start;
        for &b in s {
            let nxt = self.add();
            let mut cl = ByteClass::empty();
            cl.set(b);
            self.states[cur].edges.push((cl, nxt));
            cur = nxt;
        }
        (start, cur)
    }

    /// A fragment matching the DFA `dfa`'s language, returning `(in, out)`. Each
    /// DFA state becomes an NFA state; a fresh `in` ε-jumps to the copied start,
    /// and every copied accepting state ε-jumps to a fresh single `out`. The dead
    /// sink (a non-accepting state whose every transition is a self-loop) is
    /// dropped so it never pollutes the assembled automaton.
    fn from_dfa(&mut self, dfa: &Dfa) -> (usize, usize) {
        let dead = dfa.dead_state();
        let n = dfa.num_states();
        // Map each kept DFA state to a fresh NFA state id.
        let mut map = vec![usize::MAX; n];
        for (s, m) in map.iter_mut().enumerate() {
            if Some(s) != dead {
                *m = self.add();
            }
        }
        let entry = self.add();
        let exit = self.add();
        self.states[entry].eps.push(map[dfa.start as usize]);
        for s in 0..n {
            if Some(s) == dead {
                continue;
            }
            // Group the 256 transitions by target into one ByteClass per target.
            let mut by_target: std::collections::HashMap<usize, ByteClass> =
                std::collections::HashMap::new();
            for b in 0..256usize {
                let t = dfa.table[s * 256 + b] as usize;
                if Some(t) == dead {
                    continue;
                }
                by_target
                    .entry(t)
                    .or_insert_with(ByteClass::empty)
                    .set(b as u8);
            }
            for (t, cl) in by_target {
                self.states[map[s]].edges.push((cl, map[t]));
            }
            if dfa.accepting[s] {
                self.states[map[s]].eps.push(exit);
            }
        }
        (entry, exit)
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

    /// Subset-construct the DFA for the language of this NFA, given its start and
    /// single accepting state.
    fn to_dfa(&self, start: usize, accept: usize) -> Dfa {
        use std::collections::HashMap;
        let mut index: HashMap<Vec<usize>, u32> = HashMap::new();
        let mut sets: Vec<Vec<usize>> = Vec::new();
        let mut table: Vec<u32> = Vec::new();
        let mut accepting: Vec<bool> = Vec::new();
        let start_set = self.eps_closure(&[start]);
        index.insert(start_set.clone(), 0);
        sets.push(start_set);
        let mut processed = 0usize;
        while processed < sets.len() {
            let cur = sets[processed].clone();
            if table.len() < (processed + 1) * 256 {
                table.resize((processed + 1) * 256, 0);
            }
            accepting.resize(processed + 1, false);
            accepting[processed] = cur.contains(&accept);
            for byte in 0..256usize {
                let mut moved: Vec<usize> = Vec::new();
                for &s in &cur {
                    for (cl, to) in &self.states[s].edges {
                        if cl.contains(byte as u8) {
                            moved.push(*to);
                        }
                    }
                }
                let closure = self.eps_closure(&moved);
                let next = *index.entry(closure.clone()).or_insert_with(|| {
                    let id = sets.len() as u32;
                    sets.push(closure);
                    id
                });
                table[processed * 256 + byte] = next;
            }
            processed += 1;
        }
        table.resize(sets.len() * 256, 0);
        accepting.resize(sets.len(), false);
        for (i, set) in sets.iter().enumerate() {
            accepting[i] = set.contains(&accept);
        }
        Dfa {
            start: 0,
            accepting,
            table,
        }
    }
}

/// Build the DFA for the language `piece0 · piece1 · …` — the concatenation of
/// literal runs and hole DFAs. This is exactly the language of a string
/// interpolation `"lit0\{h1}lit1…"` when each hole `hi` ranges over a finite
/// string type (RFC-0020). An empty piece list denotes `{""}`.
pub fn concat_language(pieces: &[ConcatPiece]) -> Dfa {
    let mut nfa = GenNfa::new();
    // Seed with an ε-only fragment so an empty piece list yields `{""}`.
    let start = nfa.add();
    let mut tail = start;
    for piece in pieces {
        let (fin, fout) = match piece {
            ConcatPiece::Lit(s) => nfa.literal(s),
            ConcatPiece::Dfa(d) => nfa.from_dfa(d),
        };
        nfa.states[tail].eps.push(fin);
        tail = fout;
    }
    nfa.to_dfa(start, tail)
}

impl Dfa {
    /// The dead sink state, if any: a non-accepting state whose every transition
    /// is a self-loop (the empty-set state the subset construction interns). Used
    /// when copying a DFA into an NFA fragment, to drop the sink cleanly.
    fn dead_state(&self) -> Option<usize> {
        (0..self.num_states()).find(|&s| {
            !self.accepting[s] && (0..256usize).all(|b| self.table[s * 256 + b] as usize == s)
        })
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

    Ok(Dfa {
        start,
        accepting,
        table,
    })
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

    // ---- finiteness / enumeration / containment (RFC-0020) ------------------

    fn dfa(pat: &str) -> Dfa {
        compile(pat).unwrap()
    }

    #[test]
    fn finiteness_of_languages() {
        // Finite: literals, alternations of finite branches, counted {m,n}.
        assert!(dfa("abc").is_finite());
        assert!(dfa("cat|dog|maybe").is_finite());
        assert!(dfa("a{2,4}").is_finite());
        assert!(dfa("(home\\.(title|subtitle)|nav\\.(home|about)\\.label)").is_finite());
        assert!(dfa("nav\\.(home|about|settings)\\.label").is_finite());
        // The empty language is finite vacuously (nothing accepts).
        // `[^\\x00-\\xff]` — a class matching no byte — never accepts.
        // Infinite: any unbounded repetition on an accepting path.
        assert!(!dfa("a+").is_finite());
        assert!(!dfa("a*").is_finite());
        assert!(!dfa("a{2,}").is_finite());
        assert!(!dfa("[a-z]+").is_finite());
        assert!(!dfa("x(ab)*y").is_finite());
        // A star whose body cannot lead to an accept does NOT make it infinite.
        assert!(dfa("ab").is_finite());
    }

    #[test]
    fn enumeration_is_exact_and_ordered() {
        assert_eq!(
            dfa("cat|dog|bird").enumerate(10),
            Some(vec![
                "bird".to_string(),
                "cat".to_string(),
                "dog".to_string()
            ])
        );
        // Lexicographic (byte) order, deterministic.
        assert_eq!(
            dfa("nav\\.(home|about)\\.label").enumerate(10),
            Some(vec![
                "nav.about.label".to_string(),
                "nav.home.label".to_string()
            ])
        );
        // A single literal is a one-element language (`.` escaped to a literal).
        assert_eq!(dfa("home\\.title").enumerate(10).map(|v| v.len()), Some(1));
        // {m,n} enumerates every count.
        assert_eq!(
            dfa("a{1,3}").enumerate(10),
            Some(vec!["a".to_string(), "aa".to_string(), "aaa".to_string()])
        );
    }

    #[test]
    fn enumeration_respects_cap_and_infinity() {
        // Over the cap → None.
        assert_eq!(dfa("cat|dog|bird").enumerate(2), None);
        // Infinite language → None regardless of cap.
        assert_eq!(dfa("[a-z]+").enumerate(1000), None);
        // Exactly at the cap → Some.
        assert!(dfa("cat|dog|bird").enumerate(3).is_some());
    }

    #[test]
    fn containment_holds_and_fails_with_shortest_witness() {
        // sub ⊆ sup.
        let keys = dfa("nav\\.(home|about|settings)\\.label");
        let sub = dfa("nav\\.(home|about)\\.label");
        assert_eq!(contains(&keys, &sub), Ok(()));
        // Self-containment.
        assert_eq!(contains(&keys, &keys), Ok(()));
        // Not contained: `settings` is in sub but a narrower sup forbids it.
        let narrow = dfa("nav\\.(home|about)\\.label");
        let wide = dfa("nav\\.(home|about|settings)\\.label");
        match contains(&narrow, &wide) {
            Err(w) => assert_eq!(w, "nav.settings.label"),
            Ok(()) => panic!("expected a witness"),
        }
    }

    #[test]
    fn containment_witness_is_shortest() {
        // sup accepts only strings of length ≥ 3 starting `a`; sub accepts `a`.
        let sup = dfa("a.+");
        let sub = dfa("a|abcd");
        match contains(&sup, &sub) {
            Err(w) => assert_eq!(w, "a"), // the shortest member of sub \ sup
            Ok(()) => panic!("expected a witness"),
        }
    }

    #[test]
    fn empty_language_is_contained_in_everything() {
        // `sub` accepting nothing is a subset of any `sup`. Use an intersection
        // of disjoint literals as a guaranteed-empty language.
        let a = dfa("abc");
        let b = dfa("xyz");
        let empty = intersect(&a, &b);
        assert_eq!(empty.enumerate(10), Some(vec![]));
        assert_eq!(contains(&dfa("abc"), &empty), Ok(()));
    }

    #[test]
    fn intersection_is_the_conjunction_language() {
        // `[a-z]{3}` ∩ `.a.` = strings of 3 lowercase letters whose middle is `a`.
        let inter = intersect(&dfa("[a-z]{3}"), &dfa(".a."));
        assert!(inter.matches("cat"));
        assert!(inter.matches("bat"));
        assert!(!inter.matches("cot"));
        assert!(!inter.matches("ca")); // wrong length
    }

    #[test]
    fn concat_of_literals_and_holes() {
        // "nav." · {home,about} · ".label"
        let section = dfa("home|about");
        let l = concat_language(&[
            ConcatPiece::Lit(b"nav."),
            ConcatPiece::Dfa(&section),
            ConcatPiece::Lit(b".label"),
        ]);
        assert!(l.matches("nav.home.label"));
        assert!(l.matches("nav.about.label"));
        assert!(!l.matches("nav.settings.label"));
        assert!(!l.matches("nav.home.label.x"));
        assert_eq!(
            l.enumerate(10),
            Some(vec![
                "nav.about.label".to_string(),
                "nav.home.label".to_string()
            ])
        );
        // Empty piece list denotes {""}.
        assert_eq!(
            concat_language(&[]).enumerate(10),
            Some(vec!["".to_string()])
        );
        // A hole whose language includes the empty string concatenates correctly.
        let opt = dfa("x?");
        let l2 = concat_language(&[
            ConcatPiece::Lit(b"a"),
            ConcatPiece::Dfa(&opt),
            ConcatPiece::Lit(b"b"),
        ]);
        assert!(l2.matches("ab"));
        assert!(l2.matches("axb"));
        assert!(!l2.matches("axxb"));
    }

    #[test]
    fn concat_containment_end_to_end() {
        // The flagship: L = "nav." · Section · ".label" ⊆ TransKey.
        let trans_key = dfa("(home\\.(title|subtitle)|nav\\.(home|about|settings)\\.label)");
        let section = dfa("home|about|settings");
        let l = concat_language(&[
            ConcatPiece::Lit(b"nav."),
            ConcatPiece::Dfa(&section),
            ConcatPiece::Lit(b".label"),
        ]);
        assert_eq!(contains(&trans_key, &l), Ok(()));
        // Widen the section to include a key TransKey does not have → witness.
        let bad_section = dfa("home|about|profile");
        let bad = concat_language(&[
            ConcatPiece::Lit(b"nav."),
            ConcatPiece::Dfa(&bad_section),
            ConcatPiece::Lit(b".label"),
        ]);
        match contains(&trans_key, &bad) {
            Err(w) => assert_eq!(w, "nav.profile.label"),
            Ok(()) => panic!("expected a witness"),
        }
    }
}
