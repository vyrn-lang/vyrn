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

use crate::ast::{Type, TypeDecl};

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
    format!("call depth exceeds {}", crate::interp::CALL_DEPTH_LIMIT)
}

/// `region nesting exceeds {REGION_MAX}` — the LLVM prelude's fixed region
/// stack, and the depth the interpreter traps at, in the same words on purpose.
pub fn region_depth() -> String {
    format!("region nesting exceeds {}", crate::interp::REGION_MAX)
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
    validation(&decl.name, matches!(decl.base, Type::Record(_)))
}

/// [`validation`] for a named type, resolved through this program's
/// declarations — the form the interpreter asks in, where all it has is a name.
pub fn validation_named(name: &str, types: &HashMap<String, TypeDecl>) -> String {
    validation(
        name,
        types
            .get(name)
            .is_some_and(|d| matches!(d.base, Type::Record(_))),
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
    ("butf8", "bytes are not valid UTF-8"),
];

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
