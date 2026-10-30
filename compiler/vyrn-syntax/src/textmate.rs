//! Writes a grammar's editor files: a TextMate grammar and a language configuration.
//!
//! A class comes from a lexical rule's name, in [`CLASSES`]. A reserved word comes from
//! a `keyword` line. A contextual word is a word literal no `keyword` line reserves,
//! matched where what follows it can stand and, when literals fix it, after what precedes
//! it. Tokens come from `token` lines. What a grammar cannot state arrives as [`Naming`].
//!
//! A class rule `open (!close item)* close`, or a choice of those, lowers to begin, end
//! and patterns, with a hole where an item holds an `embed`. Any other class rule lowers
//! to one regex: atomic groups keep the PEG's order, and an alternative that refuses is
//! left out. Every other shape is refused.

use std::collections::{BTreeMap, BTreeSet};

use crate::{Builtin, Grammar, Refused, RuleId, E};

/// The vocabulary: a rule of this name has this scope, and this text before its regex.
const CLASSES: [(&str, &str, &str); 5] = [
    ("doc", "comment.line.documentation", ""),
    ("comment", "comment.line", ""),
    ("string", "string.quoted", ""),
    ("byte", "constant.character", ""),
    ("number", "constant.numeric", "\\b"),
];
/// The bracket pairs and each one's scope word.
const PAIRS: [(&str, &str, &str); 3] = [
    ("(", ")", "parens"),
    ("[", "]", "brackets"),
    ("{", "}", "braces"),
];
/// The spellings that part items. Every other token that is no bracket is an operator.
const SEPARATORS: [&str; 4] = [",", ";", ":", "."];
/// In a set of what may precede a word: anything may.
const UNFIXED: &str = "";

/// Naming conventions a grammar cannot state, each a regex and a scope without the
/// language's suffix. `before_words` outrank the words and `after_words` yield to them.
pub struct Naming<'a> {
    pub before_words: &'a [(&'a str, &'a str)],
    pub after_words: &'a [(&'a str, &'a str)],
}

type Set = BTreeSet<String>;

/// Returns the TextMate grammar of `g`, whose scopes end in `lang`, the stem of its file.
///
/// # Errors
///
/// Refuses, at its line, a class rule of any shape but the two the module names.
pub fn textmate(g: &Grammar, lang: &str, naming: &Naming) -> Result<String, Refused> {
    let mut w = Writer {
        g,
        lang,
        path: Vec::new(),
        nested: BTreeMap::new(),
    };
    let mut pats = Vec::new();
    for (class, scope, lead) in CLASSES {
        if let Some(rule) = g.rule(class).map(|r| g.body(r)) {
            let at = |why| Refused::at(rule.line, 1, format!("rule `{class}`: {why}"));
            pats.extend(w.class(&rule.body, scope, lead).map_err(at)?);
        }
    }
    let rules = |w: &Writer, list: &[(&str, &str)]| -> Vec<String> {
        list.iter().map(|(re, scope)| w.rule(scope, re)).collect()
    };
    pats.extend(rules(&w, naming.before_words));
    for ((before, after), words) in words(g) {
        let words = Vec::from_iter(words).join("|");
        let re = format!("{before}\\b(?:{words})\\b{}", ahead_re(&after));
        pats.push(w.rule("keyword.control", &re));
    }
    pats.extend(rules(&w, naming.after_words));
    pats.extend(w.tokens());
    let nested: Vec<String> = (w.nested.iter())
        .map(|(k, v)| format!("    {k:?}: {v}"))
        .collect();
    Ok(format!(
        "{{\n  \"comment\": \"Written by vyrn_syntax::textmate from {0}.syntax. VYRN_PIN=write cargo test -p vyrn-syntax rewrites it.\",\n  \"name\": {0:?},\n  \"scopeName\": \"source.{0}\",\n  \"patterns\": [\n    {1}\n  ],\n  \"repository\": {{\n{2}\n  }}\n}}\n",
        w.lang,
        pats.join(",\n    "),
        nested.join(",\n"),
    ))
}

/// Returns the language configuration of `g`: its comment opener, its bracket tokens
/// and the shortest delimiter of its `string` rule. A grammar with no such rule or token
/// states none.
pub fn language_configuration(g: &Grammar) -> String {
    let head = |class: &str| g.rule(class).and_then(|r| shortest_head(&g.body(r).body));
    let row = |a: &str, b: &str| format!("[{a:?}, {b:?}]");
    let spelled = |p: &&(&str, &str, &str)| g.tokens.iter().any(|t| t.1 == p.0);
    let mut pairs: Vec<String> = PAIRS
        .iter()
        .filter(spelled)
        .map(|p| row(p.0, p.1))
        .collect();
    let brackets = pairs.join(", ");
    pairs.extend(head("string").map(|q| row(&q, &q)));
    let pairs = pairs.join(", ");
    let comments = head("comment").map_or(String::new(), |opener| {
        format!("\"comments\": {{ \"lineComment\": {opener:?} }},\n  ")
    });
    format!("{{\n  {comments}\"brackets\": [{brackets}],\n  \"autoClosingPairs\": [{pairs}],\n  \"surroundingPairs\": [{pairs}]\n}}\n")
}

/// `e` without the wrappers that match what it matches.
fn strip(e: &E) -> &E {
    match e {
        E::Field(_, x) | E::Guard(_, x) => strip(x),
        _ => e,
    }
}

/// The shortest literal `e` opens with, across its alternatives.
fn shortest_head(e: &E) -> Option<String> {
    match strip(e) {
        E::Lit(l) => Some(l.s.to_string()),
        E::Seq(v) => v.first().and_then(shortest_head),
        E::Alt(v) => v.iter().filter_map(shortest_head).min_by_key(String::len),
        _ => None,
    }
}

/// Whether `e` refuses instead of matching.
fn refuses(e: &E) -> bool {
    match strip(e) {
        E::Throw(_) => true,
        E::Seq(v) => v.iter().any(|x| matches!(strip(x), E::Throw(_))),
        _ => false,
    }
}

/// The parts of `open (!close item)* close`.
fn delimited(e: &E) -> Option<(&E, &E, &E)> {
    let E::Seq(v) = strip(e) else { return None };
    let [open, E::Star(body), close] = &v[..] else {
        return None;
    };
    match &**body {
        E::Seq(s) if matches!(s[..], [E::Not(_), _]) => Some((open, &s[1], close)),
        _ => None,
    }
}

/// `s` as regex text that matches it literally.
fn esc(s: &str) -> String {
    let one = |c: char| match c {
        c if c.is_ascii_control() => format!("\\x{:02x}", c as u32),
        c if c.is_ascii_punctuation() && c != '_' => format!("\\{c}"),
        c => c.to_string(),
    };
    s.chars().map(one).collect()
}

/// The regex of an expression that holds no other.
fn atom(e: &E) -> Option<String> {
    let range = |&(a, b): &(char, char)| match a == b {
        true => esc(&a.to_string()),
        false => format!("{}-{}", esc(&a.to_string()), esc(&b.to_string())),
    };
    let class = |ranges: &[(char, char)]| ranges.iter().map(range).collect::<String>();
    Some(match e {
        E::Lit(l) => esc(&l.s),
        E::Split(s) => esc(s),
        E::Any => "[\\s\\S]".to_string(),
        E::Class(ranges, false) => format!("[{}]", class(ranges)),
        E::Class(ranges, true) => format!("[^{}]", class(ranges)),
        E::Builtin(Builtin::Eof) => "\\z".to_string(),
        E::Builtin(Builtin::Alpha) => "\\p{L}".to_string(),
        E::Builtin(Builtin::Alnum) => "[\\p{L}\\p{N}]".to_string(),
        E::Builtin(Builtin::Digit) => "[0-9]".to_string(),
        E::Builtin(Builtin::Hex) => "[0-9A-Fa-f]".to_string(),
        E::Builtin(Builtin::Ws) => "\\s".to_string(),
        _ => return None,
    })
}

struct Writer<'g> {
    g: &'g Grammar,
    lang: &'g str,
    /// The rules being read, so a recursion ends in a refusal.
    path: Vec<RuleId>,
    /// The pair a hole nests, by the pair's scope word.
    nested: BTreeMap<&'static str, String>,
}

impl Writer<'_> {
    fn rule(&self, scope: &str, re: &str) -> String {
        format!(
            "{{ \"name\": \"{scope}.{}\", \"match\": {re:?} }}",
            self.lang
        )
    }

    /// A begin and end rule. Its two marks take `stem` with `.begin` and `.end`.
    fn span(&self, scope: &str, stem: &str, begin: &str, end: &str, inner: &[String]) -> String {
        let l = &self.lang;
        let mark = |side: &str| format!("{{ \"0\": {{ \"name\": \"{stem}.{side}.{l}\" }} }}");
        format!(
            "{{ \"name\": \"{scope}.{l}\", \"begin\": {begin:?}, \"end\": {end:?}, \"beginCaptures\": {}, \"endCaptures\": {}, \"patterns\": [{}] }}",
            mark("begin"),
            mark("end"),
            inner.join(", ")
        )
    }

    /// The patterns of one class rule.
    fn class(&mut self, body: &E, scope: &str, lead: &str) -> Result<Vec<String>, String> {
        let forms: Vec<&E> = match strip(body) {
            E::Alt(v) => v.iter().collect(),
            e => vec![e],
        };
        let Some(forms) = forms.into_iter().map(delimited).collect::<Option<Vec<_>>>() else {
            let re = self.re(body)?;
            return Ok(vec![self.rule(scope, &format!("{lead}{re}"))]);
        };
        let stem = format!("punctuation.definition.{scope}");
        let mut out = Vec::new();
        for (open, item, close) in forms {
            let inner = self.items(item, "")?;
            let (begin, end) = (self.re(open)?, self.re(close)?);
            out.push(self.span(scope, &stem, &begin, &end, &inner));
        }
        Ok(out)
    }

    /// The patterns of a delimited rule's item, after `prefix`. Body text and an
    /// alternative that refuses have none. A regular item is an escape.
    fn items(&mut self, e: &E, prefix: &str) -> Result<Vec<String>, String> {
        if refuses(e) || (prefix.is_empty() && matches!(strip(e), E::Any | E::Class(..))) {
            return Ok(Vec::new());
        }
        if let Ok(re) = self.re(e) {
            let re = format!("{prefix}{re}");
            return Ok(vec![self.rule("constant.character.escape", &re)]);
        }
        match strip(e) {
            E::Alt(v) => {
                let each = v.iter().map(|x| self.items(x, prefix));
                Ok(each.collect::<Result<Vec<_>, _>>()?.concat())
            }
            E::Ref(r) if !self.path.contains(r) => {
                self.path.push(*r);
                let out = self.items(&self.g.body(*r).body, prefix);
                self.path.pop();
                out
            }
            E::Seq(v) => {
                let is_embed = |x: &E| matches!(strip(x), E::Embed(_));
                let k = v.iter().position(is_embed).unwrap_or(v.len() - 1);
                let prefix = format!("{prefix}{}", self.re(&E::Seq(v[..k].to_vec()))?);
                match is_embed(&v[k]) {
                    true => self.hole(&prefix, &v[k + 1..]),
                    false => self.items(&v[k], &prefix),
                }
            }
            _ => self.re(e).map(|_| Vec::new()),
        }
    }

    /// A hole, which holds the grammar's own patterns; its `end` outranks them where
    /// both match. A hole closed by a bracket nests that bracket's pair.
    fn hole(&mut self, begin: &str, close: &[E]) -> Result<Vec<String>, String> {
        let closer = close.first().and_then(shortest_head);
        let closer = closer.ok_or("an `embed` has no literal closing it")?;
        let mut inner = vec!["{ \"include\": \"$self\" }".to_string()];
        if let Some(&(a, b, word)) = PAIRS.iter().find(|p| p.1 == closer) {
            inner.insert(0, format!("{{ \"include\": \"#{word}\" }}"));
            let stem = format!("punctuation.section.{word}");
            let pair = self.span("meta.block", &stem, &esc(a), &esc(b), &inner);
            self.nested.insert(word, pair);
        }
        let stem = "punctuation.definition.template-expression";
        let end = self.re(&E::Seq(close.to_vec()))?;
        Ok(vec![self.span(
            "meta.interpolation",
            stem,
            begin,
            &end,
            &inner,
        )])
    }

    /// `e` as one regex, or why it has none.
    fn re(&mut self, e: &E) -> Result<String, String> {
        if let Some(re) = atom(e) {
            return Ok(re);
        }
        Ok(match e {
            E::Seq(v) => v
                .iter()
                .map(|x| self.re(x))
                .collect::<Result<String, _>>()?,
            E::Alt(v) => {
                let each = v.iter().filter(|x| !refuses(x)).map(|x| self.re(x));
                let parts = each.collect::<Result<Vec<_>, _>>()?;
                if parts.is_empty() {
                    return Err("every alternative of a choice refuses".into());
                }
                format!("(?>{})", parts.join("|"))
            }
            E::Star(x) => format!("(?>(?:{})*)", self.re(x)?),
            E::Plus(x) => format!("(?>(?:{})+)", self.re(x)?),
            E::Opt(x) => format!("(?>(?:{})?)", self.re(x)?),
            E::And(x) => format!("(?={})", self.re(x)?),
            E::Not(x) => format!("(?!{})", self.re(x)?),
            E::Field(_, x) | E::Guard(_, x) => self.re(x)?,
            E::Ref(r) if self.path.contains(r) => {
                return Err(format!("rule `{}` calls itself", self.g.body(*r).name))
            }
            E::Ref(r) => {
                self.path.push(*r);
                let re = self.re(&self.g.body(*r).body);
                self.path.pop();
                re?
            }
            E::Throw(t) => return Err(format!("the refusal `{}` stands outside a choice", t.row)),
            E::Embed(_) => return Err("an `embed` stands outside an item".into()),
            _ => return Err("a syntactic form or a parser builtin has no regex".into()),
        })
    }

    /// The operators, the separators and the brackets, longest spelling first.
    fn tokens(&self) -> Vec<String> {
        let mut spelled: Vec<&str> = (self.g.tokens.iter().filter(|t| !t.2))
            .map(|t| t.1.as_str())
            .collect();
        spelled.sort_by_key(|s| std::cmp::Reverse(s.len()));
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for s in spelled {
            let scope = match PAIRS.iter().find(|p| p.0 == s || p.1 == s) {
                Some(p) => format!("punctuation.section.{}", p.2),
                None if SEPARATORS.contains(&s) => "punctuation.separator".into(),
                None => "keyword.operator".into(),
            };
            groups.entry(scope).or_default().push(esc(s));
        }
        (groups.iter())
            .map(|(scope, alts)| self.rule(scope, &alts.join("|")))
            .collect()
    }
}

fn behind_re(before: &Set) -> String {
    if before.contains(UNFIXED) || before.is_empty() {
        return String::new();
    }
    let alts: Vec<String> = (before.iter())
        .map(|l| match l.strip_suffix("\\b") {
            Some(word) => format!("\\b{word}\\s"),
            None => format!("{l}|{l}\\s"),
        })
        .collect();
    format!("(?<={})", alts.join("|"))
}

fn ahead_re(after: &Set) -> String {
    match after.is_empty() {
        true => String::new(),
        false => format!("(?=\\s*(?:{}))", Vec::from_iter(after.clone()).join("|")),
    }
}

/// Adds the regexes `e` can start with to `out`, and returns whether it can match
/// empty. The loader refuses left recursion, so the descent ends.
fn first_into(g: &Grammar, e: &E, out: &mut Set) -> bool {
    match e {
        E::Lit(l) if l.word && l.s.len() > 1 => {
            out.insert(format!("{}\\b", esc(&l.s)));
            false
        }
        E::Builtin(Builtin::Eof) => false,
        E::Builtin(Builtin::NoNl) | E::And(_) | E::Not(_) | E::TestFlag(_) => true,
        E::Seq(v) => v.iter().all(|x| first_into(g, x, out)),
        E::Alt(v) => v
            .iter()
            .fold(false, |empty, x| first_into(g, x, out) | empty),
        E::Star(x) | E::Opt(x) => first_into(g, x, out) || true,
        E::Plus(x) | E::Field(_, x) | E::Try(x) | E::Guard(_, x) => first_into(g, x, out),
        E::SetFlag(_, _, x) | E::Climb(x, _) => first_into(g, x, out),
        E::Many(x, u, _) | E::List(x, _, u) => {
            first_into(g, x, out) | first_into(g, u, out) || true
        }
        E::Embed(em) => first_into(g, &em.extent, out),
        E::Ref(r) => first_into(g, &g.body(*r).body, out),
        _ => {
            out.extend(atom(e));
            false
        }
    }
}

fn first(g: &Grammar, e: &E) -> Set {
    let mut set = Set::new();
    first_into(g, e, &mut set);
    set
}

/// The words of each context, in a stable order: what may precede them, and what may
/// follow. A reserved word has no context. A contextual word is read at the syntactic
/// rules, or at the lexical rules of a grammar with no other. Each set only grows and the
/// spellings bound it, so the loop ends.
fn words(g: &Grammar) -> BTreeMap<(String, Set), Set> {
    let n = g.rules.len();
    let (behind, follow) = (vec![Set::new(); n], vec![Set::new(); n]);
    let mut this = Words {
        g,
        behind,
        follow,
        sites: BTreeMap::new(),
        moved: true,
    };
    let level = g.rules.iter().any(|r| r.syntactic);
    let read = g.rules.iter().enumerate();
    let read = read.filter(|(_, r)| r.syntactic == level);
    while std::mem::take(&mut this.moved) {
        this.sites.clear();
        for (i, rule) in read.clone() {
            let (before, after) = (this.behind[i].clone(), this.follow[i].clone());
            this.walk(&rule.body, &before, &after);
        }
    }
    if !g.reserved.is_empty() {
        let free = this.sites.entry(Default::default()).or_default();
        free.extend(g.reserved.iter().cloned());
    }
    this.sites
}

struct Words<'g> {
    g: &'g Grammar,
    /// What can precede and follow each rule, by rule index.
    behind: Vec<Set>,
    follow: Vec<Set>,
    /// The words read, by the lookbehind before them and what may follow.
    sites: BTreeMap<(String, Set), Set>,
    moved: bool,
}

impl Words<'_> {
    fn walk(&mut self, e: &E, before: &Set, after: &Set) {
        match e {
            E::Lit(l) if l.word && l.s.len() > 1 && !self.g.reserved.contains(&*l.s) => {
                let behind = behind_re(before);
                if !behind.is_empty() || !after.is_empty() {
                    let key = (behind, after.clone());
                    self.sites.entry(key).or_default().insert(l.s.to_string());
                }
            }
            E::Seq(v) => self.seq(v, before, after),
            E::Alt(v) => v.iter().for_each(|x| self.walk(x, before, after)),
            E::Opt(x) | E::Field(_, x) | E::Guard(_, x) | E::Try(x) => self.walk(x, before, after),
            E::SetFlag(_, _, x) | E::Climb(x, _) => self.walk(x, before, after),
            E::And(x) => self.walk(x, before, &Set::new()),
            E::Star(x) | E::Plus(x) | E::Many(x, ..) | E::List(x, ..) => {
                let mut prev = Set::from([UNFIXED.to_string()]);
                let mut cont = first(self.g, e);
                cont.extend(after.iter().cloned());
                if let E::List(_, sep, _) = e {
                    prev = first(self.g, sep);
                    cont.extend(prev.iter().cloned());
                    prev.extend(before.iter().cloned());
                }
                self.walk(x, &prev, &cont);
            }
            E::Ref(r) => {
                let i = r.0 as usize;
                let size = self.behind[i].len() + self.follow[i].len();
                self.behind[i].extend(before.iter().cloned());
                self.follow[i].extend(after.iter().cloned());
                self.moved |= size != self.behind[i].len() + self.follow[i].len();
            }
            _ => {}
        }
    }

    /// Walks a sequence. A predicate `&(h rest)` before what starts as `h` does states
    /// what follows it, more exactly than what the sequence goes on to read, so only the
    /// predicate is walked. A predicate before a refusal names a mistake: no word is read.
    fn seq(&mut self, v: &[E], before: &Set, after: &Set) {
        let mut prev = before.clone();
        for (i, x) in v.iter().enumerate() {
            let mut cont = Set::new();
            if first_into(self.g, &E::Seq(v[i + 1..].to_vec()), &mut cont) {
                cont.extend(after.iter().cloned());
            }
            let guarded = i > 0
                && matches!(&v[i - 1], E::And(p) if matches!(strip(p),
                E::Seq(s) if first(self.g, &s[0]) == first(self.g, x)));
            let mistake = matches!(x, E::And(_)) && matches!(v.get(i + 1), Some(E::Throw(_)));
            if !guarded && !mistake && !matches!(x, E::Not(_)) {
                self.walk(x, &prev, &cont);
            }
            prev = match strip(x) {
                E::And(_) | E::Not(_) | E::TestFlag(_) | E::Builtin(Builtin::NoNl) => prev,
                E::Lit(_) => first(self.g, x),
                _ => Set::from([UNFIXED.to_string()]),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{language_configuration, textmate, Naming};

    /// Vyrn's naming conventions. The server's semantic tokens colour these names by
    /// what each resolves to.
    const VYRN: Naming = Naming {
        before_words: &[("\\b[A-Z][A-Za-z0-9_]*\\b", "support.type")],
        after_words: &[
            ("(?<=\\bfn\\s)[a-z_][A-Za-z0-9_]*", "entity.name.function"),
            (
                "\\b[a-z_][A-Za-z0-9_]*(?=\\s*\\(|\")",
                "entity.name.function",
            ),
            ("\\b[A-Za-z_][A-Za-z0-9_]*\\b", "variable.other"),
        ],
    };

    /// Fails when `name` under `editor/vscode/` differs from `fresh`.
    fn pin(name: &str, fresh: &str) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../editor/vscode");
        if std::env::var("VYRN_PIN").as_deref() == Ok("write") {
            std::fs::write(dir.join(name), fresh).expect("write the file");
        }
        let committed = std::fs::read_to_string(dir.join(name)).unwrap_or_default();
        assert!(
            committed == fresh,
            "{name} differs; rewrite it with VYRN_PIN=write"
        );
    }

    #[test]
    fn the_editor_files_are_fresh() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        for (file, naming, grammar, configuration) in [
            (
                "vyrn",
                &VYRN,
                "vyrn.tmLanguage.json",
                "language-configuration.json",
            ),
            (
                "syntax",
                &NONE,
                "syntax.tmLanguage.json",
                "syntax-language-configuration.json",
            ),
        ] {
            let path = dir.join(format!("{file}.syntax"));
            let text = std::fs::read_to_string(path).expect("the grammar reads");
            let g = crate::load(&text).expect("the grammar loads");
            pin(
                grammar,
                &textmate(&g, file, naming).expect("the grammar lowers"),
            );
            pin(configuration, &language_configuration(&g));
        }
    }

    const NONE: Naming = Naming {
        before_words: &[],
        after_words: &[],
    };

    #[test]
    fn a_shape_the_writer_cannot_lower_has_its_sentence() {
        for (rule, sentence) in [
            ("number = '1' number / '2'", "rule `number` calls itself"),
            (
                "number = embed('1', A) '2'",
                "an `embed` stands outside an item",
            ),
            ("number = ^R", "the refusal `R` stands outside a choice"),
            ("number = ^R / ^S", "every alternative of a choice refuses"),
            (
                "number = many '1' until '2'",
                "a parser builtin has no regex",
            ),
            (
                "string = 'q' (!'q' i)* 'q'\ni = 'x' embed('1', A)",
                "no literal closing it",
            ),
        ] {
            let text = format!("syntax T\nexpect ^E\nA = 'a'\n{rule}\n");
            let g = crate::load(&text).expect("it loads");
            let refused = textmate(&g, "t", &NONE)
                .err()
                .expect("it is refused")
                .sentence;
            assert!(refused.ends_with(sentence), "{refused}");
        }
    }

    #[test]
    fn a_contextual_word_takes_its_neighbours_from_the_rule() {
        let g = crate::load("syntax T\nexpect ^E\nA = 'as' B / ':' 'lazy' 'xy'\nB = 'bc'\n");
        let text = textmate(&g.expect("it loads"), "t", &NONE).expect("it lowers");
        assert!(text.contains(r#"\\b(?:as)\\b(?=\\s*(?:bc\\b))"#), "{text}");
        let lazy = r#"(?<=\\:|\\:\\s)\\b(?:lazy)\\b(?=\\s*(?:xy\\b))"#;
        assert!(text.contains(lazy), "{text}");
    }
}
