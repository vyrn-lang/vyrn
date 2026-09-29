//! The binary operator table: one row per [`BinOp`], read by every pass that
//! folds an operator, checks it at run time or reasons about its value.

use std::cmp::Ordering;

use crate::ast::BinOp;
use crate::trap::Rule;

/// A comparison of the left operand with the right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    /// `>`, or `>=` when not `strict`; `<` and `<=` are these `flipped`.
    Order { strict: bool, flipped: bool },
    /// `==`, or `!=` when `negated`.
    Equal { negated: bool },
}

#[derive(Debug, Clone, Copy)]
pub struct Row {
    /// The comparison the operator is, which yields a `Bool`, or a mask lane-wise.
    pub cmp: Option<Cmp>,
    /// The runtime checks the operator needs on an integer, the right operand's first.
    pub traps: &'static [Rule],
    /// The value on two `Int64`s; `None` where it traps or depends on the width.
    pub int: fn(i64, i64) -> Option<i64>,
    /// The value on two `Float`s, IEEE, so `/ 0.0` is an infinity or a NaN.
    pub float: fn(f64, f64) -> Option<f64>,
}

impl BinOp {
    #[rustfmt::skip]
    pub fn row(self) -> Row {
        type Int = fn(i64, i64) -> Option<i64>;
        type Float = fn(f64, f64) -> Option<f64>;
        fn row(traps: &'static [Rule], int: Int, float: Float) -> Row {
            Row { cmp: None, traps, int, float }
        }
        fn cmp(c: Cmp) -> Row {
            Row { cmp: Some(c), ..row(&[], |_, _| None, |_, _| None) }
        }
        let order = |strict, flipped| cmp(Cmp::Order { strict, flipped });
        let equal = |negated| cmp(Cmp::Equal { negated });
        match self {
            // Two's complement wrapping, as at run time.
            BinOp::Add => row(&[], |a, b| Some(a.wrapping_add(b)), |a, b| Some(a + b)),
            BinOp::Sub => row(&[], |a, b| Some(a.wrapping_sub(b)), |a, b| Some(a - b)),
            BinOp::Mul => row(&[], |a, b| Some(a.wrapping_mul(b)), |a, b| Some(a * b)),
            BinOp::Div => row(&[Rule::DivZero, Rule::DivOverflow], i64::checked_div, |a, b| Some(a / b)),
            // `MIN % -1` is 0, as wasm's `rem_s` gives.
            BinOp::Rem => row(&[Rule::RemZero], |a, b| if b == -1 { Some(0) } else { a.checked_rem(b) }, |_, _| None),
            BinOp::Gt => order(true, false),
            BinOp::GtEq => order(false, false),
            BinOp::Lt => order(true, true),
            BinOp::LtEq => order(false, true),
            BinOp::Eq => equal(false),
            BinOp::NotEq => equal(true),
            // And, or and xor on the `i64` representation agree with every width.
            BinOp::BitAnd => row(&[], |a, b| Some(a & b), |_, _| None),
            BinOp::BitOr => row(&[], |a, b| Some(a | b), |_, _| None),
            BinOp::BitXor => row(&[], |a, b| Some(a ^ b), |_, _| None),
            BinOp::Shl | BinOp::Shr => row(&[Rule::ShiftRange], |_, _| None, |_, _| None),
            BinOp::And | BinOp::Or | BinOp::Match => row(&[], |_, _| None, |_, _| None),
        }
    }

    pub fn compare(self) -> Option<Cmp> {
        self.row().cmp
    }
}

impl Cmp {
    /// Whether the comparison holds where the operands order as `o`; `None`
    /// is an unordered pair (a NaN), which only `!=` holds for.
    pub fn holds(self, o: Option<Ordering>) -> bool {
        match self {
            Cmp::Order { strict, flipped } => o
                .map(|o| if flipped { o.reverse() } else { o })
                .is_some_and(|o| o == Ordering::Greater || !strict && o == Ordering::Equal),
            Cmp::Equal { negated } => (o == Some(Ordering::Equal)) != negated,
        }
    }

    /// The comparison with its operands swapped: `n < v` is `v > n`.
    pub fn converse(mut self) -> Cmp {
        if let Cmp::Order { flipped, .. } = &mut self {
            *flipped = !*flipped;
        }
        self
    }

    /// The inclusive `(min, max)` that `v OP n` places on `v`, saturating at
    /// the `i64` edges.
    pub fn bounds(self, n: i64) -> (Option<i64>, Option<i64>) {
        let Cmp::Order { strict, flipped } = self else {
            return (None, None);
        };
        let s = i64::from(strict);
        match flipped {
            false => (Some(n.saturating_add(s)), None),
            true => (None, Some(n.saturating_sub(s))),
        }
    }
}
