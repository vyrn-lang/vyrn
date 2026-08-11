//! Three ways the compiler used to DIE, and the three limits that make each one
//! say something instead (audit A5.2, A5.3, A5.4).
//!
//! Every one of them ended the process with no `file:line` and nothing a Vyrn
//! program could observe: a Rust stack overflow aborts, and an abort is not a
//! diagnostic. The shape of the fix is the same in all three — count the thing
//! that grows, declare a number, and refuse at it — and so is the bar: a message
//! that names the cause, and an exit code the compiler's other refusals use.
//!
//! The numbers are the language's, not this file's; they live beside the code
//! that enforces them and are read from there, so a test cannot pin a limit the
//! compiler no longer takes.

use std::process::{Command, Output};

fn run(cmd: &str, src: &str, name: &str) -> Output {
    let dir = std::env::temp_dir().join("vyrn-limits");
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join(format!("{name}.vyrn"));
    std::fs::write(&f, src).unwrap();
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg(cmd)
        .arg(&f)
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).replace("\r\n", "\n")
        + &String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n")
}

// -------------------------------------------------------------------------
// A5.4 — nested source
// -------------------------------------------------------------------------

/// 175,000 nested parentheses aborted the process; 150,000 checked fine. The
/// threshold is far above hand-written code, which is not the point: RFC-0010
/// fetches `github:` and `https:` modules, so the compiler parses source the
/// user did not write, and the LSP parses whatever is on disk.
///
/// Four shapes, because the parser has four recursive edges a file can drive
/// without bound and one counter has to cover all of them.
#[test]
fn deeply_nested_source_is_a_diagnostic_not_an_abort() {
    let deep = 200_000;
    let cases = [
        (
            "parens",
            format!(
                "fn main() -> Int64 {{\n    return {}0{}\n}}\n",
                "(".repeat(deep),
                ")".repeat(deep)
            ),
        ),
        (
            "prefix",
            format!(
                "fn main() -> Int64 {{\n    return {}0\n}}\n",
                "-".repeat(deep)
            ),
        ),
        (
            "types",
            format!(
                "fn main() -> Int64 {{\n    let x: {}Int64{} = 0\n    return 0\n}}\n",
                "Option<".repeat(5000),
                ">".repeat(5000)
            ),
        ),
        (
            "blocks",
            format!(
                "fn main() -> Int64 {{\n{}    return 0\n{}    return 0\n}}\n",
                "    if true {\n".repeat(5000),
                "    }\n".repeat(5000)
            ),
        ),
    ];
    for (what, src) in cases {
        let out = run("check", &src, what);
        let got = text(&out);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{what}: a refusal exits 1, like every other check failure — got:\n{got}"
        );
        assert!(
            got.contains("nesting exceeds 1024 levels"),
            "{what}: expected the nesting limit, got:\n{got}"
        );
        // Source-anchored: the diagnostic carries `file:line:col`, so an editor
        // can put the caret on the level that went too far.
        assert!(
            got.contains(&format!("{what}.vyrn:")),
            "{what}: the diagnostic must name the file and position, got:\n{got}"
        );
    }
}

/// The control. A limit nothing real reaches is only useful if nothing real
/// reaches it — 1,020 levels still parse, run, and compile on every backend.
#[test]
fn source_just_under_the_nesting_limit_still_runs() {
    let n = 1_020;
    let src = format!(
        "fn main() -> Int64 {{\n    print(\"\\{{{}7{}}}\")\n    return 0\n}}\n",
        "(".repeat(n),
        ")".repeat(n)
    );
    let out = run("run", &src, "undernest");
    assert_eq!(text(&out).trim(), "7", "{}", text(&out));
    assert_eq!(out.status.code(), Some(0));
}

// -------------------------------------------------------------------------
// A5.3 — call depth
// -------------------------------------------------------------------------

/// The interpreter is the declared reference semantics, and at depth 30,000 it
/// stopped existing: `thread '<unknown>' has overflowed its stack`, exit 127,
/// while the native binary printed 30000. The catch at `interp.rs` could not
/// fire, because a Rust stack overflow aborts the process rather than unwinding.
///
/// So the depth is the LANGUAGE's, counted by every engine. This pins the
/// interpreter's half; `examples/recdepth.vyrn` pins that all three agree, in
/// the parity harness, byte for byte.
#[test]
fn recursion_past_the_call_depth_limit_is_a_diagnostic() {
    let limit = vyrn_frontend::interp::CALL_DEPTH_LIMIT;
    let src = |n: u32| {
        format!(
            "fn down(n: Int64) -> Int64 {{\n    if n <= 0 {{\n        return 0\n    }}\n    \
             return 1 + down(n - 1)\n}}\n\nfn main() -> Int64 {{\n    \
             print(\"\\{{down({n})}}\")\n    return 0\n}}\n"
        )
    };
    // `main` holds one frame, so `down(limit - 2)` is the deepest run that fits.
    //
    // This half also gates the PROFILE. An unoptimized interpreter frame is ~20x
    // an optimized one, so a limit can fit in release and abort in debug — it
    // did, at 10,000, and CI runs these tests in debug. Naming the profile in
    // the failure turns that into a sentence instead of a stack overflow.
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let ok = run("run", &src(limit - 2), "depthok");
    let got_ok = text(&ok);
    assert_eq!(
        got_ok.trim(),
        (limit - 2).to_string(),
        "the limit is the LANGUAGE's, so EVERY build profile must reach it. This is a \
         {profile} build: if the run above died on the host stack, {limit} is the wrong \
         number, not this test — got:\n{got_ok}"
    );
    assert_eq!(ok.status.code(), Some(0));

    let over = run("run", &src(limit - 1), "depthover");
    let got = text(&over);
    assert!(
        got.contains(&format!("error: call depth exceeds {limit}")),
        "expected the call-depth trap, got:\n{got}"
    );
    assert_eq!(
        over.status.code(),
        Some(1),
        "a trap exits 1, as every other runtime trap does — got:\n{got}"
    );
}

// -------------------------------------------------------------------------
// A5.2 — monomorphization
// -------------------------------------------------------------------------

/// `vyrn check` said `ok` about a program `vyrn build` could not finish. Two
/// shapes, because two different bounds catch them: a spine grows deep and never
/// grows wide, a record grows wide and doubles per level.
///
/// `check` is asserted alongside `emit-ir`, because the defect was as much that
/// `check` did not predict the build as that the build never returned.
#[test]
fn polymorphic_recursion_is_refused_by_check_and_by_the_backends() {
    let spine =
        "fn f<T>(x: T, n: Int64) -> Int64 {\n    if n <= 0 {\n        return 0\n    }\n    \
                 let xs: Array<T> = [x]\n    return f(xs, n - 1)\n}\n\n\
                 fn main() -> Int64 {\n    print(\"\\{f(1, 3)}\")\n    return 0\n}\n";
    let record = "type P<T> = { a: T, b: T }\n\n\
                  fn mk<T>(x: T) -> P<T> {\n    return P { a: x, b: x }\n}\n\n\
                  fn f<T>(x: T, n: Int64) -> Int64 {\n    if n <= 0 {\n        return 0\n    }\n    \
                  return f(mk(x), n - 1)\n}\n\n\
                  fn main() -> Int64 {\n    print(\"\\{f(1, 3)}\")\n    return 0\n}\n";
    for (what, src, why) in [
        ("spine", spine, "nests 65 levels deep, past the limit of 64"),
        (
            "record",
            record,
            "has more than 65536 parts once its records are written out",
        ),
    ] {
        for cmd in ["check", "emit-ir"] {
            let out = run(cmd, src, &format!("mono{what}"));
            let got = text(&out);
            assert!(
                got.contains(vyrn_codegen::MONO_LIMIT_NEEDLE) && got.contains(why),
                "{what}/{cmd}: expected the instantiation limit ({why}), got:\n{got}"
            );
            assert!(
                got.contains("`f` is declared on line"),
                "{what}/{cmd}: the refusal must name the function and its line, got:\n{got}"
            );
            assert_eq!(
                out.status.code(),
                Some(1),
                "{what}/{cmd}: a refusal exits 1 — got:\n{got}"
            );
        }
    }
}

/// The control for the other direction: an ordinary generic, instantiated the
/// ordinary way, still compiles. A limit that refuses everything would pass the
/// test above and be worthless.
#[test]
fn an_ordinary_generic_still_compiles() {
    let src = "fn twice<T>(x: T) -> Array<T> {\n    return [x, x]\n}\n\n\
               fn main() -> Int64 {\n    let a = twice(1)\n    let b = twice(\"s\")\n    \
               print(\"\\{a.length}\\{b.length}\")\n    return 0\n}\n";
    let out = run("run", src, "genok");
    assert_eq!(text(&out).trim(), "22", "{}", text(&out));
    assert_eq!(out.status.code(), Some(0));
}
