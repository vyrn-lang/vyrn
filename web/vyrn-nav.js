// vyrn-nav.js — soft navigation for Vyrn's MPA pages (RFC-0034).
//
// Plain, dependency-free JavaScript, a sibling of vyrn-dom.js. It gives an
// ordinary server-rendered site (std/ui pages) an SPA *feel* without giving up
// the MPA *truth*: every navigation is still a real server render — only the
// page transition goes soft. No flash, no lost scroll, no dropped client state.
//
// The lineage is Turbolinks / htmx: intercept same-origin <a> clicks, fetch the
// destination, and MORPH the live document into the fetched one instead of
// letting the browser blow the page away and rebuild it.
//
// WHERE THE MORPH LIVES — and why here, not in vyrn-dom.js:
//   vyrn-dom.js is the *tree↔DOM* differ: it reconciles a parsed vnode tree
//   (the wasm app's `vyrnView()` JSON) against retained vnodes. The soft-nav
//   morph is a *DOM↔DOM* differ: it reconciles one real DOM subtree against
//   another real one (a freshly parsed `<body>`), with no vnodes anywhere. The
//   two share a discipline (keyed identity via `data-key`, positional
//   otherwise, attribute patching, focus/form preservation on reused nodes) but
//   not a data model. Keeping the DOM↔DOM morph HERE lets vyrn-nav.js stay a
//   self-contained sibling that imports nothing, and keeps vyrn-dom.js focused
//   on the vnode path. See `morphChildren` / `morphNode` below.
//
// FALLBACK BIAS (locked): soft nav is an optimization, never a correctness
// layer. Any ambiguity — a non-HTML response, a fetch failure or timeout, a
// second click while one is in flight, a morph that throws — resolves to a
// plain hard navigation. The only responses we morph are HTML documents (any
// status, so a 404/422 error PAGE stays reachable AND flash-free); everything
// else hands off to the browser.
//
// ISLANDS: a page may mount a wasm client app (the `#app` convention). Because
// morphed-in <script> tags are NOT re-executed, the client boot cannot ride the
// page's own <script> across a soft nav. Instead the boot code registers an
// island with `window.vyrnNav.registerIsland(selector, boot)`; vyrn-nav owns
// its lifecycle — boot on first appearance, tear down + re-boot on every real
// navigation (page identity changed → fresh state, exactly like a hard load),
// and leave it alone on same-page revalidation morphs.
//
// Progressive enhancement: if this file is absent, nothing here ran, every <a>
// is a normal link, and the app boot falls back to booting directly. `data-nav`
// attributes are inert hints with no soft-nav present.

const CONFIG = Object.assign(
  {
    timeoutMs: 10000, // fetch guard: past this a soft nav falls back hard
    prefetchStaleMs: 10000, // a prefetched page is served without refetch this long
    prefetchMax: 8, // LRU capacity for the prefetch cache
    progress: true, // built-in top progress bar (rides the nav events)
  },
  (typeof window !== "undefined" && window.__vyrnNavConfig) || {}
);

// ---------------------------------------------------------------------------
// Events. Consumers (and the built-in progress bar) hook these.
// ---------------------------------------------------------------------------
function emit(name, detail) {
  document.dispatchEvent(new CustomEvent("vyrn:" + name, { detail: detail || {} }));
}

// ---------------------------------------------------------------------------
// Island registry. `boot(el)` returns an instance (optionally a Promise); a
// truthy instance with a `destroy()` is torn down on the next real navigation.
// ---------------------------------------------------------------------------
const islands = []; // { selector, boot, instance }

function bootIsland(reg) {
  const el = document.querySelector(reg.selector);
  reg.instance = el ? reg.boot(el) || null : null;
}

function teardownIslands() {
  for (const reg of islands) {
    const inst = reg.instance;
    reg.instance = null;
    if (inst) {
      // boot may be async; destroy once it settles.
      Promise.resolve(inst).then((v) => {
        if (v && typeof v.destroy === "function") {
          try {
            v.destroy();
          } catch (e) {
            /* a failing teardown must not wedge navigation */
          }
        }
      });
    }
  }
}

function rebootIslands() {
  teardownIslands();
  for (const reg of islands) bootIsland(reg);
}

// ---------------------------------------------------------------------------
// The DOM↔DOM morph. Mirrors vyrn-dom.js's keyed discipline on real nodes.
// ---------------------------------------------------------------------------

// vyrn-nav's own chrome (the progress bar, any consumer UI marked likewise) is
// never touched by a body morph.
function isNavUi(node) {
  return node.nodeType === 1 && node.hasAttribute("data-vyrn-nav-ui");
}

function keyOf(node) {
  return node.nodeType === 1 ? node.getAttribute("data-key") : null;
}

// Same slot? Text/comment match by node type; elements additionally by tag.
function compatible(a, b) {
  if (a.nodeType !== b.nodeType) return false;
  if (a.nodeType === 1) return a.tagName === b.tagName;
  return true;
}

function morphChildren(oldParent, newParent) {
  const oldKids = [];
  for (const n of oldParent.childNodes) if (!isNavUi(n)) oldKids.push(n);
  const newKids = [];
  for (const n of newParent.childNodes) if (!isNavUi(n)) newKids.push(n);

  const keyed = oldKids.some((n) => keyOf(n) != null) || newKids.some((n) => keyOf(n) != null);
  if (keyed) return morphKeyed(oldParent, oldKids, newKids);

  // positional: patch the common prefix, drop the tail, append the new tail.
  const common = Math.min(oldKids.length, newKids.length);
  for (let i = 0; i < common; i++) morphNode(oldParent, oldKids[i], newKids[i]);
  for (let i = oldKids.length - 1; i >= common; i--) oldParent.removeChild(oldKids[i]);
  for (let i = common; i < newKids.length; i++) oldParent.appendChild(document.importNode(newKids[i], true));
}

// Keyed reconciliation: reuse the node for a matching key (so focus / caret /
// typed value survive a reorder), create new keys, drop gone keys, then order
// the parent's children to match the new sequence — the same shape as
// vyrn-dom.js's `patchKeyed`, on real DOM nodes.
function morphKeyed(parent, oldKids, newKids) {
  const oldByKey = new Map();
  for (const n of oldKids) {
    const k = keyOf(n);
    if (k != null) oldByKey.set(k, n);
  }
  const result = [];
  for (const nn of newKids) {
    const k = keyOf(nn);
    const on = k != null ? oldByKey.get(k) : null;
    if (on && compatible(on, nn)) {
      morphNode(parent, on, nn); // morphs `on` in place; identity preserved
      oldByKey.delete(k);
      result.push(on);
    } else {
      result.push(document.importNode(nn, true));
    }
  }
  const keep = new Set(result);
  for (const n of oldKids) {
    if (!keep.has(n) && n.parentNode === parent) parent.removeChild(n);
  }
  // insertBefore also MOVES an existing node, so reused DOM is relocated.
  for (let i = 0; i < result.length; i++) {
    const ref = childAt(parent, i);
    if (ref !== result[i]) parent.insertBefore(result[i], ref);
  }
}

// The i-th non-nav-ui child (so ordering math ignores the protected chrome).
function childAt(parent, i) {
  let n = 0;
  for (const c of parent.childNodes) {
    if (isNavUi(c)) continue;
    if (n === i) return c;
    n++;
  }
  return null;
}

function morphNode(parent, oldNode, newNode) {
  if (!compatible(oldNode, newNode)) {
    parent.replaceChild(document.importNode(newNode, true), oldNode);
    return;
  }
  if (oldNode.nodeType === 3 || oldNode.nodeType === 8) {
    // text / comment
    if (oldNode.nodeValue !== newNode.nodeValue) oldNode.nodeValue = newNode.nodeValue;
    return;
  }
  // element: same node reused → attribute patch + recurse. Because the node
  // itself is reused, document.activeElement and any typed-in `.value` survive
  // for free wherever identity holds (keyed reorders, and every unchanged slot).
  morphAttrs(oldNode, newNode);
  morphChildren(oldNode, newNode);
}

function morphAttrs(oldEl, newEl) {
  for (const attr of Array.from(oldEl.attributes)) {
    if (!newEl.hasAttribute(attr.name)) oldEl.removeAttribute(attr.name);
  }
  for (const attr of Array.from(newEl.attributes)) {
    if (oldEl.getAttribute(attr.name) !== attr.value) oldEl.setAttribute(attr.name, attr.value);
  }
  // Form controls carry LIVE properties (`value`/`checked`) that setAttribute
  // does not touch. For a field the user is NOT in, sync the live value to the
  // server's; for the FOCUSED field, leave the typed value alone (preserve it).
  const tag = oldEl.tagName;
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") {
    if (document.activeElement !== oldEl) {
      if (newEl.hasAttribute("value")) {
        const v = newEl.getAttribute("value");
        if (oldEl.value !== v) oldEl.value = v;
      }
      if (tag === "INPUT" && (oldEl.type === "checkbox" || oldEl.type === "radio")) {
        oldEl.checked = newEl.hasAttribute("checked");
      }
    }
  }
}

// ---------------------------------------------------------------------------
// Head: swap the title, append new stylesheets (never remove — additive, like a
// real navigation that keeps the page paint stable while new sheets arrive).
// ---------------------------------------------------------------------------
function morphHead(newDoc) {
  const newTitle = newDoc.querySelector("title");
  if (newTitle) document.title = newTitle.textContent;

  const have = new Set();
  for (const l of document.head.querySelectorAll('link[rel="stylesheet"]')) {
    have.add("href:" + l.getAttribute("href"));
  }
  for (const s of document.head.querySelectorAll("style")) have.add("style:" + s.textContent);

  for (const l of newDoc.head.querySelectorAll('link[rel="stylesheet"]')) {
    if (!have.has("href:" + l.getAttribute("href"))) {
      document.head.appendChild(document.importNode(l, true));
    }
  }
  for (const s of newDoc.head.querySelectorAll("style")) {
    if (!have.has("style:" + s.textContent)) document.head.appendChild(document.importNode(s, true));
  }
}

// Load any module <script src> the new document has that we don't. ES modules
// evaluate once per URL, so re-appending an already-loaded src is a no-op; a
// genuinely new src (e.g. the app boot on a page reached mid-session) loads and
// its top-level runs — which is exactly where island registration happens.
function syncScripts(newDoc) {
  const have = new Set();
  for (const s of document.querySelectorAll("script[src]")) {
    have.add(new URL(s.getAttribute("src"), location.href).href);
  }
  for (const s of newDoc.querySelectorAll('script[type="module"][src]')) {
    const abs = new URL(s.getAttribute("src"), location.href).href;
    if (!have.has(abs)) {
      const el = document.createElement("script");
      el.type = "module";
      el.src = s.getAttribute("src");
      document.body.appendChild(el);
      have.add(abs);
    }
  }
}

// ---------------------------------------------------------------------------
// Apply a parsed document to the live one. `reboot` distinguishes a real
// navigation (tear down + re-boot islands: page identity changed) from a
// same-page revalidation morph (leave islands running).
// ---------------------------------------------------------------------------
function applyDocument(newDoc, { reboot }) {
  const active = document.activeElement;
  const activeId = active && active.id ? active.id : null;

  morphHead(newDoc);
  morphChildren(document.body, newDoc.body);
  syncScripts(newDoc);

  // Restore focus if the focused element was replaced but a same-id one remains
  // (identity by id across pages, e.g. a persistent search box).
  if (activeId && document.activeElement !== active) {
    const again = document.getElementById(activeId);
    if (again && typeof again.focus === "function") again.focus();
  }

  if (reboot) rebootIslands();
}

// ---------------------------------------------------------------------------
// Prefetch cache — a tiny LRU with a staleTime, borrowing vyrn-query.js's
// conventions. Stores the raw HTML (to detect a changed page on revalidate) and
// the parsed document (to morph from instantly).
// ---------------------------------------------------------------------------
const prefetchCache = new Map(); // url -> { ts, html, doc }

function cacheGet(url) {
  const e = prefetchCache.get(url);
  if (!e) return null;
  prefetchCache.delete(url);
  prefetchCache.set(url, e); // move to MRU
  return e;
}

function cacheSet(url, html) {
  const doc = new DOMParser().parseFromString(html, "text/html");
  prefetchCache.set(url, { ts: Date.now(), html, doc });
  while (prefetchCache.size > CONFIG.prefetchMax) {
    prefetchCache.delete(prefetchCache.keys().next().value); // evict LRU
  }
}

function prefetch(url) {
  const e = prefetchCache.get(url);
  if (e && Date.now() - e.ts < CONFIG.prefetchStaleMs) return; // still fresh
  fetch(url, { headers: { "x-vyrn-nav": "soft" } })
    .then((res) => {
      const ct = res.headers.get("content-type") || "";
      if (res.ok && ct.includes("text/html")) return res.text().then((html) => cacheSet(url, html));
    })
    .catch(() => {
      /* a failed prefetch is silent; the click will just fetch for real */
    });
}

// ---------------------------------------------------------------------------
// Navigation. One in-flight guard: a second click while a soft nav is running
// falls back to a hard navigation (fallback bias).
// ---------------------------------------------------------------------------
let inflight = null;

function hardNav(url) {
  window.location.assign(url);
}

// Persist the current scroll offset into this history entry so back/forward can
// restore it. Throttled to once per frame.
let scrollScheduled = false;
function saveScroll() {
  if (scrollScheduled) return;
  scrollScheduled = true;
  requestAnimationFrame(() => {
    scrollScheduled = false;
    const st = Object.assign({}, history.state, { vyrnNav: true, scrollY: window.scrollY });
    try {
      history.replaceState(st, "");
    } catch (e) {
      /* ignore */
    }
  });
}

async function navigate(url, { push }) {
  if (inflight) {
    hardNav(url); // mid-flight second click → hard nav
    return;
  }

  // Instant path: a prefetched page morphs from cache immediately, then
  // revalidates in the background (a same-page morph, so islands are untouched).
  const cached = push ? cacheGet(url) : null;
  if (cached) {
    emit("nav-start", { url, cached: true });
    try {
      pushEntry(url);
      applyDocument(cached.doc, { reboot: true });
      window.scrollTo(0, 0);
      emit("nav-end", { url, cached: true });
    } catch (e) {
      hardNav(url);
      return;
    }
    revalidate(url, cached.html);
    return;
  }

  emit("nav-start", { url });
  const controller = new AbortController();
  inflight = controller;
  const timer = setTimeout(() => controller.abort(), CONFIG.timeoutMs);

  try {
    const res = await fetch(url, { headers: { "x-vyrn-nav": "soft" }, signal: controller.signal });
    clearTimeout(timer);
    const ct = res.headers.get("content-type") || "";
    // Only HTML is morphable. Non-HTML (a download, a redirect to JSON, …) →
    // hard nav. An HTML error page (404/422) IS morphed, so it stays reachable
    // and flash-free.
    if (!ct.includes("text/html")) {
      inflight = null;
      emit("nav-error", { url, reason: "non-html" });
      hardNav(url);
      return;
    }
    const html = await res.text();
    const doc = new DOMParser().parseFromString(html, "text/html");
    inflight = null;
    if (push) pushEntry(url);
    applyDocument(doc, { reboot: true });
    if (push) window.scrollTo(0, 0);
    else restorePopScroll();
    emit("nav-end", { url });
  } catch (err) {
    clearTimeout(timer);
    inflight = null;
    emit("nav-error", { url, reason: "fetch-failed" });
    hardNav(url); // network failure / timeout / abort → hard nav
  }
}

// Background revalidation after an instant cache morph: refetch and, if the page
// changed, morph again WITHOUT rebooting islands (same page, not a navigation).
function revalidate(url, oldHtml) {
  fetch(url, { headers: { "x-vyrn-nav": "soft" } })
    .then((res) => {
      const ct = res.headers.get("content-type") || "";
      if (!res.ok || !ct.includes("text/html")) return;
      return res.text().then((html) => {
        cacheSet(url, html);
        if (html !== oldHtml && location.href === url) {
          const doc = new DOMParser().parseFromString(html, "text/html");
          applyDocument(doc, { reboot: false });
        }
      });
    })
    .catch(() => {});
}

function pushEntry(url) {
  // Synchronously stamp the leaving entry with its scroll offset BEFORE pushing
  // the new one (the throttled scroll listener may not have run yet, and a late
  // frame must never write this offset into the new entry).
  history.replaceState(Object.assign({}, history.state, { vyrnNav: true, scrollY: window.scrollY }), "");
  history.pushState({ vyrnNav: true, scrollY: 0 }, "", url);
}

let pendingPopScroll = 0;
// Restore the saved offset — then re-apply it on a short, bounded schedule. An
// island re-boot (or any async content) can briefly collapse page height right
// after the morph, clamping the scroll; re-applying for ~½s lets the target
// stick once the content grows back. Harmless once the page is tall enough.
function restorePopScroll() {
  const target = pendingPopScroll || 0;
  const delays = [0, 60, 160, 320, 520];
  let i = 0;
  const apply = () => {
    window.scrollTo(0, target);
    i += 1;
    if (i < delays.length && window.scrollY < target) setTimeout(apply, delays[i] - delays[i - 1]);
  };
  apply();
}

// ---------------------------------------------------------------------------
// Wiring: click interception, prefetch triggers, popstate, scroll saving.
// ---------------------------------------------------------------------------
function linkFor(target) {
  return target instanceof Element ? target.closest("a[href]") : null;
}

function shouldIntercept(a, e) {
  if (e.defaultPrevented) return false;
  if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return false;
  if (!a || !a.getAttribute("href")) return false;
  if (a.hasAttribute("download")) return false;
  if (a.getAttribute("data-nav") === "hard") return false;
  const target = a.getAttribute("target");
  if (target && target !== "_self") return false;
  if ((a.getAttribute("rel") || "").split(/\s+/).includes("external")) return false;
  let url;
  try {
    url = new URL(a.href, location.href);
  } catch (_) {
    return false;
  }
  if (url.origin !== location.origin) return false; // external → native
  // pure in-page hash change → let the browser scroll/anchor natively
  if (url.pathname === location.pathname && url.search === location.search && url.hash) return false;
  return url;
}

function onClick(e) {
  const a = linkFor(e.target);
  const url = a && shouldIntercept(a, e);
  if (!url) return;
  e.preventDefault();
  navigate(url.href, { push: true });
}

function onPrefetchHint(e) {
  const a = linkFor(e.target);
  if (!a || a.getAttribute("data-nav") !== "prefetch") return;
  let url;
  try {
    url = new URL(a.href, location.href);
  } catch (_) {
    return;
  }
  if (url.origin === location.origin) prefetch(url.href);
}

function onPopState(e) {
  if (!e.state || !e.state.vyrnNav) return; // not one of ours
  pendingPopScroll = e.state.scrollY || 0;
  navigate(location.href, { push: false });
}

// ---------------------------------------------------------------------------
// Built-in top progress bar. Rides the nav events like any consumer would; it
// is marked data-vyrn-nav-ui so no morph ever touches it. Disable via
// window.__vyrnNavConfig = { progress: false } and hook the events yourself.
// ---------------------------------------------------------------------------
function installProgressBar() {
  const bar = document.createElement("div");
  bar.setAttribute("data-vyrn-nav-ui", "");
  Object.assign(bar.style, {
    position: "fixed",
    top: "0",
    left: "0",
    height: "3px",
    width: "0",
    background: "currentColor",
    color: "#7c5cff",
    opacity: "0",
    zIndex: "2147483647",
    pointerEvents: "none",
    transition: "width .2s ease, opacity .3s ease",
    boxShadow: "0 0 8px currentColor",
  });
  document.documentElement.appendChild(bar);

  let done = null;
  document.addEventListener("vyrn:nav-start", () => {
    clearTimeout(done);
    bar.style.transition = "none";
    bar.style.width = "0";
    bar.style.opacity = "1";
    // next frame: animate to a plausible "most of the way there" width
    requestAnimationFrame(() => {
      bar.style.transition = "width .3s ease, opacity .3s ease";
      bar.style.width = "80%";
    });
  });
  const finish = () => {
    bar.style.width = "100%";
    done = setTimeout(() => {
      bar.style.opacity = "0";
      setTimeout(() => (bar.style.width = "0"), 300);
    }, 150);
  };
  document.addEventListener("vyrn:nav-end", finish);
  document.addEventListener("vyrn:nav-error", finish);
}

// ---------------------------------------------------------------------------
// Public surface + boot.
// ---------------------------------------------------------------------------
export const vyrnNav = {
  navigate: (url) => navigate(new URL(url, location.href).href, { push: true }),
  prefetch: (url) => prefetch(new URL(url, location.href).href),
  registerIsland(selector, boot) {
    const reg = { selector, boot, instance: null };
    islands.push(reg);
    bootIsland(reg); // boot now if the mount is already present
    return reg;
  },
  config: CONFIG,
};

let started = false;
export function start() {
  if (started || typeof document === "undefined") return;
  started = true;

  if ("scrollRestoration" in history) history.scrollRestoration = "manual";
  // Seed the initial entry so back/forward to it is recognized as ours.
  history.replaceState(Object.assign({}, history.state, { vyrnNav: true, scrollY: window.scrollY }), "");

  document.addEventListener("click", onClick);
  document.addEventListener("mouseover", onPrefetchHint);
  document.addEventListener("focusin", onPrefetchHint);
  window.addEventListener("popstate", onPopState);
  window.addEventListener("scroll", saveScroll, { passive: true });

  if (CONFIG.progress) installProgressBar();
}

if (typeof window !== "undefined") {
  window.vyrnNav = vyrnNav; // island registration reaches this before app boot
  start();
}
