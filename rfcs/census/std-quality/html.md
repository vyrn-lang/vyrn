# std/html.vyrn

Lines: 848. Exports: 11 (plus 4 `export type`: `Attr`, `Html`, `Sub`, `PatchOp`). Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

A caller builds a UI as a pure `Html` tree with `el`/`text`/`cls`/`attr`/`on`/`keyed`, then either renders it to an HTML string with `toHtmlString`/`document` for SSR, or hands it to `diff` to produce a minimal `PatchOp` stream that `web/vyrn-dom.js` applies to the live DOM. Text and attribute values are escaped byte-level; tag, attribute and event names are checked and refused. In-repo consumers include `site/app/docshell.vyrn:31`, `examples/domdemo.vyrn:14`, `bin/client/boot.vyrn:9` (uses `diff`), and the page modules under `site/pages/`.

## Findings

### 8. Allocation frequency — MEDIUM

What: `escapeText` and `escapeAttr` allocate a fresh byte array from `bytes("&amp;")` (and the other three entities) for every special character, then copy it onto the output one byte at a time through `appendBytes`.

Where: `std/html.vyrn:240` (also 242, 244, 246, 261, 263; helper at `std/html.vyrn:226-232`).

Evidence: `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/html/b.vyrn` from N:\lang printed `escape 1000 chars plain min 4.87 µs` against `escape 1000 chars half amp min 17.56 µs` — 500 escaped characters cost 12.7 µs more than 500 plain ones, about 25 ns per occurrence, each buying five byte-by-byte pushes plus one throwaway array.

Cost if unfixed: every SSR response pays it; `site/app/docshell.vyrn:31` calls `toHtmlString` on page trees whose text nodes routinely contain escaped characters.

Smallest fix: push the entity bytes inline in each branch (no intermediate array, no per-byte helper). RECOMMENDATION, NOT A DECISION.

### 15. Best/worst/average case — HIGH

What: diffing two structurally identical trees — the common case for most of the screen between keystrokes — emits zero ops yet costs about six times more than building the tree, because every element diff unconditionally deep-copies the new node's children (`htmlKids` → `copyHtmlArray`) and attributes (`htmlAttrs`), and compares attribute lists by rendering both into strings (`attrsEqual` calls `renderAttrs` twice).

Where: `std/html.vyrn:567` (deep child copy), `std/html.vyrn:555-562` (attribute copy), `std/html.vyrn:579-581` (render-to-compare), reached from `diffEl` at `std/html.vyrn:622-627` before any change test.

Evidence: same command as above: `build keyed 100 min 32.33 µs` versus `diff keyed identical 100 min 201.27 µs` (a 100-child keyed list, zero ops out), which also equals `diff keyed reverse 100 min 206.97 µs` — an unchanged tree diffs as expensively as a fully reordered one. Rendering itself is linear (200/400/800 text children: 54.88/111.02/220.37 µs, 2.0× per doubling), so the diff cost is copy-and-render overhead, not output size.

Cost if unfixed: `bin/client/boot.vyrn:9` and `examples/patchdemo.vyrn:12` run `diff` on the whole view tree per event; every unchanged subtree pays the full copy plus two attribute renders each time.

Smallest fix: compare keys/kinds first and skip the deep copies when both sides render-equal, or add a structural attrs comparison instead of render-to-compare. RECOMMENDATION, NOT A DECISION.

### 2. Algorithm complexity — MEDIUM

What: the keyed reconciliation matches each new child by a linear scan of all old children, and each emitted move rebuilds the live-index list twice with fresh arrays.

Where: `std/html.vyrn:700-707` (per-new-child `matchNew`) nesting `std/html.vyrn:771-787` (`findOld` full scan, O(newLen × oldLen) key lookups); `std/html.vyrn:823-834` (`listMove` = two O(n) fresh-array passes) inside the walk at `std/html.vyrn:729-743`, so a full reorder performs O(n²) element copies worst case.

Evidence: loop bounds above prove the bounds in terms of `newLen`/`oldLen`. Timing at these sizes is dominated by the linear copy term: `diff keyed reverse 50 min 99.73 µs` versus `diff keyed reverse 100 min 206.97 µs`, 2.08× per doubling, same command as above.

Cost if unfixed: a keyed list of a few thousand rows (a table view) turns each reorder event into millions of key lookups in the client bundle; `bin/client/boot.vyrn:9` runs this per event.

Smallest fix: index old keys in a map before matching and maintain the live-index list in place. RECOMMENDATION, NOT A DECISION.

### 28. Initialization overhead — LOW

What: `isVoid` constructs its 13-entry `Array<String>` literal on every call and scans it linearly, once per rendered element.

Where: `std/html.vyrn:318` (literal built inside the function), `std/html.vyrn:319-323` (linear scan), called from `renderEl` at `std/html.vyrn:379`.

Evidence: NOT MEASURED (the function is private and cannot be benched in isolation); the per-call construction at line 318 and the O(13) scan at lines 319-323 prove the shape.

Cost if unfixed: every `toHtmlString` call site, including the per-request SSR path in `site/app/docshell.vyrn:31`, allocates one throwaway array of 13 strings per element node.

Smallest fix: hoist the list to a module-level constant or replace the scan with a switch on the first bytes. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 29, 30.
