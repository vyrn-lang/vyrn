// Regression test for the SVG `<a>` trap. `shouldIntercept` resolved the link
// with `new URL(a.href, location.href)`. On an SVG anchor — every link inside a
// chart — `.href` is an `SVGAnimatedString`, not a string, so the URL
// constructor received the text "[object SVGAnimatedString]", resolved it as a
// same-origin path, and the click navigated to a page that does not exist.
//
// Run: node --test web/test/
import { test } from "node:test";
import assert from "node:assert/strict";

globalThis.location = {
  href: "http://localhost:8813/compare.html",
  origin: "http://localhost:8813",
  pathname: "/compare.html",
  search: "",
};

const { shouldIntercept } = await import("../vyrn-nav.js");

const CLICK = { defaultPrevented: false, button: 0 };

// An anchor whose `href` PROPERTY is what each element kind really gives back:
// a resolved string for HTML, an `SVGAnimatedString` for SVG.
const anchor = (attrs, hrefProp) => ({
  href: hrefProp,
  getAttribute: (n) => (n in attrs ? attrs[n] : null),
  hasAttribute: (n) => n in attrs,
});

test("an SVG link is judged by its href attribute, not its property object", () => {
  const svgHref = { baseVal: "https://github.com/vyrn-lang/vyrn", animVal: "" };
  svgHref.toString = () => "[object SVGAnimatedString]";
  const a = anchor({ href: "https://github.com/vyrn-lang/vyrn" }, svgHref);
  // External origin: the browser handles it, and the navigator keeps its hands off.
  assert.equal(shouldIntercept(a, CLICK), false);
});

test("a same-origin SVG link resolves to the path it names", () => {
  const svgHref = { baseVal: "/docs/std/json.html" };
  svgHref.toString = () => "[object SVGAnimatedString]";
  const a = anchor({ href: "/docs/std/json.html" }, svgHref);
  const url = shouldIntercept(a, CLICK);
  assert.equal(url.pathname, "/docs/std/json.html");
});

test("an ordinary HTML link still resolves", () => {
  const a = anchor({ href: "/install.html" }, "http://localhost:8813/install.html");
  assert.equal(shouldIntercept(a, CLICK).pathname, "/install.html");
});
