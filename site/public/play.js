import { $, $$ } from "./dom.js";
import { writeClipboard } from "./copy.js";
import { loadPlay } from "./play-wasm.js";
import { highlight, scrollLayer, showDiagnostics } from "./play-paint.js";
import { runInWorker, say, showRun, stopWorker } from "./play-run.js";
import { encodeSource, sourceFromHash } from "./play-share.js";
import { mountIde } from "./play-ide.js";
import { squiggle } from "./play-intel.js";

const CHECK_DELAY_MS = 350;

function setLabel(btn, text, state) {
  if (btn.dataset.ico !== undefined) {
    btn.setAttribute("aria-label", text);
    btn.title = text;
    btn.dataset.state = state;
  } else {
    btn.textContent = text;
  }
}

export function mountPlay(root, opts = {}) {
  const editor = $("[data-play-editor]", root);
  const input = $("[data-play-src]", root);
  const layer = $("[data-play-hl]", root);
  const stdin = $("[data-play-stdin]", root);
  const stdinBox = $("[data-play-stdin-box]", root);
  const outPane = $("[data-play-out]", root);
  const diagPane = $("[data-play-diags]", root);
  const status = $("[data-play-status]", root);
  const runBtn = $("[data-play-run]", root);
  const shareBtn = $("[data-play-share]", root);
  const resetBtn = $("[data-play-reset]", root);
  const picker = $("[data-play-pick]", root);
  if (!input || !layer || !runBtn) return;
  const idleText = outPane.textContent.trim() || "Not run yet.";

  stopWorker();

  const examples = new Map();
  for (const pre of $$("[data-play-example]", root)) {
    examples.set(pre.dataset.playExample, { src: pre.textContent, stdin: "" });
  }
  for (const pre of $$("[data-play-example-stdin]", root)) {
    const e = examples.get(pre.dataset.playExampleStdin);
    if (e) e.stdin = pre.textContent;
  }

  let play = null;
  let checkTimer = null;

  function spansOf(src) {
    if (!play) return [];
    try {
      return play.tokens(src).spans || [];
    } catch (err) {
      return [];
    }
  }

  function paint() {
    const src = input.value;
    layer.innerHTML = highlight(src, spansOf(src));
    syncScroll();
  }

  function syncScroll() {
    scrollLayer(input, layer);
  }

  function showDiags(diags) {
    showDiagnostics(diagPane, diags, caretToLine);
    squiggle(srcHost(), diags);
  }

  function caretToLine(line) {
    const host = srcHost();
    const lines = host.value.split("\n");
    let at = 0;
    for (let i = 0; i < Math.min(line - 1, lines.length); i++) at += lines[i].length + 1;
    host.focus();
    host.setSelectionRange(at, at);
  }

  function recheck() {
    if (!play) return;
    if (!srcHost().value.trim()) {
      showDiags(null);
      return;
    }
    try {
      showDiags(play.check(srcHost().value).diagnostics);
    } catch (err) {
      showDiags([
        {
          line: 0,
          col: 0,
          severity: "error",
          message: "this program is nested too deep for the browser to check",
        },
      ]);
    }
  }

  function scheduleCheck() {
    clearTimeout(checkTimer);
    checkTimer = setTimeout(recheck, CHECK_DELAY_MS);
  }

  function edited() {
    paint();
    scheduleCheck();
  }

  function finish(label) {
    runBtn.disabled = false;
    setLabel(runBtn, "Run", "idle");
    status.textContent = label;
  }

  function run() {
    if (!play) return;
    showDiags(null);
    runBtn.disabled = true;
    setLabel(runBtn, "Running", "busy");
    status.textContent = "Running…";
    say(outPane, "…", "dim");
    const src = srcHost().value;
    runInWorker(src, stdin ? stdin.value : "", {
      done: (r) => {
        if (r.exitCode === undefined) {
          say(outPane, "This program did not compile.", "dim");
          // The diagnostics describe the text that ran; a newer edit gets a check of its own.
          if (srcHost().value === src) showDiags(r.diagnostics);
          else recheck();
          finish("Did not compile");
          return;
        }
        showRun(outPane, r);
        recheck();
        finish("Ran");
      },
      fail: (error, label) => {
        say(outPane, error, "stderr");
        finish(label);
      },
    });
  }

  let original = input.value;

  function load(id) {
    const e = examples.get(id);
    if (!e) return;
    input.value = e.src;
    original = e.src;
    if (stdin) stdin.value = e.stdin;
    if (stdinBox) stdinBox.open = e.stdin.length > 0;
    edited();
  }

  input.addEventListener("input", edited);
  input.addEventListener("scroll", syncScroll);

  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      run();
      return;
    }
    if (e.key === "Tab" && !e.shiftKey) {
      e.preventDefault();
      // insertText keeps the browser's undo stack; the splice below loses undo but not the text.
      if (!document.execCommand || !document.execCommand("insertText", false, "  ")) {
        const at = input.selectionStart;
        input.value = input.value.slice(0, at) + "  " + input.value.slice(input.selectionEnd);
        input.setSelectionRange(at + 2, at + 2);
      }
      edited();
    }
  });

  runBtn.addEventListener("click", run);

  if (resetBtn) {
    resetBtn.addEventListener("click", () => {
      stopWorker();
      input.value = original;
      input.focus();
      edited();
      say(outPane, idleText, "dim");
      if (play) {
        runBtn.disabled = false;
        status.textContent = "Ready";
      }
    });
  }

  if (picker) {
    picker.addEventListener("change", () => {
      load(picker.value);
      history.replaceState(null, "", location.pathname);
    });
  }

  const ide = mountIde(root, { input, layer, editor, runBtn, stdin, examples, diagPane, paint, scheduleCheck, spansOf, ready: () => play !== null, startCompiler, api: () => play });
  const srcHost = ide ? ide.srcHost : () => input;

  if (shareBtn) {
    shareBtn.addEventListener("click", async () => {
      const url = location.origin + location.pathname + "#" + (ide ? ide.shareHash() : "c=" + encodeSource(srcHost().value));
      history.replaceState(null, "", url);
      setLabel(shareBtn, (await writeClipboard(url)) ? "Copied" : "In the URL", "done");
      setTimeout(() => setLabel(shareBtn, "Copy link", "idle"), 1600);
    });
  }

  const fromLink = sourceFromHash();
  if (fromLink !== null) {
    input.value = fromLink;
    original = fromLink;
    if (picker) picker.value = "";
  } else if (picker && picker.value) {
    load(picker.value);
  }
  paint();

  let starting = false;
  function startCompiler() {
    if (play || starting) return;
    starting = true;
    loadPlay(new URL("play.wasm", import.meta.url)).then(
      (api) => {
        starting = false;
        play = api;
        runBtn.disabled = false;
        status.textContent = "Ready";
        edited();
        if (ide) ide.onPlayReady();
        if (opts.onReady) opts.onReady();
      },
      (err) => {
        starting = false;
        status.textContent = "The compiler did not load";
        say(outPane, "play.wasm did not load, so nothing here can run: " + err.message, "stderr");
      }
    );
  }
  startCompiler();
}
