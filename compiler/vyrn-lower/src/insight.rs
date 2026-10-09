//! What a function costs, line by line: the allocations, copies, growing
//! builtins and checks the decided core carries, for `vyrn why --cost`.
//!
//! The table reads the bodies an emitter reads ([`World::body_at`]), so it
//! shows the check verdicts the emitter acts on. A fact sits at the line of
//! the row that carries it. Order is the source's: line, then the row's
//! position among that line's facts.

use std::collections::BTreeMap;

use vyrn_frontend::ast::{FnId, Program};
use vyrn_frontend::core::check::{Raises, Verdict, Why};
use vyrn_frontend::core::{rows, Body, Callee, Copied, Made, Name, NameInfo, Op, Rhs, St};

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
    /// The row makes a fresh block every time it runs: what, in words.
    Alloc(String),
    /// A rebuilding builtin (`push`, `tally`, `reserve`): a block only when
    /// the capacity runs out. The payload is the builtin's name.
    Grows(String),
    /// A copy; `implicit` when the core made it where the reader wrote none.
    Copy { what: Copied, implicit: bool },
    Check {
        raises: Raises,
        kept: bool,
        /// A kept check's reason: its short form and its sentence.
        why: Option<(&'static str, String)>,
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
                raises: c.rule,
                kept: c.verdict == Verdict::Kept,
                why: (c.why != Why::Unsaid).then(|| (c.why.short(), said(&c.why, body, world))),
            }),
        ),
        _ => return None,
    };
    kind.filter(|_| line > 0).map(|k| (line as u32, k))
}

/// A kept check's reason in a sentence, over the names the body's source spells.
fn said(why: &Why, body: &Body, world: &World) -> String {
    let spell = |n: Name| body.spoken(n);
    let name = |n: Option<Name>| n.map_or("the indexed place".into(), spell);
    match why {
        Why::Unsaid => String::new(),
        Why::Input(n) => format!("input: {} is read from outside", spell(*n)),
        Why::Callee(n, f) => {
            let from = (f.and_then(|f| world.fn_rows().get(f.index())))
                .map_or("a call".to_string(), |r| {
                    format!("{}(..)", body.spelled(&r.name))
                });
            let line = body.names[n.index()].line;
            match body.names[n.index()].source.starts_with('@') {
                true => format!("gap: callee fact: {from} at {line}"),
                false => format!("gap: callee fact: {} from {from} at {line}", spell(*n)),
            }
        }
        Why::Caller(n, goal) => {
            let open = body.id.is_some_and(|f| world.is_open(f));
            let exported = if open { " (exported)" } else { "" };
            format!(
                "gap: caller fact{exported}: {} is a parameter; {goal}",
                spell(*n)
            )
        }
        Why::Move(2, n) => format!("gap: move 2: a call may have resized {}", name(*n)),
        Why::Move(3, n) => format!(
            "gap: move 3: {} is a field, element or global read",
            name(*n)
        ),
        Why::Move(_, n) => format!(
            "gap: move 5: {} is the only name of the goal the loop writes",
            name(*n)
        ),
        Why::Unproved(goal) => format!("gap: unproved: {goal}"),
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
    /// Whether a copy among them is one the reader did not write.
    pub implicit: bool,
    /// What it does, each wording with how many times the line states it.
    whats: Vec<(String, usize)>,
    /// The distinct short reasons of the kept checks among them.
    shorts: Vec<&'static str>,
}

impl CostRow {
    /// How many facts the row states.
    pub fn count(&self) -> usize {
        self.whats.iter().map(|w| w.1).sum()
    }

    pub fn text(&self) -> String {
        let whats: Vec<String> = (self.whats.iter())
            .map(|(w, n)| {
                if *n > 1 {
                    format!("{w} x{n}")
                } else {
                    w.clone()
                }
            })
            .collect();
        whats.join(", ")
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
                Kind::Copy { what, implicit } => {
                    let what = match what {
                        Copied::Value => "a value",
                        Copied::Render => "a String render",
                    };
                    let how = if *implicit { " (implicit)" } else { "" };
                    (2, format!("{what}{how}"))
                }
                Kind::Alloc(w) => (0, w.clone()),
                Kind::Enters(c, from) => (0, format!("{c}(..) in {}", named(from))),
                Kind::Grows(b) => (1, format!("{b}(..)")),
                Kind::Check { raises, kept, why } => {
                    let what = match why {
                        Some((_, said)) => format!("{}  {said}", raises.census()),
                        None => raises.census().to_string(),
                    };
                    (3 + usize::from(!kept), what)
                }
            };
            tally[slot].0 += 1;
            tally[slot].1 += usize::from(f.depth > 0);
            let Some(verb) = f.kind.verb() else { continue };
            let row = shown.entry((f.line, verb)).or_default();
            if let Kind::Check {
                why: Some((short, _)),
                ..
            } = &f.kind
            {
                if !row.shorts.contains(short) {
                    row.shorts.push(short);
                }
            }
            row.depth = row.depth.max(f.depth);
            row.implicit |= matches!(f.kind, Kind::Copy { implicit: true, .. });
            match row.whats.iter_mut().find(|w| w.0 == what) {
                Some(w) => w.1 += 1,
                None => row.whats.push((what, 1)),
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
                count: row.count(),
                implicit: row.implicit,
                text: row.text(),
                short: row.shorts.join(", "),
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
    if let Some(what) = rhs.copies(names) {
        // A render of a String copies it by construction.
        let implicit =
            what == Copied::Render || bound.is_some_and(|n| names[n.index()].implicit_copy);
        return Some(Kind::Copy { what, implicit });
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
    rhs.allocates(names, bound).map(|m| Kind::Alloc(words(m)))
}

/// Whether `callee` rebuilds its receiver and may reallocate it: `push`,
/// `reserve`, `tally`, `@strAppend`. `clear` rebuilds and keeps the buffer.
fn grows(callee: &str) -> bool {
    use vyrn_frontend::prelude::{builtin, Length, Spec};
    builtin(callee).is_some_and(|b| b.spec == Some(Spec::Rebuilds) && b.length != Length::SetToZero)
}

/// What a row makes, in the words the report uses.
fn words(made: Made<'_>) -> String {
    use vyrn_frontend::ast::BinOp;
    use vyrn_frontend::core::Ctor;
    match made {
        Made::Prim(Op::Bin(BinOp::Add)) => "concatenation".to_string(),
        Made::Prim(Op::Closure(_)) | Made::Make(Ctor::Closure(_)) => "closure".to_string(),
        Made::Prim(_) => "value".to_string(),
        Made::Make(Ctor::Record(t, _)) => format!("record {t}"),
        Made::Make(Ctor::Array) => "array literal".to_string(),
        Made::Make(Ctor::Map) => "map literal".to_string(),
        Made::Make(Ctor::Try(t)) => format!("checked {t}"),
        Made::Call("@str") => "render".to_string(),
        Made::Call("@concat") => "concatenation".to_string(),
        Made::Call(c) => format!("{}(..)", c.trim_start_matches('@')),
    }
}
