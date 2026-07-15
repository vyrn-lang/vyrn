// Minimal WASI preview1 shim — just enough to run `velac build --target wasm`
// output in a browser. Hand-rolled, zero dependencies, ~100 lines, matching
// the project's no-crates ethos.
//
// A velac module imports exactly five preview1 functions (wasi-libc's stdio
// path): fd_write, fd_close, fd_seek, fd_fdstat_get, proc_exit. Everything
// else is out of scope on purpose — if the import surface ever grows, the
// instantiate error names the missing function.
//
// Usage:
//   const { exitCode, stdout, stderr } = await runVela(bytes, {
//     onStdout: line => ..., onStderr: line => ...,   // optional, per-chunk
//   });

const ERRNO_SUCCESS = 0;
const ERRNO_BADF = 8;
const ERRNO_SPIPE = 29; // stdout/stderr are not seekable

/** Thrown by proc_exit to unwind out of _start; carries the exit code. */
class VelaExit {
  constructor(code) {
    this.code = code;
  }
}

export async function runVela(wasmBytes, hooks = {}) {
  let memory; // set after instantiate
  const dec = new TextDecoder();
  let stdout = "";
  let stderr = "";

  // fd_write(fd, iovs_ptr, iovs_len, nwritten_ptr) -> errno
  // Decodes the iovec array out of linear memory and appends to the right
  // stream. wasi-libc buffers internally, so chunks are usually whole lines.
  function fd_write(fd, iovsPtr, iovsLen, nwrittenPtr) {
    if (fd !== 1 && fd !== 2) return ERRNO_BADF;
    const view = new DataView(memory.buffer);
    let written = 0;
    let text = "";
    for (let i = 0; i < iovsLen; i++) {
      const base = view.getUint32(iovsPtr + i * 8, true);
      const len = view.getUint32(iovsPtr + i * 8 + 4, true);
      text += dec.decode(new Uint8Array(memory.buffer, base, len));
      written += len;
    }
    if (fd === 1) {
      stdout += text;
      if (hooks.onStdout) hooks.onStdout(text);
    } else {
      stderr += text;
      if (hooks.onStderr) hooks.onStderr(text);
    }
    view.setUint32(nwrittenPtr, written, true);
    return ERRNO_SUCCESS;
  }

  // fd_fdstat_get(fd, buf) -> errno — report a character device (a tty),
  // which is what wasi-libc expects of stdout/stderr; zero flags/rights.
  function fd_fdstat_get(fd, buf) {
    if (fd > 2) return ERRNO_BADF;
    const view = new DataView(memory.buffer);
    view.setUint8(buf, 2); // filetype: character_device
    view.setUint8(buf + 1, 0);
    view.setUint16(buf + 2, 0, true); // fdflags
    view.setUint32(buf + 4, 0, true); // padding
    view.setBigUint64(buf + 8, 0n, true); // rights_base
    view.setBigUint64(buf + 16, 0n, true); // rights_inheriting
    return ERRNO_SUCCESS;
  }

  const wasi = {
    fd_write,
    fd_fdstat_get,
    fd_close: () => ERRNO_SUCCESS,
    fd_seek: () => ERRNO_SPIPE,
    proc_exit: (code) => {
      throw new VelaExit(code);
    },
  };

  const { instance } = await WebAssembly.instantiate(wasmBytes, {
    wasi_snapshot_preview1: wasi,
  });
  memory = instance.exports.memory;

  let exitCode = 0;
  try {
    instance.exports._start();
  } catch (e) {
    if (e instanceof VelaExit) {
      exitCode = e.code;
    } else {
      throw e; // a genuine trap (unreachable, OOB) — surface it
    }
  }
  return { exitCode, stdout, stderr };
}
