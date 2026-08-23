# std/graphql.vyrn

Lines: 2556. Exports: 12 (`export fn`; 2 of them `export gen fn`: `sdl` at :828 and `graphqlServer` at :2112). Other export kinds: 5 `export type` (`GqlSel` :910, `GqlQuery` :949, `GqlErr` :1353, `GqlOut` :1358, `GqlArg` :1675). Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

A contract-to-GraphQL bridge. `sdl(contract)` reflects a module's types and procedures at compile time and bakes a deterministic SDL document; `graphqlServer(contract)` bakes a generated handler that answers `POST /graphql` by parsing the query (`gqlParseQuery`), validating it against the baked type graph, running the contract's procedures, and projecting the returned value tree down to the selected fields with null-bubbling (`gqlProject`, `gqlAnswer`). In-repo callers: `examples/shelf/server.vyrn:16-18` mounts both generators on one server root; `examples/graphql.vyrn:38-39` imports the executor directly.

## Findings

### 2. Algorithm complexity — MEDIUM

What: parsing a selection set checks each sibling's response key and each argument name against every earlier one by linear scan, so parse time is O(k²) in the number k of siblings.
Where: `std/graphql.vyrn:1223` (response keys), `std/graphql.vyrn:1162` (argument names); both scan via `gqlHas` (:1915).
Evidence: `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/graphql/b.vyrn` from N:\lang: `{ f… }` with 64 distinct siblings min 14.59 µs, 256 siblings min 86.65 µs, 512 siblings min 250.75 µs — 17x for 8x siblings (linear would be 8x); the harness that builds the query string alone costs 4.55 µs at 520 fields. The proving loop is the per-sibling `gqlHas(keys, first)` inside the `while` over siblings at :1197-1249.
Cost if unfixed: every request served through a generated `graphqlHandle` re-parses client-supplied documents, so a wide document costs quadratically per request — paid today by `examples/shelf/server.vyrn:18`.
Smallest fix: track seen keys in the same array but bail out only past a size threshold with a sort-and-scan duplicate pass, or hash the names. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — MEDIUM

What: projection deep-copies data proportionally to nesting depth — each path step copies the whole response path, and each selected value is copied once per ancestor level instead of moved.
Where: `std/graphql.vyrn:1402-1409` (`gqlStep` copies every prior element per step), called per list element at :1509 and per selected field at :1546; value copies stack up at :1313 (`gqlMemberOf` deep-copies the member subtree), :1473 (leaf `copyJson`), and :1563 (parent copies the completed child again).
Evidence: same bench command: projecting a single-field chain 16 levels deep min 35.48 µs, 32 levels 130.00 µs, 64 levels 500.95 µs — about 3.8x per depth doubling, i.e. O(d²), against an input of d nodes built in O(d).
Cost if unfixed: every GraphQL reply pays depth-times-size copying before `emit`; worst case is bounded only by the parser's own 128-level cap (:1057), so a legal client query allocates ~128x the value tree's size in copies — paid by `examples/shelf/server.vyrn:18` per request.
Smallest fix: thread an owned, append-only path buffer through the walk instead of copying per step, and move child results into parents instead of copying them. RECOMMENDATION, NOT A DECISION.

### 28. Initialization overhead — LOW

What: the comptime generators build their output by repeated prefix-copying concatenation and re-read each declaration several times, so generation work grows quadratically in the emitted source size and in the type count.
Where: `std/graphql.vyrn:1925-1936` (`gqlJoin` does `out = out + p` per part), :556-561 and :567-571 (field strings accumulated the same way), :1989-1997 and :2092-2098 (resolver and schema tables concatenated arm by arm); each declaration's source is split by `gqlSplitDecl` three separate times (:661 via `gqlTypeSdl`, :690 via `gqlDeclNames`, :847 in the `sdl` loop), and `gqlSourceOf`/:258 and `gqlRawDoc`/:332 rescan all of `iface.types` per lookup (:271, :650), making input-position type resolution O(types) per field reference.
Evidence: NOT MEASURED — the generators are `gen fn`s and cannot run inside a bench body; the loop bounds above prove O(total²) byte copies for concatenation and O(T²) scans for T reflected types.
Cost if unfixed: compile time of any module using `sdl` or `graphqlServer`, paid at build time by `examples/shelf/server.vyrn:17-18`; invisible at runtime because the output is a baked constant.
Smallest fix: accumulate pieces in an `Array<String>` and join once (the module already imports nothing for this, so add a local join helper), and split each declaration once into a map-like table before emitting. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 29, 30.
