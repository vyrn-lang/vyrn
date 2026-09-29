// The calling convention for `play.wasm` — the Vyrn front end, compiled from the
// same Rust the `vyrn` binary is built from (`compiler/vyrn-play`).
//
// Two callers, because the two halves of the playground have opposite needs:
//
//   - `play.js`, on the main thread, asks for `tokens` on every keystroke and
//     `check` shortly after. Both must be synchronous: a colour layer that
//     arrives a message later than the character shows the reader an editor that
//     lags. Neither can loop forever, because neither runs the program.
//   - `play-worker.js` asks for `compile`, and then runs the module it gets back
//     with `wasi-min.js`. THAT can loop forever, which is the whole reason a
//     worker exists: the page stays alive and can terminate it.
//
// No bindgen and no dependencies. The module owns one input buffer and one output
// buffer; `memory.buffer` is detached by a growth, so every access below re-reads
// it rather than caching a view.

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/// Fetch and instantiate the module.
export async function loadPlay(url) {
  const source = await fetch(url);
  if (!source.ok) throw new Error(`play.wasm: ${source.status} ${source.statusText}`);
  // `instantiateStreaming` needs the right content type, which a static host may
  // not send. The buffer path works either way and the module is not big enough
  // for streaming to matter.
  return instantiatePlay(await source.arrayBuffer());
}

/// Instantiate the module from its bytes. Its one import runs a generator the
/// compiler built (`derive(g, x)`): the compiler is itself a wasm module, so
/// the page instantiates the generator's module for it.
export async function instantiatePlay(bytes) {
  let wasm = null;
  const imports = { vyrn_play: { run_generator: (ptr, len) => answerGenerator(wasm, ptr, len) } };
  const { instance } = await WebAssembly.instantiate(bytes, imports);
  wasm = instance.exports;
  return api(instance);
}

/// Serves `run_generator`: reads the request `gen_request` wrote in
/// `compiler/vyrn-play`, runs the module, and writes stdout (answer 0) or the
/// error (answer 1) through `gen_output_ptr`. Nothing is thrown back into the
/// compiler, because an exception through its frames releases no borrow.
function answerGenerator(wasm, ptr, len) {
  let status = 0;
  let out;
  try {
    const req = parseRequest(new Uint8Array(wasm.memory.buffer, ptr, len).slice());
    out = runGenerator(req);
  } catch (err) {
    status = 1;
    out = encoder.encode(String(err && err.message ? err.message : err));
  }
  const at = wasm.gen_output_ptr(out.length);
  new Uint8Array(wasm.memory.buffer).set(out, at);
  return status;
}

/// `{ module, argv, atoms }` from the request bytes. An atom is a BigInt or a
/// Uint8Array, as the tag byte says.
function parseRequest(b) {
  const view = new DataView(b.buffer);
  let at = 0;
  const u32 = () => ((at += 4), view.getUint32(at - 4, true));
  const bytes = () => {
    const n = u32();
    return b.subarray(at, (at += n));
  };
  const module = bytes();
  const argv = Array.from({ length: u32() }, () => decoder.decode(bytes()));
  const atoms = Array.from({ length: u32() }, () =>
    b[at++] === 0 ? ((at += 8), view.getBigInt64(at - 8, true)) : bytes(),
  );
  return { module, argv, atoms };
}

/// Runs a generator module synchronously and returns its stdout. Synchronous
/// because the compiler is waiting inside its own call. The host serves the
/// atom stream of the one `TypeArg` and the WASI calls a generator makes; any
/// other import throws, naming itself.
function runGenerator({ module: bytes, argv, atoms }) {
  const module = new WebAssembly.Module(bytes);
  let memory = null;
  const stdout = [];
  const stderr = [];
  let cursor = 0;
  let stash = new Uint8Array(0);
  const args = ["gen", ...argv].map((a) => encoder.encode(a + "\0"));
  const view = () => new DataView(memory.buffer);
  class Exit {
    constructor(code) {
      this.code = code;
    }
  }
  const wasi = {
    args_sizes_get(countPtr, sizePtr) {
      view().setUint32(countPtr, args.length, true);
      view().setUint32(sizePtr, args.reduce((n, a) => n + a.length, 0), true);
      return 0;
    },
    args_get(argvPtr, bufPtr) {
      for (const [i, a] of args.entries()) {
        view().setUint32(argvPtr + 4 * i, bufPtr, true);
        new Uint8Array(memory.buffer).set(a, bufPtr);
        bufPtr += a.length;
      }
      return 0;
    },
    environ_sizes_get(countPtr, sizePtr) {
      view().setUint32(countPtr, 0, true);
      view().setUint32(sizePtr, 0, true);
      return 0;
    },
    environ_get: () => 0,
    fd_write(fd, iovs, n, written) {
      let total = 0;
      for (let i = 0; i < n; i++) {
        const ptr = view().getUint32(iovs + 8 * i, true);
        const len = view().getUint32(iovs + 8 * i + 4, true);
        (fd === 1 ? stdout : stderr).push(new Uint8Array(memory.buffer, ptr, len).slice());
        total += len;
      }
      view().setUint32(written, total, true);
      return 0;
    },
    proc_exit(code) {
      throw new Exit(code);
    },
  };
  const next = () => {
    if (cursor >= atoms.length) throw new Error("generator decoder read past the end of its TypeArg");
    return atoms[cursor++];
  };
  const gen = {
    reflect(kind) {
      if (kind !== 3n) throw new Error("the playground serves a generator its TypeArg and nothing else");
      cursor = 0;
    },
    nextInt: () => next(),
    nextStr() {
      stash = next();
      return BigInt(stash.length);
    },
    fetch(dest) {
      new Uint8Array(memory.buffer).set(stash, dest);
    },
  };
  const hosts = { wasi_snapshot_preview1: wasi, vyrn_gen: gen };
  const imports = {};
  for (const { module: from, name } of WebAssembly.Module.imports(module)) {
    const served = hosts[from] && hosts[from][name];
    (imports[from] ??= {})[name] =
      served ??
      (() => {
        throw new Error(`the playground's generator host has no \`${from}.${name}\``);
      });
  }
  const instance = new WebAssembly.Instance(module, imports);
  memory = instance.exports.memory;
  try {
    instance.exports._start();
  } catch (err) {
    if (!(err instanceof Exit)) throw err;
    if (err.code !== 0) throw new Error(decoder.decode(join(stderr)).trim() || `generator exited with ${err.code}`);
  }
  return join(stdout);
}

function join(chunks) {
  const out = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
  let at = 0;
  for (const c of chunks) {
    out.set(c, at);
    at += c.length;
  }
  return out;
}

function api(instance) {
  const wasm = instance.exports;

  /// Write `parts` into the module's input buffer, back to back. Returns their
  /// byte lengths, which is what the entry points take.
  function put(parts) {
    const bufs = parts.map((p) => encoder.encode(p));
    const total = bufs.reduce((n, b) => n + b.length, 0);
    const at = wasm.input_ptr(total);
    const mem = new Uint8Array(wasm.memory.buffer);
    let cursor = at;
    for (const b of bufs) {
      mem.set(b, cursor);
      cursor += b.length;
    }
    return bufs.map((b) => b.length);
  }

  /// The JSON the last call left behind.
  function take(len) {
    return JSON.parse(decoder.decode(new Uint8Array(wasm.memory.buffer, wasm.result_ptr(), len)));
  }

  /// The last call's result as BYTES, copied out. A copy because
  /// `memory.buffer` is detached by the next call's growth.
  function takeBytes(len) {
    return new Uint8Array(wasm.memory.buffer, wasm.result_ptr(), len).slice();
  }

  return {
    /// `{ spans: [[start, length, class], …] }` in UTF-16 code units, or
    /// `{ error }` when the source cannot be lexed at all — which happens on the
    /// way to typing a string literal, so the caller falls back to plain text.
    tokens(src) {
      const [n] = put([src]);
      return take(wasm.play_tokens(n));
    },
    /// `{ diagnostics: [{ line, col, endCol, severity, stage, message, note }] }`
    check(src) {
      const [n] = put([src]);
      return take(wasm.play_check(n));
    },
    /// `{ module }` — the program as a wasm module, the same bytes
    /// `vyrn build --target wasm` writes — or `{ diagnostics }` when it did not
    /// compile. The two are told apart by the first byte, because a module
    /// begins with the wasm magic and JSON begins with `{`.
    compile(src) {
      const [n] = put([src]);
      const len = wasm.play_compile(n);
      const first = new Uint8Array(wasm.memory.buffer, wasm.result_ptr(), 1)[0];
      return first === 0x00 ? { module: takeBytes(len) } : take(len);
    },
  };
}
