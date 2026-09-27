//! The text tier: `std/text`'s `chars`, `decodeUtf8`, `charCount`, `lineAt` and
//! `colAt`, judged against Rust or a pinned digest.
//!
//! The builtins are routed into `std/text`, so comparing a builtin with its Vyrn
//! function compares one function with itself. `chars` is pinned by a digest;
//! the malformed half is judged by `std::str::from_utf8`; `lineAt`/`colAt` by a
//! count made in Rust.

use std::path::{Path, PathBuf};
use std::process::Command;

use vyrn_frontend::hash::sha256_hex;

fn repo_file(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
        .canonicalize()
        .unwrap()
}

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

fn unit_tests_green(rel: &str, expected: &str) {
    let module = repo_file(rel);
    let out = vyrn().arg("test").arg(&module).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{rel} unit tests failed:\n{combined}");
    assert!(
        combined.contains(expected),
        "expected `{expected}`:\n{combined}"
    );
}

#[test]
fn text_pins_hold() {
    unit_tests_green("examples/textbytes.vyrn", "3 passed, 0 failed");
}

/// A Vyrn byte-array literal (`['\x41', '\x00']`). The malformed half cannot be
/// spelled as a `String`, so both halves use one encoding.
fn byte_array(bytes: &[u8]) -> String {
    let inner: Vec<String> = bytes.iter().map(|b| format!("'\\x{b:02x}'")).collect();
    format!("[{}]", inner.join(", "))
}

fn utf8_of(cp: u32) -> Vec<u8> {
    char::from_u32(cp)
        .expect("a scalar value")
        .to_string()
        .into_bytes()
}

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
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim_end().to_string())
        .collect()
}

/// Prints both verdicts for one buffer, `stringFromBytes` + `chars` and
/// `decodeUtf8`, for the Rust side to judge against `from_utf8`.
const DECODE_HARNESS: &str = r#"import { chars, decodeUtf8, showCps } from "std/text"

fn mine(b: Array<UInt8>) -> String {
    return match decodeUtf8(b) {
        Some(cs) => showCps(cs),
        None => "reject",
    }
}

/// The other side: `chars` on the String the builtin validator built.
/// `chars` has one spelling; a second name for the same function would
/// make this side compare `chars` with itself.
fn theirs(b: Array<UInt8>) -> String {
    return match stringFromBytes(b) {
        Ok(s) => showCps(chars(s)),
        Err(e) => "reject",
    }
}

/// Both verdicts, side by side, so the Rust side can compare each of them with
/// `std::str::from_utf8`.
///
/// `mine` is not compared with `theirs` here. `stringFromBytes` calls
/// `std/text`, so both sides read `utf8Width`, and their agreement would
/// prove nothing. Rust's decoder judges
/// both.
fn row(b: Array<UInt8>) -> String {
    return theirs(b) + "|" + mine(b)
}
"#;

/// The SHA-256 of the `chars` transcript over the codepoint corpus below,
/// captured from the Rust `str::chars` implementation (`494f883`). `chars` is
/// `decodeUtf8` itself, so only a digest can pin it.
const CODEPOINT_DIGEST: &str = "013ef87f67f7fa2b21ac9da8ae22c0261b3f4fb48e0d8fa7af7042ca32428b59";

/// Exhaustive below U+0800 and sampled above, since a three-byte form differs
/// from its neighbour only in a continuation byte. Multi-codepoint buffers
/// catch a decoder that resynchronizes wrongly.
#[test]
fn the_chars_builtin_decodes_the_codepoint_space_exactly_as_it_did() {
    let mut cps: Vec<u32> = (1..0x800).collect();
    cps.extend((0x800..0x10000).step_by(53));
    cps.extend((0x10000..0x110000).step_by(521));
    // Spelled out so a step size can never skip one.
    cps.extend([
        0x7f, 0x80, 0x7ff, 0x800, 0xd7ff, 0xe000, 0xfffd, 0xffff, 0x10000, 0x10ffff,
    ]);
    cps.retain(|c| !(0xD800..=0xDFFF).contains(c));
    cps.sort_unstable();
    cps.dedup();

    let mut buffers: Vec<Vec<u8>> = cps.iter().map(|c| utf8_of(*c)).collect();
    buffers.push(Vec::new());
    for w in cps.chunks(7) {
        buffers.push(w.iter().flat_map(|c| utf8_of(*c)).collect());
    }

    // `showCps` renders scalar values, so a wrong codepoint that still renders
    // cannot hide.
    let harness = r#"import { chars, showCps } from "std/text"

fn row(b: Array<UInt8>) -> String {
    return match stringFromBytes(b) {
        Ok(s) => showCps(chars(s)),
        Err(e) => "reject",
    }
}
"#;
    let calls: String = buffers
        .iter()
        .map(|b| format!("    print(row({}))\n", byte_array(b)))
        .collect();
    let src = format!("{harness}\nfn main() -> Int64 {{\n{calls}    return 0\n}}\n");
    let lines = run_lines("vyrn-m4c-codepoints", &src);
    assert_eq!(lines.len(), buffers.len(), "one line per buffer");
    assert!(
        !lines.iter().any(|l| l == "reject"),
        "a valid buffer was refused"
    );

    // Spot pins give a digest mismatch a readable neighbour.
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
        "`chars` decodes {} buffers differently than the pinned \
         digest records",
        buffers.len()
    );
}

/// `decodeUtf8` and `stringFromBytes` must each refuse exactly what
/// `std::str::from_utf8` refuses.
///
/// Every lead byte meets ten continuation bytes straddling each range boundary
/// (0x7F/0x80, 0x8F/0x90, 0x9F/0xA0, 0xBF/0xC0) at widths two to four: where an
/// off-by-one in the overlong and surrogate bounds lives. NUL is excluded: it is
/// valid UTF-8 a `String` cannot hold, pinned in the `test` blocks.
#[test]
fn decodeutf8_and_stringfrombytes_refuse_exactly_what_rust_refuses() {
    let tails: [u8; 10] = [0x41, 0x7f, 0x80, 0x8f, 0x90, 0x9f, 0xa0, 0xbf, 0xc0, 0xff];
    let mut buffers: Vec<Vec<u8>> = Vec::new();

    // Every byte alone: a lone continuation, and every lead truncated.
    for b in 1u8..=0xff {
        buffers.push(vec![b]);
    }
    // 0xBF pads the positions past the first, so a rejection is attributable
    // to the byte being varied.
    for lead in 0xc0u8..=0xff {
        for t in tails {
            buffers.push(vec![lead, t]);
            buffers.push(vec![lead, t, 0xbf]);
            buffers.push(vec![lead, t, 0xbf, 0xbf]);
        }
    }
    // Surrogates encoded as scalars (CESU-8); why 0xED's first continuation
    // stops at 0x9F.
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
        buffers.push(vec![
            0xf0,
            0x80,
            0x80 | (cp >> 6) as u8,
            0x80 | (cp & 0x3f) as u8,
        ]);
    }
    // Proper prefixes of valid sequences: truncation at every cut.
    for cp in [0xe9u32, 0x20ac, 0x1f600, 0x10ffff] {
        let full = utf8_of(cp);
        for cut in 1..full.len() {
            buffers.push(full[..cut].to_vec());
            // A decoder that skips the wrong number of bytes recovers silently
            // here.
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
    // `stringFromBytes` refuses NUL before it looks at UTF-8.
    buffers.retain(|b| !b.contains(&0));

    let calls: String = buffers
        .iter()
        .map(|b| format!("    print(row({}))\n", byte_array(b)))
        .collect();
    let src = format!("{DECODE_HARNESS}\nfn main() -> Int64 {{\n{calls}    return 0\n}}\n");
    let lines = run_lines("vyrn-m4b-malformed", &src);
    assert_eq!(lines.len(), buffers.len(), "one line per buffer");
    // The judge is Rust's `from_utf8`: `stringFromBytes` and `decodeUtf8` share
    // `utf8Width`, so neither can judge the other.
    let bad: Vec<String> = lines
        .iter()
        .zip(&buffers)
        .filter_map(|(l, b)| {
            let want = match std::str::from_utf8(b) {
                Ok(s) => s
                    .chars()
                    .map(|c| (c as u32).to_string())
                    .collect::<Vec<_>>()
                    .join(","),
                Err(_) => "reject".to_string(),
            };
            (*l != format!("{want}|{want}")).then(|| format!("{b:02x?}: {l} want {want}"))
        })
        .collect();
    assert!(
        bad.is_empty(),
        "{} of {} disagree:\n{}",
        bad.len(),
        buffers.len(),
        bad.join("\n")
    );
}

/// Every offset, because a walk can agree at 0 and disagree after the last
/// newline. Offsets run from -3 to `len + 3` to pin the clamping.
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
        "a\r\nb\r\nc",          // CRLF: one break, and the CR holds a column
        "\r\n\r\n",             // nothing but CRLF
        "héllo\nwörld\n😀 end", // multi-byte, so a byte column is not a char column
        "é\né\né",              // a multi-byte codepoint spanning a column boundary
    ];

    let harness = r#"fn row(b: Array<UInt8>, off: Int64) -> String {
    return lineAt(b, off).toString() + ":" + colAt(b, off).toString()
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
        .filter_map(|(l, (i, off))| {
            let b = texts[*i].as_bytes();
            let o = (*off).clamp(0, b.len() as i64) as usize;
            let line = 1 + b[..o].iter().filter(|&&c| c == b'\n').count();
            let col = o - b[..o]
                .iter()
                .rposition(|&c| c == b'\n')
                .map_or(0, |p| p + 1)
                + 1;
            let want = format!("{line}:{col}");
            (*l != want).then(|| format!("{:?} @ {off}: {l}, counted {want}", texts[*i]))
        })
        .collect();
    assert!(
        bad.is_empty(),
        "{} of {} disagree:\n{}",
        bad.len(),
        rows.len(),
        bad.join("\n")
    );

    // The column counts bytes: U+00E9 is two bytes, so the `\n` after it on
    // line 1 is column 3.
    let three = lines
        .iter()
        .zip(&rows)
        .find(|(_, (i, off))| texts[*i] == "é\né\né" && *off == 2)
        .map(|(l, _)| l.clone())
        .expect("the offset after the first é");
    assert_eq!(
        three, "1:3",
        "a column is a byte offset, not a character index"
    );
}

/// `charCountV` is a byte scan for non-continuation bytes and `charsV` a full
/// first-byte-dispatch decode, so they are independent implementations of one
/// fact; `chars`'s answers are pinned by `CODEPOINT_DIGEST`. Counting a
/// continuation byte over-counts and skipping a lead byte under-counts.
#[test]
fn charcount_agrees_with_chars_over_the_codepoint_corpus() {
    let mut cps: Vec<u32> = (0x20..0x800).step_by(3).collect();
    cps.extend((0x800..0x10000).step_by(97));
    cps.extend((0x10000..0x110000).step_by(521));
    cps.extend([
        0x7f, 0x80, 0x7ff, 0x800, 0xd7ff, 0xe000, 0xfffd, 0xffff, 0x10000, 0x10ffff,
    ]);
    cps.retain(|c| !(0xD800..=0xDFFF).contains(c));
    cps.sort_unstable();
    cps.dedup();

    // A scan that mishandles the transition between two sequences is invisible
    // on one codepoint at a time.
    let mut buffers: Vec<Vec<u8>> = Vec::new();
    buffers.push(Vec::new());
    for w in cps.chunks(7) {
        buffers.push(w.iter().flat_map(|c| utf8_of(*c)).collect());
    }

    let harness = r#"import { chars } from "std/text"

/// `charCount` beside `chars(s).length` and `byteLength`, so a mismatch says which
/// of the three disagreed rather than just that something did.
fn cmp(s: String) -> String {
    let scan = s.charCount()
    let decode = chars(s).length
    if scan == decode {
        return "ok " + scan.toString() + " of " + s.byteLength.toString()
    }
    return "MISMATCH scan=" + scan.toString() + " decode=" + decode.toString()
}

fn row(b: Array<UInt8>) -> String {
    return match stringFromBytes(b) {
        Ok(s) => cmp(s),
        Err(e) => "reject",
    }
}
"#;
    let calls: String = buffers
        .iter()
        .map(|b| format!("    print(row({}))\n", byte_array(b)))
        .collect();
    let src = format!("{harness}\nfn main() -> Int64 {{\n{calls}    return 0\n}}\n");
    let lines = run_lines("vyrn-charcount-oracle", &src);
    assert_eq!(lines.len(), buffers.len(), "one line per buffer");
    assert!(
        !lines.iter().any(|l| l.starts_with("MISMATCH")),
        "{:?}",
        lines.iter().find(|l| l.starts_with("MISMATCH"))
    );
    assert!(
        !lines.iter().any(|l| l == "reject"),
        "a valid buffer was refused"
    );
    // Not vacuous: the two answers can only differ where bytes exceed scalars.
    let multibyte = lines
        .iter()
        .filter_map(|l| {
            let mut it = l.strip_prefix("ok ")?.split(" of ");
            let n: usize = it.next()?.parse().ok()?;
            let b: usize = it.next()?.parse().ok()?;
            Some((n, b))
        })
        .filter(|(n, b)| b > n)
        .count();
    assert!(
        multibyte > 100,
        "only {multibyte} buffers had more bytes than scalars"
    );

    // `examples/bytecount.vyrn`'s rows, equal on all three engines.
    let rows = run_lines(
        "vyrn-charcount-oracle",
        "fn show(s: String) -> String {\n    \
         return s.byteLength.toString() + \"/\" + s.charCount().toString()\n}\n\
         fn main() -> Int64 {\n    \
         print(show(\"\"))\n    print(show(\"hello\"))\n    print(show(\"héllo\"))\n    \
         print(show(\"☕\"))\n    print(show(\"😀\"))\n    return 0\n}\n",
    );
    assert_eq!(
        rows,
        ["0/0", "5/5", "6/5", "3/1", "4/1"],
        "bytecount.vyrn's pinned rows"
    );
}
