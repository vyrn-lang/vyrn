// Regression test for the SVG namespace (design brief G2). `renderTree` built
// every element with `document.createElement`, which answers `<svg>` and
// `<circle>` with an inert `HTMLUnknownElement`: the chart parses, mounts, lays
// out at 0x0 and paints nothing. Six of the seven v1 site widgets are SVG.
//
// Run: node --test web/test/
//
// The stub is the smallest document `renderTree` touches. It is not a DOM: it
// records which factory made each node, which is exactly the bug.
import { test } from "node:test";
import assert from "node:assert/strict";

const made = (ns, tag) => ({
  ns, tag, attrs: {}, kids: [], style: {},
  setAttribute(n, v) { this.attrs[n] = v; },
  removeAttribute(n) { delete this.attrs[n]; },
  appendChild(k) { this.kids.push(k); return k; },
});

globalThis.document = {
  createElement: (tag) => made(null, tag),
  createElementNS: (ns, tag) => made(ns, tag),
  createTextNode: (text) => ({ ns: null, tag: "#text", text, kids: [] }),
  createComment: () => ({ ns: null, tag: "#comment", kids: [] }),
};

const { renderTree } = await import("../vyrn-dom.js");

const SVG = "http://www.w3.org/2000/svg";

test("an svg subtree is built in the SVG namespace", () => {
  const svg = renderTree({ El: ["svg", [{ A: ["viewBox", "0 0 10 10"] }], [
    { El: ["g", [], [{ El: ["circle", [{ A: ["r", "4"] }], []] }]] },
  ]] });
  assert.equal(svg.ns, SVG, "the <svg> root");
  assert.equal(svg.kids[0].ns, SVG, "a nested <g>");
  assert.equal(svg.kids[0].kids[0].ns, SVG, "a <circle> two levels down");
  assert.equal(svg.attrs.viewBox, "0 0 10 10");
});

test("html outside the svg keeps the document namespace", () => {
  const div = renderTree({ El: ["div", [], [
    { El: ["svg", [], []] },
    { El: ["p", [], [{ Text: "after" }]] },
  ]] });
  assert.equal(div.ns, null, "the <div> wrapper");
  assert.equal(div.kids[0].ns, SVG, "the <svg> sibling");
  assert.equal(div.kids[1].ns, null, "the <p> that follows the svg");
});
