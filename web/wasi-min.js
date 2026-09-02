// Minimal WASI preview1 shim — just enough to run `vyrn build --target wasm`
// output in a browser. Hand-rolled, zero dependencies, matching the project's
// no-crates ethos.
//
// A compute-only vyrn module imports five preview1 functions (wasi-libc's
// stdio path): fd_write, fd_close, fd_seek, fd_fdstat_get, proc_exit. A module
// using input (RFC-0014: args/readLine/readFile/writeFile) additionally pulls
// in args_get, args_sizes_get, fd_read, fd_fdstat_set_flags, fd_prestat_get,
// fd_prestat_dir_name, path_open, and — from the RFC-0077 direct backend, which
// reads the RFC-0043 injected clock itself instead of through the C shim's
// `getenv` — environ_sizes_get and environ_get. Those get GRACEFUL DEGRADATION, not file
// access: the page has no argv and no filesystem, so `args()` sees zero
// arguments, `readLine()` sees immediate EOF (`None`), and `readFile`/
// `writeFile` fail with their canonical `Err` payloads — the module loads and
// runs, it just sees an empty world. Real browser input is the `extern` story
// (RFC-0012). Anything else is out of scope on purpose — if the import surface
// ever grows, the instantiate error names the missing function.
//
// Usage:
//   const { exitCode, stdout, stdoutRaw, stderr, exports } = await runVyrn(bytes, {
//     onStdout: line => ..., onStderr: line => ...,   // optional, per-chunk
//     extern: {                                        // optional (RFC-0012 M1)
//       jsLog: (msg) => console.log(msg),              //   String param decoded
//       jsNow: () => Date.now() / 1000,                //   Float64 return
//       jsAdd: (a, b) => a + b,                        //   Int64 -> BigInt args
//     },
//   });
//
// After _start runs `main` once, `exports` holds a wrapper per `export extern
// fn` (RFC-0012 M2): pass a JS string for a String param, get a decoded string
// back for a String return. A String is DECODED AND FREED (RFC-0089 M3b): a
// return is owned, so the buffer is this side's to release.
//
// EVERY type on that boundary comes from the module's own `vyrn:exports` custom
// section, which the compiler writes and this file reads. It has to: the wasm
// ABI cannot tell a `String` from a `Bool` from an `Int32` (all `i32`), nor a
// `String` argument from an `(Int32, Int64)` pair (both two slots). This file
// used to infer them from instruction shapes and from the JS runtime type of
// each argument at the call, which was wrong in both directions and documented
// as a caveat. A module with no section is refused by name rather than guessed
// at — `vyrn build --target wasm` writes one for every extern.

const ERRNO_SUCCESS = 0;
const ERRNO_BADF = 8;
const ERRNO_NOENT = 44; // no filesystem in a page: every path_open fails
const ERRNO_SPIPE = 29; // stdout/stderr are not seekable

// --- the module's declared boundary, read from its own `vyrn:exports` custom
// section (RFC-0012 M3).
//
// The wasm type section is not enough and never was: `String`, `Bool`, `Int32`
// and `UInt32` all cross as `i32`, and a `String` import occupies two slots that
// look exactly like an `(Int32, Int64)` pair. This file used to walk the module's
// bytes and infer signatures from that shape — an `i32` followed by an `i64` was
// taken to BE a String — which is a guess, and `web/README.md` carried the
// collision as a documented caveat. The compiler knows every one of these types
// exactly, so it writes them down and this reads them.
//
// WHICH functions exist is a different question, and the platform already
// answers it: `WebAssembly.Module.imports` for the import side,
// `instance.exports` for the export side. So no section walking survives here —
// `WebAssembly.Module.customSections` hands over the payload.
//
// Payload (version 2), all counts and lengths uleb128:
//   u8 version, uleb exportCount, { name, ret, uleb paramCount, param… }…,
//   uleb importCount, { name, ret, uleb paramCount, param… }…
// A `kind` is one of: unit bool string i32 u32 i64 u64 f32 f64.
const ABI_SECTION = "vyrn:exports";

function readAbi(module) {
  const secs = WebAssembly.Module.customSections(module, ABI_SECTION);
  const abi = { exports: new Map(), imports: new Map() };
  if (secs.length === 0) return abi;
  const b = new Uint8Array(secs[0]);
  const dec = new TextDecoder();
  let i = 0;
  const uleb = () => {
    let r = 0, s = 0;
    for (;;) {
      const x = b[i++];
      r |= (x & 0x7f) << s;
      if (!(x & 0x80)) return r >>> 0;
      s += 7;
    }
  };
  const str = () => {
    const n = uleb();
    const s = dec.decode(b.subarray(i, i + n));
    i += n;
    return s;
  };
  const version = b[i++];
  if (version !== 2) {
    throw new Error(
      `this module declares its boundary in \`${ABI_SECTION}\` version ` +
        `${version}, and this runtime reads version 2. Rebuild it with a ` +
        `matching \`vyrn build --target wasm\`.`
    );
  }
  for (const into of [abi.exports, abi.imports]) {
    const count = uleb();
    for (let e = 0; e < count; e++) {
      const name = str();
      const ret = str();
      const params = [];
      const pc = uleb();
      for (let p = 0; p < pc; p++) params.push(str());
      into.set(name, { params, ret });
    }
  }
  return abi;
}

/// The signature a module must have declared, or a refusal naming what is
/// missing. A `vyrn build --target wasm` writes every one of these, so a miss is
/// a module from somewhere else — and guessing on its behalf is what this file
/// stopped doing.
function declared(map, name, what) {
  const sig = map.get(name);
  if (!sig) {
    throw new Error(
      `\`${name}\` is ${what} of this module, but its \`${ABI_SECTION}\` section ` +
        `does not declare a signature for it. Build it with ` +
        `\`vyrn build --target wasm\`, which writes one for every extern.`
    );
  }
  return sig;
}

/** Thrown by proc_exit to unwind out of _start; carries the exit code. */
class VyrnExit {
  constructor(code) {
    this.code = code;
  }
}

export async function runVyrn(wasmBytes, hooks = {}) {
  let memory; // set after instantiate
  // One decoder per stream, both decoding in streaming mode: a multi-byte
  // UTF-8 character split across iovecs must not turn into U+FFFD, and a
  // single shared decoder's pending state would bleed one stream into the
  // other.
  const stdoutDec = new TextDecoder();
  const stderrDec = new TextDecoder();
  let stdout = "";
  let stderr = "";
  // Every stdout write, as bytes, in order (RFC-0111). The page joins them
  // for a caller that wants the file the program actually wrote.
  const stdoutBytes = [];

  // fd_write(fd, iovs_ptr, iovs_len, nwritten_ptr) -> errno
  // Decodes the iovec array out of linear memory and appends to the right
  // stream. Chunks decode with `{ stream: true }` so a character split across
  // iovecs carries over instead of becoming U+FFFD; the final flush decode
  // closes the call boundary, so a truncated tail is still emitted before
  // this write returns. wasi-libc buffers internally, so chunks are usually
  // whole lines.
  //
  // THE BYTES ARE HANDED OVER TOO (RFC-0111). `writeStdout` exists so a program
  // can emit bytes that are NOT text — a packed pixel row, a binary PBM — and
  // decoding those to a string replaces every invalid sequence with U+FFFD
  // before any consumer sees them. That is silent corruption in the one engine
  // whose whole job here is to show what the program wrote. So `onStdout` and
  // `onStderr` receive `(text, bytes)`: `text` is what it always was, and
  // `bytes` is a COPY of exactly what the module wrote, in order. A caller that
  // only prints ignores the second argument and behaves as before; a caller
  // handling binary uses it and never looks at `text`.
  //
  // A copy, not a view: `memory.buffer` is detached by any later growth, so a
  // `Uint8Array` over it is only valid until the next allocation.
  function fd_write(fd, iovsPtr, iovsLen, nwrittenPtr) {
    if (fd !== 1 && fd !== 2) return ERRNO_BADF;
    const view = new DataView(memory.buffer);
    const dec = fd === 1 ? stdoutDec : stderrDec;
    let written = 0;
    let text = "";
    const parts = [];
    for (let i = 0; i < iovsLen; i++) {
      const base = view.getUint32(iovsPtr + i * 8, true);
      const len = view.getUint32(iovsPtr + i * 8 + 4, true);
      const raw = new Uint8Array(memory.buffer, base, len);
      parts.push(raw.slice());
      text += dec.decode(raw, { stream: true });
      written += len;
    }
    text += dec.decode();
    const bytes = new Uint8Array(written);
    let at = 0;
    for (const p of parts) {
      bytes.set(p, at);
      at += p.length;
    }
    if (fd === 1) {
      stdout += text;
      stdoutBytes.push(bytes);
      if (hooks.onStdout) hooks.onStdout(text, bytes);
    } else {
      stderr += text;
      if (hooks.onStderr) hooks.onStderr(text, bytes);
    }
    view.setUint32(nwrittenPtr, written, true);
    return ERRNO_SUCCESS;
  }

  // fd_fdstat_get(fd, buf) -> errno — report a character device (a tty),
  // which is what wasi-libc expects of stdout/stderr; zero flags/rights.
  function fd_fdstat_get(fd, buf) {
    if (fd !== 0 && fd !== 1 && fd !== 2) return ERRNO_BADF;
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
      throw new VyrnExit(code);
    },

    // ---- time & randomness (RFC-0043): host-provided, backed by the browser --
    // The C shim's now()/monotonic()/randomSeed() reach WASI clock_time_get /
    // random_get (via wasi-libc's timespec_get / getentropy). Back them with the
    // browser clock and CSPRNG. A page has no env, so the fixed-clock injection
    // (VYRN_FIXED_TIME / VYRN_FIXED_SEED) can be supplied here instead via
    // hooks.fixedTime / hooks.fixedSeed for reproducible demos (the parity
    // harness fixes the wasm column through wasmtime's --env, not this shim).
    //
    // clock_time_get(clockId, precision, outPtr) -> errno. clockId 0 = REALTIME
    // (ns since the Unix epoch, from Date.now()), else MONOTONIC (performance.now).
    clock_time_get: (clockId, _precision, outPtr) => {
      const view = new DataView(memory.buffer);
      let ns;
      if (clockId === 0) {
        ns =
          typeof hooks.fixedTime === "number" || typeof hooks.fixedTime === "bigint"
            ? BigInt(hooks.fixedTime) * 1000000n // ms -> ns
            : BigInt(Math.round(Date.now() * 1e6));
      } else {
        ns = BigInt(Math.round(performance.now() * 1e6));
      }
      view.setBigUint64(outPtr, ns, true);
      return ERRNO_SUCCESS;
    },
    // random_get(buf, len) -> errno: fill linear memory with CSPRNG bytes.
    // hooks.fixedSeed, when set, makes the fill deterministic (a tiny SplitMix64
    // stream over the buffer) so a page can reproduce a seeded run.
    random_get: (buf, len) => {
      const bytes = new Uint8Array(memory.buffer, buf, len);
      if (typeof hooks.fixedSeed === "number" || typeof hooks.fixedSeed === "bigint") {
        let s = BigInt.asUintN(64, BigInt(hooks.fixedSeed));
        for (let k = 0; k < len; k++) {
          s = BigInt.asUintN(64, s + 0x9e3779b97f4a7c15n);
          let z = s;
          z = BigInt.asUintN(64, (z ^ (z >> 30n)) * 0xbf58476d1ce4e5b9n);
          z = BigInt.asUintN(64, (z ^ (z >> 27n)) * 0x94d049bb133111ebn);
          z = z ^ (z >> 31n);
          bytes[k] = Number(z & 0xffn);
        }
      } else {
        crypto.getRandomValues(bytes);
      }
      return ERRNO_SUCCESS;
    },

    // ---- input I/O (RFC-0014): graceful degradation, not file access -------
    // A page has no argv: zero arguments, zero buffer bytes.
    args_sizes_get: (argcPtr, bufSizePtr) => {
      const view = new DataView(memory.buffer);
      view.setUint32(argcPtr, 0, true);
      view.setUint32(bufSizePtr, 0, true);
      return ERRNO_SUCCESS;
    },
    args_get: () => ERRNO_SUCCESS, // argc is 0, so there is nothing to write
    // A page has no environment either. Only the RFC-0077 direct backend asks:
    // its standalone modules read `VYRN_FIXED_TIME`/`VYRN_FIXED_SEED` themselves
    // rather than through the C shim's `getenv`, so an empty environment is what
    // sends them to `clock_time_get`/`random_get` — where hooks.fixedTime and
    // hooks.fixedSeed above are the page's own injection point.
    environ_sizes_get: (countPtr, bufSizePtr) => {
      const view = new DataView(memory.buffer);
      view.setUint32(countPtr, 0, true);
      view.setUint32(bufSizePtr, 0, true);
      return ERRNO_SUCCESS;
    },
    environ_get: () => ERRNO_SUCCESS, // the count is 0, so there is nothing to write
    // Reading stdin yields immediate EOF (0 bytes) → `readLine()` is `None`.
    fd_read: (fd, _iovsPtr, _iovsLen, nreadPtr) => {
      if (fd !== 0) return ERRNO_BADF;
      new DataView(memory.buffer).setUint32(nreadPtr, 0, true);
      return ERRNO_SUCCESS;
    },
    fd_fdstat_set_flags: () => ERRNO_SUCCESS,
    // No preopened directories: BADF ends wasi-libc's preopen scan (fd 3…).
    fd_prestat_get: () => ERRNO_BADF,
    fd_prestat_dir_name: () => ERRNO_BADF,
    // No filesystem: every open fails, so `readFile`/`writeFile` return their
    // canonical `Err` payloads in-page (never a crash).
    path_open: () => ERRNO_NOENT,
    // RFC-0044: a module using `renameFile`/`writeAtomic`/`fsyncFile` pulls in
    // path_rename / fd_sync. With no filesystem in a page, rename fails (NOENT →
    // canonical `Err`); fsync never reaches an open fd (the fopen already
    // failed), so its stub is a harmless success. Provided so the module still
    // instantiates and degrades gracefully rather than failing to link.
    path_rename: () => ERRNO_NOENT,
    fd_sync: () => ERRNO_SUCCESS,
    // RFC-0125 M5: `listDir` pulls in fd_readdir. Its open already failed
    // (path_open above), so the stub is never reached; it exists so the
    // module links.
    fd_readdir: () => ERRNO_BADF,
  };

  // Build the `vyrn` import namespace (RFC-0012) from the host's extern hooks.
  // WHICH externs the module wants is `WebAssembly.Module.imports`; WHAT each one
  // takes and returns is the module's own `vyrn:exports` section. Each hook is
  // wrapped so it sees decoded values: a `String` param occupies two wasm slots
  // (ptr, len) and arrives as a JS string, an `Int64`/`UInt64` as a `BigInt`,
  // everything else as a number or a boolean.
  const module = await WebAssembly.compile(wasmBytes);
  const abi = readAbi(module);
  const externHooks = hooks.extern || {};
  const wanted = WebAssembly.Module.imports(module)
    .filter((im) => im.module === "vyrn")
    .map((im) => im.name);
  const vyrn = {};
  for (const field of wanted) {
    const fn = externHooks[field];
    if (typeof fn !== "function") {
      const provided = Object.keys(externHooks);
      throw new Error(
        `module imports extern \`vyrn.${field}\`, but no such function was ` +
          `provided. Pass it via runVyrn(bytes, { extern: { ${field}: … } }). ` +
          `Provided: [${provided.join(", ")}]; wanted: [${wanted.join(", ")}]`
      );
    }
    const { params, ret } = declared(abi.imports, field, "an extern import");
    vyrn[field] = (...raw) => {
      const dec = new TextDecoder();
      const args = [];
      let slot = 0;
      for (const t of params) {
        if (t === "string") {
          // A String import crosses as (i32 ptr, i64 len) — two slots, one
          // argument. This is the pair the old shape-guess collided with an
          // `(Int32, Int64)` on; the declaration says which it is.
          const ptr = raw[slot++] >>> 0;
          const len = Number(raw[slot++]);
          args.push(dec.decode(new Uint8Array(memory.buffer, ptr, len)));
        } else if (t === "bool") {
          args.push(raw[slot++] !== 0);
        } else if (t === "u32") {
          args.push(raw[slot++] >>> 0);
        } else if (t === "u64") {
          args.push(BigInt.asUintN(64, raw[slot++]));
        } else {
          // i32/i64/f32/f64 arrive in their natural JS form already (i64 is a
          // BigInt). `opaque` is a type this runtime has never seen; passing it
          // through unchanged is the only honest thing left.
          args.push(raw[slot++]);
        }
      }
      const r = fn(...args);
      if (ret === "unit") return undefined;
      if (ret === "i64") return typeof r === "bigint" ? r : BigInt(Math.trunc(r));
      if (ret === "u64")
        return BigInt.asIntN(64, typeof r === "bigint" ? r : BigInt(Math.trunc(r)));
      if (ret === "bool") return r ? 1 : 0;
      if (ret === "i32" || ret === "u32") return Number(r) | 0;
      if (ret === "f32" || ret === "f64") return Number(r);
      // `string` needs an allocation inside the module, which an import cannot
      // do (RFC-0012 stage 1.5); `opaque` has no wire form at all.
      throw new Error(`extern \`${field}\` returns unsupported type \`${ret}\``);
    };
  }

  const instance = await WebAssembly.instantiate(module, {
    wasi_snapshot_preview1: wasi,
    vyrn,
  });
  memory = instance.exports.memory;

  // --- string helpers over linear memory (RFC-0012 M2 export ABI) ------------
  // A String crosses into an exported Vyrn function as a single pointer to
  // NUL-terminated UTF-8. That is unchanged. What the JS side now also writes is
  // the eight-byte `{ len, cap }` header that sits in FRONT of every Vyrn String
  // (RFC-0089 M1a) — the module reads `s.byteLength` out of it, so a String
  // built here without one would read a length out of whatever preceded it.
  //
  // The conversion belongs here, at the boundary, and nowhere else: the ABI at
  // the edge is still a bare pointer.
  //
  // This is the asymmetry vs. an IMPORT (M1), where a String is a (ptr, len)
  // pair — an import can't allocate inside the module, but an exported call can.
  const STR_HDR = 8;
  const enc = new TextEncoder();
  const encodeString = (s) => {
    const bytes = enc.encode(s);
    if (typeof instance.exports.__vyrn_malloc !== "function") {
      throw new Error(
        "a String argument needs the module's allocator, but `__vyrn_malloc` is " +
          "not exported. Rebuild: vyrn exports it whenever an `export extern fn` " +
          "takes a String parameter."
      );
    }
    const base = Number(
      instance.exports.__vyrn_malloc(BigInt(STR_HDR + bytes.length + 1))
    );
    const view = new DataView(memory.buffer);
    view.setUint32(base, bytes.length, true); // len
    view.setUint32(base + 4, bytes.length, true); // cap — non-zero: heap, freeable
    const ptr = base + STR_HDR;
    new Uint8Array(memory.buffer).set(bytes, ptr);
    new Uint8Array(memory.buffer)[ptr + bytes.length] = 0; // NUL
    return ptr;
  };
  // Decode a returned String pointer: scan linear memory for the NUL byte.
  // The scan is bounded at the end of memory — a module that returns a bogus
  // pointer is a bug to report, not a page to hang. Boundary defense is this
  // shim's job, so the failure is a catchable Error naming the export.
  const decodeCString = (ptr, field) => {
    const p = Number(ptr) >>> 0;
    const view = new Uint8Array(memory.buffer);
    let e = p;
    while (e < view.length && view[e] !== 0) e++;
    if (e >= view.length) {
      throw new Error(
        `\`${field}\` returned a String with no NUL terminator before the end ` +
          `of linear memory — the module handed back a bogus pointer.`
      );
    }
    return new TextDecoder().decode(view.subarray(p, e));
  };

  // --- wrap exported-extern functions (RFC-0012 M2) --------------------------
  // For each `export extern fn`, expose a pre-wrapped callable on the returned
  // `exports`. Both halves come from the module's declaration now: an argument
  // is encoded as the PARAMETER says (a `string` slot allocates and copies, a
  // `bool` becomes 0/1), and a result is decoded as the RETURN says.
  //
  // Arguments used to be encoded by the JS runtime type of whatever the caller
  // passed, because the wasm slot is lossy — `String`, `Bool`, `Int32` and
  // `UInt32` are all `i32`. That made `greet(42)` hand a `String` parameter the
  // number 42 as a pointer, and `wantsInt("7")` allocate a string and pass its
  // address as a number, both without a word of complaint. A declared `string`
  // parameter now takes a JS string or refuses.
  const RESERVED = new Set(["memory", "_start", "__vyrn_malloc", "__vyrn_free"]);
  const wrappedExports = {};
  for (const [field, { params, ret }] of abi.exports) {
    if (RESERVED.has(field) || field.startsWith("__")) continue;
    const raw = instance.exports[field];
    // Declared but swept, or renamed: the section lists what the source says,
    // and only what the instance actually exports is callable.
    if (typeof raw !== "function") continue;
    wrappedExports[field] = (...jsArgs) => {
      const call = [];
      // Every buffer this call allocated inside the module. The CALLER owns a
      // String argument (RFC-0012), and across this boundary the caller is here —
      // so this list is what has to be handed back. Before RFC-0077 M6 there was
      // nothing to hand it to: `__vyrn_malloc` was the only allocator symbol a
      // module exported, and 20000 keystrokes cost 18 MB.
      // A Set, not a list: an export declared `consume s: String` may hand back
      // the very pointer it was given, and one block must be released once.
      const owned = new Set();
      for (let k = 0; k < params.length; k++) {
        const t = params[k];
        const a = jsArgs[k];
        if (t === "string") {
          if (typeof a !== "string") {
            throw new Error(
              `\`${field}\` parameter ${k + 1} is declared \`String\`, but a ` +
                `${typeof a} was passed. The module would read it as a pointer.`
            );
          }
          const p = encodeString(a);
          owned.add(p);
          call.push(p);
        } else if (t === "i64" || t === "u64") {
          call.push(typeof a === "bigint" ? a : BigInt(Math.trunc(Number(a))));
        } else if (t === "bool") {
          call.push(a ? 1 : 0);
        } else if (t === "f32" || t === "f64") {
          call.push(Number(a));
        } else {
          call.push(Number(a) | 0); // i32 / u32
        }
      }
      let out;
      try {
        const r = raw(...call);
        // Decoded BEFORE the release, because an export may return the pointer it
        // was given and `free` writes its list link into the block.
        if (ret === "unit") out = undefined;
        else if (ret === "string") {
          out = decodeCString(r, field);
          // A returned String is the CALLER's (RFC-0089 rule 3), and across
          // this boundary the caller is here. The module refuses to compile a
          // lend out of an `export extern fn` — module state, a projection —
          // so the pointer is either a heap block this release reclaims or a
          // data-segment literal, which sits below `HEAP_BASE` and which
          // `__vyrn_free` therefore ignores.
          owned.add(Number(r) >>> 0);
        } else if (ret === "bool") out = r !== 0;
        // An unsigned result is unsigned: a wasm `i32` reaches JS sign-extended,
        // and a `UInt32` over 2^31 used to come back negative because nothing
        // said it was unsigned.
        else if (ret === "u32") out = r >>> 0;
        else if (ret === "u64") out = BigInt.asUintN(64, r);
        else out = r; // i32 / i64 / f32 / f64
      } finally {
        // A trap leaves the module unusable, but a `panic` the page catches does
        // not, so the release runs on both paths.
        // The block base is the pointer less its String header (RFC-0089 M1a),
        // which is what `__vyrn_malloc` handed out.
        const free = instance.exports.__vyrn_free;
        if (typeof free === "function") for (const p of owned) free(p - STR_HDR);
      }
      return out;
      // A returned String used to leak here, and the reason was that ownership
      // "differs per function and nothing crosses this boundary that says which"
      // (RFC-0087 §9a). Rule 3 removed the question rather than answering it: a
      // return is owned, and Phase 6 made an `export extern fn` that lends fail
      // to compile. So nothing has to cross — the fact is true of every export.
    };
  }

  let exitCode = 0;
  try {
    instance.exports._start();
  } catch (e) {
    if (e instanceof VyrnExit) {
      exitCode = e.code;
    } else {
      throw e; // a genuine trap (unreachable, OOB) — surface it
    }
  }
  // `exports`: the exported-extern functions, callable AFTER `_start` ran `main`
  // once — the instance stays alive (RFC-0012 M2 post-`_start` callability).
  // `memory`: the instance's linear memory. `memory.buffer.byteLength` is the
  // only way to see whether the module reclaims what it allocates, which is why
  // nothing saw that it did not (RFC-0077 M6).
  // `stdoutRaw` is the whole of standard output as BYTES — what `stdout`
  // would be if it were not a string. For a text program the two say the
  // same thing; for one that called `writeStdout` with a packed row, only
  // this one is faithful (RFC-0111).
  const total = stdoutBytes.reduce((n, b) => n + b.length, 0);
  const stdoutRaw = new Uint8Array(total);
  let rawAt = 0;
  for (const b of stdoutBytes) {
    stdoutRaw.set(b, rawAt);
    rawAt += b.length;
  }
  return { exitCode, stdout, stdoutRaw, stderr, exports: wrappedExports, memory };
}
