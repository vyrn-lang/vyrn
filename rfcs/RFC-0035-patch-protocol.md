# RFC-0035 — The Patch Protocol: Wasm-Side Diffing (Reactivity M5a)

- **Status:** Implemented
- **Status (was):** Draft (design locked)
- **Depends on:** RFC-0026 (M1 `Html` tree + M2 `vyrn-dom.js` — whose
  update loop this upgrades), RFC-0029 (module state everywhere — the
  retained previous tree lives in module state), RFC-0024 (payload enums
  on the wire — `PatchOp` is one)
- **Evidence:** every client event currently serializes the WHOLE view
  tree to JSON, ships it across the extern boundary, parses it, and
  diffs in JS — O(tree) wire cost per keystroke. RFC-0026 M5 sketched
  Svelte-style compiled reactivity; that requires cross-component
  dependency analysis (props flow through files the `.vyx` compiler sees
  one at a time) — a heavy design for an unproven bottleneck. The
  measured cost is the JSON hop; this RFC removes it with a library-only
  change and keeps dirty-bit compilation as the recorded escalation
  (M5b) should profiling ever demand it.

---

## The change

The diff moves into wasm, written in pure Vyrn:

```vyrn
// std/html additions
type PatchOp =
    | OpSetText(Array<Int64>, String)          // path, new text
    | OpSetAttrs(Array<Int64>, Array<Attr>)    // path, full new attr list
    | OpReplace(Array<Int64>, Html)            // path, new subtree
    | OpInsert(Array<Int64>, Int64, Html)      // parent path, index, node
    | OpRemove(Array<Int64>, Int64)            // parent path, child index
    | OpMove(Array<Int64>, Int64, Int64)       // parent path, from, to

fn diff(old: Html, new: Html) -> Array<PatchOp>
```

```vyrn
// client root — the new loop
let mut lastTree: Html = Empty

export extern fn vyrnPatch() -> String {
    let next = view()
    let ops = diff(lastTree, next)
    lastTree = next
    return toJson(ops)
}
```

`vyrn-dom.js` boots exactly as today (`vyrnView()` → build DOM), then on
every subsequent render calls `vyrnPatch()` and applies the ops. Only
changes cross the boundary.

## Semantics (locked)

- **Paths** are child-index vectors from the mount root (`[0, 2, 1]`),
  evaluated against the DOM **as already patched by preceding ops** —
  ops apply strictly in order; the differ emits them so that each op's
  path is valid at its turn (removes/moves emitted with this discipline;
  document the exact order convention in std/html's docs).
- **Keyed children:** `diff` implements the SAME keyed algorithm
  `vyrn-dom.js` uses today (`Key` attrs match nodes, `OpMove` reorders,
  unmatched old → `OpRemove`, unmatched new → `OpInsert`), so DOM-node
  identity behavior is unchanged — the keyed-reorder guarantees (focus,
  typed values, JS properties surviving) are preserved by construction.
- **Attrs are replaced wholesale per node** (`OpSetAttrs` carries the
  full new list; the host reconciles attribute-by-attribute against the
  live element exactly as it does today — including the event-attr and
  `data-key` handling). Text changes are `OpSetText`. A tag change is
  `OpReplace`. `Raw` nodes compare by string equality (differ → replace).
- **`diff` is pure and total** — a parity citizen. Its op streams are
  part of the observable contract: snapshot tests assert exact op
  sequences for canonical cases (text-only change, keyed reorder,
  insert/remove at ends and middle, attr toggle, subtree replace,
  `Empty` transitions).
- **Protocol negotiation, host-side and backward-compatible:** if the
  wasm exports `vyrnPatch`, the runtime uses boot-view + patches; if
  not, it falls back to today's full `vyrnView()` + JS diff loop
  unchanged. Existing apps keep working untouched; `examples/domdemo`
  keeps the old loop deliberately as the fallback's regression proof.
- **Subscriptions/effects unchanged** (`vyrnSubs` diffing stays
  host-side and by-value; `data-effect` appear/disappear now keys off
  applied ops — verify the registry still fires correctly under
  patching).

## What this deliberately does NOT do (M5b, recorded)

No dirty bits, no per-binding dependency tracking, no generated
`patch(dirty)` per component: the full tree is still recomputed in wasm
per event (pure function calls — cheap); only the boundary cost drops
from O(tree) to O(changes). If a real app shows tree recomputation
itself as the bottleneck, M5b is the RFC-0026 sketch: the `.vyx`
compiler's static binding knowledge + store-write dirty bits — a design
that would sit ON TOP of this protocol (its output is the same
`PatchOp` stream), so nothing here is throwaway.

## Consumers / proof

- **shelf + `examples/vyxdomdemo`** switch to `vyrnPatch` (one export +
  the `lastTree` state each; view code untouched). Browser-verify all
  flows AND instrument the win: log serialized bytes per interaction
  before/after on shelf (add/rate/delete/filter) — report the numbers.
- **`examples/patchdemo.vyrn`** (or extend an existing parity citizen):
  `diff` op-stream snapshots three-way byte-identical.
- The keyed test from M2 re-run under patching: reorder preserving a
  focused input's typed value and JS property.

## Out of scope

M5b compiled reactivity (recorded above), binary encodings (JSON ops are
already O(changes); revisit only with numbers), server-side use of
`diff` (nothing stops it, nothing needs it), any `.vyx`/generator
changes.

---

## As-landed notes

Library + host-runtime milestone, ZERO compiler changes, as predicted.

### Where it lives

- `std/html`: `PatchOp` (the six locked ops) + `diff(old, new) ->
  Array<PatchOp>`, pure/total Vyrn (recursion over the tree, keyed and
  positional child reconciliation, no `break`/`continue`/`%` — none exist
  in the language, so loops carry `found`/flag variables). Attribute
  equality is `renderAttrs(a) == renderAttrs(b)` — a total, order-sensitive
  reuse of the existing renderer; a false negative only costs a redundant
  (still-correct) `OpSetAttrs`.
- `web/vyrn-dom.js`: `applyOps` (naive in-order path resolution against
  `childNodes`), `reconcileAttrs` (reuses the existing per-attribute
  `applyAttrs` with old attrs read from the LIVE element — form
  `value`/`checked` stay live properties, so a reorder or unrelated attr
  change never clobbers a typed value), `runEffectsDom` (effects keyed off
  the live DOM, since the new vnode tree never reaches JS), and host-side
  negotiation (`typeof exports.vyrnPatch === "function"`). Boot with a
  patch export = an `Empty` root comment + the first patch (a full
  `OpReplace`), which primes wasm's `lastTree` and builds the DOM through
  the same applier every render uses.
- Consumers: `examples/vyxdomdemo` and `examples/shelf` client each added
  one `let mut lastTree: Html = Empty` + `export extern fn vyrnPatch()`;
  view/handler/widget code untouched, `app.js` untouched (negotiation is
  entirely inside `mount()`). `examples/domdemo` deliberately keeps only
  `vyrnView` as the fallback regression. `examples/patchdemo.vyrn` is the
  new parity citizen (op-stream `toJson` snapshots, three-way byte-identical).

### The locked op-emission order (documented in `std/html`)

Per element's children, so a naive in-order applier always lands right:

- **node level:** kind/tag change → one `OpReplace` (no recursion);
  otherwise `OpSetText`/`OpSetAttrs` for the node before its children.
- **positional:** recurse the common prefix (stable indices) → `OpRemove`
  the surplus tail HIGH-TO-LOW → `OpInsert` the new tail LOW-TO-HIGH.
- **keyed** (mirrors `vyrn-dom.js`'s `patchKeyed`): `OpRemove` dropped keys
  HIGH-TO-LOW → walk the new order LEFT-TO-RIGHT emitting `OpInsert`/`OpMove`
  (reused nodes only ever move leftward, so one `insertBefore` realizes
  each move; identity — focus/typed value/JS property — preserved by
  construction) → recurse into surviving matched children at their FINAL
  indices. Structure fully settles before any content op for that level, so
  every content path is the node's final position.

### Bytes per interaction (shelf, measured in-browser: op stream vs the full-view JSON the old loop shipped)

| Interaction              | ops bytes | full-view bytes | reduction |
|--------------------------|-----------|-----------------|-----------|
| unchanged render         | 2         | 1209            | 99.8%     |
| tag filter               | 273       | 2791            | 90.2%     |
| rate (cycle 1..5)        | 236       | 2799            | 91.6%     |
| delete — confirm prompt  | 396       | 2904            | 86.4%     |
| delete — complete        | 248       | 2254            | 89.0%     |
| add a book               | 987       | 2968            | 66.7%     |
| locale toggle (en↔uk)    | 720       | 2982            | 75.9%     |
| server 422 panel         | 329       | 3096            | 89.4%     |

The initial list populate (boot `Empty`→full list) is a genuine full
render — `2475` vs `3322`, i.e. the first patch is a whole-tree `OpReplace`
and is expectedly the same order of magnitude as the full view (it is one).

### Browser evidence

- shelf: add / rate / delete (two-step confirm) / tag filter / locale
  toggle / 422 panel all green under patching; console clean (no errors).
- **Keyed under patching (M2 guarantee re-proven):** typed value
  `SURVIVE-ME-42` + JS property `js-prop-99` + focus on an input in the
  first keyed row; `rotate` moved that row to last via `OpMove`; the SAME
  DOM node was reused (`===`) and all three survived.
- **data-effect:** a `data-effect` node gated on `count > 0` mounts and
  unmounts as `+1`/`-1` add/remove it via ops; the registered effect logged
  `flash MOUNT` (and applied its outline) then `flash UNMOUNT` (cleanup).
- **Fallback:** `domdemo` exports no `vyrnPatch` → runtime stays on the
  full-view loop; increment + keyed rotate (value/property/identity
  survive) work. Both negotiation branches thus proven live.
- **Soft-nav interplay (RFC-0034):** navigated shelf home → /about → home
  via soft-nav; the island re-booted with a FRESH interpreter instance
  (locale reset to default, i.e. module state — including `lastTree` —
  re-initialized), and incremental patching resumed correctly (296-byte
  filter op post-reboot). No stale wasm state.

### Corners

- No language walls hit: `Array<PatchOp>` (payload enum with `Array<Int64>`
  and `Html`/`Attr` payloads) codes and round-trips through the existing
  RFC-0024 codec unchanged; recursion depth was a non-issue for these trees.
- Enum variants come into scope with the type import, so
  `let mut lastTree: Html = Empty` needs only `import { Html } from "std/html"`.
- 847 workspace tests + 11 LSP tests + 4 three-way parity suites green,
  0 warnings; `patchdemo` adds 7 in-language snapshot tests.
