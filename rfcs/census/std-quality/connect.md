# std/connect.vyrn

Lines: 380. Exports: 2 (`export gen fn connectServer` at `std/connect.vyrn:267`, `export gen fn connectClient` at `std/connect.vyrn:350`). Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

A caller points `connectServer` at a contract module and mounts the generated `connectHandle` beside `rpcHandle`, so the same contract serves Connect unary-JSON paths (`POST /<service>.<Proc>`) and `/rpc/*` at once. `connectClient` emits the symmetric client stubs over one `vyrnConnectCall` extern. Both generators run at compile time over `moduleInterface` reflection and return emitted Vyrn source; the module holds no served-process code of its own beyond the two test blocks.

## Findings

### 2. Algorithm complexity — LOW

What: the emitted router answers every request with a sequential chain of one full-path string equality test per procedure, so routing is O(P) string comparisons per request, P = exported procedures of the contract.
Where: `std/connect.vyrn:241-249` (the emission loop that prints one `if req.path == "<prefix><name>"` block per procedure, before the `startsWith` fallthrough at line 250).
Evidence: benching the generated routers against a foreign path gives `min 71 ns` for P=2 and `min 143 ns` for P=32, a linear-per-procedure scan with an absolute cost of a few nanoseconds per procedure. Command, run from `N:\lang`: `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/connect/router2.vyrn` and `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/connect/router32.vyrn`. A matching hit costs `min 3.07 us` (first procedure) versus `min 3.14 us` (last), so decode/encode dominates real calls and the scan adds little.
Cost if unfixed: every request that reaches `examples/shelf/server.vyrn:13`'s mounted `connectHandle` walks the whole chain before the server falls through to the next surface; the measured size is nanoseconds.
Smallest fix: emit a prefix check followed by a per-service match table instead of P equality tests. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — LOW

What: the pure string helpers grow a fresh `Array<UInt8>` one element per byte through `push`, twice over for `connectServiceName`.
Where: `std/connect.vyrn:56-67` (`connectCapFirst` copies the whole name byte by byte to change one byte) and `std/connect.vyrn:108-135` (`connectServiceName` builds `seg`, then rebuilds `trimmed` from it).
Evidence: loop bounds prove one `push` per input byte, so O(n) element-wise grows per call at compile time, n = name length; per-call timing NOT MEASURED. The neighbouring pattern of building emitted source by `out = out + chunk` in loops (`std/connect.vyrn:190`, `243-253`, `274-290`) was measured and behaves near-linearly, not quadratically: appending 128 lines costs `min 350 ns` and 1024 lines `min 1.34 us` (`compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/connect/b.vyrn`).
Cost if unfixed: only compile time of programs importing the generators; contract names are short, so the cost is negligible today. No served-process caller pays it.
Smallest fix: copy the unchanged suffix directly and patch the first byte, or slice without rebuilding. RECOMMENDATION, NOT A DECISION.

### 28. Initialization overhead — LOW

What: `connectImportBlock` de-duplicates module specifiers with a linear `connectListContains` scan inside its loop over types, and rescans all types once per collected specifier, so the import block is built in O(T x S) equality comparisons, T = exported types, S = distinct declaring modules, S <= T.
Where: `std/connect.vyrn:170-175` (contains-scan inside the type loop) and `std/connect.vyrn:184-188` (full type rescan per specifier).
Evidence: loop bounds prove the product; T and S are contract sizes, so the cost appears once per `connectServer`/`connectClient` invocation at load time. Timing NOT MEASURED because no in-repo contract approaches sizes where it matters (the largest consumer, `examples/shelf/server.vyrn:13`, reflects one api module).
Cost if unfixed: compile-time only; a contract spread over many imported wire modules pays a quadratic comparison count while its generated module loads.
Smallest fix: sort specifiers once and merge adjacent duplicates, or key them in a `Map`. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 29, 30.
