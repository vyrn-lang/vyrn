const RUN_LIMIT_MS = 5000;

let worker = null;
let runTimer = null;

export function stopWorker() {
  clearTimeout(runTimer);
  runTimer = null;
  if (worker) {
    worker.terminate();
    worker = null;
  }
}

export function runInWorker(src, stdin, { done, fail }) {
  stopWorker();
  worker = new Worker(new URL("play-worker.js", import.meta.url), { type: "module" });
  worker.onmessage = (e) => {
    stopWorker();
    if (e.data.ok) done(e.data.result);
    else fail(e.data.error, "Stopped");
  };
  worker.onerror = () => {
    stopWorker();
    fail("The playground worker failed to start.", "Failed");
  };
  runTimer = setTimeout(() => {
    stopWorker();
    fail(`Stopped after ${RUN_LIMIT_MS / 1000} seconds. The program was still running.`, "Stopped");
  }, RUN_LIMIT_MS);
  worker.postMessage({ src, stdin });
}

export function say(pane, text, cls) {
  pane.textContent = "";
  const el = document.createElement("span");
  el.className = cls || "dim";
  el.textContent = text;
  pane.append(el);
}

function block(cls, text) {
  const el = document.createElement("div");
  el.className = cls;
  el.textContent = text;
  return el;
}

export function showRun(pane, r) {
  pane.textContent = "";
  if (r.stdout) pane.append(block("stdout", r.stdout));
  if (r.stderr) pane.append(block("stderr", r.stderr));
  pane.append(block("exit " + (r.exitCode === 0 ? "good" : "bad"), "exit " + r.exitCode));
  if (!r.stdout && !r.stderr) {
    const nothing = document.createElement("span");
    nothing.className = "dim";
    nothing.textContent = "The program wrote nothing. ";
    pane.prepend(nothing);
  }
}
