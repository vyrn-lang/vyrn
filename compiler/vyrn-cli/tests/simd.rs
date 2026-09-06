//! RFC-0083 M2: a vector load/store is bounds-checked ONCE for the whole
//! vector, not once per lane.
//!
//! That amortisation is most of what a vector load buys over a scalar loop — a
//! loop cannot amortise its own bounds test — and no output can tell the two
//! apart: four checks and one check accept and reject exactly the same
//! programs, with the same message. Only the emitted code differs. These tests
//! are that half, in the shape of `places.rs`: a structural count, not a
//! duration, so a loaded machine cannot make them flaky.
//!
//! Every count here is read out of `vyrn emit-wat`, which is the one emitter's
//! own text (RFC-0125 §2.5). They were read out of `vyrn emit-ir` until the
//! textual route went, and the move made them BETTER pins rather than worse
//! ones: the rules are wasm's — `f32x4.min` propagates a NaN and `f32x4.pmin`
//! does not, `f32x4.nearest` is roundTiesToEven — so the assertion now names
//! the decision this compiler makes instead of the intrinsic a second compiler
//! was asked for. `examples/simdoob.vyrn` / `examples/simdoobstore.vyrn` still
//! prove the two branches of the check trap identically on every engine; what
//! they cannot say is how many checks ran.

mod common;
use common::*;

/// [`common::wat_func_containing`], with the check that makes a bounds COUNT
/// mean something: the module has to have interned the wording a check reports,
/// or the count below is counting a check that cannot report.
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

/// RFC-0083 M2's claim, read out of the module that ships: what it contains is
/// `bounds_check_span`'s single branch — the signed pair `i < 0 || i > len - 4`
/// joined by `i32.or`, and NOT four copies of `bounds_check`'s scalar
/// `i64.ge_u` — and one `v128.load` where four lane loads would otherwise be.
///
/// It had a twin over `vyrn emit-ir`, asserting the same count about the textual
/// route's own check. The route went; the count stayed here.
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
    // And the one check BRANCHES to the trap: a check that computes a condition
    // and drops it reads exactly like this one from the counts alone. The trap
    // it reaches is the function's one trap site (RFC-0125 M1), so the arm
    // parks the table row and the offending lane and branches out; the call to
    // `trapAt` stands once, after the body (RFC-0125 §2.3).
    let arm = &body[body.find("i32.or").expect("no span check at all")..];
    let arm = &arm[..arm.find("\n      end").expect("unterminated check")];
    assert!(
        arm.contains("if ") && arm.contains("select") && arm.contains("br "),
        "the check's branch must reach the trap, naming the first lane out of \
         range:\n{arm}"
    );
}

/// The comparison the milestone rests on: the same four elements read one at a
/// time cost four checks, because a scalar index cannot promise anything about
/// the next one.
#[test]
fn the_same_four_elements_read_scalarly_cost_four_checks() {
    let body = wat_func_containing(
        &format!(
            "fn read(xs: Array<Float32>, i: Int64) -> Float32 {{\n\
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

/// `min`/`max` are IEEE-754-2019 `minimum`/`maximum` — NaN propagates — and the
/// intrinsic is how native was made to agree rather than left to whatever
/// `minps` does. `llvm.minnum` is the OTHER function: it returns the non-NaN
/// operand, and `min(NaN, 1.0)` would print `1.000000` natively and `NaN` on the
/// other two. Parity would catch that, but only under `--ignored` and only with
/// a clang; this catches it in the default suite.
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

/// `nearest` is roundTiesToEven, and `llvm.round` is a DIFFERENT function —
/// roundTiesAwayFromZero, which answers `3` for `2.5` where wasm's
/// `f32x4.nearest` answers `2`. The two agree everywhere except at an exact half,
/// so a wrong intrinsic is invisible until a `.5` reaches it; this fails in the
/// default suite instead. `llvm.rint` and not `llvm.roundeven` is a linking
/// choice argued where the declaration is emitted — baseline x86-64 scalarizes
/// `roundeven` to a `roundevenf` the MSVC UCRT does not ship — and the two are
/// the same function under the only rounding mode Vyrn can produce.
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

/// An `I32x4` comparison is SIGNED, and that is the M3 decision a wrong opcode
/// hides best: `icmp slt` and `icmp ult` agree on every value except across the
/// sign bit, so `min(Int32.min, 1)` is the only place the difference shows.
/// `examples/simdint.vyrn` prints it, but only under `--ignored` parity and only
/// with a clang; this fails in the default suite.
///
/// The wrap is pinned in the same body, and for a related reason. `add <4 x i32>`
/// carries no `nsw`/`nuw`, so `Int32.max + 1` is `Int32.min` — the language's
/// overflow rule at every other width, and what `i32x4.add` does with no choice
/// in the matter. An `nsw` here would make the same expression UB natively and a
/// wrap on wasm: a divergence that shows at exactly one input and nowhere else.
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

/// The wide width's bounds check spans TWO elements, not four (RFC-0083 M4).
///
/// This is the one thing the wider lane genuinely changed, and half of it cannot
/// be seen in output. A span that is too LARGE shows up immediately — the last
/// valid index stops loading, which `examples/simdwide.vyrn` reads. A span that
/// is too small does not show up at all: it accepts every legal program and also
/// accepts a read one element past the end, which is a check that silently stops
/// being one. So the limit is counted here.
///
/// The intrinsics come along for the ride, for the reason the narrow pair is
/// pinned above: `llvm.minnum.v2f64` returns the non-NaN operand and would make
/// native the only engine printing `1.000000` for `min(NaN, 1.0)`.
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

/// The mask reductions are ONE reduction over the vector, not four lane reads
/// and a branch chain — which is the whole reason they are builtins rather than
/// the Vyrn `||`/`&&` `examples/simdbench.vyrn` prices against them. `-O2` can
/// turn the chain back into a reduce (that is what makes the measured gap only
/// 1.2x on wasm), so an unoptimised body is the only place the difference is
/// visible, and no output ever shows it. Same shape as the intrinsic pin above:
/// a "simplification" back to `extractelement` fails here rather than only under
/// `--ignored` parity.
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
