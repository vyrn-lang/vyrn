//! The language's own rules, run on the compiled route (RFC-0125 §3 M5).
//!
//! These were `interp.rs`'s unit tests. Each one is a small program and the
//! answer it must give, and the tree-walker was what ran them; the tree-walker
//! is gone, so they run through `vyrn build --target wasm`'s backend in the
//! driver's WASI host instead — the route `vyrn run` takes. Not one assertion
//! moved: what a program answers is a fact about Vyrn and not about an engine,
//! which is the whole reason they could be carried over rather than deleted.
//!
//! **What did NOT come over** is every test that reached into the tree-walker's
//! own machinery — a `Val`, a `Frame`, an `Interp` — because there is nothing
//! left for those to be about. The count is in RFC-0125 §3 M5's record.
//!
//! An INTEGRATION test for the reason `loader_run.rs` states: running anything
//! needs `vyrn-codegen` and the driver's host, and a unit test inside this crate
//! that reaches for them compiles a second copy of this crate.

use std::collections::HashMap;

mod common;
use common::run_compiled;

/// The whole standard library, behind a `std` root. A builtin the tree-walker
/// answered in Rust is a CALL on this route — `std/runtime` is where the body is
/// — and the loader injects the modules a program's builtins imply, so the
/// resolver has to be able to answer for any of them. Listed rather than walked
/// because `include_str!` takes a literal.
const STD: &[(&str, &str)] = &[
    ("std/args.vyrn", include_str!("../../../std/args.vyrn")),
    ("std/arrays.vyrn", include_str!("../../../std/arrays.vyrn")),
    ("std/bench.vyrn", include_str!("../../../std/bench.vyrn")),
    ("std/cli.vyrn", include_str!("../../../std/cli.vyrn")),
    ("std/codecs.vyrn", include_str!("../../../std/codecs.vyrn")),
    (
        "std/connect.vyrn",
        include_str!("../../../std/connect.vyrn"),
    ),
    (
        "std/contract.vyrn",
        include_str!("../../../std/contract.vyrn"),
    ),
    ("std/diag.vyrn", include_str!("../../../std/diag.vyrn")),
    (
        "std/fallible.vyrn",
        include_str!("../../../std/fallible.vyrn"),
    ),
    (
        "std/graphql.vyrn",
        include_str!("../../../std/graphql.vyrn"),
    ),
    ("std/hash.vyrn", include_str!("../../../std/hash.vyrn")),
    ("std/hints.vyrn", include_str!("../../../std/hints.vyrn")),
    ("std/html.vyrn", include_str!("../../../std/html.vyrn")),
    ("std/http.vyrn", include_str!("../../../std/http.vyrn")),
    ("std/i18n.vyrn", include_str!("../../../std/i18n.vyrn")),
    ("std/icons.vyrn", include_str!("../../../std/icons.vyrn")),
    ("std/json.vyrn", include_str!("../../../std/json.vyrn")),
    ("std/json5.vyrn", include_str!("../../../std/json5.vyrn")),
    (
        "std/jsondec.vyrn",
        include_str!("../../../std/jsondec.vyrn"),
    ),
    (
        "std/jsonread.vyrn",
        include_str!("../../../std/jsonread.vyrn"),
    ),
    ("std/math.vyrn", include_str!("../../../std/math.vyrn")),
    ("std/mem.vyrn", include_str!("../../../std/mem.vyrn")),
    ("std/num.vyrn", include_str!("../../../std/num.vyrn")),
    (
        "std/openapi.vyrn",
        include_str!("../../../std/openapi.vyrn"),
    ),
    ("std/random.vyrn", include_str!("../../../std/random.vyrn")),
    ("std/regex.vyrn", include_str!("../../../std/regex.vyrn")),
    ("std/rpc.vyrn", include_str!("../../../std/rpc.vyrn")),
    (
        "std/runtime.vyrn",
        include_str!("../../../std/runtime.vyrn"),
    ),
    ("std/scan.vyrn", include_str!("../../../std/scan.vyrn")),
    ("std/slots.vyrn", include_str!("../../../std/slots.vyrn")),
    (
        "std/storage.vyrn",
        include_str!("../../../std/storage.vyrn"),
    ),
    ("std/stream.vyrn", include_str!("../../../std/stream.vyrn")),
    (
        "std/strings.vyrn",
        include_str!("../../../std/strings.vyrn"),
    ),
    (
        "std/strpred.vyrn",
        include_str!("../../../std/strpred.vyrn"),
    ),
    (
        "std/symbolmap.vyrn",
        include_str!("../../../std/symbolmap.vyrn"),
    ),
    ("std/text.vyrn", include_str!("../../../std/text.vyrn")),
    ("std/time.vyrn", include_str!("../../../std/time.vyrn")),
    ("std/tw.vyrn", include_str!("../../../std/tw.vyrn")),
    ("std/ui.vyrn", include_str!("../../../std/ui.vyrn")),
    ("std/von.vyrn", include_str!("../../../std/von.vyrn")),
    (
        "std/vyx-hints.vyrn",
        include_str!("../../../std/vyx-hints.vyrn"),
    ),
    ("std/vyx.vyrn", include_str!("../../../std/vyx.vyrn")),
];

/// Load `source` as a one-file program with `std/` behind it, check it, and run
/// it — the two lines `vyrn run` is, minus the filesystem.
///
/// **`main`'s value comes back through stdout, not through the exit code**, and
/// that is the one thing about these tests that had to change. The tree-walker
/// answered an `i64`; a process answers a BYTE, so `Ok(702)` came back as
/// `Ok(190)` and `Ok(-1)` as `Ok(255)`. So the program is wrapped: the test's
/// own `main` is renamed and a new one prints its answer. A trap still traps
/// before the print, so a trapping program is still an `Err` with the trap's
/// wording, which is the other half of the signature.
fn run(source: &str) -> Result<i64, String> {
    vyrn_genwasm::install();
    vyrn_lower::install();
    scratch();
    let wrapped = format!(
        "{}
fn main() -> Int64 {{
    print(vyrnTestMain().toString())
    return 0
}}
",
        source.replace("fn main()", "fn vyrnTestMain()")
    );
    let files: HashMap<String, String> = STD
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some("std".into()),
        ..Default::default()
    };
    // `load_warned`, not `loader::load`: it is what the CLI calls, and the
    // difference is SYNTHESIS — a validated type's constructor and a JSON codec
    // are generated there. The tree-walker validated and encoded in Rust and
    // needed neither.
    let (loaded, _warnings) = vyrn_frontend::load_warned(
        &wrapped,
        "main.vyrn",
        &opts,
        &vyrn_frontend::loader::MapResolver(files),
    );
    let program = loaded.map_err(|ds| {
        ds.iter().map(|d| d.render()).collect::<Vec<_>>().join(
            "
",
        )
    })?;
    let bytes = vyrn_codegen::direct::compile(&program)?;
    let out = vyrn_cli::wasmrun::run(
        &bytes,
        vyrn_cli::wasmrun::Run {
            argv: vec!["main.vyrn".to_string()],
            stdin_prefix: Vec::new(),
            capture_stdout: true,
            capture_stderr: true,
            meter: false,
        },
    )?;
    let said = String::from_utf8_lossy(&out.stderr);
    if let Some(at) = said.rfind("error: ") {
        if at == 0 || said.as_bytes()[at - 1] == 10 {
            return Err(said[at + 7..].trim_end().to_string());
        }
    }
    eprint!("{said}");
    let printed = String::from_utf8_lossy(&out.stdout);
    let last = printed
        .lines()
        .last()
        .ok_or_else(|| "the program printed nothing".to_string())?;
    last.trim()
        .parse::<i64>()
        .map_err(|e| format!("`main` answered {last:?}, which is not an Int64: {e}"))
}

/// The same program with one module missing from the standard library, for the
/// one row that is about a check living in a module.
///
/// Not an EMPTY resolver: `std/runtime` holds every builtin's body on this
/// route, so a program with no std root at all is refused for fifty names
/// before the interesting one. Taking away exactly the module under test is
/// what makes the refusal name it.
fn run_without(missing: &str, source: &str) -> Result<i64, String> {
    vyrn_genwasm::install();
    vyrn_lower::install();
    scratch();
    let files: HashMap<String, String> = STD
        .iter()
        .filter(|(k, _)| *k != missing)
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some("std".into()),
        ..Default::default()
    };
    let (loaded, _warnings) = vyrn_frontend::load_warned(
        source,
        "main.vyrn",
        &opts,
        &vyrn_frontend::loader::MapResolver(files),
    );
    let program = loaded.map_err(|ds| {
        ds.iter().map(|d| d.render()).collect::<Vec<_>>().join(
            "
",
        )
    })?;
    run_compiled(&program)
}

/// The same thing. It had its own resolver in `interp.rs` because that one
/// listed the JSON closure by hand; this one has the whole library behind it.
fn run_json(source: &str) -> Result<i64, String> {
    run(source)
}

/// The one directory this binary's guests can see, and the process's working
/// directory from the first test that runs.
///
/// WASI gives a module ONE preopened directory and it is the host's working
/// directory (`wasmrun`), so a guest cannot open an absolute path the way the
/// tree-walker could — it called `std::fs` directly. The scratch directory is
/// therefore made the working directory once, before any guest runs, and the
/// file rows below name their files relatively.
fn scratch() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("vyrn-semantics-{}", std::process::id()));
        std::fs::create_dir_all(&d).expect("scratch directory");
        std::env::set_current_dir(&d).expect("scratch is the working directory");
        d
    })
}

/// A scratch file for the RFC-0014 / RFC-0044 rows, named for the test so two of
/// them cannot collide. Relative, because that is what the guest can open.
fn temp_path(tag: &str) -> String {
    scratch();
    format!("vyrn-io-test-{tag}.txt")
}

#[test]
fn arithmetic_and_return() {
    assert_eq!(run("fn main() -> Int64 { return 2 + 3 * 4; }").unwrap(), 14);
}

// ---- payload enums / Result round-trip (RFC-0024) -------------------

/// `fromJson(T, toJson(x)) == Valid(x)` over the new domain: a payload enum
/// (single/tuple/nullary) and a `Result`, both nested through a record, an
/// array, and an `Option`. Returns 0 only when every arm round-trips.
#[test]
fn payload_codec_round_trip_law() {
    let src = "type Shape = | Circle(Int64) | Rect(Int64, Int64) | Nothing \
                   type Box = { s: Shape, r: Result<Int64, String>, \
                                tags: Array<Shape>, opt: Option<Shape> } \
                   fn cmp(enc: String, b: Box) -> Int64 { \
                       if toJson(b) == enc { return 0 } \
                       return 1 \
                   } \
                   fn same(a: Box) -> Int64 { \
                       let enc = toJson(a) \
                       return match fromJson(Box, enc) { \
                           Valid(b) => cmp(enc, b), \
                           Invalid(is) => 2, \
                       } \
                   } \
                   fn main() -> Int64 { \
                       let ok = Box { s: Rect(3, 4), r: Ok(9), \
                                      tags: [Circle(1), Nothing], opt: Some(Rect(2, 2)) } \
                       let err = Box { s: Nothing, r: Err(\"boom\"), tags: [], opt: None } \
                       return same(ok) + same(err) \
                   }";
    assert_eq!(run_json(src).unwrap(), 0);
}

#[test]
fn write_then_read_file_roundtrip() {
    let path = temp_path("roundtrip");
    let src = format!(
        "fn main() -> Int64 {{ \
                 let w = writeFile(\"{path}\", \"alpha\\nbeta\") \
                 let ok = match w {{ Ok(b) => b, Err(e) => false }} \
                 if ok == false {{ return 1 }} \
                 let r = readFile(\"{path}\") \
                 return match r {{ \
                     Ok(s) => s.byteLength, \
                     Err(e) => 2, \
                 }} }}"
    );
    // "alpha\nbeta" is 10 bytes.
    assert_eq!(run(&src).unwrap(), 10);
    let _ = std::fs::remove_file(path.as_str());
}

#[test]
fn read_file_missing_yields_canonical_err() {
    let src = "fn main() -> Int64 { \
                       let r = readFile(\"vyrn-io-test-definitely-missing.txt\") \
                       let msg = match r { Ok(s) => s, Err(e) => e } \
                       if msg == \"cannot read `vyrn-io-test-definitely-missing.txt`\" { \
                           return 1 } \
                       return 0 }";
    assert_eq!(run(src).unwrap(), 1);
}

// ---- crash-safe persistence (RFC-0044) --------------------------------

/// `renameFile` atomically overwrites an existing target and consumes the
/// source — after it, the target holds the new content and no source (or
/// `.tmp`) remains.
#[test]
fn rfc0044_rename_file_over_existing_replaces() {
    let dst = temp_path("rn-dst");
    let src_path = temp_path("rn-src");
    std::fs::write(&dst, "OLDOLD").unwrap();
    std::fs::write(&src_path, "NEW").unwrap();
    let src = format!(
        "fn main() -> Int64 {{ \
                 return match renameFile(\"{src_path}\", \"{dst}\") {{ \
                     Ok(b) => 1, Err(e) => 0 }} }}"
    );
    assert_eq!(run(&src).unwrap(), 1);
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "NEW");
    assert!(!std::path::Path::new(&src_path).exists());
    let _ = std::fs::remove_file(&dst);
}

/// The atomic-write algorithm (`writeAtomic` = `std/storage`): write `<path>.tmp`
/// then rename it over `path`. A successful write replaces the target and
/// leaves NO `.tmp` sibling. Uses the same body the std module ships.
#[test]
fn rfc0044_write_atomic_replaces_and_leaves_no_tmp() {
    let path = temp_path("wa-ok");
    std::fs::write(path.as_str(), "OLD").unwrap();
    let src = format!(
        "fn writeAtomic(path: String, content: String) -> Result<Bool, String> {{ \
                 let tmp = \"\\{{path}}.tmp\" \
                 return match writeFile(tmp, content) {{ \
                     Ok(d) => renameFile(tmp, path), Err(w) => Err(w) }} }} \
             fn main() -> Int64 {{ \
                 return match writeAtomic(\"{path}\", \"BRANDNEW\") {{ \
                     Ok(b) => 1, Err(e) => 0 }} }}"
    );
    assert_eq!(run(&src).unwrap(), 1);
    assert_eq!(std::fs::read_to_string(path.as_str()).unwrap(), "BRANDNEW");
    assert!(!std::path::Path::new(&format!("{path}.tmp")).exists());
    let _ = std::fs::remove_file(path.as_str());
}

/// THE atomicity proof: when the temp write FAILS, `writeAtomic` never touches
/// `path`, so the original target is byte-for-byte unchanged (the tear a bare
/// `writeFile` would cause is gone). The temp write is forced to fail by making
/// `<path>.tmp` a directory — `writeFile` cannot open it as a file.
#[test]
fn rfc0044_write_atomic_failed_temp_leaves_target_unchanged() {
    let path = temp_path("wa-tear");
    let tmp = format!("{path}.tmp");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::write(path.as_str(), "ORIGINAL").unwrap();
    std::fs::create_dir(&tmp).unwrap(); // writeFile("<path>.tmp") now fails
    let src = format!(
        "fn writeAtomic(path: String, content: String) -> Result<Bool, String> {{ \
                 let tmp = \"\\{{path}}.tmp\" \
                 return match writeFile(tmp, content) {{ \
                     Ok(d) => renameFile(tmp, path), Err(w) => Err(w) }} }} \
             fn main() -> Int64 {{ \
                 return match writeAtomic(\"{path}\", \"CLOBBERED\") {{ \
                     Ok(b) => 1, Err(e) => 0 }} }}"
    );
    // The write reports failure...
    assert_eq!(run(&src).unwrap(), 0);
    // ...and — the point — the original target is untouched.
    assert_eq!(std::fs::read_to_string(path.as_str()).unwrap(), "ORIGINAL");
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_file(path.as_str());
}

/// `load(TypeName, path)` distinguishes the three honest outcomes: a missing
/// file is `Missing`, a garbage file is `Corrupt`, a good file is `Loaded`.
/// Encoded as 1 / 2 / 100+value so one run proves all three.
#[test]
fn rfc0044_load_three_outcomes() {
    let good = temp_path("ld-good");
    let bad = temp_path("ld-bad");
    std::fs::write(&good, "{\"n\": 41}").unwrap();
    std::fs::write(&bad, "{ not json ]").unwrap();
    let missing = temp_path("ld-missing");
    let _ = std::fs::remove_file(&missing);
    let outcome = |p: &str| {
        format!(
            "match load(Rec, \"{p}\") {{ \
                     Missing => 1, Corrupt(iss) => 2, Loaded(r) => 100 + r.n }}"
        )
    };
    let src = format!(
        "type Rec = {{ n: Int64 }} \
             fn main() -> Int64 {{ \
                 let m = {} \
                 let c = {} \
                 let g = {} \
                 return m * 1000000 + c * 1000 + g }}",
        outcome(&missing),
        outcome(&bad),
        outcome(&good),
    );
    // Missing=1, Corrupt=2, Loaded(41)=141.
    assert_eq!(run_json(&src).unwrap(), 1_002_141);
    let _ = std::fs::remove_file(&good);
    let _ = std::fs::remove_file(&bad);
}

/// `loadOr(TypeName, path, default)` returns the default for a missing OR a
/// corrupt file, and the decoded value for a good one.
#[test]
fn rfc0044_load_or_defaults() {
    let good = temp_path("lo-good");
    let bad = temp_path("lo-bad");
    let missing = temp_path("lo-missing");
    std::fs::write(&good, "{\"n\": 7}").unwrap();
    std::fs::write(&bad, "nonsense").unwrap();
    let _ = std::fs::remove_file(&missing);
    let src = format!(
        "type Rec = {{ n: Int64 }} \
             fn main() -> Int64 {{ \
                 let d = Rec {{ n: 9 }} \
                 let g = loadOr(Rec, \"{good}\", d).n \
                 let m = loadOr(Rec, \"{missing}\", d).n \
                 let c = loadOr(Rec, \"{bad}\", d).n \
                 return g * 100 + m * 10 + c }}"
    );
    // good=7, missing=9(default), corrupt=9(default) -> 799.
    assert_eq!(run_json(&src).unwrap(), 799);
    let _ = std::fs::remove_file(&good);
    let _ = std::fs::remove_file(&bad);
}

/// `fsyncFile` succeeds on an existing file and errors (canonically) on a
/// missing one — the durability step never silently no-ops a bad path.
#[test]
fn rfc0044_fsync_file_ok_and_missing() {
    let path = temp_path("fs-ok");
    std::fs::write(path.as_str(), "durable").unwrap();
    let src = format!(
        "fn main() -> Int64 {{ \
                 let a = match fsyncFile(\"{path}\") {{ Ok(b) => 1, Err(e) => 0 }} \
                 let b = match fsyncFile(\"{path}-nope\") {{ Ok(x) => 0, Err(e) => 1 }} \
                 return a * 10 + b }}"
    );
    assert_eq!(run(&src).unwrap(), 11);
    let _ = std::fs::remove_file(path.as_str());
}

#[test]
fn read_file_rejects_invalid_utf8_and_nul_canonically() {
    let bad = temp_path("badutf8");
    let nul = temp_path("nul");
    std::fs::write(&bad, [0x63u8, 0xE9, 0x21]).unwrap();
    std::fs::write(&nul, [0x61u8, 0x00, 0x62]).unwrap();
    let src = format!(
        "fn msgOf(r: Result<String, String>) -> String {{ \
                 return match r {{ Ok(s) => \"ok\", Err(e) => e.copy() }} }} \
             fn main() -> Int64 {{ \
                 let a = msgOf(readFile(\"{bad}\")) \
                 let b = msgOf(readFile(\"{nul}\")) \
                 if a != \"`{bad}` is not valid UTF-8\" {{ return 1 }} \
                 if b != \"`{nul}` contains a NUL byte\" {{ return 2 }} \
                 return 0 }}"
    );
    assert_eq!(run(&src).unwrap(), 0);
    let _ = std::fs::remove_file(&bad);
    let _ = std::fs::remove_file(&nul);
}

/// RFC-0125 §3 M6 (the third judgment's fifth slice): `stringFromBytes` is
/// checked by `std/text`'s `stringFault`, so a program built with no std root
/// cannot make a `String` from bytes and says which module is missing.
///
/// This replaced three tests that ran the builtin here — the RFC-0014 M2
/// roundtrip law and the two refusals. They needed the module the interpreter
/// now calls, so their answers are pinned where a std root exists:
/// `tests/boundaries/string-nul.vyrn` and `string-utf8.vyrn` for the two
/// wordings under all three engines, and `tests/text.rs` for the roundtrip
/// over the whole codepoint corpus.
#[test]
fn string_from_bytes_names_the_module_its_check_lives_in() {
    let src = "fn main() -> Int64 { let b: Array<UInt8> = [104, 105] \
                   return match stringFromBytes(b) { Ok(s) => 1, Err(e) => 0 } }";
    let e = run_without("std/text.vyrn", src).unwrap_err();
    // The wording is the BACKEND's now, and it names the same two things:
    // the call, and the module its check is written in. The loader said it
    // before, because the tree-walker asked the loader for the body.
    assert!(e.contains("stringFromBytes"), "{e}");
    assert!(e.contains("std/text"), "{e}");
}

#[test]
fn read_file_bytes_reads_binary() {
    let path = temp_path("binary");
    std::fs::write(path.as_str(), [0u8, 1, 2, 0xFF, 0]).unwrap();
    let src = format!(
        "fn main() -> Int64 {{ \
                 return match readFileBytes(\"{path}\") {{ \
                     Ok(b) => b.length, \
                     Err(e) => -1, \
                 }} }}"
    );
    // Binary read: NUL and invalid-UTF-8 bytes are fine, all 5 come back.
    assert_eq!(run(&src).unwrap(), 5);
    let _ = std::fs::remove_file(path.as_str());
}

#[test]
fn args_default_to_empty() {
    // `run` (no args) must present an empty argv[1..] — the parity harness
    // runs every example argument-less on all three backends.
    assert_eq!(
        run("fn main() -> Int64 { return args().length }").unwrap(),
        0
    );
}

#[test]
fn functions_and_recursion() {
    let src = "
            fn fib(n: Int64) -> Int64 {
                if n < 2 { return n; }
                return fib(n - 1) + fib(n - 2);
            }
            fn main() -> Int64 { return fib(10); }
        ";
    assert_eq!(run(src).unwrap(), 55);
}

#[test]
fn option_and_match() {
    let src = "
            fn sd(a: Int64, b: Int64) -> Option<Int64> {
                if b == 0 { return None; }
                return Some(a / b);
            }
            fn uw(o: Option<Int64>, f: Int64) -> Int64 {
                return match o { Some(x) => x, None => f };
            }
            fn main() -> Int64 { return uw(sd(10, 2), 0) + uw(sd(1, 0), 100); }
        ";
    assert_eq!(run(src).unwrap(), 105); // 5 + 100
}

#[test]
fn result_and_question_mark() {
    // `?` propagates Err out of `chain`, so chain(0) returns Err(-1) and the
    // final match yields the fallback.
    let src = "
            fn checked(n: Int64) -> Result<Int64, Int64> {
                if n == 0 { return Err(0 - 1); }
                return Ok(n);
            }
            fn chain(n: Int64) -> Result<Int64, Int64> {
                let x = checked(n)?;      // early-returns Err when n == 0
                return Ok(x + 1);
            }
            fn main() -> Int64 {
                let a = match chain(5) { Ok(v) => v, Err(e) => e };   // 6
                let b = match chain(0) { Ok(v) => v, Err(e) => e };   // -1
                return a + b;             // 5
            }
        ";
    assert_eq!(run(src).unwrap(), 5);
}

#[test]
fn str_and_parse_roundtrip() {
    let src = "fn main() -> Int64 { \
                       let s = (0 - 123).toString(); \
                       return match parse(s) { Some(n) => n, None => 0 }; }";
    assert_eq!(run(src).unwrap(), -123);
}

#[test]
fn parse_rejects_non_integers() {
    let cases = [
        ("\"12x\"", -1),
        ("\"\"", -1),
        ("\"-\"", -1),
        ("\" 5\"", -1),
        ("\"42\"", 42),
    ];
    for (lit, want) in cases {
        let src = format!(
            "fn main() -> Int64 {{ return match parse({lit}) {{ Some(n) => n, None => 0 - 1 }}; }}"
        );
        assert_eq!(run(&src).unwrap(), want, "parse({lit})");
    }
}

#[test]
fn result_holds_non_int_payloads() {
    // Ok carries an Array, Err carries a String — neither rides in the word.
    let src = "
            fn lookup(k: Int64) -> Result<Array<Int64>, String> {
                if k == 0 { return Err(\"nope\"); }
                let mut a: Array<Int64> = []; a.push(k * 10); return Ok(a);
            }
            fn main() -> Int64 {
                let a = match lookup(5) { Ok(r) => r[0], Err(e) => 0 - e.byteLength };
                let b = match lookup(0) { Ok(r) => r[0], Err(e) => 0 - e.byteLength };
                return a + b;  // 50 + (-4)
            }
        ";
    assert_eq!(run(src).unwrap(), 46);
}

#[test]
fn fixed_array_literal_and_index() {
    let src = "fn main() -> Int64 { let a: Array<Int64, 4> = [10, 20, 30, 40]; \
                   let mut s = 0; let mut i = 0; \
                   while i < a.length { s = s + a[i]; i = i + 1; } return s; }";
    assert_eq!(run(src).unwrap(), 100);
}

#[test]
fn fixed_array_out_of_bounds_errors() {
    let src = "fn main() -> Int64 { let a: Array<Int64, 2> = [1, 2]; return a[4]; }";
    assert!(run(src).unwrap_err().contains("out of bounds"));
}

#[test]
fn growable_array_push_and_read() {
    let src = "fn main() -> Int64 { \
                       let mut a: Array<Int64> = []; \
                       let mut i = 0; \
                       while i < 6 { a.push(i * i); i = i + 1; } \
                       let mut s = 0; let mut j = 0; \
                       while j < a.length { s = s + a[j]; j = j + 1; } \
                       return s; }"; // 0+1+4+9+16+25 = 55
    assert_eq!(run(src).unwrap(), 55);
}

#[test]
fn array_index_out_of_bounds_errors() {
    let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
                   a.push(1); return a[3]; }";
    assert!(run(src).unwrap_err().contains("out of bounds"));
}

#[test]
fn for_over_fixed_array() {
    let src = "fn main() -> Int64 { let a: Array<Int64, 5> = [0, 1, 4, 9, 16]; \
                   let mut s = 0; for x in a { s = s + x; } return s; }";
    assert_eq!(run(src).unwrap(), 30);
}

#[test]
fn for_over_growable_array() {
    let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
                   let mut i = 0; while i < 6 { a.push(i * i); i = i + 1; } \
                   let mut s = 0; for x in a { s = s + x; } return s; }"; // 0+1+4+9+16+25
    assert_eq!(run(src).unwrap(), 55);
}

#[test]
fn for_over_empty_array_runs_zero_times() {
    let src = "fn main() -> Int64 { let a: Array<Int64> = []; \
                   let mut s = 7; for x in a { s = s + x; } return s; }";
    assert_eq!(run(src).unwrap(), 7);
}

#[test]
fn for_loop_variable_is_scoped_to_body() {
    // `x` must not leak past the loop — referencing it after is unbound.
    let src = "fn main() -> Int64 { let a: Array<Int64, 2> = [1, 2]; \
                   for x in a { let y = x; } return x; }";
    assert!(run(src).is_err());
}

#[test]
fn for_body_early_return() {
    // Returning from inside the loop stops iteration immediately.
    let src = "fn firstOver(a: Array<Int64, 4>, t: Int64) -> Int64 { \
                   for x in a { if x > t { return x; } } return 0 - 1; } \
                   fn main() -> Int64 { let a: Array<Int64, 4> = [3, 8, 1, 9]; \
                   return firstOver(a, 5); }"; // first element > 5 is 8
    assert_eq!(run(src).unwrap(), 8);
}

#[test]
fn for_over_non_array_is_rejected() {
    let src = "fn main() -> Int64 { let n = 3; for x in n { } return 0; }";
    assert!(run(src).unwrap_err().contains("Array"));
}

#[test]
fn method_index_and_length_surface() {
    // `[]` is a literal, `.length` is a field, and `.push` / `[i]` desugar
    // to the internal `@push` / `@at`. There is no other spelling.
    let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
                   a.push(10); a.push(20); a.push(30); \
                   return a.length + a[0] + a[2]; }"; // 3 + 10 + 30
    assert_eq!(run(src).unwrap(), 43);
}

#[test]
fn method_push_writes_back() {
    // `a.push(x);` as a statement mutates `a` in place (write-back).
    let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
                   let mut i = 0; while i < 5 { a.push(i); i = i + 1; } \
                   let mut s = 0; for x in a { s = s + x; } return s; }"; // 0+1+2+3+4
    assert_eq!(run(src).unwrap(), 10);
}

#[test]
fn method_tally_writes_back_in_place() {
    // `m.tally(k, n);` as a statement takes the in-place fast path: distinct
    // keys accumulate AND a repeated key adds, through the same slot.
    let src = "fn main() -> Int64 { let mut m: Map<String, Int64> = [:]; \
                   m.tally(\"a\", 2); m.tally(\"b\", 5); m.tally(\"a\", 3); \
                   let n = match m[\"a\"] { Some(v) => v, None => 0 }; \
                   return m.length * 100 + n; }"; // 2 keys, a=5
    assert_eq!(run(src).unwrap(), 205);
}

#[test]
fn method_tally_bytes_writes_back_in_place() {
    // The byte-keyed twin through the same fast path, with the buffer reused
    // between calls exactly as a counting loop reuses it.
    let src = "fn main() -> Int64 { let mut m: Map<String, Int64> = [:]; \
                   let mut w: Array<UInt8> = []; w.push(65); \
                   m.tallyBytes(w, 1); m.tallyBytes(w, 4); \
                   w[0] = 66; m.tallyBytes(w, 7); \
                   let n = match m[\"A\"] { Some(v) => v, None => 0 }; \
                   return m.length * 100 + n; }"; // keys A,B; A=5
    assert_eq!(run(src).unwrap(), 205);
}

#[test]
fn method_append_writes_back_and_self_append_uses_pre_state() {
    // `xs.append(ys);` extends in place; `xs.append(xs)` appends the
    // PRE-state elements (the compiled backends define it the same way).
    let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2]; \
                   let b: Array<Int64> = [3]; a.append(b); a.append(a); \
                   return a.length * 100 + a[3] + a[5]; }"; // [1,2,3,1,2,3]: 600+1+3
    assert_eq!(run(src).unwrap(), 604);
}

// `drop` consumes, and a use after it is refused — by the kernel now
// rather than by this crate's own check (RFC-0125 §3 M3, row 06). The
// assertion moved to `vyrn-cli`'s `refusals` suite, which runs the whole
// compiler; `vyrn_frontend::run` is the frontend alone and no longer states it.

#[test]
fn drop_of_non_heap_is_rejected() {
    let src = "fn main() -> Int64 { let n = 5; drop n; return 0; }";
    assert!(run(src).unwrap_err().contains("heap"));
}

#[test]
fn string_interpolation_renders_scalars() {
    // `\{ }` holes render Int/Bool/String; literal braces are untouched. The
    // program returns the interpolated string's length so we can assert it.
    let src = "fn main() -> Int64 { let n = 42; let ok = true; \
                   let s = \"n=\\{n} ok=\\{ok} {lit}\"; return s.byteLength; }";
    // "n=42 ok=true {lit}" -> 18 characters
    assert_eq!(run(src).unwrap(), 18);
}

#[test]
fn interpolation_evaluates_hole_expressions() {
    let src = "fn main() -> Int64 { let a = 3; let b = 4; \
                   let s = \"\\{a * b}\"; return s.byteLength; }"; // "12" -> len 2
    assert_eq!(run(src).unwrap(), 2);
}

#[test]
fn str_renders_bool_and_string() {
    let src = "fn main() -> Int64 { let s = false.toString(); return s.byteLength; }"; // "false" -> 5
    assert_eq!(run(src).unwrap(), 5);
}

#[test]
fn str_renders_sized_int() {
    // A signed Int32 renders by value; an unsigned UInt8 renders its magnitude.
    let s = "fn main() -> Int64 { let a: Int32 = 42; let b: UInt8 = 200; \
                 let s = \"\\{a}/\\{b + b}\"; return s.byteLength; }"; // "42/144" -> 6
    assert_eq!(run(s).unwrap(), 6);
}

#[test]
fn str_renders_uint64_above_i64_max() {
    // The full 64-bit magnitude renders (not a signed reinterpretation).
    let s = "fn main() -> Int64 { let n: UInt64 = 10000000000000000000; \
                 let s = n.toString(); return s.byteLength; }"; // 20 digits
    assert_eq!(run(s).unwrap(), 20);
}

#[test]
fn str_renders_float_to_six_decimals() {
    let s = "fn main() -> Int64 { let s = (3.14159).toString(); return s.byteLength; }"; // "3.141590" -> 8
    assert_eq!(run(s).unwrap(), 8);
}

#[test]
fn float_arithmetic_and_comparison() {
    // 1.5 * 2.5 = 3.75 > 3.0 → 1
    let src = "fn main() -> Int64 { let a = 1.5; let b = 2.5; \
                   if a * b > 3.0 { return 1; } return 0; }";
    assert_eq!(run(src).unwrap(), 1);
}

#[test]
fn float_through_function_and_negation() {
    let src = "fn half(x: Float64) -> Float64 { return x / 2.0; } \
                   fn main() -> Int64 { let h = half(5.0); \
                   if h == 2.5 { if -h < 0.0 { return 7; } } return 0; }";
    assert_eq!(run(src).unwrap(), 7);
}

#[test]
fn float_to_int_truncates_toward_zero() {
    let src = "fn main() -> Int64 { let f = 3.9; return Int64(f); }";
    assert_eq!(run(src).unwrap(), 3);
}

#[test]
fn int_to_float_and_back() {
    let src = "fn main() -> Int64 { let f = Float64(7); let g = f + 0.5; return Int64(g); }"; // 7.5 -> 7
    assert_eq!(run(src).unwrap(), 7);
}

#[test]
fn float32_rounds_to_single_precision() {
    // 2^24 + 1 is exact in f64 but rounds to 2^24 in f32, so `Int(..)` differs.
    let f32 = "fn main() -> Int64 { let x: Float32 = 16777217.0; return Int64(x); }";
    assert_eq!(run(f32).unwrap(), 16777216);
    let f64 = "fn main() -> Int64 { let x: Float64 = 16777217.0; return Int64(x); }";
    assert_eq!(run(f64).unwrap(), 16777217);
}

#[test]
fn float32_arithmetic_stays_single_precision() {
    // Adding 1.0 to 1e8 is below the f32 ULP → lost; f64 keeps it.
    let src = "fn addf(a: Float32, b: Float32) -> Float32 { return a + b; } \
                   fn main() -> Int64 { let g: Float32 = 100000000.0; return Int64(addf(g, 1.0)); }";
    assert_eq!(run(src).unwrap(), 100000000);
}

#[test]
fn float32_widens_to_float64_exactly() {
    // 0.5 is exact in both; Float32 -> Float64 -> Int round-trips its value.
    let src = "fn main() -> Int64 { let x: Float32 = 2.5; let d = Float64(x); \
                   if d == 2.5 { return 1; } return 0; }";
    assert_eq!(run(src).unwrap(), 1);
}

#[test]
fn float32_literal_adapts_to_sibling() {
    // A plain float literal takes the Float32 sibling's precision.
    let src = "fn main() -> Int64 { let h: Float32 = 1.5; let r = h + 2.5; return Int64(r); }";
    assert_eq!(run(src).unwrap(), 4);
}

#[test]
fn int_to_int32_wraps_and_back() {
    // 5_000_000_000 wraps into i32 to 705032704; Int(..) sext's it back.
    let src = "fn main() -> Int64 { let big = 5000000000; return Int64(Int32(big)); }";
    assert_eq!(run(src).unwrap(), 705032704);
}

#[test]
fn int8_conversion_wraps() {
    let src = "fn main() -> Int64 { return Int64(Int8(300)); }"; // 300 & 0xFF as i8 = 44
    assert_eq!(run(src).unwrap(), 44);
}

#[test]
fn rejects_conversion_of_non_number() {
    let src = "fn main() -> Int64 { let x = Int64(\"hi\"); return 0; }";
    assert!(run(src).unwrap_err().contains("converts a number"));
}

#[test]
fn int64_is_an_alias_for_int() {
    let src = "fn f(n: Int64) -> Int64 { return n + 1; } \
                   fn main() -> Int64 { let x: Int64 = 41; return f(x); }";
    assert_eq!(run(src).unwrap(), 42);
}

#[test]
fn rejects_int_float_mixing() {
    let src = "fn main() -> Int64 { let a = 1 + 2.0; return 0; }";
    assert!(run(src).unwrap_err().contains("matching numeric"));
}

#[test]
fn rejects_float_assigned_to_int() {
    let src = "fn main() -> Int64 { let x: Int64 = 1.5; return x; }";
    assert!(run(src).is_err());
}

#[test]
fn int32_overflow_wraps() {
    // 2e9 + 2e9 = 4e9 wraps at 32 bits to -294967296.
    let src = "fn main() -> Int64 { let a: Int32 = 2000000000; let b: Int32 = 2000000000; \
                   let c = a + b; if c < 0 { return 1; } return 0; }";
    assert_eq!(run(src).unwrap(), 1);
}

#[test]
fn int8_wraps_at_eight_bits() {
    // 100 + 100 = 200 wraps at 8 bits (signed) to -56.
    let src = "fn wrap(a: Int8, b: Int8) -> Int8 { return a + b; } \
                   fn main() -> Int64 { let x: Int8 = 100; let r = wrap(x, x); \
                   if r < 0 { return 1; } return 0; }";
    assert_eq!(run(src).unwrap(), 1);
}

#[test]
fn uint8_wraps_into_magnitude_range() {
    // 200 + 200 = 400 wraps at 8 bits (unsigned) to 144 — stays non-negative.
    let src = "fn main() -> Int64 { let x: UInt8 = 200; let r = x + x; return Int64(r); }";
    assert_eq!(run(src).unwrap(), 144);
}

#[test]
fn uint8_subtraction_wraps_below_zero() {
    // 200 - 250 = -50 wraps to 206 in unsigned 8-bit space.
    let src = "fn main() -> Int64 { let x: UInt8 = 200; let r = x - 250; return Int64(r); }";
    assert_eq!(run(src).unwrap(), 206);
}

#[test]
fn uint_uses_unsigned_division() {
    // A UInt64 above i64::MAX divides unsigned (signed sdiv would give a
    // different, negative-influenced quotient).
    let src = "fn main() -> Int64 { let n: UInt64 = 10000000000000000000; \
                   let q = n / 3; if q == 3333333333333333333 { return 1; } return 0; }";
    assert_eq!(run(src).unwrap(), 1);
}

#[test]
fn uint_comparison_is_unsigned() {
    // As unsigned, 10e18 (>i64::MAX, stored as a negative i64) is GREATER
    // than 5 — a signed comparison would wrongly rank it below.
    let src = "fn main() -> Int64 { let big: UInt64 = 10000000000000000000; \
                   let small: UInt64 = 5; if big > small { return 1; } return 0; }";
    assert_eq!(run(src).unwrap(), 1);
}

#[test]
fn uint32_holds_value_above_int32_max() {
    // 4_000_000_000 overflows Int32 but fits UInt32.
    let src = "fn main() -> Int64 { return Int64(UInt32(Int64(4000000000))); }";
    assert_eq!(run(src).unwrap(), 4000000000);
}

#[test]
fn sized_int_no_overflow_is_normal() {
    let src = "fn main() -> Int64 { let a: Int32 = 5; let b = a * 3; \
                   if b == 15 { return 1; } return 0; }";
    assert_eq!(run(src).unwrap(), 1);
}

#[test]
fn rejects_mixing_different_int_widths() {
    let src = "fn main() -> Int64 { let a: Int32 = 1; let b: Int8 = 2; let c = a + b; return 0; }";
    assert!(run(src).unwrap_err().contains("matching numeric"));
}

#[test]
fn tagged_template_passes_parts_and_boxed_values() {
    // A `sql` tag receives literal parts + boxed values; the structure comes
    // only from parts (here we return $N per hole and check the length).
    let src = "fn sql(parts: Array<String>, values: Array<Value>) -> Int64 { \
                       return parts.length + values.length; } \
                   fn main() -> Int64 { let a = 1; let b = 2; \
                       return sql\"x\\{a}y\\{b}z\"; }"; // parts=3, values=2 -> 5
    assert_eq!(run(src).unwrap(), 5);
}

#[test]
fn tagged_template_values_are_matchable_and_typed() {
    // The boxed values decode back to their original scalars via `match`.
    let src = "fn sql(parts: Array<String>, values: Array<Value>) -> Int64 { \
                       return match values[0] { IntVal(n) => n, BoolVal(b) => 0, StrVal(s) => s.byteLength }; } \
                   fn main() -> Int64 { let x = 41; return sql\"n=\\{x}\"; }";
    assert_eq!(run(src).unwrap(), 41);
}

#[test]
fn schema_of_extracts_where_bounds() {
    // `schemaOf(Port)` reads the `where` predicate at compile time.
    let src = "type Port = Int64 where value >= 1 && value <= 65535; \
                   fn optOr(o: Option<Int64>, d: Int64) -> Int64 { \
                       return match o { Some(n) => n, None => d }; } \
                   fn main() -> Int64 { let s = schemaOf(Port); \
                       return optOr(s.min, 0) + optOr(s.max, 0); }"; // 1 + 65535
    assert_eq!(run(src).unwrap(), 65536);
}

/// The enriched `Schema`: name, base spelling (incl. sized ints), `///`
/// doc, `multipleOf`, string length bounds, and the regex pattern.
#[test]
fn schema_of_enriched_fields() {
    let src = "/// A lowercase handle.\n\
                   type Username = String where value.byteLength >= 3 && value.byteLength <= 16 && value =~ \"[a-z]+\"\n\
                   type Even = Int64 where value % 2 == 0\n\
                   type Byte = UInt8\n\
                   fn optOr(o: Option<Int64>, d: Int64) -> Int64 {\n\
                       return match o { Some(n) => n, None => d }\n\
                   }\n\
                   fn main() -> Int64 {\n\
                       let u = schemaOf(Username)\n\
                       let e = schemaOf(Even)\n\
                       let b = schemaOf(Byte)\n\
                       let mut n = 0\n\
                       if u.name == \"Username\" { n = n + 1 }\n\
                       if u.base == \"String\" { n = n + 1 }\n\
                       if optOr(u.minLength, 0) == 3 { n = n + 1 }\n\
                       if optOr(u.maxLength, 0) == 16 { n = n + 1 }\n\
                       if match u.pattern { Some(p) => p == \"[a-z]+\", None => false } { n = n + 1 }\n\
                       if match u.doc { Some(d) => true, None => false } { n = n + 1 }\n\
                       if optOr(e.multipleOf, 0) == 2 { n = n + 1 }\n\
                       if b.base == \"UInt8\" { n = n + 1 }\n\
                       if match b.doc { Some(d) => false, None => true } { n = n + 1 }\n\
                       return n\n\
                   }";
    assert_eq!(run(src).unwrap(), 9);
}

#[test]
fn schema_of_unbounded_type_has_no_bounds() {
    let src = "type Id = Int64; \
                   fn none(o: Option<Int64>) -> Int64 { return match o { Some(n) => 1, None => 0 }; } \
                   fn main() -> Int64 { let s = schemaOf(Id); return none(s.min) + none(s.max); }";
    assert_eq!(run(src).unwrap(), 0); // both None
}

#[test]
fn schema_of_rejects_a_non_type() {
    let src = "fn main() -> Int64 { let x = 5; let s = schemaOf(x); return 0; }";
    assert!(run(src).unwrap_err().contains("not a type"));
}

#[test]
fn string_length_field() {
    let src = "fn main() -> Int64 { let s = \"hello\"; return s.byteLength; }";
    assert_eq!(run(src).unwrap(), 5);
}

#[test]
fn string_ordering_is_bytewise_lexicographic() {
    // RFC-0022: `< <= > >=` on Strings, byte order (not collation). Each
    // returns 1 when the ordering holds. Covers prefixes, empties, equality,
    // and a multibyte case where byte order puts "é" (0xC3..) after "z" (0x7A).
    let cases: &[(&str, i64)] = &[
        ("\"ab\" < \"b\"", 1),   // 'a' < 'b'
        ("\"a\" < \"ab\"", 1),   // shorter prefix sorts first
        ("\"ab\" < \"ab\"", 0),  // equal: strictly-less is false
        ("\"ab\" <= \"ab\"", 1), // equal: <= holds
        ("\"b\" > \"ab\"", 1),
        ("\"\" < \"a\"", 1), // empty precedes anything
        ("\"\" <= \"\"", 1),
        ("\"z\" < \"\u{e9}\"", 1), // 0x7A < 0xC3 (leading UTF-8 byte)
        ("\"\u{e9}\" > \"z\"", 1),
    ];
    for (expr, want) in cases {
        let src = format!("fn main() -> Int64 {{ if {expr} {{ return 1 }} return 0 }}");
        assert_eq!(run(&src).unwrap(), *want, "for `{expr}`");
    }
}

#[test]
fn string_indexing_and_char_literal() {
    // `s[1]` is the byte 'e' (101) as a `UInt8` (RFC-0022) — `Int64(..)`
    // widens it for an Int64 return; a char literal adapts to the byte.
    let src = "fn main() -> Int64 { let s = \"hello\"; return Int64(s[1]); }";
    assert_eq!(run(src).unwrap(), 101);
    let cmp = "fn main() -> Int64 { let s = \"hello\"; if s[0] == 'h' { return 1; } return 0; }";
    assert_eq!(run(cmp).unwrap(), 1);
}

#[test]
fn string_index_out_of_bounds_traps() {
    let src = "fn main() -> Int64 { let s = \"hi\"; return Int64(s[5]); }";
    assert!(run(src).unwrap_err().contains("out of bounds"));
}

#[test]
fn unicode_bytes_vs_code_points() {
    // "café": 5 UTF-8 bytes but 4 code points; `é` is U+00E9 = 233.
    let bytes = "fn main() -> Int64 { return bytes(\"caf\\u{e9}\").length; }";
    assert_eq!(run(bytes).unwrap(), 5);
    // `chars` is `std/text`'s declaration since RFC-0094 M2, so it needs the
    // loader path AND an import. `bytes` and `byteLength` are the views that
    // stayed, so `run` still serves them.
    let imp = "import { chars } from \"std/text\" ";
    let chars = format!("{imp}fn main() -> Int64 {{ return chars(\"caf\\u{{e9}}\").length; }}");
    assert_eq!(run_json(&chars).unwrap(), 4);
    let cp = format!("{imp}fn main() -> Int64 {{ return chars(\"caf\\u{{e9}}\")[3]; }}");
    assert_eq!(run_json(&cp).unwrap(), 233);
}

#[test]
fn code_point_iteration_and_emoji() {
    // A 4-byte emoji is a single code point.
    let len = "fn main() -> Int64 { return \"\\u{1F600}\".byteLength; }"; // 4 bytes
    assert_eq!(run(len).unwrap(), 4);
    let imp = "import { chars } from \"std/text\" ";
    let one = format!("{imp}fn main() -> Int64 {{ return chars(\"\\u{{1F600}}\").length; }}");
    assert_eq!(run_json(&one).unwrap(), 1); // 1 char
    let val = format!("{imp}fn main() -> Int64 {{ return chars(\"\\u{{1F600}}\")[0]; }}");
    assert_eq!(run_json(&val).unwrap(), 128512);
}

#[test]
fn byte_literal_is_its_byte_value() {
    // A byte literal (RFC-0057) evaluates to its byte, as an integer value.
    assert_eq!(run("fn main() -> Int64 { return 'a' }").unwrap(), 97);
    assert_eq!(run("fn main() -> Int64 { return '{' }").unwrap(), 123);
    assert_eq!(run("fn main() -> Int64 { return '\\n' }").unwrap(), 10);
    assert_eq!(run("fn main() -> Int64 { return '\\xff' }").unwrap(), 255);
    // It coerces against a byte from `bytes(..)` (both `UInt8`).
    assert_eq!(
        run("fn main() -> Int64 { if bytes(\"{\")[0] == '{' { return 1 } return 0 }").unwrap(),
        1
    );
}

/// The six codecs, end to end (checker + loader + interpreter). RFC-0078 M4c
/// routed them into `std/codecs` and RFC-0094 M2 made them ordinary imports,
/// so what is worth asserting is unchanged and the import line is the only
/// difference: a round trip, and the three refusals the deleted Rust helper
/// tests covered.
#[test]
fn the_codecs_answer_through_std_codecs() {
    let src = "import { base64Decode, base64Encode, hexDecode, hexEncode, urlDecode, urlEncode } from \"std/codecs\"                    fn main() -> Int64 {                    let d = base64Decode(base64Encode(\"hey\"))                    if match d { Some(s) => s, None => \"\" } != \"hey\" { return 1 }                    if hexEncode(\"Hi\") != \"4869\" { return 2 }                    if urlEncode(\"a b&c\") != \"a%20b%26c\" { return 3 }                    if match hexDecode(\"zz\") { Some(s) => 1, None => 0 } != 0 { return 4 }                    if match base64Decode(\"bad\") { Some(s) => 1, None => 0 } != 0 { return 5 }                    if match urlDecode(\"%ZZ\") { Some(s) => 1, None => 0 } != 0 { return 6 }                    return 0 }";
    assert_eq!(run_json(src).unwrap(), 0);
}

/// The seam M2b named, as RFC-0094 M2 leaves it: a bare source with no
/// resolver has no `std/codecs` in the link, so the name does not resolve at
/// all and the diagnostic says where it lives rather than that it is missing.
#[test]
fn a_moved_builtin_without_a_std_root_names_its_module() {
    let e = run("fn main() -> Int64 { return hexEncode(\"hi\").byteLength }").unwrap_err();
    assert!(e.contains("`hexEncode` is `std/codecs`'s"), "{e}");
}

/// `save` desugars to `writeAtomic` (RFC-0044), so a module that never imported
/// the primitive used to be told "call to unknown function `writeAtomic`" about
/// a call it did not write. The sentence names the spelling the reader DID
/// write, and then the import that fixes it.
#[test]
fn the_save_sugar_names_itself_when_its_primitive_is_missing() {
    let e = run("type C = { n: Int64 }\nfn main() -> Int64 { save(\"c\", C { n: 1 }) return 0 }")
        .unwrap_err();
    assert!(e.contains("`save(path, value)` writes through it"), "{e}");
    assert!(
        e.contains("add `import { writeAtomic } from \"std/storage\"`"),
        "{e}"
    );
}

#[test]
fn string_iteration_sums_bytes() {
    // 'a'(97) + 'b'(98) + 'c'(99) = 294.
    let src = "fn main() -> Int64 { let s = \"abc\"; let mut t = 0; \
                   for c in s { t = t + c; } return t; }";
    assert_eq!(run(src).unwrap(), 294);
}

#[test]
fn string_predicate_methods() {
    // `std/strpred` exports since RFC-0094 M2, so this goes through the loader
    // and carries an import — `run` has no resolver and no module to link.
    let imp = "import { contains, endsWith, startsWith } from \"std/strpred\" ";
    let c = format!(
        "{imp}fn main() -> Int64 {{ if contains(\"hello\", \"ell\") {{ return 1 }} return 0 }}"
    );
    assert_eq!(run_json(&c).unwrap(), 1);
    let s = format!(
        "{imp}fn main() -> Int64 {{ if startsWith(\"hello\", \"he\") {{ return 1 }} return 0 }}"
    );
    assert_eq!(run_json(&s).unwrap(), 1);
    let e = format!(
        "{imp}fn main() -> Int64 {{ if endsWith(\"hello\", \"lo\") {{ return 1 }} return 0 }}"
    );
    assert_eq!(run_json(&e).unwrap(), 1);
    // `endsWith` guards against a suffix longer than the string.
    let g = format!(
        "{imp}fn main() -> Int64 {{ if endsWith(\"hi\", \"ahoy\") {{ return 1 }} return 0 }}"
    );
    assert_eq!(run_json(&g).unwrap(), 0);
}

#[test]
fn indexing_in_refinement_predicate() {
    let ok = "type G = String where value.byteLength >= 1 && value[0] == 'H'; \
                  fn mk(s: consume String) -> G { return G(s); } \
                  fn main() -> Int64 { let g = mk(\"Hi\"); return g.byteLength; }";
    assert_eq!(run(ok).unwrap(), 2);
    // A provably-wrong constant is rejected at compile time (via consteval).
    let bad = "type G = String where value.byteLength >= 1 && value[0] == 'H'; \
                   fn main() -> Int64 { let g = G(\"bye\"); return 0; }";
    assert!(run(bad).unwrap_err().contains("does not satisfy `G`"));
}

#[test]
fn validated_string_accepts_valid_value() {
    let src = "type Name = String where value.byteLength >= 3; \
                   fn mk(s: consume String) -> Name { return Name(s); } \
                   fn main() -> Int64 { let n = mk(\"bob\"); return n.byteLength; }";
    assert_eq!(run(src).unwrap(), 3);
}

#[test]
fn validated_string_traps_on_too_short() {
    // Runtime construction of an invalid string aborts (matches native exit 1).
    let src = "type Name = String where value.byteLength >= 3; \
                   fn mk(s: consume String) -> Name { return Name(s); } \
                   fn main() -> Int64 { let n = mk(\"x\"); return 0; }";
    assert!(run(src)
        .unwrap_err()
        .contains("validation failed for `Name`"));
}

#[test]
fn proven_interpolation_runs_correctly() {
    // RFC-0020 M1: a statically-proven interpolation flows into TransKey and
    // runs identically (the interp validation is a no-op on a proven value).
    let src = "type TransKey = String where value =~ \"nav\\\\.(home|about)\\\\.label\"\n\
                   type Section = String where value =~ \"home|about\"\n\
                   fn t(key: TransKey) -> Int64 { return key.byteLength }\n\
                   fn main() -> Int64 { let s: Section = \"home\"  return t(\"nav.\\{s}.label\") }";
    // "nav.home.label" is 14 bytes.
    assert_eq!(run(src).unwrap(), 14);
}

#[test]
fn nonfinite_hole_interpolation_traps_at_runtime() {
    // A plain-String hole is not finite, so no static proof — an invalid
    // value produced at runtime traps through the canonical message (the
    // interp counterpart of the codegen runtime-validation test).
    let src = "type TransKey = String where value =~ \"nav\\\\.(home|about)\\\\.label\"\n\
                   fn build(x: String) -> Int64 { let k: TransKey = \"nav.\\{x}.label\"  return 0 }\n\
                   fn main() -> Int64 { return build(\"BAD\") }";
    assert!(run(src)
        .unwrap_err()
        .contains("validation failed for `TransKey`"));
}

#[test]
fn cross_field_record_valid_and_invalid() {
    let ok = "type R = { a: Int64, b: Int64 } where a < b; \
                  fn mk(x: Int64, y: Int64) -> R { return R { a: x, b: y }; } \
                  fn main() -> Int64 { let r = mk(1, 2); return r.b; }";
    assert_eq!(run(ok).unwrap(), 2);
    let bad = "type R = { a: Int64, b: Int64 } where a < b; \
                   fn mk(x: Int64, y: Int64) -> R { return R { a: x, b: y }; } \
                   fn main() -> Int64 { let r = mk(5, 1); return 0; }";
    assert!(run(bad).unwrap_err().contains("violates its `where`"));
}

#[test]
fn validation_trap_message_is_canonical() {
    let src = "type Age = Int64 where value >= 18; \
                   fn mk(n: Int64) -> Age { return Age(n); } \
                   fn main() -> Int64 { let a = mk(5); return 0; }";
    assert_eq!(run(src).unwrap_err(), "validation failed for `Age`");
}

/// The ANNOTATION's own boundary, with a value the checker cannot prove.
///
/// Its own test rather than a case of the boundary sweep below, because that
/// sweep's second assertion is a store into a binding of the same type: the two
/// failed together, and a case behind a failing assertion witnesses nothing.
///
/// `let mut a: Age = 20` is a constant the checker proves, so it emits no check
/// and says nothing about this boundary. `core::Builder` names a `let` by the
/// type of its VALUE, so the emitter's whole-body screen read `Int64` where the
/// reader wrote `Age`, took a body whose rows state no check, and this returned
/// 5.
#[test]
fn an_annotated_let_validates_a_value_the_checker_cannot_prove() {
    let src = "type Age = Int64 where value >= 18 \
                   fn main() -> Int64 { let mut x = 30 x = x - 25 \
                   let a: Age = x return a }";
    assert_eq!(run(src).unwrap_err(), "validation failed for `Age`");
}

#[test]
fn auto_validation_traps_dynamic_violations_at_each_boundary() {
    // Argument boundary.
    let arg = "type Age = Int64 where value >= 18 \
                   fn g(a: Age) -> Int64 { return a } \
                   fn main() -> Int64 { let mut x = 30 x = x - 25 return g(x) }";
    assert_eq!(run(arg).unwrap_err(), "validation failed for `Age`");
    // Assignment boundary (the binding's declared type is remembered).
    let assign = "type Age = Int64 where value >= 18 \
                      fn main() -> Int64 { let mut a: Age = 20 a = a - 15 return a }";
    assert_eq!(run(assign).unwrap_err(), "validation failed for `Age`");
    // Return boundary (a raw match join validates on the way out).
    let ret = "type Age = Int64 where value >= 18 \
                   fn pick(o: Option<Int64>) -> Age { \
                       return match o { Some(x) => x, None => 18 } } \
                   fn main() -> Int64 { return pick(Some(5)) }";
    assert_eq!(run(ret).unwrap_err(), "validation failed for `Age`");
    // Record-field boundary.
    let field = "type Age = Int64 where value >= 18 \
                     type User = { age: Age } \
                     fn mk(n: Int64) -> User { return User { age: n } } \
                     fn main() -> Int64 { let u = mk(5) return 0 }";
    assert_eq!(run(field).unwrap_err(), "validation failed for `Age`");
    // Cross-field record coercion (structural value into a predicated type).
    let xf = "type Range = { start: Int64, end: Int64 } where start < end \
                  type Plain = { start: Int64, end: Int64 } \
                  fn span(r: Range) -> Int64 { return r.end - r.start } \
                  fn mk(a: Int64, b: Int64) -> Plain { return Plain { start: a, end: b } } \
                  fn main() -> Int64 { return span(mk(9, 3)) }";
    assert_eq!(
        run(xf).unwrap_err(),
        "validation failed: `Range` violates its `where` clause"
    );
}

/// The boundary that was NOT validated: a field STORE (RFC-0082 M3).
///
/// `Stmt::SetField` never coerced, so every spelling that writes through a
/// record field let a runtime value into a validated element type while both
/// compiled backends trapped — the one hole in "a Vyrn program cannot even
/// spell a value that failed its own predicate". A literal is folded by
/// `consteval` at compile time on all three engines, which is why only a
/// runtime value reaches it and why nothing said for so long.
#[test]
fn a_field_store_validates_like_every_other_boundary() {
    let head = "type Age = Int64 where value >= 18 \
                    type T = { xs: Array<Age> } \
                    fn rt(n: Int64) -> Int64 { return n - 1 } ";
    // `t.xs.push(v)` — the in-place append fast path.
    let push = format!(
        "{head} fn main() -> Int64 {{ let mut t = T {{ xs: [] }} \
             t.xs.push(rt(6)) return t.xs[0] }}"
    );
    assert_eq!(run(&push).unwrap_err(), "validation failed for `Age`");
    // The same, through a `modify` parameter rather than the local.
    let param = format!(
        "{head} fn add(t: modify T) {{ t.xs.push(rt(6)) }} \
             fn main() -> Int64 {{ let mut t = T {{ xs: [] }} add(t) return t.xs[0] }}"
    );
    assert_eq!(run(&param).unwrap_err(), "validation failed for `Age`");
    // And through module state, whose slot type is inferred from the
    // initializer for exactly this reason.
    let global = format!(
        "{head} let mut g = T {{ xs: [] }} \
             fn main() -> Int64 {{ g.xs.push(rt(6)) return g.xs[0] }}"
    );
    assert_eq!(run(&global).unwrap_err(), "validation failed for `Age`");
    // Valid values still flow through all three.
    let ok = format!(
        "{head} let mut g = T {{ xs: [] }} \
             fn add(t: modify T) {{ t.xs.push(rt(21)) }} \
             fn main() -> Int64 {{ let mut t = T {{ xs: [] }} add(t) g.xs.push(rt(31)) \
             return t.xs[0] + g.xs[0] }}"
    );
    assert_eq!(run(&ok).unwrap(), 50);
}

#[test]
fn inline_field_refinements_validate_like_named_types() {
    // Zod/ArkType-style inline `where` on fields: valid values flow through…
    let ok = "type User = { name: String where value.byteLength >= 3, \
                                age: Int64 where value >= 18 } \
                  fn mk(n: Int64) -> User { return User { name: \"ada\", age: n } } \
                  fn main() -> Int64 { let u = mk(33) return u.age }";
    assert_eq!(run(ok).unwrap(), 33);
    // …a dynamic violation traps with the synthetic field-type name…
    let bad = "type User = { age: Int64 where value >= 18 } \
                   fn mk(n: Int64) -> User { return User { age: n } } \
                   fn main() -> Int64 { let u = mk(5) return 0 }";
    assert_eq!(run(bad).unwrap_err(), "validation failed for `User.age`");
    // …and a provably-bad constant is rejected at compile time.
    let constant = "type User = { age: Int64 where value >= 18 } \
                        fn main() -> Int64 { let u = User { age: 5 } return 0 }";
    assert!(run(constant)
        .unwrap_err()
        .contains("does not satisfy `User.age`"));
}

#[test]
fn auto_validation_passes_valid_dynamic_values() {
    let src = "type Age = Int64 where value >= 18 \
                   fn g(a: Age) -> Int64 { return a } \
                   fn main() -> Int64 { \
                       let a: Age = 25 \
                       let mut m: Age = 21 \
                       m = m + 1 \
                       let xs: Array<Age, 2> = [19, 20] \
                       return g(a) + m + xs[1] }";
    assert_eq!(run(src).unwrap(), 25 + 22 + 20);
}

#[test]
fn float_refined_type_constructs_and_rejects_at_runtime() {
    // Refinements over a Float base run under the runtime evaluator (this
    // used to fail for even VALID values — ConstVal had no Float).
    let ok = "type Ratio = Float64 where value > 0.0 && value <= 1.0; \
                  fn mk(x: Float64) -> Ratio { return Ratio(x); } \
                  fn main() -> Int64 { let r = mk(0.5); return 0; }";
    assert_eq!(run(ok).unwrap(), 0);
    let bad = "type Ratio = Float64 where value > 0.0 && value <= 1.0; \
                   fn mk(x: Float64) -> Ratio { return Ratio(x); } \
                   fn main() -> Int64 { let r = mk(2.5); return 0; }";
    assert!(run(bad)
        .unwrap_err()
        .contains("validation failed for `Ratio`"));
}

#[test]
fn sized_int_refined_type_constructs_at_runtime() {
    let src = "type Small = Int32 where value < 100; \
                   fn mk(x: Int32) -> Small { return Small(x); } \
                   fn main() -> Int64 { let s = mk(Int32(5)); return 0; }";
    assert_eq!(run(src).unwrap(), 0);
}

#[test]
fn cross_field_predicate_over_float_fields() {
    let ok = "type R = { a: Float64, b: Float64 } where a < b; \
                  fn mk(x: Float64, y: Float64) -> R { return R { a: x, b: y }; } \
                  fn main() -> Int64 { let r = mk(1.0, 2.0); return 0; }";
    assert_eq!(run(ok).unwrap(), 0);
    let bad = "type R = { a: Float64, b: Float64 } where a < b; \
                   fn mk(x: Float64, y: Float64) -> R { return R { a: x, b: y }; } \
                   fn main() -> Int64 { let r = mk(2.0, 1.0); return 0; }";
    assert!(run(bad).unwrap_err().contains("violates its `where`"));
}

#[test]
fn int_arithmetic_wraps_like_native() {
    // i64::MAX + 1 wraps to i64::MIN in BOTH backends (and independent of
    // the cargo profile — bare `+` would panic in a debug build).
    let src = "fn main() -> Int64 { \
                       let m = 9223372036854775807 \
                       let w = m + 1 \
                       if w < 0 { return 1 } return 0 }";
    assert_eq!(run(src).unwrap(), 1);
    // -i64::MIN also wraps (back to MIN).
    let neg = "fn main() -> Int64 { \
                       let m = -9223372036854775808 \
                       let w = 0 - m \
                       if w < 0 { return 1 } return 0 }";
    assert_eq!(run(neg).unwrap(), 1);
}

#[test]
fn division_traps_have_stable_messages() {
    let z = "fn main() -> Int64 { let mut d = 0; return 1 / d; }";
    assert_eq!(run(z).unwrap_err(), "division by zero");
    let rz = "fn main() -> Int64 { let mut d = 0; return 1 % d; }";
    assert_eq!(run(rz).unwrap_err(), "remainder by zero");
    // i64::MIN / -1 is unrepresentable: a clean trap, not a panic/SEH crash.
    let ovf = "fn main() -> Int64 { \
                       let m = -9223372036854775808 \
                       let mut d = 0 - 1 \
                       return m / d }";
    assert_eq!(run(ovf).unwrap_err(), "integer overflow in division");
}

#[test]
fn remainder_min_neg_one_is_zero_not_a_trap() {
    // `MIN % -1 == 0` — NO trap (RFC-0060), unlike `MIN / -1`.
    let src = "fn main() -> Int64 { \
                       let m = -9223372036854775808 \
                       let mut d = 0 - 1 \
                       return m % d }";
    assert_eq!(run(src).unwrap(), 0);
}

#[test]
fn remainder_sign_of_dividend_and_the_division_law() {
    // Truncated remainder takes the sign of the dividend (C/Rust/LLVM srem).
    let cases: &[(i64, i64, i64)] = &[
        (7, 3, 1),
        (-7, 3, -1),
        (7, -3, 1),
        (-7, -3, -1),
        (0, 5, 0),
        (9223372036854775807, 2, 1),
    ];
    for (a, b, want) in cases {
        let src = format!("fn main() -> Int64 {{ let a = {a} let b = {b} return a % b }}");
        assert_eq!(run(&src).unwrap(), *want, "{a} % {b}");
        // The law: `a == (a / b) * b + a % b` for every non-zero b.
        let law = format!(
            "fn main() -> Int64 {{ let a = {a} let b = {b} \
                 if (a / b) * b + a % b == a {{ return 1 }} return 0 }}"
        );
        assert_eq!(run(&law).unwrap(), 1, "law for {a} % {b}");
    }
}

#[test]
fn remainder_on_sized_ints_wraps_and_upholds_the_law() {
    // UInt8 / Int8: remainder computed at width, sign of dividend for signed.
    let u = "fn main() -> Int64 { let a: UInt8 = 200 let b: UInt8 = 7 \
                 let r = a % b return Int64(r) }";
    assert_eq!(run(u).unwrap(), 200 % 7);
    // Int8 MIN % -1 == 0, no trap. Build MIN (-128) and -1 by wrapping at width.
    let s = "fn main() -> Int64 { \
                 let hi: Int8 = 127 let min = hi + 1 \
                 let zero: Int8 = 0 let d = zero - 1 \
                 let r = min % d return Int64(r) }";
    assert_eq!(run(s).unwrap(), 0);
}

#[test]
fn break_exits_the_innermost_loop() {
    // Sum 0..10 but stop at 5: 0+1+2+3+4 = 10.
    let src = "fn main() -> Int64 { \
                   let mut s = 0 let mut i = 0 \
                   while i < 10 { if i == 5 { break } s = s + i i = i + 1 } \
                   return s }";
    assert_eq!(run(src).unwrap(), 10);
}

#[test]
fn continue_skips_to_the_next_iteration() {
    // Sum only the even numbers in 0..6: 0+2+4 = 6.
    let src = "fn main() -> Int64 { \
                   let mut s = 0 \
                   for i in [0, 1, 2, 3, 4, 5] { if i % 2 == 1 { continue } s = s + i } \
                   return s }";
    assert_eq!(run(src).unwrap(), 6);
}

#[test]
fn break_exits_only_the_inner_of_nested_loops() {
    // Inner breaks immediately; outer runs 3 times adding 1 each → 3.
    let src = "fn main() -> Int64 { \
                   let mut n = 0 \
                   for a in [0, 1, 2] { \
                       for b in [0, 1, 2] { break n = n + 100 } \
                       n = n + 1 } \
                   return n }";
    assert_eq!(run(src).unwrap(), 3);
}

#[test]
fn if_let_binds_on_match_and_runs_else_otherwise() {
    // Some binds `v`; None runs the else branch.
    let hit = "fn f(b: Bool) -> Option<Int64> { if b { return Some(7) } return None } \
                   fn main() -> Int64 { if let Some(v) = f(true) { return v } return 0 - 1 }";
    assert_eq!(run(hit).unwrap(), 7);
    let miss = "fn f(b: Bool) -> Option<Int64> { if b { return Some(7) } return None } \
                    fn main() -> Int64 { if let Some(v) = f(false) { return v } return 0 - 1 }";
    assert_eq!(run(miss).unwrap(), -1);
}

#[test]
fn if_let_over_result_and_user_enum() {
    let ok = "fn f() -> Result<Int64, String> { return Ok(4) } \
                  fn main() -> Int64 { if let Ok(n) = f() { return n } return 0 }";
    assert_eq!(run(ok).unwrap(), 4);
    let enm = "type Shape = | Circle(Int64) | Rect(Int64, Int64) | Empty \
                   fn main() -> Int64 { let s = Rect(3, 4) \
                       if let Rect(w, h) = s { return w * h } return 0 }";
    assert_eq!(run(enm).unwrap(), 12);
}

#[test]
fn while_let_drains_without_double_evaluating_the_scrutinee() {
    // The scrutinee `next()` decrements a global each call; if `while let`
    // double-evaluated it, the count would be wrong. It must tick once per
    // iteration (RFC-0060): 3 → prints 2,1,0 → 3 iterations, global ends at 0.
    let src = "let mut n: Int64 = 3 \
                   fn next() -> Option<Int64> { if n == 0 { return None } n = n - 1 return Some(n) } \
                   fn main() -> Int64 { let mut count = 0 \
                       while let Some(v) = next() { print(v) count = count + 1 } \
                       return count }";
    assert_eq!(run(src).unwrap(), 3);
}

#[test]
fn break_inside_if_let_exits_the_loop() {
    // `break` inside an `if let` body targets the enclosing loop (RFC-0060).
    let src = "fn main() -> Int64 { let mut s = 0 \
                   for x in [1, 2, 3, 4] { \
                       if let Some(v) = Some(x) { if v == 3 { break } s = s + v } } \
                   return s }";
    assert_eq!(run(src).unwrap(), 3);
}

#[test]
fn continue_under_a_region_still_frees_the_region() {
    // A region inside the loop body, exited early by `continue` every other
    // iteration — the interpreter decrements its region depth on that path
    // (so 100 iterations never exceed the 64-region cap). Just must not trap.
    let src = "fn main() -> Int64 { \
                   let mut n = 0 let mut i = 0 \
                   while i < 100 { \
                       i = i + 1 \
                       region { if i % 2 == 0 { continue } n = n + 1 } } \
                   return n }";
    assert_eq!(run(src).unwrap(), 50);
}

#[test]
fn wrapped_predicate_arithmetic_matches_native() {
    // `value + 1 != 0` at i64::MAX: wraps to MIN (≠ 0) — the predicate
    // holds in both backends (checked arithmetic used to refuse to prove
    // it and the interpreter then errored out).
    let src = "type T = Int64 where value + 1 != 0; \
                   fn mk(x: Int64) -> T { return T(x); } \
                   fn main() -> Int64 { let t = mk(9223372036854775807); return 0; }";
    assert_eq!(run(src).unwrap(), 0);
}

#[test]
fn regex_match_operator() {
    let src = "fn main() -> Int64 { if \"abc\" =~ \"[a-z]+\" { return 1; } return 0; }";
    assert_eq!(run(src).unwrap(), 1);
    let no = "fn main() -> Int64 { if \"ab9\" =~ \"[a-z]+\" { return 1; } return 0; }";
    assert_eq!(run(no).unwrap(), 0);
}

#[test]
fn validated_string_via_regex_traps() {
    let src = "type Code = String where value =~ \"[A-Z][A-Z][A-Z]\"; \
                   fn mk(s: consume String) -> Code { return Code(s); } \
                   fn main() -> Int64 { let c = mk(\"ab\"); return 0; }";
    assert!(run(src)
        .unwrap_err()
        .contains("validation failed for `Code`"));
}

#[test]
fn validation_accumulates_all_issues() {
    // Both checks fail → Invalid carries both issues (i18n keys included).
    let src = "type P = { n: Int64 }; \
                   fn v(a: Int64, b: Int64) -> Validation<P> { \
                       let mut issues: Array<Issue> = []; \
                       if a < 0 { issues.push(Issue { key: \"a.min\", path: \"a\", message: \"m\" }); } \
                       if b < 0 { issues.push(Issue { key: \"b.min\", path: \"b\", message: \"m\" }); } \
                       if issues.length > 0 { return Invalid(issues); } \
                       return Valid(P { n: a + b }); } \
                   fn iss(x: Validation<P>) -> Array<Issue> { \
                       return match x { Valid(p) => [], Invalid(is) => is.copy() }; } \
                   fn main() -> Int64 { return iss(v(0 - 1, 0 - 1)).length; }";
    assert_eq!(run(src).unwrap(), 2);
}

#[test]
fn validation_valid_case_carries_the_value() {
    let src = "type P = { n: Int64 }; \
                   fn v(a: Int64) -> Validation<P> { \
                       if a < 0 { return Invalid([]); } return Valid(P { n: a }); } \
                   fn valueOr(x: Validation<P>) -> Int64 { \
                       return match x { Valid(p) => p.n, Invalid(is) => 0 - 1 }; } \
                   fn main() -> Int64 { return valueOr(v(41)); }";
    assert_eq!(run(src).unwrap(), 41);
}

#[test]
fn multiline_string_includes_the_newline() {
    // A raw newline inside "..." is part of the string (RFC-0007).
    let src = "fn main() -> Int64 { let s = \"ab\ncd\"; return s.byteLength; }"; // 'a','b','\n','c','d' = 5
    assert_eq!(run(src).unwrap(), 5);
}

#[test]
fn template_value_exposes_parts_and_values() {
    // `template"..."` yields a first-class Template { parts, values }.
    let src = "fn main() -> Int64 { let n = 7; let t = template\"a\\{n}b\"; \
                   return t.parts.length + t.values.length; }"; // 2 parts + 1 value = 3
    assert_eq!(run(src).unwrap(), 3);
}

#[test]
fn tagged_template_needs_an_interpolation() {
    // A tag on a hole-less string is rejected (use a plain string instead).
    let src = "fn sql(p: Array<String>, v: Array<Value>) -> Int64 { return 0; } \
                   fn main() -> Int64 { return sql\"no holes here\"; }";
    assert!(run(src).unwrap_err().contains("interpolation"));
}

#[test]
fn value_boxes_string_and_int_distinctly() {
    let src = "fn main() -> Int64 { \
                   let a = match value(7) { IntVal(n) => n, BoolVal(b) => 0, StrVal(s) => 0 - 1 }; \
                   let b = match value(\"hey\") { IntVal(n) => 0, BoolVal(x) => 0, StrVal(s) => s.byteLength }; \
                   return a + b; }"; // 7 + 3
    assert_eq!(run(src).unwrap(), 10);
}

/// RFC-0094 M3, and RFC-0007 §v2 with it: a hole may carry any type that
/// says how it renders, and it reaches the tag as the `StrVal` it rendered
/// to — so a hole is still data and still cannot become the tag's structure.
#[test]
fn value_boxes_a_declared_type_as_the_string_it_renders_to() {
    let src = "protocol Show { fn show(self) -> String }\n\
                   type P = { x: Int64 }\n\
                   impl Show for P { fn show(self) -> String { return \"pt\" } }\n\
                   fn main() -> Int64 { let p = P { x: 1 }\n \
                   return match value(p) { IntVal(n) => 0, BoolVal(b) => 0, \
                   StrVal(s) => s.byteLength } }";
    assert_eq!(run(src).unwrap(), 2);
}

/// The scalar guard, at runtime. `impl Show for Int64` is callable by name
/// and does NOT redefine the digits: `7.toString()` is `7`, not what the
/// impl says. The impl's own body is `self.toString()`, so a dispatch that
/// took a scalar would not return at all.
#[test]
fn a_scalar_never_renders_through_a_declaration() {
    let src = "protocol Show { fn show(self) -> String }\n\
                   impl Show for Int64 { fn show(self) -> String { \
                   return \"n\" + self.toString() } }\n\
                   fn main() -> Int64 { let n = 7\n \
                   return n.toString().byteLength + n.show().byteLength }";
    assert_eq!(run(src).unwrap(), 3); // "7" is 1, "n7" is 2
}

#[test]
fn logger_and_levels_typecheck_and_run() {
    // A logger with each level, using interpolation in the message. Logs go
    // to stderr; the program returns normally.
    let src = "fn main() -> Int64 { let log = logger(\"t\"); let n = 2; \
                   log.trace(\"a\"); log.debug(\"b\"); log.info(\"n=\\{n}\"); \
                   log.warn(\"c\"); log.error(\"d\"); return n; }";
    assert_eq!(run(src).unwrap(), 2);
}

#[test]
fn log_level_requires_a_logger() {
    // Calling a level on a non-Logger is rejected.
    let src = "fn main() -> Int64 { info(\"notalogger\", \"x\"); return 0; }";
    assert!(run(src).is_err());
}

// `logging_is_forbidden_in_spawned_tasks` is `tests/isolation.rs`'s now: the
// spawn rule is the effect judgment's, and this file's `run` does not install
// it (RFC-0125 §3 M6, the isolation slice).

#[test]
fn logging_config_block_parses_and_runs() {
    let src = "logging { level: warn } \
                   fn main() -> Int64 { let log = logger(\"a\"); \
                   log.info(\"filtered\"); log.error(\"shown\"); return 0; }";
    assert_eq!(run(src).unwrap(), 0);
}

#[test]
fn invalid_log_level_is_rejected() {
    let src = "logging { level: loud } fn main() -> Int64 { return 0; }";
    assert!(run(src).unwrap_err().contains("log level"));
}

#[test]
fn duplicate_logging_block_is_rejected() {
    let src = "logging { level: info } logging { level: warn } \
                   fn main() -> Int64 { return 0; }";
    assert!(run(src).unwrap_err().contains("duplicate"));
}

#[test]
fn logging_sink_and_level_parse_together() {
    let src = "logging { level: warn, sink: stdout } \
                   fn main() -> Int64 { let l = logger(\"a\"); l.warn(\"x\"); return 0; }";
    assert_eq!(run(src).unwrap(), 0);
}

#[test]
fn unknown_sink_is_rejected() {
    let src = "logging { sink: syslog } fn main() -> Int64 { return 0; }";
    assert!(run(src).unwrap_err().contains("sink"));
}

#[test]
fn file_sink_needs_a_string_path() {
    let src = "logging { sink: file(main) } fn main() -> Int64 { return 0; }";
    assert!(run(src).is_err());
}

#[test]
fn spawn_and_join_fork_join() {
    let src = "
            fn sq(n: Int64) -> Int64 { return n * n; }
            fn main() -> Int64 {
                let a = spawn sq(6);
                let b = spawn sq(8);
                return a.join() + b.join();   // 36 + 64
            }
        ";
    assert_eq!(run(src).unwrap(), 100);
}

#[test]
fn modify_parameter_writes_back_to_caller() {
    let src = "
            type C = { x: Int64 };
            fn bump(c: modify C) { c.x = c.x + 1; }
            fn main() -> Int64 {
                let mut c = C { x: 40 };
                bump(c); bump(c);   // caller's c is mutated each time
                return c.x;          // 42
            }
        ";
    assert_eq!(run(src).unwrap(), 42);
}

#[test]
fn record_field_access_and_subtyping() {
    let src = "
            type Named = { name: Int64 };
            type Pt = { name: Int64, x: Int64, y: Int64 };
            fn nm(w: Named) -> Int64 { return w.name; }
            fn main() -> Int64 {
                let p = Pt { name: 3, x: 10, y: 20 };
                return nm(p) + p.x + p.y;   // 3 + 10 + 20
            }
        ";
    assert_eq!(run(src).unwrap(), 33);
}

#[test]
fn enum_construct_and_match() {
    let src = "
            type Shape = | Circle(Int64) | Square(Int64) | Nil;
            fn area(s: Shape) -> Int64 {
                return match s { Circle(r) => 3 * r * r, Square(w) => w * w, Nil => 0 };
            }
            fn main() -> Int64 { return area(Circle(2)) + area(Square(5)) + area(Nil); }
        ";
    assert_eq!(run(src).unwrap(), 37); // 12 + 25 + 0
}

#[test]
fn dynamic_string_concat_and_len() {
    let src = "fn g(n: String) -> String { return \"Hi, \" + n + \"!\"; } \
                   fn main() -> Int64 { return g(\"Vyrn\").byteLength; }";
    assert_eq!(run(src).unwrap(), 9); // "Hi, Vyrn!" = 9 bytes
}

#[test]
fn to_string_method_renders() {
    // `x.toString()` renders scalars, then `+` concatenates: "42/true" = 7.
    let src = "fn main() -> Int64 { let s = (42).toString() + \"/\" + true.toString(); \
                   return s.byteLength; }";
    assert_eq!(run(src).unwrap(), 7);
}

#[test]
fn contextual_array_literal_is_growable() {
    // A literal in an `Array<T>` position is a growable heap array you can
    // `push` onto — its element count is observable via `.length`.
    let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 3]; \
                   a.push(4); return a.length + a[3]; }"; // 4 + 4
    assert_eq!(run(src).unwrap(), 8);
}

#[test]
fn task_join_method_awaits_result() {
    let src = "fn sq(n: Int64) -> Int64 { return n * n } \
                   fn main() -> Int64 { let t = spawn sq(9); return t.join() }";
    assert_eq!(run(src).unwrap(), 81);
}

#[test]
fn string_eq() {
    let src = "fn main() -> Int64 { \
                   let s = \"hello\"; \
                   if s == \"hello\" { return 1; } return 0; }";
    assert_eq!(run(src).unwrap(), 1);
}

#[test]
fn while_loop_and_mut() {
    let src = "
            fn main() -> Int64 {
                let mut i = 0;
                let mut sum = 0;
                while i < 5 {
                    sum = sum + i;
                    i = i + 1;
                }
                return sum;
            }
        ";
    assert_eq!(run(src).unwrap(), 10); // 0+1+2+3+4
}

/// Calling an `extern fn` (RFC-0012) traps: the interpreter has no host to
/// provide it. Declaring one is fine — only the call is the unavailable
/// effect. Wording is byte-identical to the native trap stub's.
#[test]
fn extern_call_traps_with_canonical_wording() {
    let src = "extern fn jsNow() -> Float64\n\
                   fn main() -> Int64 {\n\
                       let t = jsNow()\n\
                       return 0\n\
                   }";
    assert_eq!(
        run(src).unwrap_err(),
        "extern `jsNow` is not available on this target"
    );
    // Declaring without calling is harmless.
    let src = "extern fn jsNow() -> Float64\nfn main() -> Int64 { return 7 }";
    assert_eq!(run(src).unwrap(), 7);
}

/// An `export extern fn` (RFC-0012 M2) is a normal function: calling it from
/// Vyrn runs its body — no trap anywhere. Only body-less imports trap
/// off-wasm, so an export-extern-using program stays three-way-parity-capable.
#[test]
fn export_extern_is_a_normal_call() {
    let src = "export extern fn vyrnAdd(a: Int64, b: Int64) -> Int64 { return a + b }\n\
                   fn main() -> Int64 { return vyrnAdd(40, 2) }";
    assert_eq!(run(src).unwrap(), 42);
}

/// The native arena runtime has a fixed 64-slot region stack and traps on
/// a 65th nested region; the interpreter enforces the identical bound with
/// the identical message — depth accumulates dynamically across calls.
#[test]
fn region_nesting_is_bounded_at_64() {
    let src = |n: i64| {
        format!(
            "fn deep(n: Int64) -> Int64 {{
                     if n == 0 {{ return 0; }}
                     region {{
                         return deep(n - 1);
                     }}
                 }}
                 fn main() -> Int64 {{ return deep({n}); }}"
        )
    };
    // 64 nested regions fill the stack exactly — fine.
    assert_eq!(run(&src(64)).unwrap(), 0);
    // The 65th traps, wording shared with the native runtime.
    assert_eq!(run(&src(65)).unwrap_err(), "region nesting exceeds 64");
}

// ---- in-place array mutation (RFC-0011) -----------------------------

#[test]
fn index_store_mutates_in_place() {
    let src = "fn main() -> Int64 { let mut a: Array<Int64> = [10, 20, 30]; \
                   a[1] = 25; return a[0] + a[1] + a[2]; }";
    assert_eq!(run(src).unwrap(), 65);
}

#[test]
fn index_store_out_of_bounds_traps() {
    let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 3]; a[5] = 9; return 0; }";
    assert_eq!(run(src).unwrap_err(), "array index 5 out of bounds");
}

#[test]
fn pop_returns_last_and_shrinks() {
    let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 7]; \
                   let p = match a.pop() { Some(x) => x, None => -1 }; \
                   return p * 100 + a.length; }";
    assert_eq!(run(src).unwrap(), 702); // popped 7, length now 2
}

#[test]
fn pop_on_empty_is_none() {
    let src = "fn main() -> Int64 { let mut a: Array<Int64> = [5]; \
                   let p1 = a.pop(); let p2 = a.pop(); \
                   return match p2 { Some(x) => x, None => -1 }; }";
    assert_eq!(run(src).unwrap(), -1);
}

#[test]
fn swapremove_moves_last_into_slot() {
    // [10, 20, 30, 40]; swapRemove(1) returns 20, moves 40 into slot 1.
    let src = "fn main() -> Int64 { let mut a: Array<Int64> = [10, 20, 30, 40]; \
                   let g = a.swapRemove(1); \
                   return g * 1000 + a[0] * 100 + a[1] + a.length; }";
    // g=20 -> 20000; a=[10,40,30]; 10*100=1000; a[1]=40; length=3 -> 21043
    assert_eq!(run(src).unwrap(), 21043);
}

#[test]
fn swapremove_out_of_bounds_traps() {
    let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 3]; \
                   let g = a.swapRemove(9); return g; }";
    assert_eq!(run(src).unwrap_err(), "array index 9 out of bounds");
}

#[test]
fn index_store_validated_element_traps_at_runtime() {
    let src = "type Age = Int64 where value >= 18 \
                   fn main() -> Int64 { let mut a: Array<Age> = [Age(20)]; \
                   let mut n = 5; a[0] = n; return 0; }";
    assert_eq!(run(src).unwrap_err(), "validation failed for `Age`");
}

// ---- module state (RFC-0013) ---------------------------------------

#[test]
fn global_mutation_persists_across_calls() {
    // Each `bump` sees the previous call's write to the shared global.
    let src = "let mut hits = 0 \
                   fn bump() -> Int64 { hits = hits + 1 return hits } \
                   fn main() -> Int64 { let a = bump() let b = bump() let c = bump() \
                                        return a + b + c }";
    assert_eq!(run(src).unwrap(), 6); // 1 + 2 + 3
}

#[test]
fn globals_initialize_in_declaration_order() {
    // `b`'s initializer reads the earlier global `a`.
    let src = "let a = 10 \
                   let b = a + 5 \
                   fn main() -> Int64 { return b }";
    assert_eq!(run(src).unwrap(), 15);
}

#[test]
fn validated_global_traps_at_runtime_on_bad_store() {
    // A non-constant store into a validated global validates at runtime.
    let src = "type Age = Int64 where value >= 18 \
                   let mut a: Age = Age(20) \
                   fn setAge(n: Int64) -> Int64 { a = n return 0 } \
                   fn main() -> Int64 { return setAge(5) }";
    assert_eq!(run(src).unwrap_err(), "validation failed for `Age`");
}

#[test]
fn local_shadows_global_in_interp() {
    // A local `hits` shadows the global; the global stays untouched.
    let src = "let mut hits = 100 \
                   fn f() -> Int64 { let hits = 1 return hits } \
                   fn main() -> Int64 { let a = f() return a + hits }";
    assert_eq!(run(src).unwrap(), 101); // local 1 + global 100
}

#[test]
fn string_global_reads_back() {
    let src = "let banner = \"vyrn\" \
                   fn f() -> Int64 { return banner.byteLength } \
                   fn main() -> Int64 { return f() }";
    assert_eq!(run(src).unwrap(), 4);
}

// ---- RFC-0011 addendum: `a[i].field = v` write-through --------------

#[test]
fn index_field_write_through_is_visible() {
    // A field write through the array must stick (load-modify-store), and the
    // RHS reads the pre-write element.
    let src = "type P = { x: Int64, y: Int64 } \
                   fn main() -> Int64 { \
                       let mut a: Array<P> = [] \
                       a.push(P { x: 1, y: 2 }) \
                       a.push(P { x: 3, y: 4 }) \
                       a[1].x = 20 \
                       a[0].y = a[0].y + 9 \
                       return a[0].y + a[1].x }"; // 11 + 20 = 31
    assert_eq!(run(src).unwrap(), 31);
}

#[test]
fn index_field_write_through_traps_on_oob_load() {
    // The bounds check on the element LOAD fires with the canonical wording.
    let src = "type P = { x: Int64 } \
                   fn main() -> Int64 { \
                       let mut a: Array<P> = [P { x: 1 }] \
                       a[5].x = 9 \
                       return 0 }";
    assert_eq!(run(src).unwrap_err(), "array index 5 out of bounds");
}

// ---- JSON codec (RFC-0018) ------------------------------------------
// `run` returns an `Int64`, and match arms are single expressions, so these
// programs fold each assertion into an integer via a tiny `eq` helper.
const EQ: &str = "fn eq(a: String, b: String) -> Int64 { if a == b { return 1; } return 0; } ";

#[test]
fn tojson_canonical_record_order_and_escaping() {
    // Declaration order, no whitespace, minimal escaping.
    let src = "type P = { name: String, age: Int64, ok: Bool } \
                   fn main() -> Int64 { \
                       let p = P { name: \"a\\\"b\", age: 30, ok: true } \
                       if toJson(p) == \"{\\\"name\\\":\\\"a\\\\\\\"b\\\",\\\"age\\\":30,\\\"ok\\\":true}\" { return 1; } \
                       return 0; }";
    assert_eq!(run_json(src).unwrap(), 1);
}

#[test]
fn tojson_omits_none_field_and_bare_option_is_null() {
    let src = "type P = { name: String, nick: Option<String> } \
                   fn main() -> Int64 { \
                       let p = P { name: \"x\", nick: None } \
                       if toJson(p) == \"{\\\"name\\\":\\\"x\\\"}\" { return 1; } \
                       return 0; }";
    assert_eq!(run_json(src).unwrap(), 1);
}

#[test]
fn roundtrip_valid_record() {
    let src = "type Age = Int64 where value >= 0 && value <= 130 \
                   type User = { name: String, age: Age, nick: Option<String> } \
                   fn main() -> Int64 { \
                       let u = User { name: \"Ada\", age: 36, nick: Some(\"A\") } \
                       let s = toJson(u) \
                       return match fromJson(User, s) { \
                           Valid(u2) => u2.age + u2.name.byteLength, \
                           Invalid(iss) => 0 - iss.length, \
                       }; }";
    // age 36 + name length 3 = 39.
    assert_eq!(run_json(src).unwrap(), 39);
}

#[test]
fn exact_large_integer_roundtrips() {
    // Beyond f64's 53-bit exact range — must survive as an exact i64.
    let src = "type W = { n: Int64 } \
                   fn main() -> Int64 { \
                       return match fromJson(W, \"{\\\"n\\\":9007199254740993}\") { \
                           Valid(w) => w.n - 9007199254740992, \
                           Invalid(iss) => 0 - iss.length, \
                       }; }";
    assert_eq!(run_json(src).unwrap(), 1);
}

#[test]
fn decode_unknown_fields_ignored_and_null_option_is_none() {
    let src = "type U = { name: String, nick: Option<String> } \
                   fn main() -> Int64 { \
                       return match fromJson(U, \"{\\\"name\\\":\\\"x\\\",\\\"nick\\\":null,\\\"extra\\\":7}\") { \
                           Valid(u) => match u.nick { Some(s) => 2, None => 1, }, \
                           Invalid(iss) => 0 - iss.length, \
                       }; }";
    assert_eq!(run_json(src).unwrap(), 1);
}

#[test]
fn decode_missing_field_issue_bytes() {
    let src = "type U = { name: String, age: Int64 } \
                   fn main() -> Int64 { \
                       return match fromJson(U, \"{\\\"name\\\":\\\"x\\\"}\") { \
                           Valid(u) => 0, \
                           Invalid(iss) => eq(iss[0].key, \"json.missing\") + eq(iss[0].path, \"age\") \
                               + eq(iss[0].message, \"missing required field `age`\"), \
                       }; }";
    assert_eq!(run_json(&format!("{EQ}{src}")).unwrap(), 3);
}

#[test]
fn decode_type_mismatch_issue_bytes() {
    let src = "type U = { age: Int64 } \
                   fn main() -> Int64 { \
                       return match fromJson(U, \"{\\\"age\\\":\\\"nope\\\"}\") { \
                           Valid(u) => 0, \
                           Invalid(iss) => eq(iss[0].key, \"json.type\") + eq(iss[0].path, \"age\") \
                               + eq(iss[0].message, \"expected integer, found string\"), \
                       }; }";
    assert_eq!(run_json(&format!("{EQ}{src}")).unwrap(), 3);
}

#[test]
fn decode_validation_issue_accumulates_all() {
    // Two failing `where` clauses -> two `validate` issues, both reported.
    let src = "type Age = Int64 where value >= 0 && value <= 130 \
                   type Name = String where value.byteLength >= 1 \
                   type U = { name: Name, age: Age } \
                   fn main() -> Int64 { \
                       return match fromJson(U, \"{\\\"name\\\":\\\"\\\",\\\"age\\\":999}\") { \
                           Valid(u) => 0, \
                           Invalid(iss) => iss.length, \
                       }; }";
    assert_eq!(run_json(src).unwrap(), 2);
}

#[test]
fn decode_validation_issue_bytes() {
    let src = "type Age = Int64 where value >= 0 && value <= 130 \
                   type U = { age: Age } \
                   fn main() -> Int64 { \
                       return match fromJson(U, \"{\\\"age\\\":999}\") { \
                           Valid(u) => 0, \
                           Invalid(iss) => eq(iss[0].key, \"validate\") + eq(iss[0].path, \"age\") \
                               + eq(iss[0].message, \"validation failed for `Age`\"), \
                       }; }";
    assert_eq!(run_json(&format!("{EQ}{src}")).unwrap(), 3);
}

#[test]
fn decode_parse_error_is_single_issue() {
    let src = "type U = { a: Int64 } \
                   fn main() -> Int64 { \
                       return match fromJson(U, \"{ bad\") { \
                           Valid(u) => 0, \
                           Invalid(iss) => iss.length + eq(iss[0].key, \"json.parse\") + eq(iss[0].path, \"\"), \
                       }; }";
    // one parse issue + key match + path match = 3.
    assert_eq!(run_json(&format!("{EQ}{src}")).unwrap(), 3);
}

#[test]
fn decode_enum_payloadless_roundtrip() {
    let src = "type Color = | Red | Green | Blue \
                   type P = { c: Color } \
                   fn main() -> Int64 { \
                       let p = P { c: Green } \
                       let s = toJson(p) \
                       if s == \"{\\\"c\\\":\\\"Green\\\"}\" { \
                           return match fromJson(P, s) { Valid(q) => 1, Invalid(iss) => 0, }; \
                       } \
                       return 5; }";
    assert_eq!(run_json(src).unwrap(), 1);
}

// ---- function values (RFC-0023) -------------------------------------

const TWICE: &str = "fn twice(xs: Array<Int64>, f: fn(Int64) -> Int64) -> Array<Int64> {\n\
         let mut out: Array<Int64> = []\n\
         for x in xs { out.push(f(x)) }\n\
         return out }\n\
         fn sum(xs: Array<Int64>) -> Int64 {\n\
             let mut s = 0  for x in xs { s = s + x }  return s }\n";

#[test]
fn lambda_argument_runs() {
    let src = format!("{TWICE}fn main() -> Int64 {{ return sum(twice([1, 2, 3], x -> x * 2)) }}");
    assert_eq!(run(&src).unwrap(), 12);
}

#[test]
fn lambda_captures_by_read() {
    let src = format!(
        "{TWICE}fn main() -> Int64 {{ let off = 10  return sum(twice([1, 2, 3], x -> x + off)) }}"
    );
    assert_eq!(run(&src).unwrap(), 36);
}

#[test]
fn named_function_as_value() {
    let src = format!(
        "{TWICE}fn dbl(n: Int64) -> Int64 {{ return n * 2 }}\n\
             fn main() -> Int64 {{ return sum(twice([1, 2, 3], dbl)) }}"
    );
    assert_eq!(run(&src).unwrap(), 12);
}

#[test]
fn passthrough_and_empty_array() {
    let src = format!(
            "{TWICE}fn outer(xs: Array<Int64>, g: fn(Int64) -> Int64) -> Array<Int64> {{ return twice(xs, g) }}\n\
             fn main() -> Int64 {{ let e: Array<Int64> = []  let z = sum(outer(e, x -> x + 1))\n\
             let bump = 5  return z + sum(outer([1, 2], x -> x + bump)) }}"
        );
    // empty → 0; outer([1,2], +5) → [6,7] → 13.
    assert_eq!(run(&src).unwrap(), 13);
}

#[test]
fn generic_map_runs() {
    let src = "fn map<T, U>(xs: Array<T>, f: fn(T) -> U) -> Array<U> {\n\
             let mut out: Array<U> = []  for x in xs { out.push(f(x)) }  return out }\n\
             fn main() -> Int64 {\n\
                 let ys: Array<Int64> = [1, 2, 3]\n\
                 let zs = map(ys, x -> x * x)\n\
                 let mut s = 0  for z in zs { s = s + z }  return s }";
    assert_eq!(run(src).unwrap(), 14);
}

// ---- stored function values (RFC-0037) -------------------------------

#[test]
fn stored_lambda_in_let_runs() {
    let src = "fn main() -> Int64 { let g: fn(Int64) -> Int64 = x -> x * 2  return g(21) }";
    assert_eq!(run(src).unwrap(), 42);
}

#[test]
fn stored_capture_survives_scope_exit() {
    // The capture is a by-value snapshot at the lambda's evaluation site —
    // it lives inside the value, so it survives the maker's return.
    let src = "fn makeAdder(n: Int64) -> fn(Int64) -> Int64 { return x -> x + n }\n\
             fn main() -> Int64 { let add5 = makeAdder(5)  let add7 = makeAdder(7)\n\
             return add5(10) + add7(10) }";
    assert_eq!(run(src).unwrap(), 32);
}

#[test]
fn stored_capture_is_a_snapshot_not_a_reference() {
    // Reassigning the captured binding after the literal is evaluated is
    // never observed (RFC-0023 capture timing, verbatim in storage).
    let src = "fn main() -> Int64 { let mut n = 1\n\
             let f: fn() -> Int64 = () -> n\n\
             n = 5\n\
             return f() }";
    assert_eq!(run(src).unwrap(), 1);
}

#[test]
fn stored_values_in_arrays_records_options() {
    let src = "type Ops = { plus: fn(Int64) -> Int64, minus: fn(Int64) -> Int64 }\n\
             fn main() -> Int64 {\n\
             let mut xs: Array<fn(Int64) -> Int64> = []\n\
             xs.push(x -> x * 2)\n\
             xs.push(x -> x + 100)\n\
             let mut s = 0\n\
             for f in xs { s = s + f(10) }\n\
             let ops = Ops { plus: x -> x + 1, minus: x -> x - 1 }\n\
             let p = ops.plus\n\
             let m = ops.minus\n\
             let o: Option<fn(Int64) -> Int64> = Some(x -> x * x)\n\
             let q = match o { Some(f) => f(3), None => 0 }\n\
             return s + p(5) + m(5) + q }";
    // s = 20 + 110 = 130; p(5)=6; m(5)=4; q=9 → 149.
    assert_eq!(run(src).unwrap(), 149);
}

#[test]
fn stored_fn_module_state_and_middleware_chain() {
    // Module state holds closures (read live at call time); a middleware
    // chain matches the RFC's surface: first Some(..) wins.
    let src = "type Middleware = fn(Int64) -> Option<Int64>\n\
             let mut chain: Array<Middleware> = []\n\
             fn add(threshold: Int64) { chain.push(x -> if x > threshold { Some(x * 10) } else { None }) }\n\
             fn runAll(x: Int64) -> Int64 {\n\
                 let mut hit = 0 - 1\n\
                 for m in chain {\n\
                     if hit < 0 { hit = match m(x) { Some(r) => r, None => hit } }\n\
                 }\n\
                 return hit }\n\
             fn main() -> Int64 { add(100)  add(10)  add(0)\n\
             return runAll(50) }";
    // 50 > 100? no. 50 > 10 → Some(500).
    assert_eq!(run(src).unwrap(), 500);
}

#[test]
fn stored_named_fn_and_composition() {
    let src = "fn dbl(n: Int64) -> Int64 { return n * 2 }\n\
             fn main() -> Int64 { let g = dbl  let h = g\n\
             let mut cur: fn(Int64) -> Int64 = h\n\
             return cur(4) }";
    assert_eq!(run(src).unwrap(), 8);
}

#[test]
fn stored_value_flows_into_v1_fn_parameter() {
    // A stored value handed to a v1 `fn`-typed parameter dispatches inside
    // the (interp-dynamic / codegen-specialized) instance.
    let src = format!(
        "{TWICE}fn main() -> Int64 {{ let bump = 3\n\
             let g: fn(Int64) -> Int64 = x -> x + bump\n\
             return sum(twice([1, 2, 3], g)) }}"
    );
    assert_eq!(run(&src).unwrap(), 15);
}

#[test]
fn stored_closure_reads_module_state_live() {
    // Module state is NOT captured — a read inside the body resolves live.
    let src = "let mut base: Int64 = 1\n\
             fn main() -> Int64 { let f: fn() -> Int64 = () -> base\n\
             base = 41\n\
             return f() + 1 }";
    assert_eq!(run(src).unwrap(), 42);
}

#[test]
fn generic_function_stores_fn_values_per_instantiation() {
    // A stored fn type mentioning `T` monomorphizes with the body: each
    // instantiation gets its own signature (and, in codegen, its own enum).
    let src = "fn relay<T>(x: T) -> T {\n\
             let f: fn(T) -> T = v -> v\n\
             return f(x) }\n\
             fn main() -> Int64 {\n\
             let n = relay(41)\n\
             let s = relay(\"ok\")\n\
             if s == \"ok\" { return n + 1 }\n\
             return 0 }";
    assert_eq!(run(src).unwrap(), 42);
}

#[test]
fn module_state_of_fn_type_with_init_order() {
    // A directly fn-typed module-state binding (RFC-0029 init order):
    // the initializer lambda is replaced at runtime; reads are live.
    let src = "let mut cur: fn(Int64) -> Int64 = x -> x + 1\n\
             fn dbl(n: Int64) -> Int64 { return n * 2 }\n\
             fn main() -> Int64 {\n\
             let before = cur(10)\n\
             cur = dbl\n\
             return before + cur(10) }";
    assert_eq!(run(src).unwrap(), 31);
}

#[test]
fn stored_value_into_generic_v1_fn_parameter() {
    // A stored value handed to a GENERIC higher-order function: the
    // outbound type parameter solves from the stored signature's return.
    let src = "fn map<T, U>(xs: Array<T>, f: fn(T) -> U) -> Array<U> {\n\
             let mut out: Array<U> = []  for x in xs { out.push(f(x)) }  return out }\n\
             fn main() -> Int64 {\n\
             let xs: Array<Int64> = [1, 2]\n\
             let g: fn(Int64) -> Int64 = x -> x * 3\n\
             let ys = map(xs, g)\n\
             return ys[0] + ys[1] }";
    assert_eq!(run(src).unwrap(), 9);
}

#[test]
fn trap_inside_stored_closure_has_canonical_wording() {
    let src = "fn main() -> Int64 { let f: fn(Int64) -> Int64 = x -> 10 / x\n\
             return f(0) }";
    let err = run(src).unwrap_err();
    assert!(err.contains("division by zero"), "{err}");
}

#[test]
fn stored_lambda_coerces_arguments_to_signature_types() {
    // The declared slot type supplies the parameter coercions: a UInt8
    // parameter wraps exactly as a named callee's would.
    let src = "fn main() -> Int64 { let f: fn(UInt8) -> Int64 = b -> Int64(b + 200)\n\
             return f(100) }";
    // 100 + 200 wraps at the UInt8 parameter's width: 300 & 0xFF = 44.
    assert_eq!(run(src).unwrap(), 44);
}

// ---- `if` as an expression (RFC-0030) --------------------------------

#[test]
fn if_expression_yields_the_taken_branch() {
    let src = "fn main() -> Int64 {\n\
             let x = if 2 > 1 { 10 } else { 20 }\n\
             return x }";
    assert_eq!(run(src).unwrap(), 10);
}

#[test]
fn if_expression_chain_selects_the_matching_arm() {
    let src = "fn tier(s: Int64) -> Int64 {\n\
             return if s >= 90 { 3 } else if s >= 50 { 2 } else { 1 } }\n\
             fn main() -> Int64 { return tier(95) + tier(60) * 10 + tier(10) * 100 }";
    // 3 + 2*10 + 1*100 = 123
    assert_eq!(run(src).unwrap(), 123);
}

#[test]
fn only_the_taken_branch_evaluates() {
    // `boom()` traps; it sits in the untaken branch and must never run, so the
    // program returns cleanly. If both branches evaluated, this would trap.
    let src = "fn boom() -> Int64 { let a: Array<Int64> = [1]  return a[99] }\n\
             fn main() -> Int64 {\n\
             let x = if true { 7 } else { boom() }\n\
             return x }";
    assert_eq!(run(src).unwrap(), 7);
}

#[test]
fn if_expression_nests_and_composes() {
    let src = "fn main() -> Int64 {\n\
             let n = if false { 1 } else { if true { 2 } else { 3 } }\n\
             let xs: Array<Int64> = [if n == 2 { 100 } else { 0 }, 5]\n\
             return xs[0] + xs[1] }";
    assert_eq!(run(src).unwrap(), 105);
}

#[test]
fn a_stepped_stream_runs_its_producer_only_when_asked() {
    // RFC-0075 M2b, in the engine with no IR to inspect. `tick` never answers
    // `None`, so this program does not terminate under the representation M1
    // shipped — it terminates here, and the count is the reason: eight `next`
    // calls out of a feed with no end.
    //
    // The cursor is module state rather than a slot, because these tests run
    // one file with no `std`: the slab a real producer's cursor comes from is
    // `std/stream`'s since RFC-0090 M3, and what the ENGINE owes is to hand
    // the two words back to the step and to call it once with `closing`.
    let src = "let mut steps = 0 \
                   let mut cur = 0 \
                   fn tick(sl: Int64, gn: Int64, cl: Bool) -> Option<Int64> { \
                   if cl { return None } \
                   let n = cur cur = n + 1 \
                   steps = steps + 1 return Some(n) } \
                   fn main() -> Int64 { \
                     let mut seen = 0 \
                     for v in fromStep(0, 1, tick) { seen = seen + v if v == 7 { break } } \
                     return steps }";
    assert_eq!(run(src), Ok(8));
}

/// RFC-0075 M2c's `map`, spelled the way `std/stream` spells it: no
/// `for … in` at all, a step that reads its source with `pullAt`, and a
/// wrapper that owns the box the source moved into. Its closing call takes
/// the source back out and closes it, which is the whole of what M2c used to
/// do inside the runtime's walk.
const LMAP: &str = "fn lmap<T, U>(s: Stream<T>, f: fn(T) -> U) -> Stream<U> { \
                        let a = boxStream(s) \
                        let g: fn(T) -> U = f \
                        let step: fn(Int64, Int64, Bool) -> Option<U> = (sl, gn, cl) -> { \
                        if cl { let src: Stream<T> = unboxStream(a) close(src) return None } \
                        let x: Option<T> = pullAt(a) \
                        if let Some(v) = x { return Some(g(v)) } return None } \
                        return fromStep(0, 1, step) } ";

#[test]
fn a_wrapper_asks_its_source_once_per_element_it_is_asked_for() {
    // The milestone, in the engine with no IR to inspect: `tick` has no end,
    // so this program does not terminate if `lmap` drains. It terminates, and
    // the count says nothing was read ahead — four elements out, four asks
    // in.
    let src = format!(
        "let mut steps = 0 \
             let mut cur = 0 \
             fn tick(sl: Int64, gn: Int64, cl: Bool) -> Option<Int64> {{ \
             if cl {{ return None }} \
             let n = cur cur = n + 1 \
             steps = steps + 1 return Some(n) }} \
             fn double(n: Int64) -> Int64 {{ return n * 2 }} \
             {LMAP} \
             fn main() -> Int64 {{ \
               let mut seen = 0 \
               for v in lmap(fromStep(0, 1, tick), double) {{ seen = seen + v \
                 if v == 6 {{ break }} }} \
               return steps }}"
    );
    assert_eq!(run(&src), Ok(4));
}

#[test]
fn releasing_a_chain_closes_one_stream_per_link() {
    // Three wrappers over one producer is four streams and four releases, and
    // the walk M2c ran inside the runtime is now the wrappers closing their
    // own sources. Each `unboxStream` empties its box, so a release that ran
    // twice would trap on the second — and one that stopped early would leave
    // a box behind, which the count below catches: 10 000 cycles, four closes
    // each, and the producer's own closing call is what `closed` counts.
    let src = format!(
        "let mut cur = 0 \
             let mut closed = 0 \
             fn tick(sl: Int64, gn: Int64, cl: Bool) -> Option<Int64> {{ \
             if cl {{ closed = closed + 1 return None }} \
             let n = cur cur = n + 1 return Some(n) }} \
             fn double(n: Int64) -> Int64 {{ return n * 2 }} \
             {LMAP} \
             fn main() -> Int64 {{ let mut i = 0 \
               while i < 10000 {{ \
                 let s = lmap(lmap(lmap(fromStep(0, 1, tick), double), double), double) \
                 close(s) i = i + 1 }} \
               return closed }}"
    );
    assert_eq!(run(&src), Ok(10000));
}

#[test]
fn pull_at_an_address_with_no_stream_traps() {
    // `pullAt` is a builtin and an address is an ordinary `Int64`, so nothing
    // stops a program from calling it on a number. The wording is the one the
    // compiled backends print.
    let src = "fn main() -> Int64 { let x: Option<Int64> = pullAt(24) return 0 }";
    match run(src) {
        Err(e) => assert!(e.contains("no stream in this box"), "unexpected trap: {e}"),
        other => panic!("expected a trap, got {other:?}"),
    }
    // And an address that HELD a stream is empty once it is taken out, which
    // is what makes a second release a trap rather than a second owner.
    let src = "fn tick(sl: Int64, gn: Int64, cl: Bool) -> Option<Int64> { return None } \
                   fn main() -> Int64 { let a = boxStream(fromStep(0, 1, tick)) \
                     let s: Stream<Int64> = unboxStream(a) close(s) \
                     let t: Stream<Int64> = unboxStream(a) close(t) return 0 }";
    match run(src) {
        Err(e) => assert!(e.contains("no stream in this box"), "unexpected trap: {e}"),
        other => panic!("expected a trap, got {other:?}"),
    }
}

#[test]
fn every_released_stream_asks_its_step_to_close_exactly_once() {
    // RFC-0075's "10 000 open-then-abandon cycles" row, as the engine can see
    // it: the release is the step's closing call, so counting those counts
    // releases. A `close` that did not run would leave the count short and a
    // double release would run it over.
    let src = "let mut closed = 0 \
                   fn tick(sl: Int64, gn: Int64, cl: Bool) -> Option<Int64> { \
                   if cl { closed = closed + 1 return None } return Some(sl) } \
                   fn main() -> Int64 { let mut i = 0 \
                     while i < 100000 { let s = fromStep(i, 1, tick) close(s) i = i + 1 } \
                     return closed }";
    assert_eq!(run(src), Ok(100000));
}
