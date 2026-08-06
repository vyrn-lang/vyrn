//! RFC-0078 M1 + M5 — the primitive census.
//!
//! M1 asked for the irreducible primitive set to be *named*. M5 asked the
//! interpreter's Rust arms to become caches or disappear. Neither was ever taken
//! as its own milestone, because each later one discovered a piece of the answer:
//! M4a found that the missing primitive is a *view* rather than an operation,
//! M4b(3) found that a *trapping* builtin needs an abort Vyrn does not have, and
//! M4c found that two of the fourteen it could have routed are a deliberate
//! cache. Twelve arms went outright (M2b's `toJson`, M4c's ten routes, M3's
//! `fromJson`), so the count has already fallen.
//!
//! What was missing is this: a single statement of **why each remaining builtin
//! is a builtin**, checkable against the code. [`CENSUS`] is that statement and
//! the tests below are the check — add an arm to `interp.rs` without a census
//! row, or route a name the census claims, and this file fails.
//!
//! ## The method, stated so the next reader can reproduce it
//!
//! The census covers every builtin the **interpreter** implements in Rust on the
//! `Expr::Call` path, which is where M5's "count of Rust arms" lives. That is two
//! regions of `interp.rs`, both located by content rather than by line number:
//!
//! - the `if name == "…"` guards between `Expr::Call { name, args, line } => {`
//!   and `match name.as_str() {` — the builtins handled *before* the arguments
//!   are evaluated, because they need the AST (`schemaOf`) or must write back
//!   through a binding (`@pop`);
//! - the arms of `match name.as_str() {` itself, up to its `_ => {` fallthrough.
//!
//! A name counts once, so `"lineAt" | "colAt"` is two entries and
//! `"trace" | … | "error"` is five. At the commit this landed on that is **62**:
//! 51 arm names in 46 arms, plus 11 guards.
//!
//! Three things are deliberately outside it, and are named here so their absence
//! is not mistaken for an omission:
//!
//! - **`byteLength`** is an `Expr::Field` read, not a call, so the scan cannot
//!   see it. It is a primitive for the reason M4c refused it — `strlen`, and
//!   `consteval` folds it inside refinement predicates, so routing it would
//!   destroy compile-time proof of `String where value.byteLength >= 3`. Its
//!   refusal is pinned by `every_route_is_spelled_with_its_modules_prefix` in
//!   `vyrn-cli/tests/codecs.rs`.
//! - **`hostNowMillis` / `hostMonotonicNanos` / `hostRandomSeed`** are `extern`
//!   declarations (RFC-0043), not builtins. They are already the arrangement this
//!   RFC is arguing for: `std/time` and `std/random` are Vyrn above a named
//!   syscall.
//! - **numeric conversions** (`Int32(x)`, `Float64(x)`, …) are resolved by
//!   `types::numeric_conv_target` before the dispatch, and are part of the type
//!   system rather than the runtime.

use std::collections::{BTreeMap, BTreeSet};

/// Why a builtin is still implemented in Rust.
///
/// Every category was discovered by a milestone hitting it rather than designed
/// up front, and the two at the bottom are not reasons to *be* a primitive —
/// they are the honest labels for the arms that are movable and were not moved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Why {
    /// **Memory.** `__vyrn_malloc` is irreducible: you cannot write the allocator
    /// without a memory-growth primitive, which is the same argument `Syscall`
    /// makes and is not a deferral.
    ///
    /// This row said the containers standing on it — `Array`, `Map`,
    /// `SmallArray`, the slot table — "need a **raw-memory view** the language
    /// does not have", and pointed at RFC-0078's open question A. **RFC-0082
    /// withdrew that question.** What forces `unsafe` in Rust's `Vec` is
    /// uninitialized capacity being observable (`set_len`, which is why
    /// `MaybeUninit` exists), not pointers; `Array` owns `len` and `cap` together
    /// and never exposes spare capacity, so containers over it are ordinary safe
    /// Vyrn. `examples/slottable.vyrn` is a generation-checked slot table written
    /// that way.
    ///
    /// So these rows are **not** blocked on a language feature. M2 measured the
    /// actual cost of moving them — ~18x on the interpreter — and that number,
    /// not a missing primitive, is why they are still here.
    Memory,
    /// **Syscall.** RFC-0077 M2j measured a directly-emitted module's entire
    /// import list as twelve WASI functions plus `__vyrn_malloc`. You cannot write
    /// `fd_write` in terms of itself.
    Syscall,
    /// **Representation — a view, not an operation.** M4a's central finding:
    /// nothing in the language could construct a `Float64` from anything but
    /// another number, so every text -> float route had to be a builtin. Give
    /// Vyrn the *bits* and the operation stops needing to be primitive at all.
    /// `bytes` is the same thing for `String`, and `stringFromBytes` is its
    /// inverse.
    View,
    /// **Control.** M4b(3)'s finding: Vyrn has no `panic` and no `abort`, so no
    /// Vyrn implementation of a *trapping* builtin can be observationally equal.
    /// `@join` is the same shape one step over — an expression that waits for
    /// another task is not something the language can spell either. The second
    /// open language question was this row's, and RFC-0079 M1 answered it: `panic`
    /// is now a row here itself, which is what makes it the only irreducible one.
    /// `slice` leaves at M3 by returning its failure instead of trapping.
    Control,
    /// **Compiler-directed.** Needs the static type of an arbitrary expression,
    /// the module graph, or the compiler's own lexer and AST. `toJson` and
    /// `fromJson` are here rather than in a library because only the *walk* needs
    /// the compiler — their serializer and reader are Vyrn (`std/json`,
    /// `std/jsonread`, `std/jsondec`), which is this RFC's whole design in one
    /// row: "a builtin becomes, at most, a type-directed compiler part plus a
    /// call into Vyrn."
    Compiler,
    /// **A cache, with a stated reason.** M5's own row, and the only one. The
    /// interpreter memoizes a line-start table per buffer that a Vyrn library
    /// **cannot** hold, because a generator may not touch module state (comptime
    /// purity). Worth 122 ms of a 291 ms `std/vyx` page compile; the native shim
    /// holds no such cache and counts exactly as the Vyrn version does.
    Cache,
    /// **The semantics differ observably.** Moving it is a language change, not a
    /// mechanical move.
    Semantics,
    /// **Movable, and refused on a measured cost.** Not a reason to be a
    /// primitive: a reason this milestone did not move it. Each is owed its own
    /// milestone with its own pin, because the blast radius is every program
    /// rather than the programs that mention a builtin.
    Measured,
    /// **No reason at all — the census's one finding.** Movable, not hot, not
    /// measured, not blocked. See RFC-0078's M1+M5 note for the price.
    Unjustified,
}

use Why::*;

/// Every builtin the interpreter implements in Rust, and why.
///
/// Checked against `interp.rs` by [`the_census_is_the_code`], so a new arm
/// without a row here fails the suite rather than quietly joining the list of
/// things nobody can explain.
const CENSUS: &[(&str, Why, &str)] = &[
    // ---- Memory: the allocator and the containers standing on it ------------
    ("array", Memory, "Array: a heap triple {ptr,len,cap}"),
    ("push", Memory, "Array: append, reallocating"),
    ("at", Memory, "Array: indexed read; also traps out of bounds"),
    ("alen", Memory, "Array: the length word"),
    ("@list", Memory, "a fixed and a growable array share one representation"),
    ("@toArray", Memory, "SmallArray (RFC-0056): copy the inline/spilled buffer out"),
    ("@pop", Memory, "Array: shrink by one, writing back through the binding"),
    ("@swapRemove", Memory, "Array (RFC-0011): swap-and-shrink; also traps"),
    ("@has", Memory, "Map (RFC-0028): key probe"),
    ("@keys", Memory, "Map: a fresh snapshot of the key column"),
    ("@remove", Memory, "Map: order-preserving removal in place"),
    ("@copy", Memory, "RFC-0089 M1b: a value that shares no heap with its receiver"),
    // RFC-0083 M2. Array access with a bounds trap, exactly as `at` is — the only
    // difference is that ONE check covers four elements, which is the whole reason
    // the vector form exists.
    ("@f32x4Load", Memory, "Array<Float32>: four consecutive elements, one bounds check"),
    ("@f32x4Store", Memory, "Array<Float32>: the same four written back"),
    // RFC-0083 M3, the same pair at the integer width — the same single check and
    // the same 4-byte stride, so these are the two rows above with a different
    // element type.
    ("@i32x4Load", Memory, "Array<Int32>: four consecutive elements, one bounds check"),
    ("@i32x4Store", Memory, "Array<Int32>: the same four written back"),
    // RFC-0083 M4, the same pair at the wide width — and the first where the SPAN
    // is not four. Two elements at an 8-byte stride behind the one check, which
    // is why the span became a parameter of the check rather than a constant in
    // it on all three engines.
    ("@f64x2Load", Memory, "Array<Float64>: two consecutive elements, one bounds check"),
    ("@f64x2Store", Memory, "Array<Float64>: the same two written back"),
    // RFC-0075. The three ways to make and unmake a `Stream<T>`, and they are
    // separate names precisely because the TYPES are separate: nothing else can
    // make or unmake a stream, which is what makes "disposed exactly once"
    // checkable. M2b is where the note M1 left here came due — the producer
    // stopped being an eager buffer, so `close` grew a real teardown and stopped
    // being the array free under another name. (The array free had a surface
    // name of its own then, `afree`; it has none now.)
    ("fromArray", Memory, "Stream: the array's buffer, moved into a buffer-tagged header"),
    ("fromStep", Memory, "Stream: a caller's cursor plus a step, pulled once per element"),
    // RFC-0075 M2c: a lazy combinator owns the stream it wraps, so `map` is a
    // stream rather than a drain. RFC-0090 M3 moved where that source LIVES —
    // out of a fourth cell-slab array and into one heap box `std/stream` holds
    // the address of — so the three rows are the box, the unbox and the read.
    ("boxStream", Memory, "Stream: a stream moved into one heap box, by address"),
    ("unboxStream", Memory, "Stream: a boxed stream moved back out, and the box freed"),
    ("pullAt", Memory, "Stream: one element from the stream in that box"),
    ("close", Memory, "Stream: variant-aware reclamation (a buffer, or the step's own release)"),
    // RFC-0074 M3a. `Syscall` rather than `Memory`: it is not about the stream's
    // storage, it is the one call that reaches the host's accept loop, which is
    // the same argument `print` makes one row down. It is also the only way a
    // stream leaves the call that made it — which is the point, since the host
    // then owes it the `close` above.
    ("serveStream", Syscall, "the host's socket: hand a producer to the accept loop, which pulls and closes it"),
    ("cell", Memory, "the slot table: allocate a slot and a generation"),
    ("get", Memory, "the slot table: generation-checked read"),
    ("set", Memory, "the slot table: generation-checked write"),
    ("release", Memory, "the slot table: invalidate the generation"),
    // ---- Syscall: RFC-0077 M2j's twelve WASI imports ------------------------
    ("print", Syscall, "fd_write on stdout"),
    ("logger", Syscall, "RFC-0008: the handle for the five level methods below"),
    ("trace", Syscall, "fd_write on the configured sink, below a folded threshold"),
    ("debug", Syscall, "as `trace`"),
    ("info", Syscall, "as `trace`"),
    ("warn", Syscall, "as `trace`"),
    ("error", Syscall, "as `trace`"),
    ("args", Syscall, "args_sizes_get + args_get"),
    ("readLine", Syscall, "fd_read on stdin"),
    ("readFile", Syscall, "path_open + fd_read"),
    ("readFileBytes", Syscall, "path_open + fd_read, unvalidated"),
    ("writeFile", Syscall, "path_open + fd_write"),
    ("renameFile", Syscall, "path_rename"),
    ("fsyncFile", Syscall, "fd_sync"),
    ("listDir", Syscall, "fd_readdir"),
    // ---- Representation: a view, not an operation ---------------------------
    ("bytes", View, "String -> Array<UInt8>: what all four runtime modules stand on"),
    ("stringFromBytes", View, "the only Array<UInt8> -> String construction there is"),
    ("floatBits", View, "RFC-0078 M4a: i64.reinterpret_f64, one instruction"),
    ("floatFromBits", View, "RFC-0078 M4a: the other direction"),
    // RFC-0083 M1. Same shape as `floatBits`: nothing in the language can build a
    // four-lane value out of four `Float32`s or read one back, so these three are
    // the whole of the representation and the ARITHMETIC is not here at all — `+`
    // on two vectors is a `BinOp`, which the census does not cover because it is
    // not a Call arm. There is no operation to move to Vyrn later, only a type the
    // language cannot otherwise name.
    ("F32x4", View, "RFC-0083: four Float32 lanes into one value"),
    ("@f32x4Splat", View, "RFC-0083: one value into all four lanes"),
    ("@lane", View, "RFC-0083: a lane back out, at a checker-proven constant index"),
    ("@replaceLane", View, "RFC-0083: one lane written back, same constant index"),
    // RFC-0083 M3. Two rows and not four: `@lane` and `@replaceLane` are the same
    // arms serving both widths, because a lane accessor is about the LANE INDEX
    // and the index rule is identical. What is width-specific is only the way a
    // value is built, which is what a `View` row is for.
    //
    // M3 adds no `Measured` row at all, and that is the milestone's own finding:
    // `I32x4.min`/`max`/`abs` exist as wasm instructions, were built and were
    // deleted at 1.0x native / 1.05x wasm. The float `min` earns its row on the
    // NaN rule and the signed zero; an integer `min` is one comparison, so there
    // is nothing left for a builtin to be faster at.
    ("I32x4", View, "RFC-0083 M3: four Int32 lanes into one value"),
    ("@i32x4Splat", View, "RFC-0083 M3: one value into all four lanes"),
    // RFC-0083 M4, the third width and the same two rows. `@lane` and
    // `@replaceLane` again serve it from the arms they already had — a lane
    // accessor is about the lane INDEX, and the only thing that changed is the
    // range the checker proves it against.
    ("F64x2", View, "RFC-0083 M4: two Float64 lanes into one value"),
    ("@f64x2Splat", View, "RFC-0083 M4: one value into both lanes"),
    // ---- Control: abort, and waiting -----------------------------------------
    ("panic", Control, "RFC-0079: the abort itself, and the only irreducible row here"),
    ("assert", Control, "RFC-0015: traps the current test"),
    ("assertEq", Control, "RFC-0015: traps, rendering both sides"),
    ("@join", Control, "waits for a task (the interpreter's are eager, so identity)"),
    // ---- Compiler-directed ---------------------------------------------------
    ("toJson", Compiler, "the walk needs the argument's static type; the writer is std/json"),
    ("fromJson", Compiler, "as `toJson`; the reader is std/jsonread + std/jsondec"),
    ("moduleInterface", Compiler, "RFC-0021: reads the module graph"),
    ("schemaOf", Compiler, "reflects a type DECLARATION into a Schema literal"),
    ("contractOf", Compiler, "RFC-0071: reflects a module contract"),
    ("jsonSchema", Compiler, "renders a declaration as JSON Schema at compile time"),
    ("value", Compiler, "boxes a scalar into the interpolation enum by its type"),
    ("blackBox", Compiler, "RFC-0055: an optimizer barrier is a backend property"),
    ("raw", Compiler, "RFC-0054: builds a code-quote value"),
    ("rawAt", Compiler, "RFC-0054: a code quote carrying an origin directive"),
    ("render", Compiler, "RFC-0054: a code quote back to text"),
    ("lex", Compiler, "RFC-0054: the compiler's OWN lexer"),
    ("@codeText", Compiler, "the desugar of vyrn\"…\""),
    ("@codeSplice", Compiler, "the desugar of an interpolation inside vyrn\"…\""),
    ("Some", Compiler, "a constructor of the compiler's own Option"),
    ("Ok", Compiler, "a constructor of the compiler's own Result"),
    ("Err", Compiler, "as `Ok`"),
    // ---- A cache, with a stated reason (M5's row) ----------------------------
    ("lineAt", Cache, "memoized line-start table; a generator may not hold state"),
    ("colAt", Cache, "as `lineAt`, sharing the same table"),
    // ---- The semantics differ observably -------------------------------------
    ("parse", Semantics, "WRAPS on overflow where std/num's parseInt64 refuses"),
    // RFC-0083 M2: the one vector operation that is not movable AT ALL. Every
    // other row below was written in Vyrn and priced; a square root cannot be,
    // because no finite sequence of Vyrn arithmetic is the correctly-rounded IEEE
    // result. A Newton iteration differs in the last bits, which under this
    // project's byte-identical promise is a different program.
    ("@f32x4Sqrt", Semantics, "a Vyrn Newton iteration is not the correctly-rounded IEEE result"),
    // RFC-0083 M4: the same argument at the wide lane, and it is the same
    // argument rather than a similar one — the reason has nothing to do with the
    // width.
    ("@f64x2Sqrt", Semantics, "as `@f32x4Sqrt`, at 64 bits"),
    // ---- Movable, refused on a measured cost --------------------------------
    // The float half LEFT via RFC-0081: `std/num`'s `f64Str` is the one
    // implementation, native and wasm route to it, and `direct.rs`'s 511 lines
    // are gone. What is still Rust here is integer/bool rendering, and the
    // interpreter's `{:.6}` — kept deliberately as the differential ORACLE, not
    // as a third peer, since exact decimal formatting cannot be pinned
    // exhaustively over 2^64 inputs.
    ("@str", Measured, "integer rendering: a Vyrn digit loop is 150 ns against 60 ns (2.5x), on every print"),
    ("@concat", Measured, "9.7x native / 11x wasm (580 ns against 60 ns): a Vyrn join must revalidate UTF-8"),
    // RFC-0083 M2. All three ARE movable — `examples/simdbench.vyrn` holds the
    // Vyrn implementations, `main` checks the three engines agree with the
    // builtins, and `vyrn bench` prices them. The Vyrn version is the scalar loop
    // the vector type exists to replace, which is why the ratios are what they are.
    ("@f32x4Min", Measured, "3.6x native (44.4 us against 12.3 us per 65536 lanes); Vyrn needs floatBits for -0.0"),
    ("@f32x4Max", Measured, "3.7x native (44.1 us against 12.1 us per 65536 lanes), the mirror of `min`"),
    // RFC-0083 M4's pair, taken the same way against `minWv`/`maxWv` in
    // `simdbench.vyrn`. The ratio is what the narrow width's is and for the same
    // reason: what the Vyrn version has to reproduce is the NaN rule and the sign
    // of a zero, twenty lines with a `floatBits` in them, and none of that gets
    // cheaper at 64 bits. These are the ONLY named operations the wide width
    // takes besides `sqrt` — the four roundings exist as `f64x2` opcodes and were
    // left out, because a rounding row is this RFC's weakest kind and four more
    // of them would be symmetry rather than evidence.
    ("@f64x2Min", Measured, "2.5x native (39.0 us against 15.2 us per 65536 lanes); the Vyrn version needs floatBits for -0.0"),
    ("@f64x2Max", Measured, "2.5x native (38.7 us against 15.0 us per 65536 lanes), the mirror of `min`"),
    // (`@f32x4Abs` was here, claiming "1.0x native but 3.5x wasm". RFC-0083 M4
    // re-took that number against an INLINE Vyrn spelling and it is 1.07x on wasm
    // (54 ms against 58 ms per 102 M lanes) and 1.00x natively — the 3.5x was four
    // calls Cranelift does not inline, which is a fact about the benchmark's shape
    // and not about `f32x4.abs`. Deleted at `select`'s bar, and with no rule to
    // reproduce to argue against it: clearing the sign bit is one line.)
    // The mask reductions, and their number depends on the DATA in a way the
    // three above do not. Written in Vyrn they are `||`/`&&` over four lane
    // reads, which SHORT-CIRCUITS: on `simdbench`'s monotonic array the chain
    // bails at lane 0 almost every pass and is predicted perfectly, so the ratio
    // there is only 1.3x / 2.3x. The rows quote the unpredictable case instead,
    // because a filter predicate over sorted data is the unusual one and the
    // 1.3x would be a bar `select` (1.1x, deleted) already failed to clear.
    ("@anyTrue", Measured, "2.5x native (1356 ms against 543 ms, unpredictable lanes) / 1.2x wasm; 1.3x when the short circuit predicts"),
    ("@allTrue", Measured, "2.4x native (1170 ms against 481 ms, unpredictable lanes) / 1.2x wasm; 2.3x when the short circuit predicts"),
    // The four roundings, and they are the block where the NATIVE column stops
    // being the one that decides. Baseline x86-64 has no `roundps` (that is
    // SSE4.1) and `vyrn build` passes clang no `-march`, so every one of
    // `llvm.ceil/floor/trunc/rint.v4f32` scalarizes to four libc calls — checked
    // by reading the assembly, and proved separately by `llvm.roundeven`, which
    // scalarizes to a `roundevenf` that does not exist and fails to LINK. So the
    // Vyrn implementations, which are ordinary inline arithmetic, are level with
    // three of them and beat one. Raising the native baseline to x86-64-v2 would
    // make all four one instruction and is the upgrade path; it is a project-wide
    // ISA decision, not this RFC's.
    //
    // Every number below is against an INLINE Vyrn spelling, re-taken in M4. The
    // wasm ratios used to read 5-9x and now read 2-4x: Cranelift does not inline
    // across function boundaries, so the earlier figures were pricing the four
    // helper calls the benchmark happened to be written with. They stayed above
    // the bar where `@f32x4Abs` did not, and the reason generalises — a rounding
    // is a dozen operations and a NaN case, so there is something left over after
    // the call is removed, which a sign-bit mask does not have.
    ("@f32x4Ceil", Measured, "1.1x native (49.9 us against 56.1 us per 65536 lanes) / 2.3x wasm (54 ms against 126 ms per 102 M lanes), both inline"),
    ("@f32x4Floor", Measured, "1.0x native (53.9 us against 52.7 us) / 2.3x wasm (54 ms against 124 ms), the mirror of `ceil`"),
    // The only row in the census where the builtin is SLOWER than the Vyrn it
    // replaces: `truncf` four times against an inlined `cvttss2si` round-trip.
    // Kept anyway, and it is the WEAKEST row here — 1.9x on one backend against a
    // 2.3x loss on the other, plus symmetry: three of four roundings and a
    // hand-written fourth would be a surface with a hole in it, and the Vyrn
    // version needs `floatBits` to keep the sign of a zero. An x86-64-v2 baseline
    // is what would settle it rather than another measurement.
    ("@f32x4Trunc", Measured, "0.43x native — the builtin LOSES (97.2 us against 42.0 us, four truncf calls) — and 1.9x wasm (53 ms against 100 ms), which is all that keeps it"),
    ("@f32x4Nearest", Measured, "1.4x native (49.8 us against 70.7 us) / 4.1x wasm (54 ms against 219 ms); ties-to-even by hand is 20 lines and gets `-0.0` wrong first"),
    // ---- The one finding, and it is CLOSED -----------------------------------
    // (`@charCount` was here, as `Unjustified`: "three implementations of a
    // four-line loop, for ONE caller". It is `std/text`'s `charCountV` now, so the
    // row is gone rather than annotated — a builtin with a reason to exist is what
    // this table lists, and it no longer is one. `Unjustified` stays as a variant
    // because a census whose one interesting category is unrepresentable cannot
    // report the next finding.)
];

/// The two regions of `interp.rs` the census covers, extracted by the method the
/// module doc states. Anchors are matched on the trimmed line and must be unique,
/// so a restructuring fails loudly here rather than silently shrinking the census.
fn interp_builtin_names() -> BTreeSet<String> {
    let src = include_str!("../src/interp.rs");
    let lines: Vec<&str> = src.lines().collect();
    let only = |needle: &str| -> usize {
        let hits: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.trim() == needle)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "the census scan anchors on `{needle}` and found {} — see the method in \
             this file's doc comment and RFC-0078's M1+M5 note",
            hits.len()
        );
        hits[0]
    };
    let call = only("Expr::Call { name, args, line } => {");
    let arms = only("match name.as_str() {");
    assert!(call < arms, "the guards come before the dispatch");
    let end = (arms + 1..lines.len())
        .find(|&i| lines[i].trim() == "_ => {")
        .expect("the dispatch's fallthrough arm");

    let mut names = BTreeSet::new();
    // The guards: `name == "X"` before any argument is evaluated. The `if` is not
    // part of the needle, because a guard may cover two builtins — RFC-0083 M3's
    // vector store is `if name == "@f32x4Store" || name == "@i32x4Store"`, one
    // body for two widths — and anchoring on `if` would silently census only the
    // first of them. Every `name == "` in this region is a guard; the assertion
    // below is what says so if that stops being true.
    for l in &lines[call..arms] {
        let mut rest = *l;
        while let Some(i) = rest.find("name == \"") {
            rest = &rest[i + "name == \"".len()..];
            let end = rest.find('"').expect("an unterminated guard literal");
            names.insert(rest[..end].to_string());
            rest = &rest[end..];
        }
    }
    // The arms. A head is one or more string literals, optionally followed by a
    // match guard, then `=>`. Anything else starting with a quote is data.
    for l in &lines[arms + 1..end] {
        let t = l.trim();
        if !t.starts_with('"') {
            continue;
        }
        let Some(head) = t.split("=>").next().filter(|h| *h != t) else {
            continue;
        };
        let head = head.split(" if ").next().unwrap();
        let parts: Vec<&str> = head.split('|').map(str::trim).collect();
        assert!(
            parts.iter().all(|p| p.len() >= 3 && p.starts_with('"') && p.ends_with('"')),
            "the census scan cannot read this arm head: {t:?}"
        );
        for p in parts {
            names.insert(p.trim_matches('"').to_string());
        }
    }
    names
}

/// The census is a statement about the code, so it is compared to the code.
///
/// A new Rust arm is a new primitive claim and has to be justified in writing;
/// a deleted one has to leave the census with it. Either way the diff names the
/// builtin, so the next reader edits one table instead of guessing.
#[test]
fn the_census_is_the_code() {
    let found = interp_builtin_names();
    let claimed: BTreeSet<String> = CENSUS.iter().map(|(n, ..)| n.to_string()).collect();
    assert_eq!(
        CENSUS.len(),
        claimed.len(),
        "a builtin is claimed twice in the census"
    );
    let missing: Vec<&String> = found.difference(&claimed).collect();
    let stale: Vec<&String> = claimed.difference(&found).collect();
    assert!(
        missing.is_empty(),
        "the interpreter implements {missing:?} and the census does not say why. \
         Add a row with its category — and if it fits none of them, that is a \
         finding for RFC-0078, not a row to invent."
    );
    assert!(
        stale.is_empty(),
        "the census claims {stale:?} and the interpreter no longer implements them. \
         If they were routed into Vyrn, delete the rows and say so in RFC-0078."
    );
    // The count at the commit that landed this, by the stated method: 62 names,
    // 51 in 46 arms plus 11 guards. It is here so a change to the boundary is a
    // visible edit rather than a silently different census. 62 -> 61 when
    // `@charCount` — the census's one `Unjustified` row — became `std/text`'s
    // `charCountV`, 61 -> 62 when RFC-0079 M1 added `panic`, and 62 -> 61 when M3
    // spent that abort on `slice`. The net of the whole RFC is one primitive
    // traded for one primitive, and the one that left had three implementations
    // where the one that arrived has three lines. 61 -> 64 when RFC-0083 M1 added
    // a TYPE the language cannot name — three `View` rows and no operation, since
    // the lane-wise arithmetic is a `BinOp` and never reaches this dispatch. 64 ->
    // 70 when M2 added the memory pair (`Memory`, a bounds trap like `at`'s), the
    // three total operations that are movable and priced (`Measured`), and the one
    // that is not movable at all (`Sqrt`, `Semantics`). M2's comparison operators
    // are `BinOp`s like M1's arithmetic, so the mask cost no rows either — and the
    // operation M2 tried to add and could NOT justify, `select`, is not here
    // because it is not in the interpreter. See RFC-0083's M2 note. 70 -> 72 when
    // RFC-0075 M1 added `fromArray`/`close` — two `Memory` rows for a type that is
    // `Array<T>` at runtime, so they are the array rows again. The pair is the
    // price of the linearity being checkable: a stream has to be unforgeable, and
    // an unforgeable type needs a constructor nothing else can spell. 72 -> 74
    // when the mask reductions completed M2's surface: two more `Measured` rows,
    // and the first pair whose ratio had to be quoted against a stated DATA
    // distribution rather than a workload — a short-circuiting `||` chain is only
    // slow when the branch is unpredictable. 74 -> 79 when the rest of M2's
    // `F32x4` surface landed: `@replaceLane` is a `View` beside `@lane` (a lane
    // written instead of read), and the four roundings are `Measured` rows whose
    // refusal is decided by the WASM column for all four — natively they
    // scalarize to libc calls and one of them, `trunc`, is slower than the Vyrn
    // it replaces. The mask combinators and `-v` cost no rows at all: they are a
    // `BinOp` and a `UnOp`, which this dispatch never sees, which is also why the
    // comparison operators never appeared here. 79 -> 83 when RFC-0083 M3 added a
    // second WIDTH: two `View` rows for building an `I32x4` and two `Memory` rows
    // for moving one, and NOTHING else. `@lane`/`@replaceLane` serve both widths
    // from one arm each, every operator is a `BinOp` or a `UnOp` this dispatch
    // never sees, and the three named operations wasm offers at this width
    // (`min_s`/`max_s`/`abs`) were built and deleted at 1.0x native / 1.05x wasm.
    // A whole width for four rows, none of them `Measured`, is what it looks like
    // when the census is asked BEFORE the arms are written rather than after.
    // 83 -> 82 when M4 re-took `@f32x4Abs`'s number the way M3 took the integer
    // ones — against a Vyrn spelling with no helper call in it — and got 1.07x on
    // the column that had been keeping it. The row is the census's own failure
    // mode caught by the census's own method: a `Measured` row can be wrong about
    // the benchmark rather than about the operation, and the only defence is that
    // the number says which shape it measured. The four roundings were re-taken
    // in the same pass and all four survived, with corrected numbers. 82 -> 83
    // when RFC-0075 M2b added `fromStep`: one `Memory` row for the producer a
    // stream can now hold, beside the buffer it always could. No row for `next` —
    // the pull is emitted inside `for … in` rather than named, so there is no
    // dispatch here to census. 83 -> 84 when RFC-0074 M3a added `serveStream`:
    // one `Syscall` row for the handoff, and none for the pull or the release —
    // the host asks for those through the serve API rather than through a name a
    // program can write, so there is again no dispatch here to census.
    // 84 -> 86 when RFC-0075 M2c made the combinators lazy: `fromWrap` for the
    // wrapper and `pull` for what its step reads. The second row is the one
    // worth noticing — M2b recorded that the pull needed no row BECAUSE it was
    // emitted inside `for … in` rather than named, and a lazy `map` is exactly
    // the thing that has to name it. A combinator written in Vyrn cannot be
    // written without it.
    // 86 -> 87 when RFC-0090 M3 took the stream cursor off Path B: `fromWrap`
    // and `pull` became `boxStream`, `unboxStream` and `pullAt`, which is three
    // rows where there were two. The extra row is the honest one — M2c hid the
    // source's release inside the runtime's walk, so nothing named it; the
    // wrapper takes its own source back out now, and a name is what makes
    // `movecheck` able to check that release.
    // 86 -> 93 when RFC-0083 M4 added the third width: two `View` rows for
    // building an `F64x2`, two `Memory` rows for moving one, `sqrt` as
    // `Semantics` for the reason the narrow one is, and `min`/`max` as the only
    // two `Measured` rows — at 2.5x, which is the narrow width's ratio and the
    // narrow width's reason. Seven rows and not eleven: the four roundings exist
    // at `f64x2` and were deliberately not taken. What did NOT need a row is the
    // whole of the rest — every operator is a `BinOp` or a `UnOp`, and both lane
    // accessors serve the new width from the arm they already had, since a lane
    // accessor is about the lane index and only the RANGE changed.
    // 93 -> 92 when `afree` was deleted, and it is the only row that ever left
    // for having no callers rather than for being routed into Vyrn. It had zero
    // uses in `examples/` and `std/`, and the direct wasm backend never lowered
    // it, so a program that called it could not build for wasm at all. `drop a`
    // is the reclamation, on all three engines. This test named the stale row
    // before anything else did.
    // 92 -> 93 when RFC-0089 M1b added `@copy`. A primitive rather than a Vyrn
    // routine for the reason `@toArray` is one: the operation is the memory
    // model, so its answer is the shape of the value, and a library written in
    // Vyrn has no way to ask what a value is made of.
    // 93 -> 94 when RFC-0090 M3 re-hosted the stream cursor: `fromWrap` and
    // `pull` retired and `boxStream`, `unboxStream` and `pullAt` arrived.
    assert_eq!(found.len(), 94, "the primitive core changed size");
}

/// RFC-0078's acceptance criterion: "No builtin has two *definitions*."
///
/// A censused builtin is implemented in Rust; a routed one is implemented in
/// Vyrn. A name in both lists means an engine holds a second opinion, which is
/// the exact failure this RFC exists to prevent — and the failure mode is silent,
/// since `routed_builtin` runs before the dispatch and the dead arm just stops
/// being reached.
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

/// The refusals this milestone re-checked, pinned with their reasons.
///
/// M1's brief named `logger` and `stringFromBytes` as the last plausibly movable
/// builtins. Both are refused, and the categories are the reasons: `logger` needs
/// a write to a file descriptor Vyrn cannot name (its threshold also *folds*, so
/// routing turns a deleted call into a runtime comparison), and
/// `stringFromBytes` IS the view — it is the only construction of a `String` from
/// bytes there is, which is why M4b(2)'s "wants a primitive the way
/// `floatFromBits` did" resolves to "it already is one".
///
/// The other three are M4c's, restated where the reason can be found:
/// `lineAt`/`colAt` are the cache, and `parse` wraps where `std/num` refuses.
/// M4c's fourth refusal was `slice`, "blocked on the abort primitive" — RFC-0079
/// M3 unblocked it by removing the need to abort rather than by supplying one, so
/// its row is gone and `nothing_is_both_censused_and_routed` is what now enforces
/// that it cannot come back as a second definition.
#[test]
fn the_refusals_keep_their_reasons() {
    let by_name: BTreeMap<&str, Why> = CENSUS.iter().map(|(n, w, _)| (*n, *w)).collect();
    assert!(
        !by_name.contains_key("slice"),
        "`slice` is `std/strpred`'s `sliceV` since RFC-0079 M3 — a census row for it          is a Rust implementation nobody reaches"
    );
    for (name, why) in [
        ("logger", Syscall),
        ("stringFromBytes", View),
        ("lineAt", Cache),
        ("colAt", Cache),
        ("parse", Semantics),
    ] {
        assert_eq!(
            by_name.get(name),
            Some(&why),
            "`{name}` was refused as {why:?} — moving it means writing down why \
             that reason stopped being true"
        );
    }
    // The census's own shape: EVERY remaining arm has a stated reason to be a
    // primitive, and the two `Measured` rows are refusals rather than
    // justifications. The one `Unjustified` row the census found was `@charCount`,
    // and it is `std/text`'s `charCountV` now — so the assertion inverted from
    // "exactly one" to "none", which is the strongest form RFC-0078's boundary
    // claim can take. A row appearing here is a finding, not a row to invent.
    let unjustified: Vec<&str> = CENSUS
        .iter()
        .filter(|(_, w, _)| *w == Unjustified)
        .map(|(n, ..)| *n)
        .collect();
    assert!(unjustified.is_empty(), "{unjustified:?} is in the interpreter with no reason given");
}

/// `Measured` claims a number was taken. This checks one was written down.
///
/// The category means "movable, and refused on a *measured* cost" — and for most
/// of this census's life neither row carried a measurement. `@str`'s said "511
/// lines in direct.rs, three engines, every print", which is a description of the
/// code and a guess about the cost; RFC-0081 measured it and the guess was wrong
/// by enough to delete two of the three implementations. `@concat`'s said "every
/// string concatenation and every interpolation in the repo", which is a
/// description of the call sites.
///
/// So the failure mode is not prose drifting in general — prose cannot be checked
/// — it is *this* category asserting evidence that does not exist. A row claiming
/// a measurement must cite one: some digit followed by `ns`, `ms` or `x`.
///
/// What this deliberately does NOT check is whether the number is still true.
/// Nothing can. `@str`'s row cited 511 deleted lines for two milestones while the
/// suite stayed green, because [`the_census_is_the_code`] pairs arms with rows and
/// never reads the reasons at all. This closes the half that is mechanizable and
/// names the half that is not.
#[test]
fn a_measured_refusal_cites_its_measurement() {
    for (name, why, reason) in CENSUS.iter().filter(|(_, w, _)| *w == Measured) {
        let cites_a_number = reason.as_bytes().windows(2).any(|w| {
            w[0].is_ascii_digit() && (w[1] == b'x' || w[1] == b'n' || w[1] == b'm')
        }) || reason.contains(" ns")
            || reason.contains(" ms");
        assert!(
            cites_a_number,
            "`{name}` is {why:?} — the category asserts a cost was measured, so the \
             reason has to carry the number (`580 ns`, `9.7x`). Refusing on a \
             description of the call sites is how both rows sat unmeasured until \
             RFC-0081 measured one and deleted two implementations. Reason was: \
             {reason:?}"
        );
    }
}
