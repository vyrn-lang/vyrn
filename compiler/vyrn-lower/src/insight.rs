//! What a function costs, line by line: the allocations, copies, growing
//! builtins and checks the decided core carries, for `vyrn why --cost`.
//!
//! The table reads the bodies an emitter reads ([`World::body_at`]), so it
//! shows the check verdicts the emitter acts on. A fact sits at the line of
//! the row that carries it. Order is the source's: line, then the row's
//! position among that line's facts.

use vyrn_frontend::ast::{FnId, Program};
use vyrn_frontend::core::check::{Raises, Verdict};
use vyrn_frontend::core::{rows, Body, Callee, Copied, Made, Name, NameInfo, Op, Rhs, St};

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
    Copy {
        what: Copied,
        implicit: bool,
    },
    Check {
        raises: Raises,
        kept: bool,
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
            }),
        ),
        _ => return None,
    };
    kind.filter(|_| line > 0).map(|k| (line as u32, k))
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
    // The first row under each name, as `Fns::id` finds it.
    let named = |name: &str| rows.iter().position(|r| r.name == name).map(FnId::nth);
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
            let file = world.body_at(*id).and_then(|b| b.file.clone())?;
            return world
                .allocates(*id)
                .then(|| Kind::Enters(frame.spelled(callee).to_string(), file));
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
