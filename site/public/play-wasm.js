// The calling convention for `play.wasm` — the Vyrn front end, compiled from the
// same Rust the `vyrn` binary is built from (`compiler/vyrn-play`).
//
// Two callers, because the two halves of the playground have opposite needs:
//
//   - `play.js`, on the main thread, asks for `tokens` on every keystroke and
//     `check` shortly after. Both must be synchronous: a colour layer that
//     arrives a message later than the character shows the reader an editor that
//     lags. Neither can loop forever, because neither runs the program.
//   - `play-worker.js` asks for `run`, which CAN loop forever. That is the whole
//     reason a worker exists: the page stays alive and can terminate it.
//
// No bindgen and no dependencies. The module owns one input buffer and one output
// buffer; `memory.buffer` is detached by a growth, so every access below re-reads
// it rather than caching a view.

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/// Fetch and instantiate the module. `imports` is empty: the front end asks the
/// host for nothing.
export async function loadPlay(url) {
  const source = await fetch(url);
  if (!source.ok) throw new Error(`play.wasm: ${source.status} ${source.statusText}`);
  // `instantiateStreaming` needs the right content type, which a static host may
  // not send. The buffer path works either way and the module is not big enough
  // for streaming to matter.
  const { instance } = await WebAssembly.instantiate(await source.arrayBuffer(), {});
  return api(instance);
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
    /// `{ stdout, stderr, exitCode, diagnostics }`, or `{ diagnostics }` alone
    /// when the program did not compile. `now` is the wall clock the program
    /// reads; it is sampled once, because a wasm module has no clock of its own.
    run(src, stdin, now) {
      const [n, m] = put([src, stdin || ""]);
      return take(wasm.play_run(n, m, now));
    },
  };
}
