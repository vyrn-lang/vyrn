//! Every wording a running Vyrn program can die with.
//!
//! Each sentence is a byte-for-byte contract: the fixtures compare stderr. The
//! emitter lays the table out as data and `std/runtime`'s `trapAt` reads it,
//! so no other file spells a wording.
//!
//! A wording is fixed ([`DIV_ZERO`]), split around a runtime value
//! ([`ARRAY_INDEX`] and the [`IO`] entries, whose `%s` is a path, so the pair
//! is the primitive), or filled by a compile-time constant ([`call_depth`],
//! [`region_depth`]). [`line`] frames a trap: a runtime chooses how to write
//! the line, never what it says. `vyrn-cli/tests/traps.rs` fails if running
//! code outside this file spells a trap wording; a comment may quote one.

use std::fmt::Display;

use crate::ast::TypeDecl;
use crate::effects::Effect;

/// The most Vyrn calls in flight at once, on every engine.
///
/// Each backend's function prologue counts calls, so a deep recursion stops
/// with the same Vyrn diagnostic everywhere instead of a host crash. 1,000 is
/// past what recursive descent over real data reaches (`.vyx` markup, GraphQL
/// selections and JSON nest in the tens), and matches CPython. An `extern` is
/// the host's frame and is not counted; a lambda cannot call itself.
pub const CALL_DEPTH_LIMIT: u32 = 1_000;

/// The most bytes one call frame may claim on the wasm backend's shadow stack.
///
/// `vyrn_codegen::wasm::STACK_BYTES` holds [`CALL_DEPTH_LIMIT`] frames of this
/// size, so the depth counter, not the stack, stops a program. The backend
/// refuses a larger frame when it lays it out, naming the function and line.
/// 8 KB is 1.5x the largest frame the corpus builds (5,552 bytes); the stack
/// costs 126 wasm pages, touched only as deep as a program recurses.
pub const FRAME_LIMIT: u32 = 8 * 1024;

/// The native stack a program's frames may use: wasmtime's wasm stack limit.
///
/// It holds [`CALL_DEPTH_LIMIT`] frames of 32 KiB of spilled values each, so the
/// depth counter stops a program before the host stack does. At wasmtime's
/// default of 512 KiB a frame with 500 live values stopped at depth 128.
pub const WASM_STACK_BYTES: usize = 32 * 1024 * 1024;

/// The stack of a thread that runs a program: [`WASM_STACK_BYTES`] and room for
/// the host's own frames. The wasm host's workers reserve it, and so does a
/// Windows native binary's main thread, whose default is 1 MiB. On Linux and
/// macOS a native binary runs on the process stack, which `ulimit -s` sets.
pub const RUN_STACK_BYTES: usize = WASM_STACK_BYTES + 16 * 1024 * 1024;

/// The trap for a program whose frames outgrow the host stack. It is wasm-rt's
/// `wasm_rt_strerror` wording, which the native host prints, so both routes
/// print one sentence.
pub const STACK_EXHAUSTED: &str = "Call stack exhausted";

/// The most elements one array literal may have.
///
/// A literal is built in a frame slot, and the array it becomes needs a second
/// slot in the same frame, so the bound is half of [`FRAME_LIMIT`] over the 8
/// bytes of an `Int64`; the frame bound catches wider elements at their real
/// stride. The checker holds it, so `vyrn check` predicts the build. A longer
/// table belongs in a data segment, which no backend lowers.
pub const ARRAY_LIT_LIMIT: usize = FRAME_LIMIT as usize / 16;

/// The most bytes one value may occupy, and one less than the most one
/// allocation may: every address and size on the wasm route is an `i32`, and
/// every element takes at least one byte. A pass may assume
/// `length <= LENGTH_LIMIT + 1` (the check-elision length postulate): `append`
/// fills a block of `LENGTH_LIMIT + 1` bytes with that many `UInt8`s.
///
/// Two homes enforce it. The emitter refuses a larger size it lays out
/// (`vyrn_codegen::direct::Fn_::extent`). At run time `std/runtime`'s `malloc`
/// traps `out of memory` past `LENGTH_LIMIT + 1` bytes, a Vyrn literal no Rust
/// constant reaches. Memory64 would move both.
pub const LENGTH_LIMIT: u32 = i32::MAX as u32;

/// What the check oracle (`vyrn_lower::check::Mode::Count`) says where a check
/// the compiler proved would have trapped: a compiler defect, never a program's.
pub const PROVED_CHECK_FAILED: &str = "a check the compiler proved failed";

/// How many `region` scopes may be open at once, on every engine: the size of
/// the backends' fixed region stack, and the number in the trap wording.
pub const REGION_MAX: u32 = 64;

/// The Rust stack a compiler thread reserves.
///
/// The loader, the checker and the backends recurse over a file's syntax, so a
/// deeply nested program needs room. Reserved pages stay virtual until a frame
/// touches them, so the number is generous; Windows' default of ~1 MB has
/// overflowed on realistic programs.
pub const DEEP_STACK_BYTES: usize = 512 * 1024 * 1024;

/// The trap for calling an `extern` on a target that provides no
/// host for it. The wasm host (`vyrn-cli`'s `wasmrun`) and the native trap
/// stub `vyrn_codegen::toolchain` writes both print it.
pub fn extern_unavailable(name: &str) -> String {
    format!("extern `{name}` is not available on this target")
}

/// What every engine puts in front of a trap on stderr.
pub const PREFIX: &str = "error: ";

/// Returns one whole line of trap output: the prefix, the message and a
/// newline.
pub fn line(msg: &str) -> String {
    format!("{PREFIX}{msg}\n")
}

/// `a / 0` on an integer.
pub const DIV_ZERO: &str = "division by zero";
/// `a % 0` on an integer. Distinct from [`DIV_ZERO`] because the operator is.
pub const REM_ZERO: &str = "remainder by zero";
/// `Int64::MIN / -1`, whose quotient is not an `Int64`.
pub const DIV_OVERFLOW: &str = "integer overflow in division";
/// A shift by a count outside `0..bits`.
pub const SHIFT_RANGE: &str = "shift amount out of range";
/// An allocation the runtime could not satisfy.
pub const OUT_OF_MEMORY: &str = "out of memory";
/// A stream box read after its stream was taken.
pub const NO_STREAM: &str = "no stream in this box";
/// A `fn` value whose tag names no lowered body: unreachable, and it says so
/// rather than run one.
pub const BAD_FN_VALUE: &str = "internal: invalid function value";
/// `serveStream` in a compiled build.
pub const SERVE_STREAM: &str =
    "serveStream: a compiled build has no accept loop — a live route needs `vyrn serve`";

/// `array index {i} out of bounds`, as the two halves around the index. The
/// pair is the primitive: the direct wasm backend concatenates
/// (`trap_idx(pre, i, post)`), and [`around`] joins it.
pub const ARRAY_INDEX: (&str, &str) = ("array index ", " out of bounds");
/// `string index {i} out of bounds`, in the same shape.
pub const STRING_INDEX: (&str, &str) = ("string index ", " out of bounds");

/// Joins a split wording around one value.
pub fn around(parts: (&str, &str), v: impl Display) -> String {
    format!("{}{v}{}", parts.0, parts.1)
}

/// `call depth exceeds {CALL_DEPTH_LIMIT}`, built from the constant
/// the prologue compares against.
pub fn call_depth() -> String {
    format!("call depth exceeds {}", CALL_DEPTH_LIMIT)
}

/// `region nesting exceeds {REGION_MAX}`.
pub fn region_depth() -> String {
    format!("region nesting exceeds {}", REGION_MAX)
}

/// The eight checks the emitter inserts because the core told it to,
/// as rows of the trap table.
///
/// A row is an index: the wasm route lays out the halves of each row as one
/// data table, and every trap site becomes `trapAt(rule, value)`. The order is
/// the table's layout, so `index` and the data segment cannot disagree.
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

    /// The row's index: what a trap site pushes.
    pub fn index(self) -> u32 {
        Self::ALL.iter().position(|r| *r == self).unwrap() as u32
    }

    /// The census name of the row, for a diagnostic and the records. No program
    /// prints it.
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

    /// The row as a compiled runtime writes it: the text before the value, and the
    /// text after it for the two rows that have a value. A row without a second
    /// half prints no number. The [`line`] framing is included: the prefix opens
    /// the first half and the newline closes the last.
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

/// Returns what a `where` violation says. A record base gets the cross-field
/// wording, because no single value violated it.
pub fn validation(name: &str, record_base: bool) -> String {
    if record_base {
        format!("validation failed: `{name}` violates its `where` clause")
    } else {
        format!("validation failed for `{name}`")
    }
}

/// [`validation`] for a declaration.
pub fn validation_of(decl: &TypeDecl) -> String {
    validation(&decl.name, crate::validate::is_cross_field(decl))
}

/// The I/O error wordings: canonical strings, never OS text, so
/// every engine produces identical `Err` payloads. `%s` is the path. The
/// direct wasm backend splits each on its `%s` into `std/mem`'s `ioTable`,
/// which `std/runtime`'s `ioErr` indexes by position among the entries with a
/// `%s`, so that order is a protocol; [`io_at`] joins it.
pub const IO: &[(&str, &str)] = &[
    ("readerr", "cannot read `%s`"),
    ("writeerr", "cannot write `%s`"),
    ("utf8err", "`%s` is not valid UTF-8"),
    // `listDir`, reached from compiled code only on the generator
    // host path.
    ("listerr", "cannot list `%s`"),
    ("nulerr", "`%s` contains a NUL byte"),
    // A cross-device (`EXDEV`) rename is reported, not degraded to a copy.
    // Other rename failures use `writeerr`.
    ("xdeverr", "cannot rename `%s` across devices"),
    // Byte-bridge errors, with no path: fixed payloads for `stringFromBytes`.
    ("bnul", "bytes contain a NUL byte"),
    // `tallyBytes` traps where `stringFromBytes` answers: a count
    // map's key must be a `String`, and `stringFromBytes` gives the reason.
    (
        "tbytes",
        "tallyBytes: the bytes are not a String — `stringFromBytes` names why",
    ),
    ("butf8", "bytes are not valid UTF-8"),
];

/// The host-boundary externs: `(Vyrn extern name, C shim symbol, the
/// effect of a call to the declaration)`.
///
/// They are not host imports: the C runtime shim implements them on
/// every target (native `timespec_get` and CSPRNG, wasi `clock_time_get` and
/// `random_get`), honoring `VYRN_FIXED_TIME` and `VYRN_FIXED_SEED`. The
/// frontend needs the list too: the floor must know `std/time` imports
/// no host function.
pub const HOST_EXTERNS: &[(&str, &str, Effect)] = &[
    ("hostNowMillis", "__vyrn_now_millis", Effect::Clock),
    (
        "hostMonotonicNanos",
        "__vyrn_monotonic_nanos",
        Effect::Clock,
    ),
    ("hostRandomSeed", "__vyrn_random_seed", Effect::Random),
];

/// Returns the shim symbol for a [`HOST_EXTERNS`] name, or `None` for an
/// ordinary extern, which lowers as a host import.
pub fn host_boundary_extern(name: &str) -> Option<&'static str> {
    HOST_EXTERNS
        .iter()
        .find(|(n, ..)| *n == name)
        .map(|(_, sym, _)| *sym)
}

/// Returns one [`IO`] entry by name.
///
/// # Panics
///
/// On an unknown name: every caller names a literal, and a typo must not
/// become a wrong payload.
pub fn io(name: &str) -> &'static str {
    IO.iter()
        .find(|(n, _)| *n == name)
        .map(|(_, m)| *m)
        .unwrap_or_else(|| panic!("no I/O message named `{name}`"))
}

/// Returns an [`io`] message with its path filled in.
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

    /// A `%s` message splits, and a fixed one does not.
    #[test]
    fn every_io_message_is_either_split_or_fixed() {
        for (n, m) in IO {
            let n_pct = m.matches("%s").count();
            assert!(n_pct <= 1, "`{n}` has {n_pct} `%s`, expected 0 or 1");
            assert_eq!(io_at(n, "P"), m.replace("%s", "P"));
        }
    }

    /// Eight rows, each a wording from this file; only the index rows carry a
    /// value.
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

    /// The framing adds the prefix and a newline and leaves the message alone.
    #[test]
    fn the_framing_is_the_prefix_and_a_newline() {
        assert_eq!(line(DIV_ZERO), "error: division by zero\n");
        assert_eq!(call_depth(), "call depth exceeds 1000");
        assert_eq!(region_depth(), "region nesting exceeds 64");
        assert_eq!(validation("Age", false), "validation failed for `Age`");
        assert_eq!(
            validation("Range", true),
            "validation failed: `Range` violates its `where` clause"
        );
    }
}
