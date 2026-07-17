# RFC-0035 — The Patch Protocol: Wasm-Side Diffing (Reactivity M5a)

- **Status:** Draft (design locked)
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
