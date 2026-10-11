import { $, $$ } from "./dom.js";

let tabGroups = 0;

const hasFocusable = (el) => Boolean($("a[href], button, input, select, textarea, [tabindex]", el));

export function tabsWidget(root) {
  const tabs = $$("[data-tab]", root);
  const panes = $$("[data-pane]", root);
  if (!tabs.length || !panes.length) return;
  const group = "tabs" + (tabGroups += 1);
  const keyOf = (tab) => tab.getAttribute("data-tab");

  const list = tabs[0].parentElement;
  if (list) list.setAttribute("role", "tablist");
  tabs.forEach((tab, i) => {
    tab.id = group + "-tab-" + i;
    tab.setAttribute("role", "tab");
    const pane = panes.find((p) => p.getAttribute("data-pane") === keyOf(tab));
    if (!pane) return;
    pane.id = group + "-panel-" + i;
    pane.setAttribute("role", "tabpanel");
    pane.setAttribute("aria-labelledby", tab.id);
    tab.setAttribute("aria-controls", pane.id);
    if (!hasFocusable(pane)) pane.tabIndex = 0;
  });

  const select = (id, focus) => {
    for (const tab of tabs) {
      const on = keyOf(tab) === id;
      tab.setAttribute("aria-selected", String(on));
      tab.tabIndex = on ? 0 : -1;
      if (on && focus) tab.focus();
    }
    for (const pane of panes) pane.hidden = pane.getAttribute("data-pane") !== id;
  };

  const step = (from, by) => select(keyOf(tabs[(from + by + tabs.length) % tabs.length]), true);

  tabs.forEach((tab, i) => {
    tab.addEventListener("click", () => select(keyOf(tab), false));
    tab.addEventListener("keydown", (e) => {
      if (e.key === "ArrowRight") return e.preventDefault(), step(i, 1);
      if (e.key === "ArrowLeft") return e.preventDefault(), step(i, -1);
      if (e.key === "Home") return e.preventDefault(), select(keyOf(tabs[0]), true);
      if (e.key === "End") return e.preventDefault(), select(keyOf(tabs[tabs.length - 1]), true);
    });
  });

  select(keyOf(tabs[0]), false);
}
