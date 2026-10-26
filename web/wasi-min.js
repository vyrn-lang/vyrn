// Minimal WASI preview1 shim — just enough to run `vyrn build --target wasm`
// output in a browser. Hand-rolled, zero dependencies, matching the project's
// no-crates ethos.
//
// A compute-only vyrn module imports five preview1 functions (wasi-libc's stdio
// path): fd_write, fd_close, fd_seek, fd_fdstat_get, proc_exit. A module using
// input (args/readLine/readFile/writeFile) additionally pulls in args_get,
// args_sizes_get, fd_read, fd_fdstat_set_flags, fd_prestat_get,
// fd_prestat_dir_name, path_open, and — from the direct wasm backend, which
// reads the injected clock itself instead of through the C shim's `getenv` —
// environ_sizes_get and environ_get. Those get GRACEFUL DEGRADATION, not file
// access: the page has no argv and no filesystem, so `args()` sees zero
// arguments and `readFile`/`writeFile` fail with their canonical `Err` payloads
// — the module loads and runs, it just sees an empty world. STDIN is the one
// exception, because a page can have it: `hooks.stdin` is served to `fd_read`,
// and `readLine()` reads EOF only when the host gave none. Real browser input
// is the `extern` story. Anything else is out of scope on purpose — if the
// import surface ever grows, the instantiate error names the missing function.
//
// A module that waits imports `vyrn_wait.sleep`, `vyrn_wait.cancel` and
// `vyrn_wait.done` and exports `__vyrn_wake`: `runVyrn` settles every timer
// before it resolves, as `vyrn run` does. A waiting export's wrapper returns a
// promise of its result, which `vyrn_wait.done` delivers when its task ends.
// A module that sends requests imports `vyrn_wait.fetch` too, which the page's
// `fetch` answers; `runVyrn` settles every request as it settles every timer.
//
// Usage:
//   const { exitCode, stdout, stdoutRaw, stderr, exports } = await runVyrn(bytes, {
//     onStdout: line => ..., onStderr: line => ...,   // optional, per-chunk
//     stdin: "one line then another",              // optional; EOF without it
//     extern: {                                        // optional
//       jsLog: (msg) => console.log(msg),              //   String param decoded
//       jsNow: () => Date.now() / 1000,                //   Float64 return
//       jsAdd: (a, b) => a + b,                        //   Int64 -> BigInt args
//     },
//   });
//
// After _start runs `main` once, `exports` holds a wrapper per `export extern
// fn`: pass a JS string for a String param, get a decoded string
// back for a String return. A String is DECODED AND FREED: a
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
// section.
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
// Payload (version 4), all counts and lengths uleb128:
//   u8 version, uleb exportCount, { name, ret, uleb paramCount, param..., u8 waits }...,
//   uleb importCount, { name, ret, uleb paramCount, param… }…
// A `kind` is one of: unit bool string i32 u32 i64 u64 f32 f64. An export
// with `waits` 1 takes the host's id for the task it starts before its
// declared parameters and returns nothing; its result comes through
// `vyrn_wait.done(task, status, result)`.
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
  if (version !== 4) {
    throw new Error(
      `this module declares its boundary in \`${ABI_SECTION}\` version ` +
        `${version}, and this runtime reads version 4. Rebuild it with a ` +
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
      const waits = into === abi.exports && b[i++] === 1;
      into.set(name, { params, ret, waits });
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
  // Every stdout write, as bytes, in order. The page joins them
  // for a caller that wants the file the program actually wrote.
  const stdoutBytes = [];
  // Standard input, and how far the module has read into it. `hooks.stdin` is a
  // string or a Uint8Array; nothing is EOF from the first read.
  const stdinBytes =
    typeof hooks.stdin === "string"
      ? new TextEncoder().encode(hooks.stdin)
      : hooks.stdin instanceof Uint8Array
        ? hooks.stdin
        : new Uint8Array(0);
  let stdinAt = 0;

  // fd_write(fd, iovs_ptr, iovs_len, nwritten_ptr) -> errno
  // Decodes the iovec array out of linear memory and appends to the right
  // stream. Chunks decode with `{ stream: true }` so a character split across
  // iovecs carries over instead of becoming U+FFFD; the final flush decode
  // closes the call boundary, so a truncated tail is still emitted before
  // this write returns. wasi-libc buffers internally, so chunks are usually
  // whole lines.
  //
  // THE BYTES ARE HANDED OVER TOO. `writeStdout` exists so a program
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

    // ---- time & randomness: host-provided, backed by the browser ------------
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

    // ---- input I/O: graceful degradation, not file access -------------------
    // A page has no argv: zero arguments, zero buffer bytes.
    args_sizes_get: (argcPtr, bufSizePtr) => {
      const view = new DataView(memory.buffer);
      view.setUint32(argcPtr, 0, true);
      view.setUint32(bufSizePtr, 0, true);
      return ERRNO_SUCCESS;
    },
    args_get: () => ERRNO_SUCCESS, // argc is 0, so there is nothing to write
    // A page has no environment either. Only the direct wasm backend asks:
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
    // Standard input is `hooks.stdin` — a string or a Uint8Array — and immediate
    // EOF when the host supplies none, which is what `readLine()` reads as
    // `None`. A page HAS input to give (the playground has a stdin box beside
    // the editor); what it has never had is a file descriptor, so the bytes are
    // handed over up front and this serves them in order.
    fd_read: (fd, iovsPtr, iovsLen, nreadPtr) => {
      if (fd !== 0) return ERRNO_BADF;
      const view = new DataView(memory.buffer);
      let read = 0;
      for (let i = 0; i < iovsLen && stdinAt < stdinBytes.length; i++) {
        const base = view.getUint32(iovsPtr + i * 8, true);
        const len = view.getUint32(iovsPtr + i * 8 + 4, true);
        const take = Math.min(len, stdinBytes.length - stdinAt);
        new Uint8Array(memory.buffer, base, take).set(
          stdinBytes.subarray(stdinAt, stdinAt + take)
        );
        stdinAt += take;
        read += take;
      }
      view.setUint32(nreadPtr, read, true);
      return ERRNO_SUCCESS;
    },
    fd_fdstat_set_flags: () => ERRNO_SUCCESS,
    // No preopened directories: BADF ends wasi-libc's preopen scan (fd 3…).
    fd_prestat_get: () => ERRNO_BADF,
    fd_prestat_dir_name: () => ERRNO_BADF,
    // No filesystem: every open fails, so `readFile`/`writeFile` return their
    // canonical `Err` payloads in-page (never a crash).
    path_open: () => ERRNO_NOENT,
    // A module using `renameFile`/`writeAtomic`/`fsyncFile` pulls in
    // path_rename / fd_sync. With no filesystem in a page, rename fails (NOENT →
    // canonical `Err`); fsync never reaches an open fd (the fopen already
    // failed), so its stub is a harmless success. Provided so the module still
    // instantiates and degrades gracefully rather than failing to link.
    path_rename: () => ERRNO_NOENT,
    fd_sync: () => ERRNO_SUCCESS,
    // `listDir` pulls in fd_readdir. Its open already failed
    // (path_open above), so the stub is never reached; it exists so the
    // module links.
    fd_readdir: () => ERRNO_BADF,
  };

  // Build the `vyrn` import namespace from the host's extern hooks.
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
      // do; `opaque` has no wire form at all.
      throw new Error(`extern \`${field}\` returns unsupported type \`${ret}\``);
    };
  }

  // The host's waits: each pending timer's deadline, in milliseconds since the
  // start, and its `setTimeout` handle, by id, as `Waits` in
  // `vyrn-cli/src/wasmrun.rs` keeps them. The clock is logical, as there:
  // `now` is the deadline of the wake the module runs in, 0 before the first,
  // or the time an export's task began, so the time the module computes never
  // moves a later deadline.
  const started = performance.now();
  const real = () => performance.now() - started;
  let now = 0;
  const due = new Map();
  let nextWait = 1n;
  // Each timer has its own `setTimeout`, armed for its deadline in real time.
  // When one fires, every pending timer up to it, earliest first and ties by
  // id, goes to `__vyrn_wake`, as `settle_waits` in `wasmrun.rs` delivers them;
  // a later timer waits for its own. Timers fire in the order they come due in
  // real time, so the fired timer bounds the delivery, not the clock.
  const vyrnWait = {
    // A timer due `ms` after `now`; a negative `ms` is due now.
    sleep: (ms) => {
      const id = nextWait++;
      const at = now + Math.max(0, Number(ms));
      const handle = setTimeout(() => deliverThrough(at, id), Math.max(0, at - real()));
      due.set(id, { at, handle });
      return id;
    },
    // A request: the packed fields at `at`, `len` bytes, are copied now and
    // sent with the page's `fetch` (compiler.md, Host waits). Its reply is
    // packed as every host packs it and delivered as a String the module owns.
    fetch: (at, len) => {
      const id = nextWait++;
      const request = new Uint8Array(memory.buffer, at >>> 0, Number(len)).slice();
      const abort = new AbortController();
      flying.set(id, abort);
      send(request, abort.signal).then((reply) => {
        if (!flying.delete(id)) return;
        deliverReply(id, reply);
      });
      return id;
    },
    // No wait waits for operation `id`, so its timer never fires and its
    // request is aborted.
    cancel: (id) => {
      const t = due.get(id);
      if (t) clearTimeout(t.handle);
      due.delete(id);
      const f = flying.get(id);
      if (f) f.abort();
      flying.delete(id);
    },
    // The task this side started as `task` ended: 0 with its result word, or
    // 1 cancelled.
    done: (task, status, result) => {
      const t = tasks.get(task);
      tasks.delete(task);
      if (status === 0) t.resolve(result);
      else t.reject(new Error(`the task an export started was cancelled`));
    },
  };
  // Each request in flight, by its operation's id, with what aborts it.
  const flying = new Map();
  // Each task a waiting export started and has not ended, by the id this side
  // gave it.
  const tasks = new Map();
  let nextTask = 1n;
  const instance = await WebAssembly.instantiate(module, {
    wasi_snapshot_preview1: wasi,
    vyrn,
    vyrn_wait: vyrnWait,
  });
  memory = instance.exports.memory;

  // --- string helpers over linear memory (the export ABI) --------------------
  // A String crosses into an exported Vyrn function as a single pointer to
  // NUL-terminated UTF-8. The JS side also writes the eight-byte `{ len, cap }`
  // header that sits in FRONT of every Vyrn String: the module reads
  // `s.byteLength` out of it, so a String built here without one would read a
  // length out of whatever preceded it.
  //
  // The conversion belongs here, at the boundary, and nowhere else: the ABI at
  // the edge is still a bare pointer.
  //
  // This is the asymmetry vs. an IMPORT, where a String is a (ptr, len)
  // pair — an import can't allocate inside the module, but an exported call can.
  const STR_HDR = 8;
  const enc = new TextEncoder();
  const encodeString = (s) => encodeBytes(enc.encode(s));
  const encodeBytes = (bytes) => {
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

  // --- requests -----------------------------------------------------------
  // The one field encoding of the host boundary (compiler.md, Host waits):
  // each field its byte length in decimal, `:`, its bytes.
  const pack = (fields) => {
    const parts = fields.map((f) => (typeof f === "string" ? enc.encode(f) : f));
    const out = [];
    for (const p of parts) out.push(enc.encode(`${p.length}:`), p);
    const all = new Uint8Array(out.reduce((n, p) => n + p.length, 0));
    let at = 0;
    for (const p of out) {
      all.set(p, at);
      at += p.length;
    }
    return all;
  };
  const unpack = (b) => {
    const out = [];
    let i = 0;
    while (i < b.length) {
      let n = 0;
      let j = i;
      while (j < b.length && b[j] >= 48 && b[j] <= 57) n = n * 10 + b[j++] - 48;
      if (j === i || b[j] !== 58 || j + 1 + n > b.length) return null;
      out.push(b.subarray(j + 1, j + 1 + n));
      i = j + 1 + n;
    }
    return out;
  };
  // Sends a packed request and answers its packed reply. The page's `fetch`
  // already lower-cases, sorts and combines the reply's headers.
  const send = async (request, signal) => {
    const text = (b) => new TextDecoder().decode(b);
    const f = unpack(request);
    const n = f && f.length >= 4 ? Number(text(f[2])) : -1;
    if (!(n >= 0) || f.length !== 4 + 2 * n) return pack(["badUrl"]);
    let url, headers;
    try {
      url = new URL(text(f[1]));
      headers = new Headers();
      for (let k = 0; k < n; k++) headers.append(text(f[3 + 2 * k]), text(f[4 + 2 * k]));
    } catch {
      return pack(["badUrl"]);
    }
    if (!/^https?:$/.test(url.protocol) || url.username || url.password || !url.hostname) {
      return pack(["badUrl"]);
    }
    const body = f[3 + 2 * n];
    let r, bytes;
    try {
      r = await fetch(url, {
        method: text(f[0]),
        headers,
        body: body.length ? body : undefined,
        redirect: "manual",
        signal,
      });
      bytes = new Uint8Array(await r.arrayBuffer());
    } catch {
      return pack(["unreachable"]);
    }
    try {
      new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    } catch {
      return pack(["badResponse"]);
    }
    const hs = [...r.headers].filter(([k]) => k !== "set-cookie");
    return pack(["ok", String(r.status), String(hs.length), ...hs.flat(), bytes]);
  };

  // --- wrap exported-extern functions -----------------------------------------
  // For each `export extern fn`, expose a pre-wrapped callable on the returned
  // `exports`. Both halves come from the module's declaration: an argument
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
  for (const [field, { params, ret, waits }] of abi.exports) {
    if (RESERVED.has(field) || field.startsWith("__")) continue;
    const raw = instance.exports[field];
    // Declared but swept, or renamed: the section lists what the source says,
    // and only what the instance actually exports is callable.
    if (typeof raw !== "function") continue;
    wrappedExports[field] = (...jsArgs) => {
      const call = [];
      // Every buffer this call allocated inside the module. The CALLER owns a
      // String argument, and across this boundary the caller is here, so this list
      // is what has to be handed back through `__vyrn_free`.
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
      // A String argument is released once the call is over: at its return, or,
      // for an export that waits, once the task it started ended, because the
      // task borrows its arguments until then.
      const release = () => {
        // The block base is the pointer less its String header,
        // which is what `__vyrn_malloc` handed out.
        const free = instance.exports.__vyrn_free;
        if (typeof free === "function") for (const p of owned) free(p - STR_HDR);
      };
      // Decoded BEFORE the release, because an export may return the pointer it
      // was given and `free` writes its list link into the block.
      const decode = (r) => {
        let out;
        if (ret === "unit") out = undefined;
        else if (ret === "string") {
          out = decodeCString(r, field);
          // A returned String is the CALLER's, and across
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
        return out;
      };
      if (!waits) {
        let out;
        try {
          out = decode(raw(...call));
        } catch (e) {
          // A trap leaves the module unusable, but a `panic` the page catches
          // does not, so the release runs on both paths.
          release();
          throw e;
        }
        release();
        return out;
      }
      // A waiting export's task ends in `vyrn_wait.done`, perhaps before the
      // call returns, so its promise is registered first. Its result word is
      // the declared kind's wasm value: an i32 kind in the low half, a float by
      // its bits.
      const task = nextTask++;
      const ended = new Promise((resolve, reject) => tasks.set(task, { resolve, reject }));
      now = Math.floor(real());
      try {
        raw(task, ...call);
      } catch (e) {
        tasks.delete(task);
        release();
        throw e;
      }
      return ended.then((w) => decode(wasmWord(ret, w))).finally(release);
      // Nothing crosses this boundary to say who owns a return, and nothing has
      // to: every return is owned, and an `export extern fn` that lends fails to
      // compile.
    };
  }

  // A result word as `vyrn_wait.done` delivers it, an i64, as the wasm value of
  // kind `ret`.
  const bits = new DataView(new ArrayBuffer(8));
  const wasmWord = (ret, w) => {
    if (ret === "i64" || ret === "u64") return w;
    if (ret === "f64") {
      bits.setBigInt64(0, w, true);
      return bits.getFloat64(0, true);
    }
    if (ret === "f32") {
      bits.setUint32(0, Number(BigInt.asUintN(32, w)), true);
      return bits.getFloat32(0, true);
    }
    return Number(BigInt.asIntN(32, w));
  };

  // A stack overflow goes back to the module: `__vyrn_overflow` flushes stdout
  // and traps with Vyrn's sentence, as `vyrn run` does. It is the engine's
  // `RangeError`, or the shadow stack's: an out-of-bounds access with the
  // stack pointer past the end of memory, where a frame push that wrapped
  // below 0 leaves it.
  const { __stack_pointer: sp, __vyrn_overflow: overflow } = instance.exports;
  const top = sp && sp.value;
  const overflowed = (e) =>
    sp &&
    overflow &&
    (e instanceof RangeError ||
      (e instanceof WebAssembly.RuntimeError && sp.value >>> 0 >= memory.buffer.byteLength));
  const guarded = (f) => {
    try {
      f();
    } catch (e) {
      if (!overflowed(e)) throw e;
      sp.value = top;
      overflow();
    }
  };
  // A `main` that waits returns from `_start` and ends in a wake. `settle`
  // resolves once no timer is pending, and rejects with a trap or the exit a
  // wake ended in; every caller waits on the same timers.
  let settling = [];
  const settled = (err) => {
    const ws = settling;
    settling = [];
    for (const w of ws) (err ? w.reject(err) : w.resolve());
  };
  const deliverThrough = (at, id) => {
    const wake = instance.exports.__vyrn_wake;
    try {
      for (;;) {
        let next = null;
        for (const [k, t] of due) {
          if (next === null || t.at < next[1].at || (t.at === next[1].at && k < next[0])) next = [k, t];
        }
        if (next === null || next[1].at > at || (next[1].at === at && next[0] > id)) break;
        clearTimeout(next[1].handle);
        due.delete(next[0]);
        now = next[1].at;
        guarded(() => wake(next[0], 0, 0n));
      }
    } catch (e) {
      failed(e);
      return;
    }
    if (due.size === 0 && flying.size === 0) settled(null);
  };
  // A wake that trapped or exited ends every timer, request and task.
  const failed = (e) => {
    for (const t of due.values()) clearTimeout(t.handle);
    due.clear();
    for (const f of flying.values()) f.abort();
    flying.clear();
    for (const t of tasks.values()) t.reject(e);
    tasks.clear();
    settled(e);
  };
  // A reply arrived: `now` is its arrival, and the module owns the String.
  const deliverReply = (id, reply) => {
    try {
      const ptr = encodeBytes(reply);
      now = Math.floor(real());
      guarded(() => instance.exports.__vyrn_wake(id, 0, BigInt(ptr)));
    } catch (e) {
      failed(e);
      return;
    }
    if (due.size === 0 && flying.size === 0) settled(null);
  };
  const settle = () =>
    (due.size === 0 && flying.size === 0) || !instance.exports.__vyrn_wake
      ? Promise.resolve()
      : new Promise((resolve, reject) => settling.push({ resolve, reject }));
  let exitCode = 0;
  try {
    guarded(() => instance.exports._start());
    await settle();
  } catch (e) {
    if (e instanceof VyrnExit) {
      exitCode = e.code;
    } else {
      throw e; // a genuine trap (unreachable, OOB) — surface it
    }
  }
  // `exports`: the exported-extern functions, callable AFTER `_start` ran `main`
  // once — the instance stays alive.
  // `memory`: the instance's linear memory. `memory.buffer.byteLength` is the
  // only way to see whether the module reclaims what it allocates.
  // `stdoutRaw` is the whole of standard output as BYTES — what `stdout`
  // would be if it were not a string. For a text program the two say the
  // same thing; for one that called `writeStdout` with a packed row, only
  // this one is faithful.
  const total = stdoutBytes.reduce((n, b) => n + b.length, 0);
  const stdoutRaw = new Uint8Array(total);
  let rawAt = 0;
  for (const b of stdoutBytes) {
    stdoutRaw.set(b, rawAt);
    rawAt += b.length;
  }
  return { exitCode, stdout, stdoutRaw, stderr, exports: wrappedExports, memory };
}
