const encoder = new TextEncoder();
const decoder = new TextDecoder();

export async function loadPlay(url) {
  const source = await fetch(url);
  if (!source.ok) throw new Error(`play.wasm: ${source.status} ${source.statusText}`);
  // Not instantiateStreaming: it needs a content type a static host may not send.
  return instantiatePlay(await source.arrayBuffer());
}

export async function instantiatePlay(bytes) {
  let wasm = null;
  const imports = { vyrn_play: { run_generator: (ptr, len) => answerGenerator(wasm, ptr, len) } };
  const { instance } = await WebAssembly.instantiate(bytes, imports);
  wasm = instance.exports;
  return api(instance);
}

// Never throws: an exception through the compiler's frames releases no borrow.
function answerGenerator(wasm, ptr, len) {
  let status = 0;
  let out;
  try {
    const req = parseRequest(new Uint8Array(wasm.memory.buffer, ptr, len).slice());
    out = runGenerator(wasm, req);
  } catch (err) {
    status = 1;
    out = encoder.encode(String(err && err.message ? err.message : err));
  }
  const at = wasm.gen_output_ptr(out.length);
  new Uint8Array(wasm.memory.buffer).set(out, at);
  return status;
}

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

// The code imports the compiler serves (`code_call` in vyrn-play), each with the
// positions of its String arguments. A String crosses as its bytes; the null
// pointer `splice` sends for any other value stays 0.
const CODE_IMPORTS = {
  text: [0, 1],
  data: [0],
  name: [0],
  diag: [0, 1, 2],
  at: [1],
  splice: [2, 3],
  concat: [],
  result: [],
  render: [],
};

// Runs one code import in the compiler; answers its i64 result and the text
// `render` leaves for `fetch`. Throws why the run ends.
function codeCall(wasm, name, args) {
  const word = encoder.encode(name);
  const parts = [u32(word.length), word, u32(args.length)];
  for (const a of args) {
    if (a instanceof Uint8Array) parts.push(Uint8Array.of(1), u32(a.length), a);
    else parts.push(Uint8Array.of(0), new Uint8Array(BigInt64Array.of(BigInt.asIntN(64, BigInt(a))).buffer));
  }
  const req = join(parts);
  new Uint8Array(wasm.memory.buffer).set(req, wasm.code_request(req.length));
  const n = wasm.code_call(req.length);
  const answer = new Uint8Array(wasm.memory.buffer, wasm.code_answer(), n).slice();
  if (answer[0] !== 0) throw new Error(decoder.decode(answer.subarray(1)));
  return { value: new DataView(answer.buffer).getBigInt64(1, true), text: answer.subarray(9) };
}

function u32(n) {
  return new Uint8Array(Uint32Array.of(n).buffer);
}

// Synchronous, because the compiler waits for the answer inside its own call.
function runGenerator(wasm, { module: bytes, argv, atoms }) {
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
  const cstr = (p) => {
    const mem = new Uint8Array(memory.buffer);
    const end = mem.indexOf(0, p);
    return mem.slice(p, end < 0 ? mem.length : end);
  };
  for (const [name, strings] of Object.entries(CODE_IMPORTS)) {
    gen[name] = (...args) => {
      const sent = args.map((a, i) => (strings.includes(i) && a !== 0 ? cstr(a >>> 0) : a));
      const { value, text } = codeCall(wasm, name, sent);
      if (name === "render") stash = text;
      return value;
    };
  }
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

// A memory growth detaches memory.buffer, so every access below re-reads it.
function api(instance) {
  const wasm = instance.exports;

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

  function take(len) {
    return JSON.parse(decoder.decode(new Uint8Array(wasm.memory.buffer, wasm.result_ptr(), len)));
  }

  function takeBytes(len) {
    return new Uint8Array(wasm.memory.buffer, wasm.result_ptr(), len).slice();
  }

  return {
    tokens(src) {
      const [n] = put([src]);
      return take(wasm.play_tokens(n));
    },
    check(src) {
      const [n] = put([src]);
      return take(wasm.play_check(n));
    },
    complete(src, line, col) {
      const [n] = put([src]);
      return take(wasm.play_complete(n, line, col));
    },
    hover(src, line, col) {
      const [n] = put([src]);
      return take(wasm.play_hover(n, line, col));
    },
    signature(src, line, col) {
      const [n] = put([src]);
      return take(wasm.play_signature(n, line, col));
    },
    compile(src) {
      const [n] = put([src]);
      const len = wasm.play_compile(n);
      const first = new Uint8Array(wasm.memory.buffer, wasm.result_ptr(), 1)[0];
      return first === 0x00 ? { module: takeBytes(len) } : take(len);
    },
  };
}
