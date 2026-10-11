// The playground's engine, off the main thread.
//
// `while true { }` is a legal program, and on the main thread it would freeze
// the tab. In a worker the page stays alive and calls `terminate()`, the only
// way to stop running WebAssembly; the kill switch is in `play.js`.
//
// Two modules: `play.wasm` is the compiler (`compiler/vyrn-play`), and it
// answers with the program as a wasm module, the bytes `vyrn build --target
// wasm` writes, which `wasi-min.js` runs. So the page runs the compiled route
// and nothing here re-implements a semantics.
//
// Each run gets a fresh compiler instance (`play.js` makes a worker per run). A
// stack overflow throws a `RangeError` out of the middle of a compile and
// releases no Rust borrow, so the next call into that instance would panic.
import { loadPlay } from "./play-wasm.js";
import { runVyrn } from "./wasi-min.js";

self.onmessage = async (e) => {
  const { src, stdin } = e.data;
  try {
    // Fetched per run; the second fetch is an HTTP cache hit.
    const play = await loadPlay(new URL("play.wasm", import.meta.url));
    const answer = play.compile(src);
    // It did not compile: the diagnostics are the answer.
    if (!answer.module) {
      self.postMessage({ ok: true, result: answer });
      return;
    }
    // The program's own module, with the page's stdin. `wasi-min.js` backs
    // `clock_time_get` with `Date.now()`.
    const { exitCode, stdout, stderr } = await runVyrn(answer.module, { stdin });
    self.postMessage({ ok: true, result: { exitCode, stdout, stderr } });
  } catch (err) {
    // A `RangeError` here is the engine's stack, not a limit of the language.
    // A program that recurses too deep never reaches this: `runVyrn` hands its
    // `RangeError` back to the module, which prints Vyrn's own trap. What is
    // left is the compiler overflowing on a deeply nested program.
    const stack = err instanceof RangeError;
    self.postMessage({
      ok: false,
      error: stack
        ? "This program nests deeper than the browser's own stack allows while the compiler reads it. The ceiling is the engine's, not the language's: `vyrn run` compiles this program."
        : String(err && err.message ? err.message : err),
    });
  }
};
