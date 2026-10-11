import { test } from "node:test";
import { sheet as css } from "./sheet.mjs";

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

// 860 (`.showgrid`) and 900 (`.demo`) are 40px apart on purpose.
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
