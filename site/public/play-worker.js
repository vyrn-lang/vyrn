// The playground's engine, off the main thread.
//
// A worker exists here for one reason: `while true { }` is a legal Vyrn program
// and a reader is going to type it. On the main thread that freezes the tab and
// the only cure is closing it. In a worker the page stays alive, watches the
// clock, and calls `terminate()` — which is the only way to stop running
// WebAssembly, and the reason the kill switch lives in `play.js` and not here.
//
// TWO MODULES, AND THE SECOND ONE IS THE PROGRAM. `play.wasm` is the compiler
// (`compiler/vyrn-play`); it answers the reader's program as a wasm module — the
// same bytes `vyrn build --target wasm` writes — and `wasi-min.js` instantiates
// and runs THOSE. So the page runs the compiled route, the one `vyrn run` runs,
// and nothing here re-implements a semantics.
//
// A FRESH COMPILER INSTANCE PER RUN. Not for isolation between programs — the
// module is stateless between calls — but because a compile can die in ways that
// leave the instance unusable: a stack overflow throws a JavaScript `RangeError`
// out of the middle of it, unwinding no Rust and releasing no borrow. The next
// call into that instance would panic on one. `play.js` makes a new worker per
// run in any case, so each run gets its own.
import { loadPlay } from "./play-wasm.js";
import { runVyrn } from "./wasi-min.js";

// Compiled once. `loadPlay` fetches and instantiates together, so the module is
// re-fetched per run from the HTTP cache; that is a cache hit, measured at a few
// milliseconds.
let ready = null;

self.onmessage = async (e) => {
  const { src, stdin } = e.data;
  try {
    ready = loadPlay(new URL("play.wasm", import.meta.url));
    const play = await ready;
    const answer = play.compile(src);
    // It did not compile: the diagnostics are the answer, and there is nothing
    // to run.
    if (!answer.module) {
      self.postMessage({ ok: true, result: answer });
      return;
    }
    // The program's own module, with the page's stdin and the page's clock —
    // `wasi-min.js` backs `clock_time_get` with `Date.now()`, so a browser tab
    // needs nothing injected for it.
    const { exitCode, stdout, stderr } = await runVyrn(answer.module, { stdin });
    self.postMessage({ ok: true, result: { exitCode, stdout, stderr } });
  } catch (err) {
    // A `RangeError` here is the ENGINE's stack, not the language's limit, and
    // saying which one happened is the difference between a rule of Vyrn and a
    // ceiling of this tab.
    //
    // A compiled Vyrn call is one wasm frame, and the module counts its own to
    // `CALL_DEPTH_LIMIT` — 1,000, the same number in every backend, reported as
    // an ordinary trap on stderr. MEASURED against this worker: the language's
    // limit arrives first, so a program that recurses too deep gets Vyrn's own
    // wording and not this one. What is left here is the compiler's own
    // recursion — a program nested deeply enough to overflow the parser — and
    // that is the engine's ceiling, so it says so.
    const stack = err instanceof RangeError;
    self.postMessage({
      ok: false,
      error: stack
        ? "This program nests deeper than the browser's own stack allows while the compiler reads it. The ceiling is the engine's, not the language's: `vyrn run` compiles this program."
        : String(err && err.message ? err.message : err),
    });
  }
};
