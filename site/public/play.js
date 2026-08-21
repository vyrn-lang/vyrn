// The playground page: an editor the compiler colours, and a run button with a
// kill switch behind it.
//
// The division of labour is the whole design. `tokens` and `check` run on THIS
// thread, synchronously, because a colour layer that arrives one message after
// the keystroke shows a reader an editor that lags — and neither can loop
// forever, because neither runs the program. `run` goes to a worker, because it
// can, and the page terminates it when it does.
//
// Nothing here decides anything about the language. Every colour, every
// diagnostic and every byte of output comes back from `play.wasm`, which is the
// compiler's own front end.
import { loadPlay } from "./play-wasm.js";

/// How long a program may run before the page stops it.
///
/// Terminating the worker is the only way to stop running WebAssembly, so this
/// is a real kill and not a request. Five seconds is far past every example and
/// far short of a reader's patience with a page that has stopped answering.
const RUN_LIMIT_MS = 5000;

/// How long the editor waits after the last keystroke before type-checking.
/// Colouring is not debounced — that one is a lex and it is instant.
const CHECK_DELAY_MS = 350;

const $ = (sel, root) => root.querySelector(sel);
const $$ = (sel, root) => Array.from(root.querySelectorAll(sel));

// ---------------------------------------------------------------------------
// The URL contract, which the guide book writes links against.
//
//   play.html#c=<base64url(utf8 source)>
//
// base64url per RFC 4648 §5: `+` becomes `-`, `/` becomes `_`, and the padding
// is dropped. `site/app/guide.vyrn` builds these and `site/export.vyrn` asserts
// every one of them; this is the other half of that agreement. A fragment never
// reaches a server, so a whole program travels in a link and nothing is stored.
// ---------------------------------------------------------------------------

function encodeSource(src) {
  const bytes = new TextEncoder().encode(src);
  let binary = "";
  // In chunks: `String.fromCharCode(...arr)` is one argument per byte and
  // overflows the argument limit on a program of any size.
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  }
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function decodeSource(code) {
  let b64 = code.replace(/-/g, "+").replace(/_/g, "/");
  // `atob` throws on a length that leaves a remainder of 1, and accepts 2 and 3;
  // padding it back makes every one of the three the same case.
  b64 += "=".repeat((4 - (b64.length % 4)) % 4);
  const binary = atob(b64);
  return new TextDecoder().decode(Uint8Array.from(binary, (c) => c.charCodeAt(0)));
}

/// The program in the URL fragment, or null when there is none.
function sourceFromHash() {
  const m = /[#&]c=([A-Za-z0-9\-_]+)/.exec(location.hash || "");
  if (!m) return null;
  try {
    return decodeSource(m[1]);
  } catch (err) {
    return null;
  }
}

// ---------------------------------------------------------------------------
// The colour layer
// ---------------------------------------------------------------------------

function escapeHtml(s) {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/// `src` as coloured markup, from the spans the compiler's lexer returned.
///
/// The classes are the stylesheet's own — `k`, `s`, `n`, `c`, `t` — so a program
/// typed here is coloured exactly like the same program printed in the guide.
/// The trailing newline is deliberate: a `<pre>` swallows one, and without it the
/// last line of the overlay sits a line above the last line of the textarea.
function highlight(src, spans) {
  let out = "";
  let at = 0;
  for (const [start, len, cls] of spans) {
    if (start < at) continue;
    out += escapeHtml(src.slice(at, start));
    out += '<span class="' + cls + '">' + escapeHtml(src.slice(start, start + len)) + "</span>";
    at = start + len;
  }
  return out + escapeHtml(src.slice(at)) + "\n";
}

// ---------------------------------------------------------------------------

/// Put `text` on the clipboard, and say whether it worked. A refusal is a real
/// answer — the clipboard is missing on an insecure origin and can be denied by
/// permission — so the caller shows what happened rather than pretending.
async function writeClipboard(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch (err) {
    return false;
  }
}

/// The worker, and the state that has to outlive one mount: a soft navigation
/// away and back builds a new page, and a worker left behind would keep a 16 MB
/// instance and a message handler alive over a DOM that is gone.
let worker = null;

function stopWorker() {
  if (worker) {
    worker.terminate();
    worker = null;
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
  // The idle line this pane started with, so `Reset` puts back what the page
  // said rather than a sentence written twice — once here and once in a template.
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
  let runTimer = null;

  // -----------------------------------------------------------------------

  /// Recolour the editor. A source that cannot be lexed at all — half a string
  /// literal, on the way to typing the closing quote — has no spans, and the
  /// overlay falls back to plain text so the reader never sees the editor go
  /// blank under the caret.
  function paint() {
    const src = input.value;
    let spans = [];
    if (play) {
      try {
        const t = play.tokens(src);
        if (t.spans) spans = t.spans;
      } catch (err) {
        spans = [];
      }
    }
    layer.innerHTML = highlight(src, spans);
    syncScroll();
  }

  function syncScroll() {
    const pre = layer.parentElement;
    pre.scrollTop = input.scrollTop;
    pre.scrollLeft = input.scrollLeft;
  }

  /// Show the compiler's diagnostics under the editor. Clicking one puts the
  /// caret on the line it is about.
  function showDiagnostics(diags) {
    diagPane.textContent = "";
    if (!diags || !diags.length) {
      diagPane.hidden = true;
      return;
    }
    for (const d of diags) {
      const row = document.createElement("button");
      row.type = "button";
      row.className = "diag " + d.severity;
      const where = document.createElement("span");
      where.className = "at";
      where.textContent = d.col > 0 ? `${d.line}:${d.col}` : String(d.line);
      row.append(where, document.createTextNode(d.message));
      if (d.note) {
        const note = document.createElement("span");
        note.className = "note";
        note.textContent = d.note;
        row.append(note);
      }
      row.addEventListener("click", () => caretToLine(d.line));
      diagPane.append(row);
    }
    diagPane.hidden = false;
  }

  function caretToLine(line) {
    const lines = input.value.split("\n");
    let at = 0;
    for (let i = 0; i < Math.min(line - 1, lines.length); i++) at += lines[i].length + 1;
    input.focus();
    input.setSelectionRange(at, at);
  }

  function recheck() {
    if (!play) return;
    try {
      showDiagnostics(play.check(input.value).diagnostics);
    } catch (err) {
      // The checker recurses over the syntax tree, and a program nested deep
      // enough can exhaust the engine's stack here too. Say so instead of
      // showing the last answer as though it were about this program.
      showDiagnostics([
        {
          line: 0,
          col: 0,
          severity: "error",
          message: "this program is nested too deep for the browser to check",
        },
      ]);
    }
  }

  function edited() {
    paint();
    clearTimeout(checkTimer);
    checkTimer = setTimeout(recheck, CHECK_DELAY_MS);
  }

  // -----------------------------------------------------------------------
  // Output
  // -----------------------------------------------------------------------

  function say(text, cls) {
    outPane.textContent = "";
    const el = document.createElement("span");
    el.className = cls || "dim";
    el.textContent = text;
    outPane.append(el);
  }

  /// What the program wrote, with the two channels kept apart, and the exit code
  /// under them — the three things a terminal shows.
  function showRun(r) {
    outPane.textContent = "";
    if (r.stdout) {
      const el = document.createElement("div");
      el.className = "stdout";
      el.textContent = r.stdout;
      outPane.append(el);
    }
    if (r.stderr) {
      const el = document.createElement("div");
      el.className = "stderr";
      el.textContent = r.stderr;
      outPane.append(el);
    }
    const code = document.createElement("div");
    code.className = "exit " + (r.exitCode === 0 ? "good" : "bad");
    code.textContent = "exit " + r.exitCode;
    outPane.append(code);
    if (!r.stdout && !r.stderr) {
      const nothing = document.createElement("span");
      nothing.className = "dim";
      nothing.textContent = "The program wrote nothing. ";
      outPane.prepend(nothing);
    }
  }

  function finish(label) {
    clearTimeout(runTimer);
    runBtn.disabled = false;
    runBtn.textContent = "Run";
    status.textContent = label;
  }

  function run() {
    if (!play) return;
    stopWorker();
    showDiagnostics(null);
    runBtn.disabled = true;
    runBtn.textContent = "Running";
    status.textContent = "Running…";
    say("…", "dim");

    worker = new Worker(new URL("play-worker.js", import.meta.url), { type: "module" });
    worker.onmessage = (e) => {
      const m = e.data;
      stopWorker();
      if (!m.ok) {
        say(m.error, "stderr");
        finish("Stopped");
        return;
      }
      const r = m.result;
      if (r.exitCode === undefined) {
        // It did not compile. The diagnostics are the answer, not the output.
        say("This program did not compile.", "dim");
        showDiagnostics(r.diagnostics);
        finish("Did not compile");
        return;
      }
      showRun(r);
      showDiagnostics(r.diagnostics);
      finish("Ran");
    };
    worker.onerror = () => {
      stopWorker();
      say("The playground worker failed to start.", "stderr");
      finish("Failed");
    };
    // The kill switch. `terminate` is the only thing that stops running
    // WebAssembly, and a program that loops forever is a program a reader will
    // write on purpose within about a minute of finding this page.
    runTimer = setTimeout(() => {
      stopWorker();
      say(`Stopped after ${RUN_LIMIT_MS / 1000} seconds. The program was still running.`, "stderr");
      finish("Stopped");
    }, RUN_LIMIT_MS);

    worker.postMessage({ src: input.value, stdin: stdin ? stdin.value : "", now: Date.now() });
  }

  // -----------------------------------------------------------------------
  // Controls
  // -----------------------------------------------------------------------

  // WHAT `Reset` PUTS BACK (RFC-0106 M3, fourth round). The program this editor
  // started from: the example the picker holds, the program a shared link
  // carried, or — on the index's hero — the snippet the page was built with. An
  // editor a reader can type into with no way back is one they stop typing into.
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
      // Through the editing command, so the browser's own undo stack keeps the
      // insertion. The fallback splices the value, which loses undo but never
      // loses the character.
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
      say(idleText, "dim");
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

  if (shareBtn) {
    shareBtn.addEventListener("click", async () => {
      const url = location.origin + location.pathname + "#c=" + encodeSource(input.value);
      history.replaceState(null, "", url);
      shareBtn.textContent = (await writeClipboard(url)) ? "Copied" : "In the URL";
      setTimeout(() => (shareBtn.textContent = "Copy link"), 1600);
    });
  }

  // -----------------------------------------------------------------------

  const fromLink = sourceFromHash();
  if (fromLink !== null) {
    input.value = fromLink;
    // A shared link IS the original here: `Reset` goes back to the program the
    // link carried, not to whatever the picker happened to hold.
    original = fromLink;
    if (picker) picker.value = "";
  } else if (picker && picker.value) {
    load(picker.value);
  }
  paint();

  loadPlay(new URL("play.wasm", import.meta.url)).then(
    (api) => {
      play = api;
      runBtn.disabled = false;
      status.textContent = "Ready";
      edited();
      // The index's hero editor is armed on first interaction and mounted then
      // (RFC-0106 M2), so the press that armed it arrives before anything can
      // run. `onReady` is how that press is honoured rather than swallowed.
      if (opts.onReady) opts.onReady();
    },
    (err) => {
      status.textContent = "The compiler did not load";
      say("play.wasm did not load, so nothing here can run: " + err.message, "stderr");
    }
  );
}
