//! The six codec builtins, pinned over the surface where two codecs can differ
//! (RFC-0078 M4b(1), converted by M4c).
//!
//! # Why this file changed shape
//!
//! M4b(1) wrote `std/codecs` and proved it equal to the builtins by calling BOTH
//! on every input in this corpus and reporting only disagreements — 6,354
//! comparisons, the builtin as the oracle. M4c then **routed the builtins into
//! `std/codecs`**, which makes that comparison `x == x`: the same function on both
//! sides of the `!=`, green forever, proving nothing.
//!
//! So the oracle is converted into a PIN. The corpus is unchanged and the program
//! now calls only the BUILTIN, printing one line per answer; the test asserts the
//! SHA-256 of the whole transcript against a literal. That statement — "the
//! builtins answer exactly this over 6,354 checks" — is true before the swap and
//! after it, which is the property a converted oracle needs. A digest rather than
//! a 2,000-line golden file because the corpus generator below is deterministic
//! and regenerating it is one command; the spot pins after the digest are what
//! give a failure a human-readable neighbour.
//!
//! # The two digests, and the bug the swap fixed
//!
//! Both were captured by running this corpus against the pre-swap build
//! (`494f883`, the C/Rust builtins) and the post-swap one:
//!
//! | | |
//! |---|---|
//! | pre-swap (hand-written LLVM IR + Rust) | `ad39879f2fbcb7df65ce9eb2da7145031af6fa99ccc92046ce9b2a591f926275` |
//! | post-swap (`std/codecs`) | [`CORPUS_DIGEST`] |
//!
//! They differ in **16 of 6,354 rows, and every one of the 16 is a NUL row** —
//! the two transcripts were diffed line by line and the whole diff is decoders
//! whose bytes contain `0x00` (`S00`, `S0041`, `S410010`, `S610062`, …, each
//! becoming `None`). That is RFC-0078 M4b(1)'s finding landing as a fix: `hexDecode("00")`
//! answered `Some` and did not agree with itself across engines (the interpreter
//! kept a Rust `String` holding the byte; the native path returned a `char*`
//! `__vyrn_strlen` truncated at it). `std/codecs` answers `None`, which RFC-0014
//! requires and which is identical on all three engines. Those rows are pinned
//! individually below rather than left inside the digest.
//!
//! Regenerating a digest after a deliberate change: run the test, take the
//! `actual` value out of the failure message, and diff the two transcripts (both
//! are written beside the generated program in `%TEMP%/vyrn-m4c-codecs/`).

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

/// Run one module's inline `test` blocks and assert the green count.
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

/// `std/codecs`'s own hand-picked pins, including the NUL rows. Literals, so they
/// were never an oracle and did not have to be converted.
#[test]
fn std_codecs_unit_tests_run_green() {
    unit_tests_green("std/codecs.vyrn", "4 passed, 0 failed");
}

/// The example's pins. Its `main` is what parity compares across the three engines;
/// its `test` blocks assert the same rows as literals, and nothing else in the suite
/// runs an example's `test` blocks, so without this row they are decoration.
///
/// M4c converted the example the same way it converted the corpus: it used to print
/// `mine == builtin` beside each value, which after the swap is the same function on
/// both sides. Now it calls the BUILTIN and prints the value, so `main` states "the
/// routed builtin answers this, identically on interp, native and wasm".
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
    for cp in [
        0x7Fu32, 0x80, 0x7FF, 0x800, 0xD7FF, 0xE000, 0xFFFD, 0xFFFF, 0x10000, 0x10FFFF,
    ] {
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
/// of them, and a decoder that accepts something another engine rejects is exactly
/// the failure worth catching.
fn decoder_corpus() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // Every byte as hex, in both cases: 256 valid `hexDecode` inputs, of which
    // `00` is the NUL row and 0x80..0xFF are lone bytes that are not UTF-8.
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
    // Percent escapes: truncated, non-hex, mixed with plain text, and the ones
    // whose bytes are not UTF-8 or are a NUL.
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
    // Plain text through the decoders, which only `urlDecode` accepts.
    for s in ["abc", "a b", "Hello, Vyrn!", "aZ09-_.~", "\t", " "] {
        out.push(s.to_string());
    }
    out
}

/// The transcript's fixed preamble: how one answer is rendered.
///
/// `dump` renders an `Option<String>` payload as its own hex, WITHOUT calling
/// `hexEncode` — the point of a pin is that its reporting does not depend on the
/// code it is pinning, and after M4c `hexEncode` is the code under test. `bytes`
/// is the irreducible view, so this is four lines and no builtin under question.
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

/// The whole corpus, as one program calling `std/codecs` through its imports.
/// The calls were builtins until RFC-0094 M2; the digest below is what proves
/// the move changed no answer.
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

/// The SHA-256 of the transcript, `std/codecs` answering. Captured post-swap; the
/// pre-swap value and the exact difference are in this file's header.
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

    // Spot pins first: a digest mismatch on its own is a wall of nothing, and one
    // of these naming the value it expected is usually the whole diagnosis. Keyed
    // by input rather than by corpus index so reordering the corpus cannot make
    // them silently pin a different row.
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
    // Both alphabet entries above 61 — the two a naive table gets wrong.
    assert_eq!(ends(&dec_row("////", "b64D")), "None"); // decodes to non-UTF-8
    assert_eq!(ends(&dec_row("%C3%A9", "urlD")), "Sc3a9"); // é
    assert_eq!(ends(&dec_row("QQ=", "b64D")), "None"); // length not a multiple of 4

    // The NUL rows, which are the swap's one behavioural change (RFC-0014: a
    // `String` cannot hold a NUL, so a decoder that would produce one must
    // decline). Spelled out rather than left inside the digest, because pre-swap
    // they were `Some` and did not agree across engines.
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
            "the NUL row `{input}` through {kind} must be None (RFC-0014); got {row}"
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

/// Four `std/` modules in one link, and the user's own names still win.
///
/// M2b injected exactly ONE module, for one builtin, and proved the reserved `$`
/// spellings make a collision unreachable. M4c turned that into a table.
/// RFC-0094 M2 then took `std/codecs` and `std/strpred` off it and made `chars`
/// an ordinary import of the still-injected `std/text`, so this program now
/// reaches all four modules by three different routes at once:
///
/// 1. **`std/json` by injection** (`toJson` desugars into it), renamed to `json$`.
/// 2. **`std/text` injected AND hand-imported** — `s.charCount()` injects it and
///    `import { chars }` names it, which is the case the rename map has to get
///    right in both directions.
/// 3. **`std/codecs` and `std/strpred` by plain import.**
///
/// The property under test is the same either way: `std/codecs` declares
/// `hexDigit`, `hexVal`, `decoded`, `ascii`, `b64Val` privately, `std/text`
/// exports `showCps` and `std/strpred` exports `byteLengthV`, and a user program
/// declaring any of those must keep its own. Injection does that with the `$`
/// prefix; a plain import does it with name-privacy renaming (RFC-0046 §3). Both
/// are unconditional, so this is a regression pin rather than a hope.
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
