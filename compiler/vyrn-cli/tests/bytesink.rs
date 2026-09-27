//! Pins the byte sinks where they would break quietly: the program
//! exits 0 and only the bytes are wrong.

mod common;

use common::{scratch, vyrn};

/// The bytes a binary sink must survive. NUL makes this not a `String`; `0x0A`
/// is what Windows text mode rewrites; `0x0D 0x0A` is what a line-ending
/// normaliser collapses; `0xFF` is what a UTF-8 round trip replaces.
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

/// C stdio opens stdout in text mode on Windows, where `fwrite` turns `0x0A`
/// into `0x0D 0x0A`: right for `print`, corruption for a pixel row. The native
/// shim sets binary mode for the write; without it this test grows two bytes.
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

#[test]
fn write_file_bytes_carries_every_byte() {
    let dir = scratch("bytesink-file");
    let target = dir.join("hostile.bin");
    let src = dir.join("hostile.vyrn");
    // A relative path, run in the scratch directory: the host preopens only the
    // working directory, so it refuses an absolute path.
    let sink = "match writeFileBytes(\"hostile.bin\", raw) \
                { Ok(d) => print(\"wrote\"), Err(w) => print(w) }";
    std::fs::write(&src, hostile_program(sink)).unwrap();

    let out = vyrn()
        .current_dir(&*dir)
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

/// The native shim switches stdout to binary mode for a `writeStdout` and back.
/// Left binary, every later `print` would emit a bare `\n` on Windows.
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
    let flat = got.replace("\r\n", "\n");
    assert_eq!(
        flat, "before\n<>after\n",
        "the byte write did not land between the two prints: {got:?}"
    );
}

/// A PBM's header is text and its pixels are not; both leave through one call.
/// The fixture's twelfth byte is a NUL, which no `String` sink can carry.
#[test]
fn the_committed_mandelbrot_fixture_now_has_a_program() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture = root.join("bench/mandelbrot-200.expected");
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
