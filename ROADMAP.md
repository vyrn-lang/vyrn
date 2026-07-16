# Vyrn — status & roadmap

The forward-looking companion to the [RFCs](rfcs/). What ships today, what's next,
and the one decision the rest of the language waits on.

**Every feature below is verified three ways**: the clang-compiled native
binary AND the `wasm32-wasi` module produce byte-identical stdout, stderr, and
exit codes against the tree-walking interpreter (the reference semantics),
across **46 examples** and **606 tests** (0 warnings) — including every runtime
trap path (one canonical `error: ...` wording on stderr, exit 1, everywhere)
and the canonical I/O error strings (RFC-0014). The whole corpus is kept
canonical by `vyrn fmt` (RFC-0017) and re-verified by the parity harness.
The permanent corpus harness is
`cargo test -p vyrn-cli --test parity -- --ignored` (needs clang; the wasm
column runs when `tools/` holds a wasi-sysroot + wasmtime, or via
`$WASI_SYSROOT`/`$VYRN_WASMTIME`; the known-divergent list is empty and must
stay that way).

**WebAssembly**: `vyrn build prog.vyrn --target wasm` compiles the same
LLVM IR against wasi-libc (`--target=wasm32-wasip1`). The runtime is
libc-portable: stream handles and all size_t-sensitive calls (`strlen`,
`malloc`, `realloc`, `strncmp`, `snprintf`) route through a tiny embedded C
shim with 64-bit-clean prototypes, and the C `main` lives in the shim (the IR
exports `vyrn_entry`), so MSVC, glibc, and wasi-libc all link the same module.
Exit codes are portable in 0..126 (WASI's constraint). **The browser demo
ships** (`web/`): a hand-rolled WASI preview1 shim (`wasi-min.js`, zero
dependencies) runs any `vyrn`-built module in a page — a compute-only vyrn
module imports five preview1 functions (`fd_write`, `fd_fdstat_get`,
`fd_close`, `fd_seek`, `proc_exit`); a module using input (RFC-0014) pulls in
seven more (`args_get`, `args_sizes_get`, `fd_read`, `fd_fdstat_set_flags`,
`fd_prestat_get`, `fd_prestat_dir_name`, `path_open`), which the shim serves
with *graceful degradation* — no argv, stdin at EOF, no filesystem — so
`args()` is empty, `readLine()` is `None`, and `readFile` yields its canonical
`Err` in-page instead of crashing. stdout/stderr stream into the page, and
trap parity holds end-to-end (division by zero prints the canonical
`error: division by zero` + exit 1 in the browser, byte-identical to interp
and native). `web/build.ps1` builds the example modules; any static server
serves it. **JS interop stage 1 ships (RFC-0012 M1)**: `extern fn
jsLog(msg: String)` declares a host import from the wasm `vyrn` namespace —
scalars cross by value (`Int64` is a `BigInt` in JS), a `String` crosses as
a `(ptr, len)` pair that `wasi-min.js` decodes, and the page supplies the
functions via `runVyrn(bytes, { extern: { … } })` (a missing one is a clear
instantiate error naming provided vs wanted). On the interpreter and the
native binary an extern *call* traps with canonical wording (``error:
extern `name` is not available on this target``, byte-identical between
the two — asserted by the parity harness's `WASM_ONLY` list; declaring is
fine everywhere, and `KNOWN_DIVERGENT` stays empty). Extern calls are
never spawn-safe (a host effect), and the signature domain is checked
(scalars + String only). See `examples/externdemo.vyrn` +
`web/externdemo.html`. **JS interop stage 2 ships too (RFC-0012 M2)**:
`export extern fn vyrnAdd(a: Int64, b: Int64) -> Int64 { … }` is a normal,
body-checked Vyrn function that is *additionally* exported to JS — after
`_start` runs `main`, the host calls it on the live instance
(`runVyrn(...).exports`). The export is an inline `wasm-export-name`
attribute on the `define` (auto-rooted; the module's `__vyrn_malloc` is
force-exported when a String parameter is present so the shim can allocate
argument buffers). A `String` crosses *into* an exported call as a single
`ptr` (the JS caller allocates + NUL-terminates — the asymmetry vs. an
import's `(ptr, len)`), and a returned `String` is NUL-decoded from linear
memory: `greet(String) -> String` round-trips a string both ways.
Because they never trap (only body-less imports do),
`examples/externdemo2.vyrn` stays fully three-way parity-capable
(interp == native == wasm); the browser round trip is in the M2 section of
`web/externdemo.html`. **The event-loop story ships too (RFC-0013)**:
`export extern` gave a live module callable after `main` returns; **module
state** — a top-level `let [mut] name = init` in the root module — gives it
state that *survives between entries*, so a handler called at t=1s finds the
counter `main` set up at t=0. A wasm module can't block the page or suspend,
so a Vyrn "event loop" is an inversion: **the host owns the loop** (a browser
`setInterval`, later a server runtime) and calls exported handlers; Vyrn owns
the state and the logic — the same shape wasm components and every embedded
runtime use, with no new control flow. Globals initialize once, in declaration
order, before `main` (native/wasm run them in a synthesized
`@__vyrn_globals_init` called from `vyrn_entry`; the interpreter seeds a
persistent frame). They validate on every store like any value boundary, are
never dropped (module lifetime, safe-leak), can't be `consume`d or `drop`ped,
and any function that touches one is not spawn-safe (module state is shared by
definition) — transitively. `examples/eventloop.vyrn` drives the handlers in a
deterministic in-`main` loop so it is a normal three-way parity citizen; the
live version is `web/eventloop.html`, where a timer renders the count and a
button calls `reset()`. Next on the browser path: the long-range goal is
same-language server (native SSR) + client (wasm), with validated types as the
wire contract.

**The server half's v1 ships (RFC-0016).** `vyrn serve prog.vyrn [--port N]`
is a hand-rolled HTTP/1.1 host on `std::net` (no crates, the no-crates ethos)
running an ordinary `fn handle(req: Request) -> Response` under the interpreter
— the *same program shape as the browser*: the host owns the accept loop, Vyrn
owns the module state (`hits` persists across requests) and the logic.
`Request`/`Response` are parser-injected records (like `Schema`/`Issue`), so
`handle` is a plain, testable, checkable function — `main` and `test` blocks
call it directly, and `examples/server.vyrn` is a normal three-way parity
citizen (interp == native == wasm). Sequential accept loop, one request at a
time, so module state is race-free by construction; a `handle` trap is caught,
logged with the canonical wording, and answered 500 (the server survives).
This RFC also **settled the async question: Vyrn adds no `async`/`await`** (for
now, deliberately) — function suspension is the highest-risk feature the
three-backend invariant could face (wasm can't switch stacks), the host-owns-
the-loop model already covers the real use cases, and determinism is the
product. When concurrency comes to the server it will be worker threads calling
`handle` in parallel, gated on the isolation analysis — same output, no new
language surface.

A 2026-07-15 hardening pass fixed ~40 reviewed defects: native
use-after-free/heap-corruption bugs (cell `set`, region escapes, `list` in
regions), invalid-IR shapes (dead `ret`, `phi void`, unpooled predicate
strings), interp/native numeric divergences (wrapping overflow semantics,
sized-int operand truncation, Float64 refinements, NaN, division traps),
validated-type soundness holes (nominal predicated records, match-arm
laundering, `modify` width subtyping, generic capability checks, spawn purity
through protocols, movecheck gaps), and a lexer/parser diagnostics batch.
A follow-up closed the rest of that review's deferred list: the `?`
propagate path now frees in-scope owned temporaries exactly like `return`
(was leaking every owned local on the early exit); region nesting past the
arena stack's 64 slots now traps (`error: region nesting exceeds 64`) in
both backends instead of corrupting memory past the fixed `[64 x ptr]`
global — and the checker's return-path analysis learned that a `region`
body that always returns satisfies the enclosing function (it runs exactly
once, unlike a loop). Allocation failure traps (`error: out of memory`)
instead of dereferencing null — the C shim's `__vyrn_malloc`/`__vyrn_realloc`
check once at the choke point, including the ILP32 guard against a 64-bit
size silently truncating in the `(size_t)` cast on wasm32. `schemaOf(T)`
was enriched: `Schema` now carries `name`, the full base spelling (sized
ints included), the `///` `doc`, `multipleOf`, `minLength`/`maxLength`,
and the regex `pattern` — enough to assemble real OpenAPI fragments in
ordinary Vyrn code (see `examples/reflection.vyrn`). Along the way, a
`///` block separated from a declaration by a blank line (a file header)
no longer glues onto that declaration's doc.

**Modules (RFC-0010)**: `import { names } from "./path"` / `export fn|type|protocol`
— TS-style, resolved relative to the importing file (`.vyrn` appended), with
`std/...` reserved for the standard library (itself written in Vyrn: `std/math`,
`std/strings` — parity for free). A loader/linker stage parses each file once,
enforces exports/visibility (a module only sees foreign names it imported;
importing an enum brings its variants, a protocol its methods), rejects cycles
and cross-module name collisions, then links everything into ONE Program — the
checker, interpreter, both code generators, and the parity harness are unaware
modules exist. I/O lives behind a `ModuleResolver` trait (filesystem in the
CLI, in-memory maps in tests). **JSON Schema type imports** (M2):
`import type { User } from "./api.schema.json"` synthesizes validated types
from a schema document — the exact inverse of `jsonSchema(T)` (bounds/lengths/
patterns become `where` clauses, `required` steers `Option<T>`, `$defs`,
`#/$defs/..` and root `#` refs resolve, `enum`-of-strings becomes a
payload-less Vyrn enum, constrained fields become synthetic `User.age`
types), byte-exact round-trip with the emitter, and any inexpressible keyword
is a hard error. The emitter side is correspondingly rich: named nested types
render as `$ref`s into a `$defs` section (recursion is a real `$ref` — `"#"`
for the root — not a lossy comment), sized ints carry their width bounds as
part of the wire contract, and payload-less enums emit `enum` arrays. **The
JSON codec** (RFC-0018) moves *values* across that same wire: `toJson(x) ->
String` (canonical — declaration-order fields, `None` omitted, bare `Option`
→ `null`, numbers through the same `toString` rendering) and `fromJson(T, s)
-> Validation<T>`, which never traps — it ignores unknown fields, takes
absent-or-`null` for an `Option`, parses integers **exactly** (never through
`f64`), and runs every `where` clause, accumulating one `Issue` per failure
(`json.parse`/`json.type`/`json.missing`/`validate`, each with a dotted/indexed
path). Encode renders through the canonical scalar path and decode runs the
same predicate lowering as every other boundary, so the encoded bytes AND
every Issue's key/path/message are byte-identical across all three backends
(`examples/jsoncodec.vyrn`). **Project manifest** (M3): an optional `vyrn.json`
(`name`/`main`/`dependencies`) found by walking up from the cwd — `vyrn run/
check/build` need no file argument in a project, bare import specifiers
(`import { x } from "money"`) resolve through the `dependencies` map (an
import map; targets are relative-to-manifest or `std/` for now), and `vyrn
new <name>` scaffolds a runnable project, `vyrn deps` prints the resolved
module graph. Bare `vyrn run file.vyrn` stays manifest-free forever.
**Reproducible remote imports** (M4): `github:owner/repo@ref/path`,
`gist:user/id[@rev]/file`, and `https://...` specifiers (inline or as manifest
targets). The first resolve pins each dep in `vyrn.lock`
(`specifier ⇥ immutable-url ⇥ sha256`, floating refs frozen to a commit via
`git ls-remote`); content lives in the content-addressed
`~/.vyrn/cache/sha256/` and is hash-verified on EVERY load (tampering fails
loudly). `--offline`/`VYRN_OFFLINE=1` builds never touch the network;
`vyrn add <spec> [--name alias]` fetches+pins+records, `vyrn update [alias]`
is the only way a pin changes, and `vyrn vendor [--check]` copies the lock's
blobs into `./vyrn_vendor/` — a committed checkout builds forever even if the
upstream is deleted (any copy of a file with the locked hash restores it).
Remote modules are sandboxed: relative imports stay inside their pinned base,
no local paths, no bare specifiers. Zero new crates (hand-rolled SHA-256 with
NIST vectors; `curl`/`git ls-remote` subprocesses, all in vyrn-cli).

**Input I/O (RFC-0014)**: a Vyrn program can finally *read* — the "computes
from constants only" gap is closed. `args() -> Array<String>` (argv[1..];
`vyrn run prog.vyrn x y` forwards trailing arguments), `readLine() ->
Option<String>` (one stdin line, `\r\n`/`\n` stripped, `None` at EOF, repeated
calls stream), `readFile(p) -> Result<String, String>`, `writeFile(p, c) ->
Result<Bool, String>` — plus the byte layer: `readFileBytes(p) ->
Result<Array<UInt8>, String>`, `bytes(s) -> Array<UInt8>` (now a true
i8-stride byte array), and `stringFromBytes(b)` with the pinned round-trip law
`stringFromBytes(bytes(s)) == Ok(s)`. All are effects (spawn-forbidden, never
constant). **Error payloads are canonical Vyrn wording, never OS text** —
``Err("cannot read `p`")``, ``Err("`p` is not valid UTF-8")``, ``Err("`p`
contains a NUL byte")`` (NUL is valid UTF-8 but cannot live in a
NUL-terminated String, so it is rejected explicitly), ``Err("cannot write
`p`")`` — byte-identical across all three backends; the wording lives in ONE
place (the codegen's `@.io.*` globals; the interpreter matches it). The parity
harness gained the I/O conventions: an `examples/<name>.stdin` fixture pipes
into all three backends, every run's cwd is `examples/`, and wasmtime gets
`--dir .` — `examples/input.vyrn`, `examples/files.vyrn`, and
`examples/args.vyrn` are ordinary three-way parity citizens.

**Testing (RFC-0015)**: a Vyrn user finally has somewhere to put a test.
`test "name" { .. }` is a top-level declaration — a named block checked exactly
like a `Unit`-returning function body (locals, `print`, spawn rules, ownership,
move-checking all apply, under a synthetic unspellable `test@<index>` name), so
every existing analysis catches bugs inside a test unchanged. Two builtins are
legal **only** inside a test: `assert(cond: Bool)` traps the test with
`assertion failed at line N`; `assertEq(a, b)` (same equatable type both sides)
traps with `assertion failed at line N: <a> != <b>` using the canonical
`toString` rendering. `vyrn test [file] [--name <substring>]` runs the root
file's tests in declaration order under the interpreter, printing `test "name"
... ok` / `... FAILED: <message>` and a `N passed, M failed` summary (exit 1 if
any failed; a file with no tests prints `no tests`). Tests are **stripped** from
`run`/`build`/`emit-ir` — a shipped binary contains no tests, and the string
pool / regex collection never see them (they live in their own `Program.tests`
field, not `functions`) — so a file with BOTH tests and a `main`
(`examples/testing.vyrn`) stays a byte-identical three-way parity citizen. A
file with tests (or exports) needs no `main` (the library-module rule). An
imported module's tests type-check but do not run under `vyrn test <root>`.

**Generator imports (RFC-0021) — the crutch-shedding story.** The compiler was
starting to accrete file-format knowledge — JSON Schema (`import type`), then a
proposed translations flavor. Each was a crutch for the same missing general
mechanism: *user code that runs at compile time and synthesizes a module*. Vyrn
now has it, with unusually strong guarantees because the compiler already
contains a deterministic interpreter, a capability-mediated resolver, and a
content-addressed cache. `gen fn` is a contextual modifier (the `extern`/`test`
precedent) — an ordinary function otherwise (callable, testable, formatted,
distributable, `github:`-pinnable). An **import target may be a `gen fn` call**
whose arguments are compile-time constants: `import { t, TransKey } from
i18n("./locales")`. The loader runs the call in the interpreter and links the
returned `String` as a synthesized module through the ordinary pipeline —
checker, backends, parity, and the LSP stay module-unaware, as always. A `gen
fn` and its transitive callees are held to a **comptime-purity** analysis (the
spawn-isolation sibling): no `extern`/`spawn`/module-state/`writeFile`/
`readLine`/`args`, so *same inputs ⇒ same output* mechanically. Its permitted,
mediated inputs — `readFile`, the new `listDir`, and **`moduleInterface(path)`**
(the `schemaOf`-generalized-to-a-module reflection primitive that makes typed
RPC a library) — route through the resolver, scoped to the call's constant path
arguments and recorded as cache inputs. Deterministic + declared inputs ⇒
content-addressed output: `sha256(generator sources ++ args ++ inputs)` keys
`~/.vyrn/cache/gen`, so cold generation runs once and rebuilds / per-keystroke
LSP re-analysis hit the cache. Guardrails (a step budget and a 4 MB output cap)
keep runaway generators loud, not hung; a generator trap becomes a load
diagnostic at the import site; `vyrn emit-gen <file>` dumps every synthesized
module. The generated code is ordinary Vyrn, so it inherits three-way parity for
free — `examples/gendemo.vyrn` reads a data file + directory at compile time,
emits a validated type and typed constants, and is a byte-identical interp ==
native == wasm citizen. **This is the foundation RFC-0019 (typed RPC) and
RFC-0020 M2 (i18n) are now built on** — with one reflection primitive, whole
layers become libraries and the language sheds format knowledge instead of
accreting it.

**Typed RPC is now that library, not a language feature (RFC-0019).** The
withdrawn `rpc fn` keyword — a compilation-role, client/server-split design that
baked a domain into the language — is gone; in its place is `std/rpc`, three
ordinary `gen fn`s over an ordinary contract module (its exported functions *are*
the procedures, their records the payloads). `rpcServer("./api")` synthesizes
`rpcHandle(req) -> Option<Response>` plugging into the RFC-0016 `handle`
convention: `POST /rpc/<name>` decode-call-encode (a bad payload comes back as
`422 {"issues":[...]}` — the contract's `where` clauses run inside `fromJson`, so
validation is the codec, not hand-written), and `GET /rpc/$schema` a procedure
registry built from `jsonSchema()` calls. `rpcClient("./api")` re-emits the
contract's types verbatim and, per procedure, a same-named stub over one shared
`vyrnRpcCall` extern plus an `export extern fn vyrnRpcDone<Proc>` dispatcher that
unifies the reply into a `Validation<T>` and hands it to your plain
`onGetUser(id, res)` — 200 decodes, 422 carries the server's own issues, a
transport failure is one `rpc.transport` `Issue`. `rpcInProcess("./api")` is the
deterministic test/SSR double (same real logic, no wire), keeping the contract in
the parity corpus (`examples/rpc.vyrn`, interp == native == wasm). The host side
is small and outside the compiler: `web/vyrn-rpc.js` (the `fetch` transport that
routes completions back to the dispatchers by name — it made the request, so it
knows the owner), `web/vyrn-query.js` (a ~110-line zero-dep query cache: dedupe,
stale-while-cached, invalidate), and **`vyrn dev`**, which reads `vyrn.json`'s
`server`/`client`, builds the client to wasm (a *plain* wasm build — no roles),
and serves the server root's `handle` with static assets in front. The whole
protocol roadmap (Connect/gRPC, `.proto`/SDL emitters, JSON-RPC/MCP, SSE) is now
*more generators and more host adapters*, and none of it will ever touch the
compiler. See `examples/fullstack/` (`vyrn dev`, then a typed round trip, a
validated 422, and a cache demo in the page).

---

## Shipped

### Language core
- `Int64` / `Bool`, `let`/`mut`, arithmetic, `if`/`else`, `while`, `for`-in over
  arrays, functions, `print`.
- **Input I/O (RFC-0014)** — `args()`, `readLine()`, `readFile`/`writeFile`,
  `readFileBytes`/`bytes`/`stringFromBytes`, with canonical (never-OS) error
  strings; see the narrative above.
- Immutable string literals (`==`, `!=`, record fields), statically allocated.
  Concatenate two Strings with `a + b`; a String's byte length is the `s.length`
  field. (The old `concat(a, b)` / `len(s)` free-functions were removed — a
  user-written call to either reports a migration hint.)
- **String encoding.** A `String` is an immutable sequence of **UTF-8 bytes**.
  `s.length` counts **bytes**, not code points — equal to the code-point count
  for ASCII text, larger for text with multi-byte characters (e.g. `"é"` has
  `length` 2, `"日"` has `length` 3). The regex engine matches
  byte-wise for the same reason, and `.` matches one byte. Source files are read
  as UTF-8. One documented divergence follows from this: JSON-Schema
  `minLength`/`maxLength` bounds derived from a `String where` predicate carry
  the byte count through unchanged, so for non-ASCII text they bound bytes where
  a JSON validator counts UTF-16 code units — a deliberate, noted trade-off (a
  code-point index would cost an O(n) scan on every `.length`).

### Types
- **Validated types** — `type Age = Int64 where value >= 18`, or inline on a
  record field, Zod/ArkType style: `type User = { age: Int64 where value >= 18 }`
  (desugars to a synthetic `User.age` validated type; the trailing record-level
  `where` stays the cross-field invariant, like Zod's object `.refine`) — with
  **automatic, exhaustive validation**: every value boundary (`let` annotation, assignment,
  call argument, return, record field, array element) checks a plain value
  flowing into a validated type by itself — no explicit constructor call
  needed. Provably-false constants are compile errors; provably-true ones cost
  nothing; dynamic values trap at runtime (`error: validation failed for
  \`T\``, both backends byte-identical). Field mutation on validated data is
  rejected — rebuild the value, which re-validates. Explicit `Age(n)` and
  fallible `Age?(n) -> Option<Age>` remain.
- **Nominal types** over `Int64`/`Bool`/`String` (a nominal type *without* a
  predicate still requires explicit construction — it is documentation).
- Every numeric type names its size: `Int8`–`Int64`, `UInt8`–`UInt64`,
  `Float32`/`Float64`. There is no unsized `Int`/`Float`.
- **Structural records** with width subtyping and mutable fields (`c.x = ...`).
- **Transformers** — `Omit` / `Pick` / `Merge` / `Partial` / `Readonly`, plus
  intersection `A & B`. Pure type-level, erased before codegen.
- **Enums / sum types** with multi-payload variants and exhaustive `match`.
- **Generics** — functions, records, enums — inferred per use and monomorphized,
  with built-in bounds `Eq` / `Ord` / `Num`.

### Errors & control
- `Option<T>`, `Result<T, E>`, `match`, and `?` propagation (no null). `Option` and
  `Result` payloads may be any type, so `Option<Ref<Node>>` gives a nil terminator.
- **Checked conversions** — `x.toString() -> String` (a method on every number,
  `Bool`, and `String`; it replaced the `str(x)` builtin) and
  `parse(String) -> Option<Int64>` (the fallible inverse is an explicit `None`,
  never a silent 0 or a crash).

### Data structures
- **Arrays** — growable `Array<T>` (a `Vec`: `[]` / `a.push(x)` / `a[i]` read /
  `a[i] = v` in-place store / `a.pop()` → `Option<T>` / `a.swapRemove(i)` → `T`
  (O(1) unordered remove) / `a.length`, a doubling heap buffer, bounds-checked;
  non-escaping arrays reclaimed automatically, `drop a;` for handoff) and
  fixed-size **`Array<T, N>`** (a const generic: stack `[N x T]`, no heap,
  array-literal `[a, b, c]` syntax; element store allowed, `pop`/`swapRemove`
  rejected — it cannot shrink). The element store and shrinking ops are RFC-0011;
  a validated element type auto-validates on store. An
  array literal written where an `Array<T>` is expected (a `let` annotation, a
  call argument, a return) is that growable heap array directly — contextual,
  replacing the old `list([..])` builtin. Both iterate with `for x in arr { .. }`.
  The surface is subject-first — no `verb(object, …)` builtins.
- **Recursive heap structures** — a singly-linked list and a binary tree. `Ref<T>`
  makes the node type finite, `Option<Ref>` terminates it, and a recursive `release`
  walk reclaims the whole structure (proven: 100,000 nodes cycled through a
  65536-cell slab). Both build/traverse/reclaim end to end, to a flat memory
  baseline — the `Option` payload is two words wide, so a `Ref` is stored inline
  with no heap box.

#### ECS notes — what a Structure-of-Arrays ECS can do today

`examples/ecs.vyrn` is a working SoA entity-component-system toy (parallel
`Array<Int64>` component stores, a movement system, spawn/despawn churn, a
deterministic checksum) verified interp == native == wasm. Writing it mapped out
exactly where the language helps and where it doesn't:

**Efficient today**
- **Contiguous scalar SoA stores.** A growable `Array<T>` lowers to
  `{ ptr, len, cap }` over a single realloc'd buffer, `a[i]` is a
  `getelementptr` + `load` at an element stride, and `a[i] = v` the matching
  `store` — genuinely cache-friendly. A system that streams over one component
  array per tick, reading and writing in place, is doing tight, linear memory
  access. Both halves of an ECS (iterate AND mutate) are good.
- **In-place update + O(1) despawn (RFC-0011).** The movement system integrates
  each survivor with an element store (`xs[i] = nx`) and despawns departing
  entities with `swapRemove` across all four SoA arrays in lockstep — no
  per-tick rebuild, no fresh allocation. This is the alive-compaction a real
  ECS does, expressed directly.
- **Cheap append-spawn.** `a.push(x)` is amortised O(1) with geometric growth.
- **Deterministic, reclaimed.** Arrays auto-drop when they don't escape; the whole
  world tears down without a GC, identically on every backend.

**Remaining gaps**
- **`Array<Record>` (AoS) is copy-only.** Indexing an `Array<Record>` returns a
  *copy*, and `a[i].field = v` write-through is still out of scope (RFC-0011
  "Out of scope"), so you cannot mutate a component struct in place through the
  array. This is why the example is SoA, not one `Array<Entity>`: the SoA scalar
  element stores *are* the write path. An AoS ECS needs place-expression field
  assignment through an index on top of what exists.
- **No order-preserving `insert`/`remove`/`truncate`/`clear`.** The shrinking
  surface is `pop` (last) and `swapRemove` (O(1), unordered); an ordered removal
  or a bulk shrink still means an explicit shift loop. Additive, future work.
- **No generational entity handles yet at the array level.** `Ref<T>` gives
  generation-checked cells, but there's no built-in dense/sparse-set or
  generational-index allocator; entity ids here are plain array positions, and
  `swapRemove` reorders survivors (fine for this toy, not for stable cross-frame
  handles).

**Verdict.** Both the storage model and the mutation model are now ready for an
efficient in-place SoA ECS: contiguous component arrays, per-element writes, and
O(1) despawn, all reallocation-free per tick. The open items (AoS field
write-through, ordered `insert`/`remove`, generational handles) are additive and
none blocks the core loop.

### Memory (RFC-0004)
- `consume` capability + move checking (using a consumed value is a compile error).
- `modify` capability — a parameter changed in place, visible to the caller
  (by-reference / call-by-value-result; the argument must be a `mut` variable).

### Concurrency (RFC-0004 §Q4)
- **Structured fork-join** — `spawn f(args) -> Task<T>` / `t.join()`. The compiler
  *proves* a spawned function is isolated (no I/O, no shared mutable state,
  transitively), so tasks are data-race-free and the result is schedule-independent
  — which is what keeps interpreter == native. `share` is the concurrent-read
  capability. (Execution is eager/sequential today; a parallel scheduler is a
  drop-in backend optimisation the model already guarantees is safe.)
- **The heap** — dynamic strings (`a + b` concatenation, `s.length`), malloc-backed.
- **Deterministic reclamation, Path A (no GC):**
  - `region { .. }` arenas free a whole *group* of allocations at block exit.
  - **ownership auto-drop** frees an *individual* heap value proven not to escape
    its block — a string, a reference cell, or a growable array (including the
    `a = push(a, x)` self-update), all via one escape analysis.
  - **ownership transfer** lets a function return a fresh value the *caller* then
    owns and frees (inferred by a call-graph fixpoint).
  - Measured flat (~3 MB) where the same million-allocation loop leaks 1.2 GB.
    Every allocation is owned by exactly one mechanism, so nothing is freed twice;
    what can't be proven single-owned leaks (always safe).
- **Generational references (Path B)** — a `Ref<T>` is a freely-copyable handle to
  a mutable heap cell holding any `T` (`cell` / `get` / `set` / `release`; the
  payload is boxed, so the handle is fixed-size). Each access is generation-checked,
  so a use-after-release traps instead of dangling — even after the slot is reused.
  The answer to the *aliasing* case. A record may hold a `Ref` to its own type
  without becoming infinite.
- **Inferred `release`** — the *same* ownership analysis that frees non-escaping
  strings auto-releases non-escaping cells, so Path A and Path B are one
  mechanism. Aggressive reclamation is safe here precisely because a missed alias
  traps cleanly instead of dangling.

### Backend
- Text LLVM-IR backend; `vyrn build prog.vyrn` emits IR and links a native exe
  with `clang`. (The Inkwell in-memory backend also works now — builds against an
  LLVM 22 dev SDK and links a `fib` exe whose exit code matches the interpreter —
  but stays excluded from the default workspace and covers only the v0.1 subset;
  the text-IR path remains the full reference backend.)

### Tooling
- **Structured diagnostics as a core API** — `vyrn_frontend::diagnostics(source)`
  returns every problem as a `Diagnostic { line, col, end_col, severity, stage,
  message }` with a precise position. Both `vyrn check` (prints
  `file:line:col: message`) and the LSP consume the same API; no duplication.
  Accumulation is bounded: lexer/parser stop at the first error, but once a file
  parses, every type/ownership error across all functions and types is reported.
- **Symbol query as a core API** — `vyrn_frontend::analyze(source)` runs the
  pipeline (lex→parse→check→movecheck) once and returns an `Analysis {
  diagnostics, symbols, tokens }`: the diagnostics, a `Symbol` per top-level
  function/type/variant/method with a precise name column (reused from the
  lexer's `Token.col` — the AST carries line only), and the identifier tokens.
  `resolve(analysis, line, col)` maps a cursor to its declaration; `
  completions(analysis)` lists top-level symbols. Non-invasive: no AST/parser
  span threading. `diagnostics()` delegates to `analyze()`, so one pipeline.
- **`vyrn fmt` — the canonical formatter** (RFC-0017). One style, no options.
  `vyrn_frontend::fmt(source) -> Result<String, Diagnostic>` prints a
  comment-preserving token stream (`lexer::lex_with_trivia`): 4-space brace-depth
  indent, the normative spacing table, semicolons dropped, 2+ blank lines
  collapsed to one, one trailing newline. It **never joins or splits lines** (no
  width reflow) and never re-synthesizes a literal — it only chooses the
  whitespace between raw token texts, so the **safety invariant** is cheap:
  `lex(fmt(src))` equals `lex(src)` modulo removed `Semi`, checked in `fmt`
  itself (a mismatch returns an error and leaves the file untouched) and over the
  whole corpus. `vyrn fmt [files…] [--check]` formats in place (or bare
  `vyrn fmt` = project main + local imports via the module graph); `--check` is
  the CI gate (lists drift, exits 1, writes nothing). Requires only *lexable*
  input, so a half-typed buffer with a parse error still formats — which is what
  makes format-on-save safe.
- **`vyrn-lsp`** — a synchronous `lsp-server` LSP server (no async runtime) and a
  pure adapter: it calls `analyze_linked` once on open/change, caches the
  `Analysis`, and serves `textDocument/publishDiagnostics`, `/hover`,
  `/definition`, `/completion`, and `/formatting` (whole-document, running the
  same `fmt`) from it (a request never re-parses). Excluded
  from the default workspace (pulls `lsp-server`/`lsp-types`); built with
  `cargo build --manifest-path compiler/vyrn-lsp/Cargo.toml`. The only compiler
  calls are `vyrn_frontend::analyze_linked` + the query layer, so the editor and
  CLI report identical errors. **Multi-file aware** (RFC-0010): the server
  resolves a document's `import`s through the module loader — local files from
  disk, `std/` via the same discovery as `vyrn`, manifest aliases from
  `vyrn.json`, and *pinned* remote modules read-only from `vyrn_vendor/` or the
  user cache (the editor never fetches; unpinned remotes get a "run `vyrn
  check` once" diagnostic). Errors inside an imported file surface in the open
  document as `in <file>: …` at the top. Hover/go-to-definition/completion cover
  top-level functions, types, and variants of the open document, plus local
  bindings (params, `let`s, `for`-in vars) — a local shadows a same-named
  top-level symbol; local hover shows the declared type for params and annotated
  lets. **Cross-file hover/go-to-definition**: names the root imports are
  indexed from the linked program with their source file — hover shows the
  imported signature, F12 jumps into the imported module (declaration line;
  columns are whole-line, the foreign token stream isn't indexed). An imported
  enum brings its variants, an imported protocol its methods. Remote modules
  (`github:...`) get hover but no jump (no local file). Imported names appear
  in completions.
- **VS Code extension** (`editor/vscode/`) — plain-JavaScript (no compile step)
  extension that spawns `vyrn-lsp` and ships a TextMate grammar for colors. `F5`
  from the repo root runs it against `examples/`: colored, squiggled, with hover
  / F12 go-to-definition / completion. A **"▶ Run" CodeLens** sits over `fn main`
  (runs `vyrn run`); for tests (RFC-0015) a **"▶ Run test"** CodeLens sits over
  each `test "name"` block (`vyrn test --name "name"`) and a **"▶ Run all
  tests"** over the first — all reusing the shared `vyrn` terminal and the
  repo-root vyrn discovery. `test` is a contextual keyword in the grammar (only
  before a string), there is a `test` snippet, and test blocks appear in the
  outline / document-symbol list.

---

## The memory model — decided (RFC-0004 §5)

The founding notes said to settle the memory model by *prototyping and measuring*,
not by argument. Both lowerings were built behind the same capability surface and
measured — and the decision is now made: **a hybrid that defaults to ownership.**
Ownership + regions handle single-owner values with zero per-access overhead and no
annotations; generational references handle the *aliasing* case, where the check
proved essentially free in a hot loop (within noise in steady state; ~10 % cold, on
a loop doing nothing but access). You reach for `Ref<T>` exactly when you need
shared mutable state — which is also where the type makes that choice legible.

Both prototypes:

- **Path A — ownership + regions.** ✅ Reclaims owned `String`s — regions,
  ownership auto-drop, and ownership transfer. Measured flat vs. a 1.2 GB leak.
- **Path B — generational references.** ✅ Prototyped. A freely-copyable `Ref<T>`
  (over any element type) carries a generation tag; the cell carries a counter;
  each access validates the tag, so a stale alias fails a cheap check instead of
  dangling. This is what makes the *aliasing* case safe.

**The two paths share one analysis.** `release` is inferred: the same escape
analysis that frees non-escaping strings auto-releases non-escaping cells. So the
capability surface stays uniform — you write neither `free` nor `release` in
ordinary code — and reclaiming aggressively is safe on Path B because a missed
alias traps cleanly rather than dangling.

The decision is recorded in RFC-0004 §5. What's left is *surface refinement*, not a
change of mechanism: inferred/invisible regions, `modify`/`share` reference
inference, and concurrency.

---

## Next / gated

Each needs dynamic allocation or references; the heap unblocks them, but most wait
on the reclamation decision above.

- **Parallel execution of tasks** — the concurrency *model* and its safety ship
  today (eager/sequential scheduler); running tasks on real threads is a portable
  threading runtime — runtime work, not language design, and it changes no answers.
- **`share`-by-reference** — pass large shared data without copying (an
  optimisation; observably identical to today's by-value `share`).
- **More conversions** — `parse` for other types; formatting helpers.

### Editor (deferred from the LSP work)
- **Parser error recovery** → multiple *parse* errors per pass, at BOTH
  granularities now. **Top-level recovery**: `parse_accum` records a bad
  `fn`/`type`/`protocol`/`impl`/`logging` declaration, synchronizes to the next
  top-level starter (brace-depth aware), and continues — so one bad declaration
  no longer hides a later one. **Within-declaration (statement-level) recovery —
  shipped**: a statement that fails to parse inside a body is recorded and
  dropped, the parser synchronizes to the next statement boundary (a fresh line
  at the block's brace depth, a `;` at that depth, or the block's `}`), and keeps
  parsing the same body — including inside nested `if`/`while`/`for`/`region`
  blocks — so several bad statements each report. The payoff: a body parse error
  now leaves a usable partial AST, so `symbols::analyze` keeps indexing symbols/
  tokens/locals (hover, outline, completion stay live while you type); the
  checker/movecheck are skipped while any parse error exists, so no cascade. This
  was the editor track's last deferred item.
- **User `protocol`/`impl` method-call resolution — shipped.** The checker
  resolves `x.foo()` through its protocol registries (RFC-0002 §5, static
  dispatch), and the LSP surfaces it: `.foo` member completion offers the
  methods of every `impl P for T` matching a concrete receiver's type, and a
  bounded generic receiver (`fn f<T: Show>(x: T)` → `x.`) offers each bound
  protocol's method signatures. Hover on the method name at a call site
  resolves to the `impl` method declaration (a user symbol, so F12 works).
  Built-in method calls (`arr.push`, `log.info`, `Ref.get`, …) resolve the
  same way, and record-field member completion works too: `u.` on a record
  receiver offers the declaration's fields, with refined fields rendered as
  written (`age: Int64 where value >= 18`).

---

## RFC status

| RFC | Title | Status | Notes |
|-----|-------|--------|-------|
| 0001 | Vision | accepted | Principles & non-goals. |
| 0002 | Type system | mostly shipped | Records, enums, generics, transformers. |
| 0003 | Validated types | shipped | The signature feature, end to end. |
| 0004 | Capabilities & memory | decided | `consume` + both lowerings shipped; model settled as a hybrid defaulting to ownership (§5.2), measured. Surface refinements remain. |
| 0005 | Error handling | shipped | `Option` / `Result` / `match` / `?`. |
| 0006 | Diagnostics | draft | Message style used by the checker. |
