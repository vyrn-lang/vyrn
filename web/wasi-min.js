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
//     extern: {                                        // optional (RFC-0012)
//       jsLog: (msg) => console.log(msg),              //   String param decoded
//       jsNow: () => Date.now() / 1000,                //   Float64 return
//       jsAdd: (a, b) => a + b,                        //   Int64 -> BigInt args
//     },
//   });

const ERRNO_SUCCESS = 0;
const ERRNO_BADF = 8;
const ERRNO_SPIPE = 29; // stdout/stderr are not seekable

// --- minimal wasm reader: enough to recover the signatures of the module's
// `vela.*` imports so the extern glue (RFC-0012) can decode/encode arguments.
// The JS WebAssembly API exposes import module/name but not their types, so we
// read the type + import sections ourselves (same shape the codegen emits).
function readModuleImports(bytes) {
  const b = new Uint8Array(bytes);
  let i = 8; // skip magic + version
  const VT = { 0x7f: "i32", 0x7e: "i64", 0x7d: "f32", 0x7c: "f64" };
  const uleb = () => {
    let r = 0, s = 0;
    for (;;) {
      const x = b[i++];
      r |= (x & 0x7f) << s;
      if (!(x & 0x80)) return r >>> 0;
      s += 7;
    }
  };
  const name = () => {
    const n = uleb();
    const s = new TextDecoder().decode(b.subarray(i, i + n));
    i += n;
    return s;
  };
  const types = [];
  const imports = [];
  while (i < b.length) {
    const id = b[i++];
    const len = uleb();
    const end = i + len;
    if (id === 1) {
      const c = uleb();
      for (let t = 0; t < c; t++) {
        i++; // 0x60 func form
        const pc = uleb();
        const params = [];
        for (let p = 0; p < pc; p++) params.push(VT[b[i++]]);
        const rc = uleb();
        const results = [];
        for (let r = 0; r < rc; r++) results.push(VT[b[i++]]);
        types.push({ params, results });
      }
    } else if (id === 2) {
      const c = uleb();
      for (let m = 0; m < c; m++) {
        const mod = name();
        const fld = name();
        const kind = b[i++];
        if (kind === 0) {
          const ti = uleb();
          imports.push({ module: mod, field: fld, type: types[ti] });
        } else if (kind === 1) {
          i++; const lim = b[i++]; uleb(); if (lim === 1) uleb();
        } else if (kind === 2) {
          const lim = b[i++]; uleb(); if (lim === 1) uleb();
        } else if (kind === 3) {
          i += 2;
        }
      }
    }
    i = end;
  }
  return imports;
}

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

  // Build the `vela` import namespace (RFC-0012) from the host's extern hooks.
  // For each `vela.*` the module imports, wrap the user function so it sees
  // decoded values: a `String` param arrives as an (i32 ptr, i64 len) pair and
  // is decoded to a JS string; an `i64` param arrives as a `BigInt`; `i32`/
  // float params arrive as numbers. Return values are converted back to the
  // wasm result type (BigInt for i64, 0/1 for a Bool i32, numbers for floats).
  //
  // String detection is by ABI shape: the only Vela type that lowers to two
  // wasm words is `String` = (i32, i64), so an i32 immediately followed by an
  // i64 is decoded as one string argument. (A hypothetical `(Int32, Int64)`
  // adjacent pair would collide — none of the v1 externs use that; documented
  // in web/README.md.)
  const externHooks = hooks.extern || {};
  const wanted = readModuleImports(wasmBytes).filter((im) => im.module === "vela");
  const vela = {};
  for (const im of wanted) {
    const fn = externHooks[im.field];
    if (typeof fn !== "function") {
      const provided = Object.keys(externHooks);
      throw new Error(
        `module imports extern \`vela.${im.field}\`, but no such function was ` +
          `provided. Pass it via runVela(bytes, { extern: { ${im.field}: … } }). ` +
          `Provided: [${provided.join(", ")}]; wanted: [${wanted.map((w) => w.field).join(", ")}]`
      );
    }
    const params = im.type.params;
    const result = im.type.results[0]; // v1 externs return at most one value
    vela[im.field] = (...raw) => {
      const dec = new TextDecoder();
      const args = [];
      for (let k = 0; k < params.length; k++) {
        if (params[k] === "i32" && params[k + 1] === "i64") {
          // String: (ptr, len) -> decoded JS string; consume both words.
          const ptr = raw[k] >>> 0;
          const len = Number(raw[k + 1]);
          args.push(dec.decode(new Uint8Array(memory.buffer, ptr, len)));
          k++;
        } else {
          // i64 arrives as BigInt (pass through); i32/f32/f64 as numbers.
          args.push(raw[k]);
        }
      }
      const r = fn(...args);
      if (result === undefined) return undefined; // Unit
      if (result === "i64") return typeof r === "bigint" ? r : BigInt(Math.trunc(r));
      if (result === "i32") return typeof r === "boolean" ? (r ? 1 : 0) : Number(r) | 0;
      // f32 / f64 (or a string-return `ptr`, unsupported without an allocator).
      if (result === "f32" || result === "f64") return Number(r);
      throw new Error(`extern \`${im.field}\` returns unsupported wasm type \`${result}\``);
    };
  }

  const { instance } = await WebAssembly.instantiate(wasmBytes, {
    wasi_snapshot_preview1: wasi,
    vela,
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
