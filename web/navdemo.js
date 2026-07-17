// navdemo.js — the persistent driver for the vyrn-nav morph demo.
//
// Loaded once (a module <script src>), it survives every soft navigation: the
// morph never re-executes scripts, so this module — and its `window.__navMarker`
// and its nav counter — persist across pages. A hard reload, by contrast, loads
// a fresh module and resets both. That contrast is the "no full reload" proof.
//
// It also demonstrates the RFC-0034 event surface: it repaints on every
// `vyrn:nav-end`, and reflects the `?id=` of the detail page into its heading.

if (!window.__navMarker) {
  window.__navMarker = "boot-" + Math.random().toString(36).slice(2, 8);
}

let softNavs = 0;

function paint() {
  const marker = document.getElementById("marker");
  if (marker) marker.textContent = window.__navMarker + " · soft-navs since load: " + softNavs;

  const id = new URLSearchParams(location.search).get("id");
  const title = document.getElementById("book-title");
  if (title && id) title.textContent = "Book #" + id;
}

document.addEventListener("vyrn:nav-end", () => {
  softNavs += 1;
  paint();
});

paint();
