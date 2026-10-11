import { $ } from "./dom.js";

export function armHeroEditor(root) {
  const runBtn = $("[data-play-run]", root);
  const src = $("[data-play-src]", root);
  const status = $("[data-play-status]", root);
  const resetBtn = $("[data-play-reset]", root);
  const outPane = $("[data-play-out]", root);
  if (!runBtn || !src) return;
  let armed = false;
  let runOnReady = false;

  const arm = (thenRun) => {
    runOnReady = runOnReady || thenRun;
    if (armed) return;
    armed = true;
    src.removeAttribute("readonly");
    if (status) status.textContent = "Loading the compiler…";
    import("./play.js").then(
      ({ mountPlay }) => {
        if (!root.isConnected) return;
        mountPlay(root, { onReady: () => runOnReady && runBtn.click() });
      },
      (err) => {
        if (status) status.textContent = "The compiler did not load";
        if (outPane) {
          outPane.textContent = "";
          const el = document.createElement("span");
          el.className = "stderr";
          el.textContent = "The compiler did not load, so nothing here can run. Every program on this page is on /play too.";
          outPane.append(el);
        }
        src.setAttribute("readonly", "readonly");
        runBtn.disabled = true;
        if (resetBtn) resetBtn.disabled = true;
        console.error("the hero editor did not load", err);
      }
    );
  };

  root.addEventListener("pointerenter", () => arm(false), { once: true });
  root.addEventListener("focusin", () => arm(false), { once: true });
  if (resetBtn) resetBtn.addEventListener("click", () => arm(false));
  // Bound before mountPlay binds its own: the first press arms, and onReady replays it.
  runBtn.addEventListener("click", () => arm(true));
}
