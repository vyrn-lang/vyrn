import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const CSS = readFileSync(fileURLToPath(new URL("../public/style.css", import.meta.url)), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");

function rule(selector) {
  const m = new RegExp(`(^|})\\s*${selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\s*\\{([^}]*)\\}`, "m").exec(CSS);
  assert.ok(m, `the sheet has no rule \`${selector}\``);
  return m[2];
}

const BLOCKS = ["pre.code", "pre.doccode", ".cmd"];

test("no code block sets a line taller than 1.5", () => {
  const tall = [];
  for (const m of CSS.matchAll(/(^|})\s*([^{}]*(?:pre\.code|pre\.doccode|pre\.hl|\.cmd code|\.out)[^{}]*)\{([^}]*)\}/g)) {
    const [, , selector, body] = m;
    const lh = /(?:^|;|\s)line-height:\s*([0-9.]+)\s*;/.exec(body) || /font:\s*[^;]*?\/([0-9.]+)\s/.exec(body);
    if (lh && Number(lh[1]) > 1.5) tall.push(`${selector.trim()} sets ${lh[1]}`);
  }
  assert.deepEqual(tall, [], `a code block reads at more than a 1.5 line:\n  ${tall.join("\n  ")}`);
});

test("a code block carries one edge, not three", () => {
  for (const selector of BLOCKS) {
    const body = rule(selector);
    const edges = [
      /(?:^|;|\s)border(?:-(?:top|right|bottom|left))?:\s*(?!0|none)/.test(body) && "border",
      /(?:^|;|\s)background(?:-color)?:\s*(?!none|transparent)/.test(body) && "background",
      /(?:^|;|\s)box-shadow:\s*(?!none)/.test(body) && "shadow",
    ].filter(Boolean);
    assert.ok(edges.length <= 1, `${selector} carries ${edges.length} edges: ${edges.join(" + ")}`);
  }
});

test("a code block's vertical padding is no taller than one of its lines", () => {
  const size = { "pre.code": 13.5, "pre.doccode": 13, ".cmd": 14 };
  for (const selector of BLOCKS) {
    const body = selector === ".cmd" ? rule(".cmd code") : rule(selector);
    const m = /(?:^|;|\s)padding:\s*([0-9]+)px/.exec(body);
    assert.ok(m, `${selector} sets no explicit vertical padding`);
    assert.ok(
      Number(m[1]) <= size[selector] * 1.5,
      `${selector} pads ${m[1]}px against a ${(size[selector] * 1.5).toFixed(2)}px line`,
    );
  }
});

test("no code block is set below 13px at the default root size", () => {
  const px = (token) => Number(new RegExp(`${token}:\\s*([0-9.]+)px`).exec(CSS)[1]);
  for (const token of ["--t-code", "--t-code-s", "--t-mono", "--t-cmd"]) {
    assert.ok(px(token) >= 13, `${token} is ${px(token)}px, below the 13px floor`);
    assert.ok(px(token) <= px("--t-body"), `${token} is ${px(token)}px, above the ${px("--t-body")}px body size`);
  }
});

test("the highlight layer and the textarea over it are declared together", () => {
  const lonely = [];
  for (const m of CSS.matchAll(/(^|})\s*([^{}]*(?:\.editor pre\.hl|\.editor textarea)[^{}]*)\{([^}]*)\}/g)) {
    const [, , selector, body] = m;
    if (!/font-size|line-height|(?:^|;|\s)padding:|(?:^|;|\s)font:/.test(body)) continue;
    const both = /pre\.hl/.test(selector) && /textarea/.test(selector);
    const one = selector.split(",").length === 1;
    if (!both && one) lonely.push(selector.trim() + " -> " + body.trim().replace(/\s+/g, " ").slice(0, 90));
  }
  const paired = [
    ".plate.block.play .editor pre.hl",
    ".play.plate.block .editor textarea",
    ".play .editor pre.hl",
    ".play .editor textarea",
  ];
  const strays = lonely.filter((l) => !paired.some((p) => l.startsWith(p + " ")));
  assert.deepEqual(strays, [], `a rule sizes one half of the editor overlay and nothing sizes the other:\n  ${strays.join("\n  ")}`);
});
