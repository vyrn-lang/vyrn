// The widget harness and every widget's logic.
//
// Vyrn computes the geometry at build time. Every position, fill and label is
// in the markup when this file loads; nothing here computes a layout.
//
// No framework, no bundler, no dependency, and no static import, because a
// static import is a download on every page. The playground (14,587 gzipped
// bytes) loads through `import()` where it is used, and the soft navigator
// (`vyrn-nav.js` and `vyrn-dom.js`, 20,275 gzipped) loads on the first reach
// for a link, at the foot of this file.

// The site's base. Every file this page loads is a sibling of this script, so
// it resolves at a domain root, under a path prefix and on `file://`.
const SITE = new URL(".", import.meta.url);

// Soft navigation on a static host: a page's payload is the `.data.json` file
// beside it, because a file host cannot vary on `Accept`. Set before the
// navigator loads.
window.__vyrnNavConfig = { staticData: true };

const REDUCED = matchMedia("(prefers-reduced-motion: reduce)").matches;

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
        // Half of it visible, or half a screen of it, so a widget taller than
        // the viewport still fires.
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

/// Pin a button's width before its label changes, so nothing reflows.
function pinWidth(btn) {
  btn.style.minWidth = Math.ceil(btn.getBoundingClientRect().width) + "px";
}

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));

/// Puts `text` on the clipboard and returns whether it worked.
///
/// `navigator.clipboard` is missing on an insecure origin and can be refused
/// by permission, so a failure is an answer the callers show.
async function writeClipboard(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch (err) {
    // The async API refuses in cases a reader cannot see (an unfocused
    // document, an embedding policy) where the selection path still works.
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

/// Wires "Copy page as Markdown". The element is a link to the `.md` file the
/// export writes beside the page, so with no script or no clipboard a press
/// follows the link. With both, the file is fetched onto the clipboard.
function copyPageButtons() {
  for (const link of $$("a[data-copy-md]")) {
    // Set when the fetch fails: the next press follows the link.
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

// Inline code copies itself on click: a command written inline in a sentence
// should not need a drag to copy. `isPlainCopyClick` keeps text selection
// working.

/// The code span `node` is inside, if that span is in running text.
///
/// A `<pre>`, a command plate and a numbered source row are blocks, and a
/// plate has its own Copy button. The marking pass and the listener share this
/// predicate, so they agree on what is in scope.
function inlineCode(node) {
  const el = node && node.closest ? node.closest("code") : null;
  if (!el || el.closest("pre, .cmd, .lines")) return null;
  return el;
}

/// What an inline span copies: its text as rendered, each run of whitespace
/// collapsed to one space and the ends trimmed.
function codeText(el) {
  return el.textContent.replace(/\s+/g, " ").trim();
}

/// Whether a click on a code span is a copy rather than part of a selection:
/// the pointer moved at most 3px, it is not the second click of a
/// double-click, and nothing is selected. A keyboard activation has no
/// pointer-down position (`down` is null) and counts as a copy.
function isPlainCopyClick(down, up, selected) {
  if (up.detail >= 2) return false;
  if (selected) return false;
  if (!down) return true;
  return Math.abs(up.x - down.x) <= 3 && Math.abs(up.y - down.y) <= 3;
}

/// The polite live region every copy reports into. A soft navigation replaces
/// `<main>`, not `<body>`, so one region serves the document.
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

/// Copies one span and reports it twice: a colour flash for the eye and the
/// live region for a screen reader. The flash is a class, because a label
/// would reflow the sentence and a tooltip is clipped inside a `.scroller`.
async function copySpan(el) {
  const text = codeText(el);
  const ok = await writeClipboard(text);
  el.classList.add(ok ? "copied" : "copyfail");
  setTimeout(() => el.classList.remove("copied", "copyfail"), 900);
  if (ok) {
    announce("Copied " + text);
  } else {
    // The clipboard refused: select the text so the reader can copy it.
    getSelection().selectAllChildren(el);
    announce("Copy failed. " + text + " is selected — press Control C.");
  }
}

/// Marks every inline code span copyable once this script runs; without it the
/// spans stay plain text.
///
/// The spans stay out of the tab order. On `std/http` they were 143 of 222 tab
/// stops. A keyboard reader selects the text and presses Ctrl+C, and every
/// command worth copying whole sits in a `.cmd` plate with its own Copy button.
function markCopyable() {
  for (const el of $$("code")) {
    if (!inlineCode(el)) continue;
    el.classList.add("copyable");
  }
}

// Where the pointer went down on a code span, read by the click that follows.
let copyDownAt = null;

// Bound once per document: `boot` reruns after every soft navigation, and a
// listener added there would stack.
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

/// Checks the OS radio that matches the visitor.
///
/// The picker works without script: two radios and a `:checked ~` selector,
/// the first checked by default. This only guesses which one to open on. The
/// choice is PowerShell or a POSIX shell, so anything that is not Windows takes
/// the first tab and a wrong guess costs one press. `navigator.platform` is
/// deprecated and frozen, so `userAgentData.platform` is asked first.
function guessOs(root) {
  const ua = navigator.userAgentData?.platform || navigator.platform || navigator.userAgent || "";
  const radio = $(`input[data-os="${/win/i.test(ua) ? "windows" : "unix"}"]`, root);
  if (radio) radio.checked = true;
}

// The tab groups (`[data-tabs]`).
//
// The markup ships plain buttons with every pane shown, which is the page
// without script. The ARIA roles are installed here, by the code that makes
// them true: a tablist, ids pairing each tab with its panel, `aria-selected`,
// a roving tabindex, and Left/Right/Home/End between the tabs.

/// Numbers tab groups, so the ids stay unique in a document.
let tabGroups = 0;

/// Whether `el` already contains something a keyboard can reach.
function hasFocusable(el) {
  return Boolean($("a[href], button, input, select, textarea, [tabindex]", el));
}

function tabsWidget(root) {
  const tabAttr = "data-tab";
  const paneAttr = "data-pane";
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
    // A panel whose whole content is a sideways-scrolling code block has
    // nothing to focus, so the panel takes the tab stop.
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

  select(keyOf(tabs[0], tabAttr), false);
}

// The benchmark radar. Vyrn computed every coordinate; this toggles a series
// and opens a program's block.
//
// Every axis name and wedge is an `<a>` to `#<program>`, so with no script it
// jumps to that block. With the blocks as a tab group, the click presses the
// matching tab and the browser's jump lands on a shown pane. Nothing animates.

function radarWidget(root) {
  const svg = $("svg.radar", root);
  if (!svg) return;

  const tabs = $$("[data-tab]");
  for (const link of $$(".names a, .wedges a", svg)) {
    link.addEventListener("click", () => {
      const key = (link.getAttribute("href") || "").slice(1);
      const tab = tabs.find((t) => t.getAttribute("data-tab") === key);
      if (tab) tab.click();
    });
  }

  for (const btn of $$("[data-series-toggle]", root)) {
    const group = $('g[data-series="' + btn.dataset.seriesToggle + '"]', svg);
    if (!group) continue;
    btn.addEventListener("click", () => {
      const on = btn.getAttribute("aria-pressed") !== "true";
      btn.setAttribute("aria-pressed", String(on));
      group.classList.toggle("off", !on);
    });
  }
}

// The search on the reference and the registry. Each row carries its own
// lowercased haystack in `data-q` and its export names in `data-e`, written by
// the export, so there is no index to fetch; filtering sets `hidden` on a row.
// Nodes are built with `createElement` and `textContent`, so typed text never
// becomes markup.

/// The export names of `row` that contain `q`, at most `limit` of them.
function matchingExports(row, q, limit) {
  const names = (row.dataset.e || "").split(" ").filter(Boolean);
  const hits = names.filter((n) => n.toLowerCase().includes(q));
  return hits.slice(0, limit);
}

/// A row's matched exports: links to each export's anchor where the row names a
/// module (`data-m`, the reference), plain chips where it does not (a registry
/// package page has no per-export anchors).
function exportHits(row, names) {
  const box = document.createElement("span");
  box.className = "hits";
  for (const name of names) {
    let chip;
    if (row.dataset.m) {
      chip = document.createElement("a");
      chip.href = new URL("docs/std/" + row.dataset.m + ".html#e-" + name, SITE).href;
    } else {
      chip = document.createElement("span");
    }
    chip.textContent = name;
    box.appendChild(chip);
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
  // What one row is called ("module" or "package"), and whether any row
  // carries export names to count.
  const noun = count?.dataset.searchNoun || "module";
  const hasExports = rows.some((r) => r.dataset.e);

  const apply = () => {
    // `kind:<k>` narrows by a row's kind. The rest matches the haystack or an
    // export name, so a search for a name finds the row that offers it.
    const terms = input.value.trim().toLowerCase().split(/\s+/).filter(Boolean);
    const kinds = terms.filter((t) => t.startsWith("kind:")).map((t) => t.slice(5)).filter(Boolean);
    const q = terms.filter((t) => !t.startsWith("kind:")).join(" ");
    let shown = 0;
    let hits = 0;
    for (const row of rows) {
      const old = $(".hits", row);
      if (old) old.remove();
      const kindOk = kinds.length === 0 || kinds.includes((row.dataset.kind || "").toLowerCase());
      const names = q.length === 0 ? [] : matchingExports(row, q, 10);
      const match = kindOk && (q.length === 0 || (row.dataset.q || "").includes(q) || names.length > 0);
      row.hidden = !match;
      if (!match) continue;
      shown += 1;
      if (q.length === 0) continue;
      // Ten is enough to show WHICH names matched; the row's own page has them all.
      hits += names.length;
      if (names.length) row.appendChild(exportHits(row, names));
    }
    if (!count) return;
    if (terms.length === 0) {
      count.textContent = total + " " + noun + "s";
    } else if (shown === 0) {
      count.textContent = "no " + noun + " matches " + input.value.trim();
    } else {
      // Only the reference's rows carry export names.
      const tail = hasExports && hits > 0 ? ", " + hits + (hits === 1 ? " export" : " exports") : "";
      count.textContent = shown + (shown === 1 ? " " + noun : " " + noun + "s") + tail;
    }
  };

  input.addEventListener("input", apply);
  // A value already in the field (restored form state, a back navigation) is
  // applied.
  if (input.value) apply();
}

// The reference's import graph. Vyrn laid out every node and wire; this lights
// one module's imports and importers on hover and on focus, because the nodes
// are links a keyboard reaches.

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

// The rail marks the section in view. Smooth scrolling is the stylesheet's
// `scroll-behavior`.

/// The rail's in-page links, each paired with its section. A soft navigation
/// replaces the rail, so this is refilled per page.
let railLinks = [];

function markRail() {
  // The active section is the last one whose top has passed a quarter of the
  // viewport; before any has, the first link is.
  let current = railLinks.length ? railLinks[0] : null;
  for (const pair of railLinks) {
    if (pair.section.getBoundingClientRect().top <= innerHeight * 0.25) current = pair;
  }
  // The last section is usually too short to reach that line before the page
  // runs out of scroll, so at the bottom it is the active one.
  if (railLinks.length && innerHeight + scrollY >= document.documentElement.scrollHeight - 2) {
    current = railLinks[railLinks.length - 1];
  }
  for (const pair of railLinks) pair.link.classList.toggle("on", pair === current);
  if (current) keepInRail(current.link);
}

/// Keeps the marked link visible in a rail long enough to scroll (a reference
/// module has up to 44 exports). Sets the rail's `scrollTop` directly, because
/// `scrollIntoView` can scroll the page and fight the scroll event that called
/// this.
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
  // The docs shell's right pane gets the same marker as the margin rail.
  const rail = $(".rail") || $(".onpage");
  if (!rail) return;
  for (const link of $$('a[href^="#"]', rail)) {
    const section = document.getElementById(decodeURIComponent(link.getAttribute("href").slice(1)));
    if (section) railLinks.push({ section, link });
  }
  markRail();
}

// Bound once per document, like the copy listeners above.
addEventListener("scroll", markRail, { passive: true });

// A `<details>` menu stays open until its summary is pressed again; a click
// elsewhere or Escape closes it here. Bound once.
addEventListener("click", (e) => {
  for (const m of $$(".pagemenu[open]")) {
    if (!m.contains(e.target)) m.open = false;
  }
});
addEventListener("keydown", (e) => {
  if (e.key !== "Escape") return;
  for (const m of $$(".pagemenu[open]")) m.open = false;
});
addEventListener("resize", markRail, { passive: true });

// The masthead marker. The export stamps `aria-current="page"` on the row this
// document belongs to; a soft navigation replaces `<main>` and not the
// masthead, so the marker is moved here.

/// Whether the navigation row `href` is the row for `path`.
///
/// A row owns the subtree named after it. The Docs row (`/guide.html`) also
/// owns `/docs`, `/web` and `/tooling`, as the export's `currentNav` rule says.
/// The areas join the row's own stem, so any mount point works.
function navOwns(href, path) {
  const stem = href.replace(/\.html$/, "");
  if (path === href || path.startsWith(stem + "/")) return true;
  if (stem.endsWith("/guide")) {
    const root = stem.slice(0, -"/guide".length);
    for (const area of ["/docs", "/web", "/tooling"]) {
      if (path === root + area + ".html" || path.startsWith(root + area + "/")) return true;
    }
  }
  return false;
}

/// Resolves the masthead's and footer's links against the document that
/// shipped them.
///
/// A soft navigation replaces `<main>` only, so the shell outlives the URL its
/// relative links were written against: after a move from `/docs.html` to
/// `/docs/std/json.html`, `docs.html` would mean `/docs/std/docs.html`. The
/// resolved URL is same-origin, so a click is still soft-navigated. Fragment
/// links stay relative: `#main` means this page.
function anchorShell() {
  for (const a of $$(".masthead a[href], .foot a[href]")) {
    if (!a.getAttribute("href").startsWith("#")) a.setAttribute("href", a.href);
  }
}

function markNav() {
  const path = location.pathname;
  for (const link of $$(".masthead .nav a")) {
    // Resolved, because the attribute is relative to its document
    // (`../docs.html` on a chapter page) and never equals a pathname.
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

// The hero editor, armed on first interaction.
//
// The index's editor is the playground on a smaller root (`mountPlay`, the
// same hooks and keys). Mounting on load would fetch the compiler on a page
// whose reader has not asked for it, so the document ships a coloured code
// block over a read-only textarea. Pointing at it, focusing it or pressing Run
// loads the playground, and a Run press is replayed once the mount is ready.
// Without script the template's `<noscript>` links to `/play`.

function armHeroEditor(root) {
  const runBtn = $("[data-play-run]", root);
  const src = $("[data-play-src]", root);
  const status = $("[data-play-status]", root);
  const resetBtn = $("[data-play-reset]", root);
  const outPane = $("[data-play-out]", root);
  if (!runBtn || !src) return;
  let armed = false;
  // `armed`: the module is loading or loaded. `runOnReady`: a Run press is
  // owed a run. Hover and focus arm without a press, so these are two flags.
  let runOnReady = false;

  const arm = (thenRun) => {
    runOnReady = runOnReady || thenRun;
    if (armed) return;
    armed = true;
    // Editable once the reader asks for it, and not before: nothing could
    // check or run what they type.
    src.removeAttribute("readonly");
    if (status) status.textContent = "Loading the compiler…";
    import("./play.js").then(
      ({ mountPlay }) => {
        if (!root.isConnected) return;
        mountPlay(root, { onReady: () => runOnReady && runBtn.click() });
      },
      (err) => {
        // Fail the way `mountPlay` does: say so in the output pane, make the
        // textarea read-only and disable both controls.
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

  // `pointerenter` covers a mouse and touch, `focusin` a keyboard; both fire
  // once.
  root.addEventListener("pointerenter", () => arm(false), { once: true });
  root.addEventListener("focusin", () => arm(false), { once: true });
  // `Reset` before the mount arms it; `mountPlay` binds the real handler.
  if (resetBtn) resetBtn.addEventListener("click", () => arm(false));
  // Bound before `mountPlay` adds its own handler, so the first press arms and
  // the replayed press runs. `mountPlay` ignores a press while the module
  // loads, which is why readiness drives the replay.
  runBtn.addEventListener("click", () => arm(true));
}

// The recorded demo: seven steps of real output from `scripts/site-demo.py`,
// all in the document. Without script they are a numbered list; with script
// they show one step at a time. The controls are built here, because a Next
// button on a page that shows every step does nothing.

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
  const replay = document.createElement("button");
  replay.type = "button";
  replay.className = "replay";
  replay.textContent = "replay";
  const next = document.createElement("button");
  next.type = "button";
  next.textContent = "›";
  next.setAttribute("aria-label", "Next");
  bar.append(back, replay, next);
  // The controls belong to the terminal they page, under its own window.
  ($(".term", root) || root).append(bar);
  // The step counter sits on the window bar; polite, because a press changes
  // it.
  const count = $("[data-demo-count]", root) || document.createElement("span");
  count.setAttribute("aria-live", "polite");

  // Every row stays on the page; `.stepped` tells the sheet a script drives
  // it. Without script no step is `.on` and every pane is open. The nth row
  // and the nth pane are marked together (the guard above requires one pane
  // per step), and `aria-current` on the row tells a screen reader which step
  // is shown.
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
    count.textContent = pad(at + 1) + " / " + pad(steps.length);
    // The bar under the marked step is its timer, restarted per step.
    const tick = $(".tick", steps[at]);
    if (tick) {
      tick.style.animation = "none";
      void tick.offsetWidth;
      tick.style.animation = "";
    }
    back.disabled = at === 0;
  };

  /// Pads to two digits, so the counter keeps its width.
  function pad(n) {
    return n < 10 ? "0" + n : String(n);
  }

  // It walks itself. The pointer over the card pauses it, and any press hands
  // the walk to the reader. A step turns when its own bar's animation ends, so
  // the bar is the only clock and `animation-play-state` pauses it exactly.
  let walking = !matchMedia("(prefers-reduced-motion: reduce)").matches;
  let paused = false;
  for (const step of steps) {
    const tick = $(".tick", step);
    if (!tick) continue;
    tick.addEventListener("animationend", () => {
      if (walking && !paused && step.classList.contains("on")) show(at + 1);
    });
  }
  root.addEventListener("pointerenter", () => {
    paused = true;
    root.classList.add("paused");
  });
  root.addEventListener("pointerleave", () => {
    paused = false;
    root.classList.remove("paused");
    // A bar that finished while the pointer rested on it turns its step now.
    const tick = $(".tick", steps[at]);
    if (walking && tick && tick.getAnimations && tick.getAnimations().length === 0) show(at + 1);
  });
  function stopWalking() {
    walking = false;
    root.classList.add("held");
  }
  // Pointer only: Back, Next and the arrow keys reach every step already.
  steps.forEach((s, k) => s.addEventListener("click", () => {
    stopWalking();
    show(k);
  }));
  back.addEventListener("click", () => {
    stopWalking();
    show(at - 1);
  });
  next.addEventListener("click", () => {
    stopWalking();
    show(at + 1);
  });
  replay.addEventListener("click", () => {
    walking = true;
    root.classList.remove("held");
    show(0);
  });
  root.addEventListener("keydown", (e) => {
    if (e.key === "ArrowRight") return e.preventDefault(), stopWalking(), show(at + 1);
    if (e.key === "ArrowLeft") return e.preventDefault(), stopWalking(), show(at - 1);
  });
  show(0);
}

/// Applies the playground's title-bar masthead one frame after the first
/// paint, so the row animates into it. Any other page removes the class, so a
/// soft navigation away animates too.
function markEditorChrome() {
  const editor = Boolean($(".page.play.ide"));
  const root = document.documentElement;
  // What the next page starts as: the inline boot script in every page's head
  // (`withThemeBoot` in `site/export.vyrn`) reads it before the first paint.
  try {
    sessionStorage.setItem("vyrn.mast", editor ? "compact" : "full");
  } catch (err) {
    /* a blocked store only costs the handoff */
  }
  // The test suite's document has no root element.
  const settle = () => root && root.classList.toggle("idecompact", editor);
  // A hidden tab runs no animation frames, and neither does the test suite's
  // minimal document, so there the class is set at once.
  if (document.visibilityState === "hidden" || typeof requestAnimationFrame !== "function") settle();
  else requestAnimationFrame(() => requestAnimationFrame(settle));
}

function boot() {
  markEditorChrome();
  copyButtons();
  copyPageButtons();
  markCopyable();
  entrance();
  anchorShell();
  markNav();
  railSpy();
  for (const el of $$('[data-widget="demo"]')) demoWidget(el);
  for (const el of $$("[data-tabs]")) tabsWidget(el);
  for (const el of $$('[data-widget="radar"]')) radarWidget(el);
  for (const el of $$(".picker")) guessOs(el);
  moduleSearch();
  // The playground, on the page that has one. The import is not awaited; a
  // rejection is logged, because a playground that never mounts must say so.
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

// The search overlay. `/` opens it on every page, Esc closes it, the arrows
// walk the results and Enter follows one. The index (`site/app/search.vyrn`)
// is one file, fetched on the first open and never inlined in a document.
//
// Bound once, outside `boot()`: the overlay lives in the shell, which a soft
// navigation never replaces.
{
  const box = $("[data-find]");
  const field = $("[data-find-input]");
  const list = $("[data-find-results]");
  const note = $("[data-find-note]");
  // A page that wears its own shell has no overlay in it.
  if (box && field && list) {
    let index = null;
    let loading = null;
    let cursor = -1;
    // The section filter; "" is All. The chips under the field set it and Tab
    // cycles it, since a dialog with one field has no other use for Tab.
    let sect = "";
    const tabs = $$("[data-find-tabs] [data-sect]", box);
    // The note's resting markup. Its suggestions are pressable, and `render`
    // replaces the note's content, so presses are delegated to the note.
    const idle = note.innerHTML;
    // Where focus was when the overlay opened, so Esc gives it back.
    let cameFrom = null;

    /// Fetches the index once. A failure says so in the panel, where an empty
    /// list would read as "nothing matches".
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

    // The sections in display order, which is the order `search.vyrn` writes.
    const SECTIONS = ["Docs", "Reference", "Packages", "Releases"];

    /// The best 40 rows matching `q`, grouped by section.
    ///
    /// A substring match over the title and the description, ranked: a title
    /// that starts with the query, then any title hit, then a description hit.
    /// The rank picks which 40 rows show, so it runs over every row; the section
    /// order then groups those 40. Both sorts are stable, so rows keep their
    /// rank inside a section.
    function search(q) {
      const needle = q.toLowerCase();
      const hits = [];
      for (const r of index) {
        if (sect && r.s !== sect) continue;
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

    /// The fragment `r` leads to: `#e-<name>` for an export row, whose title is
    /// the name, an em dash and `std/<module>`, and nothing for every other row.
    /// A module's own row (`std/json`) has no dash, which keeps the two apart.
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
        if (!q) note.innerHTML = idle;
        return;
      }
      const hits = search(q);
      note.textContent = hits.length
        ? `${hits.length} result${hits.length === 1 ? "" : "s"}.`
        : sect
          ? `Nothing in ${sect} matches "${q}".`
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
        // The index carries the route (`/docs.html`) and the mount point is
        // unknown, so the URL resolves against this script's directory, as the
        // export resolves its own. The export anchor is derived rather than
        // stored, which keeps the index inside its gzip ceiling
        // (`site/test/search.test.mjs`).
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

    /// Moves the selection by `step`, wrapping, and keeps it on screen. Focus
    /// stays in the field and the row is marked `aria-selected` (the listbox
    /// pattern), so the reader can keep typing.
    function move(step) {
      const hits = $$(".hit", list);
      if (!hits.length) return;
      if (cursor >= 0) hits[cursor].setAttribute("aria-selected", "false");
      cursor = (cursor + step + hits.length) % hits.length;
      hits[cursor].setAttribute("aria-selected", "true");
      hits[cursor].scrollIntoView({ block: "nearest" });
    }

    /// Points the filter at `v` and redraws. Focus returns to the field: a
    /// chip narrows the search and is not a place to stay.
    function setSect(v) {
      sect = v;
      for (const t of tabs) t.classList.toggle("on", t.dataset.sect === v);
      render(field.value);
      field.focus();
    }
    for (const t of tabs) t.addEventListener("click", () => setSect(t.dataset.sect));

    // A pressed suggestion becomes the query.
    note.addEventListener("click", (e) => {
      const b = e.target.closest("[data-try]");
      if (!b) return;
      field.value = b.textContent;
      // Back to All: a filter left from an earlier search could hide every
      // suggestion.
      setSect("");
    });

    function open() {
      if (!box.hidden) return;
      cameFrom = document.activeElement;
      box.hidden = false;
      field.value = "";
      setSect("");
      field.focus();
      load().then(() => render(field.value));
    }

    function close() {
      if (box.hidden) return;
      box.hidden = true;
      list.textContent = "";
      // A soft navigation may have replaced that element.
      if (cameFrom && cameFrom.isConnected) cameFrom.focus();
      cameFrom = null;
    }

    for (const btn of $$("[data-find-open]")) btn.addEventListener("click", open);

    // `/` from anywhere, unless the reader is typing in a field.
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

    // A press on the backdrop closes it; a press on the panel does not.
    box.addEventListener("mousedown", (e) => {
      if (e.target === box) close();
    });

    field.addEventListener("input", () => render(field.value));
    field.addEventListener("keydown", (e) => {
      if (e.key === "Tab") {
        // Tab and Shift+Tab walk the section chips.
        e.preventDefault();
        const order = tabs.map((t) => t.dataset.sect);
        const at = order.indexOf(sect);
        const step = e.shiftKey ? -1 : 1;
        setSect(order[(at + step + order.length) % order.length]);
      } else if (e.key === "ArrowDown") {
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
          // Closed before the navigation, without restoring focus: that
          // element is about to be replaced.
          box.hidden = true;
          list.textContent = "";
          cameFrom = null;
          pick.click();
        }
      }
    });

    // The focus trap: while the overlay is open, Tab returns to the field, so
    // nothing behind the `aria-modal` dialog is reachable.
    box.addEventListener("keydown", (e) => {
      if (e.key === "Tab") {
        e.preventDefault();
        field.focus();
      }
    });
  }
}

/// Restores the reader's place after a soft navigation.
///
/// `vyrn-nav.js` swaps `<main>` and scrolls to the top, and the clicked link
/// leaves with the old `<main>`, so focus falls to `<body>` and a screen reader
/// hears nothing. Focus moves to `#main`, the skip link's target just before
/// the content, so the next Tab reaches the new page's first link. This lives
/// here and not in `vyrn-nav.js`, which other applications share.
function landed() {
  boot();
  const main = document.getElementById("main");
  if (main) main.focus({ preventScroll: true });
  // A real navigation reads the title out; a soft one does not.
  announce(document.title);
}

// `vyrn-nav.js` fires this after it swaps the DOM; every widget on the new
// page needs booting.
document.addEventListener("vyrn:nav-end", landed);

// The navigator loads the first time a reader points at, touches or tabs to a
// link: before the click that uses it, after the paint. A hard navigation
// works until it lands.
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
