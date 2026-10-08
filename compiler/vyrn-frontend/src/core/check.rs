//! The runtime checks a row can carry ([`super::St::Check`]).

use super::{Name, Place, Val};
use crate::ast::FnId;
use crate::trap::Rule;

/// One runtime check: the trap it raises, what it compares, where, and
/// whether the emitter runs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub rule: Raises,
    pub guard: Guard,
    pub site: Site,
    pub verdict: Verdict,
    /// Why a [`Verdict::Kept`] row stays, written with the verdict
    /// (`vyrn_lower::elide`); [`Why::Unsaid`] for a proved row.
    pub why: Why,
}

/// The reason a pass could not prove a check, for `vyrn why --cost` and the
/// editor. Each variant names the name whose origin decided it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Why {
    /// No pass gave a reason: the row is proved, or no pass ran.
    #[default]
    Unsaid,
    /// The name's value comes from outside the program.
    Input(Name),
    /// The goal needs a fact about a call's result: the name it binds and
    /// the function, when the call is to a declared one.
    Callee(Name, Option<FnId>),
    /// The goal needs a fact about a parameter: the parameter, and the goal
    /// over source names.
    Caller(Name, std::sync::Arc<str>),
    /// A kind of gap the research names by its move number: 2 a length a
    /// call may have changed, 3 a field, element or global read, 5 a goal
    /// over one counter and names the loop leaves alone.
    Move(u8, Option<Name>),
    /// No rule applies; the text is the goal the prover could not show.
    Unproved(std::sync::Arc<str>),
}

impl Why {
    /// The reason in two words, for the editor's end-of-line hint.
    pub fn short(&self) -> &'static str {
        match self {
            Why::Unsaid => "",
            Why::Input(_) => "input",
            Why::Callee(..) => "callee fact",
            Why::Caller(..) => "caller fact",
            Why::Move(2, _) => "move 2",
            Why::Move(3, _) => "move 3",
            Why::Move(..) => "move 5",
            Why::Unproved(_) => "unproved",
        }
    }
}

/// What a failed check raises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Raises {
    /// A row of the trap table.
    Row(Rule),
    /// The `where` failure of the checked record's type, which its
    /// constructor raises ([`crate::trap::validation`]).
    Where,
}

impl Raises {
    /// The census name, for a diagnostic and the records.
    pub fn census(self) -> &'static str {
        match self {
            Raises::Row(r) => r.census(),
            Raises::Where => "where",
        }
    }
}

/// What a pass decided about a check. `vyrn_lower::check::state` states every row
/// [`Verdict::Kept`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The emitter runs the check.
    Kept,
    /// A pass proved the check cannot fail, and the emitter runs nothing. A
    /// proved [`Guard::Range`] is refused: `std/runtime`'s `bytesOf` checks
    /// its range inside and has no unchecked form.
    Proved,
}

/// What a check compares. The check traps when the condition fails.
#[derive(Debug, Clone, PartialEq)]
pub enum Guard {
    /// `0 <= i < length(base)`.
    Index(Place, Val),
    /// `0 <= i` and `i + span <= length(base)`: a SIMD load or store of
    /// `span` elements.
    Span(Place, Val, i64),
    /// `0 <= from <= to <= length(s)`: `bytes(s, from, to)`, checked by
    /// `std/runtime`'s `bytesOf`.
    Range(Val, Val, Val),
    /// The divisor is not zero.
    NonZero(Val),
    /// Not the minimum of a signed width over `-1`: the dividend, the
    /// divisor, and the width in bits.
    NoOverflow(Val, Val, u8),
    /// `0 <= k < bits`: a shift amount.
    Shift(Val, u8),
    /// The record name satisfies its type's `where` rule: the row after the
    /// check calls the type's constructor on it.
    Rule(Name),
}

/// Where a check stands in the source: the line of the row it guards and the
/// check's position among the checks of that line in its body, from 0.
///
/// The pair names the same check in every compile of the same source, and
/// with the body's file and name it names one check in the program. The AST
/// states no column for an operator or a call, so the ordinal stands in for
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Site {
    pub line: usize,
    pub ordinal: u32,
}
