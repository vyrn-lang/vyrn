import { $, $$ } from "./dom.js";

// Escape closes every lane a tap opened.
export function closeLanes() {
  for (const lane of $$(".lanes li[data-open]")) delete lane.dataset.open;
}

// The readout copies the item's own groups, so the page has one source for each number.
function show(readout, item) {
  const name = document.createElement("b");
  name.textContent = $(".lname", item).textContent;
  readout.replaceChildren(name, $(".rd", item).cloneNode(true));
}

// One box: a readout, its lanes, and the sparkline points that share the readout.
export function lanesWidget(box) {
  const readout = $(".readout", box);
  const lanes = $$(".lanes li", box);
  const items = [...lanes, ...$$(".spark .pt", box)];
  // A segment too narrow for its label hides the label rather than clip it.
  new ResizeObserver(() => {
    for (const seg of $$(".lbar > span", box)) seg.classList.toggle("tight", seg.scrollWidth > seg.clientWidth);
  }).observe(box);
  for (const item of items) {
    item.addEventListener("pointerenter", () => show(readout, item));
    item.addEventListener("focusin", () => show(readout, item));
    item.addEventListener("pointerleave", () => {
      const held = items.find((i) => i.contains(document.activeElement));
      show(readout, held || lanes[0]);
    });
  }
  for (const lane of lanes) {
    lane.addEventListener("click", (e) => {
      if (!e.target.closest("a")) lane.toggleAttribute("data-open");
    });
    lane.addEventListener("keydown", (e) => {
      if (e.target !== lane || (e.key !== "Enter" && e.key !== " ")) return;
      e.preventDefault();
      lane.toggleAttribute("data-open");
    });
  }
}
