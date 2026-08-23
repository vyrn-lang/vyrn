# std/rpc.vyrn

Lines: 1475. Exports: 8 — seven top-level `export gen fn` (`validateContract` :185, `rpcServer` :393, `rpcClient` :505, `rpcInProcess` :576, `rpc` :1204, `client` :1359, `clientInProcess` :1434) plus one `export contract` (`Api`, :71). Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

Callers use it to derive typed RPC client and server halves from ordinary exported functions. `rpcServer`, `rpcClient` and `rpcInProcess` take one procedure module; `rpc(dir)`, `client(dir)` and `clientInProcess(dir)` take a whole api directory and mount every export at a path derived from the module path and export name. Every generator reflects over `moduleInterface`, validates the `Api` contract plus serializability, and returns generated Vyrn source as text. All the work in this file runs at compile time; the runtime surface is the emitted code. In-repo users: `std/graphql.vyrn:85` and `std/http.vyrn:65` import `validateContract`; `examples/fullstack/server.vyrn:12-13`, `examples/bin/server.vyrn:14-15`, `examples/rpc.vyrn:14-15` and `site/app/client/boot.vyrn:5-6` drive the generators.

## Findings

### 2. Algorithm complexity — MEDIUM

What: every emitter grows its output by repeated string concatenation, which measures quadratic in output size.
Where: `std/rpc.vyrn:95-106` (`joinList`, the shared helper), same `out = out + ..` pattern throughout, e.g. :250, :279-292, :304-330, :1106-1131.
Evidence: command `compiler/target/release/vyrn bench "C:/Users/demko/AppData/Local/Temp/claude/ox-a2/rpc/b.vyrn"` (bench body replicates the pattern: append a 17-byte part, `out = out + part`, n times) gives min 103.06 µs at 256 appends, 1.91 ms at 1024, 32.92 ms at 4096 — each 4× step costs about 17-19× more time, which is the n² signature. The pairwise collision scans (:837-853, :975-991) are O(R²) in derived routes by their loop bounds (`while i < len { while j = i+1 < len }`); a matching scratch scan measures 20.73 µs at n=512, 83.41 µs at n=1024, 332.20 µs at n=2048, i.e. exactly 4× per doubling. Also inside `rpc(dir)` one module's `moduleInterface` is fetched up to six times per generator run (:860, :815, :1053, :1224, :1142, :1179); whether the compiler caches it is NOT MEASURED.
Cost if unfixed: every project that generates an RPC surface pays this on each build — `examples/fullstack/server.vyrn:13` and `examples/bin/server.vyrn:15` regenerate a whole directory surface per compile, and `vyrn dev` repeats it on every change.
Smallest fix: accumulate parts in an `Array<String>` and join once per emitter. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — MEDIUM

What: the emitted `$schema` handler rebuilds the entire registry JSON out of concatenated fragments on every GET request, allocating fresh strings per fragment at runtime.
Where: `std/rpc.vyrn:337-358` emits `acc = acc + ...` per procedure; the directory form repeats it at :1137-1164.
Evidence: the concatenation pattern is the one measured above — 32.92 ms for roughly 68 KB built from 4096 appends (`vyrn bench` on `C:/Users/demko/AppData/Local/Temp/claude/ox-a2/rpc/b.vyrn`), so cost grows with the square of schema size and is paid per request, not once. Actual per-request latency for a real schema is NOT MEASURED.
Cost if unfixed: any served surface exposing `/rpc/$schema` pays it per fetch; `examples/bin/server.vyrn:14-16` mounts exactly this surface beside its OpenAPI projection.
Smallest fix: emit the schema as a precomputed constant string assembled once at module load. RECOMMENDATION, NOT A DECISION.

### 26. Syscall frequency — LOW

What: the directory scan lists each subdirectory twice.
Where: `std/rpc.vyrn:671-686` — for a non-`.vyrn` entry, `rpcIsDir(sub)` (:635-640) calls `listDir`, and the recursive `rpcScan(sub, ..)` (:665-669) immediately calls `listDir` on the same path again.
Evidence: the two call sites cited above; one extra `listDir` per directory per scan. Count of directories affected in a real tree: NOT MEASURED.
Cost if unfixed: each `rpc(dir)`/`client(dir)`/`clientInProcess(dir)` call doubles directory reads at generation time; callers regenerate per build (`examples/fullstack/server.vyrn:12-13`). Compile-time only, so the cost is small.
Smallest fix: pass the entry listing down from `rpcIsDir` into the recursive call, or try `moduleInterface` only after a successful stat. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 27, 28, 29, 30.
