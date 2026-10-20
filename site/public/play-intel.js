// Completion for one editor textarea, served by play.wasm's play_complete. Lines and columns sent to it are 1-based, columns in code points.

const MAX_ITEMS = 60;
const IDENT = /[\p{L}\p{N}_]/u;

let reported = false;

// A failure of play.wasm is a defect, so the first one reaches the console; the editor keeps working without the answer.
function report(err) {
  if (reported) return;
  reported = true;
  console.error("play-intel: play.wasm failed", err);
}

export function lineCol(text, at) {
  const before = text.slice(0, at);
  const start = before.lastIndexOf("\n") + 1;
  return { line: before.split("\n").length, col: [...before.slice(start)].length + 1 };
}

export function prefixAt(text, at) {
  let start = at;
  while (start > 0 && IDENT.test(text[start - 1])) start--;
  return { start, prefix: text.slice(start, at) };
}

// Prefix matches first, then substring matches, each in the order play.wasm sent; one entry per label.
export function rank(items, prefix) {
  const p = prefix.toLowerCase();
  const seen = new Set();
  const head = [];
  const tail = [];
  for (const it of items) {
    if (seen.has(it.label) || it.label === prefix) continue;
    const l = it.label.toLowerCase();
    if (l.startsWith(p)) head.push(it);
    else if (p && l.includes(p)) tail.push(it);
    else continue;
    seen.add(it.label);
  }
  return head.concat(tail).slice(0, MAX_ITEMS);
}

function metrics(t) {
  const cs = getComputedStyle(t);
  const font = `${cs.fontWeight} ${cs.fontSize} ${cs.fontFamily}`;
  const ctx = (metrics.canvas ??= document.createElement("canvas")).getContext("2d");
  ctx.font = font;
  return {
    font,
    lh: parseFloat(cs.lineHeight),
    cw: ctx.measureText("0".repeat(64)).width / 64,
    left: t.clientLeft + parseFloat(cs.paddingLeft),
    top: t.clientTop + parseFloat(cs.paddingTop),
  };
}

// The viewport point of the top-left corner of 1-based (line, col).
function pointOf(t, line, col) {
  const m = metrics(t);
  const r = t.getBoundingClientRect();
  return { x: r.left + m.left + (col - 1) * m.cw - t.scrollLeft, y: r.top + m.top + (line - 1) * m.lh - t.scrollTop, lh: m.lh };
}

function floating(cls) {
  const el = document.createElement("div");
  el.className = cls;
  el.hidden = true;
  document.body.append(el);
  return el;
}

function place(el, x, y, lh, above) {
  el.hidden = false;
  const r = el.getBoundingClientRect();
  el.style.left = Math.max(4, Math.min(x, innerWidth - r.width - 4)) + "px";
  const below = y + lh + 2;
  el.style.top = (above || below + r.height > innerHeight ? Math.max(4, y - r.height - 2) : below) + "px";
}

export function bindIntel(t, api) {
  const menu = floating("intel picks");
  menu.setAttribute("role", "listbox");
  let shown = [];
  let all = [];
  let sel = 0;
  let start = 0;
  let accepting = false;

  function closeMenu() {
    menu.hidden = true;
    shown = [];
  }

  function renderMenu() {
    menu.textContent = "";
    shown.forEach((it, i) => {
      const row = document.createElement("div");
      row.className = "it" + (i === sel ? " on" : "");
      row.setAttribute("role", "option");
      const kind = document.createElement("span");
      kind.className = "ik " + it.kind;
      kind.textContent = it.kind[0];
      const label = document.createElement("span");
      label.className = "il";
      label.textContent = it.label;
      const detail = document.createElement("span");
      detail.className = "id";
      detail.textContent = it.kind === "keyword" || it.kind === "module" ? "" : it.detail.split("\n")[0];
      row.append(kind, label, detail);
      row.addEventListener("pointerdown", (e) => {
        e.preventDefault();
        sel = i;
        accept();
      });
      menu.append(row);
    });
    const on = menu.children[sel];
    if (on) on.scrollIntoView({ block: "nearest" });
  }

  function refilter() {
    const { prefix } = prefixAt(t.value, t.selectionStart);
    shown = rank(all, prefix);
    sel = 0;
    if (!shown.length) return closeMenu();
    renderMenu();
    const { line, col } = lineCol(t.value, start);
    const p = pointOf(t, line, col);
    place(menu, p.x - 22, p.y, p.lh, false);
  }

  function openMenu() {
    const play = api();
    if (!play) return;
    const { line, col } = lineCol(t.value, t.selectionStart);
    try {
      all = play.complete(t.value, line, col).items || [];
    } catch (err) {
      report(err);
      all = [];
    }
    start = prefixAt(t.value, t.selectionStart).start;
    refilter();
  }

  function accept() {
    const it = shown[sel];
    closeMenu();
    if (!it) return;
    accepting = true;
    t.setSelectionRange(start, t.selectionStart);
    if (!document.execCommand || !document.execCommand("insertText", false, it.label)) {
      t.setRangeText(it.label, start, t.selectionEnd, "end");
      t.dispatchEvent(new Event("input", { bubbles: true }));
    }
    accepting = false;
  }

  t.addEventListener("input", (e) => {
    if (accepting) return;
    const at = t.selectionStart;
    const ch = t.value[at - 1] || "";
    const lineHead = t.value.slice(t.value.lastIndexOf("\n", at - 1) + 1, at);
    const typed = e.inputType === "insertText";
    const { prefix, start: from } = prefixAt(t.value, at);
    const deleted = (e.inputType || "").startsWith("delete");
    if (!menu.hidden && ((typed && IDENT.test(ch)) || (deleted && from === start))) return refilter();
    if (!typed || lineHead.includes("//")) return closeMenu();
    const wordStart = prefix.length === 1 && !/\d/.test(prefix);
    const member = ch === ".";
    const module = ch === "/" && /from\s+"std\/[\w/]*$/.test(lineHead);
    if (wordStart || member || module) openMenu();
    else closeMenu();
  });

  t.addEventListener(
    "keydown",
    (e) => {
      if (e.key === "Escape") {
        if (menu.hidden) return;
        closeMenu();
      } else if (e.key === " " && e.ctrlKey) {
        openMenu();
      } else if (menu.hidden) {
        return;
      } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        sel = (sel + (e.key === "ArrowDown" ? 1 : shown.length - 1)) % shown.length;
        renderMenu();
      } else if ((e.key === "Enter" || e.key === "Tab") && !e.ctrlKey && !e.metaKey) {
        accept();
      } else {
        return;
      }
      e.preventDefault();
      e.stopImmediatePropagation();
    },
    true,
  );

  t.addEventListener("click", closeMenu);
  t.addEventListener("keyup", (e) => {
    if (e.key === "ArrowLeft" || e.key === "ArrowRight" || e.key === "Home" || e.key === "End") closeMenu();
  });
  t.addEventListener("blur", closeMenu);
  t.addEventListener("scroll", closeMenu);
}
