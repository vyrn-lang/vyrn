// The contrast row of RFC-0105 M4's checklist, computed rather than eyeballed.
//
// The site has two palettes and one stylesheet. Both palettes are token blocks:
// `:root` carries the light one, and the dark one is written once and applied by
// two selectors (the explicit `[data-theme="dark"]` and the system default). So
// the ratios can be read off the source instead of a screenshot — this file
// parses the token blocks, resolves `var()`, `oklch()` and `color-mix()` the way
// a browser does, and measures every pair a reader actually looks at.
//
// WHY THE SOURCE AND NOT THE EXPORT. `site/public/style.css` is copied to
// `out/style.css` byte for byte, and `export.vyrn` already asserts that copy. A
// checker that needs an export cannot run while the palette is being edited,
// which is the moment it is worth having.
//
// WHAT AA ASKS FOR. 4.5:1 for body text, 3:1 for large text and for the
// non-text parts of a control or a graphic that carries meaning. Each pair below
// names which of the two it is and why.
//
// Run: node --test site/test/contrast.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const CSS = readFileSync(fileURLToPath(new URL("../public/style.css", import.meta.url)), "utf8");

// ---------------------------------------------------------------------------
// Reading the two token blocks out of the sheet
// ---------------------------------------------------------------------------

/// The text of the brace-balanced block that starts at or after `from`.
function block(from) {
  const open = CSS.indexOf("{", from);
  let depth = 0;
  for (let i = open; i < CSS.length; i += 1) {
    if (CSS[i] === "{") depth += 1;
    else if (CSS[i] === "}" && (depth -= 1) === 0) return CSS.slice(open + 1, i);
  }
  throw new Error(`the block at ${from} never closes`);
}

/// The declarations of the first rule whose selector text contains `needle`,
/// as a `name -> value` map. Only custom properties are kept: a palette is
/// tokens, and a rule that sets a property directly is not part of one.
function tokensOf(needle) {
  const at = CSS.indexOf(needle);
  assert.ok(at >= 0, `the stylesheet has no rule containing \`${needle}\``);
  const open = CSS.indexOf("{", at);
  const close = CSS.indexOf("}", open);
  assert.ok(open > 0 && close > open, `\`${needle}\` opens a block that never closes`);
  const out = new Map();
  for (const line of CSS.slice(open + 1, close).split("\n")) {
    const m = /^\s*(--[a-z0-9-]+)\s*:\s*([^;]+);/i.exec(line);
    if (m) out.set(m[1], m[2].trim());
  }
  return out;
}

const LIGHT = tokensOf(":root {");
// The dark block is the one the explicit choice selects. The system-default copy
// below it holds the same declarations, and the last test in this file is what
// says so — so measuring one measures both.
const DARK_ONLY = tokensOf(':root[data-theme="dark"]');
const DARK = new Map([...LIGHT, ...DARK_ONLY]);

// ---------------------------------------------------------------------------
// Colour, far enough to answer the question
// ---------------------------------------------------------------------------
//
// Everything in this sheet is `oklch()`, `var()`, `color-mix(in oklab, …)` or
// `transparent`, so those four are what this understands. Anything else throws
// rather than guessing — a palette that grows an `rgb()` should fail here and be
// taught, not be measured wrong.

/// Split `s` on top-level commas (a `color-mix` argument list, where an argument
/// can itself be a function call).
function args(s) {
  const out = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < s.length; i += 1) {
    if (s[i] === "(") depth += 1;
    else if (s[i] === ")") depth -= 1;
    else if (s[i] === "," && depth === 0) {
      out.push(s.slice(start, i).trim());
      start = i + 1;
    }
  }
  out.push(s.slice(start).trim());
  return out;
}

/// `{ L, a, b, alpha }` in OKLab, for a value written in `theme`.
function color(value, theme) {
  const v = value.trim();
  if (v === "transparent") return { L: 0, a: 0, b: 0, alpha: 0 };

  const varm = /^var\((--[a-z0-9-]+)\)$/i.exec(v);
  if (varm) {
    const next = theme.get(varm[1]);
    assert.ok(next !== undefined, `\`${varm[1]}\` is used and never defined`);
    return color(next, theme);
  }

  const okm = /^oklch\(([^)]*)\)$/i.exec(v);
  if (okm) {
    const [l, c, h] = okm[1].trim().split(/\s+/).map(Number);
    const rad = (h * Math.PI) / 180;
    return { L: l, a: c * Math.cos(rad), b: c * Math.sin(rad), alpha: 1 };
  }

  const mixm = /^color-mix\((.*)\)$/is.exec(v);
  if (mixm) {
    const [space, first, second] = args(mixm[1]);
    assert.equal(space.trim(), "in oklab", `only \`in oklab\` mixes are measured here: \`${v}\``);
    // A side's share is written after the colour: a literal `22%`, the wash
    // token, or the wash token doubled.
    const pct = (side) => {
      const m = /\s([0-9.]+%|calc\([^)]*\)|var\(--[a-z0-9-]+\))$/i.exec(side);
      if (!m) return null;
      return { rest: side.slice(0, side.length - m[1].length).trim(), p: percent(m[1], theme) };
    };
    const a = pct(first);
    const b = pct(second);
    // One side may leave its share implicit; it takes what the other did not.
    const pa = a ? a.p : b ? 100 - b.p : 50;
    const ca = color(a ? a.rest : first, theme);
    const cb = color(b ? b.rest : second, theme);
    return mix(ca, cb, pa / 100);
  }

  throw new Error(`the contrast checker cannot read \`${v}\``);
}

/// A percentage written literally, or as the `calc()` over a token this sheet
/// uses for the code chip's wash.
function percent(text, theme) {
  const t = text.trim();
  if (t.endsWith("%") && !t.startsWith("calc")) return Number(t.slice(0, -1));
  const varm = /^var\((--[a-z0-9-]+)\)$/i.exec(t);
  if (varm) return percent(theme.get(varm[1]), theme);
  const calcm = /^calc\(\s*(.+?)\s*\*\s*([0-9.]+)\s*\)$/i.exec(t);
  assert.ok(calcm, `the contrast checker cannot read the percentage \`${text}\``);
  return percent(calcm[1], theme) * Number(calcm[2]);
}

/// CSS `color-mix` in OKLab: premultiplied by alpha, which is what makes a mix
/// with `transparent` keep the other side's hue and merely thin it.
function mix(x, y, share) {
  const alpha = x.alpha * share + y.alpha * (1 - share);
  if (alpha === 0) return { L: 0, a: 0, b: 0, alpha: 0 };
  const chan = (k) => (x[k] * x.alpha * share + y[k] * y.alpha * (1 - share)) / alpha;
  return { L: chan("L"), a: chan("a"), b: chan("b"), alpha };
}

/// OKLab to linear sRGB (Björn Ottosson's matrices).
function linear(c) {
  const l = (c.L + 0.3963377774 * c.a + 0.2158037573 * c.b) ** 3;
  const m = (c.L - 0.1055613458 * c.a - 0.0638541728 * c.b) ** 3;
  const s = (c.L - 0.0894841775 * c.a - 1.291485548 * c.b) ** 3;
  return [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ];
}

/// `over` composited under `c`, both as linear sRGB. A token that is a wash
/// (`color-mix(… , transparent)`) is only a colour once you say what is behind
/// it, which is why every pair below names its backdrop.
function flatten(c, backdrop) {
  const fg = linear(c);
  if (c.alpha >= 1) return fg;
  const bg = linear(backdrop);
  return fg.map((v, i) => v * c.alpha + bg[i] * (1 - c.alpha));
}

/// WCAG relative luminance, from linear sRGB clamped to the gamut. Clamping is
/// what a display does with an out-of-gamut oklch, so it is what a reader sees.
function luminance(rgb) {
  const [r, g, b] = rgb.map((v) => Math.min(1, Math.max(0, v)));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/// The WCAG 2.1 contrast ratio of two flattened colours.
function ratio(fg, bg) {
  const a = luminance(fg);
  const b = luminance(bg);
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

/// The ratio of `fg` against `bg`, in `theme`, with both resolved and any
/// transparency in `fg` composited onto `bg`.
function contrast(fg, bg, theme) {
  const back = color(bg, theme);
  assert.equal(back.alpha, 1, `a backdrop must be opaque: \`${bg}\``);
  return ratio(flatten(color(fg, theme), back), linear(back));
}

// ---------------------------------------------------------------------------
// The pairs. Every one names the rule in the sheet it stands for.
// ---------------------------------------------------------------------------

// The two surfaces a reader looks at text on. `--plate` is never used at full
// strength: a plate is `color-mix(in oklab, var(--plate) 45%, transparent)` over
// the paper, so that is what is written here.
const PAPER = "var(--paper)";
const PLATE = "color-mix(in oklab, var(--plate) 45%, var(--paper))";

const PAIRS = [
  // text — 4.5:1
  ["body text", "var(--ink)", PAPER, 4.5],
  ["body text on a plate", "var(--ink)", PLATE, 4.5],
  ["secondary prose (.lede, .note, .notice)", "var(--muted)", PAPER, 4.5],
  ["secondary prose on a plate", "var(--muted)", PLATE, 4.5],
  ["meta text (.modlist .count, .rail .n, chart axis)", "var(--n2)", PAPER, 4.5],
  ["meta text on a plate (line numbers, .lines .cl.head)", "var(--n2)", PLATE, 4.5],
  ["a link, and every accented heading", "var(--accent)", PAPER, 4.5],
  ["a link on a plate", "var(--accent)", PLATE, 4.5],
  ["failure (.diag.error, .pill, a trap)", "var(--danger)", PAPER, 4.5],
  ["a pre-release (.pill.warn, .diag.warning)", "var(--amber)", PAPER, 4.5],
  ["inline code, in its own wash", "color-mix(in oklab, var(--ink) 75%, var(--accent))", "color-mix(in oklab, var(--accent) var(--code-wash), var(--paper))", 4.5],
  // syntax — 4.5:1, on the plate every code block sits on
  ["a keyword", "var(--syn-kw)", PLATE, 4.5],
  ["a string", "var(--syn-str)", PLATE, 4.5],
  ["a comment", "var(--syn-com)", PLATE, 4.5],
  ["a type", "var(--syn-type)", PLATE, 4.5],
  ["a number", "var(--syn-num)", PLATE, 4.5],
  // non-text — 3:1
  ["the focus ring", "var(--accent)", PAPER, 3],
  ["the focus ring on a plate", "var(--accent)", PLATE, 3],
  ["an ownership lane (a graphic that carries the argument)", "var(--lane-a)", PAPER, 3],
  ["the other ownership lane", "var(--lane-b)", PAPER, 3],
];

for (const [theme, tokens] of [["light", LIGHT], ["dark", DARK]]) {
  test(`the ${theme} palette meets WCAG AA`, () => {
    const bad = [];
    for (const [what, fg, bg, need] of PAIRS) {
      const got = contrast(fg, bg, tokens);
      // The measured table, for the record in RFC-0105 M4:
      //   VYRN_CONTRAST_TABLE=1 node --test site/test/contrast.test.mjs
      if (process.env.VYRN_CONTRAST_TABLE) console.log(`| ${theme} | ${what} | ${got.toFixed(2)}:1 | ${need}:1 |`);
      if (got + 0.005 < need) bad.push(`${what}: ${got.toFixed(2)}:1, needs ${need}:1 (${fg} on ${bg})`);
    }
    assert.deepEqual(bad, [], `${theme}: ${bad.length} pair(s) below AA\n  ${bad.join("\n  ")}`);
  });
}

test("the explicit dark choice and the system dark default carry the same tokens", () => {
  // The dark palette is written twice, because a selector list cannot hold a
  // media query. This is the test that keeps the second copy honest — without
  // it, `[data-theme="dark"]` and a system-dark reader could drift apart and
  // only one of them would be measured above.
  const at = CSS.indexOf("@media (prefers-color-scheme: dark)");
  assert.ok(at >= 0, "the sheet has no system-dark block");
  const guarded = block(at);
  assert.ok(
    guarded.includes(':root:not([data-theme="light"])'),
    "the system-dark block must be guarded, so an explicit light choice beats it",
  );
  for (const [name, value] of DARK_ONLY) {
    assert.ok(
      new RegExp(`^\\s*${name}\\s*:\\s*${value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")};`, "m").test(guarded),
      `\`${name}: ${value}\` is in the explicit dark block and not in the system-dark one`,
    );
  }
});
