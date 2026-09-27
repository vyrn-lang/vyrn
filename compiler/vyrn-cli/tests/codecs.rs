//! The six `std/codecs` functions, pinned over the surface where two codecs can
//! differ. The corpus program prints one line per answer and the
//! test pins the SHA-256 of the transcript; spot pins give a failure a readable
//! neighbour. The corpus generator is deterministic, so a digest replaces a
//! golden file.
//!
//! To repin after a deliberate change, take the `actual` value from the failure
//! and diff the transcript written to `%TEMP%/vyrn-m4c-codecs/` against the
//! previous one.

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

/// Nothing else in the suite runs an example's `test` blocks.
#[test]
fn the_example_pins_hold() {
    unit_tests_green("examples/codecbytes.vyrn", "3 passed, 0 failed");
    let file = repo_file("examples/codecbytes.vyrn");
    let out = vyrn().arg("run").arg(&file).output().expect("vyrn run");
    assert!(
        out.status.success(),
        "codecbytes failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.lines().any(|l| l.trim_end() == "false"),
        "a round trip failed:\n{text}"
    );
    assert!(text.lines().count() >= 35, "too few rows:\n{text}");
    assert!(text.contains("4869"), "the hex pin is missing:\n{text}");
}

const B64_ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// A Vyrn byte-array literal for a string's UTF-8 bytes: `['\x41', '\xc3']`.
/// Bytes, not a string literal, so escaping UTF-8 into source cannot go wrong.
fn byte_literal(s: &str) -> String {
    let inner: Vec<String> = s.bytes().map(|b| format!("'\\x{b:02x}'")).collect();
    format!("[{}]", inner.join(", "))
}

/// A Vyrn string literal for printable ASCII; only `\` and `"` need escaping.
fn str_literal(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Encoder inputs that together contain every byte a Vyrn `String` can hold.
/// `0x00` is forbidden and `0xC0`, `0xC1`, `0xF5`..`0xFF` are never
/// valid UTF-8, so every byte 0..255 is reached only through the decoders.
fn encoder_corpus() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for b in 1u32..0x80 {
        out.push(char::from_u32(b).unwrap().to_string());
    }
    out.push((1u32..0x80).map(|b| char::from_u32(b).unwrap()).collect());
    // The first 64 two-byte code points cover every continuation byte
    // (0x80..0xBF); stepping by 64 covers every two-byte lead (0xC2..0xDF).
    for cp in 0x80u32..0xC0 {
        out.push(char::from_u32(cp).unwrap().to_string());
    }
    for cp in (0x80u32..0x800).step_by(64) {
        out.push(char::from_u32(cp).unwrap().to_string());
    }
    // Every three-byte lead 0xE0..0xEF; surrogates are not scalar values.
    for k in 0u32..16 {
        let cp = 0x800 + k * 0x1000;
        if let Some(c) = char::from_u32(cp) {
            out.push(c.to_string());
        }
    }
    // Every four-byte lead 0xF0..0xF4.
    for k in 0u32..5 {
        if let Some(c) = char::from_u32(0x10000 + k * 0x40000) {
            out.push(c.to_string());
        }
    }
    // The boundaries of each UTF-8 width, and the ends of the scalar range.
    for cp in [
        0x7Fu32, 0x80, 0x7FF, 0x800, 0xD7FF, 0xE000, 0xFFFD, 0xFFFF, 0x10000, 0x10FFFF,
    ] {
        out.push(char::from_u32(cp).unwrap().to_string());
    }
    // Base64 pads by the byte length mod 3, so walk lengths in 1- and 2-byte
    // characters.
    for n in 0..10 {
        out.push("a".repeat(n));
        out.push("é".repeat(n));
    }
    // Reserved characters for `urlEncode`.
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

/// Decoder inputs, each handed to all three decoders.
fn decoder_corpus() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // Every byte as hex and as a percent escape, both cases. `00` is the NUL
    // row; 0x80..0xFF alone are not UTF-8.
    for b in 0u32..256 {
        out.push(format!("{b:02x}"));
        out.push(format!("{b:02X}"));
        out.push(format!("%{b:02x}"));
        out.push(format!("%{b:02X}"));
    }
    for s in [
        "", "4", "48", "486", "4869", "48690", "486900", "zz", "4g", "g4", "4 ", " 4", "0041",
        "004100", "c3a9", "C3A9", "c3A9", "f09f9880", "c3", "7f7f", "ffff", "4869zz", "//", "==",
    ] {
        out.push(s.to_string());
    }
    for c in B64_ALPHABET.chars() {
        out.push(format!("{c}{c}{c}{c}"));
    }
    // Every printable ASCII byte in each of a group's four positions.
    for b in 0x20u8..0x7F {
        let c = b as char;
        for pos in 0..4 {
            let mut g: Vec<char> = "QQQQ".chars().collect();
            g[pos] = c;
            out.push(g.into_iter().collect());
        }
    }
    for s in [
        "Q",
        "QQ",
        "QQQ",
        "QQQQ",
        "QQQQQ",
        "QQ==",
        "QUI=",
        "QUJD",
        "QQ=",
        "Q===",
        "====",
        "=",
        "==",
        "===",
        "Q=QQ",
        "=QQQ",
        "QQ==QQ==",
        "QQQQQQ==",
        "SGVsbG8=",
        "SGVsbG8sIFZ5cm4h",
        "AA==",
        "AAAA",
        "gA==",
        "/w==",
        "////",
        "++++",
        "w7/Dvw==",
        "8J+YgA==",
    ] {
        out.push(s.to_string());
    }
    for s in [
        "%",
        "%4",
        "%zz",
        "%4z",
        "%z4",
        "%%",
        "%%41",
        "%41",
        "%41%",
        "%41%4",
        "a%2",
        "a%20b",
        "a+b",
        "%C3%A9",
        "%c3%a9",
        "%C3",
        "%A9",
        "%00",
        "a%00b",
        "%2F%2f",
        "%7E",
        "%E2%98%95",
        "%F0%9F%98%80",
        "100%25",
        "name%3Da%20b%26x",
        "%GG",
        "% 41",
        "%4%41",
    ] {
        out.push(s.to_string());
    }
    for s in ["abc", "a b", "Hello, Vyrn!", "aZ09-_.~", "\t", " "] {
        out.push(s.to_string());
    }
    out
}

/// The corpus program's preamble. `dump` renders a payload as hex without
/// calling `hexEncode`, so the report does not depend on the code under test.
const PREAMBLE: &str = r#"import { base64Decode, base64Encode, hexDecode, hexEncode, urlDecode, urlEncode } from "std/codecs"

fn nib(n: UInt8) -> UInt8 {
    if n < 10 {
        return '0' + n
    }
    return 'a' + n - 10
}

fn dump(s: String) -> String {
    let b = bytes(s)
    let mut out: Array<UInt8> = []
    let mut i = 0
    while i < b.length {
        out.push(nib(b[i] >> 4))
        out.push(nib(b[i] & 15))
        i = i + 1
    }
    return match stringFromBytes(out) {
        Ok(v) => v,
        Err(e) => "?",
    }
}

fn opt(o: Option<String>) -> String {
    return match o {
        Some(s) => "S" + dump(s),
        None => "None",
    }
}

fn fromB(b: Array<UInt8>) -> String {
    return match stringFromBytes(b) {
        Ok(s) => s,
        Err(e) => "BADINPUT",
    }
}

fn enc(x: String, label: String) {
    print(label + " hexE " + hexEncode(x))
    print(label + " b64E " + base64Encode(x))
    print(label + " urlE " + urlEncode(x))
    print(label + " hexRT " + opt(hexDecode(hexEncode(x))))
    print(label + " b64RT " + opt(base64Decode(base64Encode(x))))
    print(label + " urlRT " + opt(urlDecode(urlEncode(x))))
}

fn dec(x: String, label: String) {
    print(label + " hexD " + opt(hexDecode(x)))
    print(label + " b64D " + opt(base64Decode(x)))
    print(label + " urlD " + opt(urlDecode(x)))
}
"#;

fn corpus_program(enc: &[String], dec: &[String]) -> String {
    let mut body = String::new();
    for (i, s) in enc.iter().enumerate() {
        body.push_str(&format!("    enc(fromB({}), \"e{i}\")\n", byte_literal(s)));
    }
    for (i, s) in dec.iter().enumerate() {
        body.push_str(&format!("    dec({}, \"d{i}\")\n", str_literal(s)));
    }
    format!("{PREAMBLE}\nfn main() -> Int64 {{\n{body}    return 0\n}}\n")
}

/// The SHA-256 of the corpus program's stdout.
const CORPUS_DIGEST: &str = "2c1e8a949d6a051aea91bd9b6ca0fe67b8a8b1c6bb0a6e26ca7b163dfddac675";

#[test]
fn the_codec_builtins_answer_exactly_this_over_the_whole_surface() {
    let enc = encoder_corpus();
    let dec = decoder_corpus();
    let src = corpus_program(&enc, &dec);

    let dir = std::env::temp_dir().join("vyrn-m4c-codecs");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("corpus.vyrn");
    std::fs::write(&file, &src).unwrap();
    let out = vyrn().arg("run").arg(&file).output().expect("vyrn run");
    assert!(
        out.status.success(),
        "corpus program failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    std::fs::write(dir.join("transcript.txt"), stdout.as_bytes()).unwrap();
    assert!(
        !stdout.contains("BADINPUT"),
        "a corpus entry was not valid UTF-8"
    );
    let rows = stdout.lines().count();
    assert_eq!(rows, enc.len() * 6 + dec.len() * 3, "one line per check");

    // Spot pins before the digest, so a failure names a value. Keyed by input,
    // not index, so reordering the corpus cannot move them to another row.
    let enc_row = |input: &str, kind: &str| -> String {
        let i = enc
            .iter()
            .position(|s| s == input)
            .expect("encoder input in corpus");
        stdout
            .lines()
            .find(|l| l.starts_with(&format!("e{i} {kind} ")))
            .unwrap_or("<missing>")
            .to_string()
    };
    let dec_row = |input: &str, kind: &str| -> String {
        let i = dec
            .iter()
            .position(|s| s == input)
            .expect("decoder input in corpus");
        stdout
            .lines()
            .find(|l| l.starts_with(&format!("d{i} {kind} ")))
            .unwrap_or("<missing>")
            .to_string()
    };
    let ends = |row: &str| -> String { row.rsplit(' ').next().unwrap().to_string() };
    assert_eq!(
        ends(&enc_row("Hello, Vyrn!", "hexE")),
        "48656c6c6f2c205679726e21"
    );
    assert_eq!(ends(&enc_row("Hello, Vyrn!", "b64E")), "SGVsbG8sIFZ5cm4h");
    assert_eq!(ends(&enc_row("name=a b&x", "urlE")), "name%3Da%20b%26x");
    // `/` is alphabet entry 63, which a naive table gets wrong.
    assert_eq!(ends(&dec_row("////", "b64D")), "None"); // decodes to non-UTF-8
    assert_eq!(ends(&dec_row("%C3%A9", "urlD")), "Sc3a9"); // e with acute accent
    assert_eq!(ends(&dec_row("QQ=", "b64D")), "None"); // length not a multiple of 4

    // A `String` cannot hold a NUL, so a decoder that would produce one
    // declines. Spelled out because engines once disagreed here.
    for (input, kind) in [
        ("00", "hexD"),
        ("004100", "hexD"),
        ("AA==", "b64D"),
        ("%00", "urlD"),
    ] {
        let row = dec_row(input, kind);
        assert_eq!(
            ends(&row),
            "None",
            "the NUL row `{input}` through {kind} must be None; got {row}"
        );
    }

    let digest = sha256_hex(stdout.as_bytes());
    assert_eq!(
        digest,
        CORPUS_DIGEST,
        "the codec builtins' answers moved over {rows} checks ({} encoder, {} decoder inputs). \
         The transcript is at {}. If the change is deliberate, diff it against the previous one \
         and repin.",
        enc.len(),
        dec.len(),
        dir.join("transcript.txt").display()
    );
}

/// Reaches `std/json` by injection (`toJson`), `std/text` both injected
/// (`charCount`) and imported (`chars`), and `std/codecs` and `std/strpred` by
/// plain import. The user declares names those modules hold privately or
/// export; injection keeps them apart with the `$` prefix, a plain import with
/// name-privacy renaming.
#[test]
fn four_runtime_modules_link_at_once_and_the_users_names_win() {
    let src = r#"import { hexEncode } from "std/codecs"
import { chars } from "std/text"
import { contains, startsWith } from "std/strpred"

type Point = { x: Int64, y: Int64 }

/// Every name `std/codecs` declares privately, plus one each from `std/text` and
/// `std/strpred`. All of them must mean THESE functions here.
fn hexDigit(n: Int64) -> Int64 {
    return n + 1000
}

fn hexVal(c: Int64) -> Int64 {
    return c + 2000
}

fn decoded(s: String) -> String {
    return "mine:" + s
}

fn ascii(s: String) -> String {
    return "ascii:" + s
}

fn b64Val(c: Int64) -> Int64 {
    return c + 3000
}

fn showCps(a: Int64) -> String {
    return "cps:" + a.toString()
}

fn byteLengthV(s: String) -> Int64 {
    return 42
}

fn main() -> Int64 {
    // The user's own definitions, on every line.
    print(hexDigit(1))
    print(hexVal(1))
    print(decoded("x"))
    print(ascii("x"))
    print(b64Val(1))
    print(showCps(7))
    print(byteLengthV("anything"))
    // And all four runtime modules answering in the same program — `std/text`
    // both hand-imported (`chars`) and injected (`charCount`).
    print(toJson(Point { x: 1, y: 2 }))
    print(hexEncode("Hi"))
    print(chars("é").length)
    print("é".charCount())
    print(contains("hello", "ell"))
    print(startsWith("hello", "he"))
    return 0
}
"#;
    let dir = std::env::temp_dir().join("vyrn-m4c-collide");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.vyrn");
    std::fs::write(&file, src).unwrap();
    let out = vyrn().arg("run").arg(&file).output().expect("vyrn run");
    assert!(
        out.status.success(),
        "the collision program failed to run:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let got = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    assert_eq!(
        got,
        "1001\n2001\nmine:x\nascii:x\n3001\ncps:7\n42\n\
         {\"x\":1,\"y\":2}\n4869\n1\n1\ntrue\ntrue\n"
    );
}
