// The widget harness, and every widget's logic.
//
// The rule the whole site is built on: **Vyrn computes the geometry at build
// time, JavaScript animates one scalar.** Every bar position, every cell fill,
// every label below is already in the markup when this file loads. Nothing here
// computes a chart; it fades, sweeps, counts, or moves a playhead.
//
// No framework, no bundler, no dependency. One file.
// NOTHING ELSE IS IMPORTED HERE, and that is the page-weight rule (RFC-0106
// M1, M2). `play.js`, `play-wasm.js` and `wasi-min.js` were static imports
// once, and a static import is a download: twelve of the thirteen consumer
// pages fetched the playground's 14,587 gzipped bytes to run none of it. They
// are `import()` at the point of use now — inside the loop over the elements
// only `/play` has, and inside the hero editor's first interaction on `/`.
// `vyrn-nav.js` and `vyrn-dom.js` (20,275 gzipped between them) left the same
// way at the foot of this file: a soft navigator is fetched when a reader
// reaches for a link, not before they have read anything.

// Where the site is. Every file this page loads is a sibling of this script, and
// this script knows its own URL, so that is the whole derivation — no flag baked
// in at build time, no root assumed. It is correct at a domain root, under
// `/vyrn/` on GitHub Pages, behind a preview URL, and on `file://`. The imports
// above are relative for the same reason: a module specifier resolves against
// the module that wrote it.
const SITE = new URL(".", import.meta.url);

// Soft navigation across a static host: the payload for /why-vyrn.html lives
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
    // The async API refuses in more situations than a reader can see — the
    // document briefly unfocused, an embedding policy — while the selection
    // path still works there. The fallback is the old way, tried second.
    try {
      const box = document.createElement("textarea");
      box.value = text;
      box.setAttribute("readonly", "");
      box.style.position = "fixed";
      box.style.left = "-9999px";
      document.body.append(box);
      box.select();
      const ok = document.execCommand("copy");
      box.remove();
      return ok;
    } catch (err2) {
      return false;
    }
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

/// COPY THIS PAGE AS MARKDOWN (RFC-0106 M1).
///
/// The element is a LINK to the `.md` file the export writes beside the page, so
/// with no script — or with no clipboard, which is every insecure origin — a
/// press does the thing the element says it does. With both, the file is fetched
/// and put on the clipboard and the page does not move.
///
/// A failure is REPORTED and then falls through to the navigation: the link is
/// still the answer, and a reader who pressed a button and got nothing has been
/// lied to.
function copyPageButtons() {
  for (const link of $$("a[data-copy-md]")) {
    // Set once the fetch has failed. The second press is then an ordinary press
    // on an ordinary link, which is where a reader should end up when the copy
    // cannot happen — not at a button that reports the same failure forever.
    let broken = false;
    link.addEventListener("click", async (e) => {
      if (broken || !navigator.clipboard) return;
      e.preventDefault();
      pinWidth(link);
      const was = link.textContent;
      const res = await fetch(link.href).catch(() => null);
      const md = res && res.ok ? await res.text() : "";
      broken = !md;
      link.textContent = md && (await writeClipboard(md)) ? "Copied" : "Open it instead";
      setTimeout(() => (link.textContent = was), 1600);
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


/// Check the OS radio that matches the visitor.
///
/// The picker itself is CSS and markup — two radio buttons, a `:checked ~`
/// selector, no script (RFC-0106 M3). This is the whole of what a script adds:
/// a guess about which one to open on. With no script the first is checked and
/// the control works exactly the same, which is why this can be three lines and
/// has no keyboard contract of its own — a native radio group already has one.
///
/// TWO THINGS WERE WRONG WITH THE FIRST VERSION, and both are the kind that pass
/// review and fail in a browser (RFC-0106 M3, third round):
///
///   1. It looked for `input[data-os]` and only the INDEX carried that attribute.
///      On `/install` — the page a reader actually installs from — there was
///      nothing to match, so the guess silently did nothing.
///   2. `navigator.platform` is deprecated and frozen. It still answers `Win32`
///      in Chrome, but a Windows browser reporting the frozen `Linux x86_64`
///      answers wrong, and there is no reason to prefer it to
///      `userAgentData.platform`, which is the replacement, or to the user-agent
///      string, which every browser still has.
///
/// The question is now the one the picker actually asks: PowerShell or a POSIX
/// shell. Anything that is not Windows takes the first tab, which is also the
/// no-script default, so a wrong guess costs one press and never a wrong command.
function guessOs(root) {
  const ua = navigator.userAgentData?.platform || navigator.platform || navigator.userAgent || "";
  const radio = $(`input[data-os="${/win/i.test(ua) ? "windows" : "unix"}"]`, root);
  if (radio) radio.checked = true;
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
  // Handed back so another widget can drive the same group without owning a
  // second copy of the contract. The radar's axis wedges are exactly that: a
  // pointer shortcut to a tab a keyboard already reaches.
  return select;
}

// ---------------------------------------------------------------------------
// W9 — the benchmark radar (RFC-0104 M3).
//
// Vyrn computed every coordinate, both normalizations, and the off-scale marks
// for each. This does three things and none of them is arithmetic:
//
//   - the axis panels are a TAB GROUP, so they are `tabsWidget`'s and not this
//     one's: roving tabindex, arrow keys, aria-selected, paired ids. The wedges
//     in the SVG call the same `select`, which is why hovering an axis and
//     tabbing to it land in the same state instead of two states that look alike;
//   - the legend hides a series by putting one class on one group;
//   - the normalization switch swaps `points` for the polygon's `data-alt` and
//     puts one class on the SVG, which is what moves the off-scale marks.
//
// Nothing animates. There is no motion to reduce, which is how this widget
// respects `prefers-reduced-motion` — by not having any.
// ---------------------------------------------------------------------------

function radarWidget(root) {
  const svg = $("svg.radar", root);
  if (!svg) return;

  // The markup ships every panel visible, one after another, because that is
  // what this section IS with no script: eight compact tables in the game's own
  // columns, each with the cause in a few words and a link to the whole of it.
  // `tabsWidget` hides the other seven the moment it mounts, so nothing here
  // has to.
  const select = tabsWidget(root, { tab: "axis", pane: "axis-pane" });
  const names = $$("[data-axis-name]", svg);
  const tabs = $$("[data-axis]", root);

  /// Light the spoke of whichever axis the group has selected.
  ///
  /// It READS the selection back rather than being told what it is. That is the
  /// whole reason the chart and the panel cannot disagree: there is one source
  /// of truth — the group's own `aria-selected` — and both the arrow keys and a
  /// wedge move it before this runs. Remembering the key separately was the
  /// first version, and a keyboard walk moved the panel while leaving the spoke
  /// lit on the axis before it.
  const syncLit = () => {
    const on = tabs.find((t) => t.getAttribute("aria-selected") === "true");
    const key = on ? on.getAttribute("data-axis") : "";
    for (const t of names) t.classList.toggle("lit", t.dataset.axisName === key);
  };
  const list = tabs.length ? tabs[0].parentElement : null;
  if (list) {
    // After the group's own handlers, whichever of them moved the selection.
    list.addEventListener("click", syncLit);
    list.addEventListener("keyup", syncLit);
    list.addEventListener("focusin", syncLit);
  }
  if (select) {
    for (const wedge of $$("[data-axis-hit]", svg)) {
      wedge.addEventListener("mouseenter", () => {
        select(wedge.dataset.axisHit, false);
        syncLit();
      });
    }
  }
  syncLit();

  for (const btn of $$("[data-series-toggle]", root)) {
    const group = $('g[data-series="' + btn.dataset.seriesToggle + '"]', svg);
    if (!group) continue;
    btn.addEventListener("click", () => {
      const on = btn.getAttribute("aria-pressed") !== "true";
      btn.setAttribute("aria-pressed", String(on));
      group.classList.toggle("off", !on);
    });
  }

  const polys = $$("g.series > polygon", svg);
  for (const p of polys) p.dataset.home = p.getAttribute("points");
  for (const btn of $$("[data-norm]", root)) {
    btn.addEventListener("click", () => {
      const alt = btn.dataset.norm === "rust";
      svg.classList.toggle("alt", alt);
      for (const p of polys) p.setAttribute("points", alt ? p.dataset.alt : p.dataset.home);
      for (const other of $$("[data-norm]", root)) {
        other.setAttribute("aria-pressed", String(other === btn));
      }
    });
  }
}

// ---------------------------------------------------------------------------
// The reference's search (RFC-0105 M2: it was the explorer's, and moved with the
// module list to /docs).
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

function moduleSearch() {
  const input = $("[data-search-input]");
  const list = $("[data-search-list]");
  const count = $("[data-search-count]");
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
// The reference's import graph. Vyrn laid every node and every wire out at build
// time;
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

/// Freeze the shell's links to the URLs they resolve to on the document that
/// shipped them.
///
/// A soft navigation replaces `<main>` and nothing else, so the masthead and
/// the footer OUTLIVE the address bar they were written against. Every URL this
/// site writes is relative to its own document, which is what makes one export
/// work at any mount point — but a relative URL is read at the moment it is
/// clicked. Navigate from `/docs.html` to `/docs/std/json.html` and the
/// masthead's `docs.html` means `/docs/std/docs.html`, which is nowhere.
///
/// Resolving each one here, once, on the document it arrived with, makes the
/// shell immune to the depth changing under it. The result is same-origin, so a
/// click on it is still soft-navigated.
///
/// Fragment links are left alone: `#main` has to keep meaning THIS page.
function anchorShell() {
  for (const a of $$(".masthead a[href], .foot a[href]")) {
    if (!a.getAttribute("href").startsWith("#")) a.setAttribute("href", a.href);
  }
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
// THE HERO EDITOR, ARMED AND NOT MOUNTED (RFC-0106 M2)
//
// The index's editor is the playground on a smaller root: same `data-play-*`
// hooks, same keyboard contract, same highlighter, `mountPlay` and not a second
// editor. What is different is WHEN. Mounting it on load would fetch `play.js`,
// `play-wasm.js`, `wasi-min.js` and the compiler module itself on a page a
// reader has not decided to use yet — M0's whole page-weight finding, applied to
// the one page the finding was computed for.
//
// So the document ships a coloured code block over a read-only textarea, and
// three interactions arm it: pointing at the plate, tabbing into it, and
// pressing Run. The first two are enough time for the module to land before the
// reader reaches the button; the third does not lose the press, because the
// mount reports readiness and the click is replayed. Without script the same
// markup is a code block with a link to `/play`, which is what the `<noscript>`
// in the template says.
// ---------------------------------------------------------------------------

function armHeroEditor(root) {
  const runBtn = $("[data-play-run]", root);
  const src = $("[data-play-src]", root);
  const status = $("[data-play-status]", root);
  const resetBtn = $("[data-play-reset]", root);
  const outPane = $("[data-play-out]", root);
  if (!runBtn || !src) return;
  let armed = false;

  const arm = (thenRun) => {
    if (armed) return;
    armed = true;
    // Editable from the moment the reader has asked for it, and not before: a
    // textarea a reader can type into while nothing can check or run it is a
    // control that lies about what it does.
    src.removeAttribute("readonly");
    if (status) status.textContent = "Loading the compiler…";
    import("./play.js").then(
      ({ mountPlay }) => {
        if (!root.isConnected) return;
        mountPlay(root, { onReady: () => thenRun && runBtn.click() });
      },
      (err) => {
        // HONEST WHEN IT CANNOT RUN (RFC-0106 M3, fourth round). This used to
        // set the status line and stop: the output pane still said `Not run
        // yet.`, the textarea stayed editable, and the Run button stayed live
        // over an editor with no compiler behind it. Now the pane says what
        // happened and where to go, the box goes back to read-only, and the two
        // controls are disabled — the same shape as `mountPlay`'s own failure.
        if (status) status.textContent = "The compiler did not load";
        if (outPane) {
          outPane.textContent = "";
          const el = document.createElement("span");
          el.className = "stderr";
          el.textContent = "The compiler did not load, so nothing here can run. Every program on this page is on /play too.";
          outPane.append(el);
        }
        src.setAttribute("readonly", "readonly");
        runBtn.disabled = true;
        if (resetBtn) resetBtn.disabled = true;
        console.error("the hero editor did not load", err);
      }
    );
  };

  // `pointerenter` covers a mouse and a touch; `focusin` covers a keyboard,
  // and it is the row the a11y checklist asks for. Both are `once`.
  root.addEventListener("pointerenter", () => arm(false), { once: true });
  root.addEventListener("focusin", () => arm(false), { once: true });
  // `Reset` before the editor is mounted arms it, so the button is never a
  // control that does nothing. `mountPlay` binds the real handler after.
  if (resetBtn) resetBtn.addEventListener("click", () => arm(false));
  // Bound BEFORE `mountPlay` adds its own, so the first press arms and the
  // replayed press runs. A press while the module is in flight is a no-op in
  // `mountPlay`'s own handler, which is why readiness has to drive the replay.
  runBtn.addEventListener("click", () => arm(true));
}

// ---------------------------------------------------------------------------
// THE RECORDED DEMO (RFC-0106 M2)
//
// Seven steps, all seven in the document, all seven real output from a real
// binary — `scripts/site-demo.py` ran it before the page was rendered. Without
// script they are a numbered list and the page is complete. With script they
// become one step at a time with a counter, which is the only thing this adds:
// no content is script-only, and nothing is fetched.
//
// The controls are BUILT here rather than shipped in the markup, because a
// `Next` button on a page that shows every step at once is a control that does
// nothing (RFC-0105 M4's rule, and M1's `Menu` summary is what happens when it
// is broken).
// ---------------------------------------------------------------------------

function demoWidget(root) {
  const steps = $$("[data-step]", root);
  const panes = $$("[data-pane]", root);
  if (steps.length < 2 || panes.length !== steps.length) return;

  const bar = document.createElement("div");
  bar.className = "controls demobar";
  const back = document.createElement("button");
  back.type = "button";
  back.textContent = "‹";
  back.setAttribute("aria-label", "Back");
  const next = document.createElement("button");
  next.type = "button";
  next.textContent = "›";
  next.setAttribute("aria-label", "Next");
  const count = document.createElement("span");
  count.className = "eyebrow";
  // Polite, because the reader pressed the button that changed it.
  count.setAttribute("aria-live", "polite");
  bar.append(count, back, next);
  // The controls belong to the pane they page — the bottom of the terminal,
  // the reference site's own placement — not a toolbar over the whole card.
  ($(".panes", root) || root).append(bar);

  // The list of titles STAYS (RFC-0106 M3). It used to set `hidden` on six of
  // seven, which threw the outline away with the detail; every row is on the
  // page and `.stepped` is what tells the sheet a script is driving — with no
  // script no step is `.on`, every pane is open, and the sheet's `.stepped`
  // rules match nothing.
  //
  // TWO LISTS, ONE INDEX (fourth round): the nth row and the nth pane are
  // marked together, and the guard above refuses to drive them at all unless
  // there is exactly one pane per step. `aria-current` is on the row rather than
  // announced from the counter, so the marked step is the marked step for a
  // screen reader too.
  root.classList.add("stepped");
  let at = 0;
  const show = (i) => {
    at = (i + steps.length) % steps.length;
    steps.forEach((s, k) => {
      s.classList.toggle("on", k === at);
      if (k === at) s.setAttribute("aria-current", "step");
      else s.removeAttribute("aria-current");
    });
    panes.forEach((p, k) => p.classList.toggle("on", k === at));
    count.textContent = at + 1 + " / " + steps.length;
    back.disabled = at === 0;
    next.textContent = at === steps.length - 1 ? "Replay" : "›";
    if (at === steps.length - 1) next.removeAttribute("aria-label");
    else next.setAttribute("aria-label", "Next");
  };
  // Pointer-only, and deliberately: every step is already reachable with Back,
  // Next and the arrow keys, so this adds no function a keyboard cannot do.
  steps.forEach((s, k) => s.addEventListener("click", () => show(k)));
  back.addEventListener("click", () => show(at - 1));
  next.addEventListener("click", () => show(at === steps.length - 1 ? 0 : at + 1));
  root.addEventListener("keydown", (e) => {
    if (e.key === "ArrowRight") return e.preventDefault(), show(at + 1);
    if (e.key === "ArrowLeft") return e.preventDefault(), show(at - 1);
  });
  show(0);
}

// ---------------------------------------------------------------------------

function boot() {
  copyButtons();
  copyPageButtons();
  markCopyable();
  entrance();
  anchorShell();
  markNav();
  railSpy();
  for (const el of $$('[data-widget="demo"]')) demoWidget(el);
  for (const el of $$('[data-widget="bars"]')) barsWidget(el);
  for (const el of $$('[data-widget="leak"]')) {
    barsWidget(el);
    leakWidget(el);
  }
  for (const el of $$('[data-widget="strip"]')) stripWidget(el);
  for (const el of $$("[data-tabs]")) tabsWidget(el);
  for (const el of $$('[data-widget="radar"]')) radarWidget(el);
  for (const el of $$(".picker")) guessOs(el);
  moduleSearch();
  // The playground, on the one page that has one. `boot()` is not awaited and
  // the mount does not have to finish before the rest of the page works, so the
  // import is fired and left to land — a rejection is reported rather than
  // swallowed, because a playground that silently never mounts is worse than an
  // error in the console.
  const playgrounds = $$('[data-widget="play"]');
  if (playgrounds.length) {
    import("./play.js").then(({ mountPlay }) => {
      for (const el of playgrounds) mountPlay(el);
    }, (err) => console.error("the playground did not load", err));
  }
  for (const el of $$('[data-widget="playhero"]')) armHeroEditor(el);
  for (const el of $$("svg.graph")) graphWidget(el);
}

boot();

// ---------------------------------------------------------------------------
// THE SEARCH OVERLAY (RFC-0106 M1)
//
// `/` opens it on every page, Esc closes it, the arrows walk the results and
// Enter follows one. The index is ONE file, fetched on the first open and never
// again, and never inlined in a document: 448 rows and 42 KB that a page which
// never searches does not pay for. `site/app/search.vyrn` builds it.
//
// Bound ONCE, outside `boot()`. `boot()` runs again after every soft navigation,
// and a key listener added per navigation is a key listener per navigation — the
// overlay lives in the shell, which a soft navigation never swaps, so its
// listeners belong out here with it.
// ---------------------------------------------------------------------------
{
  const box = $("[data-find]");
  const field = $("[data-find-input]");
  const list = $("[data-find-results]");
  const note = $("[data-find-note]");
  // The backstage wears its own shell and has no overlay in it.
  if (box && field && list) {
    let index = null;
    let loading = null;
    let cursor = -1;
    // Where focus was when the overlay opened, so it can be given back. A
    // reader who presses `/` in the middle of a reference page and then Esc has
    // to end up where they were and not at the top of the document.
    let cameFrom = null;

    /// Fetch the index once. A failure says so in the panel rather than leaving
    /// an empty list, which would read as "nothing matches".
    function load() {
      if (index) return Promise.resolve();
      if (loading) return loading;
      loading = fetch(new URL("search.json", SITE))
        .then((r) => (r.ok ? r.json() : Promise.reject(new Error(r.status))))
        .then((rows) => {
          index = rows;
          loading = null;
        })
        .catch((err) => {
          loading = null;
          note.textContent = "The index did not load. Every page is still linked from the navigation.";
          console.error("the search index did not load", err);
        });
      return loading;
    }

    // The four sections, in the order the overlay shows them, which is the order
    // `site/app/search.vyrn` writes them in.
    const SECTIONS = ["Docs", "Reference", "Packages", "Releases"];

    /// The rows that match `q`: the best 40, in section order.
    ///
    /// A substring match over the title and the description, ranked by where the
    /// hit lands: a title that STARTS with the query, then any title hit, then a
    /// description hit. No fuzzy matching and no scoring model — 448 rows, and a
    /// reader who is typing a name they already know.
    ///
    /// RANKED FIRST, THEN GROUPED, and the two are separate passes because they
    /// answer different questions. The rank decides WHICH forty rows a reader
    /// sees, so it must run over all of them; the section decides what ORDER
    /// those forty appear in, so it runs over the forty. Sorting by rank alone
    /// and drawing a heading whenever the section changed gave
    /// `Reference · Docs · Reference · Docs · Reference` for `text` — five
    /// headings for two sections, which is a list that is not sectioned at all.
    /// Both sorts are stable, so a section's rows stay in their ranked order
    /// inside it.
    function search(q) {
      const needle = q.toLowerCase();
      const hits = [];
      for (const r of index) {
        const at = r.t.toLowerCase().indexOf(needle);
        let rank = -1;
        if (at === 0) rank = 0;
        else if (at > 0) rank = 1;
        else if (r.d.toLowerCase().includes(needle)) rank = 2;
        if (rank >= 0) hits.push({ r, rank });
      }
      hits.sort((a, b) => a.rank - b.rank);
      const best = hits.slice(0, 40).map((h) => h.r);
      best.sort((a, b) => SECTIONS.indexOf(a.s) - SECTIONS.indexOf(b.s));
      return best;
    }

    /// The fragment `r` leads to: an export's own id on its module's page, and
    /// nothing for every other row.
    ///
    /// A reference row's title is `emit — std/json` and the id the page carries
    /// is `e-emit`, which is why the index does not spend 354 names on saying it
    /// twice. A module's own row is `std/json` and has no dash in it, so the two
    /// kinds of reference row stay apart on the one thing that distinguishes
    /// them.
    function anchorOf(r) {
      if (r.s !== "Reference") return "";
      const cut = r.t.indexOf(" — std/");
      return cut > 0 ? "#e-" + r.t.slice(0, cut) : "";
    }

    /// Draw the results under their section headings.
    function render(q) {
      list.textContent = "";
      cursor = -1;
      if (!q || !index) {
        if (!q) note.textContent = "Type to search. Esc closes.";
        return;
      }
      const hits = search(q);
      note.textContent = hits.length
        ? `${hits.length} result${hits.length === 1 ? "" : "s"}. Arrows to move, Enter to open, Esc to close.`
        : `Nothing matches "${q}".`;
      let section = null;
      for (const r of hits) {
        if (r.s !== section) {
          section = r.s;
          const h = document.createElement("p");
          h.className = "sect";
          h.textContent = section;
          list.append(h);
        }
        const a = document.createElement("a");
        a.className = "hit";
        // The index carries the route's identity — `/docs.html` — and every
        // document is served from an unknown mount point, so the URL is resolved
        // against this script's own directory, exactly as the export resolves the
        // ones it writes.
        //
        // AN EXPORT'S ANCHOR IS DERIVED AND NOT CARRIED. `#e-emit` is the row's
        // own title with three bytes in front of it, and 354 of the 448 rows are
        // exports, so the index would have written every one of those names
        // twice: 7,763 gzipped against M0's 8,000, or 6,564 with this line here.
        // The reference landing's own filter builds the same fragment the same
        // way, further up this file.
        a.href = new URL(r.u.slice(1) + anchorOf(r), SITE).href;
        a.setAttribute("role", "option");
        a.setAttribute("aria-selected", "false");
        a.textContent = r.t;
        if (r.d) {
          const d = document.createElement("span");
          d.className = "d";
          d.textContent = r.d;
          a.append(d);
        }
        list.append(a);
      }
    }

    /// Move the selection by `step`, wrapping, and keep it on screen.
    ///
    /// FOCUS STAYS IN THE FIELD and the row is marked `aria-selected`. That is
    /// the listbox pattern: a reader who has typed three letters and pressed
    /// Down must be able to type a fourth without moving focus back.
    function move(step) {
      const hits = $$(".hit", list);
      if (!hits.length) return;
      if (cursor >= 0) hits[cursor].setAttribute("aria-selected", "false");
      cursor = (cursor + step + hits.length) % hits.length;
      hits[cursor].setAttribute("aria-selected", "true");
      hits[cursor].scrollIntoView({ block: "nearest" });
    }

    function open() {
      if (!box.hidden) return;
      cameFrom = document.activeElement;
      box.hidden = false;
      field.value = "";
      render("");
      field.focus();
      load().then(() => render(field.value));
    }

    function close() {
      if (box.hidden) return;
      box.hidden = true;
      list.textContent = "";
      // Back where the reader was. A soft navigation may have replaced that
      // element, so its presence is checked rather than assumed.
      if (cameFrom && cameFrom.isConnected) cameFrom.focus();
      cameFrom = null;
    }

    for (const btn of $$("[data-find-open]")) btn.addEventListener("click", open);

    // `/` from anywhere, unless the reader is already typing. A page with a
    // module filter or a playground editor on it must not swallow a slash.
    addEventListener("keydown", (e) => {
      const el = document.activeElement;
      const typing = /^(input|textarea|select)$/i.test(el.tagName) || el.isContentEditable;
      if (e.key === "/" && !typing && !e.metaKey && !e.ctrlKey && !e.altKey) {
        e.preventDefault();
        open();
      } else if (e.key === "Escape" && !box.hidden) {
        e.preventDefault();
        close();
      }
    });

    // Pressing the ground closes it; pressing the panel does not.
    box.addEventListener("mousedown", (e) => {
      if (e.target === box) close();
    });

    field.addEventListener("input", () => render(field.value));
    field.addEventListener("keydown", (e) => {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        move(1);
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        move(-1);
      } else if (e.key === "Enter") {
        const hits = $$(".hit", list);
        const pick = cursor >= 0 ? hits[cursor] : hits[0];
        if (pick) {
          e.preventDefault();
          // Closed BEFORE the navigation, and without restoring focus: the
          // element focus came from is about to be replaced, and a reader must
          // not land on a new page with a dialog over it.
          box.hidden = true;
          list.textContent = "";
          cameFrom = null;
          pick.click();
        }
      }
    });

    // THE FOCUS TRAP, and it is one rule because the panel is one field and a
    // list walked with the arrows: while the overlay is open, Tab returns to the
    // field. Nothing behind the dialog can be reached, which is what
    // `aria-modal` claims and what a claim has to be true of.
    box.addEventListener("keydown", (e) => {
      if (e.key === "Tab") {
        e.preventDefault();
        field.focus();
      }
    });
  }
}

/// Where the reader is after a soft navigation (RFC-0105 M4).
///
/// The defect, measured: `vyrn-nav.js` swaps `<main>` and scrolls to the top,
/// and the link that was clicked went with the old `<main>`. Focus fell to
/// `<body>`, so the next Tab started at the masthead again — a keyboard reader
/// who followed a link from the middle of a reference page landed nowhere and
/// had to walk the whole shell back, and a screen reader was told nothing at all
/// because no document load happened.
///
/// Both halves are fixed here rather than in `vyrn-nav.js`: the shell this site
/// wears is this site's business, and the navigator is shared with two other
/// applications. `#main` is the anchor the skip link already lands on — a box
/// with no size, `tabindex="-1"`, sitting immediately before the content — so
/// the next Tab is the first link of the page that just arrived, which is what
/// a real navigation does.
function landed() {
  boot();
  const main = document.getElementById("main");
  if (main) main.focus({ preventScroll: true });
  // The title is what a browser reads out on a real navigation, and this is the
  // navigation that never had one.
  announce(document.title);
}

// Soft navigation replaces the page body, so every widget on the new page needs
// booting again. `vyrn-nav.js` announces that it has swapped the DOM.
document.addEventListener("vyrn:nav-end", landed);

// AND THE NAVIGATOR ITSELF, ON THE FIRST REACH FOR A LINK (RFC-0106 M2).
//
// This was a bare `import()` on load, so every page fetched `vyrn-nav.js` and
// `vyrn-dom.js` — 20,275 gzipped between them, the single largest item left in
// M0's page-weight census and the one M1 recorded as out of reach. It is not a
// module a page NEEDS: a hard navigation is the declared fallback and it is
// what happens before this lands. So it is fetched the first time a reader
// points at, touches, or tabs to a link, which is earlier than the click that
// would use it and later than the paint that would pay for it.
{
  let asked = false;
  const fetchNavigator = () => {
    if (asked) return;
    asked = true;
    import("./vyrn-nav.js").catch(() => {}); // a hard navigation is a fine fallback
  };
  for (const kind of ["pointerover", "touchstart", "focusin"]) {
    document.addEventListener(kind, (e) => {
      if (e.target.closest && e.target.closest("a[href]")) fetchNavigator();
    }, { passive: true });
  }
}
