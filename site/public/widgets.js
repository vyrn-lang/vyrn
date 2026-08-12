// The widget harness, and every widget's logic.
//
// The rule the whole site is built on: **Vyrn computes the geometry at build
// time, JavaScript animates one scalar.** Every bar position, every cell fill,
// every label below is already in the markup when this file loads. Nothing here
// computes a chart; it fades, sweeps, counts, or moves a playhead.
//
// No framework, no bundler, no dependency. Three files: this one, hero.js, and
// fresh.js.
import { mountHero } from "/hero.js";
import { refreshRelease } from "/fresh.js";

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
function markCopyable() {
  for (const el of $$("code")) {
    if (!inlineCode(el)) continue;
    el.classList.add("copyable");
    el.tabIndex = 0;
    el.setAttribute("role", "button");
    el.setAttribute("aria-label", "Copy " + codeText(el));
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

document.addEventListener("keydown", (e) => {
  if (e.key !== "Enter" && e.key !== " ") return;
  const el = inlineCode(e.target);
  if (!el) return;
  // A span with `role="button"` gets no synthetic click from the browser, so
  // this is the only keyboard path and it cannot double-fire. Space would
  // scroll the page.
  e.preventDefault();
  copySpan(el);
});

/// Pick the platform tab that matches the visitor, and let them override it.
function installPicker(root) {
  const guess = /Win/i.test(navigator.platform)
    ? "windows"
    : /Mac/i.test(navigator.platform)
      ? "macos"
      : "linux";
  const select = (id) => {
    for (const btn of $$("[data-plat]", root)) {
      btn.setAttribute("aria-selected", String(btn.dataset.plat === id));
    }
    for (const pane of $$("[data-plat-pane]", root)) {
      pane.hidden = pane.dataset.platPane !== id;
    }
  };
  for (const btn of $$("[data-plat]", root)) {
    btn.addEventListener("click", () => select(btn.dataset.plat));
  }
  select($(`[data-plat="${guess}"]`, root) ? guess : "linux");
}

// ---------------------------------------------------------------------------
// W2 — the parity strip. The three columns fill, then the verdict lands.
// The pause before it is the widget.
// ---------------------------------------------------------------------------

function parityWidget(root) {
  const cols = $("[data-parity-cols]", root);
  const verdict = $("[data-parity-verdict]", root);
  const num = $("[data-parity-num]", root);
  const runBtn = $("[data-parity-run]", root);
  const breakBtn = $("[data-parity-break]", root);
  const wasmPane = $('[data-col="wasm"]', root);
  const good = wasmPane.textContent.trim();
  // One byte of the last column, flipped. Nothing else about the run changes,
  // which is the point: the check is a byte comparison, not a green tick.
  const bad = good.slice(0, -1) + (good.endsWith("3") ? "7" : "3");
  let broken = false;

  const settle = () => {
    cols.classList.toggle("broken", broken);
    verdict.classList.toggle("bad", broken);
    wasmPane.textContent = broken ? bad : good;
    verdict.textContent = broken ? "1 byte differs — the harness fails" : "0 bytes differ";
    num.firstChild.textContent = broken ? "1 " : "0 ";
    num.querySelector("small").textContent = broken ? "byte differs" : "bytes differ";
  };

  function run() {
    if (REDUCED) {
      settle();
      return;
    }
    cols.classList.add("arming");
    setTimeout(() => cols.classList.remove("arming"), 120);
    // Hold before the payoff, per the brief: the pause is the widget.
    setTimeout(settle, 900);
  }

  settle();
  runBtn.addEventListener("click", run);
  breakBtn.addEventListener("click", () => {
    broken = !broken;
    breakBtn.setAttribute("aria-pressed", String(broken));
    breakBtn.textContent = broken ? "Put the byte back" : "Break one byte";
    run();
  });
  // No callback at all — a widget never scrolled into view — leaves the markup
  // as the export wrote it, which is already the answer.
  onView(root, (animate) => animate && run());
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
// W5 — the comparison tabs.
// ---------------------------------------------------------------------------

function tabsWidget(root) {
  const select = (id) => {
    for (const btn of $$("[data-tab]", root)) {
      btn.setAttribute("aria-selected", String(btn.dataset.tab === id));
    }
    for (const pane of $$("[data-pane]", root)) pane.hidden = pane.dataset.pane !== id;
  };
  for (const btn of $$("[data-tab]", root)) {
    btn.addEventListener("click", () => select(btn.dataset.tab));
  }
  // Every pane is visible in the exported markup, stacked, so a reader with no
  // JavaScript still sees all four. Selecting one is what a script adds.
  const first = $("[data-tab]", root);
  if (first) select(first.dataset.tab);
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

function markNav() {
  const path = location.pathname;
  for (const link of $$(".masthead .nav a")) {
    const href = link.getAttribute("href");
    // A reference page belongs under Docs, the one row that owns a subtree.
    // This is the export's `currentNav` rule, in the language the browser has.
    const here = path === href || (href === "/docs.html" && path.startsWith("/docs/"));
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
  const canvas = document.getElementById("field");
  if (canvas) mountHero(canvas, { cellPx: 6 });
  refreshRelease();
}

boot();

// Soft navigation replaces the page body, so every widget on the new page needs
// booting again. `vyrn-nav.js` announces that it has swapped the DOM.
document.addEventListener("vyrn:nav-end", boot);
import("/vyrn-nav.js").catch(() => {}); // a hard navigation is a fine fallback
