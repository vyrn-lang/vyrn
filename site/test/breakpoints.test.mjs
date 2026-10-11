// The stylesheet's width breakpoints.
//
// Custom properties cannot parameterise `@media`, so the breakpoints are
// literals. This checks the two things an unmanaged set of them grows: a value
// off the sanctioned scale, and a max-width rule that can never fire because a
// later, wider rule sets the same property on the same selector.
//
// Run: node --test site/test/breakpoints.test.mjs
import { test } from "node:test";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const css = await readFile(fileURLToPath(new URL("../public/style.css", import.meta.url)), "utf8");

// Every width-conditioned block, with the rules it holds, in source order.
function widthBlocks(src) {
  const out = [];
  const re = /@media \((max|min)-width: (\d+)px\)\s*\{/g;
  for (let m; (m = re.exec(src)); ) {
    let depth = 0, end = m.end;
    for (let j = m.index + m[0].length - 1; j < src.length; j++) {
      if (src[j] === "{") depth++;
      else if (src[j] === "}" && --depth === 0) { end = j; break; }
    }
    const body = src.slice(m.index + m[0].length, end);
    const rules = [];
    for (const r of body.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
      const props = [...r[2].matchAll(/([a-z-]+)\s*:/g)].map((p) => p[1]);
      for (const sel of r[1].split(",").map((s) => s.trim()).filter(Boolean)) {
        for (const prop of props) rules.push({ sel, prop });
      }
    }
    out.push({ kind: m[1], px: Number(m[2]), rules, at: m.index });
  }
  return out;
}

const blocks = widthBlocks(css);

test("the stylesheet has width breakpoints to check", () => {
  if (blocks.length < 10) throw new Error(`only ${blocks.length} width queries found — the parse shape changed`);
});

// The sanctioned scale. Each value is a component's own decision about where
// its columns stop fitting: 640 is the phone rule for the page grid, 1024 is
// where the rail leaves the margin, and 720, 860, 900 and 960 are single
// components. 860 (`.showgrid`, a fixed 16rem sidebar) and 900 (`.demo`, a
// fluid split) are 40px apart on purpose. Adding a value is allowed; adding one
// by accident is not.
const SANCTIONED = new Set([640, 720, 860, 900, 960, 1024]);

test("no unsanctioned breakpoint has appeared", () => {
  const strays = [...new Set(blocks.map((b) => b.px))].filter((px) => !SANCTIONED.has(px)).sort((a, b) => a - b);
  if (strays.length) {
    throw new Error(
      `breakpoints not on the scale: ${strays.join(", ")}px. Reuse one of ` +
        `${[...SANCTIONED].sort((a, b) => a - b).join(", ")}px, or add it here and say what component needs it.`,
    );
  }
});

// A max-width rule is dead when a later rule sets the same property on the same
// selector at the same or a wider max-width: the later rule covers every width
// and wins the cascade. Progressive narrowing (`.rail` at 1024 and again at
// 640) is not that: the narrower rule comes second.
test("no max-width rule is shadowed by a later, wider one", () => {
  const dead = [];
  const maxes = blocks.filter((b) => b.kind === "max");
  for (const b of maxes) {
    for (const r of b.rules) {
      const shadow = maxes.find(
        (o) => o.at > b.at && o.px >= b.px && o.rules.some((x) => x.sel === r.sel && x.prop === r.prop),
      );
      if (shadow) dead.push(`${r.sel} { ${r.prop} } at max-width ${b.px}px, shadowed by the ${shadow.px}px block below it`);
    }
  }
  if (dead.length) throw new Error(`rules that can never fire:\n  ${[...new Set(dead)].join("\n  ")}`);
});
