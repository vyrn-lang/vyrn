//! Five ways the compiler used to DIE, and the limits that make each one say
//! something instead (audit A5.2, A5.3, A5.4; review G4.3, G4.4).
//!
//! Every one of them ended with no `file:line` and nothing a Vyrn program could
//! observe: a Rust stack overflow aborts, clang runs out of memory, a wasm module
//! traps at a wild address — and none of those is a diagnostic. The shape of the
//! fix is the same in all five — count the thing that grows, declare a number,
//! and refuse at it — and so is the bar: a message that names the cause, and an
//! exit code the compiler's other refusals use.
//!
//! The numbers are the language's, not this file's; they live beside the code
//! that enforces them and are read from there, so a test cannot pin a limit the
//! compiler no longer takes.

use std::process::{Command, Output};

fn run(cmd: &str, src: &str, name: &str) -> Output {
    run_args(cmd, src, name, &[])
}

/// [`run`] with extra arguments after the file — `--target wasm -o <path>`, the
/// only build this file makes, because the frame limit is the wasm backend's.
fn run_args(cmd: &str, src: &str, name: &str, args: &[String]) -> Output {
    let dir = std::env::temp_dir().join("vyrn-limits");
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join(format!("{name}.vyrn"));
    std::fs::write(&f, src).unwrap();
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg(cmd)
        .arg(&f)
        .args(args)
        .output()
        .unwrap()
}

/// `vyrn build --target wasm` of `src`, giving the output and the module path.
fn build_wasm(src: &str, name: &str) -> (Output, std::path::PathBuf) {
    let out = std::env::temp_dir()
        .join("vyrn-limits")
        .join(format!("{name}.wasm"));
    // Removed first, so "the refused build left no module" is about this run.
    let _ = std::fs::remove_file(&out);
    let o = run_args(
        "build",
        src,
        name,
        &[
            "--target".into(),
            "wasm".into(),
            "-o".into(),
            out.display().to_string(),
        ],
    );
    (o, out)
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
    let limit = vyrn_frontend::trap::CALL_DEPTH_LIMIT;
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
///
/// Both programs write `.copy()` where a `read` parameter reaches a literal,
/// which is RFC-0089 rule 2 and not a concession to the limit: `mk` would
/// otherwise give one buffer two owners. `examples/polyrecursion.vyrn` is the
/// same program with the same fix and the comment that names the rule; these
/// two were its stale copies (RFC-0125 §3 M3, the
/// by-default sweep, the programs tests write).
#[test]
fn polymorphic_recursion_is_refused_by_check_and_by_the_backends() {
    let spine =
        "fn f<T>(x: T, n: Int64) -> Int64 {\n    if n <= 0 {\n        return 0\n    }\n    \
                 let xs: Array<T> = [x.copy()]\n    return f(xs, n - 1)\n}\n\n\
                 fn main() -> Int64 {\n    print(\"\\{f(1, 3)}\")\n    return 0\n}\n";
    let record = "type P<T> = { a: T, b: T }\n\n\
                  fn mk<T>(x: T) -> P<T> {\n    return P { a: x.copy(), b: x.copy() }\n}\n\n\
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
///
/// `[x.copy(), x.copy()]` for rule 2 again, and here the array would hold the
/// caller's buffer twice over (RFC-0125 §3 M3, the
/// by-default sweep, the programs tests write).
#[test]
fn an_ordinary_generic_still_compiles() {
    let src = "fn twice<T>(x: T) -> Array<T> {\n    return [x.copy(), x.copy()]\n}\n\n\
               fn main() -> Int64 {\n    let a = twice(1)\n    let b = twice(\"s\")\n    \
               print(\"\\{a.length}\\{b.length}\")\n    return 0\n}\n";
    let out = run("run", src, "genok");
    assert_eq!(text(&out).trim(), "22", "{}", text(&out));
    assert_eq!(out.status.code(), Some(0));
}

// -------------------------------------------------------------------------
// G4.4 — the array literal
// -------------------------------------------------------------------------

/// `vyrn check` said `ok` in 0.1 s about a literal native could not build.
///
/// 100,000 constant elements lower to 100,000 chained `insertvalue`
/// instructions over an aggregate of the full width, and clang's `-O2` pipeline
/// allocated until it died: `LLVM ERROR: out of memory`, after 2 m 53 s on the
/// machine this was written on. The same file compiled to wasm in 0.1 s and
/// trapped `out of bounds memory access` on its first statement, so it ran on no
/// compiled backend at all, and `check` predicted neither.
///
/// Refused in the checker, which is why all four commands below refuse it: a
/// literal's length is a compile-time cost in both backends, and the one place
/// that can say so before either of them starts is the front end.
#[test]
fn an_array_literal_past_the_limit_is_a_diagnostic_not_a_two_minute_crash() {
    let limit = vyrn_frontend::trap::ARRAY_LIT_LIMIT;
    let n = limit + 1;
    let elems: Vec<String> = (0..n).map(|i| (i % 97).to_string()).collect();
    let src = format!(
        "fn main() -> Int64 {{\n    let xs: Array<Int64> = [{}]\n    \
         print(\"\\{{xs.length}}\")\n    return 0\n}}\n",
        elems.join(", ")
    );
    // Every command that reads a program, including the two that used to hand it
    // to a backend and wait.
    for cmd in ["check", "run", "emit-ir", "build"] {
        let (out, _) = if cmd == "build" {
            build_wasm(&src, "biglit")
        } else {
            (run(cmd, &src, "biglit"), Default::default())
        };
        let got = text(&out);
        assert!(
            got.contains(&format!(
                "this array literal has {n} elements, past the limit of {limit}"
            )),
            "{cmd}: expected the literal limit, got:\n{got}"
        );
        // Source-anchored: `file:line:col`, like every other front-end refusal.
        assert!(
            got.contains("biglit.vyrn:2:"),
            "{cmd}: the diagnostic must name the file and position, got:\n{got}"
        );
        assert_eq!(
            out.status.code(),
            Some(1),
            "{cmd}: a refusal exits 1 — got:\n{got}"
        );
    }
}

/// The control. A literal AT the limit still checks, runs and builds — a bound
/// that refused the ordinary case would pass the test above and be worthless.
/// This is also what pins the two limits to each other: the largest literal the
/// checker admits has to fit in a frame the backend admits, in the same
/// function, together with the array it becomes.
#[test]
fn an_array_literal_at_the_limit_still_runs() {
    let limit = vyrn_frontend::trap::ARRAY_LIT_LIMIT;
    let elems: Vec<String> = (0..limit).map(|i| (i % 97).to_string()).collect();
    let src = format!(
        "fn main() -> Int64 {{\n    let xs: Array<Int64> = [{}]\n    \
         print(\"\\{{xs.length}}\")\n    return 0\n}}\n",
        elems.join(", ")
    );
    let out = run("run", &src, "litok");
    assert_eq!(text(&out).trim(), limit.to_string(), "{}", text(&out));
    assert_eq!(out.status.code(), Some(0));
    let (build, _) = build_wasm(&src, "litok");
    assert!(
        build.status.success(),
        "a literal at the limit must still build:\n{}",
        text(&build)
    );
}

// -------------------------------------------------------------------------
// G4.3 — the call frame
// -------------------------------------------------------------------------

/// A frame the shadow stack cannot hold at every allowed depth built silently.
///
/// The wasm backend's whole stack was one 64 KB page and nothing compared a
/// frame against it, so a body with more locals than that compiled in a tenth of
/// a second into a module whose first statement trapped
/// `out of bounds memory access` at address `0xffe89600` — no build error, no
/// position, nothing a reader could act on.
///
/// The refusal names the function and its line, like the instantiation limit
/// beside it: the size is the sum of that function's own locals, and its author
/// is the one who can make them smaller.
#[test]
fn a_frame_that_cannot_fit_is_a_diagnostic_not_a_module_that_traps() {
    // Two 4 KB records, so the frame is past the limit by a whole record and no
    // rounding decides the outcome.
    let src = "type K8 = { a: Int64, b: Int64, c: Int64, d: Int64, e: Int64, f: Int64, \
               g: Int64, h: Int64 }\n\
               type K64 = { a: K8, b: K8, c: K8, d: K8, e: K8, f: K8, g: K8, h: K8 }\n\
               type K512 = { a: K64, b: K64, c: K64, d: K64, e: K64, f: K64, g: K64, h: K64 }\n\n\
               fn mk8(n: Int64) -> K8 {\n    \
               return K8 { a: n, b: n, c: n, d: n, e: n, f: n, g: n, h: n }\n}\n\n\
               fn mk64(n: Int64) -> K64 {\n    let q = mk8(n)\n    \
               return K64 { a: q, b: q, c: q, d: q, e: q, f: q, g: q, h: q }\n}\n\n\
               fn mk512(n: Int64) -> K512 {\n    let q = mk64(n)\n    \
               return K512 { a: q, b: q, c: q, d: q, e: q, f: q, g: q, h: q }\n}\n\n\
               fn wide(n: Int64) -> Int64 {\n    let one = mk512(n)\n    let two = mk512(n + 1)\n    let three = mk512(n + 2)\n    \
               return one.a.a.h - two.b.b.h + three.c.c.h\n}\n\n\
               fn main() -> Int64 {\n    print(\"\\{wide(3)}\")\n    return 0\n}\n";
    let (out, module) = build_wasm(src, "bigframe");
    let got = text(&out);
    assert!(
        got.contains(vyrn_codegen::FRAME_LIMIT_NEEDLE)
            && got.contains(&format!(
                "past the frame limit of {}",
                vyrn_frontend::trap::FRAME_LIMIT
            )),
        "expected the frame limit, got:\n{got}"
    );
    assert!(
        got.contains("`wide` needs") && got.contains("`wide` is declared on line 19"),
        "the refusal must name the function and its line, got:\n{got}"
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "a refusal exits 1 — got:\n{got}"
    );
    assert!(
        !module.exists(),
        "a refused build must not leave a module behind"
    );
}

// -------------------------------------------------------------------------
// The shape that does not fit in the number describing it
// -------------------------------------------------------------------------

/// A fixed array bigger than the address space was measured with wrapping
/// arithmetic, so the compiler believed a small number about a huge shape.
///
/// `Array<T, N>` takes any non-negative literal — the parser accepts it and the
/// checker never bounds it, unlike `SmallArray`'s 1..=64 — and `layout.rs`
/// multiplied `N` by the element size in `u32`. Debug panicked on the multiply,
/// with no `file:line` and nothing a program could observe. Release wrapped, and
/// the wrapped figure was what every bound downstream then compared: the frame
/// limit refused `Array<Int64, 600000000>` while REPORTING 505,032,704 bytes for
/// a 4.8 GB shape.
///
/// The exact-multiple case is why the refusal has to be at the measurement.
/// `536870912 * 8` is 2^32, so the wrap is ZERO: a 4 GiB array that claimed to
/// need no stack at all passed the frame limit and got a module written for it.
/// That one is a silent miscompile, and the number it turns on is the element
/// count — nothing about the program looks wrong.
#[test]
fn a_shape_past_the_address_space_is_a_diagnostic_not_a_wrapped_number() {
    let cases = [
        // A parameter, which is where a big fixed array is cheapest to write.
        (
            "hugeparam",
            "fn sum(xs: Array<Int64, 600000000>) -> Int64 {\n    return xs[0]\n}\n\n\
             fn main() -> Int64 {\n    return 0\n}\n",
            "4800000000",
        ),
        // The wrap that lands on zero.
        (
            "wrapstozero",
            "fn sum(xs: Array<Int64, 536870912>) -> Int64 {\n    return xs[0]\n}\n\n\
             fn main() -> Int64 {\n    return 0\n}\n",
            "4294967296",
        ),
        // Nested, where the product is far past 64 bits' worth of plausibility
        // and each factor on its own is unremarkable.
        (
            "hugefield",
            "type Grid = { cells: Array<Array<Int64, 100000>, 100000> }\n\n\
             fn first(g: Grid) -> Int64 {\n    return g.cells[0][0]\n}\n\n\
             fn main() -> Int64 {\n    return 0\n}\n",
            "80000000000",
        ),
    ];
    for (name, src, bytes) in cases {
        let (out, module) = build_wasm(src, name);
        let got = text(&out);
        assert!(
            got.contains(&format!("needs {bytes} bytes")),
            "{name}: the refusal must state the TRUE size, got:\n{got}"
        );
        assert!(
            got.contains("past the 4294967295 one shape may occupy")
                && got.contains("belongs on the heap as `Array<T>`"),
            "{name}: the refusal must name the limit and the remedy, got:\n{got}"
        );
        assert!(
            got.contains("at line "),
            "{name}: the refusal must carry a source position, got:\n{got}"
        );
        assert_eq!(
            out.status.code(),
            Some(1),
            "{name}: a refusal exits 1:\n{got}"
        );
        assert!(
            !module.exists(),
            "{name}: a refused build left a module behind"
        );
    }
}

/// A shape under the bound is still measured, and still refused by the bound it
/// actually breaks.
///
/// Without this the test above passes on a compiler that refuses every fixed
/// array, which is a different bug wearing the same diagnostic. Both of these
/// are too big for a frame and neither is too big to DESCRIBE, so the answer has
/// to come from the frame limit — naming the function, which is the diagnostic
/// that was always meant to fire here.
#[test]
fn a_shape_under_the_bound_is_still_measured() {
    for (name, n, bytes) in [
        // 8 MB: nowhere near the address space, and reported to the byte.
        (
            "undersize",
            1_000_000usize,
            Some("`sum` needs 8000000 bytes"),
        ),
        // 4 GB minus 8, the largest `[N x i64]` there is. The frame counter
        // clamps at the top of its own range rather than wrapping, so the figure
        // is the clamp and not the shape; what matters is which limit answers.
        ("justunder", 536_870_911, None),
    ] {
        let src = format!(
            "fn sum(xs: Array<Int64, {n}>) -> Int64 {{\n    return xs[0]\n}}\n\n\
             fn main() -> Int64 {{\n    return 0\n}}\n"
        );
        let got = text(&build_wasm(&src, name).0);
        assert!(
            got.contains(vyrn_codegen::FRAME_LIMIT_NEEDLE),
            "{name}: the frame limit is the bound this breaks, got:\n{got}"
        );
        assert!(
            !got.contains("one shape may occupy"),
            "{name}: the layout engine can describe this and must not refuse it, got:\n{got}"
        );
        if let Some(exact) = bytes {
            assert!(got.contains(exact), "{name}: expected {exact}, got:\n{got}");
        }
    }
}

// -------------------------------------------------------------------------
// The statics
// -------------------------------------------------------------------------

/// A program with more static data than the module can hold killed the compiler.
///
/// The limit was already written down and already compared — as `assert!`, in
/// `Module::finish`, which returned `Vec<u8>` while its only caller returned
/// `Result<_, String>`. So the number was right and the refusal was a Rust panic:
/// no `error:` line, no exit code the other refusals use, and nothing naming the
/// program. A big i18n catalogue, a generator's output or any large corpus of
/// literals reached it.
///
/// Ninety DISTINCT literals, because `Module::data` shares identical contents —
/// ninety copies of one string are one address and 100 KB, and the first attempt
/// at this row built fine for exactly that reason.
///
/// The control lives with the code, in `wasm.rs`: a module whose statics end
/// exactly on the line still finishes. It is there rather than here because the
/// build at the limit is an 8 MB module to link, and this row is about the
/// refusal reaching the user.
#[test]
fn statics_past_what_the_module_holds_are_a_diagnostic_not_a_panic() {
    let mut src = String::from("fn main() -> Int64 {\n");
    for i in 0..90 {
        src.push_str(&format!("    print(\"s{i}-{}\")\n", "q".repeat(100_000)));
    }
    src.push_str("    return 0\n}\n");

    let (out, module) = build_wasm(&src, "bigstatics");
    let got = text(&out);
    assert!(
        got.contains(vyrn_codegen::STATICS_LIMIT_NEEDLE),
        "expected the statics limit, got:\n{got}"
    );
    // The numbers are the code's, not this file's — the room is where the data
    // segments start and where the shim's stack does.
    let room = vyrn_codegen::wasm::STATICS_LIMIT - vyrn_codegen::wasm::DATA_BASE;
    assert!(
        got.contains(&format!("past the statics limit of {room}")),
        "the refusal must name the room a module actually has ({room}), got:\n{got}"
    );
    assert!(
        !got.contains("panicked at"),
        "a limit is a sentence, not a Rust panic:\n{got}"
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "a refusal exits 1, like every other one — got:\n{got}"
    );
    assert!(
        !module.exists(),
        "a refused build must not leave a module behind"
    );
}

// -------------------------------------------------------------------------
// One source per limit
// -------------------------------------------------------------------------

/// The limits are one number each, and the others are derived from them.
///
/// This is the half of the defect that outlives any single fix. `CALL_DEPTH_LIMIT`
/// was already shared by all three engines; the shadow stack's size was not
/// related to it at all, and the region-nesting bound was written eight times
/// across three engines — three of those inside string literals, one a
/// hand-counted LLVM array length — with the two backends' comparisons already
/// differing in signedness.
///
/// So the relations are asserted rather than the values: raising `FRAME_LIMIT`
/// moves the stack with it, and a stack sized by hand fails here.
#[test]
fn every_limit_has_one_source() {
    use vyrn_frontend::trap::{ARRAY_LIT_LIMIT, CALL_DEPTH_LIMIT, FRAME_LIMIT, REGION_MAX};
    assert_eq!(
        vyrn_codegen::wasm::STACK_BYTES,
        FRAME_LIMIT * CALL_DEPTH_LIMIT + 65_536,
        "the shadow stack is the product plus one page for the uncounted runtime \
         frames; a stack chosen independently is a depth limit that means a \
         different number on wasm"
    );
    assert_eq!(
        vyrn_codegen::wasm::DATA_BASE,
        vyrn_codegen::wasm::STACK_BYTES,
        "the data segments start where the stack ends, or a frame push walks into them"
    );
    assert_eq!(
        ARRAY_LIT_LIMIT * 16,
        FRAME_LIMIT as usize,
        "the literal bound is HALF the frame bound over the width of an Int64 — the \
         other half is for the array the literal becomes, in the same frame"
    );

    // The region bound, in all three engines' own output. A copy re-written by
    // hand shows up as a different number in exactly one of these.
    let src = "fn main() -> Int64 {\n    region {\n    }\n    return 0\n}\n";
    let msg = format!("error: region nesting exceeds {REGION_MAX}");
    let ir = text(&run("emit-ir", src, "regionsrc"));
    assert!(ir.contains(&msg), "the textual backend's wording:\n{ir}");
    assert!(
        ir.contains(&format!("[{REGION_MAX} x ptr]"))
            && ir.contains(&format!("icmp uge i64 %sp, {REGION_MAX}")),
        "the textual backend's stack width and comparison:\n{ir}"
    );
    let (build, module) = build_wasm(src, "regionsrc");
    assert!(build.status.success(), "{}", text(&build));
    let bytes = std::fs::read(&module).unwrap();
    assert!(
        bytes.windows(msg.len()).any(|w| w == msg.as_bytes()),
        "the direct backend interns the same wording"
    );
    // The interpreter's own copy, from the engine that defines the semantics.
    let deep = format!(
        "fn main() -> Int64 {{\n{}    return 0\n{}}}\n",
        "    region {\n".repeat(REGION_MAX as usize + 1),
        "    }\n".repeat(REGION_MAX as usize + 1)
    );
    let out = run("run", &deep, "regiondeep");
    let got = text(&out);
    assert!(got.contains(&msg), "the interpreter's wording:\n{got}");
    assert_eq!(out.status.code(), Some(1));
}

// -------------------------------------------------------------------------
// A manifest read from an ancestor directory
// -------------------------------------------------------------------------

/// The sixth death, and the only one reachable through a file the user never
/// named.
///
/// `find_manifest` walks UP from the working directory on every command, and the
/// JSON parser it uses had no depth limit. A `vyrn.json` anywhere above the cwd —
/// corrupt, hostile, or simply belonging to a project that is not this one —
/// ended every `vyrn` invocation in a stack overflow: exit 127, nothing on
/// stderr, no file named. The compiler died reading a file it was never asked
/// about.
///
/// The bound is the parser's own, read from where it is enforced.
#[test]
fn a_deeply_nested_manifest_above_the_cwd_is_a_diagnostic_not_an_abort() {
    use vyrn_frontend::schema::MAX_JSON_DEPTH;
    let root = std::env::temp_dir().join(format!("vyrn-limits-manifest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(
        sub.join("main.vyrn"),
        "fn main() -> Int64 {\n    return 0\n}\n",
    )
    .unwrap();

    let manifest = |depth: usize| {
        format!(
            "{{\"main\":\"main.vyrn\",\"x\":{}{}}}",
            "[".repeat(depth),
            "]".repeat(depth)
        )
    };
    let check_in_sub = || {
        Command::new(env!("CARGO_BIN_EXE_vyrn"))
            .arg("check")
            .arg("main.vyrn")
            .current_dir(&sub)
            .output()
            .unwrap()
    };

    // At the limit the manifest is read and the program checks, so the bound is
    // a bound and not a refusal to read manifests. The manifest OBJECT is the
    // first enclosing level, so the arrays inside it may go one less deep.
    std::fs::write(root.join("vyrn.json"), manifest(MAX_JSON_DEPTH - 1)).unwrap();
    let at = check_in_sub();
    assert_eq!(at.status.code(), Some(0), "at the limit:\n{}", text(&at));

    // One level past it, and at a depth that used to abort the process.
    for depth in [MAX_JSON_DEPTH, 200_000] {
        std::fs::write(root.join("vyrn.json"), manifest(depth)).unwrap();
        let out = check_in_sub();
        let got = text(&out);
        assert_ne!(
            out.status.code(),
            Some(127),
            "depth {depth}: a stack overflow is not a diagnostic:\n{got}"
        );
        assert_eq!(
            out.status.code(),
            Some(2),
            "depth {depth}: an unreadable manifest is a refusal:\n{got}"
        );
        assert!(
            got.contains("vyrn.json"),
            "depth {depth}: the diagnostic must name the file it could not read:\n{got}"
        );
        assert!(
            got.contains(&MAX_JSON_DEPTH.to_string()),
            "depth {depth}: and the limit it hit:\n{got}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}
