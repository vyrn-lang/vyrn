import { $, $$ } from "./dom.js";

export function radarWidget(root) {
  const svg = $("svg.radar", root);
  if (!svg) return;

  const tabs = $$("[data-tab]");
  for (const link of $$(".names a, .wedges a", svg)) {
    link.addEventListener("click", () => {
      const key = (link.getAttribute("href") || "").slice(1);
      const tab = tabs.find((t) => t.getAttribute("data-tab") === key);
      if (tab) tab.click();
    });
  }

  for (const btn of $$("[data-series-toggle]", root)) {
    const group = $('g[data-series="' + btn.dataset.seriesToggle + '"]', svg);
    if (!group) continue;
    btn.addEventListener("click", () => {
      const on = btn.getAttribute("aria-pressed") !== "true";
      btn.setAttribute("aria-pressed", String(on));
      group.classList.toggle("off", !on);
    });
  }
}
