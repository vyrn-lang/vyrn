import { $, $$ } from "./dom.js";

const pad = (n) => (n < 10 ? "0" + n : String(n));

function button(label, aria) {
  const b = document.createElement("button");
  b.type = "button";
  b.textContent = label;
  if (aria) b.setAttribute("aria-label", aria);
  return b;
}

export function demoWidget(root) {
  const steps = $$("[data-step]", root);
  const panes = $$("[data-pane]", root);
  if (steps.length < 2 || panes.length !== steps.length) return;

  const bar = document.createElement("div");
  bar.className = "controls demobar";
  const back = button("‹", "Back");
  const replay = button("replay");
  replay.className = "replay";
  const next = button("›", "Next");
  bar.append(back, replay, next);
  ($(".term", root) || root).append(bar);
  const count = $("[data-demo-count]", root) || document.createElement("span");
  count.setAttribute("aria-live", "polite");

  root.classList.add("stepped");
  let at = 0;
  const show = (i) => {
    at = (i + steps.length) % steps.length;
    steps.forEach((s, k) => {
      s.classList.toggle("on", k === at);
      if (k === at) s.setAttribute("aria-current", "step");
      else s.removeAttribute("aria-current");
    });
    panes.forEach((p, k) => p.classList.toggle("on", k === at));
    count.textContent = pad(at + 1) + " / " + pad(steps.length);
    const tick = $(".tick", steps[at]);
    if (tick) {
      tick.style.animation = "none";
      // Forces a reflow, so the cleared animation restarts.
      void tick.offsetWidth;
      tick.style.animation = "";
    }
    back.disabled = at === 0;
  };

  let walking = !matchMedia("(prefers-reduced-motion: reduce)").matches;
  let paused = false;
  for (const step of steps) {
    const tick = $(".tick", step);
    if (!tick) continue;
    tick.addEventListener("animationend", () => {
      if (walking && !paused && step.classList.contains("on")) show(at + 1);
    });
  }
  root.addEventListener("pointerenter", () => {
    paused = true;
    root.classList.add("paused");
  });
  root.addEventListener("pointerleave", () => {
    paused = false;
    root.classList.remove("paused");
    const tick = $(".tick", steps[at]);
    if (walking && tick && tick.getAnimations && tick.getAnimations().length === 0) show(at + 1);
  });
  const goTo = (i) => {
    walking = false;
    root.classList.add("held");
    show(i);
  };
  steps.forEach((s, k) => s.addEventListener("click", () => goTo(k)));
  back.addEventListener("click", () => goTo(at - 1));
  next.addEventListener("click", () => goTo(at + 1));
  replay.addEventListener("click", () => {
    walking = true;
    root.classList.remove("held");
    show(0);
  });
  root.addEventListener("keydown", (e) => {
    if (e.key === "ArrowRight") return e.preventDefault(), goTo(at + 1);
    if (e.key === "ArrowLeft") return e.preventDefault(), goTo(at - 1);
  });
  show(0);
}
