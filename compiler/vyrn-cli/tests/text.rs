//! The text tier (RFC-0078 M4b(2), converted by M4c).
//!
//! `std/text` writes `chars`, `lineAt` and `colAt` as ordinary Vyrn. **One of the
//! three was routed** and the file's shape follows that split, because a converted
//! oracle has to stop comparing a function with itself:
//!
//! 1. the two modules' inline `test` blocks, which pin the hand-picked cases as
//!    literals (nothing else in the suite runs an example's `test` blocks);
//! 2. the `chars` builtin over ~2,000 codepoints, as a **pinned digest**. It was
//!    `decodeUtf8` against `chars`; after M4c those are one function, so what the
//!    test asserts now is that the builtin's answers over 5,972 buffers hash to a
//!    literal captured from the C/Rust implementations before the swap — and the
//!    pre- and post-swap digests are the SAME value;
//! 3. `decodeUtf8`'s accept/reject against `stringFromBytes`'s over ~1,400
//!    MALFORMED byte strings — **still a live oracle**, because `stringFromBytes`
//!    did not move. This is the half that matters: a decoder that waves through an
//!    overlong form still decodes every valid string correctly, so a corpus of
//!    valid text proves almost nothing about it;
//! 4. `lineAtV`/`colAtV` against `lineAt`/`colAt` at every offset — **also still a
//!    live oracle**, because those two builtins did not move either (the
//!    interpreter's memoized line-start table is why, and retiring it is M5's
//!    question). Every offset rather than a chosen few precisely because the two
//!    engine implementations compute the answer differently: a binary search over
//!    the memoized table against a backward walk to the previous LF.

use std::path::{Path, PathBuf};
use std::process::Command;

use vyrn_frontend::hash::sha256_hex;

fn repo_file(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel).canonicalize().unwrap()
}

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

/// Run one module's inline `test` blocks and assert the green count.
fn unit_tests_green(rel: &str, expected: &str) {
    let module = repo_file(rel);
    let out = vyrn().arg("test").arg(&module).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{rel} unit tests failed:\n{combined}");
    assert!(combined.contains(expected), "expected `{expected}`:\n{combined}");
}

#[test]
fn text_pins_hold() {
    unit_tests_green("examples/textbytes.vyrn", "3 passed, 0 failed");
}

#[test]
fn std_text_unit_tests_run_green() {
    unit_tests_green("std/text.vyrn", "3 passed, 0 failed");
}

/// A Vyrn byte-array literal (`['\x41', '\x00']`). Byte literals rather than a
/// string literal on purpose: the malformed half of the corpus cannot be spelled
/// as a `String` at all, and using one encoding for both halves means the valid
/// rows exercise the same path.
fn byte_array(bytes: &[u8]) -> String {
    let inner: Vec<String> = bytes.iter().map(|b| format!("'\\x{b:02x}'")).collect();
    format!("[{}]", inner.join(", "))
}

fn utf8_of(cp: u32) -> Vec<u8> {
    char::from_u32(cp).expect("a scalar value").to_string().into_bytes()
}

/// Run a generated program and return its stdout lines.
fn run_lines(dir: &str, src: &str) -> Vec<String> {
    let dir = std::env::temp_dir().join(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("oracle.vyrn");
    std::fs::write(&file, src).unwrap();
    let out = vyrn().arg("run").arg(&file).output().expect("vyrn run");
    assert!(
        out.status.success(),
        "generated program failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).lines().map(|l| l.trim_end().to_string()).collect()
}

/// The comparator, in Vyrn: for one byte buffer, `decodeUtf8`'s answer against
/// `stringFromBytes` + `chars`, and `charsV`'s against `chars` too. Prints `ok`
/// or a diff, so the Rust side asserts on lines rather than reimplementing UTF-8
/// a fifth time.
///
/// The oracle is all-or-nothing: `stringFromBytes` either builds a `String` (and
/// then `chars` is the truth about its codepoints) or refuses. `decodeUtf8` must
/// make the same call on the same bytes.
const DECODE_HARNESS: &str = r#"import { charsV, decodeUtf8, showCps } from "std/text"

fn mine(b: Array<UInt8>) -> String {
    return match decodeUtf8(b) {
        Some(cs) => showCps(cs),
        None => "reject",
    }
}

/// The builtin's answer, twice: `chars` on the String the builtin validator built,
/// and `charsV` on the same String. They must agree with each other and with
/// `mine`.
fn theirs(b: Array<UInt8>) -> String {
    return match stringFromBytes(b) {
        Ok(s) => showCps(chars(s)) + "|" + showCps(charsV(s)),
        Err(e) => "reject",
    }
}

fn row(b: Array<UInt8>) -> String {
    let t = theirs(b)
    let m = mine(b)
    if t == "reject" {
        if m == "reject" {
            return "ok"
        }
        return "MISMATCH builtin rejected, decodeUtf8 gave " + m
    }
    if t == m + "|" + m {
        return "ok"
    }
    return "MISMATCH decodeUtf8 " + m + " vs builtin " + t
}
"#;

/// The SHA-256 of the `chars` transcript over the codepoint corpus below.
///
/// Captured from the pre-swap build (`494f883`: Rust's `str::chars` in the
/// interpreter) and re-measured after the routing — **the two are the same value**,
/// which is the statement M4c needs and the one a `charsV == chars` comparison can
/// no longer make. `std/text`'s `decodeUtf8` therefore decodes all 5,972 buffers
/// byte for byte as the deleted implementations did.
const CODEPOINT_DIGEST: &str =
    "013ef87f67f7fa2b21ac9da8ae22c0261b3f4fb48e0d8fa7af7042ca32428b59";

/// The `chars` builtin over the codepoint space, pinned (RFC-0078 M4b(2), M4c).
///
/// Exhaustive where the encoding changes shape (every scalar below U+0800, so both
/// one- and two-byte forms are covered byte for byte) and sampled above it, since a
/// three-byte form differs from its neighbour only in a continuation byte. Then the
/// same codepoints in multi-codepoint buffers, because a decoder that
/// resynchronizes wrongly only shows up on a sequence.
///
/// It used to compare `decodeUtf8` and `charsV` against `chars`. After M4c the
/// builtin IS `charsV` (which is `decodeUtf8` plus a `bytes`), so all three sides
/// are one function and the comparison proves nothing. The digest does.
#[test]
fn the_chars_builtin_decodes_the_codepoint_space_exactly_as_it_did() {
    let mut cps: Vec<u32> = (1..0x800).collect();
    cps.extend((0x800..0x10000).step_by(53));
    cps.extend((0x10000..0x110000).step_by(521));
    // The boundaries, spelled out so a step size can never skip one.
    cps.extend([0x7f, 0x80, 0x7ff, 0x800, 0xd7ff, 0xe000, 0xfffd, 0xffff, 0x10000, 0x10ffff]);
    cps.retain(|c| !(0xD800..=0xDFFF).contains(c));
    cps.sort_unstable();
    cps.dedup();

    let mut buffers: Vec<Vec<u8>> = cps.iter().map(|c| utf8_of(*c)).collect();
    buffers.push(Vec::new());
    for w in cps.chunks(7) {
        buffers.push(w.iter().flat_map(|c| utf8_of(*c)).collect());
    }

    // One `print` per buffer, of the builtin's codepoints. `stringFromBytes` builds
    // the `String` (it did not move, so this is still the only route from bytes) and
    // `showCps` renders the scalar values rather than the text — a wrong codepoint
    // that still renders would otherwise hide.
    let harness = r#"import { showCps } from "std/text"

fn row(b: Array<UInt8>) -> String {
    return match stringFromBytes(b) {
        Ok(s) => showCps(chars(s)),
        Err(e) => "reject",
    }
}
"#;
    let calls: String =
        buffers.iter().map(|b| format!("    print(row({}))\n", byte_array(b))).collect();
    let src = format!("{harness}\nfn main() -> Int64 {{\n{calls}    return 0\n}}\n");
    let lines = run_lines("vyrn-m4c-codepoints", &src);
    assert_eq!(lines.len(), buffers.len(), "one line per buffer");
    assert!(!lines.iter().any(|l| l == "reject"), "a valid buffer was refused");

    // Spot pins, so a digest mismatch has a readable neighbour. Keyed by the
    // buffer's bytes rather than by index.
    let row_of = |cp: u32| -> &str {
        let want = utf8_of(cp);
        let i = buffers.iter().position(|b| *b == want).expect("in corpus");
        lines[i].as_str()
    };
    assert_eq!(row_of(0x41), "65");
    assert_eq!(row_of(0x7f), "127");
    assert_eq!(row_of(0x80), "128"); // the first two-byte form
    assert_eq!(row_of(0x7ff), "2047");
    assert_eq!(row_of(0x800), "2048"); // the first three-byte form
    assert_eq!(row_of(0xffff), "65535");
    assert_eq!(row_of(0x10000), "65536"); // the first four-byte form
    assert_eq!(row_of(0x10ffff), "1114111"); // the last codepoint there is

    let digest = sha256_hex(lines.join("\n").as_bytes());
    assert_eq!(
        digest,
        CODEPOINT_DIGEST,
        "`chars` decodes {} buffers differently than it did before RFC-0078 M4c \
         routed it into `std/text`",
        buffers.len()
    );
}

/// The malformed half: `decodeUtf8` must refuse exactly what `stringFromBytes`
/// refuses (RFC-0078 M4b).
///
/// This is the row the milestone exists for. Every shape RFC-0077 M2g pinned by
/// hand is in here, but so is the cross product that a hand list cannot cover:
/// every lead byte against ten continuation bytes chosen to straddle each
/// range boundary the encoding cares about (0x7F/0x80, 0x8F/0x90, 0x9F/0xA0,
/// 0xBF/0xC0), at widths two, three and four. That is where an off-by-one in the
/// overlong and surrogate bounds lives, and it is invisible to any corpus of
/// valid text.
///
/// NUL is excluded and pinned separately (in both modules' `test` blocks):
/// `0x00` is valid UTF-8 that a `String` cannot hold, so `stringFromBytes`
/// refuses it for RFC-0014's reason rather than a decoding one — the one place
/// the two verdicts legitimately differ.
#[test]
fn decodeutf8_refuses_exactly_what_stringfrombytes_refuses() {
    let tails: [u8; 10] = [0x41, 0x7f, 0x80, 0x8f, 0x90, 0x9f, 0xa0, 0xbf, 0xc0, 0xff];
    let mut buffers: Vec<Vec<u8>> = Vec::new();

    // Every byte on its own: a lone continuation, and every lead truncated to
    // nothing.
    for b in 1u8..=0xff {
        buffers.push(vec![b]);
    }
    // Every lead byte against each tail, at each width. 0xBF pads the positions
    // past the first, so a rejection is attributable to the byte being varied.
    for lead in 0xc0u8..=0xff {
        for t in tails {
            buffers.push(vec![lead, t]);
            buffers.push(vec![lead, t, 0xbf]);
            buffers.push(vec![lead, t, 0xbf, 0xbf]);
        }
    }
    // The surrogate range, encoded as if it were a scalar — CESU-8's mistake,
    // and the reason 0xED's first continuation stops at 0x9F.
    for cp in (0xd800u32..=0xdfff).step_by(97) {
        buffers.push(vec![
            0xe0 | (cp >> 12) as u8,
            0x80 | ((cp >> 6) & 0x3f) as u8,
            0x80 | (cp & 0x3f) as u8,
        ]);
    }
    // Overlong encodings of values that fit in fewer bytes: every ASCII byte
    // spelled in two, three and four.
    for cp in [0x00u32, 0x01, 0x2f, 0x41, 0x7f, 0x80, 0x7ff] {
        if cp != 0 {
            buffers.push(vec![0xc0 | (cp >> 6) as u8, 0x80 | (cp & 0x3f) as u8]);
        }
        buffers.push(vec![0xe0, 0x80 | (cp >> 6) as u8, 0x80 | (cp & 0x3f) as u8]);
        buffers.push(vec![0xf0, 0x80, 0x80 | (cp >> 6) as u8, 0x80 | (cp & 0x3f) as u8]);
    }
    // Proper prefixes of valid sequences: truncation at every cut.
    for cp in [0xe9u32, 0x20ac, 0x1f600, 0x10ffff] {
        let full = utf8_of(cp);
        for cut in 1..full.len() {
            buffers.push(full[..cut].to_vec());
            // And a truncated sequence followed by valid text, which is where a
            // decoder that skips the wrong number of bytes recovers silently.
            let mut mixed = full[..cut].to_vec();
            mixed.push(b'z');
            buffers.push(mixed);
        }
    }
    // Above U+10FFFF: the five-byte forms UTF-8 originally allowed.
    for lead in 0xf5u8..=0xfd {
        buffers.push(vec![lead, 0x80, 0x80, 0x80]);
        buffers.push(vec![lead, 0x80, 0x80, 0x80, 0x80]);
    }
    // No NUL anywhere: `stringFromBytes` refuses it before it looks at UTF-8.
    buffers.retain(|b| !b.contains(&0));

    let calls: String =
        buffers.iter().map(|b| format!("    print(row({}))\n", byte_array(b))).collect();
    let src = format!("{DECODE_HARNESS}\nfn main() -> Int64 {{\n{calls}    return 0\n}}\n");
    let lines = run_lines("vyrn-m4b-malformed", &src);
    assert_eq!(lines.len(), buffers.len(), "one line per buffer");
    let bad: Vec<String> = lines
        .iter()
        .zip(&buffers)
        .filter(|(l, _)| *l != "ok")
        .map(|(l, b)| format!("{b:02x?}: {l}"))
        .collect();
    assert!(bad.is_empty(), "{} of {} disagree:\n{}", bad.len(), buffers.len(), bad.join("\n"));
}

/// `lineAtV`/`colAtV` against `lineAt`/`colAt`, at every offset of every buffer
/// (RFC-0078 M4b).
///
/// Every offset rather than a chosen few, because the two engine implementations
/// compute the answer differently — the interpreter binary-searches a memoized
/// table of line starts, the native shim walks backwards to the previous LF — and
/// a third implementation agreeing with both at offset 0 and disagreeing at the
/// byte after the last newline is exactly the failure mode. Offsets run from -3
/// to `len + 3`, since both builtins clamp and the clamping is unwritten
/// behaviour nothing else pins.
#[test]
fn line_and_column_match_the_builtins_at_every_offset() {
    let texts: [&str; 12] = [
        "",
        "x",
        "\n",
        "\n\n\n",
        "ab\ncd\n\nx",
        "no newline at all",
        "ends with one\n",
        "\nstarts with one",
        "a\r\nb\r\nc",           // CRLF: one break, and the CR holds a column
        "\r\n\r\n",             // nothing but CRLF
        "héllo\nwörld\n😀 end", // multi-byte, so a byte column is not a char column
        "é\né\né",              // a multi-byte codepoint spanning a column boundary
    ];

    let harness = r#"import { colAtV, lineAtV } from "std/text"

fn row(b: Array<UInt8>, off: Int64) -> String {
    let ml = lineAtV(b, off)
    let mc = colAtV(b, off)
    let tl = lineAt(b, off)
    let tc = colAt(b, off)
    if ml == tl && mc == tc {
        return "ok " + tl.toString() + ":" + tc.toString()
    }
    return "MISMATCH mine " + ml.toString() + ":" + mc.toString() + " builtin " +
        tl.toString() + ":" + tc.toString()
}
"#;
    let mut rows: Vec<(usize, i64)> = Vec::new();
    let mut calls = String::new();
    for (i, t) in texts.iter().enumerate() {
        let b = t.as_bytes();
        for off in -3i64..=(b.len() as i64 + 3) {
            calls.push_str(&format!("    print(row({}, {off}))\n", byte_array(b)));
            rows.push((i, off));
        }
    }
    let src = format!("{harness}\nfn main() -> Int64 {{\n{calls}    return 0\n}}\n");
    let lines = run_lines("vyrn-m4b-linecol", &src);
    assert_eq!(lines.len(), rows.len(), "one line per (buffer, offset)");
    let bad: Vec<String> = lines
        .iter()
        .zip(&rows)
        .filter(|(l, _)| !l.starts_with("ok"))
        .map(|(l, (i, off))| format!("{:?} @ {off}: {l}", texts[*i]))
        .collect();
    assert!(bad.is_empty(), "{} of {} disagree:\n{}", bad.len(), rows.len(), bad.join("\n"));

    // The column counts BYTES, not codepoints, and that is a measurement off the
    // builtin rather than a choice: `é` is two bytes, so the `\n` after `é` on
    // line 1 of "é\né\né" is column 3. Asserted here so the property is stated
    // somewhere a reader will find it, not just implied by 400 green rows.
    let three = lines
        .iter()
        .zip(&rows)
        .find(|(_, (i, off))| texts[*i] == "é\né\né" && *off == 2)
        .map(|(l, _)| l.clone())
        .expect("the offset after the first é");
    assert_eq!(three, "ok 1:3", "a column is a byte offset, not a character index");
}
