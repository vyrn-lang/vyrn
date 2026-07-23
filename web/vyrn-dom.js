// vyrn-dom.js — the client runtime for the Vyrn UI layer (RFC-0026 M2).
//
// Plain, dependency-free JavaScript beside wasi-min.js. It talks to ORDINARY
// wasm exports — nothing here is privileged, and a third party could write a
// competing runtime against the same exports.
//
// The Elm Architecture, host-side:
//   - the app exports `vyrnView() -> String` (one line: `return toJson(view())`).
//     We call it, parse the JSON `Html` tree, and build the DOM under a mount.
//   - after any handler returns we call `vyrnView()` again, DIFF the new tree
//     against the retained old one (keyed where `Key` attrs are present,
//     positional otherwise), and patch the real DOM minimally.
//   - events are delegated: one listener per event type on the mount root. On
//     an event we walk to the nearest `data-on-<type>` element and invoke the
//     exported extern handler by name (the locked handler ABI, below).
//   - subscriptions (`vyrnSubs()`) and imperative effects (`data-effect`) are
//     declared as data and reconciled by value after each render.
//
// Handler ABI (locked): every handler is `export extern fn name(arg: String)`.
//   click / keydown  -> the element's `data-arg-<type>` payload
//   input / change   -> the control's current value
//   submit           -> the payload, and `preventDefault()` is called
// Handlers parse/validate their own arg — the same boundary discipline as the
// rest of Vyrn.
//
// Usage:
//   import { mount } from "./vyrn-dom.js";
//   const app = await mount(wasmBytes, document.getElementById("app"), {
//     effects: { mountChart: (el) => { /* … */ return () => {/* cleanup */}; } },
//     onStdout: (s) => console.log(s),
//   });
//   // app.exports — the raw wasm exports; app.rerender(); app.destroy();
//   // app.effect(name, fn) — register an imperative effect after mount.

import { runVyrn } from "./wasi-min.js";

// The HTML void-element set — mirrors std/html's `isVoid`, so the DOM builder
// never tries to give these children (createElement + no kids).
const VOID = new Set([
  "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta",
  "source", "track", "wbr",
]);

// ---------------------------------------------------------------------------
// Parse the JSON `Html` tree (the wire form of std/html's payload enum, RFC-0024)
// into normalized vnodes. Encoding recap:
//   Empty                 -> "Empty"
//   Text(s)               -> {"Text": s}
//   Raw(s)                -> {"Raw": s}
//   El(tag, attrs, kids)  -> {"El": [tag, [attr…], [html…]]}
//   Cls/Id/Key(v)         -> {"Cls": v} …            A(n,v) -> {"A": [n, v]}
//   On(event,handler,pay) -> {"On": [event, handler, payload]}
// ---------------------------------------------------------------------------
function parseHtml(j) {
  if (j === "Empty") return { kind: "empty", key: null };
  if (j && typeof j === "object") {
    if ("Text" in j) return { kind: "text", text: String(j.Text), key: null };
    if ("Raw" in j) return { kind: "raw", html: String(j.Raw), key: null };
    if ("El" in j) {
      const [tag, attrsJson, kidsJson] = j.El;
      const a = parseAttrs(attrsJson || []);
      return {
        kind: "el",
        tag: String(tag),
        attrs: a.attrs,
        on: a.on,
        key: a.key,
        effect: a.effect,
        kids: (kidsJson || []).map(parseHtml),
      };
    }
  }
  // Unknown shape: render nothing rather than throw (stay total, like SSR).
  return { kind: "empty", key: null };
}

function parseAttrs(list) {
  const attrs = {}; // final DOM attribute name -> value
  const on = {}; // eventType -> { handler, payload }
  let key = null;
  let effect = null;
  for (const at of list) {
    if ("Cls" in at) attrs["class"] = String(at.Cls);
    else if ("Id" in at) attrs["id"] = String(at.Id);
    else if ("A" in at) {
      const [n, v] = at.A;
      attrs[String(n)] = String(v);
      if (n === "data-effect") effect = String(v);
    } else if ("On" in at) {
      const [e, h, p] = at.On;
      on[String(e)] = { handler: String(h), payload: String(p) };
      // Mirror the SSR wire form so delegation reads the same attributes.
      attrs[`data-on-${e}`] = String(h);
      attrs[`data-arg-${e}`] = String(p);
    } else if ("Key" in at) {
      key = String(at.Key);
      attrs["data-key"] = key;
    }
  }
  return { attrs, on, key, effect };
}

// ---------------------------------------------------------------------------
// Static tree → DOM (RFC-0069 §3). Build a DOM subtree from a JSON `Html` tree
// with NO event delegation or effect wiring — used by vyrn-nav v3 to paint a
// soft-navigated PAGE body into `<main>`. Pages are pure over their props (any
// interactivity lives in an island, which re-mounts itself, or in `<a href>`
// links the navigator's document-level listener already intercepts), so a static
// build is exactly right; the returned node replaces the live `<main>`.
// ---------------------------------------------------------------------------
function buildStatic(v) {
  let dom;
  if (v.kind === "empty") {
    dom = document.createComment("");
  } else if (v.kind === "text") {
    dom = document.createTextNode(v.text);
  } else if (v.kind === "raw") {
    dom = document.createElement("span");
    dom.style.display = "contents";
    dom.setAttribute("data-vyrn-raw", "");
    dom.innerHTML = v.html;
  } else {
    dom = document.createElement(v.tag);
    for (const name of Object.keys(v.attrs)) dom.setAttribute(name, v.attrs[name]);
    if (!VOID.has(v.tag)) {
      for (const kid of v.kids) dom.appendChild(buildStatic(kid));
    }
  }
  // Record the vnode↔DOM mapping so a later static patch (RFC-0070) can reuse the
  // node in place — the difference from mount()'s createNode is only that this
  // builder wires no events/effects (a page body is pure; any interactivity lives
  // in an island, a static leaf here that is left untouched when unchanged).
  v.dom = dom;
  return dom;
}

/// Parse a JSON `Html` tree (a `renderPage` result) into a detached DOM node.
export function renderTree(json) {
  return buildStatic(parseHtml(json));
}

// ---------------------------------------------------------------------------
// Static page-view patching (RFC-0070) — the lazy skeleton→data transition. A
// lazy page first paints its `Loading` tree, then re-renders `Ready(props)` once
// the payload arrives; diffing the two trees patches ONLY the region that changed
// (the data region), leaving the shell — and the island mount inside it — exactly
// where it was, so the form never repaints and the wasm instance is never touched.
// No events/effects here: pages are pure (see buildStatic). The differ mirrors
// mount()'s internal keyed reconciliation, but standalone over retained vnodes.
// ---------------------------------------------------------------------------
function sameStatic(a, b) {
  if (a.kind !== b.kind) return false;
  if (a.kind === "el") return a.tag === b.tag;
  return true;
}

function setStaticAttrs(dom, oldAttrs, newAttrs) {
  for (const name of Object.keys(oldAttrs)) if (!(name in newAttrs)) dom.removeAttribute(name);
  for (const name of Object.keys(newAttrs)) if (oldAttrs[name] !== newAttrs[name]) dom.setAttribute(name, newAttrs[name]);
}

function patchStatic(oldV, newV) {
  if (!sameStatic(oldV, newV)) {
    const dom = buildStatic(newV);
    if (oldV.dom && oldV.dom.parentNode) oldV.dom.parentNode.replaceChild(dom, oldV.dom);
    return;
  }
  newV.dom = oldV.dom;
  if (newV.kind === "text") {
    if (oldV.text !== newV.text) newV.dom.textContent = newV.text;
  } else if (newV.kind === "raw") {
    if (oldV.html !== newV.html) newV.dom.innerHTML = newV.html;
  } else if (newV.kind === "el") {
    setStaticAttrs(newV.dom, oldV.attrs, newV.attrs);
    if (!VOID.has(newV.tag)) patchStaticChildren(newV.dom, oldV.kids, newV.kids);
  }
  // empty: nothing to do
}

function patchStaticChildren(parent, oldK, newK) {
  const keyed = oldK.some((v) => v.key != null) || newK.some((v) => v.key != null);
  if (keyed) return patchStaticKeyed(parent, oldK, newK);
  const common = Math.min(oldK.length, newK.length);
  for (let i = 0; i < common; i++) patchStatic(oldK[i], newK[i]);
  for (let i = oldK.length - 1; i >= common; i--) {
    const v = oldK[i];
    if (v.dom && v.dom.parentNode === parent) parent.removeChild(v.dom);
  }
  for (let i = common; i < newK.length; i++) parent.appendChild(buildStatic(newK[i]));
}

function patchStaticKeyed(parent, oldK, newK) {
  const oldByKey = new Map();
  for (const v of oldK) if (v.key != null) oldByKey.set(v.key, v);
  const newDoms = [];
  for (const nv of newK) {
    const ov = nv.key != null ? oldByKey.get(nv.key) : null;
    if (ov && sameStatic(ov, nv)) {
      patchStatic(ov, nv);
      oldByKey.delete(nv.key);
    } else {
      nv.dom = buildStatic(nv);
    }
    newDoms.push(nv.dom);
  }
  const keep = new Set(newDoms);
  for (const v of oldK) {
    if (v.dom && v.dom.parentNode === parent && !keep.has(v.dom)) parent.removeChild(v.dom);
  }
  for (let i = 0; i < newDoms.length; i++) {
    const ref = parent.childNodes[i] || null;
    if (ref !== newDoms[i]) parent.insertBefore(newDoms[i], ref);
  }
}

/// Build a page view from a JSON `Html` tree, RETAINING the vnode so a later
/// `patchPageView` can diff against it. Returns `{ vnode, dom }` — the caller puts
/// `dom` in the document (e.g. as `<main>`), then hands the view back to patch.
export function makePageView(json) {
  // Callers pass a `renderPage` result STRING; parseHtml consumes a parsed tree
  // (renderTree is handed `JSON.parse(...)`). Parse here so both entry points
  // share one contract — a raw string otherwise fell through parseHtml to an
  // empty comment node, and replaceWith then deleted the live `<main>`.
  const tree = typeof json === "string" ? JSON.parse(json) : json;
  const v = parseHtml(tree);
  return { vnode: v, dom: buildStatic(v) };
}

/// Patch a retained page view to a new JSON `Html` tree, in place — only the
/// changed nodes touch the DOM. The view's root DOM node (already in the document)
/// is reused; `view.vnode` is advanced to the new tree. Returns the same `view`.
export function patchPageView(view, json) {
  const tree = typeof json === "string" ? JSON.parse(json) : json;
  const next = parseHtml(tree);
  patchStatic(view.vnode, next);
  view.vnode = next;
  return view;
}

// ---------------------------------------------------------------------------
// The runtime instance.
// ---------------------------------------------------------------------------
export async function mount(wasmBytes, mountEl, opts = {}) {
  const { effects = {}, ...runHooks } = opts;
  const result = await runVyrn(wasmBytes, {
    ...runHooks,
    // vyrnView / vyrnSubs / vyrnPatch return Strings; name them so the export
    // glue decodes (RFC-0035 adds vyrnPatch — the op stream, when present).
    exportReturns: { vyrnView: "string", vyrnSubs: "string", vyrnPatch: "string", ...(runHooks.exportReturns || {}) },
  });
  const exports = result.exports;
  if (typeof exports.vyrnView !== "function") {
    throw new Error(
      "vyrn-dom: the module does not export `vyrnView` — add " +
        "`export extern fn vyrnView() -> String { return toJson(view()) }`"
    );
  }

  // Protocol negotiation (RFC-0035, host-side): if the module exports
  // `vyrnPatch`, the wasm side owns the diff — we boot from `vyrnView()` once,
  // then apply the op stream `vyrnPatch()` returns on every render. If it does
  // NOT, we keep today's full `vyrnView()` + JS-diff loop, untouched.
  const hasPatch = typeof exports.vyrnPatch === "function";

  const effectRegistry = { ...effects };
  const activeEffects = new Map(); // domNode -> { name, cleanup }
  const activeSubs = new Map(); // subKey -> teardown fn
  const wiredEvents = new Set(); // event types with a delegated root listener
  let current = null; // the retained root vnode (full-view loop only)
  let rootDom = null; // the single mounted DOM node (patch loop: path [] resolves here)
  let destroyed = false;

  // --- invoke an exported handler, then re-render -------------------------
  function invoke(handler, arg) {
    const fn = exports[handler];
    if (typeof fn !== "function") {
      console.warn(`vyrn-dom: no exported handler \`${handler}\``);
      return;
    }
    fn(String(arg));
    rerender();
  }

  // --- delegated events ---------------------------------------------------
  function ensureDelegated(type) {
    if (wiredEvents.has(type)) return;
    wiredEvents.add(type);
    mountEl.addEventListener(type, (ev) => {
      for (let n = ev.target; n && n !== mountEl.parentNode; n = n.parentNode) {
        if (n.nodeType === 1 && n.hasAttribute(`data-on-${type}`)) {
          const handler = n.getAttribute(`data-on-${type}`);
          let arg;
          if (type === "input" || type === "change") {
            arg = n.value != null ? String(n.value) : "";
          } else {
            arg = n.getAttribute(`data-arg-${type}`) || "";
          }
          if (type === "submit") ev.preventDefault();
          invoke(handler, arg);
          return;
        }
      }
    });
  }

  // --- DOM construction ---------------------------------------------------
  function createNode(v) {
    let dom;
    if (v.kind === "empty") {
      dom = document.createComment("");
    } else if (v.kind === "text") {
      dom = document.createTextNode(v.text);
    } else if (v.kind === "raw") {
      // A single wrapper (display:contents, invisible to layout) keeps the
      // vnode↔DOM mapping 1:1 for diffing while emitting arbitrary markup.
      dom = document.createElement("span");
      dom.style.display = "contents";
      dom.setAttribute("data-vyrn-raw", "");
      dom.innerHTML = v.html;
    } else {
      dom = document.createElement(v.tag);
      applyAttrs(dom, {}, v.attrs);
      if (!VOID.has(v.tag)) {
        for (const kid of v.kids) dom.appendChild(createNode(kid));
      }
    }
    v.dom = dom;
    return dom;
  }

  // Value/checked are live PROPERTIES on form controls, not just default
  // attributes — set them as properties so a re-render updates a control the
  // user has typed into, and skip the write when unchanged (preserve caret).
  function isFormControl(dom) {
    const t = dom.tagName;
    return t === "INPUT" || t === "TEXTAREA" || t === "SELECT";
  }

  function applyAttrs(dom, oldAttrs, newAttrs) {
    // remove attrs gone in new
    for (const name of Object.keys(oldAttrs)) {
      if (!(name in newAttrs)) {
        if (name === "value" && isFormControl(dom)) dom.value = "";
        else if (name === "checked" && isFormControl(dom)) dom.checked = false;
        else dom.removeAttribute(name);
      }
    }
    // set changed / new attrs
    for (const name of Object.keys(newAttrs)) {
      const v = newAttrs[name];
      if (oldAttrs[name] === v) continue;
      if (name === "value" && isFormControl(dom)) {
        if (dom.value !== v) dom.value = v;
      } else if (name === "checked" && isFormControl(dom)) {
        const on = v === "true" || v === "checked" || v === "1";
        if (dom.checked !== on) dom.checked = on;
      } else {
        dom.setAttribute(name, v);
        if (name.startsWith("data-on-")) ensureDelegated(name.slice("data-on-".length));
      }
    }
  }

  // --- diffing ------------------------------------------------------------
  function sameType(a, b) {
    if (a.kind !== b.kind) return false;
    if (a.kind === "el") return a.tag === b.tag;
    return true;
  }

  function removeVnode(parent, v) {
    // unmount effects nested under this node happen in the post-render sweep;
    // here we only detach the DOM.
    if (v.dom && v.dom.parentNode === parent) parent.removeChild(v.dom);
  }

  function patchNode(parent, oldV, newV) {
    if (!sameType(oldV, newV)) {
      const dom = createNode(newV);
      if (oldV.dom && oldV.dom.parentNode) oldV.dom.parentNode.replaceChild(dom, oldV.dom);
      return;
    }
    // same type: reuse the DOM node
    newV.dom = oldV.dom;
    if (newV.kind === "text") {
      if (oldV.text !== newV.text) newV.dom.textContent = newV.text;
    } else if (newV.kind === "raw") {
      if (oldV.html !== newV.html) newV.dom.innerHTML = newV.html;
    } else if (newV.kind === "el") {
      applyAttrs(newV.dom, oldV.attrs, newV.attrs);
      if (!VOID.has(newV.tag)) patchChildren(newV.dom, oldV.kids, newV.kids);
    }
    // empty: nothing to do
  }

  function patchChildren(parent, oldV, newV) {
    const keyed = oldV.some((v) => v.key != null) || newV.some((v) => v.key != null);
    if (keyed) return patchKeyed(parent, oldV, newV);
    const common = Math.min(oldV.length, newV.length);
    for (let i = 0; i < common; i++) patchNode(parent, oldV[i], newV[i]);
    for (let i = oldV.length - 1; i >= common; i--) removeVnode(parent, oldV[i]);
    for (let i = common; i < newV.length; i++) parent.appendChild(createNode(newV[i]));
  }

  // Keyed reconciliation: reuse the DOM node for a matching key (so input
  // focus / caret / scroll survive a reorder), create new keys, drop gone keys,
  // then order the parent's children to match the new sequence.
  function patchKeyed(parent, oldV, newV) {
    const oldByKey = new Map();
    for (const v of oldV) if (v.key != null) oldByKey.set(v.key, v);
    const newDoms = [];
    for (const nv of newV) {
      const ov = nv.key != null ? oldByKey.get(nv.key) : null;
      if (ov && sameType(ov, nv)) {
        patchNode(parent, ov, nv); // sets nv.dom = ov.dom, patches in place
        oldByKey.delete(nv.key);
      } else {
        nv.dom = createNode(nv);
      }
      newDoms.push(nv.dom);
    }
    const keep = new Set(newDoms);
    for (const v of oldV) {
      if (v.dom && v.dom.parentNode === parent && !keep.has(v.dom)) parent.removeChild(v.dom);
    }
    // Place each desired node at its index (insertBefore also MOVES an existing
    // node, so reused DOM is relocated, not recreated).
    for (let i = 0; i < newDoms.length; i++) {
      const ref = parent.childNodes[i] || null;
      if (ref !== newDoms[i]) parent.insertBefore(newDoms[i], ref);
    }
  }

  // --- the patch protocol (RFC-0035) --------------------------------------
  // `vyrnPatch()` returns a `PatchOp` stream (std/html's `diff`). Paths are
  // child-index vectors from the mount root; each vnode is exactly one DOM node
  // so a path indexes `childNodes` directly. Ops apply strictly in order — the
  // differ emits them so each op's path/index is valid at its turn (see
  // std/html's emission-order docs), so this applier is deliberately naive.

  // Resolve a child-index path against the live subtree. `[]` is the root node.
  function nodeAt(path) {
    let n = rootDom;
    for (const idx of path) {
      if (!n) return null;
      n = n.childNodes[idx];
    }
    return n;
  }

  // Re-run the existing per-attribute reconciliation for an OpSetAttrs. Old
  // attrs come from the LIVE element's attributes (form `value`/`checked` are
  // never attributes — they stay live properties, so a reorder or an unrelated
  // attr change never clobbers a typed value; only a changed `value`/`checked`
  // in the view rewrites the property, exactly as the full-view loop does).
  function reconcileAttrs(el, attrsJson) {
    const parsed = parseAttrs(attrsJson || []);
    const oldAttrs = {};
    for (const name of el.getAttributeNames()) oldAttrs[name] = el.getAttribute(name);
    applyAttrs(el, oldAttrs, parsed.attrs);
  }

  function applyOp(op) {
    if ("OpSetText" in op) {
      const [path, txt] = op.OpSetText;
      const n = nodeAt(path);
      if (n) n.textContent = String(txt);
    } else if ("OpSetAttrs" in op) {
      const [path, attrsJson] = op.OpSetAttrs;
      const n = nodeAt(path);
      if (n && n.nodeType === 1) reconcileAttrs(n, attrsJson);
    } else if ("OpReplace" in op) {
      const [path, htmlJson] = op.OpReplace;
      const dom = createNode(parseHtml(htmlJson));
      if (path.length === 0) {
        if (rootDom && rootDom.parentNode) rootDom.parentNode.replaceChild(dom, rootDom);
        rootDom = dom;
      } else {
        const old = nodeAt(path);
        if (old && old.parentNode) old.parentNode.replaceChild(dom, old);
      }
    } else if ("OpInsert" in op) {
      const [ppath, index, htmlJson] = op.OpInsert;
      const parent = nodeAt(ppath);
      if (!parent) return;
      const dom = createNode(parseHtml(htmlJson));
      parent.insertBefore(dom, parent.childNodes[index] || null);
    } else if ("OpRemove" in op) {
      const [ppath, index] = op.OpRemove;
      const parent = nodeAt(ppath);
      if (!parent) return;
      const child = parent.childNodes[index];
      if (child) parent.removeChild(child);
    } else if ("OpMove" in op) {
      const [ppath, from, to] = op.OpMove;
      const parent = nodeAt(ppath);
      if (!parent) return;
      const node = parent.childNodes[from];
      if (!node) return;
      // insertBefore also relocates: reference the node currently at `to`
      // (accounting for the removal when moving rightward).
      const ref = from < to ? parent.childNodes[to + 1] || null : parent.childNodes[to] || null;
      parent.insertBefore(node, ref);
    }
  }

  function applyOps(ops) {
    for (const op of ops) applyOp(op);
  }

  function patchTick() {
    let ops;
    try {
      ops = JSON.parse(exports.vyrnPatch());
    } catch {
      return;
    }
    applyOps(ops || []);
  }

  // Effect reconciliation for the patch loop: the new vnode tree never reaches
  // JS (only ops do), so effects are keyed off the LIVE DOM — inserts, removes,
  // and replaces that add/drop a `data-effect` element fire the registry both
  // ways by construction.
  function runEffectsDom() {
    const seen = new Map();
    if (rootDom && rootDom.nodeType === 1 && rootDom.hasAttribute("data-effect")) {
      seen.set(rootDom, rootDom.getAttribute("data-effect"));
    }
    const scope = rootDom && rootDom.querySelectorAll ? rootDom : mountEl;
    for (const el of scope.querySelectorAll("[data-effect]")) {
      seen.set(el, el.getAttribute("data-effect"));
    }
    for (const [dom, name] of seen) {
      if (!activeEffects.has(dom)) {
        const reg = effectRegistry[name];
        const cleanup = typeof reg === "function" ? reg(dom) : null;
        activeEffects.set(dom, { name, cleanup });
      }
    }
    for (const [dom, info] of [...activeEffects]) {
      if (!seen.has(dom)) {
        if (typeof info.cleanup === "function") info.cleanup(dom);
        activeEffects.delete(dom);
      }
    }
  }

  // --- effects (imperative escape hatch) ----------------------------------
  function collectEffects(v, out) {
    if (!v) return;
    if (v.kind === "el") {
      if (v.effect && v.dom) out.set(v.dom, v.effect);
      for (const k of v.kids) collectEffects(k, out);
    }
  }

  function runEffects() {
    const seen = new Map();
    collectEffects(current, seen);
    for (const [dom, name] of seen) {
      if (!activeEffects.has(dom)) {
        const reg = effectRegistry[name];
        const cleanup = typeof reg === "function" ? reg(dom) : null;
        activeEffects.set(dom, { name, cleanup });
      }
    }
    for (const [dom, info] of [...activeEffects]) {
      if (!seen.has(dom)) {
        if (typeof info.cleanup === "function") info.cleanup(dom);
        activeEffects.delete(dom);
      }
    }
  }

  // --- subscriptions ------------------------------------------------------
  function parseSub(j) {
    if (j && typeof j === "object") {
      if ("Every" in j) return { kind: "Every", ms: Number(j.Every[0]), handler: String(j.Every[1]) };
      if ("Keydown" in j) return { kind: "Keydown", key: String(j.Keydown[0]), handler: String(j.Keydown[1]) };
    }
    return null;
  }
  function subKey(s) {
    return s.kind === "Every" ? `Every|${s.ms}|${s.handler}` : `Keydown|${s.key}|${s.handler}`;
  }
  function wireSub(s) {
    if (s.kind === "Every") {
      const id = setInterval(() => invoke(s.handler, ""), s.ms);
      return () => clearInterval(id);
    }
    // Keydown: a document-level listener for one key.
    const listener = (ev) => {
      if (ev.key === s.key) invoke(s.handler, s.key);
    };
    document.addEventListener("keydown", listener);
    return () => document.removeEventListener("keydown", listener);
  }
  function syncSubs() {
    if (typeof exports.vyrnSubs !== "function") return;
    let list;
    try {
      list = JSON.parse(exports.vyrnSubs());
    } catch {
      return;
    }
    const next = (list || []).map(parseSub).filter(Boolean);
    const nextKeys = new Set(next.map(subKey));
    for (const s of next) {
      const k = subKey(s);
      if (!activeSubs.has(k)) activeSubs.set(k, wireSub(s));
    }
    for (const k of [...activeSubs.keys()]) {
      if (!nextKeys.has(k)) {
        activeSubs.get(k)();
        activeSubs.delete(k);
      }
    }
  }

  // --- the render loop ----------------------------------------------------
  function readView() {
    return parseHtml(JSON.parse(exports.vyrnView()));
  }

  function rerender() {
    if (destroyed) return;
    if (hasPatch) {
      patchTick();
      runEffectsDom();
      syncSubs();
      return;
    }
    const next = readView();
    patchChildren(mountEl, [current], [next]);
    current = next;
    runEffects();
    syncSubs();
  }

  // Re-attach this instance's view to a NEW mount element after a soft nav
  // (RFC-0067). The wasm instance — and all of its module state — is untouched;
  // only the per-mount DOM / effect / subscription / delegated-event state is
  // reset and rebuilt against the fresh node. For the patch protocol we
  // deliberately rebuild from the FULL `vyrnView()` rather than `vyrnPatch()`:
  // the new mount node is empty, but wasm's retained `lastTree` still equals the
  // current view (a navigation never changes module state), so a full paint
  // restores the DOM↔`lastTree` invariant the op stream relies on — no need to
  // reset wasm-side state we cannot reach from here. Subsequent `rerender()`s
  // then patch from `rootDom` exactly as before.
  function remount(newEl) {
    if (destroyed) return;
    // Tear down the OLD mount's DOM-bound state (its element is being discarded).
    for (const [, teardown] of activeSubs) teardown();
    activeSubs.clear();
    for (const [dom, info] of activeEffects) {
      if (typeof info.cleanup === "function") info.cleanup(dom);
    }
    activeEffects.clear();
    wiredEvents.clear(); // delegated listeners re-attach to the new element
    current = null;
    rootDom = null;
    // Rebuild from the full view against the new element.
    mountEl = newEl;
    mountEl.textContent = "";
    current = readView();
    const dom = createNode(current);
    mountEl.appendChild(dom);
    rootDom = dom; // the patch loop's nodeAt() resolves from here after re-mount
    runEffects();
    syncSubs();
  }

  // initial mount
  mountEl.textContent = "";
  if (hasPatch) {
    // Boot the patch loop: start from an empty root, then the FIRST patch (a
    // diff of `Empty` against the initial view) builds the tree — so the boot
    // primes wasm's retained `lastTree` and constructs the DOM in one step,
    // through the same op applier every later render uses.
    rootDom = document.createComment("");
    mountEl.appendChild(rootDom);
    patchTick();
    runEffectsDom();
    syncSubs();
  } else {
    current = readView();
    mountEl.appendChild(createNode(current));
    runEffects();
    syncSubs();
  }

  return {
    exports,
    rerender,
    // Re-attach the view to a new mount node across a soft nav, keeping this
    // wasm instance (and its module state) alive — RFC-0067 island re-mount.
    remount,
    // Register an imperative effect after mount (also settable via opts.effects).
    effect(name, fn) {
      effectRegistry[name] = fn;
    },
    destroy() {
      destroyed = true;
      for (const [, teardown] of activeSubs) teardown();
      activeSubs.clear();
      for (const [dom, info] of activeEffects) {
        if (typeof info.cleanup === "function") info.cleanup(dom);
      }
      activeEffects.clear();
      mountEl.textContent = "";
    },
  };
}
