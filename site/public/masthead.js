import { $, $$ } from "./dom.js";

export function navOwns(href, path) {
  const stem = href.replace(/\.html$/, "");
  if (path === href || path.startsWith(stem + "/")) return true;
  if (stem.endsWith("/guide")) {
    const root = stem.slice(0, -"/guide".length);
    for (const area of ["/docs", "/web", "/tooling"]) {
      if (path === root + area + ".html" || path.startsWith(root + area + "/")) return true;
    }
  }
  return false;
}

export function anchorShell() {
  for (const a of $$(".masthead a[href], .foot a[href]")) {
    if (!a.getAttribute("href").startsWith("#")) a.setAttribute("href", a.href);
  }
}

export function markNav() {
  const path = location.pathname;
  for (const link of $$(".masthead .nav a")) {
    const href = new URL(link.getAttribute("href"), location.href).pathname;
    if (navOwns(href, path)) link.setAttribute("aria-current", "page");
    else link.removeAttribute("aria-current");
  }
}

export function markEditorChrome() {
  const editor = Boolean($(".page.play.ide"));
  const root = document.documentElement;
  try {
    sessionStorage.setItem("vyrn.mast", editor ? "compact" : "full");
  } catch (err) {}
  const settle = () => root.classList.toggle("idecompact", editor);
  if (document.visibilityState === "hidden") settle();
  // Two frames: the class lands after the first paint, so the masthead animates into it.
  else requestAnimationFrame(() => requestAnimationFrame(settle));
}
