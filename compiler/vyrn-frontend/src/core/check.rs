//! The runtime checks a row can carry ([`super::St::Check`]).

use super::{Place, Val};
use crate::trap::Rule;

/// One runtime check: the trap it raises, what it compares, where, and
/// whether the emitter runs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub rule: Rule,
    pub guard: Guard,
    pub site: Site,
    pub verdict: Verdict,
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
