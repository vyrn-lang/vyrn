// How much room a code block takes, read off the sheet rather than off a
// screenshot.
//
// The owner reported the blocks as too big and too clumsy. They were: a 1.7 line
// on a 13.5px face inside 16px of padding, in a box that carried a border AND a
// wash, so a six-line example stood 172px tall where six lines of body text
// stand 163px. The numbers below are the ones that fix followed, and this file
// is what stops them drifting back.
//
// It reads the SOURCE sheet, for the reason `contrast.test.mjs` gives: the
// export is this file with its comments removed, so every declaration here is in
// the published sheet, and a checker that needs an export cannot run while the
// sheet is being edited.
//
// Run: node --test site/test/codeblocks.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const CSS = readFileSync(fileURLToPath(new URL("../public/style.css", import.meta.url)), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");

/// The declarations of the first rule whose selector is exactly `selector`.
function rule(selector) {
  const m = new RegExp(`(^|})\\s*${selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\s*\\{([^}]*)\\}`, "m").exec(CSS);
  assert.ok(m, `the sheet has no rule \`${selector}\``);
  return m[2];
}

// The four block types a reader meets: a fenced program, a fenced block inside
// doc prose, the highlight layer under the editor, and a command.
const BLOCKS = ["pre.code", "pre.doccode", ".cmd"];

test("no code block sets a line taller than 1.5", () => {
  // Every line-height in the sheet that sits in a rule naming a code block.
  const tall = [];
  for (const m of CSS.matchAll(/(^|})\s*([^{}]*(?:pre\.code|pre\.doccode|pre\.hl|\.cmd code|\.out)[^{}]*)\{([^}]*)\}/g)) {
    const [, , selector, body] = m;
    const lh = /(?:^|;|\s)line-height:\s*([0-9.]+)\s*;/.exec(body) || /font:\s*[^;]*?\/([0-9.]+)\s/.exec(body);
    if (lh && Number(lh[1]) > 1.5) tall.push(`${selector.trim()} sets ${lh[1]}`);
  }
  assert.deepEqual(tall, [], `a code block reads at more than a 1.5 line:\n  ${tall.join("\n  ")}`);
});

test("a code block carries one edge, not three", () => {
  // A border, a background fill and a shadow are three ways to say where a box
  // ends. One is an edge; two is a frame around a frame, which is what made the
  // blocks read as heavy.
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
  // The padding was 16px on a 22.95px line, which is close enough to a line to
  // read as a blank row at the top and another at the bottom of every block.
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
  // The floor is a reading floor, not a taste one. `--t-code-s` sizes the
  // guide's plate, the specimen plate and the editor, and it was 12.5px.
  const px = (token) => Number(new RegExp(`${token}:\\s*([0-9.]+)px`).exec(CSS)[1]);
  for (const token of ["--t-code", "--t-code-s", "--t-mono", "--t-cmd"]) {
    assert.ok(px(token) >= 13, `${token} is ${px(token)}px, below the 13px floor`);
    assert.ok(px(token) <= px("--t-body"), `${token} is ${px(token)}px, above the ${px("--t-body")}px body size`);
  }
});

test("the highlight layer and the textarea over it are declared together", () => {
  // The playground draws the text once, in a `pre` under a transparent
  // `textarea`. The two boxes must agree on the face, the line and the padding
  // to the pixel, or the caret walks away from the glyph it is under. Every
  // rule that sizes one of them sizes the other in the same declaration.
  const lonely = [];
  for (const m of CSS.matchAll(/(^|})\s*([^{}]*(?:\.editor pre\.hl|\.editor textarea)[^{}]*)\{([^}]*)\}/g)) {
    const [, , selector, body] = m;
    if (!/font-size|line-height|(?:^|;|\s)padding:|(?:^|;|\s)font:/.test(body)) continue;
    const both = /pre\.hl/.test(selector) && /textarea/.test(selector);
    const one = selector.split(",").length === 1;
    // A rule that names only one of the pair may still be right — it sets a
    // value the other already has from a shared rule — so the assertion is on
    // the pair, checked in the browser and recorded in the report. What fails
    // here is a rule that names one of them and no shared rule beside it.
    if (!both && one) lonely.push(selector.trim() + " -> " + body.trim().replace(/\s+/g, " ").slice(0, 90));
  }
  // The four known singles, each one paired by a rule beside it in the sheet.
  const paired = [
    ".plate.block.play .editor pre.hl",
    ".play.plate.block .editor textarea",
    ".play .editor pre.hl",
    ".play .editor textarea",
  ];
  const strays = lonely.filter((l) => !paired.some((p) => l.startsWith(p + " ")));
  assert.deepEqual(strays, [], `a rule sizes one half of the editor overlay and nothing sizes the other:\n  ${strays.join("\n  ")}`);
});
