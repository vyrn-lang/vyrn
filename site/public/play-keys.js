function lineBounds(t, at) {
  const s0 = t.value.lastIndexOf("\n", at - 1) + 1;
  let e0 = t.value.indexOf("\n", at);
  if (e0 < 0) e0 = t.value.length;
  return [s0, e0];
}

function replaceAll(t, text, caret) {
  t.value = text;
  t.selectionStart = t.selectionEnd = caret;
  t.dispatchEvent(new Event("input", { bubbles: true }));
}

export function editorKeys(t, e, indentsOnTab) {
  const ctrl = e.ctrlKey || e.metaKey;
  if (e.key === "Tab" && indentsOnTab) {
    e.preventDefault();
    t.setRangeText("  ", t.selectionStart, t.selectionEnd, "end");
    t.dispatchEvent(new Event("input", { bubbles: true }));
    return;
  }
  if (e.key === "Enter" && !e.isComposing && !ctrl) {
    e.preventDefault();
    const at = t.selectionStart;
    const before = t.value.slice(0, at);
    const line = before.slice(before.lastIndexOf("\n") + 1);
    const indent = (line.match(/^ */) || [""])[0];
    const opened = /\{\s*$/.test(line);
    const closingNext = t.value.slice(t.selectionEnd).startsWith("}");
    let insert = "\n" + indent + (opened ? "  " : "");
    if (opened && closingNext) insert += "\n" + indent;
    const caret = at + 1 + indent.length + (opened ? 2 : 0);
    t.setRangeText(insert, at, t.selectionEnd, "end");
    t.selectionStart = t.selectionEnd = caret;
    t.dispatchEvent(new Event("input", { bubbles: true }));
    return;
  }
  const [s0, e0] = lineBounds(t, t.selectionStart);
  const line = t.value.slice(s0, e0);
  if (ctrl && !e.shiftKey && e.key.toLowerCase() === "d") {
    e.preventDefault();
    replaceAll(t, t.value.slice(0, e0) + "\n" + line + t.value.slice(e0), t.selectionStart + line.length + 1);
  } else if (ctrl && !e.shiftKey && e.key.toLowerCase() === "y") {
    e.preventDefault();
    const cut = e0 < t.value.length ? e0 + 1 : Math.max(0, s0 - 1);
    replaceAll(t, t.value.slice(0, Math.min(s0, cut)) + t.value.slice(Math.max(e0 + 1, cut)), Math.min(s0, t.value.length));
  } else if (ctrl && e.key === "/") {
    e.preventDefault();
    const on = /^\s*\/\/ ?/.test(line);
    const next = on ? line.replace(/^(\s*)\/\/ ?/, "$1") : line.replace(/^(\s*)/, "$1// ");
    replaceAll(t, t.value.slice(0, s0) + next + t.value.slice(e0), s0 + Math.min(next.length, t.selectionStart - s0 + (on ? -3 : 3)));
  } else if (ctrl && e.shiftKey && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
    e.preventDefault();
    const lines = t.value.split("\n");
    const idx = t.value.slice(0, t.selectionStart).split("\n").length - 1;
    const to = e.key === "ArrowUp" ? idx - 1 : idx + 1;
    if (to < 0 || to >= lines.length) return;
    const col = t.selectionStart - s0;
    [lines[idx], lines[to]] = [lines[to], lines[idx]];
    const text = lines.join("\n");
    let at = 0;
    for (let i = 0; i < to; i++) at += lines[i].length + 1;
    replaceAll(t, text, at + Math.min(col, lines[to].length));
  }
}
