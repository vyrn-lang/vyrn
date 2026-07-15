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
//   const { exitCode, stdout, stderr, exports } = await runVela(bytes, {
//     onStdout: line => ..., onStderr: line => ...,   // optional, per-chunk
//     extern: {                                        // optional (RFC-0012 M1)
//       jsLog: (msg) => console.log(msg),              //   String param decoded
//       jsNow: () => Date.now() / 1000,                //   Float64 return
//       jsAdd: (a, b) => a + b,                        //   Int64 -> BigInt args
//     },
//     exportReturns: { greet: "string" },              // optional (M2): name an
//   });                                                //   i32 result's real type
//
// After _start runs `main` once, `exports` holds a wrapper per `export extern
// fn` (RFC-0012 M2): pass a JS string for a String param, get a decoded string
// back for a String return. `exportReturns` disambiguates an `i32` result
// (String / Bool / Int32 share the wasm type) — "string" or "bool", else number.

const ERRNO_SUCCESS = 0;
const ERRNO_BADF = 8;
const ERRNO_SPIPE = 29; // stdout/stderr are not seekable

// --- minimal wasm reader: recover the signatures of the module's `vela.*`
// imports (so the extern-import glue can decode/encode arguments, RFC-0012 M1)
// AND of its exported functions (so the export glue can wrap them, M2). The JS
// WebAssembly API exposes names but not types, so we read the type, import,
// function, and export sections ourselves (the same shape the codegen emits).
//
// Function index space: imported functions occupy the first indices (in import
// order), then the module's own defined functions (in function-section order).
// An export names a function by that combined index; we map it back through the
// function section to a type index to recover the signature.
function readModule(bytes) {
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
  const funcSec = []; // type index of each DEFINED function, in order
  const rawExports = []; // { field, kind, index }
  let importedFuncs = 0;
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
          importedFuncs++;
        } else if (kind === 1) {
          i++; const lim = b[i++]; uleb(); if (lim === 1) uleb();
        } else if (kind === 2) {
          const lim = b[i++]; uleb(); if (lim === 1) uleb();
        } else if (kind === 3) {
          i += 2;
        }
      }
    } else if (id === 3) {
      const c = uleb();
      for (let f = 0; f < c; f++) funcSec.push(uleb());
    } else if (id === 7) {
      const c = uleb();
      for (let e = 0; e < c; e++) {
        const fld = name();
        const kind = b[i++];
        const index = uleb();
        rawExports.push({ field: fld, kind, index });
      }
    }
    i = end;
  }
  // Resolve each function export (kind 0) to its signature via the function
  // section; non-function exports (memory, globals) carry no `type`.
  const exports = rawExports.map((e) => {
    if (e.kind === 0 && e.index >= importedFuncs) {
      const ti = funcSec[e.index - importedFuncs];
      return { ...e, type: types[ti] };
    }
    return e;
  });
  return { imports, exports };
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
  const mod = readModule(wasmBytes);
  const wanted = mod.imports.filter((im) => im.module === "vela");
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

  // --- string helpers over linear memory (RFC-0012 M2 export ABI) ------------
  // A String crosses into an exported Vela function as a single pointer: the JS
  // side allocates `len + 1` bytes via the module's own `__vela_malloc`, copies
  // UTF-8, and writes a NUL terminator (a Vela String is a NUL-terminated ptr).
  // This is the asymmetry vs. an IMPORT (M1), where a String is a (ptr, len)
  // pair — an import can't allocate inside the module, but an exported call can.
  const enc = new TextEncoder();
  const encodeString = (s) => {
    const bytes = enc.encode(s);
    if (typeof instance.exports.__vela_malloc !== "function") {
      throw new Error(
        "a String argument needs the module's allocator, but `__vela_malloc` is " +
          "not exported. Rebuild: velac exports it whenever an `export extern fn` " +
          "takes a String parameter."
      );
    }
    const ptr = Number(instance.exports.__vela_malloc(BigInt(bytes.length + 1)));
    const view = new Uint8Array(memory.buffer);
    view.set(bytes, ptr);
    view[ptr + bytes.length] = 0; // NUL
    return ptr;
  };
  // Decode a returned String pointer: scan linear memory for the NUL byte.
  const decodeCString = (ptr) => {
    const p = Number(ptr) >>> 0;
    const view = new Uint8Array(memory.buffer);
    let e = p;
    while (view[e] !== 0) e++;
    return new TextDecoder().decode(view.subarray(p, e));
  };

  // --- wrap exported-extern functions (RFC-0012 M2) --------------------------
  // For each `export extern fn`, expose a pre-wrapped callable on the returned
  // `exports`. Argument encoding is by the ARG's JS type (the wasm export ABI is
  // lossy: String / Bool / Int32 all lower to `i32`, so an i32 slot is decided
  // at the call by the value passed — a JS string is allocated + copied, a
  // boolean becomes 0/1, a number stays an i32; an i64 slot takes a BigInt).
  // A result is likewise ambiguous for `i32`; `hooks.exportReturns[name]` may
  // name it `"string"` (NUL-decoded) or `"bool"`, else an i32 result is a
  // number. `i64` results are BigInt, floats are numbers. See web/README.md.
  const returnHints = hooks.exportReturns || {};
  const RESERVED = new Set(["memory", "_start", "__vela_malloc"]);
  const wrappedExports = {};
  for (const ex of mod.exports) {
    if (ex.kind !== 0 || !ex.type) continue; // functions only
    if (RESERVED.has(ex.field) || ex.field.startsWith("__")) continue;
    const params = ex.type.params;
    const result = ex.type.results[0];
    const hint = returnHints[ex.field];
    const raw = instance.exports[ex.field];
    if (typeof raw !== "function") continue;
    wrappedExports[ex.field] = (...jsArgs) => {
      const call = [];
      for (let k = 0; k < params.length; k++) {
        const t = params[k];
        const a = jsArgs[k];
        if (t === "i64") {
          call.push(typeof a === "bigint" ? a : BigInt(Math.trunc(Number(a))));
        } else if (t === "i32") {
          if (typeof a === "string") call.push(encodeString(a));
          else if (typeof a === "boolean") call.push(a ? 1 : 0);
          else call.push(Number(a) | 0);
        } else {
          call.push(Number(a)); // f32 / f64
        }
      }
      const r = raw(...call);
      if (result === undefined) return undefined; // Unit
      if (result === "i64") return r; // BigInt
      if (result === "i32") {
        if (hint === "string") return decodeCString(r);
        if (hint === "bool") return r !== 0;
        return r;
      }
      return r; // f32 / f64 — number
    };
  }

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
  // `exports`: the exported-extern functions, callable AFTER `_start` ran `main`
  // once — the instance stays alive (RFC-0012 M2 post-`_start` callability).
  return { exitCode, stdout, stderr, exports: wrappedExports };
}
