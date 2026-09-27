//! The primitive census: why each remaining builtin is a builtin.
//!
//! [`CENSUS`] states the reason per name, and the tests check it against the
//! direct backend both ways. Outside it: `byteLength` is a field read that
//! `consteval` folds in refinements; `hostNowMillis` and its kin are `extern`
//! declarations; numeric conversions resolve in `types::numeric_conv_target`.

use std::collections::{BTreeMap, BTreeSet};

/// Why a builtin is still implemented in Rust.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Why {
    /// The allocator and the containers on it. Containers over `Array` are safe
    /// Vyrn (`examples/slottable.vyrn`); moving them cost ~18x on the
    /// interpreter.
    Memory,
    /// A host import: `fd_write` cannot be written in terms of itself.
    Syscall,
    /// A view, not an operation: nothing else builds a `Float64` from bits or a
    /// `String` from bytes.
    View,
    /// Vyrn cannot abort except through `panic`, the one irreducible row.
    Control,
    /// Needs the static type of an expression, the module graph, or the
    /// compiler's own lexer and AST.
    Compiler,
    /// The semantics differ observably; moving it is a language change.
    Semantics,
    /// Movable, and refused on a measured cost.
    Measured,
    /// No reason at all. Kept so the census can report the next finding.
    Unjustified,
}

use Why::*;

/// Every builtin an engine implements in Rust, and why.
const CENSUS: &[(&str, Why, &str)] = &[
    ("@push", Memory, "Array: append, reallocating"),
    ("@reserve", Memory, "Array: capacity for n more, one realloc"),
    ("@clear", Memory, "Array: forget the elements, keep the buffer"),
    ("@append", Memory, "Array: bulk copy of a heapless source, one growth"),
    ("@copyFrom", Memory, "Array: overwrite in place, reusing the buffer"),
    ("@tally", Memory, "Map: insert-or-add in one probe"),
    ("@tallyBytes", Memory, "Map: tally keyed by raw bytes; the key exists only on a miss"),
    ("@at", Memory, "Array: indexed read; also traps out of bounds"),
    ("@list", Memory, "a fixed and a growable array share one representation"),
    ("@toArray", Memory, "SmallArray: copy the inline/spilled buffer out"),
    ("@pop", Memory, "Array: shrink by one, writing back through the binding"),
    ("@swapRemove", Memory, "Array: swap-and-shrink; also traps"),
    ("@has", Memory, "Map: key probe"),
    ("@keys", Memory, "Map: a fresh snapshot of the key column"),
    ("@remove", Memory, "Map: order-preserving removal in place"),
    ("@copy", Memory, "a value that shares no heap with its receiver"),
    // Array access with a bounds trap, as `at`; one check covers the span.
    ("@f32x4Load", Memory, "Array<Float32>: four consecutive elements, one bounds check"),
    ("@f32x4Store", Memory, "Array<Float32>: the same four written back"),
    ("@i32x4Load", Memory, "Array<Int32>: four consecutive elements, one bounds check"),
    ("@i32x4Store", Memory, "Array<Int32>: the same four written back"),
    // The span of the check is a parameter: two elements at an 8-byte stride.
    ("@f64x2Load", Memory, "Array<Float64>: two consecutive elements, one bounds check"),
    ("@f64x2Store", Memory, "Array<Float64>: the same two written back"),
    // Nothing else makes or unmakes a `Stream<T>`, which is what makes
    // "disposed exactly once" checkable.
    ("fromArray", Memory, "Stream: the array's buffer, moved into a buffer-tagged header"),
    ("fromStep", Memory, "Stream: a caller's cursor plus a step, pulled once per element"),
    // A lazy combinator owns the stream it wraps, held in one heap box.
    ("boxStream", Memory, "Stream: a stream moved into one heap box, by address"),
    ("unboxStream", Memory, "Stream: a boxed stream moved back out, and the box freed"),
    ("pullAt", Memory, "Stream: one element from the stream in that box"),
    ("close", Memory, "Stream: variant-aware reclamation (a buffer, or the step's own release)"),
    // The one call that reaches the host's accept loop; the host then owes the
    // stream its `close`.
    ("serveStream", Syscall, "the host's socket: hand a producer to the accept loop, which pulls and closes it"),
    ("print", Syscall, "fd_write on stdout"),
    ("logger", Syscall, "the handle for the five level methods below"),
    ("@trace", Syscall, "fd_write on the configured sink, below a folded threshold"),
    ("@debug", Syscall, "as `@trace`"),
    ("@info", Syscall, "as `@trace`"),
    ("@warn", Syscall, "as `@trace`"),
    ("@error", Syscall, "as `@trace`"),
    ("bytes", View, "String -> Array<UInt8>, whole or a byte range: what all four runtime modules stand on"),
    ("stringFromBytes", View, "the only Array<UInt8> -> String construction there is"),
    ("floatBits", View, "i64.reinterpret_f64, one instruction"),
    ("floatFromBits", View, "the other direction"),
    // Vector arithmetic is a `BinOp`, not a Call arm, so the census covers only
    // construction and lane access.
    ("F32x4", View, "four Float32 lanes into one value"),
    ("@f32x4Splat", View, "one value into all four lanes"),
    ("@lane", View, "a lane back out, at a checker-proven constant index"),
    ("@replaceLane", View, "one lane written back, same constant index"),
    // `@lane` and `@replaceLane` serve every width: the index rule is the same.
    ("I32x4", View, "four Int32 lanes into one value"),
    ("@i32x4Splat", View, "one value into all four lanes"),
    ("F64x2", View, "two Float64 lanes into one value"),
    ("@f64x2Splat", View, "one value into both lanes"),
    ("panic", Control, "the abort itself, and the only irreducible row here"),
    // `@panicAt` is `panic` with the file and line the loader stamped. `@` does
    // not lex, so no source can write it and `panic` stays one-argument.
    ("@panicAt", Control, "census U5: `panic` carrying the site it is written at"),
    ("assert", Control, "traps the current test"),
    ("assertEq", Control, "traps, rendering both sides"),
    ("toJson", Compiler, "the walk needs the argument's static type; the writer is std/json"),
    ("fromJson", Compiler, "as `toJson`; the reader is std/jsonread + std/jsondec"),
    ("moduleInterface", Compiler, "reads the module graph"),
    ("schemaOf", Compiler, "reflects a type DECLARATION into a Schema literal"),
    ("contractOf", Compiler, "reflects a module contract"),
    ("jsonSchema", Compiler, "renders a declaration as JSON Schema at compile time"),
    ("value", Compiler, "boxes a scalar by its type; anything else routes to `impl Show`"),
    ("blackBox", Compiler, "an optimizer barrier is a backend property"),
    ("@pull", Compiler, "the core's callee for a stream `for` head; no program spells it"),
    ("@strAppend", Compiler, "the core's callee for a String accumulator; no program spells it"),
    ("raw", Compiler, "builds a code-quote value"),
    ("rawAt", Compiler, "a code quote carrying an origin directive"),
    ("render", Compiler, "a code quote back to text"),
    ("lex", Compiler, "the compiler's OWN lexer"),
    ("@codeText", Compiler, "the desugar of vyrn\"…\""),
    ("@codeSplice", Compiler, "the desugar of an interpolation inside vyrn\"…\""),
    ("Some", Compiler, "a constructor of the compiler's own Option"),
    ("Ok", Compiler, "a constructor of the compiler's own Result"),
    ("Err", Compiler, "as `Ok`"),
    // No finite sequence of Vyrn arithmetic is the correctly-rounded IEEE
    // square root, and parity compares bytes.
    ("@f32x4Sqrt", Semantics, "a Vyrn Newton iteration is not the correctly-rounded IEEE result"),
    ("@f64x2Sqrt", Semantics, "as `@f32x4Sqrt`, at 64 bits"),
    // The interpreter's `{:.6}` float rendering is kept as the differential
    // oracle; floats render through `std/num`'s `f64Str`. The scalar rendering
    // is the seed a `Show` declaration overrides, one lowering per engine.
    ("@str", Measured, "integer rendering: a Vyrn digit loop is 150 ns against 60 ns (2.5x), on every print"),
    ("@concat", Measured, "9.7x native / 11x wasm (580 ns against 60 ns): a Vyrn join must revalidate UTF-8"),
    // `examples/simdbench.vyrn` holds the Vyrn implementations and `vyrn bench`
    // prices them against these builtins.
    ("@f32x4Min", Measured, "3.6x native (44.4 us against 12.3 us per 65536 lanes); Vyrn needs floatBits for -0.0"),
    ("@f32x4Max", Measured, "3.7x native (44.1 us against 12.1 us per 65536 lanes), the mirror of `min`"),
    // The Vyrn version must reproduce the NaN rule and the sign of a zero,
    // which costs the same at 64 bits. The `f64x2` roundings are left out.
    ("@f64x2Min", Measured, "2.5x native (39.0 us against 15.2 us per 65536 lanes); the Vyrn version needs floatBits for -0.0"),
    ("@f64x2Max", Measured, "2.5x native (38.7 us against 15.0 us per 65536 lanes), the mirror of `min`"),
    // The mask reductions in Vyrn short-circuit, so on a monotonic array the
    // chain is predicted and the ratio drops to 1.3x / 2.3x. The rows quote
    // the unpredictable case.
    ("@anyTrue", Measured, "2.5x native (1356 ms against 543 ms, unpredictable lanes) / 1.2x wasm; 1.3x when the short circuit predicts"),
    ("@allTrue", Measured, "2.4x native (1170 ms against 481 ms, unpredictable lanes) / 1.2x wasm; 2.3x when the short circuit predicts"),
    // Baseline x86-64 has no `roundps` (SSE4.1) and `vyrn build` passes no
    // `-march`, so each of `llvm.ceil/floor/trunc/rint.v4f32` scalarizes to
    // four libc calls. An x86-64-v2 baseline would make each one instruction.
    // Every number is against an inline Vyrn spelling: Cranelift does not
    // inline across calls.
    ("@f32x4Ceil", Measured, "1.1x native (49.9 us against 56.1 us per 65536 lanes) / 2.3x wasm (54 ms against 126 ms per 102 M lanes), both inline"),
    ("@f32x4Floor", Measured, "1.0x native (53.9 us against 52.7 us) / 2.3x wasm (54 ms against 124 ms), the mirror of `ceil`"),
    // The one row where the builtin is slower natively. Kept for the 1.9x on
    // wasm and because the Vyrn version needs `floatBits` for the sign of a
    // zero; an x86-64-v2 baseline would settle it.
    ("@f32x4Trunc", Measured, "0.43x native — the builtin LOSES (97.2 us against 42.0 us, four truncf calls) — and 1.9x wasm (53 ms against 100 ms), which is all that keeps it"),
    ("@f32x4Nearest", Measured, "1.4x native (49.8 us against 70.7 us) / 4.1x wasm (54 ms against 219 ms); ties-to-even by hand is 20 lines and gets `-0.0` wrong first"),
];

/// Every censused name is lowered by the direct backend; `ABSENT` is empty.
///
/// A name is covered if the backend mentions it as a literal or through its
/// Rust constant. The substring scan errs safe: a mention in a comment reads
/// as coverage.
#[test]
fn the_direct_backend_carries_the_census_too() {
    // A censused name absent from the direct backend, and why it may be. Each
    // refuses at build time with `no lowering for the call`.
    const ABSENT: &[(&str, &str)] = &[];
    // Names a backend spells with a Rust constant, or reaches through the
    // frontend function that names their callee.
    let alias = |n: &str| match n {
        "@panicAt" => Some("PANIC_AT"),
        "@at" => Some("project::AT"),
        "@slot" => Some("ELEM"),
        "contractOf" | "toJson" | "fromJson" => Some("routed_callee"),
        "schemaOf" => Some("schema_at"),
        _ => None,
    };
    // The core states a builtin and the emitter answers its row, so the
    // backend is the two files together.
    let direct = [
        include_str!("../../vyrn-codegen/src/direct.rs"),
        include_str!("../../vyrn-lower/src/core.rs"),
    ]
    .concat();
    let covers = |n: &str| {
        direct.contains(&format!("{n:?}")) || alias(n).is_some_and(|a| direct.contains(a))
    };
    let missing: Vec<&str> = CENSUS
        .iter()
        .map(|(n, ..)| *n)
        .filter(|n| !covers(n))
        .collect();
    let allowed: BTreeSet<&str> = ABSENT.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        missing.iter().copied().collect::<BTreeSet<_>>(),
        allowed,
        "the direct wasm backend's coverage of the census moved. A name that \
         LEFT this set is covered now — delete its row. A name that JOINED it \
         runs on three engines and refuses on the fourth, which is what the \
         census found by reading and this test exists to find by running."
    );
}

/// Every name a compiling backend dispatches on, in the four spellings the
/// emitters use: `name == "x"`, `matches!(name, "x" | "y")`, an arm of
/// `match name`, and an arm of `match (name, args.len())`.
///
/// A substring scan of the emitter itself, so a name added there is seen.
fn dispatched(region: &str) -> BTreeSet<&str> {
    /// The contents of every `"..."` in a segment that holds no escape.
    fn quoted(seg: &str) -> impl Iterator<Item = &str> {
        seg.split('"').skip(1).step_by(2).filter(|n| !n.is_empty())
    }
    let mut out = BTreeSet::new();
    for (i, m) in region.match_indices("name == \"") {
        out.extend(quoted(&region[i + m.len() - 1..]).next());
    }
    // `matches!(name, "x" | "y")`, over one line or several. The alternation
    // holds no parenthesis of its own, so the first `)` closes it.
    for (i, m) in region.match_indices("matches!(") {
        let tail = region[i + m.len()..].trim_start();
        if let Some(alts) = tail.strip_prefix("name,") {
            out.extend(quoted(&alts[..alts.find(')').unwrap_or(0)]));
        }
    }
    // `("x", 1)`: a `match (name, args.len())` arm, told from other tuples by
    // its literal arity. `("x", Spec::..)` or `one("x", &p, &r)`: a row of
    // `builtin_rows`.
    for (i, _) in region.match_indices("(\"") {
        let Some((name, after)) = region[i + 2..].split_once('"') else {
            continue;
        };
        let arity = after.trim_start_matches([',', ' ']);
        let close = arity.trim_start_matches(|c: char| c.is_ascii_digit());
        let row = arity.starts_with("Spec::") || arity.starts_with('&');
        if !name.is_empty() && (row || close.len() < arity.len() && close.starts_with(')')) {
            out.insert(name);
        }
    }
    // An arm of `match name`, at twelve spaces (both matches sit two blocks
    // inside a method), continued below with `| "y"`.
    for line in region.lines() {
        let arm = line.trim_start();
        if line.len() - arm.len() != 12 || !(arm.starts_with('"') || arm.starts_with("| \"")) {
            continue;
        }
        let pat = arm.split(" if ").next().unwrap_or(arm);
        out.extend(quoted(pat.split("=>").next().unwrap_or(pat)));
    }
    out
}

/// The anti-rot direction: a builtin an emitter branches on with no census row.
///
/// The regions are the core's `builtin_rows` and the emitter's methods that
/// branch on a row's name. There is no list of permitted exceptions. The scan
/// cannot see a name spelled by a constant (`ast::PANIC_AT`,
/// `checker::GEN_REFLECT`); the forward direction aliases `@panicAt`.
#[test]
fn the_backends_dispatch_on_nothing_the_census_omits() {
    let direct = include_str!("../../vyrn-codegen/src/direct.rs");
    let core = include_str!("../../vyrn-lower/src/core.rs");
    // Located by content, so a reorganised emitter fails here rather than
    // scanning nothing and passing.
    let cut = |src: &'static str, from: &str, to: &str| -> &'static str {
        let i = src
            .find(from)
            .unwrap_or_else(|| panic!("the region opening `{from}`"));
        let j = src[i + from.len()..]
            .find(to)
            .unwrap_or_else(|| panic!("the region closing `{to}`"));
        &src[i..i + from.len() + j]
    };
    let mut regions = vec![cut(core, "pub fn builtin_rows(", "\n}\n")];
    for f in ["asserts", "effect", "logs", "lanes"] {
        regions.push(cut(direct, &format!("    fn {f}("), "\n    fn "));
    }
    let censused: BTreeSet<&str> = CENSUS.iter().map(|(n, ..)| *n).collect();
    let uncensused: BTreeSet<&str> = regions
        .iter()
        .flat_map(|r| dispatched(r))
        .filter(|n| !censused.contains(n))
        .collect();
    assert!(
        uncensused.is_empty(),
        "a compiling backend dispatches on {uncensused:?}, and the census has \
         no row for them. A builtin IS a name an emitter branches on, so either \
         it gets a row saying why it is implemented in Rust, or the branch is \
         not a builtin and belongs somewhere the census does not read."
    );
}

/// No builtin has two definitions.
///
/// The failure is silent: `routed_builtin` runs before the dispatch, so the
/// censused arm stops being reached.
#[test]
fn nothing_is_both_censused_and_routed() {
    for rt in vyrn_frontend::loader::RT_MODULES {
        for (builtin, reserved) in rt.routes {
            assert!(
                !CENSUS.iter().any(|(n, ..)| n == builtin),
                "`{builtin}` is routed to `{reserved}` AND censused as a Rust \
                 primitive — one of the two is now dead code"
            );
        }
    }
}

/// The refusals re-checked, pinned with their reasons.
///
/// `logger` needs a write to a file descriptor Vyrn cannot name, and its
/// threshold folds, so routing turns a deleted call into a runtime comparison.
/// `stringFromBytes` is the only construction of a `String` from bytes. `parse`
/// wraps where `std/num` refuses.
#[test]
fn the_refusals_keep_their_reasons() {
    let by_name: BTreeMap<&str, Why> = CENSUS.iter().map(|(n, w, _)| (*n, *w)).collect();
    assert!(
        !by_name.contains_key("slice"),
        "`slice` is `std/strpred`'s — a census row for it \
         is a Rust implementation nobody reaches"
    );
    // A name that left `RESERVED` for a `std/` module may not keep a Rust arm
    // either: an engine would hold a second opinion.
    for (name, gone) in vyrn_frontend::checker::MOVED_TO_STD {
        let vyrn_frontend::checker::Gone::Module(module) = gone else {
            // A removed spelling keeps its Rust arm under the internal `@` name
            // the sugar produces (`push`, `at`).
            continue;
        };
        assert!(
            !by_name.contains_key(name),
            "`{name}` is `{module}`'s declaration now; a census row for it is a \
             Rust implementation nobody reaches"
        );
    }
    for (name, why) in [("logger", Syscall), ("stringFromBytes", View)] {
        assert_eq!(
            by_name.get(name),
            Some(&why),
            "`{name}` was refused as {why:?} — moving it means writing down why \
             that reason stopped being true"
        );
    }
    // Every remaining arm states a reason to be a primitive. A row appearing here
    // is a finding, not a row to invent.
    let unjustified: Vec<&str> = CENSUS
        .iter()
        .filter(|(_, w, _)| *w == Unjustified)
        .map(|(n, ..)| *n)
        .collect();
    assert!(
        unjustified.is_empty(),
        "{unjustified:?} is in the interpreter with no reason given"
    );
}
