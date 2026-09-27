//! The compiler's resource limits. Each counts the thing that grows, declares a
//! number and refuses at it with a message naming the cause and the usual refusal
//! exit code, where the process would otherwise overflow its stack, run out of
//! memory or emit a module that traps at a wild address.
//!
//! The numbers are read from the code that enforces them, so a test cannot pin a
//! limit the compiler does not take.

use std::process::{Command, Output};

fn run(cmd: &str, src: &str, name: &str) -> Output {
    run_args(cmd, src, name, &[])
}

/// [`run`] with extra arguments after the file.
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
    // Removed first, so "a refused build left no module" is about this run.
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

/// The compiler parses source the user did not write (remote
/// modules), and the LSP parses whatever is on disk. Four shapes, because the
/// parser has four recursive edges a file can drive, and one counter covers all.
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
        assert!(
            got.contains(&format!("{what}.vyrn:")),
            "{what}: the diagnostic must name the file and position, got:\n{got}"
        );
    }
}

/// The control: a limit that refused real code would pass the test above.
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

/// The call depth is the language's, counted by every engine;
/// `examples/recdepth.vyrn` pins it in the fixture corpus.
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
    // A debug frame is far larger than a release one and CI runs debug, so the
    // failure names the profile.
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

/// Two shapes, because two bounds catch them: a spine grows deep, a record grows
/// wide and doubles per level. `check` must predict what the build refuses.
///
/// `.copy()` where a `read` parameter reaches a literal is the borrow rule, not
/// a concession to the limit: `mk` would otherwise give one buffer two owners.
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
        for cmd in ["check", "emit-wat"] {
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

/// The control: a limit that refused everything would pass the test above.
/// `.copy()` because `x` is borrowed: the array would hold the caller's buffer twice.
#[test]
fn an_ordinary_generic_still_compiles() {
    let src = "fn twice<T>(x: T) -> Array<T> {\n    return [x.copy(), x.copy()]\n}\n\n\
               fn main() -> Int64 {\n    let a = twice(1)\n    let b = twice(\"s\")\n    \
               print(\"\\{a.length}\\{b.length}\")\n    return 0\n}\n";
    let out = run("run", src, "genok");
    assert_eq!(text(&out).trim(), "22", "{}", text(&out));
    assert_eq!(out.status.code(), Some(0));
}

/// A literal's length is a compile-time cost in both backends, so the front end
/// refuses it, and every command that reads a program refuses it too.
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
    for cmd in ["check", "run", "emit-wat", "build"] {
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

/// The control. It also ties two limits: the largest literal the checker admits
/// must fit in a frame the backend admits, beside the array it becomes.
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

/// A frame the shadow stack cannot hold at every allowed depth would build into
/// a module that traps. The refusal names the function: the size is the sum of
/// its own locals, and its author can shrink them.
#[test]
fn a_frame_that_cannot_fit_is_a_diagnostic_not_a_module_that_traps() {
    // Three 4 KB records: past the limit by a whole record, so no rounding decides.
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

/// `Array<T, N>` takes any non-negative literal, so its size can pass the address
/// space. The refusal sits at the measurement because a wrapped size misleads
/// every bound downstream: `536870912 * 8` is 2^32, which wraps to zero, and a
/// 4 GiB array would pass the frame limit as needing no stack.
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
        // Nested: each factor is unremarkable, the product is not.
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

/// The control: a compiler that refused every fixed array would pass the test
/// above. Both shapes are too big for a frame but not to describe, so the frame
/// limit must answer.
#[test]
fn a_shape_under_the_bound_is_still_measured() {
    for (name, n, bytes) in [
        // 8 MB, reported to the byte.
        (
            "undersize",
            1_000_000usize,
            Some("`sum` needs 8000000 bytes"),
        ),
        // 4 GB minus 8, the largest `[N x i64]`. The frame counter clamps rather
        // than wraps, so the figure is the clamp; only which limit answers matters.
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

/// A large i18n catalogue or generator output can reach this.
///
/// Ninety distinct literals, because `Module::data` shares identical contents:
/// ninety copies of one string are one 100 KB segment. The control lives in
/// `wasm.rs`, because a build at the limit is an 8 MB module to link.
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
    // The room runs from where the data segments start to the statics limit.
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

/// Each limit is one number, and the others derive from it. The relations are
/// asserted, not the values: raising `FRAME_LIMIT` moves the stack with it, and
/// a stack sized by hand fails here.
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

    // The region bound, in the wording the module interns and the wording a run
    // prints. A copy written by hand shows a different number in one of them.
    let src = "fn main() -> Int64 {\n    region {\n    }\n    return 0\n}\n";
    let msg = format!("error: region nesting exceeds {REGION_MAX}");
    let wat = text(&run("emit-wat", src, "regionsrc"));
    assert!(wat.contains(&msg), "the emitter's wording:\n{wat}");
    let (build, module) = build_wasm(src, "regionsrc");
    assert!(build.status.success(), "{}", text(&build));
    let bytes = std::fs::read(&module).unwrap();
    assert!(
        bytes.windows(msg.len()).any(|w| w == msg.as_bytes()),
        "the direct backend interns the same wording"
    );
    // The wording a run prints.
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

/// `find_manifest` walks up from the working directory on every command, so a
/// `vyrn.json` anywhere above the cwd, corrupt or someone else's, reaches the
/// JSON parser through a file the user never named.
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

    // At the limit the manifest is read. The manifest object is the first level,
    // so the arrays inside it go one less deep.
    std::fs::write(root.join("vyrn.json"), manifest(MAX_JSON_DEPTH - 1)).unwrap();
    let at = check_in_sub();
    assert_eq!(at.status.code(), Some(0), "at the limit:\n{}", text(&at));

    // One level past it, and deep enough to overflow an unbounded parser.
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
