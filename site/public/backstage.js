// The backstage's one script, and the only page that loads it is /backstage.
//
// The consumer pages load `widgets.js`, which also carries the soft navigator;
// this section navigates hard on purpose, so it takes this file instead. What it
// does is one thing: the citation ring on the index page lights one record's own
// chords — what it names, and what names it — on hover AND on focus, because
// every node is a link and a keyboard reaches it.
//
// Vyrn placed all 104 nodes and all 723 chords at build time
// (`site/app/corpus.vyrn`). Nothing here computes geometry, and with this file
// blocked the ring is still the whole corpus at rest, with the same list under
// it that it draws.

const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));

function ringWidget(svg) {
  const nodes = $$("g.rnode", svg);
  const chords = $$("path.chord", svg);
  const listOf = (el, key) => (el.dataset[key] || "").split(" ").filter(Boolean);

  const lift = (id) => {
    svg.classList.toggle("lit", Boolean(id));
    const node = id ? nodes.find((n) => n.dataset.n === id) : null;
    // Two directions, kept apart: what this record names is drawn one way and
    // what names it the other, which is the whole question the ring answers.
    const cites = node ? listOf(node, "cites") : [];
    const citedBy = node ? listOf(node, "citedby") : [];
    for (const c of chords) {
      c.classList.toggle("out", Boolean(id) && c.dataset.a === id);
      c.classList.toggle("in", Boolean(id) && c.dataset.b === id);
    }
    for (const n of nodes) {
      n.classList.toggle("on", n.dataset.n === id);
      n.classList.toggle("names", cites.includes(n.dataset.n));
      n.classList.toggle("named", citedBy.includes(n.dataset.n));
    }
  };

  for (const n of nodes) {
    n.addEventListener("pointerenter", () => lift(n.dataset.n));
    n.addEventListener("pointerleave", () => lift(null));
    n.addEventListener("focusin", () => lift(n.dataset.n));
    n.addEventListener("focusout", () => lift(null));
  }
}

for (const svg of $$("svg.cites")) ringWidget(svg);
