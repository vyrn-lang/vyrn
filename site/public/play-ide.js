import { $, $$ } from "./dom.js";
import { highlight, scrollLayer } from "./play-paint.js";
import { encodeSource, projectFromHash } from "./play-share.js";
import { zipOf } from "./play-zip.js";
import { editorKeys } from "./play-keys.js";
import { openMenu } from "./play-menu.js";
import { bindIntel } from "./play-intel.js";

const KEY_FILES = "vyrn.play.files.v1";
const KEY_FOLDERS = "vyrn.play.folders.v1";
const KEY_LAYOUT = "vyrn.play.layout.v1";
const KEY_SESSION = "vyrn.play.session.v1";
const WELCOME = "__welcome";

const makeGroup = (els) => Object.assign({ tabs: [], active: null }, els);
const labelOf = (name) => (name === WELCOME ? "Welcome" : name.slice(name.lastIndexOf("/") + 1));

function read(k, fallback) {
  try {
    const v = JSON.parse(localStorage.getItem(k));
    return v === null || v === undefined ? fallback : v;
  } catch (err) {
    return fallback;
  }
}

function write(k, v) {
  try {
    localStorage.setItem(k, JSON.stringify(v));
  } catch (err) {}
}

function bindProblems(root, diagPane) {
  const ptabs = $$("[data-play-ptab]", root);
  const panes = $$("[data-play-pane]", root);
  const badge = $("[data-play-badge]", root);
  const errsEl = $("[data-play-errs]", root);
  const noprob = $("[data-play-noprob]", root);
  const showPane = (k) => {
    for (const t of ptabs) t.classList.toggle("on", t.dataset.playPtab === k);
    for (const p of panes) p.hidden = p.dataset.playPane !== k;
  };
  for (const t of ptabs) t.addEventListener("click", () => showPane(t.dataset.playPtab));
  if (!ptabs.length || !diagPane) return;
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

export function mountIde(root, core) {
  const { input, layer, editor, runBtn, stdin, examples, diagPane, paint, scheduleCheck, spansOf, ready, startCompiler, api } = core;
  bindProblems(root, diagPane);
  const tabsBox = $("[data-play-tabs]", root);
  const filesBox = $("[data-play-files]", root);
  const wrapEl = $(".idewrap", root);
  if (!tabsBox || !filesBox || !wrapEl) return null;
  const gutter = $("[data-play-gutter]", root);
  const lncol = $("[data-play-lncol]", root);
  const crumbs = $("[data-play-crumbs]", root);
  const curline = $("[data-play-curline]", root);
  const minimap = $("[data-play-minimap]", root);
  const mmtext = $("[data-play-mmtext]", root);
  const mmview = $("[data-play-mmview]", root);
  const welcomeEl = $("[data-play-welcome]", root);
  const uploadInput = $("[data-play-uploadinput]", root);

  const myFiles = new Map(read(KEY_FILES, []).map((f) => [f.name, f.text]));
  let userFiles = myFiles;
  const isShared = () => userFiles !== myFiles;
  const scopeEl = $("[data-play-scope]", root);
  function setFiles(map) {
    if (isShared()) sharedFiles = map;
    else myFiles.clear(), map.forEach((v, k) => myFiles.set(k, v));
    userFiles = isShared() ? sharedFiles : myFiles;
  }
  let sharedFiles = null;
  const folders = new Set(read(KEY_FOLDERS, []));
  const layout = Object.assign({ side: 224, panel: 200, split: 0.5 }, read(KEY_LAYOUT, {}));
  const collapsed = new Set();
  let saveTimer = 0;
  function persistFiles() {
    if (isShared()) return;
    clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      write(KEY_FILES, [...userFiles].map(([name, text]) => ({ name, text })));
      write(KEY_FOLDERS, [...folders]);
    }, 150);
  }
  function persistSession() {
    if (isShared()) return;
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

  function freshName(base, into) {
    const map = into || userFiles;
    if (!map.has(base)) return base;
    const m = base.match(/^(.*?)(\.[^./]*)?$/);
    const stem = m[1];
    const ext = m[2] || "";
    for (let i = 2; ; i++) {
      const name = stem + "-" + i + ext;
      if (!map.has(name)) return name;
    }
  }

  const gA = makeGroup({ strip: tabsBox, input, layer, gutter, curline, editorEl: editor, main: $(".idemain", root), runBtn, minimap, mmtext, mmview });
  let gB = null;
  let focusedG = gA;

  function updateRunState() {
    const busy = runBtn.dataset.state === "busy";
    for (const g of groups()) {
      if (!g.runBtn) continue;
      const want = !ready() || busy || !g.active || g.active === WELCOME;
      // Only on change: the observer below watches this button, so every write re-enters here.
      if (g.runBtn.disabled !== want) g.runBtn.disabled = want;
    }
  }
  new MutationObserver(updateRunState).observe(runBtn, { attributes: true, attributeFilter: ["disabled", "data-state"] });
  const contentOf = (name) => userFiles.get(name) || "";
  const groups = () => (gB ? [gA, gB] : [gA]);

  function paintG(g) {
    if (g === gA) {
      paint();
      return;
    }
    g.layer.innerHTML = highlight(g.input.value, spansOf(g.input.value));
    scrollLayer(g.input, g.layer);
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
    if (scopeEl) scopeEl.textContent = isShared() ? "Shared project" : "Explorer";
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
      scheduleCheck();
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

  function bindGroup(g) {
    bindIntel(g.input, g.editorEl, api);
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
        scheduleCheck();
      }
      syncG(g);
    });
    ["keyup", "click"].forEach((ev) => g.input.addEventListener(ev, () => syncG(g)));
    g.input.addEventListener("scroll", () => {
      if (g.gutter) g.gutter.scrollTop = g.input.scrollTop;
      scrollLayer(g.input, g.layer);
      syncG(g);
    });
    g.input.addEventListener("keydown", (e) => editorKeys(g.input, e, g !== gA));
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

  const openElsewhere = (name) => window.open(location.pathname + "#c=" + encodeSource(contentOf(name)), "_blank");

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
        if (fromG && g === fromG && drag.index >= 0) {
          const from = g.tabs.indexOf(name);
          const to = drag.index;
          if (from >= 0 && to !== from && to !== from + 1) {
            g.tabs.splice(from, 1);
            g.tabs.splice(to > from ? to - 1 : to, 0, name);
            renderTabsG(g);
          }
        }
        // The main box, not the editor: the Welcome view hides the editor, and its rectangle is empty.
        const er = g.main.getBoundingClientRect();
        drag.split = !gB && g === gA && name !== WELCOME && ev.clientX > er.left + er.width * 0.6 && ev.clientY > er.top && ev.clientY < er.bottom;
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
      // Late, so the click that follows this pointerup still sees drag.moved.
      setTimeout(() => (drag = null));
    };
    addEventListener("pointermove", move);
    addEventListener("pointerup", up);
  }
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

  function treeOf() {
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
      setFiles(rebuilt);
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
      setFiles(rebuilt);
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

  const shareHash = () => {
    const payload = { v: 1, files: [...userFiles], open: focusedG.active === WELCOME ? null : focusedG.active };
    return "p=" + encodeSource(JSON.stringify(payload));
  };

  function openShared(files, open) {
    sharedFiles = new Map(files);
    userFiles = sharedFiles;
    for (const g of groups()) {
      g.tabs = [];
      g.active = null;
    }
    if (gB) unsplit();
    wrapEl.classList.add("shared");
    const first = open && sharedFiles.has(open) ? open : sharedFiles.keys().next().value;
    activateIn(gA, first || WELCOME);
  }
  function leaveShared(keep) {
    if (!isShared()) {
      wrapEl.classList.remove("shared");
      syncChrome();
      return;
    }
    const carried = keep ? [...sharedFiles] : [];
    let landed = null;
    const wasOpen = gA.active;
    sharedFiles = null;
    userFiles = myFiles;
    for (const g of groups()) {
      g.tabs = [];
      g.active = null;
    }
    if (gB) unsplit();
    for (const [name, text] of carried) {
      const fresh = freshName(name, myFiles);
      myFiles.set(fresh, text);
      if (name === wasOpen) landed = fresh;
    }
    if (carried.length) persistFiles();
    wrapEl.classList.remove("shared");
    history.replaceState(null, "", location.pathname);
    const fallback = myFiles.size ? myFiles.keys().next().value : WELCOME;
    activateIn(gA, landed || fallback);
  }
  for (const b of $$("[data-play-keep]", root)) b.addEventListener("click", () => leaveShared(true));
  for (const b of $$("[data-play-discard]", root)) b.addEventListener("click", () => leaveShared(false));

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

  const onPlayReady = () => {
    for (const g of groups()) {
      paintG(g);
      syncG(g);
    }
    updateRunState();
  };

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

  function firstLight() {
    const session = read(KEY_SESSION, null);
    const project = projectFromHash();
    if (project) {
      openShared(project.files, project.open);
      return;
    }
    if (location.hash.startsWith("#c=") && input.value) {
      openShared([["shared.vyrn", input.value]], "shared.vyrn");
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
  }
  queueMicrotask(firstLight);

  addEventListener("pageshow", (e) => {
    // A mount that soft navigation detached keeps this listener; only the live one wakes.
    if (!e.persisted || !root.isConnected) return;
    if (!focusedG.active) firstLight();
    startCompiler();
  });

  return { srcHost: () => focusedG.input, onPlayReady, shareHash };
}
