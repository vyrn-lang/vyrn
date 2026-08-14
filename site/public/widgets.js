// The widget harness, and every widget's logic.
//
// The rule the whole site is built on: **Vyrn computes the geometry at build
// time, JavaScript animates one scalar.** Every bar position, every cell fill,
// every label below is already in the markup when this file loads. Nothing here
// computes a chart; it fades, sweeps, counts, or moves a playhead.
//
// No framework, no bundler, no dependency. Three files: this one, hero.js, and
// fresh.js.
import { mountHero } from "./hero.js";
import { refreshRelease } from "./fresh.js";
import { mountPlay } from "./play.js";
import { runVyrn } from "./wasi-min.js";

// Where the site is. Every file this page loads is a sibling of this script, and
// this script knows its own URL, so that is the whole derivation — no flag baked
// in at build time, no root assumed. It is correct at a domain root, under
// `/vyrn/` on GitHub Pages, behind a preview URL, and on `file://`. The imports
// above are relative for the same reason: a module specifier resolves against
// the module that wrote it.
const SITE = new URL(".", import.meta.url);

// Soft navigation across a static host: the payload for /philosophy.html lives
// beside it in /philosophy.data.json, because a file host cannot vary on
// `Accept`. Set before the runtime module loads.
window.__vyrnNavConfig = { staticData: true };

const REDUCED = matchMedia("(prefers-reduced-motion: reduce)").matches;

// ---------------------------------------------------------------------------
// The harness (design brief 4.1)
// ---------------------------------------------------------------------------

/// Fire `cb` once, the first time `root` is properly on screen, then disconnect.
/// Under reduced motion it fires immediately with `false`, and every widget
/// reads that as "render the end state, do not animate".
function onView(root, cb) {
  if (REDUCED) {
    cb(false);
    return;
  }
  const io = new IntersectionObserver(
    (entries) => {
      for (const e of entries) {
        // Half of it visible, OR half a screen of it visible — so a widget
        // taller than the viewport still fires.
        if (e.intersectionRatio >= 0.5 || e.intersectionRect.height >= innerHeight / 2) {
          io.disconnect();
          cb(true);
          return;
        }
      }
    },
    { threshold: [0.1, 0.3, 0.5, 0.75] }
  );
  io.observe(root);
}

const ease = (t) => 1 - Math.pow(1 - t, 3);

/// Run `render(p)` from 0 to 1 over `ms`. `render` must be a pure function of
/// `p`, so a replay and a scrubber are the same code.
function play(ms, render) {
  const start = performance.now();
  function step(now) {
    const p = Math.min(1, (now - start) / ms);
    render(ease(p));
    if (p < 1) requestAnimationFrame(step);
  }
  requestAnimationFrame(step);
}

/// Count `el` up to `to` over `ms`, keeping the digits from jittering.
function countUp(el, to, ms, suffix = "") {
  if (REDUCED) {
    el.textContent = to + suffix;
    return;
  }
  play(ms, (p) => (el.textContent = Math.round(to * p) + suffix));
}

/// Pin a button's width before its label changes, so nothing reflows.
function pinWidth(btn) {
  btn.style.minWidth = Math.ceil(btn.getBoundingClientRect().width) + "px";
}

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));

// ---------------------------------------------------------------------------
// W3 — the install command. The most important widget on the site.
// ---------------------------------------------------------------------------

/// Put `text` on the clipboard, and say whether it worked.
///
/// `navigator.clipboard` is missing on an insecure origin and can be refused by
/// permission, so failure is a real answer here rather than an exception nobody
/// catches. Both callers — the COPY button and an inline span — show what
/// happened instead of pretending.
async function writeClipboard(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch (err) {
    return false;
  }
}

function copyButtons() {
  for (const box of $$("[data-copy]")) {
    const btn = $("[data-copy-btn]", box);
    const code = $("code", box);
    if (!btn || !code) continue;
    btn.addEventListener("click", async () => {
      pinWidth(btn);
      if (await writeClipboard(code.textContent.trim())) {
        btn.textContent = "Copied";
      } else {
        btn.textContent = "Press Ctrl+C";
        getSelection().selectAllChildren(code);
      }
      setTimeout(() => (btn.textContent = "Copy"), 1600);
    });
  }
}

// ---------------------------------------------------------------------------
// Inline code copies itself.
//
// `Run it yourself: cargo test -p vyrn-cli --test parity -- --ignored` is
// written INLINE on purpose — it belongs in the sentence, not in a plate — and a
// reader who wants that command should not have to drag across it. So every
// code span in prose copies itself when clicked, and the same for a short
// reference like `$PATH` or `~/.vyrn`.
//
// The one real cost is text selection, and the rules that protect it are
// `isPlainCopyClick` below.
// ---------------------------------------------------------------------------

/// The code span `node` is inside, if that span is one in a SENTENCE.
///
/// A `<pre>`, a command plate and a numbered source row are blocks: the plate
/// already has its own COPY button, and a second handler inside one would fire
/// twice on a single click. One predicate, used by both the marking pass and the
/// delegated listener, so the two can never disagree about what is in scope.
function inlineCode(node) {
  const el = node && node.closest ? node.closest("code") : null;
  if (!el || el.closest("pre, .cmd, .lines")) return null;
  return el;
}

/// What an inline code span puts on the clipboard: what the reader sees.
///
/// Markup indentation reaches `textContent` as newlines and runs of spaces, and
/// an inline box renders every one of those as a single space. The copy does the
/// same, and trims the ends.
function codeText(el) {
  return el.textContent.replace(/\s+/g, " ").trim();
}

/// Whether a click on a code span is a COPY or part of a SELECTION.
///
/// Copy-on-click eats text selection unless it is careful, so a click has to
/// pass all three rules:
///
///  - the pointer did not travel — a drag is a selection, not a click,
///  - it is not the second click of a double-click, which selects a word,
///  - nothing is selected when the button comes up.
///
/// A keyboard-driven activation arrives with no pointer position at all, and
/// passes. `down` is null then, and null is the answer to "where did the pointer
/// press", not a missing value to guess at.
function isPlainCopyClick(down, up, selected) {
  if (up.detail >= 2) return false;
  if (selected) return false;
  if (!down) return true;
  return Math.abs(up.x - down.x) <= 3 && Math.abs(up.y - down.y) <= 3;
}

/// The polite live region every copy reports into. One per document, and a soft
/// navigation replaces `<main>` rather than `<body>`, so it survives.
function announce(msg) {
  let region = document.getElementById("copy-status");
  if (!region) {
    region = document.createElement("div");
    region.id = "copy-status";
    region.className = "sr-only";
    region.setAttribute("aria-live", "polite");
    document.body.appendChild(region);
  }
  region.textContent = msg;
}

/// Copy one span, and say so twice: a colour flash for the eye and the live
/// region for a screen reader.
///
/// The flash is a class, not a label. A label would change the span's box and
/// reflow the sentence around it, and a tooltip above a table cell is clipped by
/// the `.scroller` it sits in. Colour cannot do either.
async function copySpan(el) {
  const text = codeText(el);
  const ok = await writeClipboard(text);
  el.classList.add(ok ? "copied" : "copyfail");
  setTimeout(() => el.classList.remove("copied", "copyfail"), 900);
  if (ok) {
    announce("Copied " + text);
  } else {
    // The clipboard refused. Select the text so the reader can take it, and say
    // that is what happened.
    getSelection().selectAllChildren(el);
    announce("Copy failed. " + text + " is selected — press Control C.");
  }
}

/// Give every inline code span the affordance, once the script that backs it is
/// running. Without JavaScript the spans stay plain text and advertise nothing,
/// which is the honest state.
///
/// WHAT THIS DELIBERATELY DOES NOT DO IS PUT THEM IN THE TAB ORDER. Each span
/// used to take `tabIndex = 0` and `role="button"`, and `std/http` — the longest
/// reference page — turned into 143 copy buttons out of 222 tab stops, every one
/// of them between a keyboard reader and the next paragraph. The copy is a
/// POINTER convenience over text that is already selectable: a keyboard reader
/// selects the span and presses Ctrl+C, which needs no widget, and every command
/// worth copying whole sits in a `.cmd` plate that has its own Copy button in
/// the tab order. An affordance that costs 143 stops to save one drag is not
/// worth what it charges.
function markCopyable() {
  for (const el of $$("code")) {
    if (!inlineCode(el)) continue;
    el.classList.add("copyable");
  }
}

// Where the pointer went down, when it went down on a code span. Read once, by
// the click that follows.
let copyDownAt = null;

// One listener per document for the life of the page. `boot` runs again after
// every soft navigation and re-marks the spans; registering here would stack a
// listener per page.
document.addEventListener("pointerdown", (e) => {
  copyDownAt = inlineCode(e.target) ? { x: e.clientX, y: e.clientY } : null;
});

document.addEventListener("click", (e) => {
  const el = inlineCode(e.target);
  if (!el) return;
  const sel = getSelection();
  const selected = !!sel && !sel.isCollapsed && sel.toString().length > 0;
  if (isPlainCopyClick(copyDownAt, e, selected)) copySpan(el);
});


/// Pick the platform tab that matches the visitor, and let them override it.
///
/// The picker IS the tab widget below — same buttons, same panes, same keyboard
/// contract — with one difference: which tab starts selected is a guess about
/// the reader rather than the first one.
function installPicker(root) {
  const guess = /Win/i.test(navigator.platform)
    ? "windows"
    : /Mac/i.test(navigator.platform)
      ? "macos"
      : "linux";
  tabsWidget(root, { tab: "plat", pane: "plat-pane", initial: guess });
}

// ---------------------------------------------------------------------------
// W2 — the parity check. It RUNS.
//
// This widget used to be a 900 ms timer that revealed a digest written into the
// markup. On the one page whose argument is "believe the measurement", that was
// the picture of a green tick the design brief said not to build.
//
// What happens now:
//
//   - The INTERP column is a digest the site's export computed while this page
//     was built. The interpreter ran `examples/herofield.vyrn`'s
//     `parityReport()` and hashed what it returned. It is in the markup because
//     it was measured, not because it was typed.
//   - The WASM column is empty until it runs. Then `/hero.wasm` — the same
//     module compiled from the same file — is fetched, instantiated by
//     `wasi-min.js`, and its `main` runs IN THIS BROWSER. The number that
//     appears is the FNV-1a-64 of the bytes that module wrote to stdout,
//     computed here, from that run.
//   - The verdict compares the two, byte for byte.
//
// The NATIVE column cannot run in a page and does not pretend to: it says so,
// and names the harness that does compare it.
//
// No digest is written into this file, and nothing is on a timer.
// ---------------------------------------------------------------------------

/// FNV-1a-64 of a string's UTF-8 bytes, as sixteen lowercase hex digits.
///
/// This is `std/hash`'s `fnv1a` in the language the browser has — same offset
/// basis, same prime, same wrapping 64-bit multiply, which is why a digest this
/// computes can be compared with one the export computed. `BigInt` because a
/// `Number` loses the low bits of a 64-bit product.
function fnv1aHex(text) {
  const PRIME = 1099511628211n;
  const MASK = 0xffffffffffffffffn;
  let h = 14695981039346656037n;
  for (const b of new TextEncoder().encode(text)) {
    h = ((h ^ BigInt(b)) * PRIME) & MASK;
  }
  return h.toString(16).padStart(16, "0");
}

/// How many of the eight digest bytes differ. Two hex digits are one byte.
function digestBytesDiffer(a, b) {
  if (a.length !== b.length) return 8;
  let n = 0;
  for (let i = 0; i < a.length; i += 2) {
    if (a.slice(i, i + 2) !== b.slice(i, i + 2)) n += 1;
  }
  return n;
}

/// Run `examples/herofield.vyrn` as wasm, here, and hash what it printed.
///
/// Returns the digest, or null if the module could not be fetched or would not
/// instantiate — a page served without `hero.wasm` beside it says that, rather
/// than showing a number it did not compute.
async function wasmDigest() {
  try {
    const res = await fetch(new URL("hero.wasm", SITE));
    if (!res.ok) return null;
    const bytes = new Uint8Array(await res.arrayBuffer());
    const run = await runVyrn(bytes, { onStdout: () => {}, onStderr: () => {} });
    if (run.exitCode !== 0 || !run.stdout) return null;
    return fnv1aHex(run.stdout);
  } catch (err) {
    return null;
  }
}

function parityWidget(root) {
  const cols = $("[data-parity-cols]", root);
  const verdict = $("[data-parity-verdict]", root);
  const num = $("[data-parity-num]", root);
  const runBtn = $("[data-parity-run]", root);
  const breakBtn = $("[data-parity-break]", root);
  const interpPane = $('[data-col="interp"]', root);
  const wasmPane = $('[data-col="wasm"]', root);
  if (!interpPane || !wasmPane || !runBtn) return;

  // What the build measured. The only number this file trusts, and it came off
  // the page rather than out of a constant here.
  const built = interpPane.textContent.trim();
  // The same digest with one byte flipped. "Break one byte" corrupts the number
  // the run is compared AGAINST — the run itself is real either way, which is
  // the honest way to show that the check is a byte comparison.
  const flipped = built.slice(0, -1) + (built.endsWith("3") ? "7" : "3");
  let breaking = false;
  let measured = null;
  let running = false;

  const expected = () => (breaking ? flipped : built);

  /// Everything the widget shows, as a function of what has actually happened.
  const settle = () => {
    interpPane.textContent = expected();
    interpPane.classList.toggle("bad", breaking);
    wasmPane.textContent = measured || "—";
    const differ = measured ? digestBytesDiffer(measured, expected()) : -1;
    cols.classList.toggle("broken", differ > 0);
    verdict.classList.toggle("bad", differ > 0);
    if (differ < 0) {
      verdict.textContent = running
        ? "running examples/herofield.vyrn here…"
        : "nothing has run yet — press Run it here";
      num.firstChild.textContent = "— ";
      num.querySelector("small").textContent = "bytes differ";
      return;
    }
    verdict.textContent =
      differ === 0
        ? "0 bytes differ — this browser and the build agree"
        : differ + (differ === 1 ? " byte differs" : " bytes differ") + " — the harness fails";
    num.firstChild.textContent = differ + " ";
    num.querySelector("small").textContent = differ === 1 ? "byte differs" : "bytes differ";
  };

  /// Fetch, instantiate, run, hash. The wait is the module doing the work.
  async function run() {
    if (running) return;
    running = true;
    runBtn.disabled = true;
    pinWidth(runBtn);
    const label = runBtn.textContent;
    runBtn.textContent = "Running";
    settle();
    measured = await wasmDigest();
    running = false;
    runBtn.disabled = false;
    runBtn.textContent = label;
    if (!measured) {
      verdict.classList.add("bad");
      verdict.textContent = "hero.wasm did not load — nothing ran, so there is nothing to compare";
      return;
    }
    settle();
  }

  settle();
  runBtn.addEventListener("click", run);
  if (breakBtn) {
    breakBtn.addEventListener("click", () => {
      breaking = !breaking;
      breakBtn.setAttribute("aria-pressed", String(breaking));
      breakBtn.textContent = breaking ? "Put the byte back" : "Break one byte";
      settle();
    });
  }
  // Run it when the reader reaches it, so the number is theirs before they ask.
  // `onView` fires immediately under reduced motion, which is right here: this
  // is a computation, not an animation, and a reader who asked for less motion
  // did not ask for less measurement.
  onView(root, () => run());
}

// ---------------------------------------------------------------------------
// W6 — the ownership replay. Shape A: one scalar, one pure render.
//
// The playhead is a SOURCE LINE, and every part of the widget is that one
// number: the readout prints it, the code row carries it, and the report row
// that names it lights up with them. There is nothing left for the three to
// disagree about, because there is only one value.
// ---------------------------------------------------------------------------

function ownershipWidget(root) {
  const rows = $$("[data-own-rows] .cl", root);
  const report = $$("[data-own-report] > div", root);
  const label = $("[data-own-n]", root);
  if (!rows.length || !label) return;
  const lineOf = (el) => Number(el.dataset.l);

  /// Everything the widget shows, as a function of p alone.
  const render = (p) => {
    const i = Math.min(rows.length - 1, Math.floor(p * rows.length));
    const line = lineOf(rows[i]);
    label.textContent = String(line);
    rows.forEach((r, k) => {
      r.classList.toggle("on", k === i);
      r.classList.toggle("ahead", k > i);
    });
    for (const row of report) {
      row.classList.toggle("on", Number(row.dataset.from) <= line && line <= Number(row.dataset.to));
    }
  };

  const replay = () => (REDUCED ? render(1) : play(3200, render));
  render(0);
  const btn = $("[data-own-play]", root);
  if (btn) btn.addEventListener("click", replay);
  onView(root, (animate) => (animate ? replay() : render(1)));
}

// ---------------------------------------------------------------------------
// W1 supporting act — the validated-types specimen.
//
// The control has to be TRUE: it says it uncomments a line, so it uncomments
// the line. The export ships both renderings of that one row — the commented
// markup in place, the uncommented markup on the row's own `data-alt` — so the
// swap is a string assignment and the compiler's lexer coloured both halves.
// The diagnostic then moves under the row it is about, rather than to the foot
// of the plate where it belonged to nothing.
// ---------------------------------------------------------------------------

function typesWidget(root) {
  const btn = $("[data-types-btn]", root);
  const err = $("[data-types-error]", root);
  const row = $$("[data-types-code] .cl", root).find((r) => r.dataset.alt);
  if (!btn || !err || !row) return;
  const code = $("code", row);
  const commented = code.innerHTML;
  const uncommented = row.dataset.alt;
  // The export renders the diagnostic visible and last, so a reader with no
  // JavaScript still sees the compiler's answer. The script anchors it to the
  // line and hides it until that line exists.
  row.after(err);
  err.hidden = true;
  let on = false;
  btn.addEventListener("click", () => {
    pinWidth(btn);
    on = !on;
    code.innerHTML = on ? uncommented : commented;
    err.hidden = !on;
    btn.textContent = on ? "Comment it out again" : "Uncomment the bad line";
    btn.setAttribute("aria-pressed", String(on));
    btn.classList.toggle("on", on);
    if (REDUCED) return;
    row.classList.remove("changed");
    // Restart the flash even when the two clicks are close together.
    void row.offsetWidth;
    row.classList.add("changed");
    setTimeout(() => row.classList.remove("changed"), 900);
  });
}

// ---------------------------------------------------------------------------
// W8 — the leak shape. Two measured series, one keyword between them.
// ---------------------------------------------------------------------------

function leakWidget(root) {
  const btn = $("[data-leak-toggle]", root);
  const svg = $("[data-leak-svg]", root);
  const peak = $("[data-leak-peak]", root);
  const word = $("[data-leak-word]", root);
  if (!btn || !svg || !peak || !word) return;
  const peaks = [peak.firstChild.textContent, peak.dataset.alt];
  const words = [word.textContent, word.dataset.alt];
  let on = false;
  btn.addEventListener("click", () => {
    pinWidth(btn);
    on = !on;
    svg.classList.toggle("declared", on);
    peak.firstChild.textContent = peaks[on ? 1 : 0];
    word.textContent = words[on ? 1 : 0];
    btn.setAttribute("aria-pressed", String(on));
    btn.textContent = on ? "Take the keyword back" : "Declare consume";
    growBars(root);
  });
}

// ---------------------------------------------------------------------------
// W4 — the bar charts. Vyrn set every width; this only grows them.
// ---------------------------------------------------------------------------

/// A value label as a number and whatever follows it: "37.9 KB" -> 37.9, " KB",
/// one decimal. The export wrote the final string, so the count-up cannot
/// disagree with it — at p = 1 it prints the same characters back.
function counter(el) {
  const m = /^\s*([\d.]+)(.*)$/.exec(el.dataset.to || el.textContent);
  if (!m) return null;
  const dot = m[1].indexOf(".");
  return { el, to: Number(m[1]), places: dot < 0 ? 0 : m[1].length - dot - 1, rest: m[2] };
}

/// Grow every bar in `root` from nothing to the width the export gave it, and
/// count its value label up beside it.
///
/// The bar's `x` never changes and the label is anchored at the sheet's right
/// edge, so no frame of this moves anything: only a width and a string of
/// digits change, and both are in their own column.
function growBars(root) {
  const bars = $$("rect.bar", root).map((b) => {
    if (!b.dataset.w) b.dataset.w = b.getAttribute("width");
    return b;
  });
  const vals = $$("text.val", root).map(counter).filter(Boolean);
  const render = (p) => {
    for (const b of bars) b.setAttribute("width", String(Number(b.dataset.w) * p));
    for (const v of vals) v.el.textContent = (v.to * p).toFixed(v.places) + v.rest;
  };
  if (REDUCED) return render(1);
  render(0);
  play(900, render);
}

function barsWidget(root) {
  onView(root, (animate) => animate && growBars(root));
  const btn = $("[data-bars-replay]", root);
  if (btn) btn.addEventListener("click", () => growBars(root));
}

// ---------------------------------------------------------------------------
// W7 — the RFC strip. One sweep left to right, and a counter that follows it.
// No requestAnimationFrame at all: one transition per cell, staggered.
// ---------------------------------------------------------------------------

function stripWidget(root) {
  const cells = $$("rect.cell", root);
  const counter = $("[data-strip-count]", root);
  onView(root, (animate) => {
    if (!animate) return; // the markup already holds every cell at its tone
    for (const c of cells) c.setAttribute("fill-opacity", "0");
    cells.forEach((c, i) => {
      setTimeout(() => c.setAttribute("fill-opacity", c.dataset.tone), (i / cells.length) * 2400);
    });
    if (counter) countUp(counter, cells.length, 2400);
  });
}

// ---------------------------------------------------------------------------
// W5 — the comparison tabs, and the install picker, which is the same widget.
//
// The markup ships plain buttons and every pane visible, stacked, because that
// is what the page IS with no script: four specimens one after another, and
// nothing to press. It used to ship `role="tab"` as well — a promise of a
// keyboard contract that nothing kept: no tablist, no tabpanel, no
// `aria-controls`, and arrow keys that moved nothing.
//
// So the roles are installed HERE, by the code that makes them true, and all of
// them: a tablist, ids pairing each tab with its panel in both directions,
// `aria-selected`, ONE tab in the page's tab order at a time (roving tabindex),
// and Left/Right/Home/End to move between them.
// ---------------------------------------------------------------------------

/// Ids have to be unique in a document, and a page carries up to four groups.
let tabGroups = 0;

/// Whether `el` already contains something a keyboard can reach.
function hasFocusable(el) {
  return Boolean($("a[href], button, input, select, textarea, [tabindex]", el));
}

function tabsWidget(root, opts = {}) {
  const tabAttr = "data-" + (opts.tab || "tab");
  const paneAttr = "data-" + (opts.pane || "pane");
  const tabs = $$("[" + tabAttr + "]", root);
  const panes = $$("[" + paneAttr + "]", root);
  if (!tabs.length || !panes.length) return;
  const group = "tabs" + (tabGroups += 1);
  const keyOf = (el, attr) => el.getAttribute(attr);
  const paneFor = (id) => panes.find((p) => keyOf(p, paneAttr) === id);

  const list = tabs[0].parentElement;
  if (list) list.setAttribute("role", "tablist");
  tabs.forEach((tab, i) => {
    tab.id = group + "-tab-" + i;
    tab.setAttribute("role", "tab");
    const pane = paneFor(keyOf(tab, tabAttr));
    if (!pane) return;
    pane.id = group + "-panel-" + i;
    pane.setAttribute("role", "tabpanel");
    pane.setAttribute("aria-labelledby", tab.id);
    tab.setAttribute("aria-controls", pane.id);
    // A panel whose whole content is a code block has nothing to focus, and
    // that block scrolls sideways — so the panel itself takes the stop. One
    // that already holds a button does not need a second.
    if (!hasFocusable(pane)) pane.tabIndex = 0;
  });

  const select = (id, focus) => {
    for (const tab of tabs) {
      const on = keyOf(tab, tabAttr) === id;
      tab.setAttribute("aria-selected", String(on));
      // Roving: the group is ONE tab stop, and the arrows move inside it.
      tab.tabIndex = on ? 0 : -1;
      if (on && focus) tab.focus();
    }
    for (const pane of panes) pane.hidden = keyOf(pane, paneAttr) !== id;
  };

  const step = (from, by) =>
    select(keyOf(tabs[(from + by + tabs.length) % tabs.length], tabAttr), true);

  tabs.forEach((tab, i) => {
    tab.addEventListener("click", () => select(keyOf(tab, tabAttr), false));
    tab.addEventListener("keydown", (e) => {
      if (e.key === "ArrowRight") return e.preventDefault(), step(i, 1);
      if (e.key === "ArrowLeft") return e.preventDefault(), step(i, -1);
      if (e.key === "Home") return e.preventDefault(), select(keyOf(tabs[0], tabAttr), true);
      if (e.key === "End") {
        return e.preventDefault(), select(keyOf(tabs[tabs.length - 1], tabAttr), true);
      }
    });
  });

  const wanted = opts.initial && paneFor(opts.initial) ? opts.initial : keyOf(tabs[0], tabAttr);
  select(wanted, false);
}

// ---------------------------------------------------------------------------
// The explorer's search.
//
// The rows ARE the index. Each one carries its own lowercased haystack in
// `data-q` and its export names in `data-e`, both written by the export, so
// there is no fetch, no index file, and nothing to go stale. Filtering is
// `hidden` on a row.
//
// Every node this builds is built with `createElement` and `textContent`. What
// the reader typed reaches the DOM as TEXT and never as markup — the same rule
// the rest of the site keeps, and the reason a fragment cannot become an
// element here.
// ---------------------------------------------------------------------------

/// The export names of `row` that contain `q`, at most `limit` of them.
function matchingExports(row, q, limit) {
  const names = (row.dataset.e || "").split(" ").filter(Boolean);
  const hits = names.filter((n) => n.toLowerCase().includes(q));
  return hits.slice(0, limit);
}

/// A row's list of matched exports, each one a link to that export's anchor on
/// the module's own page.
function exportHits(row, names) {
  const box = document.createElement("span");
  box.className = "hits";
  for (const name of names) {
    const a = document.createElement("a");
    a.href = new URL("docs/std/" + row.dataset.m + ".html#e-" + name, SITE).href;
    a.textContent = name;
    box.appendChild(a);
  }
  return box;
}

function exploreSearch() {
  const input = $("[data-explore-input]");
  const list = $("[data-explore-list]");
  const count = $("[data-explore-count]");
  if (!input || !list) return;
  const rows = $$("li", list);
  const total = rows.length;

  const apply = () => {
    const q = input.value.trim().toLowerCase();
    let shown = 0;
    let hits = 0;
    for (const row of rows) {
      const old = $(".hits", row);
      if (old) old.remove();
      const match = q.length === 0 || (row.dataset.q || "").includes(q);
      row.hidden = !match;
      if (!match) continue;
      shown += 1;
      if (q.length === 0) continue;
      // Ten is enough to show WHICH names matched; the module page has them all.
      const names = matchingExports(row, q, 10);
      hits += names.length;
      if (names.length) row.appendChild(exportHits(row, names));
    }
    if (!count) return;
    if (q.length === 0) {
      count.textContent = total + " modules";
    } else if (shown === 0) {
      count.textContent = "no module matches " + q;
    } else {
      count.textContent = shown + (shown === 1 ? " module" : " modules") + ", " + hits + (hits === 1 ? " export" : " exports");
    }
  };

  input.addEventListener("input", apply);
  // A reader who arrives with a value already in the field (a restored form
  // state, a back navigation) sees it applied rather than ignored.
  if (input.value) apply();
}

// ---------------------------------------------------------------------------
// The explorer's graph. Vyrn laid every node and every wire out at build time;
// this lights up one module's own imports and importers, on hover AND on focus,
// because the nodes are links and a keyboard reaches them.
// ---------------------------------------------------------------------------

function graphWidget(svg) {
  const nodes = $$("g.node", svg);
  const wires = $$("path.wire", svg);
  const listOf = (el, key) => (el.dataset[key] || "").split(" ").filter(Boolean);

  const lift = (name) => {
    svg.classList.toggle("lit", Boolean(name));
    const node = name ? nodes.find((n) => n.dataset.m === name) : null;
    const near = node ? listOf(node, "uses").concat(listOf(node, "usedby")) : [];
    for (const w of wires) {
      w.classList.toggle("on", Boolean(name) && (w.dataset.from === name || w.dataset.to === name));
    }
    for (const n of nodes) {
      n.classList.toggle("on", n.dataset.m === name);
      n.classList.toggle("near", near.includes(n.dataset.m));
    }
  };

  for (const n of nodes) {
    n.addEventListener("pointerenter", () => lift(n.dataset.m));
    n.addEventListener("pointerleave", () => lift(null));
    n.addEventListener("focusin", () => lift(n.dataset.m));
    n.addEventListener("focusout", () => lift(null));
  }
}

// ---------------------------------------------------------------------------
// One orchestrated page entrance, rather than scattered micro-interactions.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The rail. Its links look clickable and are, so they also have to say where
// the reader is. Smooth scrolling is `html { scroll-behavior: smooth }` — the
// stylesheet's job, not this file's.
// ---------------------------------------------------------------------------

/// The rail's in-page links, paired with the section each one points at. A soft
/// navigation replaces the rail, so this is refilled per page rather than
/// captured once.
let railLinks = [];

function markRail() {
  // The active section is the LAST one whose top has passed a quarter of the
  // viewport. One rule, no thresholds to tune, and it is right at both ends of
  // the page: nothing has passed yet, so the first link wins.
  let current = railLinks.length ? railLinks[0] : null;
  for (const pair of railLinks) {
    if (pair.section.getBoundingClientRect().top <= innerHeight * 0.25) current = pair;
  }
  // The last section is usually too short to reach a quarter of the viewport,
  // because the page runs out of scroll first. At the bottom, it is the one.
  if (railLinks.length && innerHeight + scrollY >= document.documentElement.scrollHeight - 2) {
    current = railLinks[railLinks.length - 1];
  }
  for (const pair of railLinks) pair.link.classList.toggle("on", pair === current);
  if (current) keepInRail(current.link);
}

/// Keep the marked link visible when the rail is long enough to scroll — a
/// reference module has up to 44 exports, and a marker below the fold marks
/// nothing.
///
/// The rail's own `scrollTop` is set directly rather than through
/// `scrollIntoView`, which can scroll the PAGE as well and would fight the
/// scroll event that called this.
function keepInRail(link) {
  const rail = link.parentElement;
  if (!rail || rail.scrollHeight <= rail.clientHeight + 1) return;
  const top = link.offsetTop;
  const bottom = top + link.offsetHeight;
  if (top < rail.scrollTop) rail.scrollTop = top;
  else if (bottom > rail.scrollTop + rail.clientHeight) rail.scrollTop = bottom - rail.clientHeight;
}

function railSpy() {
  railLinks = [];
  const rail = $(".rail");
  if (!rail) return;
  for (const link of $$('a[href^="#"]', rail)) {
    const section = document.getElementById(decodeURIComponent(link.getAttribute("href").slice(1)));
    if (section) railLinks.push({ section, link });
  }
  markRail();
}

// One listener for the life of the document. `boot` runs again after every soft
// navigation, and re-registering here would stack a listener per page.
addEventListener("scroll", markRail, { passive: true });
addEventListener("resize", markRail, { passive: true });

// ---------------------------------------------------------------------------
// The masthead. `site/export.vyrn` stamps `aria-current="page"` on the row this
// document belongs to, so a reader with no JavaScript still sees where they are.
// A soft navigation replaces `<main>` and never touches the masthead, so the
// marker has to be moved here — otherwise it keeps pointing at the page the
// visitor arrived on.
// ---------------------------------------------------------------------------

/// Whether the navigation row `href` is the row for `path`.
///
/// A reference page belongs under Docs and a chapter under Guide: a row owns the
/// subtree named after it. This is the export's `currentNav` rule, in the
/// language the browser has, and it needs no list of the prefixes — adding a
/// third section adds nothing here.
function navOwns(href, path) {
  return path === href || path.startsWith(href.replace(/\.html$/, "") + "/");
}

function markNav() {
  const path = location.pathname;
  for (const link of $$(".masthead .nav a")) {
    // RESOLVED, not the attribute. The export writes every URL relative to the
    // document that carries it, so the attribute on a chapter page reads
    // `../docs.html` and would never equal a pathname. The URL constructor
    // answers what the browser itself would navigate to, at any mount point.
    const href = new URL(link.getAttribute("href"), location.href).pathname;
    const here = navOwns(href, path);
    if (here) link.setAttribute("aria-current", "page");
    else link.removeAttribute("aria-current");
  }
}

function entrance() {
  for (const group of $$(".specs")) {
    onView(group, (animate) => {
      if (!animate) return;
      group.classList.add("staged");
      requestAnimationFrame(() => {
        group.classList.remove("staged");
        group.classList.add("entered");
      });
    });
  }
}

// ---------------------------------------------------------------------------

/// The hero mounted by the last `boot`, if the page it was on had one.
let hero = null;

function boot() {
  copyButtons();
  markCopyable();
  entrance();
  markNav();
  railSpy();
  for (const el of $$('[data-widget="parity"]')) parityWidget(el);
  for (const el of $$('[data-widget="ownership"]')) ownershipWidget(el);
  for (const el of $$('[data-widget="types"]')) typesWidget(el);
  for (const el of $$('[data-widget="bars"]')) barsWidget(el);
  for (const el of $$('[data-widget="leak"]')) {
    barsWidget(el);
    leakWidget(el);
  }
  for (const el of $$('[data-widget="strip"]')) stripWidget(el);
  for (const el of $$("[data-tabs]")) tabsWidget(el);
  for (const el of $$("[data-install]")) installPicker(el);
  exploreSearch();
  for (const el of $$('[data-widget="play"]')) mountPlay(el);
  for (const el of $$("svg.graph")) graphWidget(el);
  // One hero at a time. `boot` runs again after every soft navigation, and a
  // mount that was never torn down keeps a window `resize`, a document
  // `visibilitychange` and a `matchMedia` listener alive over a canvas that is
  // no longer in the document.
  if (hero) {
    hero.destroy();
    hero = null;
  }
  const canvas = document.getElementById("field");
  if (canvas) hero = mountHero(canvas, { cellPx: 6 });
  refreshRelease();
}

boot();

// Soft navigation replaces the page body, so every widget on the new page needs
// booting again. `vyrn-nav.js` announces that it has swapped the DOM.
document.addEventListener("vyrn:nav-end", boot);
import("./vyrn-nav.js").catch(() => {}); // a hard navigation is a fine fallback
