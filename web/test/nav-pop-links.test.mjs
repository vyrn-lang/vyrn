// Back and Forward change the URL before the next page is fetched, so the page
// still on screen must keep the links it was rendered with: a root page's
// `docs/std/strings.html`, shown under `/tooling/editors.html`, resolved to
// `/tooling/docs/std/strings.html`.
//
// Run: node --test web/test/
import { test } from "node:test";
import assert from "node:assert/strict";

globalThis.location = {
  href: "https://vyrn-lang.github.io/tooling/editors.html",
  origin: "https://vyrn-lang.github.io",
  pathname: "/tooling/editors.html",
  search: "",
};

const { pinLinks } = await import("../vyrn-nav.js");

const anchor = (href) => {
  const attrs = { href };
  return { getAttribute: (n) => attrs[n], setAttribute: (n, v) => (attrs[n] = v) };
};
const root = (links) => ({ querySelectorAll: () => links });

test("a relative link keeps the page it was rendered for", () => {
  const links = [anchor("docs/std/strings.html"), anchor("../install.html"), anchor("/play.html")];
  pinLinks(root(links), "https://vyrn-lang.github.io/docs.html");
  assert.deepEqual(
    links.map((a) => a.getAttribute("href")),
    [
      "https://vyrn-lang.github.io/docs/std/strings.html",
      "https://vyrn-lang.github.io/install.html",
      "https://vyrn-lang.github.io/play.html",
    ]
  );
});

test("a fragment and an absolute link stay as written", () => {
  const links = [anchor("#main"), anchor("https://github.com/vyrn-lang/vyrn")];
  pinLinks(root(links), "https://vyrn-lang.github.io/docs.html");
  assert.deepEqual(
    links.map((a) => a.getAttribute("href")),
    ["#main", "https://github.com/vyrn-lang/vyrn"]
  );
});

test("an empty page has nothing to pin", () => {
  pinLinks(root([]), "https://vyrn-lang.github.io/docs.html");
});
