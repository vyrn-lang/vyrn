const escapeHtml = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

export function highlight(src, spans) {
  let out = "";
  let at = 0;
  for (const [start, len, cls] of spans) {
    if (start < at) continue;
    out += escapeHtml(src.slice(at, start));
    out += '<span class="' + cls + '">' + escapeHtml(src.slice(start, start + len)) + "</span>";
    at = start + len;
  }
  // A <pre> swallows one trailing newline; without this the layer's last line sits above the textarea's.
  return out + escapeHtml(src.slice(at)) + "\n";
}

export function scrollLayer(textarea, code) {
  const pre = code.parentElement;
  pre.scrollTop = textarea.scrollTop;
  pre.scrollLeft = textarea.scrollLeft;
}

export function showDiagnostics(pane, diags, pickLine) {
  pane.textContent = "";
  if (!diags || !diags.length) {
    pane.hidden = true;
    return;
  }
  for (const d of diags) {
    const row = document.createElement("button");
    row.type = "button";
    row.className = "diag " + d.severity;
    const where = document.createElement("span");
    where.className = "at";
    where.textContent = d.col > 0 ? `${d.line}:${d.col}` : String(d.line);
    row.append(where, document.createTextNode(d.message));
    if (d.note) {
      const note = document.createElement("span");
      note.className = "note";
      note.textContent = d.note;
      row.append(note);
    }
    row.addEventListener("click", () => pickLine(d.line));
    pane.append(row);
  }
  pane.hidden = false;
}
