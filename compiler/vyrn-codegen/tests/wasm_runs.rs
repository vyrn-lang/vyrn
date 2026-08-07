//! The module encoder, checked by running what it encodes (RFC-0077 M1).
//!
//! `wasm-encoder` frames sections; it does not validate, and this crate cannot
//! ask wasmtime to — wasmtime lives in the excluded `vyrn-genwasm`, and keeping
//! `vyrn-codegen` buildable with no LLVM, no clang and no wasi sysroot is the
//! property the workspace defends. So the check is the same one M0 used for
//! layout: shell out to a `wasmtime` binary and run the thing.
//!
//! Which is a better check than validation anyway. A module that validates has
//! well-formed sections; a module that RUNS and prints the right bytes has a
//! correct section order, a memory map that put the data where the code thinks
//! it is, a stack pointer that starts where the map says, and a frame convention
//! whose prologue and epilogue agree — all four at once, in one assertion.
//!
//! Skips, loudly, when wasmtime is absent. Same posture as the parity harness.

use std::path::{Path, PathBuf};
use vyrn_codegen::layout::of_ll;
use vyrn_codegen::wasm::{abi, Instruction, MemArg, Module, ValType};

fn find_wasmtime() -> Option<PathBuf> {
    vyrn_codegen::toolchain::find_wasmtime_from(Path::new(env!("CARGO_MANIFEST_DIR")))
}

/// Run `wasm` under wasmtime: exit code, stdout, stderr.
fn run(name: &str, wasm: &[u8]) -> Option<(i32, Vec<u8>, String)> {
    let wasmtime = find_wasmtime()?;
    let dir = std::env::temp_dir().join(format!("vyrn-wasm-m1-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.wasm"));
    std::fs::write(&path, wasm).unwrap();
    let out = std::process::Command::new(&wasmtime)
        .arg("run")
        .arg(&path)
        .output()
        .expect("run wasmtime");
    Some((
        out.status.code().unwrap_or(-1),
        out.stdout,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

/// `wasi_snapshot_preview1.fd_write` — the shape every import has: scalars and
/// pointers, no aggregate (M0 measured 0 of those across the whole boundary).
fn import_fd_write(m: &mut Module) -> u32 {
    m.import(
        "wasi_snapshot_preview1",
        "fd_write",
        &[ValType::I32; 4],
        &[abi("i32").unwrap()],
    )
}

fn i32_store(off: u32) -> Instruction<'static> {
    Instruction::I32Store(MemArg {
        offset: off as u64,
        align: 2,
        memory_index: 0,
    })
}
fn i64_store(off: u32) -> Instruction<'static> {
    Instruction::I64Store(MemArg {
        offset: off as u64,
        align: 3,
        memory_index: 0,
    })
}
fn i32_load(off: u32) -> Instruction<'static> {
    Instruction::I32Load(MemArg {
        offset: off as u64,
        align: 2,
        memory_index: 0,
    })
}
fn i64_load(off: u32) -> Instruction<'static> {
    Instruction::I64Load(MemArg {
        offset: off as u64,
        align: 3,
        memory_index: 0,
    })
}

/// The floor: sections, an import, an export, and a body that does one thing.
/// If this does not exit 7 the encoder is not producing a module at all.
#[test]
fn a_module_that_only_returns_a_constant() {
    let mut m = Module::new();
    let exit = m.import("wasi_snapshot_preview1", "proc_exit", &[ValType::I32], &[]);
    let start = m.func(&[], &[], &[], 0, |b| {
        b.ins(&Instruction::I32Const(7))
            .ins(&Instruction::Call(exit));
    });
    m.export("_start", start);
    let Some((code, _, err)) = run("constant", &m.finish()) else {
        eprintln!("NOTE: no wasmtime — the M1 encoder is unverified on this machine");
        return;
    };
    assert_eq!(code, 7, "{err}");
}

/// Data placement plus the frame plus a call to an import, together: the string
/// is in the data segment at [`DATA_BASE`], the iovec describing it is built in
/// the shadow-stack frame, and the shim-shaped import is what turns one into the
/// other. Getting the address wrong prints garbage; getting the frame wrong
/// writes over the string.
#[test]
fn a_frame_and_a_data_segment_and_an_imported_call() {
    let mut m = Module::new();
    let fd_write = import_fd_write(&mut m);
    let hello = m.data(b"hello from a directly emitted module\n", 1);
    let len = 37u32;
    // iovec { ptr, len } at 0, the returned byte count at 8.
    let start = m.func(&[], &[], &[], 12, |b| {
        b.slot(0)
            .ins(&Instruction::I32Const(hello as i32))
            .ins(&i32_store(0));
        b.slot(0)
            .ins(&Instruction::I32Const(len as i32))
            .ins(&i32_store(4));
        b.ins(&Instruction::I32Const(1)); // stdout
        b.slot(0);
        b.ins(&Instruction::I32Const(1)); // one iovec
        b.slot(8);
        b.ins(&Instruction::Call(fd_write)).ins(&Instruction::Drop);
    });
    m.export("_start", start);
    let Some((code, out, err)) = run("hello", &m.finish()) else {
        return;
    };
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        String::from_utf8_lossy(&out),
        "hello from a directly emitted module\n"
    );
}

/// The round trip M0's clang comparison could only make on paper: one function
/// writes an `{ ptr, i64, i64 }` into a frame slot at the offsets [`of_ll`]
/// computed, a DIFFERENT function reads them back out, and the raw bytes go to
/// stdout so the padding is visible too.
///
/// Two things fail here that nothing else catches. If the two functions disagree
/// about an offset the read-back values are wrong — that is the silent
/// miscompile this RFC keeps warning about, made loud. And the 4-byte hole after
/// the pointer has to still be zero: it is the hole that makes the triple 24
/// bytes instead of 20, and the only reason we know it exists is that clang said
/// so.
#[test]
fn a_struct_round_trips_through_the_shadow_stack_at_the_computed_offsets() {
    let l = of_ll("{ ptr, i64, i64 }").unwrap();
    assert_eq!((l.size, &l.fields[..]), (24, &[0, 8, 16][..]));
    let (p, a, c) = (
        0x1111_1111u32,
        0x2222_2222_2222_2222u64,
        0x3333_3333_3333_3333u64,
    );

    let mut m = Module::new();
    let fd_write = import_fd_write(&mut m);
    // Takes the address of a slot in its CALLER's frame — the aggregate ABI:
    // on the operand stack an aggregate is always an i32 address.
    let fill = m.func(&[ValType::I32], &[], &[], 0, |b| {
        b.ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I32Const(p as i32))
            .ins(&i32_store(l.fields[0]));
        b.ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I64Const(a as i64))
            .ins(&i64_store(l.fields[1]));
        b.ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I64Const(c as i64))
            .ins(&i64_store(l.fields[2]));
    });
    // struct at 0, the re-packed read-back at 24, iovec at 48, count at 56.
    let start = m.func(&[], &[], &[], 60, |b| {
        b.slot(0).ins(&Instruction::Call(fill));
        // Read each field back at its own offset and re-pack them contiguously
        // as three i64s, so a wrong offset cannot land on the right byte.
        b.slot(24)
            .slot(0)
            .ins(&i32_load(l.fields[0]))
            .ins(&Instruction::I64ExtendI32U)
            .ins(&i64_store(0));
        b.slot(24)
            .slot(0)
            .ins(&i64_load(l.fields[1]))
            .ins(&i64_store(8));
        b.slot(24)
            .slot(0)
            .ins(&i64_load(l.fields[2]))
            .ins(&i64_store(16));
        b.slot(48).slot(0).ins(&i32_store(0));
        b.slot(48)
            .ins(&Instruction::I32Const(48))
            .ins(&i32_store(4));
        b.ins(&Instruction::I32Const(1));
        b.slot(48);
        b.ins(&Instruction::I32Const(1));
        b.slot(56);
        b.ins(&Instruction::Call(fd_write)).ins(&Instruction::Drop);
    });
    m.export("_start", start);
    let Some((code, out, err)) = run("roundtrip", &m.finish()) else {
        return;
    };
    assert_eq!(code, 0, "{err}");

    let mut want = Vec::new();
    want.extend_from_slice(&p.to_le_bytes());
    want.extend_from_slice(&[0; 4]); // the hole clang says is there
    want.extend_from_slice(&a.to_le_bytes());
    want.extend_from_slice(&c.to_le_bytes());
    want.extend_from_slice(&(p as u64).to_le_bytes());
    want.extend_from_slice(&a.to_le_bytes());
    want.extend_from_slice(&c.to_le_bytes());
    assert_eq!(out, want, "the struct did not survive the frame");
}

/// The sweep (RFC-0077 M2p), checked the only way a renumbering can be checked:
/// by running the module afterwards.
///
/// Every index in a wasm `call` is absolute, so dropping one function shifts every
/// function above it. A prune that forgot to rewrite a call still VALIDATES
/// whenever the two signatures match — which is the silent case `Rt::next_is`
/// exists for inside the runtime, and the case this arranges deliberately: all
/// four helpers here are `() -> i64`, so a call left pointing one slot off returns
/// the wrong number and nothing complains. Two of the four are unreachable and
/// sit BETWEEN the two that are, so the surviving pair cannot keep its old
/// indices.
#[test]
fn a_swept_module_still_calls_what_it_meant_to() {
    let mut m = Module::new();
    // Unreachable: nothing in the module calls it, so the sweep takes it and
    // every index above it moves down two.
    let unused = import_fd_write(&mut m);
    let exit = m.import("wasi_snapshot_preview1", "proc_exit", &[ValType::I32], &[]);
    let n = |m: &mut Module, v: i64| {
        m.func(&[], &[ValType::I64], &[], 0, |b| {
            b.ins(&Instruction::I64Const(v));
        })
    };
    let three = n(&mut m, 3);
    let dead_a = n(&mut m, 90);
    let dead_b = n(&mut m, 91);
    let four = n(&mut m, 4);
    let start = m.func(&[], &[], &[], 0, |b| {
        b.ins(&Instruction::Call(three))
            .ins(&Instruction::Call(four))
            .ins(&Instruction::I64Add)
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::Call(exit));
    });
    m.export("_start", start);
    m.sweep();
    let bytes = m.finish();
    let _ = (unused, dead_a, dead_b);
    let Some((code, _, err)) = run("swept", &bytes) else {
        return;
    };
    // 7 says both surviving calls landed on their own bodies. 93, 94, 181 or a
    // trap would each be one specific renumbering mistake.
    assert_eq!(
        code, 7,
        "the swept module called something else; stderr:\n{err}"
    );
    assert!(
        !bytes.windows(8).any(|w| w == b"fd_write"),
        "an unreached import survived"
    );
}

/// The half of the memory map that is a safety property rather than an address:
/// a frame bigger than the 64 KB below `STACK_TOP` underflows past 0, wraps to
/// near `0xFFFFFFFF`, and the first access traps. It does NOT wrap into the data
/// segments — which is the whole reason the stack is at the bottom of memory
/// (`--stack-first`) and not above the statics.
#[test]
fn a_frame_past_the_bottom_of_memory_traps_rather_than_wrapping_into_data() {
    let mut m = Module::new();
    m.data(b"do not overwrite me", 1);
    let start = m.func(&[], &[], &[], 65_536 + 16, |b| {
        b.slot(0).ins(&Instruction::I32Const(1)).ins(&i32_store(0));
    });
    m.export("_start", start);
    let Some((code, _, err)) = run("overflow", &m.finish()) else {
        return;
    };
    assert_ne!(code, 0, "an overflowing frame must not succeed");
    assert!(
        err.contains("out of bounds"),
        "expected an out-of-bounds trap, got:\n{err}"
    );
}
