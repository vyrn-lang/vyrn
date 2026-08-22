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
/// A button's label and state. An icon button (`data-ico`) keeps its glyph
/// and carries the words on its title and accessible name; a text button
/// shows them.
function setLabel(btn, text, state) {
  if (btn.dataset.ico !== undefined) {
    btn.setAttribute("aria-label", text);
    btn.title = text;
    btn.dataset.state = state;
  } else {
    btn.textContent = text;
  }
}
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
  // Which editor group the compiler reads: the frame (a second, split group
  // exists on /play) points this at the focused one; alone, it is the one
  // textarea this mount owns.
  let srcHost = () => input;
  // The shell sets this; the core calls it the moment the compiler lands, so
  // every editor group repaints with real spans and the Run buttons settle.
  let onPlayReady = null;
  // What the share button copies. The shell replaces it with the WHOLE
  // project (user); a hero editor has no project, so the core's one-file
  // link stands.
  let shareHash = null;

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
    const host = srcHost();
    const lines = host.value.split("\n");
    let at = 0;
    for (let i = 0; i < Math.min(line - 1, lines.length); i++) at += lines[i].length + 1;
    host.focus();
    host.setSelectionRange(at, at);
  }

  function recheck() {
    if (!play) return;
    try {
      showDiagnostics(play.check(srcHost().value).diagnostics);
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
    setLabel(runBtn, "Run", "idle");
    status.textContent = label;
  }

  function run() {
    if (!play) return;
    stopWorker();
    showDiagnostics(null);
    runBtn.disabled = true;
    setLabel(runBtn, "Running", "busy");
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

    worker.postMessage({ src: srcHost().value, stdin: stdin ? stdin.value : "", now: Date.now() });
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

  // =========================================================================
  // The editor shell (RFC-0106 M5, rounds 4-12). Everything below exists only
  // on /play — every hook is queried and silently absent on the hero editors —
  // and everything is REAL: files and folders persist in this browser and the
  // session (open tabs, the split, the pane sizes) comes back on the next
  // visit; the examples are templates a file is created FROM; tabs and
  // explorer rows drag into either editor group; the panes resize on sashes;
  // the keys are IntelliJ's; the project downloads as a zip.
  // =========================================================================
  const gutter = $("[data-play-gutter]", root);
  const lncol = $("[data-play-lncol]", root);
  const crumbs = $("[data-play-crumbs]", root);
  const curline = $("[data-play-curline]", root);
  const minimap = $("[data-play-minimap]", root);
  const mmtext = $("[data-play-mmtext]", root);
  const mmview = $("[data-play-mmview]", root);
  const ptabs = $$("[data-play-ptab]", root);
  const panes = $$("[data-play-pane]", root);
  const badge = $("[data-play-badge]", root);
  const errsEl = $("[data-play-errs]", root);
  const noprob = $("[data-play-noprob]", root);
  const tabsBox = $("[data-play-tabs]", root);
  const filesBox = $("[data-play-files]", root);
  const welcomeEl = $("[data-play-welcome]", root);
  const wrapEl = $(".idewrap", root);
  const uploadInput = $("[data-play-uploadinput]", root);

  // ---- the panel's own tabs ----------------------------------------------
  function showPane(k) {
    for (const t of ptabs) t.classList.toggle("on", t.dataset.playPtab === k);
    for (const p of panes) p.hidden = p.dataset.playPane !== k;
  }
  for (const t of ptabs) t.addEventListener("click", () => showPane(t.dataset.playPtab));
  if (ptabs.length && diagPane) {
    const syncProblems = () => {
      const n = diagPane.hidden ? 0 : diagPane.children.length;
      if (badge) {
        badge.textContent = n;
        badge.hidden = n === 0;
      }
      if (errsEl) errsEl.textContent = n;
      if (noprob) noprob.hidden = n > 0;
      const onProblems = ptabs.some((t) => t.dataset.playPtab === "problems" && t.classList.contains("on"));
      if (n > 0) showPane("problems");
      else if (onProblems) showPane("output");
    };
    new MutationObserver(syncProblems).observe(diagPane, { childList: true, attributes: true, attributeFilter: ["hidden"] });
    syncProblems();
  }

  if (tabsBox && filesBox && wrapEl) {
    // ---- persistence: files, folders, layout, session -----------------------
    const KEY_FILES = "vyrn.play.files.v1";
    const KEY_FOLDERS = "vyrn.play.folders.v1";
    const KEY_LAYOUT = "vyrn.play.layout.v1";
    const KEY_SESSION = "vyrn.play.session.v1";
    const read = (k, fallback) => {
      try {
        const v = JSON.parse(localStorage.getItem(k));
        return v === null || v === undefined ? fallback : v;
      } catch (err) {
        return fallback;
      }
    };
    const write = (k, v) => {
      try {
        localStorage.setItem(k, JSON.stringify(v));
      } catch (err) {
        /* a full or absent store loses persistence, never the session */
      }
    };
    let userFiles = new Map(read(KEY_FILES, []).map((f) => [f.name, f.text]));
    const folders = new Set(read(KEY_FOLDERS, []));
    const layout = Object.assign({ side: 224, panel: 200, split: 0.5 }, read(KEY_LAYOUT, {}));
    const collapsed = new Set();
    let saveTimer = 0;
    function persistFiles() {
      clearTimeout(saveTimer);
      saveTimer = setTimeout(() => {
        write(KEY_FILES, [...userFiles].map(([name, text]) => ({ name, text })));
        write(KEY_FOLDERS, [...folders]);
      }, 150);
    }
    function persistSession() {
      write(KEY_SESSION, {
        a: { tabs: gA.tabs, active: gA.active },
        b: gB ? { tabs: gB.tabs, active: gB.active } : null,
        focused: focusedG === gB ? "b" : "a",
      });
    }
    function applyLayout() {
      wrapEl.style.setProperty("--side-w", layout.side + "px");
      wrapEl.style.setProperty("--panel-h", layout.panel + "px");
      wrapEl.style.setProperty("--split-a", layout.split + "fr");
      wrapEl.style.setProperty("--split-b", 1 - layout.split + "fr");
      write(KEY_LAYOUT, layout);
    }

    function freshName(base) {
      if (!userFiles.has(base)) return base;
      const m = base.match(/^(.*?)(\.[^./]*)?$/);
      const stem = m[1];
      const ext = m[2] || "";
      for (let i = 2; ; i++) {
        const name = stem + "-" + i + ext;
        if (!userFiles.has(name)) return name;
      }
    }

    // ---- editor groups ----------------------------------------------------
    const WELCOME = "__welcome";
    function makeGroup(els) {
      return Object.assign({ tabs: [], active: null }, els);
    }
    const gA = makeGroup({ strip: tabsBox, input, layer, gutter, curline, editorEl: editor, main: $(".idemain", root), runBtn, minimap, mmtext, mmview });
    let gB = null;
    let focusedG = gA;
    srcHost = () => focusedG.input;

    const labelOf = (name) => (name === WELCOME ? "Welcome" : name.slice(name.lastIndexOf("/") + 1));
    /// Which Run buttons are pressable: the compiler has to be there, and the
    /// group has to hold a file — the Welcome view runs nothing (user).
    function updateRunState() {
      const busy = runBtn.dataset.state === "busy";
      for (const g of groups()) {
        if (!g.runBtn) continue;
        const want = !play || busy || !g.active || g.active === WELCOME;
        // WRITTEN ONLY WHEN IT CHANGES. The observer below watches the core's
        // own Run button, and an unconditional write to it is a mutation that
        // re-enters this function for ever — the loop that froze the tab.
        if (g.runBtn.disabled !== want) g.runBtn.disabled = want;
      }
    }
    // The core disables its own Run while a program runs; the split's follows.
    new MutationObserver(updateRunState).observe(runBtn, { attributes: true, attributeFilter: ["disabled", "data-state"] });
    const contentOf = (name) => userFiles.get(name) || "";
    const groups = () => (gB ? [gA, gB] : [gA]);

    function paintG(g) {
      if (g === gA) {
        paint();
        return;
      }
      let spans = [];
      if (play) {
        try {
          const t = play.tokens(g.input.value);
          if (t.spans) spans = t.spans;
        } catch (err) {
          spans = [];
        }
      }
      g.layer.innerHTML = highlight(g.input.value, spans);
      const pre = g.layer.parentElement;
      pre.scrollTop = g.input.scrollTop;
      pre.scrollLeft = g.input.scrollLeft;
    }

    function syncG(g) {
      if (g.gutter) {
        const n = g.input.value.split("\n").length;
        if (g.gutter.childElementCount !== n) {
          g.gutter.textContent = "";
          for (let i = 1; i <= n; i++) {
            const d = document.createElement("span");
            d.textContent = i;
            g.gutter.append(d);
          }
        }
        g.gutter.scrollTop = g.input.scrollTop;
      }
      const before = g.input.value.slice(0, g.input.selectionStart).split("\n");
      const line = before.length;
      if (g.gutter) {
        const kids = g.gutter.children;
        for (let i = 0; i < kids.length; i++) kids[i].classList.toggle("on", i + 1 === line);
      }
      if (g.curline) {
        const cs = getComputedStyle(g.input);
        const lh = parseFloat(cs.lineHeight);
        g.curline.style.top = parseFloat(cs.paddingTop) + (line - 1) * lh - g.input.scrollTop + "px";
        g.curline.style.height = lh + "px";
      }
      if (g === focusedG && lncol) lncol.textContent = "Ln " + line + ", Col " + (before[before.length - 1].length + 1);
      if (g.mmtext) {
        if (g.mmtext.textContent !== g.input.value) g.mmtext.textContent = g.input.value;
        if (g.mmview) {
          const mapH = g.mmtext.scrollHeight;
          g.mmview.style.top = (g.input.scrollTop / g.input.scrollHeight) * mapH + "px";
          g.mmview.style.height = Math.max(12, (g.input.clientHeight / g.input.scrollHeight) * mapH) + "px";
        }
      }
    }

    function syncChrome() {
      if (crumbs) crumbs.textContent = focusedG.active && focusedG.active !== WELCOME ? focusedG.active.split("/").join(" \u203a ") : "Welcome";
      renderFiles();
      persistSession();
    }

    const welcomeShown = () => gA.active === WELCOME;
    function applyWelcome() {
      const on = welcomeShown();
      if (welcomeEl) welcomeEl.hidden = !on;
      gA.editorEl.hidden = on;
    }
    runBtn.addEventListener(
      "click",
      (e) => {
        if (focusedG === gA && welcomeShown()) e.stopImmediatePropagation();
      },
      { capture: true },
    );

    function activateIn(g, name) {
      if (g === gB && name === WELCOME) return;
      if (!userFiles.has(name) && name !== WELCOME) return;
      if (!g.tabs.includes(name)) g.tabs.push(name);
      g.active = name;
      focusedG = g;
      if (name !== WELCOME) {
        g.input.value = contentOf(name);
        paintG(g);
        clearTimeout(checkTimer);
        checkTimer = setTimeout(recheck, CHECK_DELAY_MS);
      }
      if (g === gA) applyWelcome();
      for (const grp of groups()) renderTabsG(grp);
      syncG(g);
      syncChrome();
      updateRunState();
    }

    function closeIn(g, name) {
      const at = g.tabs.indexOf(name);
      if (at < 0) return;
      g.tabs.splice(at, 1);
      if (g.active === name) {
        const next = g.tabs[at] || g.tabs[at - 1];
        if (next) activateIn(g, next);
        else if (g === gB) unsplit();
        else activateIn(gA, WELCOME);
      } else {
        renderTabsG(g);
        persistSession();
      }
      if (g === gB && gB && !gB.tabs.length) unsplit();
    }

    // ---- the second group: a real editor, built when a tab is sent right --
    function ensureSplit() {
      if (gB) return gB;
      const sash = document.createElement("div");
      sash.className = "sash v";
      sash.dataset.playSash = "split";
      const wrap = document.createElement("div");
      wrap.className = "idemain second";
      wrap.innerHTML =
        '<div class="idebar"><span class="idetabs"></span><span class="spacer"></span>' +
        '<button type="button" class="btn ib primary" data-ico="play" disabled="disabled" aria-label="Run (Shift+F10)" title="Run (Shift+F10)"></button></div>' +
        '<div class="idecrumbs" aria-hidden="true"></div>' +
        '<div class="editor"><div class="curline" aria-hidden="true"></div><div class="gutter" aria-hidden="true"></div>' +
        '<pre class="code hl" aria-hidden="true"><code></code></pre>' +
        '<textarea spellcheck="false" autocapitalize="off" autocorrect="off" autocomplete="off" wrap="off" aria-label="Vyrn source, second editor group"></textarea>' +
        '<div class="minimap" aria-hidden="true"><pre class="mmtext"></pre><div class="mmview"></div></div></div>';
      gA.main.after(sash, wrap);
      wrapEl.classList.add("splitview");
      gB = makeGroup({
        strip: $(".idetabs", wrap),
        input: $("textarea", wrap),
        layer: $("code", wrap),
        gutter: $(".gutter", wrap),
        curline: $(".curline", wrap),
        editorEl: $(".editor", wrap),
        crumbs: $(".idecrumbs", wrap),
        main: wrap,
        sash,
        runBtn: $(".btn.ib", wrap),
        minimap: $(".minimap", wrap),
        mmtext: $(".mmtext", wrap),
        mmview: $(".mmview", wrap),
      });
      bindGroup(gB);
      bindSash(sash);
      bindMinimap(gB);
      // The split's Run is the same run, on the group that asked for it.
      gB.runBtn.addEventListener("click", () => {
        focusedG = gB;
        runBtn.click();
      });
      updateRunState();
      return gB;
    }
    function unsplit() {
      if (!gB) return;
      gB.main.remove();
      gB.sash.remove();
      wrapEl.classList.remove("splitview");
      gB = null;
      focusedG = gA;
      if (!gA.active) activateIn(gA, WELCOME);
      else syncChrome();
    }

    // ---- one binding for any group's editor -------------------------------
    function bindGroup(g) {
      g.input.addEventListener("focus", () => {
        focusedG = g;
        syncG(g);
        syncChrome();
      });
      g.input.addEventListener("input", () => {
        if (g.active && g.active !== WELCOME) {
          userFiles.set(g.active, g.input.value);
          persistFiles();
        }
        if (g !== gA) {
          paintG(g);
          clearTimeout(checkTimer);
          checkTimer = setTimeout(recheck, CHECK_DELAY_MS);
        }
        syncG(g);
      });
      ["keyup", "click"].forEach((ev) => g.input.addEventListener(ev, () => syncG(g)));
      g.input.addEventListener("scroll", () => {
        if (g.gutter) g.gutter.scrollTop = g.input.scrollTop;
        const pre = g.layer.parentElement;
        pre.scrollTop = g.input.scrollTop;
        pre.scrollLeft = g.input.scrollLeft;
        syncG(g);
      });
      g.input.addEventListener("keydown", (e) => editorKeys(g, e));
    }

    // ---- the keys: IntelliJ's, on whichever editor has focus --------------
    function lineBounds(t, at) {
      const s0 = t.value.lastIndexOf("\n", at - 1) + 1;
      let e0 = t.value.indexOf("\n", at);
      if (e0 < 0) e0 = t.value.length;
      return [s0, e0];
    }
    function replaceAll(t, text, caret) {
      t.value = text;
      t.selectionStart = t.selectionEnd = caret;
      t.dispatchEvent(new Event("input", { bubbles: true }));
    }
    function editorKeys(g, e) {
      const t = g.input;
      const ctrl = e.ctrlKey || e.metaKey;
      if (e.key === "Tab" && g !== gA) {
        e.preventDefault();
        t.setRangeText("  ", t.selectionStart, t.selectionEnd, "end");
        t.dispatchEvent(new Event("input", { bubbles: true }));
        return;
      }
      if (e.key === "Enter" && !e.isComposing && !ctrl) {
        e.preventDefault();
        const at = t.selectionStart;
        const before = t.value.slice(0, at);
        const line = before.slice(before.lastIndexOf("\n") + 1);
        const indent = (line.match(/^ */) || [""])[0];
        const opened = /\{\s*$/.test(line);
        const closingNext = t.value.slice(t.selectionEnd).startsWith("}");
        let insert = "\n" + indent + (opened ? "  " : "");
        if (opened && closingNext) insert += "\n" + indent;
        const caret = at + 1 + indent.length + (opened ? 2 : 0);
        t.setRangeText(insert, at, t.selectionEnd, "end");
        t.selectionStart = t.selectionEnd = caret;
        t.dispatchEvent(new Event("input", { bubbles: true }));
        return;
      }
      const [s0, e0] = lineBounds(t, t.selectionStart);
      const line = t.value.slice(s0, e0);
      // Ctrl+D duplicates the line; Ctrl+Y deletes it; Ctrl+/ toggles `//`;
      // Ctrl+Shift+Up/Down moves it — IntelliJ's bindings.
      if (ctrl && !e.shiftKey && e.key.toLowerCase() === "d") {
        e.preventDefault();
        replaceAll(t, t.value.slice(0, e0) + "\n" + line + t.value.slice(e0), t.selectionStart + line.length + 1);
      } else if (ctrl && !e.shiftKey && e.key.toLowerCase() === "y") {
        e.preventDefault();
        const cut = e0 < t.value.length ? e0 + 1 : Math.max(0, s0 - 1);
        replaceAll(t, t.value.slice(0, Math.min(s0, cut)) + t.value.slice(Math.max(e0 + 1, cut)), Math.min(s0, t.value.length));
      } else if (ctrl && e.key === "/") {
        e.preventDefault();
        const on = /^\s*\/\/ ?/.test(line);
        const next = on ? line.replace(/^(\s*)\/\/ ?/, "$1") : line.replace(/^(\s*)/, "$1// ");
        replaceAll(t, t.value.slice(0, s0) + next + t.value.slice(e0), s0 + Math.min(next.length, t.selectionStart - s0 + (on ? -3 : 3)));
      } else if (ctrl && e.shiftKey && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
        e.preventDefault();
        const lines = t.value.split("\n");
        const idx = t.value.slice(0, t.selectionStart).split("\n").length - 1;
        const to = e.key === "ArrowUp" ? idx - 1 : idx + 1;
        if (to < 0 || to >= lines.length) return;
        const col = t.selectionStart - s0;
        [lines[idx], lines[to]] = [lines[to], lines[idx]];
        const text = lines.join("\n");
        let at = 0;
        for (let i = 0; i < to; i++) at += lines[i].length + 1;
        replaceAll(t, text, at + Math.min(col, lines[to].length));
      }
    }
    addEventListener("keydown", (e) => {
      if (!root.isConnected) return;
      const ctrl = e.ctrlKey || e.metaKey;
      if (e.key === "F10" && e.shiftKey) {
        e.preventDefault();
        runBtn.click();
      } else if (e.key === "F4" && ctrl) {
        e.preventDefault();
        if (focusedG.active) closeIn(focusedG, focusedG.active);
      } else if (e.altKey && (e.key === "ArrowRight" || e.key === "ArrowLeft") && focusedG.tabs.length > 1) {
        e.preventDefault();
        const i = focusedG.tabs.indexOf(focusedG.active);
        const n = focusedG.tabs.length;
        activateIn(focusedG, focusedG.tabs[(i + (e.key === "ArrowRight" ? 1 : n - 1)) % n]);
      } else if (e.altKey && e.key === "Insert") {
        e.preventDefault();
        newEntryInline("file", "");
      } else if (e.key === "F6" && e.shiftKey) {
        e.preventDefault();
        if (focusedG.active && focusedG.active !== WELCOME) renameInline(null, focusedG.active);
      }
    });
    bindGroup(gA);

    // ---- context menus ----------------------------------------------------
    let menuEl = null;
    function closeMenu() {
      if (menuEl) {
        menuEl.remove();
        menuEl = null;
      }
    }
    function openMenu(x, y, items) {
      closeMenu();
      menuEl = document.createElement("div");
      menuEl.className = "ctxmenu";
      menuEl.setAttribute("role", "menu");
      for (const it of items) {
        if (it === "-") {
          const hr = document.createElement("div");
          hr.className = "sep";
          menuEl.append(hr);
          continue;
        }
        const b = document.createElement("button");
        b.type = "button";
        b.className = "mi";
        b.setAttribute("role", "menuitem");
        b.textContent = it.label;
        if (it.keys) {
          const k = document.createElement("span");
          k.className = "mk";
          k.textContent = it.keys;
          b.append(k);
        }
        b.addEventListener("click", () => {
          closeMenu();
          it.act();
        });
        menuEl.append(b);
      }
      document.body.append(menuEl);
      const r = menuEl.getBoundingClientRect();
      menuEl.style.left = Math.min(x, innerWidth - r.width - 6) + "px";
      menuEl.style.top = Math.min(y, innerHeight - r.height - 6) + "px";
    }
    addEventListener("pointerdown", (e) => {
      if (menuEl && !menuEl.contains(e.target)) closeMenu();
    });
    addEventListener("keydown", (e) => {
      if (e.key === "Escape") closeMenu();
    });
    const openElsewhere = (name) => window.open(location.pathname + "#c=" + encodeSource(contentOf(name)), "_blank");

    // ---- dragging: a tab or an explorer row, into any group ---------------
    let drag = null;
    function groupAt(x, y) {
      for (const g of groups()) {
        const r = g.main.getBoundingClientRect();
        if (x >= r.left && x <= r.right && y >= r.top && y <= r.bottom) return g;
      }
      return null;
    }
    function beginDrag(e, name, fromG) {
      if (e.button !== 0) return;
      const sx = e.clientX;
      const sy = e.clientY;
      drag = { name, fromG, moved: false, ghost: null, target: null, index: -1, split: false };
      const move = (ev) => {
        if (!drag.moved && Math.hypot(ev.clientX - sx, ev.clientY - sy) < 5) return;
        if (!drag.moved) {
          drag.moved = true;
          drag.ghost = document.createElement("span");
          drag.ghost.className = "tabghost";
          drag.ghost.textContent = labelOf(name);
          document.body.append(drag.ghost);
          wrapEl.classList.add("dragging");
        }
        drag.ghost.style.left = ev.clientX + 8 + "px";
        drag.ghost.style.top = ev.clientY + 8 + "px";
        // A folder row under the pointer takes the file (user: explorer
        // drag and drop) — checked before the editor groups, because the
        // explorer is not one.
        const over = document.elementFromPoint(ev.clientX, ev.clientY);
        const dirRow = over && over.closest ? over.closest(".idefile.dir") : null;
        const rootDrop = !dirRow && over && over.closest && over.closest(".ideside");
        dropDir = dirRow ? dirRow.dataset.path : rootDrop ? "" : null;
        for (const r of $$(".idefile.dir", root)) r.classList.toggle("dropinto", r === dirRow);
        if (dropDir !== null) {
          for (const grp of groups()) grp.main.classList.remove("dropzone");
          wrapEl.classList.remove("splitzone");
          drag.target = null;
          drag.split = false;
          return;
        }
        const g = groupAt(ev.clientX, ev.clientY);
        drag.target = g;
        drag.index = -1;
        drag.split = false;
        for (const grp of groups()) grp.main.classList.toggle("dropzone", grp === g);
        if (g) {
          const sr = g.strip.getBoundingClientRect();
          if (ev.clientY <= sr.bottom + 8) {
            drag.index = g.tabs.length;
            for (let i = 0; i < g.strip.children.length; i++) {
              const cr = g.strip.children[i].getBoundingClientRect();
              if (ev.clientX < cr.left + cr.width / 2) {
                drag.index = i;
                break;
              }
            }
          }
          // Live reorder within the strip it came from.
          if (fromG && g === fromG && drag.index >= 0) {
            const from = g.tabs.indexOf(name);
            const to = drag.index;
            if (from >= 0 && to !== from && to !== from + 1) {
              g.tabs.splice(from, 1);
              g.tabs.splice(to > from ? to - 1 : to, 0, name);
              renderTabsG(g);
            }
          }
          const er = g.editorEl.getBoundingClientRect();
          drag.split = !gB && g === gA && name !== WELCOME && ev.clientX > er.left + er.width * 0.66 && ev.clientY > er.top && ev.clientY < er.bottom;
        }
        wrapEl.classList.toggle("splitzone", drag.split);
      };
      const up = () => {
        removeEventListener("pointermove", move);
        removeEventListener("pointerup", up);
        wrapEl.classList.remove("dragging", "splitzone");
        for (const grp of groups()) grp.main.classList.remove("dropzone");
        if (drag && drag.ghost) drag.ghost.remove();
        for (const r of $$(".idefile.dir", root)) r.classList.remove("dropinto");
        if (drag && drag.moved) {
          if (dropDir !== null && name !== WELCOME) moveInto(name, dropDir);
          else if (drag.split) sendTo(name, ensureSplit(), fromG);
          else if (drag.target) sendTo(name, drag.target, fromG, drag.index);
          else if (fromG) activateIn(fromG, name);
        }
        dropDir = null;
        setTimeout(() => (drag = null));
      };
      addEventListener("pointermove", move);
      addEventListener("pointerup", up);
    }
    // Put `name` in group `to` (at `index` if given), removing it from `from`.
    function sendTo(name, to, from, index) {
      if (name === WELCOME && to !== gA) return;
      if (from && from !== to) {
        const at = from.tabs.indexOf(name);
        if (at >= 0) from.tabs.splice(at, 1);
        if (from.active === name) from.active = from.tabs[at] || from.tabs[at - 1] || null;
        if (from === gA && !gA.active) {
          gA.active = WELCOME;
          gA.tabs.push(WELCOME);
          applyWelcome();
        }
        if (from === gB && !gB.tabs.length) unsplit();
        else if (from.active) {
          if (from === gA) {
            gA.input.value = contentOf(gA.active);
            if (gA.active !== WELCOME) paintG(gA);
            applyWelcome();
          } else {
            gB.input.value = contentOf(gB.active);
            paintG(gB);
          }
        }
      }
      if (!to.tabs.includes(name)) {
        if (index === undefined || index < 0 || index > to.tabs.length) to.tabs.push(name);
        else to.tabs.splice(index, 0, name);
      }
      activateIn(to, name);
    }
    const sendRight = (name) => sendTo(name, ensureSplit(), groups().find((g) => g.tabs.includes(name)) || null);

    // ---- tabs -------------------------------------------------------------
    function renderTabsG(g) {
      g.strip.textContent = "";
      for (const name of g.tabs) {
        const box = document.createElement("span");
        box.className = "itab" + (name === g.active ? " on" : "");
        box.dataset.name = name;
        const btn = document.createElement("button");
        btn.type = "button";
        btn.className = "tname";
        btn.dataset.ico = name === WELCOME ? "home" : "file";
        btn.append(document.createTextNode(labelOf(name)));
        btn.addEventListener("pointerdown", (e) => beginDrag(e, name, g));
        btn.addEventListener("click", () => {
          if (!drag || !drag.moved) activateIn(g, name);
        });
        btn.addEventListener("contextmenu", (e) => {
          e.preventDefault();
          const items = [
            { label: "Close", keys: "Ctrl+F4", act: () => closeIn(g, name) },
            { label: "Close others", act: () => { g.tabs = [name]; activateIn(g, name); } },
          ];
          if (name !== WELCOME) {
            items.push("-");
            if (g === gA) items.push({ label: "Split right", act: () => sendRight(name) });
            else items.push({ label: "Move left", act: () => sendTo(name, gA, gB) });
            items.push({ label: "Move to new window", act: () => openElsewhere(name) });
          }
          openMenu(e.clientX, e.clientY, items);
        });
        const close = document.createElement("button");
        close.type = "button";
        close.className = "tclose";
        close.setAttribute("aria-label", "Close " + labelOf(name));
        close.dataset.ico = "close";
        close.addEventListener("click", (e) => {
          e.stopPropagation();
          closeIn(g, name);
        });
        box.append(btn, close);
        g.strip.append(box);
      }
      if (g.crumbs) g.crumbs.textContent = g.active ? g.active.split("/").join(" \u203a ") : "";
    }

    // ---- the explorer: a tree of your files ------------------------------
    function treeOf() {
      // Every folder implied by a path, plus the explicit (possibly empty) ones.
      const dirs = new Set(folders);
      for (const name of userFiles.keys()) {
        const parts = name.split("/");
        for (let i = 1; i < parts.length; i++) dirs.add(parts.slice(0, i).join("/"));
      }
      const childrenOf = (dir) => {
        const out = [];
        const prefix = dir ? dir + "/" : "";
        for (const d of dirs) if (d.startsWith(prefix) && !d.slice(prefix.length).includes("/") && d !== dir) out.push({ dir: true, name: d });
        for (const f of userFiles.keys()) if (f.startsWith(prefix) && !f.slice(prefix.length).includes("/")) out.push({ dir: false, name: f });
        out.sort((a, b) => (a.dir === b.dir ? a.name.localeCompare(b.name) : a.dir ? -1 : 1));
        return out;
      };
      return childrenOf;
    }
    function renderFiles() {
      filesBox.textContent = "";
      const childrenOf = treeOf();
      const emit = (dir, depth) => {
        for (const node of childrenOf(dir)) {
          const row = document.createElement("button");
          row.type = "button";
          row.className = "idefile" + (node.dir ? " dir" : "");
          row.dataset.path = node.name;
          row.style.setProperty("--depth", depth);
          row.dataset.ico = node.dir ? (collapsed.has(node.name) ? "folder" : "folder-open") : "file";
          row.append(document.createTextNode(labelOf(node.name)));
          if (!node.dir && focusedG.active === node.name) row.setAttribute("aria-current", "true");
          if (node.dir) {
            row.addEventListener("click", () => {
              if (collapsed.has(node.name)) collapsed.delete(node.name);
              else collapsed.add(node.name);
              renderFiles();
            });
            row.addEventListener("contextmenu", (e) => {
              e.preventDefault();
              openMenu(e.clientX, e.clientY, [
                { label: "New file", act: () => newEntryInline("file", node.name + "/") },
                { label: "New folder", act: () => newEntryInline("folder", node.name + "/") },
                "-",
                { label: "Rename", act: () => renameInline(row, node.name, true) },
                { label: "Delete", act: () => removeEntry(node.name, true) },
              ]);
            });
          } else {
            row.addEventListener("pointerdown", (e) => beginDrag(e, node.name, null));
            row.addEventListener("click", () => {
              if (!drag || !drag.moved) activateIn(focusedG === gB ? gB : gA, node.name);
            });
            row.addEventListener("contextmenu", (e) => {
              e.preventDefault();
              openMenu(e.clientX, e.clientY, [
                { label: "Open", act: () => activateIn(gA, node.name) },
                { label: "Open to the side", act: () => sendRight(node.name) },
                "-",
                { label: "Rename", keys: "Shift+F6", act: () => renameInline(row, node.name, false) },
                { label: "Delete", act: () => removeEntry(node.name, false) },
                "-",
                { label: "Move to new window", act: () => openElsewhere(node.name) },
              ]);
            });
          }
          filesBox.append(row);
          if (node.dir && !collapsed.has(node.name)) emit(node.name, depth + 1);
        }
      };
      emit("", 0);
      if (!filesBox.childElementCount) {
        const empty = document.createElement("p");
        empty.className = "cap dim";
        empty.textContent = "No files yet.";
        filesBox.append(empty);
      }
    }
    filesBox.addEventListener("contextmenu", (e) => {
      if (e.target !== filesBox && !e.target.classList.contains("cap")) return;
      e.preventDefault();
      openMenu(e.clientX, e.clientY, [
        { label: "New file", keys: "Alt+Ins", act: () => newEntryInline("file", "") },
        { label: "New folder", act: () => newEntryInline("folder", "") },
        "-",
        { label: "Upload files", act: () => uploadInput && uploadInput.click() },
        { label: "Download project (zip)", act: downloadZip },
      ]);
    });

    function removeEntry(name, isDir) {
      if (isDir) {
        for (const f of [...userFiles.keys()]) if (f.startsWith(name + "/")) removeEntry(f, false);
        for (const d of [...folders]) if (d === name || d.startsWith(name + "/")) folders.delete(d);
      } else {
        userFiles.delete(name);
        for (const g of groups()) if (g.tabs.includes(name)) closeIn(g, name);
      }
      persistFiles();
      renderFiles();
    }
    function renamePath(from, to, isDir) {
      if (isDir) {
        const rebuilt = new Map();
        for (const [k, v] of userFiles) rebuilt.set(k === from || k.startsWith(from + "/") ? to + k.slice(from.length) : k, v);
        userFiles = rebuilt;
        for (const d of [...folders]) if (d === from || d.startsWith(from + "/")) {
          folders.delete(d);
          folders.add(to + d.slice(from.length));
        }
        for (const g of groups()) {
          g.tabs = g.tabs.map((t) => (t.startsWith(from + "/") ? to + t.slice(from.length) : t));
          if (g.active && g.active.startsWith(from + "/")) g.active = to + g.active.slice(from.length);
        }
      } else {
        const text = userFiles.get(from);
        const rebuilt = new Map();
        for (const [k, v] of userFiles) rebuilt.set(k === from ? to : k, k === from ? text : v);
        userFiles = rebuilt;
        for (const g of groups()) {
          const i = g.tabs.indexOf(from);
          if (i >= 0) g.tabs[i] = to;
          if (g.active === from) g.active = to;
        }
      }
      persistFiles();
      for (const g of groups()) renderTabsG(g);
      syncChrome();
    }
    function inlineField(initial, selectTo, onDone) {
      const field = document.createElement("input");
      field.className = "newname";
      field.value = initial;
      let settled = false;
      const done = (commit) => {
        if (settled) return;
        settled = true;
        const v = field.value.trim();
        field.remove();
        onDone(commit ? v : null);
      };
      field.addEventListener("keydown", (e) => {
        if (e.key === "Enter") done(true);
        if (e.key === "Escape") done(false);
      });
      field.addEventListener("blur", () => done(true));
      queueMicrotask(() => {
        field.focus();
        field.setSelectionRange(0, selectTo);
      });
      return field;
    }
    function renameInline(row, name, isDir) {
      const base = labelOf(name);
      const dir = name.includes("/") ? name.slice(0, name.lastIndexOf("/") + 1) : "";
      const field = inlineField(base, isDir ? base.length : base.replace(/\.[^.]*$/, "").length, (v) => {
        if (v && v !== base && !v.includes("/")) {
          let to = dir + v;
          if (!isDir && !/\.[^.]+$/.test(to)) to += ".vyrn";
          if (!isDir) to = freshName(to);
          renamePath(name, to, isDir);
        } else {
          renderFiles();
        }
      });
      if (row && row.isConnected) row.replaceWith(field);
      else filesBox.append(field);
    }
    function newEntryInline(kind, prefix) {
      const field = inlineField(kind === "file" ? "untitled.vyrn" : "folder", kind === "file" ? "untitled".length : "folder".length, (v) => {
        if (!v || v.includes("/")) {
          renderFiles();
          return;
        }
        if (kind === "folder") {
          folders.add(prefix + v);
          persistFiles();
          renderFiles();
          return;
        }
        let name = prefix + v;
        if (!/\.[^.]+$/.test(name)) name += ".vyrn";
        name = freshName(name);
        userFiles.set(name, "");
        persistFiles();
        activateIn(focusedG === gB ? gB : gA, name);
      });
      if (prefix) collapsed.delete(prefix.slice(0, -1));
      filesBox.append(field);
    }
    for (const b of $$("[data-play-newfile]", root)) b.addEventListener("click", () => newEntryInline("file", ""));
    for (const b of $$("[data-play-newfolder]", root)) b.addEventListener("click", () => newEntryInline("folder", ""));
    for (const b of $$("[data-play-upload]", root)) b.addEventListener("click", () => uploadInput && uploadInput.click());
    for (const b of $$("[data-play-download]", root)) b.addEventListener("click", downloadZip);
    if (uploadInput) {
      uploadInput.addEventListener("change", async () => {
        let last = null;
        for (const f of uploadInput.files) {
          const name = freshName(f.name);
          userFiles.set(name, await f.text());
          last = name;
        }
        uploadInput.value = "";
        persistFiles();
        if (last) activateIn(focusedG === gB ? gB : gA, last);
        else renderFiles();
      });
    }

    // ---- the project as a zip: stored entries, CRC-32, written by hand ----
    const CRC = (() => {
      const t = new Uint32Array(256);
      for (let n = 0; n < 256; n++) {
        let c = n;
        for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
        t[n] = c >>> 0;
      }
      return t;
    })();
    function crc32(bytes) {
      let c = 0xffffffff;
      for (let i = 0; i < bytes.length; i++) c = CRC[(c ^ bytes[i]) & 0xff] ^ (c >>> 8);
      return (c ^ 0xffffffff) >>> 0;
    }
    function zipOf(entries) {
      const enc = new TextEncoder();
      const parts = [];
      const central = [];
      let offset = 0;
      const u16 = (n) => [n & 0xff, (n >>> 8) & 0xff];
      const u32 = (n) => [n & 0xff, (n >>> 8) & 0xff, (n >>> 16) & 0xff, (n >>> 24) & 0xff];
      for (const [name, text] of entries) {
        const nameB = enc.encode(name);
        const data = enc.encode(text);
        const crc = crc32(data);
        const head = new Uint8Array([
          0x50, 0x4b, 0x03, 0x04, ...u16(20), ...u16(0x0800), ...u16(0), ...u16(0), ...u16(0),
          ...u32(crc), ...u32(data.length), ...u32(data.length), ...u16(nameB.length), ...u16(0),
        ]);
        parts.push(head, nameB, data);
        central.push(new Uint8Array([
          0x50, 0x4b, 0x01, 0x02, ...u16(20), ...u16(20), ...u16(0x0800), ...u16(0), ...u16(0), ...u16(0),
          ...u32(crc), ...u32(data.length), ...u32(data.length), ...u16(nameB.length), ...u16(0), ...u16(0),
          ...u16(0), ...u16(0), ...u32(0), ...u32(offset),
        ]), nameB);
        offset += head.length + nameB.length + data.length;
      }
      const cdSize = central.reduce((n, p) => n + p.length, 0);
      const end = new Uint8Array([0x50, 0x4b, 0x05, 0x06, ...u16(0), ...u16(0), ...u16(entries.length), ...u16(entries.length), ...u32(cdSize), ...u32(offset), ...u16(0)]);
      return new Blob([...parts, ...central, end], { type: "application/zip" });
    }
    function downloadZip() {
      const entries = [...userFiles];
      if (!entries.length) return;
      const url = URL.createObjectURL(zipOf(entries));
      const a = document.createElement("a");
      a.href = url;
      a.download = "vyrn-project.zip";
      document.body.append(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 2000);
    }

    // ---- templates on the Welcome view -----------------------------------
    for (const b of $$("[data-play-tpl]", root)) {
      b.addEventListener("click", () => {
        const id = b.dataset.playTpl;
        const e = examples.get(id);
        if (!e) return;
        const name = freshName(id + ".vyrn");
        userFiles.set(name, e.src);
        persistFiles();
        if (stdin && e.stdin) stdin.value = e.stdin;
        activateIn(gA, name);
      });
    }

    // ---- the sashes: the panes resize, and the sizes persist ---------------
    function bindSash(el) {
      el.addEventListener("pointerdown", (e) => {
        e.preventDefault();
        const kind = el.dataset.playSash;
        const wr = wrapEl.getBoundingClientRect();
        const move = (ev) => {
          if (kind === "side") layout.side = Math.max(140, Math.min(480, ev.clientX - wr.left));
          else if (kind === "panel") layout.panel = Math.max(80, Math.min(wr.height - 200, wr.bottom - ev.clientY - 28));
          else if (kind === "split" && gB) {
            const a = gA.main.getBoundingClientRect();
            const b = gB.main.getBoundingClientRect();
            layout.split = Math.max(0.2, Math.min(0.8, (ev.clientX - a.left) / (b.right - a.left)));
          }
          applyLayout();
          for (const g of groups()) syncG(g);
        };
        const up = () => {
          removeEventListener("pointermove", move);
          removeEventListener("pointerup", up);
          wrapEl.classList.remove("resizing");
        };
        wrapEl.classList.add("resizing");
        addEventListener("pointermove", move);
        addEventListener("pointerup", up);
      });
    }
    for (const el of $$("[data-play-sash]", root)) bindSash(el);
    applyLayout();

    // ---- a link that carries the whole project ---------------------------
    // `#p=` is base64url of the project's own JSON: every file, and which
    // one was open. `#c=` (one program) still opens, because links written
    // before this exist and a link that stops working is a broken promise.
    shareHash = () => {
      const payload = { v: 1, files: [...userFiles], open: focusedG.active === WELCOME ? null : focusedG.active };
      return "p=" + encodeSource(JSON.stringify(payload));
    };
    function projectFromHash() {
      if (!location.hash.startsWith("#p=")) return null;
      try {
        const json = decodeSource(location.hash.slice(3));
        const payload = JSON.parse(json);
        return payload && Array.isArray(payload.files) ? payload : null;
      } catch (err) {
        return null;
      }
    }

    // ---- the explorer's drops: a file into a folder, an OS file in -------
    function moveInto(name, dir) {
      const base = labelOf(name);
      const to = freshName((dir ? dir + "/" : "") + base);
      if (to === name) return;
      renamePath(name, to, false);
      renderFiles();
    }
    let dropDir = null;
    async function takeFiles(list, dir) {
      let last = null;
      for (const f of list) {
        if (!f.name) continue;
        const name = freshName((dir ? dir + "/" : "") + f.name);
        userFiles.set(name, await f.text());
        last = name;
      }
      persistFiles();
      if (last) activateIn(focusedG === gB ? gB : gA, last);
      else renderFiles();
    }
    const sideEl = $(".ideside", root);
    if (sideEl) {
      sideEl.addEventListener("dragover", (e) => {
        if (!e.dataTransfer || ![...e.dataTransfer.types].includes("Files")) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = "copy";
        const row = e.target.closest ? e.target.closest(".idefile.dir") : null;
        for (const r of $$(".idefile.dir", root)) r.classList.toggle("dropinto", r === row);
        sideEl.classList.add("dropping");
      });
      sideEl.addEventListener("dragleave", (e) => {
        if (e.target === sideEl) sideEl.classList.remove("dropping");
      });
      sideEl.addEventListener("drop", (e) => {
        if (!e.dataTransfer || !e.dataTransfer.files.length) return;
        e.preventDefault();
        sideEl.classList.remove("dropping");
        const row = e.target.closest ? e.target.closest(".idefile.dir") : null;
        for (const r of $$(".idefile.dir", root)) r.classList.remove("dropinto");
        takeFiles(e.dataTransfer.files, row ? row.dataset.path : "");
      });
    }

    // A file opened before the compiler landed was drawn with no spans; the
    // core calls this the moment it lands, and every group is redrawn.
    onPlayReady = () => {
      for (const g of groups()) {
        paintG(g);
        syncG(g);
      }
      updateRunState();
    };

    // ---- the minimap scrolls the window it draws --------------------------
    function bindMinimap(g) {
      if (!g.minimap) return;
      const jump = (e) => {
        const rect = g.minimap.getBoundingClientRect();
        const mapH = Math.min(rect.height, g.mmtext ? g.mmtext.scrollHeight : rect.height);
        const frac = Math.max(0, Math.min(1, (e.clientY - rect.top) / mapH));
        g.input.scrollTop = frac * g.input.scrollHeight - g.input.clientHeight / 2;
        syncG(g);
      };
      g.minimap.addEventListener("pointerdown", (e) => {
        jump(e);
        e.preventDefault();
      });
      g.minimap.addEventListener("pointermove", (e) => {
        if (e.buttons & 1) jump(e);
      });
    }
    bindMinimap(gA);

    // ---- first light: the session comes back ------------------------------
    queueMicrotask(() => {
      const session = read(KEY_SESSION, null);
      const project = projectFromHash();
      if (project) {
        let open = null;
        for (const [name, text] of project.files) {
          const fresh = freshName(name);
          userFiles.set(fresh, text);
          if (name === project.open) open = fresh;
        }
        persistFiles();
        activateIn(gA, open || (project.files[0] && freshName(project.files[0][0])) || WELCOME);
        return;
      }
      if (location.hash.startsWith("#c=") && input.value) {
        const name = freshName("shared.vyrn");
        userFiles.set(name, input.value);
        persistFiles();
        activateIn(gA, name);
        return;
      }
      if (session && session.a) {
        const keep = (t) => t === WELCOME || userFiles.has(t);
        gA.tabs = (session.a.tabs || []).filter(keep);
        if (session.b && session.b.tabs && session.b.tabs.some(keep)) {
          ensureSplit();
          gB.tabs = session.b.tabs.filter(keep);
          const bActive = keep(session.b.active) && gB.tabs.includes(session.b.active) ? session.b.active : gB.tabs[0];
          if (bActive) activateIn(gB, bActive);
          else unsplit();
        }
        const aActive = keep(session.a.active) && gA.tabs.includes(session.a.active) ? session.a.active : gA.tabs[0];
        activateIn(gA, aActive || WELCOME);
        if (session.focused === "b" && gB) {
          focusedG = gB;
          gB.input.focus();
        }
        return;
      }
      if (userFiles.size) activateIn(gA, userFiles.keys().next().value);
      else activateIn(gA, WELCOME);
    });
  }

  if (shareBtn) {
    shareBtn.addEventListener("click", async () => {
      const url = location.origin + location.pathname + "#" + (shareHash ? shareHash() : "c=" + encodeSource(srcHost().value));
      history.replaceState(null, "", url);
      setLabel(shareBtn, (await writeClipboard(url)) ? "Copied" : "In the URL", "done");
      setTimeout(() => setLabel(shareBtn, "Copy link", "idle"), 1600);
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
      if (onPlayReady) onPlayReady();
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
