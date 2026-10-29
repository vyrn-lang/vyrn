//! A grammar engine: reads a `.syntax` file and interprets it over a source text.
//!
//! A grammar states shape only: a token table, rules and the rows it refuses
//! with. Lexical rules (lower-case names) match characters with full PEG
//! backtracking and skip no trivia. Syntactic rules (upper-case names) skip the
//! `trivia` rule before each terminal and commit as Parsec does: once a sequence
//! has consumed input, a later element that fails is an error at that element,
//! not a backtrack. `@e` backtracks over a committed failure; `&e` and `!e`
//! never commit. A refusal is `^Row(hole=source)@anchor`: it names a row and
//! fills its holes, and the host renders the sentence. `compiler/vyrn-syntax/
//! syntax.syntax` is the file format, written in itself.
//!
//! Entry points: [`load`] reads a grammar, [`parse`] runs one of its rules over a
//! text. `vyrn.syntax` beside this file is Vyrn's grammar, and the `syntaxgate`
//! test holds the hand lexer and parser equal to it.

mod load;
mod run;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

pub use load::load;
pub use run::{parse, Diag, HoleValue, Node, NodeKind, Parse, Stage};

/// One grammar expression.
#[derive(Debug, Clone)]
enum E {
    Seq(Vec<E>),
    Alt(Vec<E>),
    Star(Box<E>),
    Plus(Box<E>),
    Opt(Box<E>),
    And(Box<E>),
    Not(Box<E>),
    Try(Box<E>),
    Lit(Lit),
    /// `split '>'`: the first character of a punctuation token, so `>>` closes
    /// two generic lists.
    Split(Rc<str>),
    /// A character class, negated when the flag is set.
    Class(Rc<[(char, char)]>, bool),
    Any,
    Ref(RuleId),
    Builtin(Builtin),
    /// `label:e`: records where `e` matched, for a hole or an anchor.
    Field(Rc<str>, Box<E>),
    Throw(Rc<Throw>),
    /// `~^Row`: report the row, skip one token, and succeed.
    Skip(Rc<Throw>),
    Climb(Box<E>, Rc<[Level]>),
    /// `+flag(e)` sets and `-flag(e)` clears a context flag while `e` runs.
    SetFlag(usize, bool, Box<E>),
    /// `?flag`: matches empty when the flag is set.
    TestFlag(usize),
    /// `many X until C [recover ..]`: while C does not match, X must.
    Many(Box<E>, Box<E>, Recover),
    /// `list X sep S until C`: while C does not match, X must; then S or stop.
    List(Box<E>, Box<E>, Box<E>),
    Embed(Rc<Embed>),
    /// An alternative that can start only with one of the bytes in the set.
    Guard(Rc<ByteSet>, Box<E>),
}

/// A set of bytes, one bit each.
type ByteSet = [u64; 4];

/// A literal. In a syntactic rule a word literal needs a non-word character
/// after it, and a punctuation literal yields to a longer token spelling.
#[derive(Debug, Clone)]
struct Lit {
    s: Rc<str>,
    word: bool,
    /// The token spellings that start with this one and are longer.
    longer: Rc<[Rc<str>]>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Builtin {
    Eof,
    /// Matches empty when the trivia ahead holds no newline.
    NoNl,
    Alpha,
    Alnum,
    Digit,
    Hex,
    Ws,
    /// A word the token table reserves.
    Reserved,
    /// The longest punctuation spelling of the token table.
    Punct,
}

/// Where a failed item of a `many` loop resumes.
#[derive(Debug, Clone)]
enum Recover {
    None,
    /// At a token on a new line, after a `;`, or before the block's `}`.
    Stmt,
    /// At the next token that matches the expression, outside braces.
    Decl(Box<E>),
}

/// One precedence level of a `climb`, loosest first.
#[derive(Debug)]
struct Level {
    right: bool,
    ops: Vec<Lit>,
}

/// `embed(extent, Rule, wrap=^Row, trailing=^Row)`: matches `extent`, then
/// parses exactly that text with `Rule`, after the grammar's `scan` rule.
#[derive(Debug)]
struct Embed {
    extent: E,
    inner: RuleId,
    /// The row an error inside the island is wrapped in.
    wrap: Option<Rc<Throw>>,
    /// The row for text the inner rule leaves unparsed.
    trailing: Option<Rc<Throw>>,
}

/// What fills a hole of a refusal.
#[derive(Debug, Clone)]
enum HoleSrc {
    /// The text a label matched.
    Label(Rc<str>),
    Text(Rc<str>),
    /// The character here; `.:?` spells it as Rust's `Debug`.
    Char {
        debug: bool,
    },
    /// The host's spelling of the token here.
    Found,
    /// The token name of the literal that failed (the `expect` row only).
    Token,
    /// The error an `embed` island raised (its `wrap` row only).
    Inner,
    /// The `embed` extent, trimmed (its `trailing` row only).
    Extent,
}

/// Where a refusal points.
#[derive(Debug, Clone, Copy, PartialEq)]
enum At {
    /// The token here, after trivia.
    Here,
    /// The token after the one here.
    After,
    /// Where the label of this index in [`Throw::labels`] matched.
    Label(usize),
}

/// A refusal: a row, its holes, and where its line and column point.
#[derive(Debug)]
struct Throw {
    row: Rc<str>,
    holes: Vec<(Rc<str>, HoleSrc)>,
    line: At,
    col: At,
    /// The labels of the rule the refusal sits in, by [`At::Label`] index.
    labels: Vec<Rc<str>>,
}

/// The index of a rule in [`Grammar::rules`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RuleId(u32);

#[derive(Debug)]
struct Rule {
    name: String,
    syntactic: bool,
    body: E,
    /// Counts toward the nesting limit.
    nest: bool,
    /// What a committed failure of this rule states.
    fail: Option<Rc<Throw>>,
}

/// A loaded grammar.
pub struct Grammar {
    rules: Vec<Rule>,
    by_name: HashMap<String, RuleId>,
    /// Token name and spelling, in file order, and whether a keyword reserves it.
    tokens: Vec<(String, String, bool)>,
    reserved: HashSet<String>,
    flags: Vec<String>,
    trivia: Option<RuleId>,
    token_rule: Option<RuleId>,
    scan_rule: Option<RuleId>,
    nest_limit: Option<(u32, Rc<Throw>)>,
    /// The row a committed literal that fails states.
    expect: Rc<Throw>,
    /// The rows a refusal of the grammar names.
    stated: BTreeSet<Rc<str>>,
    /// The rows a `row` line names: the host's tree builder raises them.
    builder: BTreeSet<Rc<str>>,
}

impl Grammar {
    /// Returns the rule named `name`.
    pub fn rule(&self, name: &str) -> Option<RuleId> {
        self.by_name.get(name).copied()
    }

    /// Returns the name of rule `id`.
    pub fn rule_name(&self, id: RuleId) -> &str {
        &self.rules[id.0 as usize].name
    }

    /// Returns the rows a refusal of the grammar names, in name order.
    pub fn stated_rows(&self) -> impl Iterator<Item = &str> {
        self.stated.iter().map(|r| &**r)
    }

    /// Returns the rows a `row` line names, which the host raises as a value or
    /// tree check, in name order. No row is both stated and a builder's.
    pub fn builder_rows(&self) -> impl Iterator<Item = &str> {
        self.builder.iter().map(|r| &**r)
    }

    fn body(&self, id: RuleId) -> &Rule {
        &self.rules[id.0 as usize]
    }
}

/// Whether `c` continues a word.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grammar(file: &str) -> (Grammar, String) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
        let text = std::fs::read_to_string(path).expect("the grammar reads");
        (load(&text).expect("the grammar loads"), text)
    }

    fn accepts(meta: &Grammar, text: &str) -> Vec<Diag> {
        let file = meta.rule("file").expect("syntax.syntax names a file rule");
        parse(meta, file, text, &|_: &str| String::new()).errors
    }

    #[test]
    fn the_meta_grammar_reads_itself_and_vyrn() {
        let (meta, own) = grammar("syntax.syntax");
        let (_, vyrn) = grammar("vyrn.syntax");
        assert!(accepts(&meta, &own).is_empty());
        assert!(accepts(&meta, &vyrn).is_empty());
    }

    #[test]
    fn the_meta_grammar_refuses_an_unclosed_group() {
        let (meta, _) = grammar("syntax.syntax");
        assert!(!accepts(&meta, "a = ( 'x'\n").is_empty());
        assert!(!accepts(&meta, "a = ^\n").is_empty());
    }

    #[test]
    fn a_left_recursive_rule_does_not_load() {
        let err = load("expect ^E\nA = A 'x' / 'y'\n").err().expect("refused");
        assert!(err.contains("left recursion"), "{err}");
    }

    #[test]
    fn a_site_bound_hole_source_loads_only_at_its_site() {
        let err = load("expect ^E\na = ^R(x=inner)\n").err().expect("refused");
        assert!(err.contains("`inner` fills no hole"), "{err}");
    }
}
