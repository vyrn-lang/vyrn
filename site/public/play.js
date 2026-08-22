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
  // Which editor group the compiler reads: the frame (a second, split group
  // exists on /play) points this at the focused one; alone, it is the one
  // textarea this mount owns.
  let srcHost = () => input;

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
  // The editor shell (RFC-0106 M5, rounds 4-11). Everything below exists only
  // on /play — every hook is queried and silently absent on the hero editors —
  // and everything is REAL: files persist in localStorage and reopen on the
  // next visit, the examples are templates a file is created FROM, tabs
  // reorder by drag and split into a second live editor group, context menus
  // do what their words say, and the status bar's numbers are measurements.
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

  // ---- the panel's own tabs (present with the shell) ----------------------
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
    // ---- files: named, persisted, yours -----------------------------------
    const STORE = "vyrn.play.files.v1";
    let userFiles = new Map();
    try {
      userFiles = new Map(JSON.parse(localStorage.getItem(STORE) || "[]").map((f) => [f.name, f.text]));
    } catch (err) {
      userFiles = new Map();
    }
    let saveTimer = 0;
    function persist() {
      clearTimeout(saveTimer);
      saveTimer = setTimeout(() => {
        try {
          localStorage.setItem(STORE, JSON.stringify([...userFiles].map(([name, text]) => ({ name, text }))));
        } catch (err) {
          /* a full or absent store loses persistence, never the session */
        }
      }, 150);
    }
    function freshName(base) {
      if (!userFiles.has(base)) return base;
      const stem = base.replace(/\.vyrn$/, "");
      for (let i = 2; ; i++) {
        const name = stem + "-" + i + ".vyrn";
        if (!userFiles.has(name)) return name;
      }
    }

    // ---- editor groups ----------------------------------------------------
    const WELCOME = "__welcome";
    function makeGroup(els) {
      return Object.assign({ tabs: [], active: null }, els);
    }
    const gA = makeGroup({
      strip: tabsBox,
      input,
      layer,
      gutter,
      curline,
      editorEl: editor,
    });
    let gB = null;
    let focusedG = gA;
    srcHost = () => focusedG.input;

    function labelOf(name) {
      return name === WELCOME ? "Welcome" : name;
    }
    function contentOf(name) {
      return userFiles.get(name) || "";
    }

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
      if (g === focusedG && lncol) {
        lncol.textContent = "Ln " + line + ", Col " + (before[before.length - 1].length + 1);
      }
      if (g === gA && mmtext) {
        if (mmtext.textContent !== g.input.value) mmtext.textContent = g.input.value;
        if (mmview) {
          const mapH = mmtext.scrollHeight;
          mmview.style.top = (g.input.scrollTop / g.input.scrollHeight) * mapH + "px";
          mmview.style.height = Math.max(12, (g.input.clientHeight / g.input.scrollHeight) * mapH) + "px";
        }
      }
    }

    function syncChrome() {
      if (crumbs) crumbs.textContent = focusedG.active ? "files › " + labelOf(focusedG.active) : "files";
      renderFiles();
    }

    function welcomeShown() {
      return gA.active === WELCOME;
    }
    function applyWelcome() {
      const on = welcomeShown();
      if (welcomeEl) welcomeEl.hidden = !on;
      gA.editorEl.hidden = on;
    }
    // Running the Welcome view would run nothing: the press opens no worker.
    runBtn.addEventListener(
      "click",
      (e) => {
        if (focusedG === gA && welcomeShown()) e.stopImmediatePropagation();
      },
      { capture: true },
    );

    function activateIn(g, name) {
      if (g === gB && name === WELCOME) return;
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
      renderTabsG(gA);
      if (gB) renderTabsG(gB);
      syncG(g);
      syncChrome();
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
      }
      if (g === gB && gB && !gB.tabs.length) unsplit();
    }

    // ---- the second group: a real editor, built when a tab is sent right --
    function ensureSplit() {
      if (gB) return gB;
      const wrap = document.createElement("div");
      wrap.className = "idemain second";
      wrap.innerHTML =
        '<div class="idebar"><span class="idetabs"></span><span class="spacer"></span></div>' +
        '<div class="editor"><div class="curline" aria-hidden="true"></div><div class="gutter" aria-hidden="true"></div>' +
        '<pre class="code hl" aria-hidden="true"><code></code></pre>' +
        '<textarea spellcheck="false" autocapitalize="off" autocorrect="off" autocomplete="off" wrap="off" aria-label="Vyrn source, second editor group"></textarea></div>';
      $(".idemain", root).after(wrap);
      wrapEl.classList.add("split");
      gB = makeGroup({
        strip: $(".idetabs", wrap),
        input: $("textarea", wrap),
        layer: $("code", wrap),
        gutter: $(".gutter", wrap),
        curline: $(".curline", wrap),
        editorEl: $(".editor", wrap),
        wrap,
      });
      bindGroup(gB);
      return gB;
    }
    function unsplit() {
      if (!gB) return;
      gB.wrap.remove();
      wrapEl.classList.remove("split");
      gB = null;
      focusedG = gA;
      if (!gA.active) activateIn(gA, WELCOME);
      syncChrome();
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
          persist();
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
      // Enter keeps the line's indentation, one level deeper after `{`; a `}`
      // under the caret gets its own dedented line. Tab is two spaces (the
      // core binds Tab on group A; the second group gets it here).
      g.input.addEventListener("keydown", (e) => {
        if (e.key === "Tab" && g !== gA) {
          e.preventDefault();
          g.input.setRangeText("  ", g.input.selectionStart, g.input.selectionEnd, "end");
          g.input.dispatchEvent(new Event("input", { bubbles: true }));
          return;
        }
        if (e.key !== "Enter" || e.isComposing) return;
        e.preventDefault();
        const at = g.input.selectionStart;
        const before = g.input.value.slice(0, at);
        const line = before.slice(before.lastIndexOf("\n") + 1);
        const indent = (line.match(/^ */) || [""])[0];
        const opened = /\{\s*$/.test(line);
        const closingNext = g.input.value.slice(g.input.selectionEnd).startsWith("}");
        let insert = "\n" + indent + (opened ? "  " : "");
        if (opened && closingNext) insert += "\n" + indent;
        const caret = at + 1 + indent.length + (opened ? 2 : 0);
        g.input.setRangeText(insert, at, g.input.selectionEnd, "end");
        g.input.selectionStart = g.input.selectionEnd = caret;
        g.input.dispatchEvent(new Event("input", { bubbles: true }));
      });
    }
    bindGroup(gA);
    // Group A's core paint runs on its own input listener; recheck rides it.

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

    // ---- tabs: rendered, reorderable, sendable ----------------------------
    let drag = null;
    function renderTabsG(g) {
      g.strip.textContent = "";
      for (const name of g.tabs) {
        const box = document.createElement("span");
        box.className = "itab" + (name === g.active ? " on" : "");
        box.dataset.name = name;
        const btn = document.createElement("button");
        btn.type = "button";
        btn.className = "tname";
        btn.textContent = labelOf(name);
        btn.addEventListener("pointerdown", (e) => beginDrag(e, g, name));
        btn.addEventListener("click", () => {
          if (!drag || !drag.moved) activateIn(g, name);
        });
        btn.addEventListener("contextmenu", (e) => {
          e.preventDefault();
          const items = [
            { label: "Close", act: () => closeIn(g, name) },
            { label: "Close others", act: () => { g.tabs = [name]; activateIn(g, name); } },
          ];
          if (name !== WELCOME) {
            items.push("-");
            if (g === gA) items.push({ label: "Split right", act: () => sendRight(name) });
            else items.push({ label: "Move left", act: () => { closeIn(gB, name); activateIn(gA, name); } });
            items.push({ label: "Move to new window", act: () => window.open(location.pathname + "#c=" + encodeSource(contentOf(name)), "_blank") });
          }
          openMenu(e.clientX, e.clientY, items);
        });
        const close = document.createElement("button");
        close.type = "button";
        close.className = "tclose";
        close.setAttribute("aria-label", "Close " + labelOf(name));
        close.textContent = "\u00d7";
        close.addEventListener("click", (e) => {
          e.stopPropagation();
          closeIn(g, name);
        });
        box.append(btn, close);
        g.strip.append(box);
      }
    }
    function sendRight(name) {
      const from = gA.tabs.includes(name) ? gA : gB;
      ensureSplit();
      if (from === gA) {
        const at = gA.tabs.indexOf(name);
        gA.tabs.splice(at, 1);
        if (gA.active === name) gA.active = gA.tabs[at] || gA.tabs[at - 1] || null;
        if (!gA.active) {
          gA.active = WELCOME;
          gA.tabs.push(WELCOME);
        }
        activateIn(gA, gA.active);
      }
      activateIn(gB, name);
    }
    function beginDrag(e, g, name) {
      if (e.button !== 0) return;
      const sx = e.clientX;
      const sy = e.clientY;
      drag = { g, name, moved: false, ghost: null };
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
        // Reorder live inside whichever strip the pointer is over.
        for (const grp of gB ? [gA, gB] : [gA]) {
          const r = grp.strip.getBoundingClientRect();
          if (ev.clientY < r.top - 6 || ev.clientY > r.bottom + 14 || ev.clientX < r.left || ev.clientX > r.right + 60) continue;
          const from = drag.g.tabs.indexOf(name);
          if (from < 0) continue;
          let to = grp.tabs.length;
          for (let i = 0; i < grp.strip.children.length; i++) {
            const cr = grp.strip.children[i].getBoundingClientRect();
            if (ev.clientX < cr.left + cr.width / 2) {
              to = i;
              break;
            }
          }
          if (grp === drag.g) {
            if (to === from || to === from + 1) break;
            drag.g.tabs.splice(from, 1);
            drag.g.tabs.splice(to > from ? to - 1 : to, 0, name);
            renderTabsG(drag.g);
          } else if (name !== WELCOME) {
            drag.g.tabs.splice(from, 1);
            if (drag.g.active === name) drag.g.active = drag.g.tabs[0] || null;
            grp.tabs.splice(to, 0, name);
            const was = drag.g;
            drag.g = grp;
            if (was === gA && !gA.active) {
              gA.active = WELCOME;
              gA.tabs.push(WELCOME);
              applyWelcome();
            }
            renderTabsG(gA);
            if (gB) renderTabsG(gB);
          }
          break;
        }
        // The right third of the last group's editor, with no split yet: the
        // drop that CREATES the split, hinted while the pointer is there.
        const er = gA.editorEl.getBoundingClientRect();
        const inSplitZone = !gB && name !== WELCOME && ev.clientX > er.left + er.width * 0.66 && ev.clientY > er.top && ev.clientY < er.bottom;
        wrapEl.classList.toggle("splitzone", inSplitZone);
        drag.split = inSplitZone;
      };
      const up = () => {
        removeEventListener("pointermove", move);
        removeEventListener("pointerup", up);
        wrapEl.classList.remove("dragging", "splitzone");
        if (drag && drag.ghost) drag.ghost.remove();
        if (drag && drag.moved) {
          if (drag.split) sendRight(name);
          else activateIn(drag.g, name);
        }
        setTimeout(() => (drag = null));
      };
      addEventListener("pointermove", move);
      addEventListener("pointerup", up);
    }

    // ---- the explorer: your files, with the editor's own menus ------------
    function renderFiles() {
      filesBox.textContent = "";
      for (const name of userFiles.keys()) {
        const row = document.createElement("button");
        row.type = "button";
        row.className = "idefile";
        row.textContent = name;
        if (focusedG.active === name) row.setAttribute("aria-current", "true");
        row.addEventListener("click", () => activateIn(focusedG === gB ? gB : gA, name));
        row.addEventListener("contextmenu", (e) => {
          e.preventDefault();
          openMenu(e.clientX, e.clientY, [
            { label: "Open", act: () => activateIn(gA, name) },
            { label: "Open to the side", act: () => sendRight(name) },
            "-",
            { label: "Rename", act: () => renameInline(row, name) },
            { label: "Delete", act: () => removeFile(name) },
            "-",
            { label: "Move to new window", act: () => window.open(location.pathname + "#c=" + encodeSource(contentOf(name)), "_blank") },
          ]);
        });
        filesBox.append(row);
      }
    }
    function removeFile(name) {
      userFiles.delete(name);
      persist();
      for (const g of gB ? [gA, gB] : [gA]) if (g.tabs.includes(name)) closeIn(g, name);
      renderFiles();
    }
    function renameInline(row, name) {
      const field = document.createElement("input");
      field.className = "newname";
      field.value = name;
      row.replaceWith(field);
      field.focus();
      field.setSelectionRange(0, name.replace(/\.vyrn$/, "").length);
      const done = (commit) => {
        let next = field.value.trim();
        if (commit && next && next !== name) {
          if (!next.endsWith(".vyrn")) next += ".vyrn";
          next = freshName(next);
          const text = userFiles.get(name);
          const rebuilt = new Map();
          for (const [k, v] of userFiles) rebuilt.set(k === name ? next : k, k === name ? text : v);
          userFiles = rebuilt;
          persist();
          for (const g of gB ? [gA, gB] : [gA]) {
            const i = g.tabs.indexOf(name);
            if (i >= 0) g.tabs[i] = next;
            if (g.active === name) g.active = next;
          }
          renderTabsG(gA);
          if (gB) renderTabsG(gB);
        }
        syncChrome();
      };
      field.addEventListener("keydown", (e) => {
        if (e.key === "Enter") done(true);
        if (e.key === "Escape") done(false);
      });
      field.addEventListener("blur", () => done(true));
    }
    function newFileInline() {
      const field = document.createElement("input");
      field.className = "newname";
      field.value = "untitled.vyrn";
      filesBox.append(field);
      field.focus();
      field.setSelectionRange(0, "untitled".length);
      let settled = false;
      const done = (commit) => {
        if (settled) return;
        settled = true;
        let name = field.value.trim();
        field.remove();
        if (!commit || !name) {
          renderFiles();
          return;
        }
        if (!name.endsWith(".vyrn")) name += ".vyrn";
        name = freshName(name);
        userFiles.set(name, "");
        persist();
        activateIn(focusedG === gB ? gB : gA, name);
      };
      field.addEventListener("keydown", (e) => {
        if (e.key === "Enter") done(true);
        if (e.key === "Escape") done(false);
      });
      field.addEventListener("blur", () => done(true));
    }
    for (const b of $$("[data-play-newfile]", root)) b.addEventListener("click", newFileInline);

    // ---- the Welcome view's templates: a file is created FROM one ---------
    for (const b of $$("[data-play-tpl]", root)) {
      b.addEventListener("click", () => {
        const id = b.dataset.playTpl;
        const e = examples.get(id);
        if (!e) return;
        const name = freshName(id + ".vyrn");
        userFiles.set(name, e.src);
        persist();
        if (stdin && e.stdin) stdin.value = e.stdin;
        activateIn(gA, name);
      });
    }

    // ---- first light ------------------------------------------------------
    // The core's own tail decides what a `#c=` link puts in the textarea; the
    // shell reads its decision one microtask later.
    queueMicrotask(() => {
      if (location.hash.startsWith("#c=") && input.value) {
        const name = freshName("shared.vyrn");
        userFiles.set(name, input.value);
        persist();
        activateIn(gA, name);
      } else if (userFiles.size) {
        activateIn(gA, userFiles.keys().next().value);
      } else {
        activateIn(gA, WELCOME);
      }
    });

    // ---- the minimap scrolls the window it draws --------------------------
    if (minimap) {
      const jump = (e) => {
        const rect = minimap.getBoundingClientRect();
        const mapH = Math.min(rect.height, mmtext ? mmtext.scrollHeight : rect.height);
        const frac = Math.max(0, Math.min(1, (e.clientY - rect.top) / mapH));
        gA.input.scrollTop = frac * gA.input.scrollHeight - gA.input.clientHeight / 2;
        syncG(gA);
      };
      minimap.addEventListener("pointerdown", (e) => {
        jump(e);
        e.preventDefault();
      });
      minimap.addEventListener("pointermove", (e) => {
        if (e.buttons & 1) jump(e);
      });
    }
  }

  if (shareBtn) {
    shareBtn.addEventListener("click", async () => {
      const url = location.origin + location.pathname + "#c=" + encodeSource(srcHost().value);
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
