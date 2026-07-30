//! `std/codecs` against the six codec builtins it is a candidate to replace
//! (RFC-0078 M4b).
//!
//! The builtin IS the oracle, and that is the whole point of the milestone.
//! `hexEncode`, `base64Encode`, `urlEncode` and their inverses exist twice today
//! — once in Rust in the interpreter, once as hand-written LLVM IR printed by the
//! textual emitter — and RFC-0078's rule is that a builtin may not have two
//! definitions. Before either copy can be deleted, the Vyrn implementation has to
//! be shown to answer identically, and shown over more than the handful of inputs
//! `examples/encoding.vyrn` prints.
//!
//! So this test generates one Vyrn program that calls BOTH implementations on
//! every input in a wide corpus and prints a line only when they disagree. The
//! corpus is the surface where two codecs can differ: every byte a `String` can
//! hold, every base64 alphabet digit, every printable ASCII byte in each of a
//! group's four positions, all three padding residues, misplaced padding, odd
//! hex lengths, non-hex digits, truncated percent escapes, and every `%XX` in
//! both cases.
//!
//! Written this way the test does not need rewriting when the swap lands: with
//! the builtins gone it becomes the regression pin for whatever replaces them,
//! and until then it is the equivalence proof.
//!
//! # The divergence rule
//!
//! Exactly one class of disagreement is expected, and it is accepted by RULE
//! rather than by an enumerated allow-list, so a NEW divergence cannot hide
//! inside a stale list: a decoder whose bytes contain `0x00`. RFC-0014 forbids a
//! NUL inside a `String` and `stringFromBytes` enforces it, so `std/codecs`
//! answers `None`; the builtin answers `Some`, and does not agree with itself
//! across engines while doing so (the interpreter keeps a Rust `String` holding
//! the byte, the native path hands back a `char*` truncated at it). The test
//! asserts that class is non-empty, so the rule is exercised rather than
//! vacuous.

use std::path::{Path, PathBuf};
use std::process::Command;

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

/// `std/codecs`'s own hand-picked pins, including the NUL rows.
#[test]
fn std_codecs_unit_tests_run_green() {
    unit_tests_green("std/codecs.vyrn", "4 passed, 0 failed");
}

/// The example's three-way rows, which parity runs on all three engines but whose
/// `main` nothing else in the suite asserts a shape for.
#[test]
fn the_example_agrees_with_the_builtins_on_the_interpreter() {
    let file = repo_file("examples/codecbytes.vyrn");
    let out = vyrn().arg("run").arg(&file).output().expect("vyrn run");
    assert!(
        out.status.success(),
        "codecbytes failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    // Every comparison row in the example prints a bool; none may be `false`.
    assert!(!text.lines().any(|l| l.trim_end() == "false"), "a row disagreed:\n{text}");
    assert!(text.lines().filter(|l| l.trim_end() == "true").count() >= 25, "too few rows:\n{text}");
}

const B64_ALPHABET: &str =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// A Vyrn byte-array literal for a string's UTF-8 bytes: `['\x41', '\xc3']`.
///
/// Encoder inputs go in as bytes rather than as string literals on purpose — the
/// corpus is chosen for BYTE coverage, and escaping arbitrary UTF-8 into a source
/// literal is a second thing to get wrong.
fn byte_literal(s: &str) -> String {
    let inner: Vec<String> = s.bytes().map(|b| format!("'\\x{b:02x}'")).collect();
    format!("[{}]", inner.join(", "))
}

/// A Vyrn string literal for printable ASCII — decoder inputs, which are ASCII by
/// definition, so only `\` and `"` need escaping.
fn str_literal(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Encoder inputs: strings chosen so that between them they contain **every byte
/// a Vyrn `String` can hold**.
///
/// Which is not every byte, and that is worth stating: `0x00` is forbidden by
/// RFC-0014, and `0xC0`, `0xC1` and `0xF5`..`0xFF` cannot appear in valid UTF-8 at
/// all — so an encoder can never be handed them and "every byte 0..255" is only
/// reachable through the DECODERS, where it is.
fn encoder_corpus() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // Every ASCII byte, one string each, then all of them at once.
    for b in 1u32..0x80 {
        out.push(char::from_u32(b).unwrap().to_string());
    }
    out.push((1u32..0x80).map(|b| char::from_u32(b).unwrap()).collect());
    // Two-byte forms: the first 64 code points cover every CONTINUATION byte
    // (0x80..0xBF), and stepping by 64 covers every two-byte LEAD (0xC2..0xDF).
    for cp in 0x80u32..0xC0 {
        out.push(char::from_u32(cp).unwrap().to_string());
    }
    for cp in (0x80u32..0x800).step_by(64) {
        out.push(char::from_u32(cp).unwrap().to_string());
    }
    // Three-byte forms: every lead 0xE0..0xEF, skipping the surrogate range,
    // which is not a scalar value.
    for k in 0u32..16 {
        let cp = 0x800 + k * 0x1000;
        if let Some(c) = char::from_u32(cp) {
            out.push(c.to_string());
        }
    }
    // Four-byte forms: every lead 0xF0..0xF4.
    for k in 0u32..5 {
        if let Some(c) = char::from_u32(0x10000 + k * 0x40000) {
            out.push(c.to_string());
        }
    }
    // The boundaries of each UTF-8 width, and the ends of the scalar range.
    for cp in [0x7Fu32, 0x80, 0x7FF, 0x800, 0xD7FF, 0xE000, 0xFFFD, 0xFFFF, 0x10000, 0x10FFFF] {
        out.push(char::from_u32(cp).unwrap().to_string());
    }
    // Lengths 0..=9: base64 pads by `len % 3` and hex by nothing, so the residues
    // have to be walked in bytes rather than in characters.
    for n in 0..10 {
        out.push("a".repeat(n));
        out.push("é".repeat(n));
    }
    // Reserved-character runs, which is where `urlEncode` earns its name.
    for s in [
        "name=a b&x",
        "a+b",
        "?q=1&r=2#frag",
        "/path/to/x",
        "100%",
        "aZ09-_.~",
        "!*'();:@$,[]",
        "\t\n\r",
        "  ",
        "café ☕",
        "Hello, Vyrn!",
    ] {
        out.push(s.to_string());
    }
    out
}

/// Decoder inputs, shared by all three decoders — any text may be handed to any
/// of them, and a decoder that accepts something the builtin rejects is exactly
/// the failure worth catching.
fn decoder_corpus() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // Every byte as hex, in both cases: 256 valid `hexDecode` inputs, of which
    // `00` is the divergence and 0x80..0xFF are lone bytes that are not UTF-8.
    for b in 0u32..256 {
        out.push(format!("{b:02x}"));
        out.push(format!("{b:02X}"));
        // The same byte as a percent escape, both cases.
        out.push(format!("%{b:02x}"));
        out.push(format!("%{b:02X}"));
    }
    // Hex lengths and non-digits.
    for s in [
        "", "4", "48", "486", "4869", "48690", "486900", "zz", "4g", "g4", "4 ", " 4", "0041",
        "004100", "c3a9", "C3A9", "c3A9", "f09f9880", "c3", "7f7f", "ffff", "4869zz", "//", "==",
    ] {
        out.push(s.to_string());
    }
    // Every base64 digit, four times over — one group per alphabet entry.
    for c in B64_ALPHABET.chars() {
        out.push(format!("{c}{c}{c}{c}"));
    }
    // Every printable ASCII byte in each of a group's four positions: the
    // alphabet check, the padding rules and the length rule all live here.
    for b in 0x20u8..0x7F {
        let c = b as char;
        for pos in 0..4 {
            let mut g: Vec<char> = "QQQQ".chars().collect();
            g[pos] = c;
            out.push(g.into_iter().collect());
        }
    }
    // Padding, at every residue and in every wrong place.
    for s in [
        "Q", "QQ", "QQQ", "QQQQ", "QQQQQ", "QQ==", "QUI=", "QUJD", "QQ=", "Q===", "====", "=",
        "==", "===", "Q=QQ", "=QQQ", "QQ==QQ==", "QQQQQQ==", "SGVsbG8=", "SGVsbG8sIFZ5cm4h",
        "AA==", "AAAA", "gA==", "/w==", "////", "++++", "w7/Dvw==", "8J+YgA==",
    ] {
        out.push(s.to_string());
    }
    // Percent escapes: truncated, non-hex, mixed with plain text, and the ones
    // whose bytes are not UTF-8 or are a NUL.
    for s in [
        "%", "%4", "%zz", "%4z", "%z4", "%%", "%%41", "%41", "%41%", "%41%4", "a%2", "a%20b",
        "a+b", "%C3%A9", "%c3%a9", "%C3", "%A9", "%00", "a%00b", "%2F%2f", "%7E", "%E2%98%95",
        "%F0%9F%98%80", "100%25", "name%3Da%20b%26x", "%GG", "% 41", "%4%41",
    ] {
        out.push(s.to_string());
    }
    // Plain text through the decoders, which only `urlDecode` accepts.
    for s in ["abc", "a b", "Hello, Vyrn!", "aZ09-_.~", "\t", " "] {
        out.push(s.to_string());
    }
    out
}

/// One generated program, both implementations, every input — and a line printed
/// only where they disagree.
#[test]
fn std_codecs_agrees_with_the_codec_builtins_over_the_whole_surface() {
    let enc = encoder_corpus();
    let dec = decoder_corpus();

    let mut body = String::new();
    for (i, s) in enc.iter().enumerate() {
        body.push_str(&format!("    enc(fromB({}), \"e{i}\")\n", byte_literal(s)));
    }
    for (i, s) in dec.iter().enumerate() {
        body.push_str(&format!("    dec({}, \"d{i}\")\n", str_literal(s)));
    }

    // `opt` renders a decoded payload through the BUILTIN `hexEncode`, so a
    // mismatch line is ASCII even when the payload holds a NUL or raw UTF-8 — and
    // so the test's own reporting does not depend on the code under test.
    let src = format!(
        r#"import {{
    base64DecodeV,
    base64EncodeV,
    hexDecodeV,
    hexEncodeV,
    urlDecodeV,
    urlEncodeV,
}} from "std/codecs"

fn fromB(b: Array<UInt8>) -> String {{
    return match stringFromBytes(b) {{
        Ok(s) => s,
        Err(e) => "BADINPUT",
    }}
}}

fn opt(o: Option<String>) -> String {{
    return match o {{
        Some(s) => "S" + hexEncode(s),
        None => "None",
    }}
}}

fn chk(mine: String, oracle: String, label: String) {{
    if mine != oracle {{
        print("MISMATCH " + label + " mine=" + mine + " builtin=" + oracle)
    }}
}}

fn enc(x: String, label: String) {{
    chk(hexEncodeV(x), hexEncode(x), "hexE " + label)
    chk(base64EncodeV(x), base64Encode(x), "b64E " + label)
    chk(urlEncodeV(x), urlEncode(x), "urlE " + label)
    chk(opt(hexDecodeV(hexEncode(x))), opt(hexDecode(hexEncode(x))), "hexRT " + label)
    chk(
        opt(base64DecodeV(base64Encode(x))),
        opt(base64Decode(base64Encode(x))),
        "b64RT " + label,
    )
    chk(opt(urlDecodeV(urlEncode(x))), opt(urlDecode(urlEncode(x))), "urlRT " + label)
}}

fn dec(x: String, label: String) {{
    chk(opt(hexDecodeV(x)), opt(hexDecode(x)), "hexD " + label)
    chk(opt(base64DecodeV(x)), opt(base64Decode(x)), "b64D " + label)
    chk(opt(urlDecodeV(x)), opt(urlDecode(x)), "urlD " + label)
}}

fn main() -> Int64 {{
{body}    return 0
}}
"#
    );

    let dir = std::env::temp_dir().join("vyrn-m4b-codecs");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("oracle.vyrn");
    std::fs::write(&file, &src).unwrap();
    let out = vyrn().arg("run").arg(&file).output().expect("vyrn run");
    assert!(
        out.status.success(),
        "oracle program failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("BADINPUT"), "a corpus entry was not valid UTF-8:\n{stdout}");

    // Every line is a disagreement; the NUL class is expected, anything else is a
    // real difference between the Vyrn implementation and the builtin.
    let mut nul: Vec<&str> = Vec::new();
    let mut real: Vec<&str> = Vec::new();
    for line in stdout.lines().map(str::trim_end).filter(|l| !l.is_empty()) {
        assert!(line.starts_with("MISMATCH "), "unexpected output: {line}");
        if is_the_nul_divergence(line) {
            nul.push(line);
        } else {
            real.push(line);
        }
    }
    assert!(
        real.is_empty(),
        "{} of {} inputs disagree beyond the NUL rule:\n{}",
        real.len(),
        enc.len() * 6 + dec.len() * 3,
        real.join("\n")
    );
    // The rule must bite: `00`, `AA==` and `%00` are in the corpus, so an empty
    // NUL class would mean the comparison is not running rather than that the
    // implementations agree everywhere.
    assert!(!nul.is_empty(), "the NUL divergence did not appear — is the corpus reaching it?");
    for expect in ["hexD ", "b64D ", "urlD "] {
        assert!(
            nul.iter().any(|l| l.contains(expect)),
            "no NUL divergence from {expect}:\n{}",
            nul.join("\n")
        );
    }
    eprintln!(
        "{} checks over {} encoder and {} decoder inputs; {} NUL divergences, 0 others",
        enc.len() * 6 + dec.len() * 3,
        enc.len(),
        dec.len(),
        nul.len()
    );
}

/// `std/codecs` said `None` where the builtin produced a string containing a NUL
/// byte — the one accepted difference (RFC-0014's rule that a `String` cannot
/// hold one). Recognised from the line rather than from a list of inputs, so it
/// cannot drift.
fn is_the_nul_divergence(line: &str) -> bool {
    let Some((mine, builtin)) = line.split_once(" builtin=") else { return false };
    if !mine.ends_with(" mine=None") {
        return false;
    }
    let Some(hex) = builtin.strip_prefix('S') else { return false };
    hex.len() % 2 == 0 && hex.as_bytes().chunks(2).any(|p| p == b"00")
}
