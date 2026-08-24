//! RFC-0111: a program can write bytes that are not text.
//!
//! `examples/mandelbrot.vyrn` proves the feature works, and the parity harness
//! proves all three engines agree. This file pins the two things that would
//! break QUIETLY — where the bytes are still written, the program still exits 0,
//! and only the content is wrong.
//!
//! Both were named by `rfcs/census/blocked-byte-sink.md` before the feature was
//! built, and both are transport-level: the payload is identical by
//! construction, because one array of bytes is handed to each engine.

mod common;

use common::{scratch, vyrn};

/// The bytes a binary sink must survive: a NUL, a lone `\n`, a `\r\n` pair, and
/// a byte no UTF-8 decoder accepts.
///
/// Each is chosen for a rule it would break. NUL is refused by `stringFromBytes`
/// before UTF-8 is even considered, so it is what makes this not a `String`.
/// `0x0A` is what Windows text mode rewrites. `0x0D 0x0A` is what a
/// line-ending normaliser would collapse. `0xFF` starts no valid UTF-8
/// sequence, so a decode-then-encode round trip replaces it with U+FFFD.
const HOSTILE: &[u8] = &[0x00, 0x0A, 0x0D, 0x0A, 0xFF, 0xC3, 0x28, 0x7F];

fn hostile_program(sink: &str) -> String {
    let list = HOSTILE
        .iter()
        .map(|b| b.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "fn main() -> Int64 {{\n    let raw: Array<UInt8> = [{list}]\n    {sink}\n    return 0\n}}\n"
    )
}

/// Standard output carries every byte, unchanged.
///
/// THE FAILURE THIS EXISTS FOR: C stdio opens stdout in TEXT mode on Windows,
/// where `fwrite` turns a `0x0A` into `0x0D 0x0A`. For `print` that is the
/// platform's own newline and it is right. For a packed pixel row it is
/// corruption nothing downstream can undo, because nothing can tell which
/// `0x0D 0x0A` was a real pair of bytes. The native shim sets binary mode for
/// the write; if that guard is removed, this test grows two bytes.
#[test]
fn write_stdout_carries_every_byte() {
    let dir = scratch("bytesink-stdout");
    let src = dir.join("hostile.vyrn");
    std::fs::write(&src, hostile_program("writeStdout(raw)")).unwrap();

    let out = vyrn()
        .args(["run", src.to_str().unwrap()])
        .output()
        .expect("run the program");
    assert!(out.status.success(), "the program failed: {out:?}");
    assert_eq!(
        out.stdout, HOSTILE,
        "stdout is not byte-for-byte what the program wrote — \
         a 0x0A that became 0x0D 0x0A is text mode, and a 0xEF 0xBF 0xBD is a \
         decode-and-re-encode somewhere in the path"
    );
}

/// A file carries every byte, unchanged.
#[test]
fn write_file_bytes_carries_every_byte() {
    let dir = scratch("bytesink-file");
    let target = dir.join("hostile.bin");
    let src = dir.join("hostile.vyrn");
    let sink = format!(
        "match writeFileBytes(\"{}\", raw) {{ Ok(d) => print(\"wrote\"), Err(w) => print(w) }}",
        target.to_str().unwrap().replace('\\', "/")
    );
    std::fs::write(&src, hostile_program(&sink)).unwrap();

    let out = vyrn()
        .args(["run", src.to_str().unwrap()])
        .output()
        .expect("run the program");
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        said.contains("wrote"),
        "the write reported a failure: {said}"
    );
    assert_eq!(
        std::fs::read(&target).expect("the file the program wrote"),
        HOSTILE,
        "the file is not byte-for-byte what the program wrote"
    );
}

/// `writeStdout` and `print` interleave in call order, and `print` keeps its
/// own newline behaviour afterwards.
///
/// The native shim switches stdout to binary mode for a `writeStdout` and
/// switches it BACK. Leaving it binary would be the quiet kind of wrong: every
/// later `print` would emit a bare `\n` on Windows, which is not what `print`
/// has ever done, and no test of `writeStdout` alone would notice.
#[test]
fn a_byte_write_does_not_change_what_print_does_after_it() {
    let dir = scratch("bytesink-order");
    let src = dir.join("mixed.vyrn");
    std::fs::write(
        &src,
        "fn main() -> Int64 {\n    \
         let mid: Array<UInt8> = [60, 62]\n    \
         print(\"before\")\n    \
         writeStdout(mid)\n    \
         print(\"after\")\n    \
         return 0\n}\n",
    )
    .unwrap();

    let out = vyrn()
        .args(["run", src.to_str().unwrap()])
        .output()
        .expect("run the program");
    let got = String::from_utf8(out.stdout).expect("this one IS text");
    // The bytes land between the two lines and not before or after both, which
    // is what "shares the handle" has to mean.
    let flat = got.replace("\r\n", "\n");
    assert_eq!(
        flat, "before\n<>after\n",
        "the byte write did not land between the two prints: {got:?}"
    );
}

/// The header of a PBM is text and its pixels are not, and both leave through
/// the same call.
///
/// This is the whole gap in one assertion: `mandelbrot-200.expected` sat in the
/// corpus from RFC-0104 with no program beside it, because the twelfth byte is
/// a NUL and no sink would carry it.
#[test]
fn the_committed_mandelbrot_fixture_now_has_a_program() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture = root.join("rfcs/bench-0104/mandelbrot-200.expected");
    let want = std::fs::read(&fixture).expect("the committed fixture");
    assert!(
        want.contains(&0u8),
        "the fixture has no NUL — it is no longer the case that a String could not hold it"
    );

    let src = root.join("examples/mandelbrot.vyrn");
    let out = vyrn()
        .args(["run", src.to_str().unwrap()])
        .output()
        .expect("run mandelbrot");
    assert!(out.status.success(), "mandelbrot failed: {out:?}");
    assert_eq!(
        out.stdout.len(),
        want.len(),
        "mandelbrot wrote {} bytes and the fixture is {}",
        out.stdout.len(),
        want.len()
    );
    assert!(
        out.stdout == want,
        "mandelbrot's output is not the committed fixture"
    );
}

/// The three engines write the SAME BYTES, compared as bytes.
///
/// THE PARITY HARNESS CANNOT DO THIS, and finding that out is why this test
/// exists. `common::norm` is `String::from_utf8_lossy(bytes).replace("\r\n",
/// "\n")` — it destroys invalid UTF-8 and it collapses CRLF, which are precisely
/// the two ways a binary artifact gets corrupted. Removing the native shim's
/// binary-mode guard makes `mandelbrot` write 5013 bytes instead of 5011, and
/// `examples_interp_native_parity` still passes. Measured, not reasoned about.
///
/// So binary output gets its own comparison, with no normalisation anywhere in
/// it. Native only: the interpreter is covered by the tests above and the wasm
/// column has no text mode to get wrong. `#[ignore]` because a native build
/// needs clang, like every other test that builds one.
#[test]
#[ignore = "needs clang for the native build"]
fn every_engine_writes_the_same_bytes_for_mandelbrot() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let src = root.join("examples/mandelbrot.vyrn");
    let want = std::fs::read(root.join("rfcs/bench-0104/mandelbrot-200.expected"))
        .expect("the committed fixture");

    let interp = vyrn()
        .args(["run", src.to_str().unwrap()])
        .output()
        .expect("run mandelbrot");
    assert_eq!(
        interp.stdout, want,
        "the interpreter's bytes are not the fixture"
    );

    let dir = scratch("bytesink-native");
    let exe = dir.join(if cfg!(windows) { "mb.exe" } else { "mb" });
    let built = vyrn()
        .args(["build", src.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("build mandelbrot");
    assert!(built.status.success(), "the native build failed: {built:?}");

    let native = std::process::Command::new(&exe)
        .output()
        .expect("run the native mandelbrot");
    assert_eq!(
        native.stdout.len(),
        want.len(),
        "the native binary wrote {} bytes and the fixture is {} — a difference of \
         exactly the number of 0x0A bytes in the image is text-mode stdio, and the \
         binary-mode guard in `__vyrn_write_stdout` is what stops it",
        native.stdout.len(),
        want.len()
    );
    assert!(
        native.stdout == want,
        "the native binary's bytes are not the fixture"
    );
}
