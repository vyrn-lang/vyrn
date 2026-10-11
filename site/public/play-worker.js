import { loadPlay } from "./play-wasm.js";
import { runVyrn } from "./wasi-min.js";

self.onmessage = async (e) => {
  const { src, stdin } = e.data;
  try {
    // A fresh instance per run: a RangeError mid-compile leaves an instance that panics on the next call.
    const play = await loadPlay(new URL("play.wasm", import.meta.url));
    const answer = play.compile(src);
    if (!answer.module) {
      self.postMessage({ ok: true, result: answer });
      return;
    }
    const { exitCode, stdout, stderr } = await runVyrn(answer.module, { stdin });
    self.postMessage({ ok: true, result: { exitCode, stdout, stderr } });
  } catch (err) {
    const stack = err instanceof RangeError;
    self.postMessage({
      ok: false,
      error: stack
        ? "This program nests deeper than the browser's own stack allows while the compiler reads it. The ceiling is the engine's, not the language's: `vyrn run` compiles this program."
        : String(err && err.message ? err.message : err),
    });
  }
};
