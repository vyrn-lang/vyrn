//! Pins the numeric conversions: text to `Int64`, `%f`, saturating
//! float to integer, wrapping narrowings, and `std/num`'s formatter and parser
//! against Rust's.

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

/// Nothing else runs an example's `test` blocks, so without this row the pins in
/// `examples/numbytes.vyrn` are decoration.
#[test]
fn number_conversion_pins_hold() {
    unit_tests_green("examples/numbytes.vyrn", "8 passed, 0 failed");
}

/// `%f`'s six places are the exact decimal value of a binary fraction, up to
/// 1074 digits, and any implementation in floating point is wrong somewhere. The
/// corpus is bit patterns, so it reaches values no literal can name.
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
        // Exact ties at the sixth place: half-to-even keeps the even digit and
        // rounds the odd one up.
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
    // The bottom of the subnormal range and the top of the finite one, by bits.
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

    // A fixed LCG, so a failure reproduces. Random bit patterns reach every
    // exponent; the scaled half concentrates on the range programs print.
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

/// The oracle is an implementation known to be correctly rounded. The corpus is
/// the standard list of values that break naive parsers, random digit strings at
/// random exponents, and non-numbers such as `"1 "` a parser must refuse.
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
        // The two literals that hung PHP and Java, on both sides.
        "2.2250738585072011e-308",
        "2.2250738585072012e-308",
        "2.2250738585072014e-308",
        // Both ends of the subnormal range, and half of the smallest one: a tie
        // that must round down to zero.
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
        // A rounding carry out of the top mantissa bit: each rounds up to a power
        // of two, so the encoding must raise the exponent rather than OR a bit
        // into it. An OR fails only for an odd biased exponent, hence both parities.
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

    // Past the 800-digit cap, so the truncation flag decides the rounding;
    // dropping the tail silently would produce an exact power of ten instead.
    corpus.push(format!("1.{}1e-5", "0".repeat(898)));
    corpus.push(format!("{}5", "9".repeat(400)));

    // A fixed LCG, so a failure reproduces.
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
        // Rust accepts `inf` and `NaN`, which `std/num` refuses on purpose.
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
