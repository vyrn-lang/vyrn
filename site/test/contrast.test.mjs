import { test } from "node:test";
import assert from "node:assert/strict";
import { sheet as CSS } from "./sheet.mjs";

function block(from) {
  const open = CSS.indexOf("{", from);
  let depth = 0;
  for (let i = open; i < CSS.length; i += 1) {
    if (CSS[i] === "{") depth += 1;
    else if (CSS[i] === "}" && (depth -= 1) === 0) return CSS.slice(open + 1, i);
  }
  throw new Error(`the block at ${from} never closes`);
}

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
const DARK_ONLY = tokensOf(':root[data-theme="dark"]');
const DARK = new Map([...LIGHT, ...DARK_ONLY]);

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
    const pct = (side) => {
      const m = /\s([0-9.]+%|calc\([^)]*\)|var\(--[a-z0-9-]+\))$/i.exec(side);
      if (!m) return null;
      return { rest: side.slice(0, side.length - m[1].length).trim(), p: percent(m[1], theme) };
    };
    const a = pct(first);
    const b = pct(second);
    const pa = a ? a.p : b ? 100 - b.p : 50;
    const ca = color(a ? a.rest : first, theme);
    const cb = color(b ? b.rest : second, theme);
    return mix(ca, cb, pa / 100);
  }

  throw new Error(`the contrast checker cannot read \`${v}\``);
}

function percent(text, theme) {
  const t = text.trim();
  if (t.endsWith("%") && !t.startsWith("calc")) return Number(t.slice(0, -1));
  const varm = /^var\((--[a-z0-9-]+)\)$/i.exec(t);
  if (varm) return percent(theme.get(varm[1]), theme);
  const calcm = /^calc\(\s*(.+?)\s*\*\s*([0-9.]+)\s*\)$/i.exec(t);
  assert.ok(calcm, `the contrast checker cannot read the percentage \`${text}\``);
  return percent(calcm[1], theme) * Number(calcm[2]);
}

function mix(x, y, share) {
  const alpha = x.alpha * share + y.alpha * (1 - share);
  if (alpha === 0) return { L: 0, a: 0, b: 0, alpha: 0 };
  const chan = (k) => (x[k] * x.alpha * share + y[k] * y.alpha * (1 - share)) / alpha;
  return { L: chan("L"), a: chan("a"), b: chan("b"), alpha };
}

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

function flatten(c, backdrop) {
  const fg = linear(c);
  if (c.alpha >= 1) return fg;
  const bg = linear(backdrop);
  return fg.map((v, i) => v * c.alpha + bg[i] * (1 - c.alpha));
}

function luminance(rgb) {
  const [r, g, b] = rgb.map((v) => Math.min(1, Math.max(0, v)));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function ratio(fg, bg) {
  const a = luminance(fg);
  const b = luminance(bg);
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

function contrast(fg, bg, theme) {
  const back = color(bg, theme);
  assert.equal(back.alpha, 1, `a backdrop must be opaque: \`${bg}\``);
  return ratio(flatten(color(fg, theme), back), linear(back));
}

const PAPER = "var(--paper)";
const PLATE = "color-mix(in oklab, var(--plate) 45%, var(--paper))";
const CMD = "color-mix(in oklab, var(--plate) 55%, var(--paper))";
const CONTROL = "color-mix(in oklab, var(--ink) 4%, var(--paper))";

const PAIRS = [
  ["body text", "var(--ink)", PAPER, 7],
  ["body text on a plate", "var(--ink)", PLATE, 7],
  ["secondary prose (.lede, .note, .notice)", "var(--muted)", PAPER, 4.5],
  ["secondary prose on a plate", "var(--muted)", PLATE, 4.5],
  ["meta text (.modgrid .count, .rail .n, chart axis)", "var(--n2)", PAPER, 4.5],
  ["meta text on a plate (line numbers, .lines .cl.head)", "var(--n2)", PLATE, 4.5],
  ["the copy button's word, over the command it copies", "var(--n2)", CMD, 4.5],
  ["a link, and every accented heading", "var(--accent)", PAPER, 4.5],
  ["a link on a plate", "var(--accent)", PLATE, 4.5],
  ["failure (.diag.error, .pill, a trap)", "var(--danger)", PAPER, 4.5],
  ["a pre-release (.pill.warn, .diag.warning)", "var(--amber)", PAPER, 4.5],
  ["inline code, in its own wash", "color-mix(in oklab, var(--ink) 75%, var(--accent))", "color-mix(in oklab, var(--accent) var(--code-wash), var(--paper))", 4.5],
  ["a keyword", "var(--syn-kw)", PLATE, 4.5],
  ["a string", "var(--syn-str)", PLATE, 4.5],
  ["a comment", "var(--syn-com)", PLATE, 4.5],
  ["a type", "var(--syn-type)", PLATE, 4.5],
  ["a number", "var(--syn-num)", PLATE, 4.5],
  ["the focus ring", "var(--accent)", PAPER, 3],
  ["the focus ring on a plate", "var(--accent)", PLATE, 3],
  ["the focus ring against the control it rings", "var(--accent)", CONTROL, 3],
  ["the filled call to action's own label", PAPER, "var(--accent)", 4.5],
  ["the C line on the radar", "var(--sr-c)", PLATE, 3],
  ["the Rust line", "var(--sr-rust)", PLATE, 3],
  ["the node line", "var(--sr-node)", PLATE, 3],
  ["the two Vyrn lines", "var(--sr-vyrn)", PLATE, 3],
  ["a legend label", "var(--sr-c)", PLATE, 4.5],
  ["a legend label (Rust)", "var(--sr-rust)", PLATE, 4.5],
  ["a legend label (node)", "var(--sr-node)", PLATE, 4.5],
  ["a legend label (Vyrn)", "var(--sr-vyrn)", PLATE, 4.5],
];

for (const [theme, tokens] of [["light", LIGHT], ["dark", DARK]]) {
  test(`the ${theme} palette clears 7:1 on body text and 4.5:1 on the rest`, () => {
    const bad = [];
    for (const [what, fg, bg, need] of PAIRS) {
      const got = contrast(fg, bg, tokens);
      if (process.env.VYRN_CONTRAST_TABLE) console.log(`| ${theme} | ${what} | ${got.toFixed(2)}:1 | ${need}:1 |`);
      if (got + 0.005 < need) bad.push(`${what}: ${got.toFixed(2)}:1, needs ${need}:1 (${fg} on ${bg})`);
    }
    assert.deepEqual(bad, [], `${theme}: ${bad.length} pair(s) below the bar\n  ${bad.join("\n  ")}`);
  });
}

test("the explicit dark choice and the system dark default carry the same tokens", () => {
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

test("a grid whose column count the viewport decides never paints its own background", () => {
  const painted = [];
  for (const m of CSS.matchAll(/(^|\n)([^\n{}]+)\{([^}]*)\}/g)) {
    const [, , selector, body] = m;
    if (!/auto-fill|auto-fit/.test(body)) continue;
    if (/(^|[;\s])background(-color)?\s*:\s*(?!none|transparent)/.test(body)) painted.push(selector.trim());
  }
  assert.deepEqual(
    painted,
    [],
    `an auto-fill grid paints its own background, so a short last row is a filled empty cell:\n  ${painted.join("\n  ")}`,
  );
});
