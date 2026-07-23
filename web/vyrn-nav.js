// vyrn-nav.js — soft navigation v2 for Vyrn's MPA pages (RFC-0067).
//
// Plain, dependency-free JavaScript, a sibling of vyrn-dom.js. It gives an
// ordinary server-rendered site (std/ui pages) an SPA *feel* without giving up
// the MPA *truth*: every navigation is still a real server render — only the
// page transition goes soft. No flash, no lost scroll, no rebooted wasm.
//
// WHAT v2 CHANGES over v1 (RFC-0034): v1 did a DOM↔DOM keyed *morph* of the
// whole <body> and RE-BOOTED every client island on each nav — which, for a
// wasm island, refetched `/client.wasm`, re-instantiated the module, and threw
// away its module state (the draft). RFC-0067's point is exactly that the wasm
// instance must SURVIVE a navigation. So v2:
//   1. fetches the destination (an ordinary GET — the SERVER IS UNCHANGED),
//   2. swaps `document.title` + the page-owned <head> tags (stylesheets and
//      already-loaded scripts are kept, never refetched),
//   3. REPLACES the layout's content region (`<main>`, falling back to
//      `<body>`) with the fetched one — the shell (header/nav, the persistent
//      <head> assets, and the wasm instance) is never touched,
//   4. RE-MOUNTS the TEA islands inside the new content against the EXISTING
//      wasm instance (the boot path minus instantiation — the widget re-requests
//      its view and paints it into the fresh mount node),
//   5. pushState on forward nav (scroll to top), restores scroll on popstate.
// After a soft nav the network log shows exactly ONE document fetch: no wasm,
// no runtimes, no stylesheets are refetched.
//
// FALLBACK BIAS (locked): soft nav is an optimization, never a correctness
// layer. Anything ambiguous — a cross-origin target, a non-2xx response (a 404
// hard-navs BY DESIGN so the themed error page still loads normally), a
// non-HTML body, a fetch failure/timeout, a second click mid-flight, or any
// exception thrown while swapping — degrades to a plain hard navigation (the
// reload that works today), never to a broken page.
//
// ISLANDS: a page may mount a wasm client app (the `#app` convention). Because
// morphed-in <script> tags are NOT re-executed, the client boot cannot ride the
// page's own <script> across a soft nav. Instead the boot registers an island
// with `window.vyrnNav.registerIsland(selector, boot)`; vyrn-nav owns its
// lifecycle:
//   - FIRST time the selector appears: call `boot(el)`, which does the one-time
//     instantiation and returns an INSTANCE.
//   - Every later nav where the selector reappears: if the instance exposes a
//     `mount(el)`, call it — the SAME instance re-attaches its view to the new
//     node (module state, e.g. a draft, is intact). If it does not (a legacy
//     island), tear it down and `boot(el)` afresh (v1 semantics — a fresh
//     instance per nav).
//   - A nav to a page WITHOUT the selector: the instance is left alive and
//     unmounted, so its module state persists until the mount reappears.
//
// Progressive enhancement: if this file is absent, nothing here ran, every <a>
// is a normal link, and the app boot falls back to booting directly. `data-nav`
// attributes are inert hints with no soft-nav present.
//
// WHAT v3 CHANGES over v2 (RFC-0069): v2 fetched full HTML on every nav — a real
// server render each time, only the transition going soft. v3 goes the Nuxt route:
// when the client page bundle is present (the island's wasm exposes `renderPage`),
// a soft nav fetches ONLY a `{page, title, props}` JSON payload (the data marker
// `?__vyrn=data`), renders the next page CLIENT-side through `renderPage`, and
// paints the resulting tree into `<main>` via vyrn-dom — no HTML transfer, no shell
// re-render. Fallbacks, in order: a route not in the client bundle (the server
// answers non-JSON) → the v2 HTML swap; anything odd → v2's hard nav. If the bundle
// is absent (no `renderPage` registered), v3 IS v2 — the data path never engages.
//
// WHAT M4 CHANGES (RFC-0069 M4): v3 as first shipped fetched the JSON payload on
// EVERY soft nav, including pages with no `load()` (e.g. /about, ~61 B of props:null).
// That is not the Nuxt model — Nuxt navigates a dataless page with ZERO network; the
// page's script decides (declaring `load()` IS the declaration). So the bundle now
// also exports `resolvePage(path) -> {found, hasData, page, title}`; a soft nav
// resolves the target client-side FIRST — a known dataless page renders immediately
// from null props (no fetch), a data page fetches its payload as before, an unknown
// path falls through to the v2 HTML swap. An older bundle that hands over only the
// renderer keeps the M3 fetch-every-nav behavior.

import { renderTree } from "./vyrn-dom.js";

const CONFIG = Object.assign(
  {
    timeoutMs: 10000, // fetch guard: past this a soft nav falls back hard
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
// Island registry. An island is booted ONCE (its wasm instance then survives
// every navigation); its view is re-mounted from the new DOM on each nav that
// lands on a page carrying the mount node.
// ---------------------------------------------------------------------------
const islands = []; // { selector, boot, instance, created }

// Re-mount an already-created island against the node now in the document.
function remountIsland(reg, el) {
  Promise.resolve(reg.instance).then((inst) => {
    try {
      if (inst && typeof inst.mount === "function") {
        inst.mount(el); // persistent instance: re-attach the view (wasm survives)
      } else {
        // Legacy island (no persistent mount): tear down + boot a fresh instance,
        // exactly like a hard load — v1 semantics for consumers not yet on v2.
        if (inst && typeof inst.destroy === "function") {
          try {
            inst.destroy();
          } catch (e) {
            /* a failing teardown must not wedge navigation */
          }
        }
        reg.instance = reg.boot(el) || null;
      }
    } catch (e) {
      /* an island that fails to re-mount must not break the whole nav */
    }
  });
}

// Reconcile every registered island against the CURRENT document. Called after
// each content swap (and once per registration).
function syncIslands() {
  for (const reg of islands) {
    const el = document.querySelector(reg.selector);
    if (!el) continue; // not on this page: leave the instance alive + unmounted
    if (!reg.created) {
      reg.created = true;
      reg.instance = reg.boot(el) || null; // first appearance: one-time boot
    } else {
      remountIsland(reg, el);
    }
  }
}

// ---------------------------------------------------------------------------
// Head. Swap the title and the page-owned tags, but NEVER refetch an asset:
// stylesheets (<link rel=stylesheet>, <style>) and loaded scripts (<script
// src>) are kept in place and only ADDED when genuinely new. Everything else in
// <head> (meta, canonical/icon links, base) is "page-owned" and swapped to the
// incoming page's set.
//
// NOTE (deviation): RFC-0067 §2.1 assumes RFC-0041 emits per-tag head-ownership
// MARKERS. It does not — `document()` concatenates the layout head and the page
// head with no attribute distinguishing them, and adding one would be a server
// change (out of scope: the server stays byte-for-byte unchanged). So we
// identify the never-refetch assets by KIND instead, which for the pages
// runtime is exactly the layout-owned stylesheets + the runtime module. This
// keeps the wasm/runtime/stylesheet "never refetched" guarantee and still swaps
// the genuinely page-owned tags (a dynamic <title>, page <meta>).
// ---------------------------------------------------------------------------
function isKeptAsset(el) {
  const tag = el.tagName;
  if (tag === "STYLE") return true;
  if (tag === "LINK") return (el.getAttribute("rel") || "").split(/\s+/).includes("stylesheet");
  if (tag === "SCRIPT") return el.hasAttribute("src");
  return false;
}

function assetKey(el) {
  const tag = el.tagName;
  if (tag === "STYLE") return "style:" + el.textContent;
  if (tag === "LINK") return "css:" + new URL(el.getAttribute("href"), location.href).href;
  return "js:" + new URL(el.getAttribute("src"), location.href).href;
}

// A page-owned head tag: neither a kept asset, nor the <title> (handled via
// document.title), nor the charset <meta>.
function isPageOwnedHead(el) {
  if (isKeptAsset(el)) return false;
  if (el.tagName === "TITLE") return false;
  if (el.tagName === "META" && el.hasAttribute("charset")) return false;
  return true;
}

// Import a head element so it WORKS in this document. A <script> from a
// DOMParser document is permanently inert — the spec's "already started" flag
// survives importNode, so the node lands in <head> but never fetches or runs
// (observed live: soft-nav /about → / never loaded the home page's /app.js
// island module, leaving the create form dead until a hard reload). Rebuild
// scripts with createElement so insertion executes them; everything else
// imports as-is. A module script re-added with a previously-seen src is a
// no-op by ES module caching, so this stays idempotent.
function executableImport(el) {
  if (el.tagName !== "SCRIPT") return document.importNode(el, true);
  const s = document.createElement("script");
  for (const a of el.attributes) s.setAttribute(a.name, a.value);
  s.textContent = el.textContent;
  return s;
}

function swapHead(newDoc) {
  const newTitle = newDoc.querySelector("title");
  if (newTitle) document.title = newTitle.textContent;

  // 1) Additive assets — add any new stylesheet/style/script[src], remove none.
  const have = new Set();
  for (const el of document.head.children) if (isKeptAsset(el)) have.add(assetKey(el));
  for (const el of newDoc.head.children) {
    if (isKeptAsset(el) && !have.has(assetKey(el))) {
      document.head.appendChild(executableImport(el));
      have.add(assetKey(el));
    }
  }

  // 2) Page-owned tags — swap the set: drop the current ones, add the incoming.
  for (const el of Array.from(document.head.children)) {
    if (isPageOwnedHead(el)) el.remove();
  }
  for (const el of newDoc.head.children) {
    if (isPageOwnedHead(el)) document.head.appendChild(executableImport(el));
  }
}

// ---------------------------------------------------------------------------
// Content region. Replace the layout's <main> (falling back to <body>) with the
// fetched one. The header/nav and the persistent <head> assets sit OUTSIDE
// <main>, so they — and the delegated document-level click listener that binds
// every <a> — are never disturbed.
// ---------------------------------------------------------------------------
function isNavUi(node) {
  return node.nodeType === 1 && node.hasAttribute("data-vyrn-nav-ui");
}

function replaceContent(newDoc) {
  const liveMain = document.querySelector("main");
  const newMain = newDoc.querySelector("main");
  if (liveMain && newMain) {
    liveMain.replaceWith(document.importNode(newMain, true));
    return;
  }
  // Fallback: a page with no <main> — replace the body's children wholesale
  // (the progress bar lives on <html>, so a body swap never touches it; any
  // body-level nav UI a consumer marked is preserved defensively).
  const newBody = newDoc.body;
  if (!newBody) throw new Error("vyrn-nav: fetched document has no <body>");
  const preserved = Array.from(document.body.childNodes).filter(isNavUi);
  const incoming = Array.from(newBody.childNodes).map((n) => document.importNode(n, true));
  document.body.replaceChildren(...preserved, ...incoming);
}

// ---------------------------------------------------------------------------
// Apply a parsed document to the live one: title/head, content region, islands.
// ---------------------------------------------------------------------------
function applyDocument(newDoc) {
  swapHead(newDoc);
  replaceContent(newDoc);
  syncIslands();
}

// ---------------------------------------------------------------------------
// The client page renderer (RFC-0069 §3). The island's wasm bundle exports
// `renderPage(payloadJson) -> String`; its boot hands it here. Feature-detected
// once per nav: when it is a function, the navigator uses the data channel; when
// it is null (no bundle, still booting, or a no-JS page), the navigator is v2.
// ---------------------------------------------------------------------------
let pageRenderer = null;

// The client page RESOLVER (RFC-0069 M4). The island's wasm also exports
// `resolvePage(path) -> String` ({found, hasData, page, title} JSON); its boot hands
// it here. With a resolver, a soft nav decides its channel WITHOUT a network round-
// trip: a known dataless page renders immediately (zero fetch), a data page fetches
// its payload, an unknown path falls through to the v2 HTML swap. When it is null (an
// older bundle that registered only the renderer), the navigator keeps the M3
// behavior — fetch the data marker on every nav.
let pageResolver = null;

// The data-marked variant of a URL — the same path, with `?__vyrn=data` added, so
// the server answers with the page's JSON payload instead of its HTML document.
function withDataMarker(url) {
  const u = new URL(url, location.href);
  u.searchParams.set("__vyrn", "data");
  return u.href;
}

// Resolve a nav target against the client bundle (RFC-0069 M4), returning the parsed
// {found, hasData, page, title} descriptor, or null when there is no resolver / it
// throws / its answer will not parse (the caller then behaves as M3).
function resolveTarget(url) {
  if (typeof pageResolver !== "function") return null;
  try {
    return JSON.parse(pageResolver(new URL(url, location.href).pathname));
  } catch (_) {
    return null;
  }
}

// Paint a `renderPage` tree (a page body whose root is `<main>`) into the live
// document, replacing the current `<main>`. Throws if there is no `<main>` — the
// caller turns that into a hard nav (fallback bias).
function paintMain(treeJson) {
  const liveMain = document.querySelector("main");
  if (!liveMain) throw new Error("vyrn-nav: no <main> to paint into");
  liveMain.replaceWith(renderTree(JSON.parse(treeJson)));
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

function pushEntry(url) {
  // Synchronously stamp the leaving entry with its scroll offset BEFORE pushing
  // the new one (the throttled scroll listener may not have run yet, and a late
  // frame must never write this offset into the new entry).
  history.replaceState(Object.assign({}, history.state, { vyrnNav: true, scrollY: window.scrollY }), "");
  history.pushState({ vyrnNav: true, scrollY: 0 }, "", url);
}

let pendingPopScroll = 0;
// Restore the saved offset — then re-apply it on a short, bounded schedule. An
// island re-mount (or any async content) can briefly collapse page height right
// after the swap, clamping the scroll; re-applying for ~½s lets the target stick
// once the content grows back. Harmless once the page is tall enough.
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

async function navigate(url, { push }) {
  if (inflight) {
    hardNav(url); // mid-flight second click → hard nav
    return;
  }

  // Feature-detect the client bundle once per nav. With a renderer AND a resolver
  // (RFC-0069 M4), resolve the target against the bundle FIRST, no network.
  const dataMode = typeof pageRenderer === "function";
  const resolved = dataMode ? resolveTarget(url) : null;

  // A known page with NO data renders immediately from null props — ZERO fetch (the
  // Nuxt model: declaring load() is the declaration; a dataless page navigates with no
  // network). The render is synchronous, so the in-flight guard above is enough — no
  // AbortController is armed. Any failure degrades to a hard nav (fallback bias).
  if (dataMode && resolved && resolved.found && resolved.hasData === false) {
    emit("nav-start", { url });
    try {
      const treeJson = pageRenderer(JSON.stringify({ page: resolved.page, props: null, params: null }));
      if (!treeJson || treeJson === "__vyrn_fallback__") {
        throw new Error("client bundle cannot render this page");
      }
      if (push) pushEntry(url);
      // An empty resolved title leaves document.title at the layout default (a page
      // that declares no title{} must NOT overwrite it with the url-pattern).
      if (resolved.title) document.title = String(resolved.title);
      paintMain(treeJson);
      syncIslands();
      if (push) window.scrollTo(0, 0);
      else restorePopScroll();
      emit("nav-end", { url });
    } catch (err) {
      emit("nav-error", { url, reason: "resolve-render-failed" });
      hardNav(url);
    }
    return;
  }

  emit("nav-start", { url });
  const controller = new AbortController();
  inflight = controller;
  const timer = setTimeout(() => controller.abort(), CONFIG.timeoutMs);

  // Fetch the data marker (JSON payload) ONLY for a resolved DATA page; an unresolved
  // path — not in the client bundle — takes the plain HTML channel (the v2 swap /
  // hard-nav chain, unchanged). Without a resolver (an older bundle that registered
  // only the renderer) keep the M3 behavior: the marker on every nav. `x-vyrn-nav`
  // stays a hint the server ignores (it keys off the query marker), kept for
  // observability.
  const wantData = dataMode && (resolved ? resolved.found && resolved.hasData : true);
  const fetchUrl = wantData ? withDataMarker(url) : url;

  let res;
  try {
    res = await fetch(fetchUrl, { headers: { "x-vyrn-nav": wantData ? "data" : "soft" }, signal: controller.signal });
  } catch (err) {
    clearTimeout(timer);
    inflight = null;
    emit("nav-error", { url, reason: "fetch-failed" });
    hardNav(url); // network failure / timeout / abort → hard nav
    return;
  }
  clearTimeout(timer);
  inflight = null;

  if (!res.ok) {
    emit("nav-error", { url, reason: "non-2xx" });
    hardNav(url);
    return;
  }
  const ct = res.headers.get("content-type") || "";

  // Data channel (RFC-0069 §3): a JSON payload the client renders itself. Set the
  // title, render the page's tree via the wasm, paint it into <main>, and re-sync
  // islands (the create island re-mounts against its surviving instance). A
  // distinguished fallback sentinel — or any exception — degrades to a hard nav.
  if (wantData && ct.includes("application/json")) {
    let payloadText;
    try {
      payloadText = await res.text();
    } catch (err) {
      emit("nav-error", { url, reason: "body-failed" });
      hardNav(url);
      return;
    }
    try {
      const treeJson = pageRenderer(payloadText);
      if (!treeJson || treeJson === "__vyrn_fallback__") {
        throw new Error("client bundle cannot render this page");
      }
      let payload = null;
      try {
        payload = JSON.parse(payloadText);
      } catch (_) {
        /* title stays as-is if the envelope won't parse */
      }
      if (push) pushEntry(url);
      if (payload && payload.title != null) document.title = String(payload.title);
      paintMain(treeJson);
      syncIslands();
      if (push) window.scrollTo(0, 0);
      else restorePopScroll();
      emit("nav-end", { url });
    } catch (err) {
      emit("nav-error", { url, reason: "render-failed" });
      hardNav(url);
    }
    return;
  }

  // HTML channel (v2): a full document — either pure-v2 mode, or a valid route not
  // in the client bundle (a `.vyrn`/respond page whose marked request returned its
  // real HTML). Swap it exactly as v2 does.
  if (ct.includes("text/html")) {
    let html;
    try {
      html = await res.text();
    } catch (err) {
      emit("nav-error", { url, reason: "body-failed" });
      hardNav(url);
      return;
    }
    try {
      const doc = new DOMParser().parseFromString(html, "text/html");
      if (push) pushEntry(url);
      applyDocument(doc);
      if (push) window.scrollTo(0, 0);
      else restorePopScroll();
      emit("nav-end", { url });
    } catch (err) {
      // Any exception mid-swap: reload for real rather than leave a half-swapped
      // page (hardNav discards whatever partial mutation happened).
      emit("nav-error", { url, reason: "swap-failed" });
      hardNav(url);
    }
    return;
  }

  // Anything else — a non-client route answering non-JSON (e.g. `/raw` text/plain),
  // or any other body — hands off to the browser.
  emit("nav-error", { url, reason: "non-html" });
  hardNav(url);
}

// ---------------------------------------------------------------------------
// Wiring: click interception, popstate, scroll saving.
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
  if (url.protocol !== "http:" && url.protocol !== "https:") return false; // mailto:, tel:, … → native
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

function onPopState(e) {
  if (!e.state || !e.state.vyrnNav) return; // not one of ours
  pendingPopScroll = e.state.scrollY || 0;
  navigate(location.href, { push: false });
}

// ---------------------------------------------------------------------------
// Built-in top progress bar. Rides the nav events like any consumer would; it
// is marked data-vyrn-nav-ui so no swap ever touches it. Disable via
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

  // The bar only appears when a nav is actually SLOW (>150ms). A localhost
  // swap completes in a few ms; flashing 0→80→100% on it reads exactly like a
  // full page load — the opposite of what a soft nav should feel like.
  let done = null;
  let arm = null;
  let shown = false;
  document.addEventListener("vyrn:nav-start", () => {
    clearTimeout(done);
    clearTimeout(arm);
    shown = false;
    arm = setTimeout(() => {
      shown = true;
      bar.style.transition = "none";
      bar.style.width = "0";
      bar.style.opacity = "1";
      // next frame: animate to a plausible "most of the way there" width
      requestAnimationFrame(() => {
        bar.style.transition = "width .3s ease, opacity .3s ease";
        bar.style.width = "80%";
      });
    }, 150);
  });
  const finish = () => {
    clearTimeout(arm);
    if (!shown) return; // fast nav: the bar never appeared — keep it that way
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
  // Prefetch is out of scope for v2 (RFC-0067) — kept as a no-op so a `data-nav
  // ="prefetch"` link and any `vyrnNav.prefetch(url)` caller stay harmless.
  prefetch: () => {},
  registerIsland(selector, boot) {
    const reg = { selector, boot, instance: null, created: false };
    islands.push(reg);
    syncIslands(); // boot now if the mount is already present
    return reg;
  },
  // The island boot hands its wasm `renderPage` here (RFC-0069 §3) once the client
  // bundle is instantiated; a soft nav then renders pages client-side. Until it is
  // set (no bundle, still booting, a no-JS page), navigation stays pure v2.
  setPageRenderer(fn) {
    pageRenderer = typeof fn === "function" ? fn : null;
  },
  // The island boot hands its wasm `resolvePage` here (RFC-0069 M4). With it, a soft
  // nav resolves the target against the bundle before any network — a dataless page
  // then navigates with zero fetch. Until it is set, the navigator keeps the M3
  // behavior (fetch the data marker on every nav).
  setPageResolver(fn) {
    pageResolver = typeof fn === "function" ? fn : null;
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
  window.addEventListener("popstate", onPopState);
  window.addEventListener("scroll", saveScroll, { passive: true });

  if (CONFIG.progress) installProgressBar();
}

if (typeof window !== "undefined") {
  window.vyrnNav = vyrnNav; // island registration reaches this before app boot
  start();
}
