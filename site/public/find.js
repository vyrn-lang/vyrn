import { $, $$ } from "./dom.js";

const SITE = new URL(".", import.meta.url);
const SECTIONS = ["Docs", "Reference", "Packages", "Releases"];

export function bindFind() {
  const box = $("[data-find]");
  const field = $("[data-find-input]");
  const list = $("[data-find-results]");
  const note = $("[data-find-note]");
  if (!box || !field || !list) return;
  let index = null;
  let loading = null;
  let cursor = -1;
  let sect = "";
  const tabs = $$("[data-find-tabs] [data-sect]", box);
  const idle = note.innerHTML;
  let cameFrom = null;

  function load() {
    if (index) return Promise.resolve();
    if (loading) return loading;
    loading = fetch(new URL("search.json", SITE))
      .then((r) => (r.ok ? r.json() : Promise.reject(new Error(r.status))))
      .then((rows) => {
        index = rows;
        loading = null;
      })
      .catch((err) => {
        loading = null;
        note.textContent = "The index did not load. Every page is still linked from the navigation.";
        console.error("the search index did not load", err);
      });
    return loading;
  }

  function search(q) {
    const needle = q.toLowerCase();
    const hits = [];
    for (const r of index) {
      if (sect && r.s !== sect) continue;
      const at = r.t.toLowerCase().indexOf(needle);
      let rank = -1;
      if (at === 0) rank = 0;
      else if (at > 0) rank = 1;
      else if (r.d.toLowerCase().includes(needle)) rank = 2;
      if (rank >= 0) hits.push({ r, rank });
    }
    hits.sort((a, b) => a.rank - b.rank);
    const best = hits.slice(0, 40).map((h) => h.r);
    best.sort((a, b) => SECTIONS.indexOf(a.s) - SECTIONS.indexOf(b.s));
    return best;
  }

  function anchorOf(r) {
    if (r.s !== "Reference") return "";
    const cut = r.t.indexOf(" — std/");
    return cut > 0 ? "#e-" + r.t.slice(0, cut) : "";
  }

  function render(q) {
    list.textContent = "";
    cursor = -1;
    if (!q || !index) {
      if (!q) note.innerHTML = idle;
      return;
    }
    const hits = search(q);
    note.textContent = hits.length
      ? `${hits.length} result${hits.length === 1 ? "" : "s"}.`
      : sect
        ? `Nothing in ${sect} matches "${q}".`
        : `Nothing matches "${q}".`;
    let section = null;
    for (const r of hits) {
      if (r.s !== section) {
        section = r.s;
        const h = document.createElement("p");
        h.className = "sect";
        h.textContent = section;
        list.append(h);
      }
      const a = document.createElement("a");
      a.className = "hit";
      a.href = new URL(r.u.slice(1) + anchorOf(r), SITE).href;
      a.setAttribute("role", "option");
      a.setAttribute("aria-selected", "false");
      a.textContent = r.t;
      if (r.d) {
        const d = document.createElement("span");
        d.className = "d";
        d.textContent = r.d;
        a.append(d);
      }
      list.append(a);
    }
  }

  function move(step) {
    const hits = $$(".hit", list);
    if (!hits.length) return;
    if (cursor >= 0) hits[cursor].setAttribute("aria-selected", "false");
    cursor = (cursor + step + hits.length) % hits.length;
    hits[cursor].setAttribute("aria-selected", "true");
    hits[cursor].scrollIntoView({ block: "nearest" });
  }

  function setSect(v) {
    sect = v;
    for (const t of tabs) t.classList.toggle("on", t.dataset.sect === v);
    render(field.value);
    field.focus();
  }
  for (const t of tabs) t.addEventListener("click", () => setSect(t.dataset.sect));

  note.addEventListener("click", (e) => {
    const b = e.target.closest("[data-try]");
    if (!b) return;
    field.value = b.textContent;
    setSect("");
  });

  function open() {
    if (!box.hidden) return;
    cameFrom = document.activeElement;
    box.hidden = false;
    field.value = "";
    setSect("");
    field.focus();
    load().then(() => render(field.value));
  }

  function close() {
    if (box.hidden) return;
    box.hidden = true;
    list.textContent = "";
    if (cameFrom && cameFrom.isConnected) cameFrom.focus();
    cameFrom = null;
  }

  for (const btn of $$("[data-find-open]")) btn.addEventListener("click", open);

  addEventListener("keydown", (e) => {
    const el = document.activeElement;
    const typing = /^(input|textarea|select)$/i.test(el.tagName) || el.isContentEditable;
    if (e.key === "/" && !typing && !e.metaKey && !e.ctrlKey && !e.altKey) {
      e.preventDefault();
      open();
    } else if (e.key === "Escape" && !box.hidden) {
      e.preventDefault();
      close();
    }
  });

  box.addEventListener("mousedown", (e) => {
    if (e.target === box) close();
  });

  field.addEventListener("input", () => render(field.value));
  field.addEventListener("keydown", (e) => {
    if (e.key === "Tab") {
      e.preventDefault();
      const order = tabs.map((t) => t.dataset.sect);
      const at = order.indexOf(sect);
      const step = e.shiftKey ? -1 : 1;
      setSect(order[(at + step + order.length) % order.length]);
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      move(1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      move(-1);
    } else if (e.key === "Enter") {
      const hits = $$(".hit", list);
      const pick = cursor >= 0 ? hits[cursor] : hits[0];
      if (pick) {
        e.preventDefault();
        // Not close(): it would focus an element the navigation is about to replace.
        box.hidden = true;
        list.textContent = "";
        cameFrom = null;
        pick.click();
      }
    }
  });

  box.addEventListener("keydown", (e) => {
    if (e.key === "Tab") {
      e.preventDefault();
      field.focus();
    }
  });
}
