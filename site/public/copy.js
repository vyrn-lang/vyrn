import { $, $$, pinWidth } from "./dom.js";

export async function writeClipboard(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch (err) {
    // The async API refuses an unfocused or embedded document, where a selection copy still works.
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

export function copyButtons() {
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

export function copyPageButtons() {
  for (const link of $$("a[data-copy-md]")) {
    let fetchFailed = false;
    // Read once: a second click inside the 1.6 s would read "Copied" back as the label.
    const was = link.textContent;
    link.addEventListener("click", async (e) => {
      if (fetchFailed || !navigator.clipboard) return;
      e.preventDefault();
      pinWidth(link);
      const res = await fetch(link.href).catch(() => null);
      const md = res && res.ok ? await res.text() : "";
      fetchFailed = !md;
      link.textContent = md && (await writeClipboard(md)) ? "Copied" : "Open it instead";
      setTimeout(() => (link.textContent = was), 1600);
    });
  }
}

function inlineCode(node) {
  const el = node && node.closest ? node.closest("code") : null;
  if (!el || el.closest("pre, .cmd, .lines")) return null;
  return el;
}

export const codeText = (el) => el.textContent.replace(/\s+/g, " ").trim();

export function isPlainCopyClick(down, up, selected) {
  if (up.detail >= 2) return false;
  if (selected) return false;
  if (!down) return true;
  return Math.abs(up.x - down.x) <= 3 && Math.abs(up.y - down.y) <= 3;
}

export function announce(msg) {
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

async function copySpan(el) {
  const text = codeText(el);
  const ok = await writeClipboard(text);
  el.classList.add(ok ? "copied" : "copyfail");
  setTimeout(() => el.classList.remove("copied", "copyfail"), 900);
  if (ok) {
    announce("Copied " + text);
  } else {
    getSelection().selectAllChildren(el);
    announce("Copy failed. " + text + " is selected — press Control C.");
  }
}

export function markCopyable() {
  for (const el of $$("code")) {
    if (inlineCode(el)) el.classList.add("copyable");
  }
}

export function bindInlineCopy() {
  let downAt = null;
  document.addEventListener("pointerdown", (e) => {
    downAt = inlineCode(e.target) ? { x: e.clientX, y: e.clientY } : null;
  });
  document.addEventListener("click", (e) => {
    const el = inlineCode(e.target);
    if (!el) return;
    const sel = getSelection();
    const selected = !!sel && !sel.isCollapsed && sel.toString().length > 0;
    if (isPlainCopyClick(downAt, e, selected)) copySpan(el);
  });
}
