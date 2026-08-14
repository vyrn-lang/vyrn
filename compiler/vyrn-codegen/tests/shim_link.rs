//! A directly-emitted module linked against the shared runtime shim
//! (RFC-0077 M2i), checked by running the pair.
//!
//! `tests/imports_vs_shim.rs` compares two *strings* — the emitter's `declare`
//! lines against the C the shim defines — and that comparison has been the only
//! thing standing behind the import section since M1, because `direct::compile`
//! imported exactly `fd_write` and `proc_exit` and never touched the boundary at
//! all. This is the same audit with wasmtime doing the checking: every signature
//! in `wasm::boundary` that the shim exports is declared as an import, and
//! instantiation succeeds only if all of them agree with the module they resolve
//! to. A wrong one is a validation failure with the name in it.
//!
//! It also runs the thing the split exists for. Shared linear memory is not
//! provable by inspection: the guest allocates from the shim's dlmalloc heap,
//! writes bytes into it, and C reads them back — three parties, one address
//! space, or the numbers do not come out.
//!
//! Needs clang, a wasi sysroot and a wasmtime binary, so both tests are in the
//! IGNORED tier — a plain `cargo test` counts them `ignored` (and prints the
//! reason, without `--quiet`) rather than green,
//! and CI's parity job runs `cargo test -p vyrn-codegen -- --ignored` with
//! `VYRN_REQUIRE_TOOLS=1`, where a missing tool panics instead of skipping.
//! (This is also why the STANDALONE ladder is the one that carries this RFC's
//! no-toolchain acceptance criterion.)

use std::path::{Path, PathBuf};
use vyrn_codegen::toolchain::require_tools;
use vyrn_codegen::wasm::{boundary, Instruction, MemArg, Module, ValType, SHIM_BASE};

/// The `env` namespace both halves agree on: the name a `--preload`ed module is
/// registered under, and the module every `__vyrn_*` import names.
const ENV: &str = "env";

fn tools() -> Option<(PathBuf, PathBuf)> {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let wasmtime = require_tools(
        "wasmtime",
        "VYRN_WASMTIME",
        vyrn_codegen::toolchain::find_wasmtime_from(here),
    )?;
    // `shim_wasm` needs clang AND the sysroot AND the builtins archive, and
    // answers `None` for any of the three — so the variable named here is the
    // one a CI job is most likely to have got wrong.
    let shim = require_tools(
        "the compiled runtime shim (clang + wasi sysroot + builtins)",
        "WASI_SYSROOT/WASI_BUILTINS",
        vyrn_codegen::toolchain::shim_wasm(),
    )?;
    Some((wasmtime, shim))
}

/// Every function a wasm module exports, read off the bytes.
///
/// Off the bytes rather than out of a wasm crate's parser, for the reason M0's
/// clang test gives: a parser that agrees with the encoder because they share a
/// dependency is not a second opinion. It is also the only way to know what the
/// shim exports before instantiating anything, which is when a mismatch has
/// already cost the run.
fn exported_funcs(wasm: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 8usize; // past the magic and the version
    while i < wasm.len() {
        let id = wasm[i];
        i += 1;
        let (len, adv) = leb(&wasm[i..]);
        i += adv;
        let body = &wasm[i..i + len as usize];
        i += len as usize;
        if id != 7 {
            continue;
        }
        let (n, mut j) = leb(body);
        for _ in 0..n {
            let (nlen, adv) = leb(&body[j..]);
            j += adv;
            let name = String::from_utf8_lossy(&body[j..j + nlen as usize]).into_owned();
            j += nlen as usize;
            let kind = body[j];
            j += 1;
            let (_, adv) = leb(&body[j..]);
            j += adv;
            if kind == 0 {
                out.push(name);
            }
        }
    }
    out
}

fn leb(b: &[u8]) -> (u32, usize) {
    let (mut v, mut shift, mut i) = (0u32, 0, 0);
    loop {
        v |= ((b[i] & 0x7f) as u32) << shift;
        shift += 7;
        i += 1;
        if b[i - 1] & 0x80 == 0 {
            return (v, i);
        }
    }
}

fn i32_store(off: u32) -> Instruction<'static> {
    Instruction::I32Store(MemArg {
        offset: off as u64,
        align: 2,
        memory_index: 0,
    })
}

/// Build the guest: it imports the whole boundary, then does the smallest thing
/// that cannot work unless the two modules share one address space.
///
/// `wrong` replaces one import's signature with a plausible mis-mapping, and is
/// the only reason any of this is parameterized: it is how the two negative tests
/// below show what agreeing is worth.
type Wrong<'a> = Option<(&'a str, Vec<ValType>, Vec<ValType>)>;

fn guest(shim_exports: &[String], wrong: Wrong) -> (Vec<u8>, usize) {
    let mut m = Module::new();
    m.import_memory();
    let fd_write = m.import(
        "wasi_snapshot_preview1",
        "fd_write",
        &[ValType::I32; 4],
        &[ValType::I32],
    );
    let proc_exit = m.import("wasi_snapshot_preview1", "proc_exit", &[ValType::I32], &[]);

    let mut at: std::collections::BTreeMap<&str, u32> = std::collections::BTreeMap::new();
    for (name, sig) in boundary() {
        // Variadic: wasm cannot express one at all, which is M3's milestone
        // rather than a mismatch. Not exported by the shim: `__vyrn_extern_*` is a
        // per-program trap stub and `__vyrn_gen_list_dir` belongs to the
        // generator-host variant, so neither is this module's to import.
        let Some((params, ret)) = sig else { continue };
        if !shim_exports.iter().any(|e| e == name) {
            continue;
        }
        let mut params: Vec<ValType> = params.iter().flatten().copied().collect();
        let mut ret: Vec<ValType> = ret.iter().copied().collect();
        if let Some((bad, p, r)) = &wrong {
            if name == bad {
                (params, ret) = (p.clone(), r.clone());
            }
        }
        let i = m.import(ENV, name, &params, &ret);
        at.insert(name.as_str(), i);
    }
    let n = at.len();
    let (malloc, strlen) = (at["__vyrn_malloc"], at["__vyrn_strlen"]);

    // iovec at 0, the written count at 8; `p` and `s` in locals.
    let (p, s) = (1u32, 2u32);
    let start = m.func(&[], &[], &[ValType::I32, ValType::I32], 12, |b| {
        // Eight bytes out of the SHIM's heap, not ours: this module has no
        // allocator of its own at all.
        b.ins(&Instruction::I64Const(8))
            .ins(&Instruction::Call(malloc))
            .ins(&Instruction::LocalSet(p));
        // Fail loudly rather than subtly if the pointer is not where the shim's
        // heap is: a private heap in a private memory would also "work" until
        // something on the other side read it.
        b.ins(&Instruction::LocalGet(p))
            .ins(&Instruction::I32Const(SHIM_BASE as i32))
            .ins(&Instruction::I32LtU)
            .ins(&Instruction::If(vyrn_codegen::wasm::BlockType::Empty))
            .ins(&Instruction::I32Const(99))
            .ins(&Instruction::Call(proc_exit))
            .ins(&Instruction::End);
        // "hi\0", written by the guest into the shim's allocation.
        for (off, byte) in [(0u64, b'h'), (1, b'i'), (2, 0)] {
            b.ins(&Instruction::LocalGet(p))
                .ins(&Instruction::I32Const(byte as i32))
                .ins(&Instruction::I32Store8(MemArg {
                    offset: off,
                    align: 0,
                    memory_index: 0,
                }));
        }
        // The bytes we compare are the ones the guest just wrote into the shim's
        // allocation, read back through C. This used to be `__vyrn_vj_bool(true)`
        // through `__vyrn_vj_encode` — M0's one widening feeding the shim's JSON
        // writer — then `__vyrn_charcount` after RFC-0078 M2b retired both, and now
        // `__vyrn_strlen` below, after the census routed `charCount` into `std/text`
        // as well. Same property each time, and fewer C functions each time: only a
        // shared address space makes a pointer the guest filled mean anything over
        // there. `strlen` is the one that cannot leave, because `byteLength` is a
        // view rather than an operation.
        b.ins(&Instruction::LocalGet(p))
            .ins(&Instruction::LocalSet(s));
        b.slot(0).ins(&Instruction::LocalGet(s)).ins(&i32_store(0));
        b.slot(0)
            .ins(&Instruction::LocalGet(s))
            .ins(&Instruction::Call(strlen))
            .ins(&Instruction::I32WrapI64)
            .ins(&i32_store(4));
        b.ins(&Instruction::I32Const(1));
        b.slot(0);
        b.ins(&Instruction::I32Const(1));
        b.slot(8);
        b.ins(&Instruction::Call(fd_write)).ins(&Instruction::Drop);
        // strlen("hi") * 10 + strlen("true") = 24, and every digit of it went
        // through C reading memory the guest wrote.
        b.ins(&Instruction::LocalGet(p))
            .ins(&Instruction::Call(strlen))
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::I32Const(10))
            .ins(&Instruction::I32Mul)
            .ins(&Instruction::LocalGet(s))
            .ins(&Instruction::Call(strlen))
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::I32Add)
            .ins(&Instruction::Call(proc_exit));
    });
    m.export("_start", start);
    (m.finish().unwrap(), n)
}

fn run(wasmtime: &Path, shim: &Path, name: &str, wasm: &[u8]) -> (i32, Vec<u8>, String) {
    let dir = std::env::temp_dir().join(format!("vyrn-shimlink-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.wasm"));
    std::fs::write(&path, wasm).unwrap();
    let out = std::process::Command::new(wasmtime)
        .arg("run")
        .arg("--preload")
        .arg(format!("{ENV}={}", shim.display()))
        .arg(&path)
        .output()
        .expect("run wasmtime");
    (
        out.status.code().unwrap_or(-1),
        out.stdout,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
#[ignore = "needs clang + a wasi sysroot + wasmtime: `cargo test -p vyrn-codegen -- --ignored` (CI's parity job)"]
fn the_whole_boundary_agrees_with_the_shim_it_resolves_to() {
    let Some((wasmtime, shim)) = tools() else {
        eprintln!(
            "SKIP: needs clang, a wasi sysroot and wasmtime — the shim link is unverified here"
        );
        return;
    };
    let exports = exported_funcs(&std::fs::read(&shim).unwrap());
    let (wasm, n) = guest(&exports, None);
    let (code, out, err) = run(&wasmtime, &shim, "boundary", &wasm);

    assert_ne!(code, 99, "the allocation did not come from the shim's heap");
    assert_eq!(
        code, 22,
        "expected strlen(\"hi\")*10 + strlen(\"hi\"); stderr:\n{err}"
    );
    assert_eq!(
        String::from_utf8_lossy(&out),
        "hi",
        "the bytes the guest wrote into the shim's heap, read back through C"
    );
    // Was 60 before RFC-0078 M2b, which removed eleven `declare` lines: the JSON
    // DOM builders and the serializer, all of them unreferenced once `toJson`
    // became a shared AST walk over `std/json`. Then 57 -> 55 at M4c, which routed
    // `startsWith`/`endsWith`/`contains` into `std/strpred` and took
    // `declare i32 @__vyrn_strncmp` and `declare ptr @strstr` with them. Then
    // 55 -> 33 at M3, the largest single drop this RFC has produced: `fromJson`
    // became `std/jsonread` plus a generated per-type walk, so the DOM accessors,
    // the parser, the number readers, the message assemblers, the path helpers and
    // the whole Issue accumulator left the boundary together — twenty-two declares
    // and forty-seven C definitions. A boundary that shrinks because a milestone
    // deleted C is the point; one that shrinks quietly is not.
    // 33 -> 32 when the census retired `charCount`: one declare, one C definition,
    // and the smallest drop this RFC has produced, which is what a builtin with one
    // caller and no justification is worth.
    assert!(
        n >= 32,
        "only {n} of the boundary was importable — the census shrank"
    );
    eprintln!(
        "shim link: {n} boundary signatures instantiated against the module that defines them"
    );
}

/// What agreeing is worth, in the two ways it can fail.
///
/// Nothing in a passing run says what would happen if a signature were wrong,
/// because wasm has no `i1` to get wrong once `abi` has widened it. So both
/// failures are asserted, and they are not the same failure:
///
/// **The one M0 named.** A pointer parameter mis-mapped to an `i64` does not reach
/// instantiation at all — the caller pushes what `abi` said the value was, and
/// wasm's own type checker rejects the module. That is `abi` being load-bearing at
/// emission time rather than at link time. M0 demonstrated it on
/// `declare ptr @__vyrn_vj_bool(i1)`, the boundary's only sub-i32 argument until
/// RFC-0078 M2b retired the DOM builders; `__vyrn_strlen(ptr)` is the same crossing
/// one width up. (It was `__vyrn_charcount(ptr)` until the census routed `charCount`
/// into `std/text` — the two have the same signature, and `strlen` is the one no
/// milestone can take, because `byteLength` is a view.)
///
/// **The one only a shim can catch.** A mismatch on an import nothing in this
/// body calls validates perfectly and fails when the two modules are put
/// together, with the name in the message. `__vyrn_now_millis` returning `i32`
/// instead of `i64` is exactly the shape of a `size_t` mistake, and it is
/// unreachable for a module with no shim beside it — which is why the entire
/// 68-signature boundary was unchecked-by-running until this milestone.
#[test]
#[ignore = "needs clang + a wasi sysroot + wasmtime: `cargo test -p vyrn-codegen -- --ignored` (CI's parity job)"]
fn a_boundary_signature_that_does_not_match_is_refused() {
    let Some((wasmtime, shim)) = tools() else {
        eprintln!("SKIP: needs clang, a wasi sysroot and wasmtime");
        return;
    };
    let exports = exported_funcs(&std::fs::read(&shim).unwrap());

    let widening = Some(("__vyrn_strlen", vec![ValType::I64], vec![ValType::I64]));
    let (wasm, _) = guest(&exports, widening);
    let (code, _, err) = run(&wasmtime, &shim, "narrowed", &wasm);
    assert_ne!(code, 24, "a mismatched signature must not run");
    assert!(
        err.contains("type mismatch: expected i64, found i32"),
        "a pointer that is not an i32 should fail wasm's own type check:
{err}"
    );

    let unused = Some(("__vyrn_now_millis", vec![], vec![ValType::I32]));
    let (wasm, _) = guest(&exports, unused);
    let (code, _, err) = run(&wasmtime, &shim, "unused", &wasm);
    assert_ne!(code, 24, "a mismatched signature must not run");
    assert!(
        err.contains("__vyrn_now_millis"),
        "an import nothing calls must still be checked, by name:
{err}"
    );
}
