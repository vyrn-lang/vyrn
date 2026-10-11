// How much room a code block takes, read off the sheet.
//
// A code block is at most a 1.5 line, carries one edge, pads no more than one
// line, and is set at 13px or more. It reads the source sheet: the export only
// strips comments, so every declaration here ships, and the check runs while
// the sheet is edited.
//
// Run: node --test site/test/codeblocks.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { sheet } from "./sheet.mjs";

const CSS = sheet.replace(/\/\*[\s\S]*?\*\//g, "");

/// The declarations of the first rule whose selector is exactly `selector`.
function rule(selector) {
  const m = new RegExp(`(^|})\\s*${selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\s*\\{([^}]*)\\}`, "m").exec(CSS);
  assert.ok(m, `the sheet has no rule \`${selector}\``);
  return m[2];
}

// The block types a reader meets: a fenced program, a fenced block in doc
// prose, and a command.
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
  // A border, a background fill and a shadow each say where a box ends; two of
  // them make a frame around a frame.
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
  // A padding near a full line reads as a blank row above and below.
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
  // A reading floor. `--t-code-s` sizes the guide's plate and the editor.
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
      // A rule naming one half may be right when a shared rule sizes the
      // other; the four known singles below are paired that way.
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
