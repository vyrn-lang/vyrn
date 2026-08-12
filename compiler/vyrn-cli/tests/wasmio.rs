//! What the wasm runtime does when a host call FAILS.
//!
//! The parity harness runs every engine against a working filesystem, so every
//! I/O path it compares is a success path. The failures are the half that cannot
//! be reached that way on every platform — a full disk, a read-only mount, a
//! closed pipe — and `writeFile` reported `Ok(true)` for all three on this
//! backend for as long as it existed: `write_all` broke out of its loop on a
//! non-zero errno and returned nothing, and the caller stored `Ok` without
//! asking. Native has checked the same two conditions since it was written
//! (`__vyrn_write_file` compares `wrote != n` AND `fclose`).
//!
//! So the host is the fixture. The module imports five WASI functions for this
//! program, which is few enough to write by hand in the driver: the failing run
//! answers `ENOSPC` to every write that is not stdout or stderr, and the passing
//! run answers success. BOTH are asserted, because a `writeFile` that always
//! said `Err` would pass the first half alone.
//!
//! Skips without node, loudly under `VYRN_REQUIRE_TOOLS` — the same posture as
//! `memory.rs`, which measures the other thing only node can see here.

mod common;

use std::path::PathBuf;
use std::process::Command;

fn find_node() -> Option<PathBuf> {
    let node = std::env::var("VYRN_NODE").unwrap_or_else(|_| "node".into());
    let found = Command::new(&node)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| PathBuf::from(&node));
    common::require_tools("node", "VYRN_NODE", found)
}

const FIXTURE: &str = r#"fn main() -> Int64 {
    let r = match writeFile("out.txt", "hello") {
        Ok(b) => "wrote: \{b}",
        Err(e) => "error: \{e}",
    }
    print(r)
    return 0
}
"#;

/// A five-import WASI stub: one preopened directory, a `path_open` that always
/// succeeds, and an `fd_write` that fails for every fd but stdout and stderr when
/// asked to.
const HOST: &str = r#"const fs = require("fs");
const bytes = fs.readFileSync(process.argv[2]);
const mode = process.argv[3]; // "fail" | "ok"
const dec = new TextDecoder();
let inst;
const view = () => new DataView(inst.exports.memory.buffer);
const wasi = {
  fd_prestat_get: (fd, buf) => {
    if (fd !== 3) return 8; // EBADF: exactly one preopen
    const v = view();
    v.setUint8(buf, 0); // a directory
    v.setUint32(buf + 4, 1, true); // the length of "."
    return 0;
  },
  fd_prestat_dir_name: (fd, ptr) => {
    new Uint8Array(inst.exports.memory.buffer)[ptr] = 46; // "."
    return 0;
  },
  path_open: (fd, df, p, pl, of, rb, ri, ff, outFd) => {
    if (fd !== 3) return 8;
    view().setUint32(outFd, 7, true);
    return 0;
  },
  fd_write: (fd, iovs, n, nw) => {
    const v = view();
    let written = 0;
    let text = "";
    for (let i = 0; i < n; i++) {
      const base = v.getUint32(iovs + i * 8, true);
      const len = v.getUint32(iovs + i * 8 + 4, true);
      text += dec.decode(new Uint8Array(inst.exports.memory.buffer, base, len));
      written += len;
    }
    if (fd === 1 || fd === 2) {
      process.stdout.write(text);
      v.setUint32(nw, written, true);
      return 0;
    }
    if (mode === "fail") return 28; // ENOSPC
    v.setUint32(nw, written, true);
    return 0;
  },
  fd_close: () => 0,
  proc_exit: (code) => {
    process.exit(code);
  },
};
inst = new WebAssembly.Instance(new WebAssembly.Module(bytes), {
  wasi_snapshot_preview1: wasi,
});
inst.exports._start();
"#;

#[test]
fn a_failed_write_is_not_reported_as_a_written_file() {
    let Some(node) = find_node() else {
        eprintln!("NOTE: no node — the wasm write-failure path is unverified on this machine");
        return;
    };
    let dir = std::env::temp_dir().join(format!("vyrn-wasmio-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("wr.vyrn"), FIXTURE).unwrap();
    std::fs::write(dir.join("host.cjs"), HOST).unwrap();

    let build = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg("build")
        .arg(dir.join("wr.vyrn"))
        .args(["--target", "wasm", "-o"])
        .arg(dir.join("wr.wasm"))
        .output()
        .expect("vyrn build");
    assert!(
        build.status.success(),
        "build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = |mode: &str| -> String {
        let out = Command::new(&node)
            .arg(dir.join("host.cjs"))
            .arg(dir.join("wr.wasm"))
            .arg(mode)
            .output()
            .expect("node");
        assert!(
            out.status.success(),
            "the {mode} host exited {:?}:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
    };

    // The canonical wording, which the other two engines already print for the
    // same failure — this is a missing check, not a new message.
    assert_eq!(
        run("fail"),
        "error: cannot write `out.txt`\n",
        "a write that never landed must not read as a written file"
    );
    assert_eq!(
        run("ok"),
        "wrote: true\n",
        "and a write that did land must still read as one"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
