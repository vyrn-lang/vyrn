# RFC-0103 — A Target Is a Capability Set

- **Status:** **Proposed.** No implementation. Milestones below; a milestone
  that fails its gate says so in this file.
- **Depends on:** RFC-0072 (audience — kept, and demoted to what it is),
  RFC-0071 (module contracts and roles, untouched), RFC-0014 (input I/O — the
  builtins the floor watches), RFC-0043 (time/random), RFC-0044 (storage),
  RFC-0012 (`extern` — the wasm-only direction), RFC-0084 (sse/ws).
- **Evidence (user):** "this doesn't make sense, user still can include secrets
  in client or whatever", "in gradle it is checked too and you should configure
  it too in Vyrn", "it looks like something almost gradle modules just more
  fuzzy? How it actually should be? To be clear, easily usable, reliable".

---

## The problem

RFC-0072 shipped audience: path segments declared in `vyrn.json`, an
edge check, `vyrn why`. It works, and roles and derived RPC stand on it. But
its own document overclaims what the mechanism guarantees. It says a rejected
import prevents "a leaked secret at worst". It does not. Two facts, both
surfaced by the user, bound what any such system can do:

1. **The compiler does not know what a secret is.** A string literal pasted
   into a client module is invisible to every checker ever built. Audience
   checks import edges; it cannot check intent.
2. **A label is configuration.** `server/` means server-only because
   `vyrn.json` says so, and whoever edits `vyrn.json` can say otherwise. A
   check whose premise is user-editable is a fence, not a floor. Gradle has
   exactly this: declared project boundaries, checked conformance, no
   knowledge of what an edge means. Audience as shipped is Gradle with better
   diagnostics.

A fence is worth having. But the honest design names a second layer under it —
one the user cannot relabel, because it is not a label. That layer is the
target.

## The design: a floor and a fence

**The floor (new).** An artifact is a real thing: an entry point and a target.
A target is a capability set — a fact about where the code runs, not a policy
about who should read it. A browser page has no filesystem. That is physics,
not configuration, and `web/wasi-min.js` already lives it: `path_open` answers
`NOENT`, `fd_read` on stdin answers EOF, `args_get` answers an empty list,
while the clock and the CSPRNG work. The floor makes that answer arrive at
compile time instead of runtime: every module reachable from an artifact's
entry must need only capabilities the artifact's target has.

**The fence (shipped, RFC-0072).** The audience map stays exactly as it is:
declared segments, edge check, nearest-wins, `vyrn why`. What changes is the
claim made for it. It prevents the *accidental* class — a client file
importing a helper that imports a config module, five hops the author never
saw. It does not prevent the deliberate class, and its documentation stops
saying it does.

The two layers fail differently, and that is the point. The fence fails when
the manifest is wrong. The floor cannot be wrong that way: nobody can grant a
browser a filesystem by editing JSON.

### §1 Artifacts

```json
{
  "artifacts": {
    "api": { "entry": "server/main.vyrn", "target": "native" },
    "app": { "entry": "client/boot.vyrn", "target": "browser" }
  }
}
```

- `target` is one of `native`, `wasi`, `browser`. It is a CAPABILITY
  declaration, not a build-target selection: there are two build targets,
  `native` and `wasm`, and `wasi` and `browser` pick the same built artifact —
  the identical bytes, under hosts that answer the WASI imports differently
  (M0's [finding 1](#what-the-census-contradicts-in-this-rfcs-own-design)).
  `browser` is today spelled "wasm plus wasi-min.js" and gets a name.
- The existing manifest keys are sugar and stay: `main` and `server` are
  native artifacts, `client` is a browser artifact. A project that writes only
  those keys is already using artifacts and never sees the new spelling.
- Opt-in stays absolute: a manifest with no entry-point keys and no
  `artifacts` map gets no floor check, exactly as a manifest with no
  `audience` key gets no fence.

### §2 Capabilities

A capability names a way out of the program. M0 censused the vocabulary by
running one program per capability on each target; the table below is that
result, and every cell in it was produced by an execution recorded under
[M0 — as landed](#m0--as-landed).

| capability | carried by (today) | native | wasi | browser |
|---|---|---|---|---|
| `fs` | `readFile`, `readFileBytes`, `writeFile`, `renameFile`, `std/storage`'s `writeAtomic` | yes | yes | **no** — the canonical `Err` payloads (`` cannot read `p` ``, `` cannot write `p` ``), exit 0 |
| `fs` durability | `fsyncFile` | yes | **no — the build is refused**: `` direct backend: no lowering for the call `fsyncFile` `` | **no** — same refusal, same artifact |
| `fs` listing | `listDir` | **no — the build is refused** (interpreter / generation only) | **no** — same refusal | **no** — same refusal |
| `fs` through a declaration | `logging { sink: file("…") }` | yes | yes | **no, and silently**: the line is dropped, nothing is printed, exit 0 |
| `stdin` | `readLine` | yes | yes | **no** — EOF, so `None` |
| `args` | `args` | yes | yes | **no** — empty `Array<String>` |
| `stdout` / `stderr` | `print`, `logger(..).info` and the rest of RFC-0008, every trap and `panic` frame | yes | yes | yes — `fd_write` on fd 1 / 2 is captured by the page |
| clock | `std/time`'s `now`, `monotonic` | yes | yes | yes — `clock_time_get` off the browser clock |
| entropy | `std/random`'s `randomSeed` | yes | yes | yes — `random_get` off the browser CSPRNG |
| `extern` import | `extern fn` (RFC-0012), and the `vyrnRpcCall` / `vyrnConnectCall` the `std/rpc` and `std/connect` generators emit | **no** — the build succeeds and the *call* traps: `` error: extern `jsAdd` is not available on this target `` | **no** under this repo's wasi runner, and worse than a trap: `` unknown import: `vyrn::jsAdd` has not been defined `` — the module never instantiates, so a program that never reaches the call still cannot start. A wasi host that supplies the `vyrn` namespace would answer yes | yes — the page **is** the namespace; a missing hook is refused by name, also before instantiation |
| `extern` export | `export extern fn` (RFC-0012 M2) | yes — it has a body, so it is an ordinary function | yes | yes, and additionally callable from the page after `_start` |
| serving | `serveStream` (RFC-0074 M3a) | **no** — runtime trap: `` serveStream: a compiled build has no accept loop — a live route needs `vyrn serve` `` | **no** — the same trap | **no** — the same trap |
| threads | `spawn` / `t.join()` | yes — a real operating-system thread | yes, with no thread: the direct backend runs the callee eagerly at the spawn point | yes, the same eager schedule |

Two things the census found are **not** capabilities and are recorded here so a
later reader does not go looking for them in the table:

- **The generation-time reads.** `moduleInterface`, `contractOf`, `lex`, and
  `readFile` / `readFileBytes` / `listDir` *inside a `gen fn`* reach the
  compiler's filesystem, never the artifact's. Outside a generation the checker
  refuses them identically on every target
  (`` `moduleInterface` is only available during generation ``), before any
  backend. There is no per-target cell to fill.
- **`vyrn serve` and `vyrn run`.** The interpreter answers yes to `listDir` and
  to `serveStream`, which no compiling target does. That is a fourth column, not
  a fourth target — nothing is *built* for it.

A module's requirement is the union of the capabilities its calls carry —
presence in the source, not reachability of the branch, because the check must
not depend on control flow. An artifact's requirement is the union over its
import closure. The check is one subset test per artifact:

```
requirement(closure(entry)) ⊆ capabilities(target)
```

`extern` is the inverse direction — a capability native lacks — and today it
is a runtime trap. Whether the floor turns that trap into a compile error for
native artifacts is an M0 census question, answered by counting how many
existing programs rely on compiling (not running) extern calls natively. The
count is **four examples and three tests**; see
[the extern question](#the-extern-question-answered-with-counts).

### §3 The diagnostic

The error shows the chain, because the chain is the whole usability story —
the author never saw hop three:

```
error: artifact `app` (browser) cannot include `server/db.vyrn`: it reads files
  client/boot.vyrn → shared/format.vyrn → server/db.vyrn
   = `readFile` needs `fs`; target `browser` has no filesystem
   = call it through the wire instead: connect("./server/db.vyrn")
```

The remedy names the module that was actually reached, not a fixed path.
RFC-0072's `remedy()` says `client("./server/api")` for every rejection; that
string is replaced by the concrete crossing for the concrete module, in both
the floor's diagnostic and the fence's.

### §4 What this does not do

Stated in the RFC because the absence of these claims is a design decision:

- It does not classify data. A secret written as a literal in a
  browser-artifact module compiles and ships. No compiler can prevent this,
  and this one does not pretend to.
- It does not replace the fence. A server module that holds a secret in a
  plain constant uses no capability; only the audience fence catches its
  import, and only if the manifest declares it. That is Gradle's guarantee,
  and it is the most any declared boundary gives.
- It does not touch parity. The floor is a frontend check that runs before
  any backend; interp, native, and wasm see the same accepted programs.

## Milestones

**M0 — census.** Every builtin and std module that reaches outside the
program, one row each: the capability it carries, its behavior per target
today (verified by running one program per capability in-page and under
wasmtime, not by reading comments). The extern question answered with counts.
Gate: the table in this file has no "unknown" cell.

### M0 — as landed

**Gate: met.** §2's table has no "unknown" cell, and no cell was filled by
reading a comment. Every one is an execution or a refusal this milestone
produced.

#### Method

One program per capability under `rfcs/census-0103/`, each run four ways from
that directory:

| leg | how |
|---|---|
| interpreter | `vyrn run <p>.vyrn` |
| native | `vyrn build <p>.vyrn -o <p>.exe`, then the exe |
| wasi | `vyrn build <p>.vyrn --target wasm -o <p>.wasm`, then `wasmtime --dir . <p>.wasm` |
| browser | the **same** `<p>.wasm`, fetched and run in-page through `web/wasi-min.js` by `rfcs/census-0103/census.html` |

The browser leg is a real run, not a shim-read: `census.html` loads every module
in turn, prints stdout, stderr and the exit code for each, and was read out of a
live page served from the repo root. Its `cap-extern` row runs twice — once with
a `jsAdd` hook supplied and once without — because the two answers are different
facts about the same capability.

The `.wasm` files are gitignored. They are not build products worth keeping:
`--target wasm` is deterministic, and the page's own header says how to rebuild
them.

#### The programs

| program | capability |
|---|---|
| `cap-fs.vyrn` | `readFile`, `readFileBytes`, `writeFile`, `renameFile`, `writeAtomic` |
| `cap-fsync.vyrn` | `fsyncFile`, alone, because its answer differs from the rest of `fs` |
| `cap-listdir.vyrn` | `listDir` |
| `cap-logfile.vyrn` | `logging { sink: file(..) }` |
| `cap-stdin.vyrn` | `readLine` |
| `cap-args.vyrn` | `args` |
| `cap-stdio.vyrn` | `print` and `logger(..).info` |
| `cap-clock.vyrn` | `std/time`'s `now` and `monotonic` |
| `cap-entropy.vyrn` | `std/random`'s `randomSeed` |
| `cap-extern.vyrn` | an `extern fn` import, called |
| `cap-externexport.vyrn` | an `export extern fn` |
| `cap-serve.vyrn` | `serveStream` |
| `cap-spawn.vyrn` | `spawn` / `join` |
| `cap-genread.vyrn` | `moduleInterface` outside a generation |

#### The extern question, answered with counts

The question was how many existing programs rely on **compiling** — not running
— an `extern` import for native, because that is the cost of turning the trap
into a compile error.

**Four programs in `examples/`.** Each was built with `vyrn build` (no
`--target`) for this census, and all four succeed today:

| program | where its externs come from |
|---|---|
| `examples/externdemo.vyrn` | written by hand: `jsLog`, `jsNow`, `jsAdd` |
| `examples/fullstack/client/boot.vyrn` | the `std/rpc` generator's `vyrnRpcCall` |
| `examples/shelf/client/boot.vyrn` | the `std/rpc` and `std/connect` generators |
| `examples/bin/client/boot.vyrn` | the `std/connect` generator |

Only **one** of the four is ever built natively by the tree itself. The three
clients are `check`ed and built to wasm; nothing asks for a native binary of
them. That matters for M2: three of the four costs are notional.

It also means three of the four never had an `extern` line in their source. The
generators put it there, so a compile error would arrive at a program whose
author wrote `connect(..)`, and the diagnostic has to name the generator's line
as the *cause* and the author's call as the *site*.

**Three tests.** These are what a compile error would break:

| test | what it asserts |
|---|---|
| `vyrn-cli` `parity::wasm_only_examples_trap_identically` | the native build of `externdemo.vyrn` **must succeed** ("extern trap stubs link"), and the exe must then trap with wording byte-identical to the interpreter's |
| `vyrn-codegen` `tests::extern_fn_emits_wasm_import_declaration` | the native emitter writes `declare i64 @__vyrn_extern_jsAdd(i64, i64)` and a call at the use site |
| `vyrn-codegen` `toolchain::tests::the_extern_stub_is_built_from_the_symbol_and_the_trap_it_quotes` | the C trap stub is assembled from the symbol scheme and the interpreter's wording |

Everything else that mentions `extern fn` in the suites either checks, emits
generated source as text, or builds only to wasm.

So the answer is: **the floor may turn the native `extern` trap into a compile
error for four example builds and three tests, one example build and all three
tests being real work.** The decision is M2's; the count is here.

#### What the census contradicts in this RFC's own design

Five findings, each recorded because it changes something written above.

1. **`wasi` and `browser` are not two build targets.** §1 says "the first two
   exist today as build targets". They do not. There are two: `native` and
   `wasm`. The browser leg above ran the *identical bytes* wasmtime ran — the
   only difference is which host answers the WASI imports. So `target` in the
   manifest cannot be a build-target selection with three values; it is a
   capability declaration, two of whose values pick the same artifact. That is
   fine for a frontend check, and §1's wording is what is wrong.

2. **`fs` is not one capability.** The provisional table had one `fs` row,
   yes/yes/no. The census splits it into four rows with four different answers,
   and two of them are already **compile errors on a compiling backend**:
   `fsyncFile` has no lowering in the direct backend that `--target wasm` now
   uses unconditionally, and `listDir` has none in either. The language already
   refuses a capability at compile time in two places, which is precedent M2 can
   point at rather than novelty M2 has to argue for.

3. **`extern` under wasi fails at instantiation, not at the call.** The
   provisional cell said "host-dependent", which is true and too weak. Under
   wasmtime the module does not load at all. That is exactly the shape §2's
   check has — presence in the source, not reachability of the branch — so the
   runtime this repo ships already behaves the way the floor proposes to. It is
   the strongest argument in the RFC and it was not in it.

4. **One `fs` reach fails silently.** `readFile` and `writeFile` degrade
   *loudly* in a page: a canonical `Err` the program can match on. The
   `logging { sink: file(..) }` declaration degrades *silently* — the line
   vanishes, nothing is printed, the exit code is 0. §3's diagnostic must
   therefore be able to name a `logging` declaration as the thing that needs a
   capability, not only a call. It is the one capability in the table carried by
   a declaration rather than by a call, and "the union of the capabilities its
   calls carry" does not reach it as written.

5. **Two capabilities no compiled target has.** `listDir` and `serveStream` are
   available in the interpreter and in nothing else. A capability set indexed by
   `native | wasi | browser` cannot say that: it is not that the browser lacks
   them, it is that every artifact lacks them. Either the vocabulary needs a
   "compiled targets have no such capability" answer, or these two stay outside
   the floor and keep the refusals they already have.

**M1 — artifacts in the manifest.** The `artifacts` map parsed; `main` /
`server` / `client` mapped onto it as sugar. Gate: every existing example and
test builds unchanged; `examples/shelf` and `examples/bin` gain explicit
artifact maps and behave identically.

### M1 — as landed

**Gate: met.** Every existing example and test builds unchanged, and `shelf`
and `bin` declare their artifacts explicitly with byte-identical `vyrn check`
output before and after.

**What landed.** `vyrn_frontend::artifacts` reads the map off the PARSED
manifest and hands back `Option<Vec<Artifact>>` — `{ name, entry, target }`,
the entry joined onto the same canonical project base an audience entry point
is joined onto, so `client` names one file to both rules. `Manifest.artifacts`
carries it. Nothing consumes it yet: M1 parses and exposes, and the floor is
M2's.

`None` is the whole compatibility story, as it is for `audience`: a manifest
with no `artifacts` map and no entry-point key declares nothing. Opt-in stays
absolute.

**The sugar, as implemented.** `main` and `server` each yield a native
artifact, `client` a browser one, each named by its own key. Sugar and an
explicit map coexist, which is what lets a project write its artifacts out in
full without deleting the keys `vyrn dev` and `vyrn serve` read:

- An explicit artifact whose NAME matches a sugar key and agrees with it
  (same entry, same target) is that one artifact, not two — accepted.
- One that disagrees is refused, quoting both readings:
  `` artifact `client` in …/vyrn.json disagrees with the `client` key: `…/client/other.vyrn` (browser) against `…/client/boot.vyrn` (browser) ``.
- A name written twice inside `artifacts` is refused whether or not the two
  agree (`` artifact `app` is declared twice in …/vyrn.json ``): one name, one
  declaration, and which of two JSON keys wins is otherwise whichever the
  reader happened to keep.

  The brief asked for that refusal and the milestone found it was already
  there: `parse_json` refuses a repeated key outright
  (`` `app` is defined twice at offset 129 ``), so no manifest on disk reaches
  this check. It is kept for the documents the JSON reader did not build, and
  its test asserts the parser's refusal first — where the rule actually lives.

Structural validation only — `entry` a string, `target` one of the three, the
unknown one naming all three back. Whether the entry FILE exists is not asked:
that is the closure walk, and it is M2's.

A manifest that contradicts itself this way travels the channel an unparseable
manifest already travels (`find` returns `Err`, the CLI prints it and stops),
for the reason RFC-0010's reader states: a rule that cannot be read is not the
empty rule.

**The gate, measured.**

| evidence | result |
|---|---|
| `cargo test --release` (workspace) | 1712 passed, 0 failed, 69 ignored |
| `cargo test` (`vyrn-lsp`, the excluded crate that reads manifests) | 83 passed, 0 failed, 4 ignored |
| `cargo fmt --check` (workspace + `vyrn-lsp`, `vyrn-genwasm`, `vyrn-play`) | clean |
| `vyrn check` on `shelf` and `bin`, both entries each, main's binary against the old manifests vs this branch's against the new ones | `ok` / exit 0, all four, diff empty |

The examples gained the map their keys already implied:

```json
"artifacts": {
  "server": { "entry": "server.vyrn", "target": "native" },
  "client": { "entry": "client/boot.vyrn", "target": "browser" }
}
```

which is the coexistence rule's own test: both projects now say the same thing
twice, and are required to be read as saying it once.

One existing test changed, and it is named here rather than left in the diff:
`vyrn-cli` `von::from_json_prints_a_manifest_as_von` converts
`examples/shelf/vyrn.json` to VON and compares the whole document, so the four
new lines are in its expectation. No behavior changed — the conversion is
generic over the JSON it is given — but the gate says "unchanged", and this
file did change.

**M2 — the floor check.** Requirement inference per module, closure per
artifact, the subset test, the chain diagnostic. Gate: a new example that
deliberately leaks (`client → shared → server file-reader`) is rejected with
the full chain; all existing examples stay green; parity suite untouched.

**M3 — the remedy and the reframe.** `remedy()` replaced by the concrete
crossing; RFC-0072's document amended to the fence claim (accidental class,
not secrets); `vyrn why` learns the capability axis: `vyrn why --capability fs
<artifact>` prints every chain that pulls `fs` in. Gate: no diagnostic in the
tree names a path the project does not contain.

**M4 — dogfood.** The fullstack example (`shelf`) declares both artifacts and
compiles with the floor on; one commit in its history introduces a leak and
shows the rejection. Gate: the leak commit's error message pasted into this
file, unedited.
