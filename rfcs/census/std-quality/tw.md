# std/tw.vyrn

Lines: 1127. Exports: 1. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

`grep -n "^export" std/tw.vyrn` matches twice, but the line 859 match (`export fn css()`) sits inside the `vyrn"""…"""` code quote of `twEmitCss` (`std/tw.vyrn:857-863`) — it is text of the synthesized module, not an export of this one. The only real export is `export gen fn tw` at `std/tw.vyrn:868`.

## What this module is for

A caller writes a flat `theme.json` and imports `import * as theme from tw("./theme.json")`. At compile time `tw` reads the file, flattens it into dotted keys, derives a closed utility vocabulary (colours × bg/text/border, spacing × padding/margin/gap/w/h, radius, font sizes, fixed static utilities), validates every class name, breakpoint key, and CSS value, and returns source for a module with two checked string types (`TwClass`, `Tw`), a checked `cls(c)` bridge into `std/html`, and a baked, byte-stable `css()` stylesheet. In-repo callers today: `examples/twdemo.vyrn:20`, `examples/bin/server.vyrn:22`, `examples/shelf/server.vyrn:22`, `examples/shelf/server/view.vyrn:9`, and `std/vyx.vyrn:2794`, which emits the import into compiled `.vyx` components.

## Findings

### 21. Footprint size — MEDIUM

What: `css()` bakes every derived class in all three state layers and once per breakpoint prefix, independent of which classes any template uses, so the served stylesheet scales with theme size, not usage.
Where: `std/tw.vyrn:736-745` — `twCssBody` re-runs `twBlockLit(vocab, bp.token + ":", 0, n)` for every breakpoint over the full `vocab.length * 3` index space; `std/tw.vyrn:561-576` derives the whole vocabulary unconditionally.
Evidence: the shelf example theme (`examples/shelf/theme.json`, 828 bytes, 158 derived classes, 2 breakpoints) produces a 60,616-byte sheet (1,422 rules); a synthetic 310-class, 4-breakpoint theme produces 197,333 bytes; a minimal 1-colour theme produces 5,621 bytes. Command: `compiler/target/release/vyrn run main.vyrn` on scratch programs printing `theme.css().byteLength` from `C:/Users/demko/AppData/Local/Temp/claude/ox-a2/tw/{small,big}/main.vyrn`. The site guide promises the opposite behaviour: "emits one stylesheet holding only the rules those classes need. Nothing is fetched and nothing unused ships" (`site/guide/guide.vyrn:558`) — the implementation ships every derived rule.
Cost if unfixed: `examples/shelf/server.vyrn:61` and `examples/bin/server.vyrn:58` return `theme.css()` to every browser that requests `/theme.css`, so each page load downloads all 60 KB while the shelf pages use a fraction of the 1,422 rules; the baked literal also lands whole in every importing binary.
Smallest fix: prune the emitted sheet to classes that appear in scanned templates before baking, or document whole-vocabulary emission as the v1 contract. `RECOMMENDATION, NOT A DECISION`.

### 2. Algorithm complexity — LOW

What: one `tw(...)` generation makes about eight full passes over the flattened theme entry array instead of routing each entry once.
Where: `std/tw.vyrn:562-565` calls `twAxisOf` five times (each a full scan of `theme.entries`, loop at `std/tw.vyrn:377-382`), `std/tw.vyrn:914` calls it twice more (breakpoints, safelist), and `std/tw.vyrn:405` plus `std/tw.vyrn:651` scan all entries again for unknown keys and unsafe values; `std/tw.vyrn:407` and `std/tw.vyrn:652` add an O(families)-per-entry linear `includes` search.
Evidence: complexity is O(F·E) with F = 6 fixed families and E entries, proved by the scan loop bounds at `std/tw.vyrn:377-382` and the call sites above. Measured on the replicated idiom (`vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/tw/b.vyrn`): one filter pass over 1,024 entries min 113.73 µs; eight such passes min 416.80 µs. End-to-end this stays small: `vyrn run` of a program generating the 197,333-byte sheet took 0.36 s wall against 0.19 s for the minimal theme (same command shape, release binary).
Cost if unfixed: compile-time only, paid once per import by every generator consumer, e.g. `std/vyx.vyrn:2794` and `examples/shelf/server.vyrn:22`; at current theme sizes the redundancy costs well under a millisecond.
Smallest fix: flatten once into a keyed map or group entries by head segment during `twFlattenObj` so each family projection is a lookup, not a rescan. `RECOMMENDATION, NOT A DECISION`.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 22, 23, 24, 25, 26, 27, 29, 30.
