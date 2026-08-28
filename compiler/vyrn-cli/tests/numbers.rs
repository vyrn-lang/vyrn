//! The number tier's pins (RFC-0078 M4a).
//!
//! `examples/numbytes.vyrn` captures every numeric conversion the language has —
//! text -> `Int64` including each way it declines, `%f`'s six places on the values
//! that tell an exact formatter from a plausible one, the saturating float ->
//! integer conversions and the wrapping narrowings — as literals in `test` blocks,
//! BEFORE any of it moves out of Rust and C and into Vyrn.
//!
//! Parity already runs the example's `main`, so interp == native == wasm is
//! covered without this file. What is not covered is the `test` blocks: nothing
//! else in the suite runs an example's, so without this row the pins are
//! decoration.

use std::path::{Path, PathBuf};
use std::process::Command;

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

#[test]
fn number_conversion_pins_hold() {
    unit_tests_green("examples/numbytes.vyrn", "7 passed, 0 failed");
}

#[test]
fn std_num_unit_tests_run_green() {
    unit_tests_green("std/num.vyrn", "8 passed, 0 failed");
}

/// `std/num`'s `f64Str` against Rust's own `{:.6}`, byte for byte, over bit
/// patterns rather than literals (RFC-0081 M1).
///
/// The inverse of the test below it, and it needs a stronger oracle for the same
/// reason: `%f`'s six places are the exact decimal value of a binary fraction,
/// which is up to 1074 digits long, and every implementation that computes them
/// in floating point is wrong somewhere. The three that exist agree because
/// someone made them agree.
///
/// The corpus is bit patterns, so it reaches values no literal in the language
/// can name — every exponent, both zeros, subnormals at the bottom of the range,
/// and the three non-finite spellings — and the comparison is against the
/// interpreter's own formatter, which IS `{:.6}`. The parity suite runs the same
/// comparison on native and wasm, where the other two implementations live.
#[test]
fn f64str_is_byte_identical_to_rusts_own_formatter() {
    let mut corpus: Vec<u64> = vec![
        0.0f64,
        -0.0,
        1.0,
        -1.0,
        1.5,
        -1.5,
        0.1,
        0.2,
        0.3,
        2.0,
        10.0,
        100.0,
        // The two exact ties at the sixth place: half-to-even keeps the even
        // digit in one and rounds the odd one up.
        0.0078125,
        0.0234375,
        0.5,
        0.05,
        0.005,
        0.0005,
        0.00005,
        0.000005,
        0.0000005,
        0.00000005,
        // A carry that runs out of the top of the number.
        0.9999999,
        -0.9999999,
        0.99999949999,
        9.9999995,
        1e300,
        1e-300,
        1e22,
        1e23,
        1e100,
        123456789.123456789,
        3.141592653589793,
        2.718281828459045,
        9007199254740992.0,
        9007199254740993.0,
        4503599627370495.5,
        f64::MAX,
        f64::MIN,
        f64::MIN_POSITIVE,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        f64::EPSILON,
        2.2250738585072011e-308,
    ]
    .into_iter()
    .map(f64::to_bits)
    .collect();
    // The bottom of the subnormal range and the top of the finite one, by bits —
    // there is no literal for either end.
    corpus.extend([
        1u64,
        2,
        3,
        0x000F_FFFF_FFFF_FFFF,
        0x0010_0000_0000_0000,
        0x7FEF_FFFF_FFFF_FFFF,
    ]);
    corpus.extend([
        0x8000_0000_0000_0001u64,
        0xFFF0_0000_0000_0000,
        0xFFF8_0000_0000_0000,
    ]);

    // Deterministic pseudorandom, a fixed LCG so a failure reproduces. Whole
    // random bit patterns reach every exponent including the extremes; the
    // scaled half concentrates on the range programs actually print, which is
    // where a rounding bug would be seen rather than merely present.
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        seed
    };
    for _ in 0..400 {
        corpus.push(next());
    }
    for _ in 0..400 {
        let m = (next() >> 11) as f64 / (1u64 << 53) as f64;
        let e = (next() % 61) as i32 - 30;
        corpus.push((m * 10f64.powi(e)).to_bits());
    }

    let calls: String = corpus
        .iter()
        .map(|b| format!("    print(f64Str(floatFromBits({b})))\n"))
        .collect();
    let src = format!(
        "import {{ f64Str }} from \"std/num\"\nfn main() -> Int64 {{\n{calls}    return 0\n}}\n"
    );
    let dir = std::env::temp_dir().join("vyrn-m1-f64str");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("f64str.vyrn");
    std::fs::write(&file, &src).unwrap();
    let out = vyrn().arg("run").arg(&file).output().expect("vyrn run");
    assert!(
        out.status.success(),
        "differential program failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let got: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim_end().to_string())
        .collect();
    assert_eq!(got.len(), corpus.len(), "one line per input");

    let mut bad: Vec<String> = Vec::new();
    for (bits, mine) in corpus.iter().zip(&got) {
        let x = f64::from_bits(*bits);
        let oracle = format!("{x:.6}");
        if *mine != oracle {
            bad.push(format!("{bits} ({x:e}): f64Str {mine}, Rust {oracle}"));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} disagree:\n{}",
        bad.len(),
        corpus.len(),
        bad.join("\n")
    );
}

/// `std/num`'s `parseFloat64` against Rust's own `str::parse::<f64>()`, bit for
/// bit, over a corpus (RFC-0078 M4a).
///
/// This is the test that decides whether "correctly rounded" is a claim or a
/// fact. The module's own `test` blocks pin about a dozen values chosen by hand,
/// which proves the hard cases someone thought of; a rounding bug lives in the
/// cases nobody thought of, and the only oracle worth comparing against is an
/// implementation already known to be correctly rounded.
///
/// The corpus is the union of two things. The hand-picked half is the standard
/// list of values that break naive parsers — the exact ties at `2^53`, the two
/// famous denial-of-service literals near `2^-1022` that hung PHP and Java, the
/// largest finite double and the first value past it, both ends of the subnormal
/// range, and a 900-digit significand that exercises the truncation flag. The
/// generated half is deterministic pseudorandom (a fixed LCG, so a failure
/// reproduces): random digit strings at random exponents, which is where a carry
/// or an off-by-one in the scaling shows up.
///
/// Non-numbers are in the corpus too, because a parser that accepts `"1 "` or
/// `"1e"` is wrong in a way `assertEq` on a value never catches.
#[test]
fn parsefloat64_is_bit_identical_to_rusts_own_parser() {
    let mut corpus: Vec<String> = vec![
        "0",
        "-0",
        "0.0",
        "1",
        "-1",
        "1.5",
        "-1.5",
        "0.1",
        "0.2",
        "0.3",
        "1e0",
        "1E0",
        "+1.5",
        "1e3",
        "1e-3",
        "-2.5E-1",
        "123456789.123456789",
        "3.141592653589793",
        "2.718281828459045",
        // The exact ties at 2^53, where half-to-even decides the last bit.
        "9007199254740992",
        "9007199254740993",
        "9007199254740994",
        "9007199254740995",
        "9007199254740996",
        // The two literals that hung a production runtime, on both sides.
        "2.2250738585072011e-308",
        "2.2250738585072012e-308",
        "2.2250738585072014e-308",
        // Both ends of the subnormal range, and half of the smallest one — a tie
        // that must round DOWN to zero rather than up to something.
        "5e-324",
        "4.9406564584124654e-324",
        "2.4703282292062327e-324",
        "2.4703282292062328e-324",
        "1e-323",
        "2.2250738585072009e-308",
        // The largest finite double, the first value past it, and beyond.
        "1.7976931348623157e308",
        "1.7976931348623158e308",
        "1.8e308",
        "1e309",
        "1e400",
        "-1e400",
        "1e-400",
        "1e-1000",
        // A rounding carry OUT of the top mantissa bit — the class RFC-0078 M3's
        // decode corpus found this oracle missing. Every one of these rounds up to
        // an exact power of two, so the mantissa leaves the doubling loop at 2^53
        // and the encoding has to raise the exponent rather than OR a bit into it.
        // `|` is idempotent, so the bug showed only for an ODD biased exponent:
        // `1.9999999999999999999` was `1.0` and `9223372036854775807` was 2^62,
        // while `0.9999999999999999999` and `3.9999999999999999999` were right.
        "9223372036854775807",
        "9223372036854775808",
        "0.9999999999999999999",
        "1.9999999999999999999",
        "3.9999999999999999999",
        "7.9999999999999999999",
        "0.49999999999999999999",
        "0.24999999999999999999",
        "18446744073709551615",
        "4611686018427387903",
        "1.4999999999999999999",
        "2.9999999999999999999",
        // Powers of ten, which are exact in decimal and not in binary.
        "1e22",
        "1e23",
        "1e-22",
        "1e-23",
        "1e100",
        "-1e100",
        "1e-100",
        // Values that a fast path computed in floating point gets wrong.
        "8.98846567431158e307",
        "7.8459735791271921e65",
        "3.5844466002796428e298",
        "9.194366959071701e-91",
        "7.4e47",
        "5.9e-8",
        // Forms the scanner has to accept, and edges of its own grammar.
        "000123",
        "0.000000000000000000001",
        "123000000000000000000000",
        ".5",
        "5.",
        "-.5",
        "1e+3",
        "1e-0",
        "0e999999999",
        "-0e-99",
        // Refusals.
        "",
        "-",
        "+",
        ".",
        "abc",
        "1 ",
        " 1",
        "1x",
        "1e",
        "1e+",
        "1.2.3",
        "--1",
        "1_000",
        "NaN",
        "inf",
        "0x10",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();

    // A 900-digit significand: past the 800-digit cap, so the truncation flag is
    // what decides its rounding. `1` then 898 zeros then `1` is the shape where
    // dropping the tail silently would produce an exact power of two instead.
    corpus.push(format!("1.{}1e-5", "0".repeat(898)));
    corpus.push(format!("{}5", "9".repeat(400)));

    // Deterministic pseudorandom: a fixed LCG so a failure is reproducible.
    let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = |m: u64| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) % m
    };
    for _ in 0..220 {
        let ndig = 1 + next(19) as usize;
        let digits: String = (0..ndig).map(|_| (b'0' + next(10) as u8) as char).collect();
        let exp = next(61) as i64 - 30;
        corpus.push(format!(
            "{}{}e{}",
            if next(2) == 0 { "-" } else { "" },
            digits,
            exp
        ));
    }

    let calls: String = corpus
        .iter()
        .map(|s| {
            format!(
                "    print(match parseFloat64(\"{}\") {{ Some(v) => floatBits(v).toString(), None => \"None\" }})\n",
                s.replace('\\', "\\\\").replace('"', "\\\"")
            )
        })
        .collect();
    let src = format!(
        "import {{ parseFloat64 }} from \"std/num\"\nfn main() -> Int64 {{\n{calls}    return 0\n}}\n"
    );

    let dir = std::env::temp_dir().join("vyrn-m4a-strtod");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("strtod.vyrn");
    std::fs::write(&file, &src).unwrap();
    let out = vyrn().arg("run").arg(&file).output().expect("vyrn run");
    assert!(
        out.status.success(),
        "differential program failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let got: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim_end().to_string())
        .collect();
    assert_eq!(got.len(), corpus.len(), "one line per input");

    let mut bad: Vec<String> = Vec::new();
    for (input, mine) in corpus.iter().zip(&got) {
        // Rust's parser accepts `inf`/`NaN` and a few spellings `std/num`
        // deliberately does not, so the oracle is "a finite decimal literal Rust
        // reads the same way", and the rest must be refused by both.
        let oracle = match input.parse::<f64>() {
            Ok(v) if v.is_finite() || input.contains(|c: char| c.is_ascii_digit()) => {
                v.to_bits().to_string()
            }
            _ => "None".to_string(),
        };
        if *mine != oracle {
            bad.push(format!("{input:?}: std/num {mine}, Rust {oracle}"));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} disagree:\n{}",
        bad.len(),
        corpus.len(),
        bad.join("\n")
    );
}
