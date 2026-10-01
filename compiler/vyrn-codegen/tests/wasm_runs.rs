//! The module encoder, checked by running what it encodes.
//!
//! `wasm-encoder` does not validate, and this crate cannot link wasmtime: that
//! lives in the excluded `vyrn-genwasm`, so `vyrn-codegen` builds with no LLVM,
//! clang or wasi sysroot. The tests shell out to a `wasmtime` binary instead. A
//! module that runs and prints the right bytes also proves the section order, the
//! memory map, the stack pointer and the frame convention.
//!
//! Every test needs wasmtime, so all are `#[ignore]`d. CI's parity job runs them
//! with `VYRN_REQUIRE_TOOLS=1`, which turns a missing wasmtime into a panic
//! rather than a silent pass.

use std::path::{Path, PathBuf};
use vyrn_codegen::layout::{Leaf, Shape};
use vyrn_codegen::toolchain::require_tools;
use vyrn_codegen::wasm::{abi, Instruction, MemArg, Module, ValType};

fn find_wasmtime() -> Option<PathBuf> {
    require_tools(
        "wasmtime",
        "VYRN_WASMTIME",
        vyrn_codegen::toolchain::find_wasmtime_from(Path::new(env!("CARGO_MANIFEST_DIR"))),
    )
}

/// Runs `wasm` under wasmtime: exit code, stdout, stderr.
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

/// `wasi_snapshot_preview1.fd_write`, the shape every import has: scalars and
/// pointers, no aggregate.
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

/// The floor: if this does not exit 7, the encoder produces no module at all.
#[test]
#[ignore = "needs wasmtime: `cargo test -p vyrn-codegen -- --ignored` (CI's parity job)"]
fn a_module_that_only_returns_a_constant() {
    let mut m = Module::new();
    let exit = m.import("wasi_snapshot_preview1", "proc_exit", &[ValType::I32], &[]);
    let start = m.func(&[], &[], &[], 0, |b| {
        b.ins(&Instruction::I32Const(7))
            .ins(&Instruction::Call(exit));
    });
    m.export("_start", start);
    let Some((code, _, err)) = run("constant", &m.finish().unwrap()) else {
        eprintln!("NOTE: no wasmtime — the M1 encoder is unverified on this machine");
        return;
    };
    assert_eq!(code, 7, "{err}");
}

/// The string sits in the data segment and its iovec in the shadow-stack frame. A
/// wrong address prints garbage; a wrong frame writes over the string.
#[test]
#[ignore = "needs wasmtime: `cargo test -p vyrn-codegen -- --ignored` (CI's parity job)"]
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
    let Some((code, out, err)) = run("hello", &m.finish().unwrap()) else {
        return;
    };
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        String::from_utf8_lossy(&out),
        "hello from a directly emitted module\n"
    );
}

/// One function writes `{ ptr, i64, i64 }` at the offsets [`Shape::layout`] computed and
/// another reads it back, with the raw bytes on stdout. A disagreement on an
/// offset, a silent miscompile otherwise, shows as wrong values. The 4-byte hole
/// after the pointer (24 bytes, not 20, as clang lays it out) must stay zero.
#[test]
#[ignore = "needs wasmtime: `cargo test -p vyrn-codegen -- --ignored` (CI's parity job)"]
fn a_struct_round_trips_through_the_shadow_stack_at_the_computed_offsets() {
    let l = Shape::Struct([Leaf::Ptr, Leaf::I64, Leaf::I64].map(Shape::Leaf).to_vec())
        .layout()
        .unwrap();
    assert_eq!((l.size, &l.fields[..]), (24, &[0, 8, 16][..]));
    let (p, a, c) = (
        0x1111_1111u32,
        0x2222_2222_2222_2222u64,
        0x3333_3333_3333_3333u64,
    );

    let mut m = Module::new();
    let fd_write = import_fd_write(&mut m);
    // Takes a slot address in its caller's frame: on the operand stack an
    // aggregate is always an i32 address.
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
    let Some((code, out, err)) = run("roundtrip", &m.finish().unwrap()) else {
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

/// A wasm `call` index is absolute, so the sweep renumbers, and a call it forgot
/// to rewrite still validates when the signatures match. All four helpers are
/// `() -> i64`, and the two unreachable ones sit between the two that survive, so
/// a call left one slot off returns the wrong number.
#[test]
#[ignore = "needs wasmtime: `cargo test -p vyrn-codegen -- --ignored` (CI's parity job)"]
fn a_swept_module_still_calls_what_it_meant_to() {
    let mut m = Module::new();
    // Unreachable, so the sweep takes it and every index above it moves.
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
    let bytes = m.finish().unwrap();
    let _ = (unused, dead_a, dead_b);
    let Some((code, _, err)) = run("swept", &bytes) else {
        return;
    };
    // 93, 94, 181 or a trap would each be one specific renumbering mistake.
    assert_eq!(
        code, 7,
        "the swept module called something else; stderr:\n{err}"
    );
    assert!(
        !bytes.windows(8).any(|w| w == b"fd_write"),
        "an unreached import survived"
    );
}

/// A frame larger than `STACK_BYTES` underflows past 0 to near `0xFFFFFFFF` and
/// traps, rather than wrapping into the data segments: that is why the stack sits
/// at the bottom of memory (`--stack-first`). A lowered body cannot reach this,
/// because `direct::compile` refuses a frame past `FRAME_LIMIT`, so the frame is
/// built by hand.
#[test]
#[ignore = "needs wasmtime: `cargo test -p vyrn-codegen -- --ignored` (CI's parity job)"]
fn a_frame_past_the_bottom_of_memory_traps_rather_than_wrapping_into_data() {
    let mut m = Module::new();
    m.data(b"do not overwrite me", 1);
    let start = m.func(&[], &[], &[], vyrn_codegen::wasm::STACK_BYTES + 16, |b| {
        b.slot(0).ins(&Instruction::I32Const(1)).ins(&i32_store(0));
    });
    m.export("_start", start);
    let Some((code, _, err)) = run("overflow", &m.finish().unwrap()) else {
        return;
    };
    assert_ne!(code, 0, "an overflowing frame must not succeed");
    assert!(
        err.contains("out of bounds"),
        "expected an out-of-bounds trap, got:\n{err}"
    );
}
