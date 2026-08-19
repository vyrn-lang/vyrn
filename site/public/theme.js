// The theme control (RFC-0105 M4). Three states: system, light, dark.
//
// WHY THIS IS A CLASSIC SCRIPT IN THE HEAD, and not part of `widgets.js`. A
// module is deferred, so it runs after the document is parsed and after the
// first paint — which is exactly the flash this file exists to prevent: a reader
// who chose dark would see a white page and then a dark one, on every navigation.
// A classic `<script src>` in `<head>` blocks rendering, so the root attribute is
// on the element before anything is painted. It is four hundred bytes and every
// page loads it, which is the price of not flashing.
//
// WHAT IT WRITES. One attribute on `<html>`:
//
//   data-theme="light" | "dark"   the reader's explicit choice
//   (absent)                      system — the default, and the only state a
//                                 browser with no script can be in
//
// `style.css` reads it: the dark palette applies under `[data-theme="dark"]` and
// under `prefers-color-scheme: dark` guarded by `:not([data-theme="light"])`, so
// the explicit choice beats the system in both directions and the absence of a
// choice follows the system.
//
// AND THE NO-SCRIPT RULE. The control is hidden until this file marks the
// document with `data-js`, because a control that cannot do anything is worse
// than no control: it says the reader may choose, and then does not let them.
// With no script the site is exactly what it was before this milestone —
// `prefers-color-scheme` decides, and nothing on the page claims otherwise.
(function () {
  var KEY = "vyrn.theme";
  var root = document.documentElement;

  /// The stored choice, or `""` for system. A blocked or full `localStorage`
  /// throws on read; it is not a reason to fail to render a page.
  function stored() {
    try {
      var v = localStorage.getItem(KEY);
      return v === "light" || v === "dark" ? v : "";
    } catch (err) {
      return "";
    }
  }

  /// Apply `choice` — before first paint on load, and on every click after.
  function apply(choice) {
    if (choice) root.setAttribute("data-theme", choice);
    else root.removeAttribute("data-theme");
  }

  /// Mark the pressed button in every theme control on the page. The consumer
  /// masthead and the backstage masthead each carry one, and only one of the two
  /// is ever in a document.
  function mark(choice) {
    var buttons = document.querySelectorAll("[data-theme-set]");
    for (var i = 0; i < buttons.length; i += 1) {
      var b = buttons[i];
      b.setAttribute("aria-pressed", String(b.getAttribute("data-theme-set") === (choice || "system")));
    }
  }

  apply(stored());
  // The control is inert markup until this line, and styled `display: none`
  // until it. Both halves of the progressive rule are here, so neither can be
  // shipped without the other.
  root.setAttribute("data-js", "on");

  // One delegated listener, registered on `document` — which exists while the
  // head is being parsed, unlike anything in the body. It therefore survives a
  // soft navigation replacing `<main>` for free, and needs no re-registering.
  document.addEventListener("click", function (e) {
    var btn = e.target.closest ? e.target.closest("[data-theme-set]") : null;
    if (!btn) return;
    var choice = btn.getAttribute("data-theme-set");
    if (choice === "system") choice = "";
    try {
      if (choice) localStorage.setItem(KEY, choice);
      else localStorage.removeItem(KEY);
    } catch (err) {
      // A theme that lasts the session is better than one that throws.
    }
    apply(choice);
    mark(choice);
    // The hero canvas paints from `--field`, which just changed. It listens for
    // a resize and for `prefers-color-scheme`; neither fires on a click, so this
    // is what tells it. A page without a hero has nothing listening.
    dispatchEvent(new Event("resize"));
  });

  // The buttons are further down the document than this script, so the first
  // marking waits for them.
  document.addEventListener("DOMContentLoaded", function () {
    mark(stored());
  });
})();
