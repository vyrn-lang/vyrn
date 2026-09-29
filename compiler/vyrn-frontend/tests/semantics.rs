//! The language's own rules: each test is a small program and the answer it must give,
//! run through the wasm backend in the driver's WASI host, the route `vyrn run` takes.
//!
//! An integration test for the reason `loader_run.rs` states: running needs
//! `vyrn-codegen` and the driver's host, and a unit test reaching for them compiles a
//! second copy of this crate.

use std::collections::HashMap;
use vyrn_frontend::project::Memo;

mod common;
use common::run_compiled;

/// The whole standard library behind a `std` root. Builtins are calls into
/// `std/runtime`, and the loader injects the modules a program's builtins imply, so the
/// resolver must answer for any of them. Listed, not walked: `include_str!` takes a literal.
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

/// Loads `source` as a one-file program with `std/` behind it, checks it, and runs it.
///
/// `main`'s value comes back through stdout, not the exit code, which a process
/// truncates to a byte: the test's `main` is renamed and a wrapper prints its answer. A
/// trapping program traps before the print and returns `Err` with the trap's wording.
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
    // The compile scope `vyrn run` opens before its load: the checker types a
    // `schemaOf<T>()` literal inside it; outside it the call has no row.
    // `load_warned`, not `loader::load`, because it synthesizes validated types'
    // constructors and JSON codecs, as the CLI does.
    let (program, memo) = Memo::load(|| {
        vyrn_lower::load_warned(
            &wrapped,
            "main.vyrn",
            &opts,
            &vyrn_frontend::loader::MapResolver(files),
        )
        .0
    })
    .map_err(|ds| {
        ds.iter().map(|d| d.render()).collect::<Vec<_>>().join(
            "
",
        )
    })?;
    let bytes = vyrn_codegen::direct::compile(&program, &memo)?;
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

/// The same program with one module missing from the standard library.
///
/// Not an empty resolver: `std/runtime` holds every builtin's body, so a program with no
/// std root is refused for fifty names first. Removing only the module under test makes
/// the refusal name it.
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
    let (program, memo) = Memo::load(|| {
        vyrn_lower::load_warned(
            source,
            "main.vyrn",
            &opts,
            &vyrn_frontend::loader::MapResolver(files),
        )
        .0
    })
    .map_err(|ds| {
        ds.iter().map(|d| d.render()).collect::<Vec<_>>().join(
            "
",
        )
    })?;
    run_compiled(&program, &memo)
}

fn run_json(source: &str) -> Result<i64, String> {
    run(source)
}

/// The one directory this binary's guests can see, made the process's working directory
/// by the first test that runs.
///
/// WASI gives a module one preopened directory, the host's working directory
/// (`wasmrun`), so a guest cannot open an absolute path; the file rows name files
/// relatively.
fn scratch() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("vyrn-semantics-{}", std::process::id()));
        std::fs::create_dir_all(&d).expect("scratch directory");
        std::env::set_current_dir(&d).expect("scratch is the working directory");
        d
    })
}

/// A scratch file for the file I/O rows, named for the test so two cannot
/// collide. Relative, because that is what the guest can open.
fn temp_path(tag: &str) -> String {
    scratch();
    format!("vyrn-io-test-{tag}.txt")
}

/// `renameFile` atomically overwrites an existing target and consumes the source: after
/// it, the target holds the new content and no source (or `.tmp`) remains.
#[test]
fn rename_file_over_existing_replaces() {
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

/// The atomic write (`writeAtomic`, the body `std/storage` ships) writes `<path>.tmp`
/// then renames it over `path`, leaving no `.tmp` sibling.
#[test]
fn write_atomic_replaces_and_leaves_no_tmp() {
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

/// When the temp write fails, `writeAtomic` never touches `path`, so the target is
/// unchanged. The temp write fails because `<path>.tmp` is a directory.
#[test]
fn write_atomic_failed_temp_leaves_target_unchanged() {
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
    // ...and the original target is untouched.
    assert_eq!(std::fs::read_to_string(path.as_str()).unwrap(), "ORIGINAL");
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_file(path.as_str());
}

/// `loadOr(TypeName, path, default)` returns the default for a missing OR a
/// corrupt file, and the decoded value for a good one.
#[test]
fn load_or_defaults() {
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

/// `stringFromBytes` is checked by `std/text`'s `stringFault`, so a program built with no
/// std root cannot make a `String` from bytes and says which module is missing. The
/// roundtrip and refusal wordings are pinned by `tests/boundaries/string-nul.vyrn`,
/// `string-utf8.vyrn` and `tests/text.rs`.
#[test]
fn string_from_bytes_names_the_module_its_check_lives_in() {
    let src = "fn main() -> Int64 { let b: Array<UInt8> = [104, 105] \
                   return match stringFromBytes(b) { Ok(s) => 1, Err(e) => 0 } }";
    let e = run_without("std/text.vyrn", src).unwrap_err();
    // The backend's wording names the call and the module its check is written in.
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
fn fixed_array_out_of_bounds_errors() {
    let src = "fn main() -> Int64 { let a: Array<Int64, 2> = [1, 2]; return a[4]; }";
    assert!(run(src).unwrap_err().contains("out of bounds"));
}

#[test]
fn array_index_out_of_bounds_errors() {
    let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
                   a.push(1); return a[3]; }";
    assert!(run(src).unwrap_err().contains("out of bounds"));
}

#[test]
fn for_over_empty_array_runs_zero_times() {
    let src = "fn main() -> Int64 { let a: Array<Int64> = []; \
                   let mut s = 7; for x in a { s = s + x; } return s; }";
    assert_eq!(run(src).unwrap(), 7);
}

#[test]
fn for_loop_variable_is_scoped_to_body() {
    // `x` does not leak past the loop: referencing it after is unbound.
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
fn float_through_function_and_negation() {
    let src = "fn half(x: Float64) -> Float64 { return x / 2.0; } \
                   fn main() -> Int64 { let h = half(5.0); \
                   if h == 2.5 { if -h < 0.0 { return 7; } } return 0; }";
    assert_eq!(run(src).unwrap(), 7);
}

#[test]
fn int_to_int32_wraps_and_back() {
    // 5_000_000_000 wraps into i32 to 705032704; Int(..) sign-extends it back.
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
fn uint_comparison_is_unsigned() {
    // As unsigned, 10e18 (above i64::MAX, stored as a negative i64) is greater than 5;
    // a signed comparison would rank it below.
    let src = "fn main() -> Int64 { let big: UInt64 = 10000000000000000000; \
                   let small: UInt64 = 5; if big > small { return 1; } return 0; }";
    assert_eq!(run(src).unwrap(), 1);
}

#[test]
fn rejects_mixing_different_int_widths() {
    let src = "fn main() -> Int64 { let a: Int32 = 1; let b: Int8 = 2; let c = a + b; return 0; }";
    assert!(run(src).unwrap_err().contains("matching numeric"));
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
                       let u = schemaOf<Username>()\n\
                       let e = schemaOf<Even>()\n\
                       let b = schemaOf<Byte>()\n\
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

/// The target is a type argument, so a name that is not a type is refused with the
/// answer every type spelling gets.
#[test]
fn schema_of_rejects_a_non_type() {
    let src = "fn main() -> Int64 { let x = 5; let s = schemaOf<x>(); return 0; }";
    assert!(run(src).unwrap_err().contains("unknown type `x`"));
}

#[test]
fn string_ordering_is_bytewise_lexicographic() {
    // `< <= > >=` on Strings compare byte order, not collation. Each returns 1
    // when the ordering holds. Covers prefixes, empties, equality, and a multibyte case
    // where byte order puts "e-acute" (0xC3..) after "z" (0x7A).
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
fn string_index_out_of_bounds_traps() {
    let src = "fn main() -> Int64 { let s = \"hi\"; return Int64(s[5]); }";
    assert!(run(src).unwrap_err().contains("out of bounds"));
}

#[test]
fn byte_literal_is_its_byte_value() {
    // A byte literal evaluates to its byte, as an integer value.
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

/// A bare source with no resolver has no `std/codecs` in the link, so the diagnostic
/// says where the name lives rather than that it is missing.
#[test]
fn a_moved_builtin_without_a_std_root_names_its_module() {
    let e = run("fn main() -> Int64 { return hexEncode(\"hi\").byteLength }").unwrap_err();
    assert!(e.contains("`hexEncode` is `std/codecs`'s"), "{e}");
}

/// `save` desugars to `writeAtomic`. A module that never imported it is told
/// about the spelling it wrote, `save`, and the import that fixes it.
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
fn nonfinite_hole_interpolation_traps_at_runtime() {
    // A plain-String hole is not finite, so no static proof: an invalid value produced
    // at runtime traps with the canonical message.
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

/// The annotation's own boundary, with a value the checker cannot prove.
///
/// Separate from the boundary sweep below, whose second assertion stores into a binding
/// of the same type: a case behind a failing assertion witnesses nothing. `core::Builder`
/// names a `let` by the type of its value; a screen that read `Int64` for `Age` skipped
/// the check and returned 5.
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

/// A field store is a validated boundary: a runtime value written through a
/// record field into a validated element type traps. Literals are folded at compile time,
/// so only a runtime value reaches the check.
#[test]
fn a_field_store_validates_like_every_other_boundary() {
    let head = "type Age = Int64 where value >= 18 \
                    type T = { xs: Array<Age> } \
                    fn rt(n: Int64) -> Int64 { return n - 1 } ";
    // `t.xs.push(v)`: the in-place append fast path.
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
    // Zod/ArkType-style inline `where` on fields: valid values flow through...
    let ok = "type User = { name: String where value.byteLength >= 3, \
                                age: Int64 where value >= 18 } \
                  fn mk(n: Int64) -> User { return User { name: \"ada\", age: n } } \
                  fn main() -> Int64 { let u = mk(33) return u.age }";
    assert_eq!(run(ok).unwrap(), 33);
    // ...a dynamic violation traps with the synthetic field-type name...
    let bad = "type User = { age: Int64 where value >= 18 } \
                   fn mk(n: Int64) -> User { return User { age: n } } \
                   fn main() -> Int64 { let u = mk(5) return 0 }";
    assert_eq!(run(bad).unwrap_err(), "validation failed for `User.age`");
    // ...and a provably-bad constant is rejected at compile time.
    let constant = "type User = { age: Int64 where value >= 18 } \
                        fn main() -> Int64 { let u = User { age: 5 } return 0 }";
    assert!(run(constant)
        .unwrap_err()
        .contains("does not satisfy `User.age`"));
}

#[test]
fn float_refined_type_constructs_and_rejects_at_runtime() {
    // Refinements over a Float base run under the runtime evaluator, valid values
    // included.
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
    // i64::MAX + 1 wraps to i64::MIN in both backends, in any cargo profile (a bare `+`
    // panics in a debug build).
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
    // `MIN % -1 == 0`: no trap, unlike `MIN / -1`.
    let src = "fn main() -> Int64 { \
                       let m = -9223372036854775808 \
                       let mut d = 0 - 1 \
                       return m % d }";
    assert_eq!(run(src).unwrap(), 0);
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
fn break_exits_only_the_inner_of_nested_loops() {
    // Inner breaks immediately; outer runs 3 times adding 1 each, giving 3.
    let src = "fn main() -> Int64 { \
                   let mut n = 0 \
                   for a in [0, 1, 2] { \
                       for b in [0, 1, 2] { break n = n + 100 } \
                       n = n + 1 } \
                   return n }";
    assert_eq!(run(src).unwrap(), 3);
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
fn continue_under_a_region_still_frees_the_region() {
    // A region inside the loop body, exited early by `continue` every other iteration,
    // must release its depth: 100 iterations stay under the 64-region cap.
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
    // `value + 1 != 0` at i64::MAX wraps to MIN, which is not 0, so the predicate holds
    // in both backends.
    let src = "type T = Int64 where value + 1 != 0; \
                   fn mk(x: Int64) -> T { return T(x); } \
                   fn main() -> Int64 { let t = mk(9223372036854775807); return 0; }";
    assert_eq!(run(src).unwrap(), 0);
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

#[test]
fn log_level_requires_a_logger() {
    // Calling a level on a non-Logger is rejected.
    let src = "fn main() -> Int64 { info(\"notalogger\", \"x\"); return 0; }";
    assert!(run(src).is_err());
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
fn unknown_sink_is_rejected() {
    let src = "logging { sink: syslog } fn main() -> Int64 { return 0; }";
    assert!(run(src).unwrap_err().contains("sink"));
}

#[test]
fn file_sink_needs_a_string_path() {
    let src = "logging { sink: file(main) } fn main() -> Int64 { return 0; }";
    assert!(run(src).is_err());
}

/// The native arena runtime has a fixed 64-slot region stack and traps on a 65th nested
/// region; depth accumulates across calls.
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
    // 64 nested regions fill the stack exactly.
    assert_eq!(run(&src(64)).unwrap(), 0);
    // The 65th traps, wording shared with the native runtime.
    assert_eq!(run(&src(65)).unwrap_err(), "region nesting exceeds 64");
}

#[test]
fn index_store_out_of_bounds_traps() {
    let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 3]; a[5] = 9; return 0; }";
    assert_eq!(run(src).unwrap_err(), "array index 5 out of bounds");
}

#[test]
fn pop_on_empty_is_none() {
    let src = "fn main() -> Int64 { let mut a: Array<Int64> = [5]; \
                   let p1 = a.pop(); let p2 = a.pop(); \
                   return match p2 { Some(x) => x, None => -1 }; }";
    assert_eq!(run(src).unwrap(), -1);
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
fn index_field_write_through_traps_on_oob_load() {
    // The bounds check on the element LOAD fires with the canonical wording.
    let src = "type P = { x: Int64 } \
                   fn main() -> Int64 { \
                       let mut a: Array<P> = [P { x: 1 }] \
                       a[5].x = 9 \
                       return 0 }";
    assert_eq!(run(src).unwrap_err(), "array index 5 out of bounds");
}

const TWICE: &str = "fn twice(xs: Array<Int64>, f: fn(Int64) -> Int64) -> Array<Int64> {\n\
         let mut out: Array<Int64> = []\n\
         for x in xs { out.push(f(x)) }\n\
         return out }\n\
         fn sum(xs: Array<Int64>) -> Int64 {\n\
             let mut s = 0  for x in xs { s = s + x }  return s }\n";

#[test]
fn passthrough_and_empty_array() {
    let src = format!(
            "{TWICE}fn outer(xs: Array<Int64>, g: fn(Int64) -> Int64) -> Array<Int64> {{ return twice(xs, g) }}\n\
             fn main() -> Int64 {{ let e: Array<Int64> = []  let z = sum(outer(e, x -> x + 1))\n\
             let bump = 5  return z + sum(outer([1, 2], x -> x + bump)) }}"
        );
    // empty gives 0; outer([1,2], +5) gives [6,7], summing to 13.
    assert_eq!(run(&src).unwrap(), 13);
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
    // A directly fn-typed module-state binding and its init order:
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
fn stored_lambda_coerces_arguments_to_signature_types() {
    // The declared slot type supplies the parameter coercions: a UInt8
    // parameter wraps exactly as a named callee's would.
    let src = "fn main() -> Int64 { let f: fn(UInt8) -> Int64 = b -> Int64(b + 200)\n\
             return f(100) }";
    // 100 + 200 wraps at the UInt8 parameter's width: 300 & 0xFF = 44.
    assert_eq!(run(src).unwrap(), 44);
}

/// A stream `map`, spelled as `std/stream` spells it: no `for ... in`, a step that reads
/// its source with `pullAt`, and a wrapper owning the box the source moved into. Its
/// closing call takes the source back out and closes it.
const LMAP: &str = "fn lmap<T, U>(s: Stream<T>, f: fn(T) -> U) -> Stream<U> { \
                        let a = boxStream(s) \
                        let g: fn(T) -> U = f \
                        let step: fn(Int64, Int64, Bool) -> Option<U> = (sl, gn, cl) -> { \
                        if cl { let src: Stream<T> = unboxStream(a) close(src) return None } \
                        let x: Option<T> = pullAt(a) \
                        if let Some(v) = x { return Some(g(v)) } return None } \
                        return fromStep(0, 1, step) } ";

#[test]
fn releasing_a_chain_closes_one_stream_per_link() {
    // Three wrappers over one producer: four streams, four releases, each wrapper
    // closing its own source. Each `unboxStream` empties its box, so a double release
    // traps and a missed one leaves the count short: 10 000 cycles, four closes each,
    // and `closed` counts the producer's own closing call.
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
    // `pullAt` is a builtin and an address is an ordinary `Int64`, so a program can
    // call it on a number. The wording is the compiled backends'.
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
fn a_box_address_past_the_32_bit_range_traps_rather_than_wrapping() {
    // The address is an `Int64`; `h + 2^32` names no box, although its low 32 bits
    // are `h`'s.
    let src = "fn main() -> Int64 { let h = boxStream(fromArray([7, 8])) \
                 let x: Option<Int64> = pullAt(h + 4294967296) \
                 let s: Stream<Int64> = unboxStream(h) close(s) return 0 }";
    match run(src) {
        Err(e) => assert!(e.contains("no stream in this box"), "unexpected trap: {e}"),
        other => panic!("expected a trap, got {other:?}"),
    }
}

#[test]
fn a_byte_range_past_the_32_bit_range_traps_rather_than_wrapping() {
    let src = "fn main() -> Int64 { let b = bytes(\"abc\", 4294967296, 4294967297) \
                 return b.length }";
    match run(src) {
        Err(e) => assert!(e.contains("out of bounds"), "unexpected trap: {e}"),
        other => panic!("expected a trap, got {other:?}"),
    }
}

#[test]
fn every_released_stream_asks_its_step_to_close_exactly_once() {
    // 10 000 open-then-abandon cycles: the release is the step's
    // closing call, so counting those counts releases. A missed `close` leaves the count
    // short; a double release runs it over.
    let src = "let mut closed = 0 \
                   fn tick(sl: Int64, gn: Int64, cl: Bool) -> Option<Int64> { \
                   if cl { closed = closed + 1 return None } return Some(sl) } \
                   fn main() -> Int64 { let mut i = 0 \
                     while i < 100000 { let s = fromStep(i, 1, tick) close(s) i = i + 1 } \
                     return closed }";
    assert_eq!(run(src), Ok(100000));
}
