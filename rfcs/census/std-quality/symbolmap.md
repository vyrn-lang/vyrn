# std/symbolmap.vyrn

Lines: 132. Exports: 4 (`symbol`, `strField`, `mapJson`, `symbolMapFn`; there is also one `export type Symbol` at line 34). Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

A generator calls this module when it emits a module. It builds the RFC-0073 symbol map: one record per exported symbol carrying the compiler `Origin` the declaration came from plus open derived facts. `symbol` and `strField` construct records, `mapJson` renders the compact JSON document, and `symbolMapFn` wraps the document in an `export fn symbolMap<Slug>() -> String` declaration the generator appends to its output, so the map travels inside the module and shares its content-hash cache entry.

## Findings

### 8. Allocation frequency — LOW

What: `mapJson` materializes a whole intermediate JSON tree before rendering it, so each mapped symbol pays several allocations (a field array, four `JsonField` nodes, one `Json` wrapper) on top of the output string.
Where: `std/symbolmap.vyrn:63-78` (`syms` accumulates `JObj(fs)` per symbol, then `emit(JObj(doc))` walks the tree).
Evidence: command `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/symbolmap/b.vyrn` printed `bench "mapJson 100 syms" min 315.35 µs` and `bench "mapJson 400 syms" min 1.22 ms` — about 2.9-3.1 µs per symbol and scaling 3.9x for 4x the symbols, confirming linear work with per-symbol allocation.
Cost if unfixed: every generator emission pays it once per module — `std/rpc.vyrn:1234`, `std/rpc.vyrn:1391`, and `std/http.vyrn:1779` all route their symbols through `symbolMapFn` into `mapJson` — but the content-hash cache (module doc, lines 10-12) means a warm cache never re-pays.
Smallest fix: render fields directly into the output string instead of building the tree first. RECOMMENDATION, NOT A DECISION.

### 7. Peak memory use — LOW

What: the JSON tree and the rendered string live at the same time, so peak memory is roughly twice the finished document.
Where: `std/symbolmap.vyrn:74-78` (the `doc` array holds the tree while `emit` produces the string).
Evidence: NOT MEASURED (no memory facility exercised; the two-live-representations claim follows from lines 63-78: `syms` stays reachable through `doc` until `emit` returns).
Cost if unfixed: `std/rpc.vyrn:1382` hands `consume types.symbols` straight to `symbolMapFn` for the whole client surface, so the largest generated client holds both copies during emission.
Smallest fix: same single-pass rendering as the axis-8 fix. RECOMMENDATION, NOT A DECISION.

### 28. Initialization overhead — LOW

What: `symbol` hand-copies `name` and the whole `origin` and `strField` hand-copies `key` on every call.
Where: `std/symbolmap.vyrn:38` (`name.copy()`, `origin.copy()`) and `std/symbolmap.vyrn:43` (`key.copy()`).
Evidence: command `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/symbolmap/b.vyrn` printed `bench "symbol ctor x1000" min 60.84 µs` — 61 ns per construction including the two copies.
Cost if unfixed: `std/rpc.vyrn:1011` and `std/rpc.vyrn:1338` build one symbol per exported function or type, so a large API pays one extra copy chain per symbol at generation time only.
Smallest fix: none recommended without a consume/move analysis of the parameter passing rules; 61 ns per generation-time call is noise next to the 3 µs `mapJson` spends on the same symbol. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 2, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 29, 30.
