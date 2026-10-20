import { test } from "node:test";
import assert from "node:assert/strict";
import { gzipSync } from "node:zlib";
import { sheet as css } from "./sheet.mjs";

const bare = css.replace(/\/\*[\s\S]*?\*\//g, "");

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
  "--t-code-s": "13px",
  "--t-mono": "13px",
  "--t-eyebrow": "12px",
  "--t-key": "11px",
  "--t-tick": "10px",
  "--t-minimap": "2px",
  "--t-svg": "18px",
  "--t-svg-s": "17px",
  "--t-hero": "clamp(84px, 10vw, 168px)",
  "--t-hero-lede": "clamp(23px, 2.3vw, 32px)",
  "--t-scene": "clamp(36px, 3.8vw, 58px)",
  "--t-scene-lede": "clamp(19px, 1.7vw, 23px)",
};

const ONE_OFFS = new Set(["1.6rem", "1.25rem", "0.92em", "0.9em", "inherit"]);

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
  assert.deepEqual([...seen.keys()].sort(), Object.keys(SCALE).sort());
});

test("no font-size and no font shorthand in the sheet is an unnamed literal", () => {
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
  const seen = [...bare.matchAll(/font-size:|[\s{;]font:/g)].length;
  assert.ok(seen > 70, `only ${seen} type declarations found — the scan missed the file`);
});

test("only one selector raises the display step, and a leaf page cannot match it", () => {
  const overrides = tokenDecls(bare).filter((d) => d.selector !== ":root");
  assert.deepEqual(
    overrides.map((d) => `${d.selector} { ${d.token}: ${d.value} }`),
    ['[data-landing] { --t-display: clamp(2.6rem, 0.9rem + 4.6vw, 5.1rem) }'.replace("[data-landing]", ":root[data-landing]")],
  );
  const base = /clamp\(([^,]+),([^,]+),([^)]+)\)/.exec(SCALE["--t-display"]);
  const raised = /clamp\(([^,]+),([^,]+),([^)]+)\)/.exec(overrides[0].value);
  assert.equal(raised[2].trim(), base[2].trim(), "the vw coefficient moved");
  assert.equal(raised[3].trim(), base[3].trim(), "the cap moved");
  assert.notEqual(raised[1].trim(), base[1].trim(), "the floor did not move, so nothing was fixed");
  assert.equal(raised[1].trim(), "2.6rem");
});

test("the sheet stays inside its byte ceiling, both halves", () => {
  const shipped = bare.replace(/\n[ \t]*(?=\n)/g, "").replace(/\n{2,}/g, "\n");
  assert.ok(shipped.length <= 100000, `the published style.css is ${shipped.length} bytes, ceiling 100,000`);
  const gz = gzipSync(Buffer.from(shipped, "utf8"), { level: 9 }).length;
  assert.ok(gz <= 27000, `the published style.css is ${gz} bytes gzipped (${shipped.length} raw), ceiling 27,000`);
});
