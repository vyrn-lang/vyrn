// The theme control: one button that cycles system, light and dark.
//
// The reader's choice is applied before the first paint by an inline script in
// every page's head (`withThemeBoot` in `site/export.vyrn`), because a script
// fetched for it would block on a request and still flash on a cold load. This
// module holds only the toggle. The two share `vyrn.theme` in `localStorage`
// and the `data-theme` attribute on `<html>`:
//
//   data-theme="light" | "dark"   the reader's explicit choice
//   (absent)                      system: the default, and the only state
//                                 without script
//
// `style.css` applies the dark palette under `[data-theme="dark"]` and under
// `prefers-color-scheme: dark` guarded by `:not([data-theme="light"])`, so an
// explicit choice beats the system in both directions.
//
// The inline script also sets `data-js`, which unhides the control: without
// script there is no control, and `prefers-color-scheme` decides.
(function () {
  var KEY = "vyrn.theme";
  var root = document.documentElement;

  // The masthead's height across a navigation is the inline script's job too:
  // the arriving page has to start in the state the last one ended in, which
  // is only possible before the first paint. `boot()` in `widgets.js` then sets
  // this page's state a frame later, and the difference animates.

  // The cycle, in order, and each state's word. `""` is system, first because
  // it is the default. The markup renders the system word (`themeControl()`),
  // and `mark()` corrects it before a reader can look.
  var ORDER = ["", "light", "dark"];
  var WORD = { "": "System", light: "Light", dark: "Dark" };

  /// The stored choice, or `""` for system. A blocked or full `localStorage`
  /// throws on read, which is no reason to fail a page.
  function stored() {
    try {
      var v = localStorage.getItem(KEY);
      return v === "light" || v === "dark" ? v : "";
    } catch (err) {
      return "";
    }
  }

  /// Applies `choice` to the root element.
  function apply(choice) {
    if (choice) root.setAttribute("data-theme", choice);
    else root.removeAttribute("data-theme");
  }

  /// The state after `choice`.
  function next(choice) {
    return ORDER[(ORDER.indexOf(choice) + 1) % ORDER.length];
  }

  /// Shows the state on every control: the word as its text, and as its
  /// accessible name the state and what the next press does. The name changes
  /// on the focused element, so a screen reader announces it with no live
  /// region. The visible word is inside the name, so a voice command naming
  /// the word hits the button (WCAG 2.5.3). A page with its own shell has no
  /// control, which is why this loops over all matches.
  function mark(choice) {
    var buttons = document.querySelectorAll("[data-theme-cycle]");
    for (var i = 0; i < buttons.length; i += 1) {
      buttons[i].textContent = WORD[choice];
      buttons[i].setAttribute("aria-label", "Theme: " + WORD[choice] + ". Press to use " + WORD[next(choice)] + ".");
    }
  }

  // One listener on `document`, which exists while the head is parsed and
  // survives a soft navigation replacing `<main>`.
  document.addEventListener("click", function (e) {
    var btn = e.target.closest ? e.target.closest("[data-theme-cycle]") : null;
    if (!btn) return;
    // The state lives on `<html>`, where a reload reads it too.
    var choice = next(root.getAttribute("data-theme") || "");
    try {
      if (choice) localStorage.setItem(KEY, choice);
      else localStorage.removeItem(KEY);
    } catch (err) {
      // A theme that lasts the session is better than one that throws.
    }
    apply(choice);
    mark(choice);
  });

  // The button comes after this script in the document.
  document.addEventListener("DOMContentLoaded", function () {
    mark(stored());
  });
})();
