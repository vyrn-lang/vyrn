import { $$ } from "./dom.js";

export function graphWidget(svg) {
  const nodes = $$("g.node", svg);
  const wires = $$("path.wire", svg);
  const listOf = (el, key) => (el.dataset[key] || "").split(" ").filter(Boolean);

  const lift = (name) => {
    svg.classList.toggle("lit", Boolean(name));
    const node = name ? nodes.find((n) => n.dataset.m === name) : null;
    const near = node ? listOf(node, "uses").concat(listOf(node, "usedby")) : [];
    for (const w of wires) {
      w.classList.toggle("on", Boolean(name) && (w.dataset.from === name || w.dataset.to === name));
    }
    for (const n of nodes) {
      n.classList.toggle("on", n.dataset.m === name);
      n.classList.toggle("near", near.includes(n.dataset.m));
    }
  };

  for (const n of nodes) {
    n.addEventListener("pointerenter", () => lift(n.dataset.m));
    n.addEventListener("pointerleave", () => lift(null));
    n.addEventListener("focusin", () => lift(n.dataset.m));
    n.addEventListener("focusout", () => lift(null));
  }
}
