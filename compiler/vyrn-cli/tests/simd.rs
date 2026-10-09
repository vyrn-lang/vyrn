//! A vector load or store is bounds-checked once for the whole vector, not once
//! per lane.
//!
//! One check and four accept and reject the same programs with the same message,
//! so only the emitted code differs. Every count is read out of `vyrn emit-wat`:
//! a structural count, not a duration, so a loaded machine cannot make it flaky.
//! `examples/simdoob.vyrn` and `examples/simdoobstore.vyrn` prove the trap.

mod common;
use common::*;

/// [`common::wat_func_containing`], asserting the module interns the bounds
/// wording, or the count is counting a check that cannot report.
fn wat_func_containing(src: &str, marker: &str) -> String {
    let dir = scratch("simd-wat");
    let body = common::wat_func_containing(&dir, "vec", src, marker);
    assert!(
        common::wat_of(&dir, "vec", src).contains("error: array index "),
        "the module does not intern the bounds wording, so the count is counting \
         a check that cannot report"
    );
    body
}

const PROLOGUE: &str = "fn main() -> Int64 {\n\
                        let xs: Array<Float32> = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]\n\
                        print(read(xs, 0))\n\
                        return 0\n\
                        }\n";

/// The load holds `bounds_check_span`'s single branch (the signed pair
/// `i < 0 || i > len - 4` joined by `i32.or`), no scalar `i64.ge_u`, and one
/// `v128.load`.
#[test]
fn a_vector_load_is_bounds_checked_once_and_not_per_lane() {
    let body = wat_func_containing(
        &format!(
            "fn read(xs: Array<Float32>, i: Int64) -> Float32 {{\n\
             let v = F32x4.load(xs, i)\n\
             return v.lane(0) + v.lane(1) + v.lane(2) + v.lane(3)\n\
             }}\n{PROLOGUE}"
        ),
        "v128.load",
    );
    assert_eq!(
        body.matches("i64.gt_s").count(),
        1,
        "one span check for four lanes:\n{body}"
    );
    assert_eq!(
        body.matches("i64.ge_u").count(),
        0,
        "a scalar per-lane check would spell itself `i64.ge_u`:\n{body}"
    );
    assert_eq!(
        body.matches("v128.load").count(),
        1,
        "one 16-byte load, not four 4-byte ones:\n{body}"
    );
    assert_eq!(
        body.matches("f32x4.extract_lane").count(),
        4,
        "four lanes out of the one load:\n{body}"
    );
    // The check must branch to the trap: one that computes a condition and drops
    // it reads the same in the counts. The arm parks the row and the lane and
    // branches to the function's one trap site.
    let arm = &body[body.find("i32.or").expect("no span check at all")..];
    let arm = &arm[..arm.find("\n      end").expect("unterminated check")];
    assert!(
        arm.contains("if ") && arm.contains("select") && arm.contains("br "),
        "the check's branch must reach the trap, naming the first lane out of \
         range:\n{arm}"
    );
}

/// A scalar index promises nothing about the next one, so four reads cost four
/// checks. `read` is exported, so no caller's fact proves the first.
#[test]
fn the_same_four_elements_read_scalarly_cost_four_checks() {
    let body = wat_func_containing(
        &format!(
            "export fn read(xs: Array<Float32>, i: Int64) -> Float32 {{\n\
             return xs[i] + xs[i + 1] + xs[i + 2] + xs[i + 3]\n\
             }}\n{PROLOGUE}"
        ),
        "f32.add",
    );
    assert_eq!(
        body.matches("i64.ge_u").count(),
        4,
        "four scalar reads, four checks:\n{body}"
    );
    assert_eq!(
        body.matches("f32.load").count(),
        4,
        "and four four-byte loads, not one sixteen-byte one:\n{body}"
    );
}

#[test]
fn a_vector_store_is_bounds_checked_once_and_not_per_lane() {
    let body = wat_func_containing(
        "fn write(xs: Array<Float32>, i: Int64) {\n\
         F32x4.store(xs, i, F32x4.splat(1.0))\n\
         }\n\
         fn main() -> Int64 {\n\
         let xs: Array<Float32> = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]\n\
         write(xs, 0)\n\
         print(xs[0])\n\
         return 0\n\
         }\n",
        "v128.store",
    );
    assert_eq!(
        body.matches("i64.gt_s").count(),
        1,
        "one span check for four lanes:\n{body}"
    );
    assert_eq!(
        body.matches("i64.ge_u").count(),
        0,
        "a scalar per-lane check would spell itself `i64.ge_u`:\n{body}"
    );
    assert_eq!(
        body.matches("v128.store").count(),
        1,
        "one vector store:\n{body}"
    );
}

/// `min`/`max` are IEEE-754-2019 `minimum`/`maximum`: NaN propagates.
/// `pmin`/`pmax` return an operand instead, so `min(NaN, 1.0)` would differ
/// between engines.
#[test]
fn min_and_max_lower_to_the_nan_propagating_opcode() {
    let body = wat_func_containing(
        "fn both(a: F32x4, b: F32x4) -> Float32 {\n\
         return F32x4.min(a, b).lane(0) + F32x4.max(a, b).lane(0)\n\
         }\n\
         fn main() -> Int64 {\n\
         print(both(F32x4.splat(1.0), F32x4.splat(2.0)))\n\
         return 0\n\
         }\n",
        "f32x4.min",
    );
    assert_eq!(body.matches("f32x4.min").count(), 1, "not min:\n{body}");
    assert_eq!(body.matches("f32x4.max").count(), 1, "not max:\n{body}");
    assert!(
        !body.contains("f32x4.pmin") && !body.contains("f32x4.pmax"),
        "`pmin` and `pmax` answer with the second operand for a NaN, which is \
         the other rule:\n{body}"
    );
}

/// `nearest` is roundTiesToEven: `2.5` rounds to `2`, where ties-away answers
/// `3`. The two agree except at an exact half, so a wrong opcode hides until a
/// `.5` reaches it.
#[test]
fn nearest_lowers_to_ties_to_even_and_not_to_ties_away() {
    let body = wat_func_containing(
        "fn four(v: F32x4) -> Float32 {\n\
         return F32x4.ceil(v).lane(0) + F32x4.floor(v).lane(1)\n\
         + F32x4.trunc(v).lane(2) + F32x4.nearest(v).lane(3)\n\
         }\n\
         fn main() -> Int64 {\n\
         print(four(F32x4.splat(2.5)))\n\
         return 0\n\
         }\n",
        "f32x4.nearest",
    );
    assert!(body.contains("f32x4.ceil"), "not ceil:\n{body}");
    assert!(body.contains("f32x4.floor"), "not floor:\n{body}");
    assert!(body.contains("f32x4.trunc"), "not trunc:\n{body}");
    assert_eq!(
        body.matches("f32x4.nearest").count(),
        1,
        "not ties-to-even:\n{body}"
    );
    assert_eq!(
        body.matches("f32x4.extract_lane").count(),
        4,
        "one lane read per rounding:\n{body}"
    );
}

/// An `I32x4` comparison is signed: signed and unsigned agree except across the
/// sign bit, so `min(Int32.min, 1)` is the only place a wrong opcode shows.
/// The add wraps (`Int32.max + 1` is `Int32.min`), the overflow rule at every
/// other width.
#[test]
fn integer_lane_compare_is_signed_and_the_add_wraps() {
    let body = wat_func_containing(
        "fn both(a: I32x4, b: I32x4) -> Int32 {\n\
         if (a < b).anyTrue() { return (a + b).lane(0) }\n\
         return (a - b).lane(0)\n\
         }\n\
         fn main() -> Int64 {\n\
         print(both(I32x4.splat(1), I32x4.splat(2)))\n\
         return 0\n\
         }\n",
        "i32x4.lt_s",
    );
    assert!(
        !body.contains("i32x4.lt_u"),
        "`lt_u` is the `U32x4` comparison and answers false for \
         `Int32.min < 1`:\n{body}"
    );
    assert!(body.contains("i32x4.add"), "no vector add at all:\n{body}");
    assert!(
        body.contains("i32x4.sub"),
        "no vector subtract at all:\n{body}"
    );
}

/// The wide width's bounds check spans two elements, not four.
///
/// A span too large refuses the last legal index, which
/// `examples/simdwide.vyrn` reads. A span too small accepts a read one past
/// the end and shows in no output, so the limit is counted here.
#[test]
fn the_wide_load_spans_two_elements_and_is_still_checked_once() {
    let body = wat_func_containing(
        "fn read(xs: Array<Float64>, i: Int64) -> Float64 {\n\
         let v = F64x2.min(F64x2.load(xs, i), F64x2.splat(1.0))\n\
         return F64x2.max(v, F64x2.sqrt(v)).lane(0) + v.lane(1)\n\
         }\n\
         fn main() -> Int64 {\n\
         let xs: Array<Float64> = [1.0, 2.0, 3.0, 4.0]\n\
         print(read(xs, 0))\n\
         return 0\n\
         }\n",
        "v128.load",
    );
    assert_eq!(
        body.matches("i64.gt_s").count(),
        1,
        "one check for two lanes:\n{body}"
    );
    assert!(
        body.contains("i64.const 2"),
        "the limit is `len - 2`; a `len - 4` refuses the last legal index:\n{body}"
    );
    assert!(
        !body.contains("i64.const 4"),
        "a four-element span is the narrow width's, and here it is wrong in \
         both directions:\n{body}"
    );
    assert_eq!(body.matches("f64x2.min").count(), 1, "not min:\n{body}");
    assert_eq!(body.matches("f64x2.max").count(), 1, "not max:\n{body}");
    assert!(body.contains("f64x2.sqrt"), "not a vector sqrt:\n{body}");
    assert!(
        !body.contains("f64x2.pmin") && !body.contains("f64x2.pmax"),
        "`pmin` and `pmax` are the other NaN rule:\n{body}"
    );
    assert_eq!(
        body.matches("v128.load").count(),
        1,
        "one sixteen-byte load, not two eight-byte ones:\n{body}"
    );
}

/// The mask reductions are builtins because they are one reduce, not the lane
/// reads and branch chain of the Vyrn `||`/`&&` that `examples/simdbench.vyrn`
/// prices against them. No output shows the difference.
#[test]
fn the_mask_reductions_are_one_reduce_and_not_four_lane_reads() {
    let body = wat_func_containing(
        "fn both(a: F32x4, b: F32x4) -> Bool {\n\
         return (a < b).anyTrue() && (a > b).allTrue()\n\
         }\n\
         fn main() -> Int64 {\n\
         print(both(F32x4.splat(1.0), F32x4.splat(2.0)))\n\
         return 0\n\
         }\n",
        "v128.any_true",
    );
    assert_eq!(
        body.matches("v128.any_true").count(),
        1,
        "not a reduce:\n{body}"
    );
    assert_eq!(
        body.matches("i32x4.all_true").count(),
        1,
        "not a reduce:\n{body}"
    );
    assert!(
        !body.contains("extract_lane"),
        "a reduction read lanes one at a time:\n{body}"
    );
}
