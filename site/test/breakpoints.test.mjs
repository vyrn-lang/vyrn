// The stylesheet's width breakpoints.
//
// The census (rfcs/census/hardcoded-data.md, item 9) counted six px values
// across thirteen media queries and proposed stamping a breakpoints table into
// the sheet from the export generator, "since custom properties cannot
// parameterise `@media`". That last part is true, and it is why this does not do
// it: the fix would make style.css a build artifact — no longer editable in
// place, no longer previewable without a build — to name seven numbers that
// nothing compares against anything. The numbers are not the defect.
//
// The defect the count was pointing at is what an unmanaged set of breakpoints
// grows: `.demo` was collapsed to one column at BOTH 860px and 900px, and the
// 860px rule could never fire, because every width it covers is also covered by
// the later 900px rule. One component, two breakpoints, one of them dead.
//
// So this checks the two things a table would not have caught.
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

// THE SANCTIONED SCALE. Not tokens — CSS cannot take a custom property in a
// media condition — but a list, so that adding an eighth value is a deliberate
// act with a failing test in front of it rather than a number typed once.
//
// Each is a component's own decision about where its columns stop fitting, and
// they are not interchangeable: 640 is the phone rule for the whole page grid,
// 1024 is where the rail leaves the margin. The four in between are single
// components, and 860 and 900 sitting 40px apart is not an oversight —
// `.showgrid` carries a FIXED 16rem sidebar and stops fitting sooner than
// `.demo`, which is a fluid 5fr/7fr split with no minimum. Widening this list is
// allowed; doing it by accident is not.
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

// A max-width rule is DEAD when a later rule sets the same property on the same
// selector at a max-width that is the same or wider: every width the narrow rule
// covers, the wide one covers too, and the later one wins the cascade.
//
// This is not the same as "set at two breakpoints", which is ordinary
// progressive narrowing — `.rail` tightens its padding at 1024 and again at 640,
// and both fire, because the second is narrower and comes second.
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
