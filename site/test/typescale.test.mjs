// THE TYPE SCALE, MEASURED (RFC-0106 M1).
//
// RFC-0106 M0 found that `site/public/style.css` had tokenized colour (25
// tokens, zero literals left on a property) and tokenized spacing (6 tokens),
// and left type as 34 distinct literal font sizes spread over 1,729 lines — a
// scale nobody could measure. M1 named every step. This file is what keeps them
// named, and what makes M0's constraint on the leaf pages checkable.
//
// WHY IT READS THE STYLESHEET AND NOT A RENDERED PAGE. The constraint M0 wrote
// is "the computed `font-size` of every element on the 160 leaf pages is
// unchanged, at 375px, 768px and 1280px". A computed value needs a layout
// engine, and this repository has no headless browser in CI and is not adding
// one for a stylesheet assertion. The constraint is therefore held in two
// halves, and between them they are exact rather than approximate:
//
//   1. HERE: every `font-size` and every size in a `font:` shorthand names a
//      token or is on the one-off list, the tokens resolve to the values the
//      census recorded, and the display step is raised on ONE selector —
//      `:root[data-landing]`. Nothing else in the file redefines a type token,
//      so no rule that a leaf page matches can compute differently.
//   2. In `site/export.vyrn`, "the display type step reaches nine pages and no
//      leaf page": `data-landing` is stamped on the nine landing documents and
//      on none of the 160 leaves, asserted per published document.
//
// A leaf page can only reach the raised step through that attribute, and it does
// not carry it. That is the whole proof, and each half fails on its own.
//
// It also runs without the export, unlike its two siblings in this directory:
// the stylesheet is a committed file, so this is the one gate here that a
// contributor can run before building anything.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { gzipSync } from "node:zlib";
import path from "node:path";

const SHEET = path.resolve(import.meta.dirname, "../public/style.css");
const css = await readFile(SHEET, "utf8");

// Comments carry example sizes and the census's own numbers, so they are removed
// before anything below counts a declaration.
const bare = css.replace(/\/\*[\s\S]*?\*\//g, "");

/// The scale, as the census recorded it: the token, and what it resolves to.
/// This table is the assertion — a token whose value moves fails here with both
/// numbers, which is what "measurable" means for a type scale.
const SCALE = {
  "--t-display": "clamp(2.1rem, 0.9rem + 4.6vw, 5.1rem)",
  "--t-h1": "clamp(1.7rem, 1.1rem + 1.8vw, 2.4rem)",
  "--t-h2": "clamp(1.6rem, 1rem + 2vw, 2.75rem)",
  "--t-h3": "1.5rem",
  "--t-h4": "1.1rem",
  "--t-h5": "1rem",
  "--t-lede": "1.2rem",
  "--t-body": "17px",
  "--t-body-s": "16px",
  "--t-note": "0.95rem",
  "--t-meta": "0.92rem",
  "--t-matrix": "0.9rem",
  "--t-cap": "0.85rem",
  "--t-mark": "15px",
  "--t-cmd": "14px",
  "--t-code": "13.5px",
  "--t-code-s": "12.5px",
  "--t-mono": "13px",
  "--t-eyebrow": "12px",
  "--t-key": "11px",
  "--t-tick": "10px",
  // The playground's minimap: the program as texture, deliberately unreadable
  // (RFC-0106 M5 round 8).
  "--t-minimap": "2px",
  // The two chart steps are USER UNITS inside a `viewBox`, not pixels on a
  // screen: what a reader gets is the number times the chart's own scale, which
  // `svg.chart`'s `max-width` and the phone scroller bound to [0.73, 1.0]
  // (RFC-0106 M3, fourth round). They replaced `--t-axis: 9px`, which painted
  // 3px text on a phone.
  "--t-svg": "18px",
  "--t-svg-s": "17px",
};

/// The sizes that are deliberately not steps on a scale. Four mono numerals each
/// sized to one widget's box, the one numeral ramp, one normalized figure in one
/// table cell, and three that are a SHARE of their context by intent — a code
/// span in prose, a code span in a heading, and the unit beside a big number.
const ONE_OFFS = new Set(["2rem", "1.6rem", "1.3rem", "1.25rem", "clamp(1.8rem, 1rem + 2.4vw, 3rem)", "0.78rem", "0.92em", "0.9em", "0.42em", "inherit"]);

/// Every `--t-*` declaration in the file, as [selector, token, value]. The
/// selector is the text between the previous `}` (or the file's start) and the
/// `{` that opens the block.
function tokenDecls(text) {
  const out = [];
  const re = /(--t-[a-z0-9-]+)\s*:\s*([^;]+);/g;
  for (const m of text.matchAll(re)) {
    const before = text.slice(0, m.index);
    const open = before.lastIndexOf("{");
    const close = before.lastIndexOf("}");
    out.push({ selector: before.slice(close + 1, open).trim().replace(/\s+/g, " "), token: m[1], value: m[2].trim() });
  }
  return out;
}

test("every step of the type scale has a name, and the name resolves to the number the census recorded", () => {
  const declared = tokenDecls(bare).filter((d) => d.selector === ":root");
  const seen = new Map(declared.map((d) => [d.token, d.value]));
  for (const [token, value] of Object.entries(SCALE)) {
    assert.equal(seen.get(token), value, `${token} on :root`);
  }
  // No extra token on `:root` — a step added without a row in the table above is
  // a step nothing measures, which is the state this file exists to end.
  assert.deepEqual([...seen.keys()].sort(), Object.keys(SCALE).sort());
});

test("no font-size and no font shorthand in the sheet is an unnamed literal", () => {
  // A declaration passes if, once every `var(--t-…)` and every listed one-off is
  // struck out of it, no length is left. That is the check for both forms: a
  // `font:` shorthand carries the size inside it rather than as its whole value,
  // and a shorthand holding `clamp(1.8rem, 1rem + 2.4vw, 3rem)` has three
  // lengths in one term. Nothing is parsed, so nothing about the shorthand's
  // grammar has to be re-implemented here.
  const unnamed = [];
  const struck = (value) => {
    let v = value.replace(/var\(--t-[a-z0-9-]+\)/g, "");
    for (const one of ONE_OFFS) v = v.split(one).join("");
    return v;
  };
  for (const re of [/font-size:\s*([^;]+);/g, /(?:^|[\s{;])font:\s*([^;]+);/g]) {
    for (const m of bare.matchAll(re)) {
      const v = m[1].trim();
      if (/\d*\.?\d+(px|rem|em|vw)/.test(struck(v))) unnamed.push(v);
    }
  }
  assert.deepEqual(unnamed, [], `type sizes that name no token:\n${unnamed.join("\n")}`);
  // And the scan reached the file: 80-odd declarations, not zero.
  const seen = [...bare.matchAll(/font-size:|[\s{;]font:/g)].length;
  assert.ok(seen > 70, `only ${seen} type declarations found — the scan missed the file`);
});

test("only one selector raises the display step, and a leaf page cannot match it", () => {
  // Every place in the file that redefines a type token outside `:root`. The
  // display raise is meant to be the only one, and it is meant to be reachable
  // only through an attribute the export stamps per route.
  const overrides = tokenDecls(bare).filter((d) => d.selector !== ":root");
  assert.deepEqual(
    overrides.map((d) => `${d.selector} { ${d.token}: ${d.value} }`),
    ['[data-landing] { --t-display: clamp(2.6rem, 0.9rem + 4.6vw, 5.1rem) }'.replace("[data-landing]", ":root[data-landing]")],
  );
  // The raise is the FLOOR and nothing else: the middle term and the cap are the
  // same in both curves, so every width from 641px up computes what it computed
  // before, on a landing page and on a leaf alike.
  const base = /clamp\(([^,]+),([^,]+),([^)]+)\)/.exec(SCALE["--t-display"]);
  const raised = /clamp\(([^,]+),([^,]+),([^)]+)\)/.exec(overrides[0].value);
  assert.equal(raised[2].trim(), base[2].trim(), "the vw coefficient moved");
  assert.equal(raised[3].trim(), base[3].trim(), "the cap moved");
  assert.notEqual(raised[1].trim(), base[1].trim(), "the floor did not move, so nothing was fixed");
  // 2.6rem against a 16px root is 41.6px, and 2.6x the 16px body a phone gets.
  assert.equal(raised[1].trim(), "2.6rem");
});

test("the sheet stays inside M0's byte ceiling, both halves", () => {
  // RFC-0106 M0 sets TWO numbers on this file: `<= 90,000 bytes raw, <= 27,000
  // gzipped`. Both are about what a READER DOWNLOADS, and since RFC-0106 M3 that
  // is not this file: `site/export.vyrn` strips the comments on the way to
  // `out/style.css`, because 48% of this sheet is a design record and a design
  // record is not a page's payload. The ceilings are therefore measured on the
  // shipped form — which is what `bare` already is, modulo the blank lines the
  // export also drops, so this is the conservative side of the real number.
  //
  // The source keeps no byte ceiling of its own on purpose: a comment budget is
  // a budget on explaining yourself, and the reason the numbers existed was
  // never that.
  const shipped = bare.replace(/\n[ \t]*(?=\n)/g, "").replace(/\n{2,}/g, "\n");
  assert.ok(shipped.length <= 90000, `the published style.css is ${shipped.length} bytes, ceiling 90,000`);
  const gz = gzipSync(Buffer.from(shipped, "utf8"), { level: 9 }).length;
  assert.ok(gz <= 27000, `the published style.css is ${gz} bytes gzipped (${shipped.length} raw), ceiling 27,000`);
});
