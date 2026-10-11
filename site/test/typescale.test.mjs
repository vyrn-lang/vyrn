// The type scale, measured.
//
// Every `font-size` and every size in a `font:` shorthand names a `--t-*`
// token or is a listed one-off, and the tokens resolve to the recorded values.
// One selector, `:root[data-landing]`, raises the display step, and
// `site/export.vyrn` asserts that only the landing pages carry `data-landing`.
// Between them, no leaf page's computed font size can move, and neither half
// needs a browser.
//
// The stylesheet is a committed file, so this runs before any build.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { gzipSync } from "node:zlib";
import path from "node:path";

const SHEET = path.resolve(import.meta.dirname, "../public/style.css");
const css = await readFile(SHEET, "utf8");

// Comments carry example sizes, so they are removed
// before anything below counts a declaration.
const bare = css.replace(/\/\*[\s\S]*?\*\//g, "");

/// The scale: each token and what it resolves to. A token whose value moves
/// fails here with both numbers.
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
  // 13px and not 12.5: no code block reads below 13px at the default root size,
  // and this token sizes the guide's plate, the specimen plate and the editor.
  "--t-code-s": "13px",
  "--t-mono": "13px",
  "--t-eyebrow": "12px",
  "--t-key": "11px",
  "--t-tick": "10px",
  // The playground's minimap: the program as texture, deliberately unreadable.
  "--t-minimap": "2px",
  // The chart steps are user units inside a `viewBox`; `svg.chart`'s width
  // bounds keep them at 12px or more.
  "--t-svg": "18px",
  "--t-svg-s": "17px",
};

/// The sizes that are deliberately not steps: one figure in a stat tile
/// (`.specs dd`), an entry heading, and shares of their context (a code span in
/// prose and in a heading).
const ONE_OFFS = new Set(["1.6rem", "1.25rem", "0.92em", "0.9em", "inherit"]);

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
  // No extra token on `:root`: a step without a row above is a step nothing
  // measures.
  assert.deepEqual([...seen.keys()].sort(), Object.keys(SCALE).sort());
});

test("no font-size and no font shorthand in the sheet is an unnamed literal", () => {
  // A declaration passes if, once every `var(--t-...)` and every listed one-off is
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
  // The scan reached the file: many declarations, not zero.
  const seen = [...bare.matchAll(/font-size:|[\s{;]font:/g)].length;
  assert.ok(seen > 70, `only ${seen} type declarations found — the scan missed the file`);
});

test("only one selector raises the display step, and a leaf page cannot match it", () => {
  // Every place that redefines a type token outside `:root`. The display raise
  // is the only one, reachable only through an attribute the export stamps.
  const overrides = tokenDecls(bare).filter((d) => d.selector !== ":root");
  assert.deepEqual(
    overrides.map((d) => `${d.selector} { ${d.token}: ${d.value} }`),
    ['[data-landing] { --t-display: clamp(2.6rem, 0.9rem + 4.6vw, 5.1rem) }'.replace("[data-landing]", ":root[data-landing]")],
  );
  // The raise is the floor only: the middle term and the cap match, so every
  // width from 641px up computes the same on a landing page and on a leaf.
  const base = /clamp\(([^,]+),([^,]+),([^)]+)\)/.exec(SCALE["--t-display"]);
  const raised = /clamp\(([^,]+),([^,]+),([^)]+)\)/.exec(overrides[0].value);
  assert.equal(raised[2].trim(), base[2].trim(), "the vw coefficient moved");
  assert.equal(raised[3].trim(), base[3].trim(), "the cap moved");
  assert.notEqual(raised[1].trim(), base[1].trim(), "the floor did not move, so nothing was fixed");
  // 2.6rem against a 16px root is 41.6px, and 2.6x the 16px body a phone gets.
  assert.equal(raised[1].trim(), "2.6rem");
});

test("the sheet stays inside its byte ceiling, both halves", () => {
  // Both ceilings are on what a reader downloads: `site/export.vyrn` strips
  // the comments, so they are measured on `bare` with blank lines dropped.
  // The gzipped size is the binding one.
  const shipped = bare.replace(/\n[ \t]*(?=\n)/g, "").replace(/\n{2,}/g, "\n");
  assert.ok(shipped.length <= 100000, `the published style.css is ${shipped.length} bytes, ceiling 100,000`);
  const gz = gzipSync(Buffer.from(shipped, "utf8"), { level: 9 }).length;
  assert.ok(gz <= 27000, `the published style.css is ${gz} bytes gzipped (${shipped.length} raw), ceiling 27,000`);
});
