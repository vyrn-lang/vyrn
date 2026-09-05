//! Every wording a running Vyrn program can die with — RFC-0101 M5.
//!
//! Parity compares stderr, so each of these sentences is a byte-for-byte
//! contract between three engines. Before this file, **not one of them was held
//! in a place all three could read.** Sharing stopped at a crate boundary:
//! `vyrn-codegen` depends on `vyrn-frontend`, so the two compiled backends could
//! share `IO_MESSAGES`, `validation_message` and the `serveStream` refusal
//! between themselves, and the interpreter — which lives in the crate they both
//! depend on — re-spelled all of them. What the three engines DID share was two
//! integers, `CALL_DEPTH_LIMIT` and `REGION_MAX`, and not the sentences they
//! appear in.
//!
//! What held them together was comment discipline: fourteen comments saying one
//! engine mirrors another, in both directions, the clearest of them being
//! `interp.rs`'s "kept byte-identical to the codegen's format strings so all
//! three backends agree" — a rule, written as a wish, in a comment, in the file
//! that could not import the constant.
//!
//! **This file is that crate.** `vyrn-frontend` is what all three engines can
//! read, which `own` proved when RFC-0101 M4 put the release placement here for
//! the same reason: the interpreter cannot import `vyrn-lower`. RFC-0101 §6.4
//! asked where the trap table would go and this is the answer — below the
//! lowering, not inside it.
//!
//! # The shape, and why it is three shapes
//!
//! A trap wording is one of:
//!
//! 1. **Fixed** — [`DIV_ZERO`] and its neighbours. A `&'static str`.
//! 2. **Split around a runtime value** — [`ARRAY_INDEX`], and every
//!    [`IO`] entry, whose `%s` is a path. The native backend renders these with
//!    `__vyrn_snprintf`; the direct wasm backend has no `snprintf` (RFC-0077
//!    M2j) and concatenates the two halves around the value instead. So the pair
//!    is the primitive and the joined string is the convenience, rather than the
//!    other way round.
//! 3. **Filled by a compile-time constant** — [`call_depth`] and
//!    [`region_depth`]. `interp.rs:93` records what the other choice cost:
//!    `REGION_MAX` "was written eight times across three engines before this
//!    constant, three of those inside string literals".
//!
//! # The framing is the engine's, and only the framing
//!
//! The interpreter's trap value is the message alone; its driver prints
//! `error: ` in front and a newline after. A compiled runtime writes the whole
//! line itself, because it has no driver. [`line`] is that framing, in one
//! place, so an engine chooses HOW to say it and never WHAT.
//!
//! # The gate
//!
//! `vyrn-cli/tests/traps.rs` scans the compiler's own sources and asserts that
//! **no trap wording appears as a literal outside this file** — the shape
//! RFC-0094 M2's reserved-name gate landed. A comment may quote one; running
//! code may not spell one.

use std::collections::HashMap;
use std::fmt::Display;

use crate::ast::TypeDecl;

// ---- the limits every engine holds --------------------------------------
//
// RFC-0125 §3 M5, the tenth slice: these were declared in `interp.rs` because
// that is where the first engine needed them, and 43 references over 12 files
// therefore named the interpreter to read a number that was never its own.
// They are the LANGUAGE's, which is what the wordings below already say —
// `call_depth` and `region_depth` fill themselves from two of them. A constant
// is the same constant wherever it is declared, so nothing here changes what
// any engine does.

/// The most Vyrn calls that may be in flight at once, in EVERY engine (audit
/// A5.3, RFC-0016 addendum).
///
/// A recursion limit is the language's, not the interpreter's. Without one the
/// three engines disagreed about the same program: at depth 30,000 the native
/// binary printed the answer while the reference semantics aborted with a Rust
/// runtime message, exit 127, no `file:line`. Counting the calls — here, and in
/// each backend's function prologue — is what makes the outcome the same
/// everywhere, and makes it a Vyrn diagnostic rather than a death.
///
/// 1,000 is what every engine reaches in every BUILD PROFILE, which is the part
/// the first number (10,000) got wrong. The interpreter spends ~8.5 KB of Rust
/// stack per Vyrn call in a release build and ~190 KB in a debug build — the
/// unoptimized `expr`/`stmt` frames keep every local of a large match alive — so
/// 10,000 fitted in release and died in debug, where CI runs the tests. A limit
/// only one profile honors is not a limit.
///
/// Measured on the debug build against [`INTERP_STACK_BYTES`], with this counter
/// lifted: depth 2,600 runs and 2,800 overflows, so 1,000 keeps 2.6x margin in
/// the profile that has the least. The native binary and `wasmtime` run past
/// 20,000 frames of an ordinary function in either profile. 1,000 is also where
/// CPython settles, and it is past what a recursive descent over real data
/// reaches: `.vyx` markup, a GraphQL selection set and a JSON document all nest
/// in the tens. Data nested deeper than that is data no engine should try — it
/// stops with the same diagnostic everywhere, which is the whole contract.
///
/// An `extern` is NOT counted: it is the host's frame, and no backend gives it a
/// Vyrn prologue. Neither is a lambda body, which has no name to call itself by
/// (RFC-0037) and so cannot recurse without passing through a named function.
pub const CALL_DEPTH_LIMIT: u32 = 1_000;

/// The most bytes one call frame may claim on the wasm backend's shadow stack.
///
/// A backend's stack is finite, and until this number existed nothing compared a
/// frame against it. The wasm backend's whole stack was one 64 KB page, so a
/// function with a 256-byte frame ran out of stack at depth 256 while
/// [`CALL_DEPTH_LIMIT`] said 1,000 and the other two engines reached it — the
/// program died there with `out of bounds memory access` at a wild address, and
/// stopped with the shared diagnostic everywhere else.
///
/// Bounding the frame is what makes the depth one number again.
/// `vyrn_codegen::wasm::STACK_BYTES` holds [`CALL_DEPTH_LIMIT`] of these, so at
/// every depth the counter admits the stack pointer is still above 0: the
/// counter is what stops the program, on every engine, with the same words. A
/// frame past this is refused when it is built, naming the function and its
/// line, because the backend that lays a frame out is the one that knows its
/// size.
///
/// 8 KB is 1.5x the largest frame the corpus builds (5,552 bytes, `createForm`
/// in `examples/shelf/boot.vyrn`), and the stack it implies costs 8,257,536
/// bytes of linear memory — 126 wasm pages a module reserves and touches only as
/// deep as it recurses.
pub const FRAME_LIMIT: u32 = 8 * 1024;

/// The most elements one array literal may have.
///
/// Half of [`FRAME_LIMIT`], over the eight bytes of an `Int64`: a literal is
/// built in a frame slot, and the widest element type an ordinary literal has is
/// what turns one bound into the other. HALF, because the slot is not all a
/// literal costs — the array it becomes needs its own slot in the same frame, so
/// a bound of a whole frame would let the checker admit a literal the backend
/// then refuses. Wider elements — a literal of records — are caught by the frame
/// bound itself, which knows the real stride.
///
/// The checker holds this rather than either backend, because the other half of
/// the defect is one no frame can express: the textual backend lowers a literal
/// to one `insertvalue` per element over an aggregate of the full width, so
/// 100,000 elements ran clang for 2 m 53 s and died `LLVM ERROR: out of memory`,
/// after `vyrn check` had said `ok` in 0.1 s. Refusing in the checker is what
/// makes `check` predict the build, and makes all three engines refuse the same
/// literal.
///
/// The corpus's largest literal has 24 elements, so this is 21x anything written
/// so far. A table longer than it belongs in a data segment rather than in
/// instructions, which is a lowering neither backend has yet.
pub const ARRAY_LIT_LIMIT: usize = FRAME_LIMIT as usize / 16;

/// How many `region` scopes may be open at once, in EVERY engine.
///
/// The two backends each keep a fixed stack of region records, so the number is
/// the length of an array in one and a reserved block in the other, and it is in
/// the trap's wording as well. It was written eight times across three engines
/// before this constant, three of those inside string literals; the backends'
/// comparisons had already drifted apart in signedness. One number, read by
/// everything that has an opinion about it.
pub const REGION_MAX: u32 = 64;

/// The Rust stack every thread that runs the interpreter reserves.
///
/// Reserving is cheap — the pages are virtual until a frame touches them — and
/// what is touched is [`CALL_DEPTH_LIMIT`] frames deep at worst: ~8.5 MB in a
/// release build, ~190 MB in a debug one. Both sit well inside this, which is
/// what gives the limit above room to be the same number in either profile.
pub const INTERP_STACK_BYTES: usize = 512 * 1024 * 1024;

/// The trap for calling an `extern` (RFC-0012) on a target that provides no
/// host for it. Parity compares these bytes byte-for-byte
/// (`vyrn-cli/tests/parity.rs`), so there is one definition: the interpreter
/// raises it, and the native trap stub `vyrn_codegen::toolchain` writes prints
/// it. Neither backend spells it a second time.
pub fn extern_unavailable(name: &str) -> String {
    format!("extern `{name}` is not available on this target")
}

/// What every engine puts in front of a trap before it reaches a terminal.
///
/// The interpreter's driver adds it; a compiled runtime writes it as part of
/// [`line`]. It is here so that "what a trap looks like on stderr" is one fact.
pub const PREFIX: &str = "error: ";

/// One whole line of a compiled runtime's trap output: the prefix, the message,
/// a newline. What `vyrn run` prints for the same trap is the same three pieces
/// assembled by its driver.
pub fn line(msg: &str) -> String {
    format!("{PREFIX}{msg}\n")
}

// ---- the fixed wordings -------------------------------------------------

/// `a / 0` on an integer.
pub const DIV_ZERO: &str = "division by zero";
/// `a % 0` on an integer. Distinct from [`DIV_ZERO`] because the operator is.
pub const REM_ZERO: &str = "remainder by zero";
/// `Int64::MIN / -1`, whose quotient is not an `Int64`.
pub const DIV_OVERFLOW: &str = "integer overflow in division";
/// A shift by a count outside `0..bits`. Both backends mirror the
/// interpreter's `y < 0 || y >= bits` — one condition, now one sentence.
pub const SHIFT_RANGE: &str = "shift amount out of range";
/// An allocation the runtime could not satisfy. Six sites across three
/// runtimes, the C shim (`toolchain.rs`) included.
pub const OUT_OF_MEMORY: &str = "out of memory";
/// A stream box read after its stream was taken (RFC-0075).
pub const NO_STREAM: &str = "no stream in this box";
/// A `fn` value whose tag names no lowered body — unreachable, and it says so
/// rather than running one.
pub const BAD_FN_VALUE: &str = "internal: invalid function value";
/// `serveStream` in a compiled build (RFC-0074 M3a). One constant so the two
/// engines cannot drift, which is the rule every wording in this file follows.
pub const SERVE_STREAM: &str =
    "serveStream: a compiled build has no accept loop — a live route needs `vyrn serve`";

// ---- the wordings with a runtime value in the middle ---------------------

/// `array index {i} out of bounds`, as the two halves around the index.
///
/// The pair is the primitive because one backend cannot format: the direct wasm
/// backend concatenates (`trap_idx(pre, i, post)`), the native one hands both
/// halves to one `fprintf`, and the interpreter joins them with [`around`].
pub const ARRAY_INDEX: (&str, &str) = ("array index ", " out of bounds");
/// `string index {i} out of bounds` — the same shape, the other container.
pub const STRING_INDEX: (&str, &str) = ("string index ", " out of bounds");

/// A split wording, joined around one value.
pub fn around(parts: (&str, &str), v: impl Display) -> String {
    format!("{}{v}{}", parts.0, parts.1)
}

/// `array index {i} out of bounds`.
pub fn array_index(i: impl Display) -> String {
    around(ARRAY_INDEX, i)
}

/// `string index {i} out of bounds`.
pub fn string_index(i: impl Display) -> String {
    around(STRING_INDEX, i)
}

// ---- the wordings a compile-time constant fills --------------------------

/// `call depth exceeds {CALL_DEPTH_LIMIT}` — RFC-0004 §4.
///
/// Built from the constant the prologue compares against, so the number in the
/// message and the number enforced cannot drift. There was a fourth copy of
/// this sentence in `vyrn-play`.
pub fn call_depth() -> String {
    format!("call depth exceeds {}", CALL_DEPTH_LIMIT)
}

/// `region nesting exceeds {REGION_MAX}` — the LLVM prelude's fixed region
/// stack, and the depth the interpreter traps at, in the same words on purpose.
pub fn region_depth() -> String {
    format!("region nesting exceeds {}", REGION_MAX)
}

// ---- the trap table ------------------------------------------------------

/// The eight rows RFC-0125 §2.3 gives the EMITTER: a check the emitter inserts
/// because the core told it to, not one a producer runs. The census of §3 M6
/// sorts every value boundary into five lines and this is the second of them.
///
/// A row is an index. The wasm route lays the two halves of each row out as
/// one data table and every trap site becomes `trapAt(rule, value)` — the
/// shape §2.3 names, "a call with a table index". Before it, each site pushed
/// its own interned wording, so the emitter knew eight sentences; now it knows
/// eight numbers and the table knows the sentences.
///
/// The order is the census's, and it is the table's layout, so `index` and the
/// data segment cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    ArrayIndex,
    StringIndex,
    DivZero,
    RemZero,
    DivOverflow,
    ShiftRange,
    CallDepth,
    RegionDepth,
}

impl Rule {
    /// Every row, in table order.
    pub const ALL: [Rule; 8] = [
        Rule::ArrayIndex,
        Rule::StringIndex,
        Rule::DivZero,
        Rule::RemZero,
        Rule::DivOverflow,
        Rule::ShiftRange,
        Rule::CallDepth,
        Rule::RegionDepth,
    ];

    /// The row's index — what a trap site pushes.
    pub fn index(self) -> u32 {
        Self::ALL.iter().position(|r| *r == self).unwrap() as u32
    }

    /// The census's name for the row (RFC-0125 §3 M6), for a diagnostic and
    /// for the record. Not a wording: no program prints it.
    pub fn census(self) -> &'static str {
        match self {
            Rule::ArrayIndex => "array-index",
            Rule::StringIndex => "string-index",
            Rule::DivZero => "int-div-zero",
            Rule::RemZero => "int-rem-zero",
            Rule::DivOverflow => "int-div-overflow",
            Rule::ShiftRange => "shift-range",
            Rule::CallDepth => "call-depth",
            Rule::RegionDepth => "region-depth",
        }
    }

    /// The row as a compiled runtime writes it: what stands before the value,
    /// and what stands after it for the two rows that HAVE a value. A row with
    /// no second half prints no number, which is how the runtime tells the two
    /// shapes apart with one comparison.
    ///
    /// The framing is [`line`]'s, because a compiled runtime writes the whole
    /// line itself (the note above): the prefix opens the first half and the
    /// newline closes the last one.
    pub fn parts(self) -> (String, Option<String>) {
        let split = |p: (&str, &str)| (format!("{PREFIX}{}", p.0), Some(format!("{}\n", p.1)));
        match self {
            Rule::ArrayIndex => split(ARRAY_INDEX),
            Rule::StringIndex => split(STRING_INDEX),
            Rule::DivZero => (line(DIV_ZERO), None),
            Rule::RemZero => (line(REM_ZERO), None),
            Rule::DivOverflow => (line(DIV_OVERFLOW), None),
            Rule::ShiftRange => (line(SHIFT_RANGE), None),
            Rule::CallDepth => (line(&call_depth()), None),
            Rule::RegionDepth => (line(&region_depth()), None),
        }
    }
}

// ---- validation ----------------------------------------------------------

/// What a `where` violation says. A record base gets the cross-field wording,
/// because what violated it is not one value.
///
/// Three copies before this: `vyrn-codegen`'s `validation_message` for the two
/// backends, `codec.rs`'s for the JSON decoder, and the interpreter's own — and
/// the interpreter spelled it at four sites.
pub fn validation(name: &str, record_base: bool) -> String {
    if record_base {
        format!("validation failed: `{name}` violates its `where` clause")
    } else {
        format!("validation failed for `{name}`")
    }
}

/// [`validation`] for a declaration, which is what every caller actually holds.
pub fn validation_of(decl: &TypeDecl) -> String {
    validation(&decl.name, crate::validate::is_cross_field(decl))
}

/// [`validation`] for a named type, resolved through this program's
/// declarations — the form the interpreter asks in, where all it has is a name.
pub fn validation_named(name: &str, types: &HashMap<String, TypeDecl>) -> String {
    validation(
        name,
        types.get(name).is_some_and(crate::validate::is_cross_field),
    )
}

// ---- the I/O boundary (RFC-0014) -----------------------------------------

/// The I/O error wording: canonical Vyrn strings and NEVER OS text, so every
/// engine produces byte-identical `Err` payloads. `%s` is the path.
///
/// One list because parity compares these bytes. The textual emitter interns
/// them as `@.io.<name>` globals and renders them with `__vyrn_snprintf`; the
/// direct wasm backend splits each on its `%s`; the interpreter joins them with
/// [`io_at`]. A message reworded here changes all three, and none can hold a
/// private copy that drifts — which is what the interpreter had, at thirteen
/// sites.
pub const IO: &[(&str, &str)] = &[
    ("readerr", "cannot read `%s`"),
    ("writeerr", "cannot write `%s`"),
    ("utf8err", "`%s` is not valid UTF-8"),
    // `listDir` (RFC-0021), reachable from a compiled module only on the
    // generator-host path (RFC-0076 M2) — the wording still lives here, with the
    // rest, rather than in the shim that renders it.
    ("listerr", "cannot list `%s`"),
    ("nulerr", "`%s` contains a NUL byte"),
    // RFC-0044: a cross-device (`EXDEV`) rename — surfaced distinctly instead of
    // silently degrading to copy. Ordinary not-found/permission rename failures
    // reuse `writeerr` (rewriting the destination).
    ("xdeverr", "cannot rename `%s` across devices"),
    // Byte-bridge errors (M2, no path): fixed payloads for `stringFromBytes`.
    ("bnul", "bytes contain a NUL byte"),
    // RFC-0116: `tallyBytes` traps where `stringFromBytes` answers — a count
    // map's key must be a String, and the caller who wants the WHY calls
    // `stringFromBytes` and reads the `Err`.
    (
        "tbytes",
        "tallyBytes: the bytes are not a String — `stringFromBytes` names why",
    ),
    ("butf8", "bytes are not valid UTF-8"),
];

/// RFC-0043's host-boundary externs: `(Vyrn extern name, C shim symbol)`.
///
/// These are NOT ordinary RFC-0012 externs. An RFC-0012 `extern fn` is a HOST
/// import — it lowers to an import from the wasm `vyrn` namespace, traps on
/// native, and never instantiates under a wasi host that does not supply it.
/// These three are implemented by the C runtime shim on EVERY target (native
/// `timespec_get`/CSPRNG, wasi `clock_time_get`/`random_get` via wasi-libc,
/// both honoring `VYRN_FIXED_TIME`/`VYRN_FIXED_SEED`), which is what keeps a
/// clock or random example a full three-way parity citizen.
///
/// The list is here rather than in `vyrn-codegen` because the frontend needs it
/// too: RFC-0103's floor asks whether a module imports a host function, and the
/// answer for `std/time` is no. A second copy of the three names in the frontend
/// would be exactly the drift this file exists to end.
pub const HOST_EXTERNS: &[(&str, &str)] = &[
    ("hostNowMillis", "__vyrn_now_millis"),
    ("hostMonotonicNanos", "__vyrn_monotonic_nanos"),
    ("hostRandomSeed", "__vyrn_random_seed"),
];

/// The shim symbol for a [`HOST_EXTERNS`] name, or `None` for an ordinary
/// RFC-0012 extern (which is a host import and lowers as one).
pub fn host_boundary_extern(name: &str) -> Option<&'static str> {
    HOST_EXTERNS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, sym)| *sym)
}

/// One [`IO`] entry by name. Panics on an unknown key, because every caller
/// names a literal and a typo is a wrong payload rather than a miss.
pub fn io(name: &str) -> &'static str {
    IO.iter()
        .find(|(n, _)| *n == name)
        .map(|(_, m)| *m)
        .unwrap_or_else(|| panic!("no I/O message named `{name}`"))
}

/// The two halves of an [`io`] message around its `%s`, for a backend that
/// concatenates rather than formatting.
pub fn io_parts(name: &str) -> (&'static str, &'static str) {
    io(name)
        .split_once("%s")
        .unwrap_or_else(|| panic!("`{name}` has no `%s`"))
}

/// An [`io`] message with its path filled in — the interpreter's form, and the
/// only one that needs no host formatter.
pub fn io_at(name: &str, path: impl Display) -> String {
    let m = io(name);
    match m.split_once("%s") {
        Some((a, b)) => format!("{a}{path}{b}"),
        None => m.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two shapes the table promises, asserted rather than assumed: a
    /// `%s` message splits, and a fixed one does not pretend to.
    #[test]
    fn every_io_message_is_either_split_or_fixed() {
        for (n, m) in IO {
            let n_pct = m.matches("%s").count();
            assert!(n_pct <= 1, "`{n}` has {n_pct} `%s`, expected 0 or 1");
            if n_pct == 1 {
                let (a, b) = io_parts(n);
                assert_eq!(format!("{a}%s{b}"), *m);
            }
            assert_eq!(io_at(n, "P"), m.replace("%s", "P"));
        }
    }

    /// The table the wasm route indexes: eight rows, each a wording this file
    /// already held, and only the two index rows carry a value.
    #[test]
    fn every_trap_table_row_is_one_of_the_two_shapes() {
        for (i, r) in Rule::ALL.iter().enumerate() {
            assert_eq!(r.index() as usize, i, "the order is the layout");
            let (pre, post) = r.parts();
            assert!(pre.starts_with(PREFIX), "{}", r.census());
            match post {
                Some(p) => assert!(p.ends_with('\n'), "{}", r.census()),
                None => assert!(pre.ends_with('\n'), "{}", r.census()),
            }
        }
        assert_eq!(Rule::ArrayIndex.parts().1.unwrap(), " out of bounds\n");
        assert_eq!(Rule::DivZero.parts().0, line(DIV_ZERO));
    }

    /// The framing an engine adds, and the message it must not touch.
    #[test]
    fn the_framing_is_the_prefix_and_a_newline() {
        assert_eq!(line(DIV_ZERO), "error: division by zero\n");
        assert_eq!(array_index(7), "array index 7 out of bounds");
        assert_eq!(call_depth(), "call depth exceeds 1000");
        assert_eq!(region_depth(), "region nesting exceeds 64");
        assert_eq!(validation("Age", false), "validation failed for `Age`");
        assert_eq!(
            validation("Range", true),
            "validation failed: `Range` violates its `where` clause"
        );
    }
}
