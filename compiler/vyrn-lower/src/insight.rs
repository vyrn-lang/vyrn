//! What a function costs, line by line: the allocations, copies, growing
//! builtins and checks the decided core carries, for `vyrn why --cost`.
//!
//! The table reads the bodies an emitter reads ([`World::body_at`]), so it
//! shows the check verdicts the emitter acts on. A fact sits at the line of
//! the row that carries it. Order is the source's: line, then the row's
//! position among that line's facts.

use std::collections::BTreeMap;

use vyrn_frontend::ast::{FnId, Program};
use vyrn_frontend::core::check::{Guard, Verdict, Why};
use vyrn_frontend::core::{
    rows, Body, BorrowKind, Callee, Copied, Ctor, Made, Name, NameInfo, Op, Rhs, St,
};

use vyrn_frontend::symbols::{CostLine, FnCost};

use crate::World;

/// One cost a core row carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    pub line: u32,
    /// The fact's position among this body's facts on `line`, from 0.
    pub ordinal: u32,
    /// How many loops enclose the row.
    pub depth: u32,
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// The row makes a fresh block every time it runs: the type it makes,
    /// spelled.
    Alloc(String),
    /// A rebuilding builtin (`push`, `tally`, `reserve`): a block only when
    /// the capacity runs out. The payload is the builtin's name.
    Grows(String),
    /// A copy: the type it copies, spelled, and why the core made it where
    /// the reader wrote none (empty when the reader wrote it).
    Copy { ty: String, why: &'static str },
    Check {
        /// What the check guards: `index`, `slice`, `division`, `shift`,
        /// `where rule` or `where clause`.
        what: &'static str,
        kept: bool,
        /// A kept check's reason in source terms; empty when no pass gave one.
        why: String,
    },
    /// A call into a function outside the root file whose effect set holds
    /// `alloc`. Its blocks count at this line: the callee, and its file.
    Enters(String, String),
}

/// What a fact does, in the order a reader asks: what is copied, what is
/// allocated, what is entered, what grows, what is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Verb {
    Copies,
    Allocates,
    Enters,
    Grows,
    Keeps,
}

impl Verb {
    /// The verb's key: a profile's sites ([`crate::lastrun`]) name a row by it.
    pub fn word(self) -> &'static str {
        match self {
            Verb::Copies => "copies",
            Verb::Allocates => "allocates",
            Verb::Enters => "enters",
            Verb::Grows => "grows",
            Verb::Keeps => "check kept",
        }
    }
}

impl Kind {
    /// The verb a fact prints under, `None` for a proved check.
    pub fn verb(&self) -> Option<Verb> {
        match self {
            Kind::Copy { .. } => Some(Verb::Copies),
            Kind::Alloc(_) => Some(Verb::Allocates),
            Kind::Enters(..) => Some(Verb::Enters),
            Kind::Grows(_) => Some(Verb::Grows),
            Kind::Check { kept, .. } => kept.then_some(Verb::Keeps),
        }
    }
}

/// Every fact of the frame `body`, ordered by line and then by the row's
/// position. A row with no source line (line 0) carries none. A lifted
/// lambda is a frame of its own ([`root`] joins it to its function).
pub fn facts(body: &Body, world: &World) -> Vec<Fact> {
    let mut out: Vec<Fact> = Vec::new();
    for (s, depth) in rows(&body.stmts) {
        if let Some((line, kind)) = row(s, body, world) {
            out.push(Fact {
                line,
                ordinal: 0,
                depth,
                kind,
            });
        }
    }
    number(&mut out);
    out
}

/// What the row `s` of `body` costs, and the source line it costs at. A row
/// with no source line (line 0) costs nothing.
pub fn row(s: &St, body: &Body, world: &World) -> Option<(u32, Kind)> {
    let (line, kind) = match s {
        St::Let(n, rhs) => (
            body.names[n.index()].line,
            row_kind(rhs, &body.names, Some(*n), world, body),
        ),
        St::Do { rhs, line, .. } => (*line, row_kind(rhs, &body.names, None, world, body)),
        St::Check(c) => (
            c.site.line,
            Some(Kind::Check {
                what: guarded(&c.guard),
                kept: c.verdict == Verdict::Kept,
                why: said(&c.why, body, world),
            }),
        ),
        _ => return None,
    };
    kind.filter(|_| line > 0).map(|k| (line as u32, k))
}

/// What `guard` protects, in the words the hints use.
fn guarded(guard: &Guard) -> &'static str {
    match guard {
        Guard::Index(..) | Guard::Span(..) => "index",
        Guard::Range(..) => "slice",
        Guard::NonZero(_) | Guard::NoOverflow(..) => "division",
        Guard::Shift(..) => "shift",
        Guard::Rule(_) => "where rule",
        Guard::Clause { .. } => "where clause",
    }
}

/// A kept check's reason, over the names the body's source spells: what the
/// check needs, why the compiler cannot show it, and what would let it drop
/// the check, where something would. Empty for [`Why::Unsaid`].
fn said(why: &Why, body: &Body, world: &World) -> String {
    let temp = |n: Name| body.names[n.index()].source.starts_with('@');
    // A temporary reads `the value of line N`, which is no code.
    let code = |n: Name| match temp(n) {
        true => body.spoken(n).into_owned(),
        false => format!("`{}`", body.spoken(n)),
    };
    let needs = |goal: &Option<std::sync::Arc<str>>| match goal {
        Some(g) => format!("it needs {g}, and "),
        None => String::new(),
    };
    match why {
        Why::Unsaid => String::new(),
        Why::Input(n) => format!("{} is read from input", code(*n)),
        Why::Callee(n, f, goal) => {
            let from = (f.and_then(|f| world.fn_rows().get(f.index())))
                .map_or("a call".to_string(), |r| {
                    format!("`{}(..)`", body.spelled(&r.name))
                });
            let line = body.names[n.index()].line;
            let who = match temp(*n) && goal.is_some() {
                true => "that value".to_string(),
                false => code(*n),
            };
            let states = match goal {
                Some(_) => "does not state it",
                None => "states nothing about it",
            };
            format!(
                "{}{who} is the result of {from} at line {line}, whose signature {states}",
                needs(goal)
            )
        }
        Why::Caller(n, goal, clause) => {
            let read = matches!(
                &body.names[n.index()].borrow_kind,
                Some(BorrowKind::Param { cap: "read", .. })
            );
            let fix = match clause {
                Some(c) => format!(
                    "; add `where {c}` to `{0}`, and each call checks it where the caller cannot \
                     show it",
                    body.spoken(*n)
                ),
                None if !read => {
                    "; only a `read` parameter takes a `where` clause that could state it".into()
                }
                None => String::new(),
            };
            format!(
                "it needs {goal}, and `{}` is a parameter, so the function cannot know what its \
                 callers pass{fix}",
                body.spoken(*n)
            )
        }
        Why::Resized(n) => format!("a call in the loop may resize {}", code(*n)),
        Why::Place(n) => format!(
            "{} is read from a field, an element or module state, and the compiler keeps no \
             facts about such reads",
            n.map_or("the array".into(), code)
        ),
        Why::Counter(n, goal) => format!(
            "{}the loop moves {} without a test of that; a loop condition that implies it drops \
             the check",
            needs(goal),
            code(*n)
        ),
        Why::Unproved(goal) => format!("it needs {goal}, and the compiler cannot show that here"),
        Why::Rule => "the compiler proves only `where` rules that state equal lengths".into(),
        Why::Nonlinear => {
            "the compiler proves only sums of names and lengths times constants, and this check \
             is no such sum"
                .into()
        }
    }
}

/// Orders `facts` by line and numbers each within its line.
fn number(facts: &mut [Fact]) {
    facts.sort_by_key(|f| f.line);
    let mut ordinal = 0;
    for i in 0..facts.len() {
        let same = i > 0 && facts[i - 1].line == facts[i].line;
        ordinal = if same { ordinal + 1 } else { 0 };
        facts[i].ordinal = ordinal;
    }
}

/// The facts of each function the root file declares, as `(source function,
/// facts)` in function-table order, a lifted lambda counted with its function.
/// The first instance answers for a generic function, and a function with no
/// fact is left out. A `test` or `bench` block is no function of `program`.
pub fn root(program: &Program, world: &World) -> Vec<(FnId, Vec<Fact>)> {
    let rows = world.fn_rows();
    let named = |name: &str| world.fn_id(name);
    let source = |id: FnId| rows[id.index()].generic.as_ref().map_or(id, |g| g.0);
    let mut out: Vec<(FnId, FnId, Vec<Fact>)> = Vec::new();
    for i in 0..rows.len() {
        let Some(body) = world.body_at(FnId::nth(i)).filter(|b| b.file.is_none()) else {
            continue;
        };
        // `f@lambda:L:C` is the lambda lifted out of `f`.
        let Some(holder) = named(body.name.split("@lambda").next().unwrap_or_default()) else {
            continue;
        };
        if source(holder).index() >= program.functions.len() {
            continue;
        }
        let facts = facts(body, world);
        match out.iter_mut().find(|r| r.0 == source(holder)) {
            Some(r) if r.1 == holder => r.2.extend(facts),
            Some(_) => {}
            None => out.push((source(holder), holder, facts)),
        }
    }
    out.into_iter()
        .map(|(id, _, mut facts)| {
            number(&mut facts);
            (id, facts)
        })
        .filter(|(_, facts)| !facts.is_empty())
        .collect()
}

/// One printed row of `why --cost`: what one verb does on one line of a function.
#[derive(Debug, Clone, Default)]
pub struct CostRow {
    /// The deepest loop nest among the line's facts of this verb.
    pub depth: u32,
    /// What it does: the noun (a type, a callee, a check's kind), the reason, and how many
    /// times the line states the pair.
    whats: Vec<(String, String, usize)>,
}

impl CostRow {
    /// Each noun as `say` spells it, with its count: `String x3`.
    fn nouns(&self, say: impl Fn(&str) -> String) -> Vec<String> {
        (self.whats.iter())
            .map(|(noun, _, n)| match n {
                1 => say(noun),
                n => format!("{} x{n}", say(noun)),
            })
            .collect()
    }

    /// The row in a few plain words, for an inlay label: `allocates Folded`,
    /// `index checked`, `push may allocate`.
    pub fn label(&self, verb: Verb) -> String {
        let nouns = self.nouns(str::to_string).join(", ");
        match verb {
            Verb::Copies => format!("copies {nouns}"),
            Verb::Allocates => format!("allocates {nouns}"),
            Verb::Enters => format!("{nouns} allocates"),
            Verb::Grows => format!("{nouns} may allocate"),
            Verb::Keeps => format!("{nouns} checked"),
        }
    }

    /// The row in full, with code in backticks: what it does and why, for `why --cost`
    /// and a hover.
    pub fn text(&self, verb: Verb) -> String {
        let code = |n: &str| format!("`{n}`");
        let call = |n: &str| format!("`{n}(..)`");
        let reasoned = |nouns: Vec<String>, sep: &str| {
            (nouns.into_iter().zip(&self.whats))
                .map(|(noun, (_, why, _))| match why.is_empty() {
                    true => noun,
                    false => format!("{noun}{sep}{why}"),
                })
                .collect::<Vec<_>>()
        };
        match verb {
            Verb::Copies => format!("copies {}", reasoned(self.nouns(code), ": ").join("; ")),
            Verb::Allocates => format!("allocates {}", self.nouns(code).join(", ")),
            Verb::Enters => format!(
                "allocates in {}",
                reasoned(self.nouns(call), " from ").join(", ")
            ),
            Verb::Grows => format!(
                "{} may allocate when it outgrows its capacity",
                self.nouns(call).join(", ")
            ),
            Verb::Keeps => reasoned(self.nouns(|n| format!("{n} check")), ": ").join("; "),
        }
    }
}

/// The rows of one function, by line and then by verb.
pub type CostFn = (FnId, BTreeMap<(u32, Verb), CostRow>);

/// The rows of each function and the summary counts: `(rows, rows in loops)` of allocating,
/// growing, copying, kept and proved.
pub type CostTable = (Vec<CostFn>, [(usize, usize); 5]);

/// The rows of `why --cost` for each function of the root file ([`root`]), in function-table
/// order. `named` spells the file a call enters.
pub fn table(program: &Program, world: &World, named: &dyn Fn(&str) -> String) -> CostTable {
    let mut tally = [(0usize, 0usize); 5];
    let mut out = Vec::new();
    for (id, facts) in root(program, world) {
        let mut shown: BTreeMap<(u32, Verb), CostRow> = BTreeMap::new();
        for f in &facts {
            let (slot, what) = match &f.kind {
                Kind::Copy { ty, why } => (2, (ty.clone(), why.to_string())),
                Kind::Alloc(w) => (0, (w.clone(), String::new())),
                Kind::Enters(c, from) => (0, (c.clone(), named(from))),
                Kind::Grows(b) => (1, (b.clone(), String::new())),
                Kind::Check { what, kept, why } => {
                    (3 + usize::from(!kept), (what.to_string(), why.clone()))
                }
            };
            tally[slot].0 += 1;
            tally[slot].1 += usize::from(f.depth > 0);
            let Some(verb) = f.kind.verb() else { continue };
            let row = shown.entry((f.line, verb)).or_default();
            row.depth = row.depth.max(f.depth);
            match row
                .whats
                .iter_mut()
                .find(|w| (&w.0, &w.1) == (&what.0, &what.1))
            {
                Some(w) => w.2 += 1,
                None => row.whats.push((what.0, what.1, 1)),
            }
        }
        out.push((id, shown));
    }
    (out, tally)
}

/// [`table`] for the editor: each function's rows in the frontend's shape. A call into another
/// file is named by the file's last path segment, `std/strings` by its import.
pub fn fn_costs(program: &Program, world: &World) -> Vec<FnCost> {
    let std = vyrn_frontend::manifest::std_root();
    let (rows, _) = table(program, world, &|file| match std
        .as_deref()
        .and_then(|s| file.strip_prefix(s))
    {
        Some(m) => format!("std{}", m.trim_end_matches(".vyrn")),
        None => file.rsplit('/').next().unwrap_or(file).to_string(),
    });
    let lines = |shown: BTreeMap<(u32, Verb), CostRow>| {
        (shown.into_iter())
            .map(|((line, verb), row)| CostLine {
                line: line as usize,
                verb: verb.word(),
                depth: row.depth,
                label: row.label(verb),
                text: row.text(verb),
            })
            .collect()
    };
    (rows.into_iter())
        .filter(|(_, shown)| !shown.is_empty())
        .map(|(id, shown)| {
            let f = &program.functions[id.index()];
            FnCost {
                name: f.name.clone(),
                line: f.line,
                lines: lines(shown),
            }
        })
        .collect()
}

/// What the row costs, if anything. A copy, a growing builtin and a call
/// into another file's allocating function come before the allocation test,
/// because each of them is also a row that binds an owned result.
fn row_kind(
    rhs: &Rhs,
    names: &[NameInfo],
    bound: Option<Name>,
    world: &World,
    frame: &Body,
) -> Option<Kind> {
    let sp = frame.speech();
    let ty = |made: Made<'_>| match bound {
        Some(n)
            if !matches!(
                made,
                Made::Prim(Op::Closure(_)) | Made::Make(Ctor::Closure(_))
            ) =>
        {
            sp.ty(&names[n.index()].ty).to_string()
        }
        _ => words(made),
    };
    if let Some(what) = rhs.copies(names) {
        let why = match what {
            Copied::Render => "interpolating a `String` copies it",
            Copied::Value if bound.is_some_and(|n| names[n.index()].implicit_copy) => {
                "the value is borrowed, and its new holder must own it"
            }
            Copied::Value => "",
        };
        let copied = match (what, bound) {
            (Copied::Render, _) => "String".to_string(),
            (Copied::Value, Some(n)) => sp.ty(&names[n.index()].ty).to_string(),
            (Copied::Value, None) => "value".to_string(),
        };
        return Some(Kind::Copy { ty: copied, why });
    }
    if let Rhs::Call { callee, kind, .. } = rhs {
        if grows(callee) {
            return Some(Kind::Grows(callee.trim_start_matches('@').to_string()));
        }
        if let Callee::Fn(id) = kind {
            let from = world.allocating_file(*id)?;
            return Some(Kind::Enters(
                frame.spelled(callee).to_string(),
                from.to_string(),
            ));
        }
        // A user body states its own allocations.
        if kind.declared() || kind.value().is_some() {
            return None;
        }
    }
    rhs.allocates(names, bound).map(|m| Kind::Alloc(ty(m)))
}

/// Whether `callee` rebuilds its receiver and may reallocate it: `push`,
/// `reserve`, `tally`, `@strAppend`. `clear` rebuilds and keeps the buffer.
fn grows(callee: &str) -> bool {
    use vyrn_frontend::prelude::{builtin, ShapeMap, Spec};
    builtin(callee)
        .is_some_and(|b| b.spec == Some(Spec::Rebuilds) && b.shape_map != ShapeMap::Empties)
}

/// What a row makes when no name binds it, in the words the report uses.
fn words(made: Made<'_>) -> String {
    use vyrn_frontend::ast::BinOp;
    match made {
        Made::Prim(Op::Bin(BinOp::Add)) | Made::Call("@str" | "@concat") => "String".to_string(),
        Made::Prim(Op::Closure(_)) | Made::Make(Ctor::Closure(_)) => "closure".to_string(),
        Made::Prim(_) => "value".to_string(),
        Made::Make(Ctor::Record(t, _) | Ctor::Try(t)) => t.to_string(),
        Made::Make(Ctor::Array) => "Array".to_string(),
        Made::Make(Ctor::Map) => "Map".to_string(),
        Made::Call(c) => format!("{}(..)", c.trim_start_matches('@')),
    }
}
