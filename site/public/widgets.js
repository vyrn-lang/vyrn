import { $$ } from "./dom.js";
import { announce, bindInlineCopy, copyButtons, copyPageButtons, markCopyable } from "./copy.js";
import { tabsWidget } from "./tabs.js";
import { closeLanes, lanesWidget } from "./lanes.js";
import { moduleSearch } from "./search.js";
import { bindFind } from "./find.js";
import { graphWidget } from "./graph.js";
import { demoWidget } from "./demo.js";
import { armHeroEditor } from "./hero.js";
import { railSpy } from "./rail.js";
import { anchorShell, markEditorChrome, markNav } from "./masthead.js";
import { vortexWidget } from "./vortex.js";

window.__vyrnNavConfig = { staticData: true };

function onView(root, cb) {
  if (matchMedia("(prefers-reduced-motion: reduce)").matches) {
    cb(false);
    return;
  }
  const io = new IntersectionObserver(
    (entries) => {
      for (const e of entries) {
        if (e.intersectionRatio >= 0.5 || e.intersectionRect.height >= innerHeight / 2) {
          io.disconnect();
          cb(true);
          return;
        }
      }
    },
    { threshold: [0.1, 0.3, 0.5, 0.75] }
  );
  io.observe(root);
}

function entrance() {
  for (const group of $$(".specs")) {
    onView(group, (animate) => {
      if (!animate) return;
      group.classList.add("staged");
      requestAnimationFrame(() => {
        group.classList.remove("staged");
        group.classList.add("entered");
      });
    });
  }
}

function guessOs(root) {
  const ua = navigator.userAgentData?.platform || navigator.platform || navigator.userAgent || "";
  const radio = root.querySelector(`input[data-os="${/win/i.test(ua) ? "windows" : "unix"}"]`);
  if (radio) radio.checked = true;
}

function boot() {
  markEditorChrome();
  copyButtons();
  copyPageButtons();
  markCopyable();
  entrance();
  anchorShell();
  markNav();
  railSpy();
  for (const el of $$('[data-widget="demo"]')) demoWidget(el);
  for (const el of $$("[data-tabs]")) tabsWidget(el);
  for (const el of $$(".lanebox")) lanesWidget(el);
  for (const el of $$(".picker")) guessOs(el);
  moduleSearch();
  const playgrounds = $$('[data-widget="play"]');
  if (playgrounds.length) {
    import("./play.js").then((m) => playgrounds.forEach((el) => m.mountPlay(el)), (err) => console.error("the playground did not load", err));
  }
  for (const el of $$('[data-widget="playhero"]')) armHeroEditor(el);
  for (const el of $$("svg.graph")) graphWidget(el);
  for (const el of $$('[data-widget="vortex"]')) vortexWidget(el);
}

bindInlineCopy();
addEventListener("click", (e) => {
  for (const m of $$(".pagemenu[open]")) {
    if (!m.contains(e.target)) m.open = false;
  }
});
addEventListener("keydown", (e) => {
  if (e.key !== "Escape") return;
  closeLanes();
  for (const m of $$(".pagemenu[open]")) m.open = false;
});

boot();
bindFind();

document.addEventListener("vyrn:nav-end", () => {
  boot();
  const main = document.getElementById("main");
  if (main) main.focus({ preventScroll: true });
  announce(document.title);
});

let navigatorAsked = false;
for (const kind of ["pointerover", "touchstart", "focusin"]) {
  document.addEventListener(kind, (e) => {
    if (navigatorAsked || !(e.target.closest && e.target.closest("a[href]"))) return;
    navigatorAsked = true;
    // A failed load leaves hard navigation, which works.
    import("./vyrn-nav.js").catch(() => {});
  }, { passive: true });
}
