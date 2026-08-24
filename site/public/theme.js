// The theme control (RFC-0105 M4). Three states — system, light, dark — on ONE
// button that cycles through them.
//
// WHAT IS NOT IN THIS FILE, AND WHY. This was a classic `<script src>` in the
// head, so that it blocked rendering and the root attribute was on the element
// before the first paint. It blocked on a REQUEST to do it, and nothing is
// painted while that request is in flight — so the flash it existed to prevent
// was still there on a cold load, moved from after the paint to before it
// (B3 defect 5). The reader's choice is in `localStorage`, which needs no
// request, so the four lines that read it are written INLINE in the head of
// every page by `withThemeBoot` in `site/export.vyrn`. This file is a module
// now, deferred like every other script on the page, and what it holds is the
// toggle — which runs when a reader presses it, long after any paint.
//
// The inline piece and this file share `vyrn.theme` and the `data-theme`
// attribute and nothing else. If they disagree, the inline piece wins on load
// and this one wins on the next press, which is the right way round.
//
// WHAT THE INLINE PIECE WRITES. One attribute on `<html>`:
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

  // THE MASTHEAD'S HEIGHT CROSSES THE NAVIGATION IN THE INLINE PIECE, not here.
  // The editor wears a title-bar masthead and every other page wears the full
  // one, and the two live in different documents — so nothing can animate
  // between them unless the arriving page STARTS in the state the last one
  // ended in. That is true before the first paint and at no later moment, which
  // is why `withThemeBoot` carries it and this file does not; `boot()` in
  // `widgets.js` then sets the state this page actually wants, one frame later,
  // and the difference animates (user: it changed in one direction only).

  // The cycle, in order, and the word each state is called. `""` is system, and
  // it is first because it is the default. The words live here rather than in
  // the markup because this file is what changes them on every press; the
  // markup's one copy — the `system` state, in `themeControl()` — is corrected
  // by `mark()` before a reader can look at it.
  var ORDER = ["", "light", "dark"];
  var WORD = { "": "System", light: "Light", dark: "Dark" };

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

  /// The state after `choice`.
  function next(choice) {
    return ORDER[(ORDER.indexOf(choice) + 1) % ORDER.length];
  }

  /// Say the state on the control: the word as its text, and the sentence as its
  /// accessible name. The name states the state AND what the next press does,
  /// which is the half a cycling button otherwise hides. It changes on the
  /// element that has focus when it is pressed, so a screen reader announces the
  /// new state without a live region beside the button saying it a second time.
  ///
  /// The visible word is inside the name, so a reader who says "click System"
  /// to a voice control hits the same button they can see (WCAG 2.5.3).
  ///
  /// The consumer masthead and the backstage masthead each carry one control,
  /// and only one of the two is ever in a document.
  function mark(choice) {
    var buttons = document.querySelectorAll("[data-theme-cycle]");
    for (var i = 0; i < buttons.length; i += 1) {
      buttons[i].textContent = WORD[choice];
      buttons[i].setAttribute("aria-label", "Theme: " + WORD[choice] + ". Press to use " + WORD[next(choice)] + ".");
    }
  }

  // `apply(stored())` and `data-js` are the inline piece's, for the reason at
  // the top of this file: both decide what a reader looks at, and a decision
  // made after the paint is a change a reader watches happen.

  // One delegated listener, registered on `document` — which exists while the
  // head is being parsed, unlike anything in the body. It therefore survives a
  // soft navigation replacing `<main>` for free, and needs no re-registering.
  document.addEventListener("click", function (e) {
    var btn = e.target.closest ? e.target.closest("[data-theme-cycle]") : null;
    if (!btn) return;
    // The button carries no state of its own: the state is the attribute on
    // `<html>`, which is where a reload reads it from as well.
    var choice = next(root.getAttribute("data-theme") || "");
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

  // The button is further down the document than this script, so the first
  // marking waits for it.
  document.addEventListener("DOMContentLoaded", function () {
    mark(stored());
  });
})();
