// The playground's engine, off the main thread.
//
// A worker exists here for one reason: `while true { }` is a legal Vyrn program
// and a reader is going to type it. On the main thread that freezes the tab and
// the only cure is closing it. In a worker the page stays alive, watches the
// clock, and calls `terminate()` — which is the only way to stop running
// WebAssembly, and the reason the kill switch lives in `play.js` and not here.
//
// A FRESH INSTANCE PER RUN. Not for isolation between programs — the module is
// stateless between calls — but because a run can die in ways that leave the
// instance unusable: a stack overflow throws a JavaScript `RangeError` out of the
// middle of the interpreter, unwinding no Rust and releasing no borrow. The next
// call into that instance would panic on one. So each run gets its own, which
// costs an instantiation of an already-compiled module.
import { loadPlay } from "/play-wasm.js";

// Compiled once. `loadPlay` fetches and instantiates together, so the module is
// re-fetched per run from the HTTP cache; that is 16 MB of linear memory and a
// cache hit, measured at a few milliseconds.
let ready = null;

self.onmessage = async (e) => {
  const { src, stdin, now } = e.data;
  try {
    ready = loadPlay("/play.wasm");
    const play = await ready;
    self.postMessage({ ok: true, result: play.run(src, stdin, now) });
  } catch (err) {
    // A `RangeError` here is the ENGINE's stack, not the language's limit, and
    // saying which one happened is the difference between a rule of Vyrn and a
    // ceiling of this tab.
    //
    // The interpreter counts its own calls and stops at `CALL_DEPTH_LIMIT` —
    // 1,000, the same number in every backend, reported as an ordinary trap. It
    // rarely gets the chance here: V8 gives every WebAssembly call a frame on its
    // own native stack, and a worker's native stack is about half the main
    // thread's. MEASURED by binary search against this worker: 466 to 468 nested
    // Vyrn calls, stable across runs, against the language's 1,000. Node reaches
    // 996 with the same module, which is why the number below says "this browser"
    // and not a constant.
    //
    // The linear-memory stack is not the constraint and raising it buys nothing;
    // see `compiler/vyrn-play/.cargo/config.toml`.
    const stack = err instanceof RangeError;
    self.postMessage({
      ok: false,
      error: stack
        ? "This program recurses deeper than the browser's own stack allows — measured at 466 nested calls here, against the interpreter's limit of 1,000. The ceiling is the engine's, not the language's: `vyrn run` takes this program to 1,000."
        : String(err && err.message ? err.message : err),
    });
  }
};
