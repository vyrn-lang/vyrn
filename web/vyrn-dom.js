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
// The runtime instance.
// ---------------------------------------------------------------------------
export async function mount(wasmBytes, mountEl, opts = {}) {
  const { effects = {}, ...runHooks } = opts;
  const result = await runVyrn(wasmBytes, {
    ...runHooks,
    // vyrnView / vyrnSubs return Strings; name them so the export glue decodes.
    exportReturns: { vyrnView: "string", vyrnSubs: "string", ...(runHooks.exportReturns || {}) },
  });
  const exports = result.exports;
  if (typeof exports.vyrnView !== "function") {
    throw new Error(
      "vyrn-dom: the module does not export `vyrnView` — add " +
        "`export extern fn vyrnView() -> String { return toJson(view()) }`"
    );
  }

  const effectRegistry = { ...effects };
  const activeEffects = new Map(); // domNode -> { name, cleanup }
  const activeSubs = new Map(); // subKey -> teardown fn
  const wiredEvents = new Set(); // event types with a delegated root listener
  let current = null; // the retained root vnode
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
    const next = readView();
    patchChildren(mountEl, [current], [next]);
    current = next;
    runEffects();
    syncSubs();
  }

  // initial mount
  current = readView();
  mountEl.textContent = "";
  mountEl.appendChild(createNode(current));
  runEffects();
  syncSubs();

  return {
    exports,
    rerender,
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
