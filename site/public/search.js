import { $, $$ } from "./dom.js";

const SITE = new URL(".", import.meta.url);

export function matchingExports(row, q, limit) {
  const names = (row.dataset.e || "").split(" ").filter(Boolean);
  return names.filter((n) => n.toLowerCase().includes(q)).slice(0, limit);
}

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

export function moduleSearch() {
  const input = $("[data-search-input]");
  const list = $("[data-search-list]");
  const count = $("[data-search-count]");
  if (!input || !list) return;
  const rows = $$("li", list);
  const total = rows.length;
  const noun = count?.dataset.searchNoun || "module";
  const hasExports = rows.some((r) => r.dataset.e);

  const apply = () => {
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
      hits += names.length;
      if (names.length) row.appendChild(exportHits(row, names));
    }
    if (!count) return;
    if (terms.length === 0) {
      count.textContent = total + " " + noun + "s";
    } else if (shown === 0) {
      count.textContent = "no " + noun + " matches " + input.value.trim();
    } else {
      const tail = hasExports && hits > 0 ? ", " + hits + (hits === 1 ? " export" : " exports") : "";
      count.textContent = shown + (shown === 1 ? " " + noun : " " + noun + "s") + tail;
    }
  };

  input.addEventListener("input", apply);
  if (input.value) apply();
}
