//! The layout engine, checked against the compiler that has been laying these
//! shapes out all along (RFC-0077 M0).
//!
//! A wrong offset is not a link error. Both halves of a wasm build agree on the
//! bytes purely by convention — the emitted code writes a `Map` and the C shim's
//! `__vyrn_map_reserve` reads one — so a layout engine that is merely *plausible*
//! produces a program that runs and is wrong. The only ground truth worth having
//! is clang's own answer on the real target, so this asks for it: transcribe
//! every shape in [`vyrn_codegen::layout::SHAPES`] to a C struct, compile the
//! `sizeof`/`offsetof` program for wasm32-wasip1 with the flags `vyrn build
//! --target wasm` uses, run it under wasmtime, and diff.
//!
//! The transcription is mechanical (`ptr` → `void*`, `i64` → `long long`, …)
//! rather than hand-written, because a hand-written C struct is a second chance
//! to make the same mistake in both places. LLVM lays literal structs out with
//! the rule clang uses for plain C structs, which is what makes the comparison
//! sound; where the two languages genuinely differ — C has no `i1` — the note is
//! on the mapping below.
//!
//! Needs clang, a wasi sysroot and wasmtime, so it is in the IGNORED tier: a
//! plain `cargo test` counts it `ignored` (and prints the reason, without
//! `--quiet`) rather than green, and CI's parity job
//! — the one place with all three — runs `cargo test -p vyrn-codegen --
//! --ignored` with `VYRN_REQUIRE_TOOLS=1`, which turns a missing tool into a
//! panic. Before that this was an ordinary `#[test]` that early-`return`ed, so
//! it passed in every job having checked nothing, while `ci.yml` cited it as
//! evidence in its argument about ARM coverage.

use std::path::{Path, PathBuf};
use vyrn_codegen::layout::{of_ll, SHAPES};
use vyrn_codegen::toolchain::require_tools;

/// A wasmtime executable, from `$VYRN_WASMTIME` or the repo's `tools/` — the
/// same lookup `vyrn-cli`'s parity harness does, moved into the crate when
/// RFC-0077 M1's tests needed the second copy.
fn find_wasmtime() -> Option<PathBuf> {
    require_tools(
        "wasmtime",
        "VYRN_WASMTIME",
        vyrn_codegen::toolchain::find_wasmtime_from(Path::new(env!("CARGO_MANIFEST_DIR"))),
    )
}

/// One LLVM type string as a C type expression around the declarator `decl`.
///
/// C's declarator syntax is inside-out, so the type has to be built around the
/// name rather than prepended to it: `[2 x ptr]` is `void* m[2]`, not
/// `void*[2] m`. Recursing with the partially-built declarator is the smallest
/// way to get that right.
fn c_decl(ll: &str, decl: &str) -> String {
    let t = ll.trim();
    if let Some(inner) = t.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
        let members: Vec<String> = split_members(inner)
            .iter()
            .enumerate()
            .map(|(i, m)| format!("{};", c_decl(m, &format!("f{i}"))))
            .collect();
        return format!("struct {{ {} }} {decl}", members.join(" "));
    }
    if let Some(inner) = t.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        let (n, elem) = inner.split_once(" x ").expect("array shape is `[N x T]`");
        return c_decl(elem, &format!("{decl}[{}]", n.trim()));
    }
    // A vector (RFC-0083). C has no vector type, but clang has the extension the
    // whole target is built with, and `vector_size` is what `<N x T>` MEANS to
    // LLVM — the size in bytes, and the alignment it derives from it. Asking
    // clang for `_Alignof` of this is the only reason the 16 in the engine is a
    // measurement rather than a guess.
    if let Some(inner) = t.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
        let (n, elem) = inner.split_once(" x ").expect("vector shape is `<N x T>`");
        let lanes: usize = n.trim().parse().expect("a lane count");
        let bytes = lanes * lane_bytes(elem.trim());
        return format!(
            "{} __attribute__((vector_size({bytes})))",
            c_decl(elem, decl)
        );
    }
    let base = match t {
        // wasm32 is ILP32: `void*` is the 4 bytes `ptr` is.
        "ptr" => "void*",
        "double" => "double",
        "float" => "float",
        // C has no `i1`. `_Bool` is the equivalent for LAYOUT — 1 byte, 1-aligned
        // — which is the only property under test here; nothing about the value
        // representation of a Vyrn `Bool` rides on this.
        "i1" => "_Bool",
        "i8" => "signed char",
        "i16" => "short",
        "i32" => "int",
        "i64" => "long long",
        other => panic!("no C spelling for {other:?}"),
    };
    format!("{base} {decl}")
}

/// A vector lane's width in bytes, which `vector_size` wants and the LLVM
/// spelling states. Written out rather than taken from the engine so that the C
/// side is not derived from the answer under test.
fn lane_bytes(ll: &str) -> usize {
    match ll {
        "float" | "i32" => 4,
        "double" | "i64" => 8,
        other => panic!("no vector lane width for {other:?}"),
    }
}

/// Split a struct body on top-level commas (nested `{}` / `[]` / `<>` do not
/// count).
fn split_members(body: &str) -> Vec<String> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0i32, 0usize);
    for (i, c) in body.char_indices() {
        match c {
            '{' | '[' | '<' => depth += 1,
            '}' | ']' | '>' => depth -= 1,
            ',' if depth == 0 => {
                out.push(body[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    if !body.trim().is_empty() {
        out.push(body[start..].to_string());
    }
    out
}

/// The C program: for every shape, one `printf` per number the engine claims.
/// One number per line, keyed `<shape>|<what>`, because shape names contain
/// spaces and slashes and a mismatch should name itself.
fn c_program() -> String {
    let mut src = String::from("#include <stdio.h>\n#include <stddef.h>\n");
    for (i, (_, ll)) in SHAPES.iter().enumerate() {
        // `typedef` rather than a named struct so the anonymous nested structs
        // `c_decl` produces need no names of their own.
        src.push_str(&format!("typedef {};\n", c_decl(ll, &format!("T{i}"))));
    }
    // The one struct that is not a transcription: the shim's own `VMap`, copied
    // verbatim from RUNTIME_SHIM. `Map<String, V>` is the single aggregate whose
    // bytes BOTH halves of a build touch — the emitted code builds it, the shim
    // grows it through a pointer — so its layout is not a convention this crate
    // may choose, it is one it must match.
    src.push_str("typedef struct { char** keys; char* vals; long long len, cap; } VMap;\n");
    src.push_str("int main(void) {\n");
    for (i, (name, ll)) in SHAPES.iter().enumerate() {
        src.push_str(&format!(
            "  printf(\"{name}|size %d\\n\", (int)sizeof(T{i}));\n\
             \x20 printf(\"{name}|align %d\\n\", (int)_Alignof(T{i}));\n"
        ));
        for f in 0..member_count(ll) {
            src.push_str(&format!(
                "  printf(\"{name}|f{f} %d\\n\", (int)offsetof(T{i}, f{f}));\n"
            ));
        }
    }
    src.push_str(
        "  printf(\"VMap|size %d\\n\", (int)sizeof(VMap));\n\
         \x20 printf(\"VMap|align %d\\n\", (int)_Alignof(VMap));\n\
         \x20 printf(\"VMap|f0 %d\\n\", (int)offsetof(VMap, keys));\n\
         \x20 printf(\"VMap|f1 %d\\n\", (int)offsetof(VMap, vals));\n\
         \x20 printf(\"VMap|f2 %d\\n\", (int)offsetof(VMap, len));\n\
         \x20 printf(\"VMap|f3 %d\\n\", (int)offsetof(VMap, cap));\n\
         \x20 return 0;\n}\n",
    );
    src
}

/// How many members a shape has at the top level; 0 for anything that is not a
/// struct (only structs get `offsetof` lines).
fn member_count(ll: &str) -> usize {
    let t = ll.trim();
    match t.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
        Some(body) => split_members(body).len(),
        None => 0,
    }
}

#[test]
#[ignore = "needs clang + a wasi sysroot + wasmtime: `cargo test -p vyrn-codegen -- --ignored` (CI's parity job)"]
fn clang_agrees_with_the_layout_engine_on_wasm32() {
    let (Some(clang), Some(sysroot), Some(wasmtime)) = (
        require_tools("clang", "CLANG", vyrn_codegen::toolchain::find_clang()),
        require_tools(
            "a wasi sysroot",
            "WASI_SYSROOT",
            std::env::var("WASI_SYSROOT")
                .map(PathBuf::from)
                .ok()
                .filter(|p| p.exists())
                .or_else(|| {
                    vyrn_codegen::toolchain::tools_wasi_sysroot_from(Path::new(env!(
                        "CARGO_MANIFEST_DIR"
                    )))
                }),
        ),
        find_wasmtime(),
    ) else {
        eprintln!(
            "NOTE: no clang / wasi sysroot / wasmtime — layout is unverified on this machine"
        );
        return;
    };

    let dir = std::env::temp_dir().join(format!("vyrn-layout-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let c = dir.join("layout.c");
    let wasm = dir.join("layout.wasm");
    std::fs::write(&c, c_program()).unwrap();

    let mut cmd = std::process::Command::new(&clang);
    cmd.arg(&c)
        .arg("-o")
        .arg(&wasm)
        .arg("--target=wasm32-wasip1")
        .arg(format!("--sysroot={}", sysroot.display()));
    // The builtins archive, when the dev tree has it — `vyrn build` requires it,
    // but a `printf`-only program links without it on a full wasi-sdk.
    if let Some(b) = vyrn_codegen::toolchain::builtins_near_sysroot(&sysroot) {
        cmd.arg("-nodefaultlibs").arg(&b).arg("-lc");
    }
    let out = cmd.output().expect("run clang");
    assert!(
        out.status.success(),
        "clang failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = std::process::Command::new(&wasmtime)
        .arg("run")
        .arg(&wasm)
        .output()
        .expect("run wasmtime");
    assert!(
        run.status.success(),
        "wasmtime failed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let text = String::from_utf8(run.stdout).expect("clang's output is ascii");

    // Everything clang said, as `<shape>|<what>` -> value.
    let clang_says: std::collections::HashMap<&str, u32> = text
        .lines()
        .filter_map(|l| l.trim().split_once(' '))
        .map(|(k, v)| (k, v.trim().parse().expect("a number")))
        .collect();

    let mut disagreements = Vec::new();
    let mut checked = 0usize;
    // The shim's `VMap` rides along with the transcribed shapes: it is checked
    // against what `llt` gives a `Map<String, V>`, which is the agreement that
    // is not this crate's to choose.
    let cases = SHAPES
        .iter()
        .copied()
        .chain(std::iter::once(("VMap", "{ ptr, ptr, i64, i64 }")));
    for (name, ll) in cases {
        let l = of_ll(ll).unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut want = vec![("size".to_string(), l.size), ("align".to_string(), l.align)];
        want.extend(
            l.fields
                .iter()
                .enumerate()
                .map(|(f, off)| (format!("f{f}"), *off)),
        );
        for (what, mine) in want {
            let got = *clang_says
                .get(format!("{name}|{what}").as_str())
                .unwrap_or_else(|| panic!("clang printed no {what} for {name}"));
            checked += 1;
            if got != mine {
                disagreements.push(format!("{name}.{what} ({ll}): engine {mine}, clang {got}"));
            }
        }
    }

    assert!(
        disagreements.is_empty(),
        "layout disagrees with clang:\n{}",
        disagreements.join("\n")
    );
    eprintln!(
        "layout: {checked} numbers over {} shapes agree with clang",
        SHAPES.len() + 1
    );
}

/// The transcription itself, since a wrong C spelling would make the comparison
/// above pass by agreeing about the wrong struct.
#[test]
fn c_transcription_is_declarator_shaped() {
    assert_eq!(c_decl("i64", "x"), "long long x");
    assert_eq!(c_decl("[2 x ptr]", "x"), "void* x[2]");
    assert_eq!(
        c_decl("{ i8, i64 }", "x"),
        "struct { signed char f0; long long f1; } x"
    );
    assert_eq!(c_decl("[2 x [3 x i8]]", "x"), "signed char x[2][3]");
    assert_eq!(
        c_decl("<4 x float>", "x"),
        "float x __attribute__((vector_size(16)))"
    );
    assert_eq!(
        c_decl("{ i8, <2 x i64> }", "x"),
        "struct { signed char f0; long long f1 __attribute__((vector_size(16))); } x"
    );
    assert_eq!(split_members("i8, { i8, i64 }, i32").len(), 3);
    assert_eq!(split_members("i8, <4 x float>").len(), 2);
    assert_eq!(split_members("  ").len(), 0);
}
