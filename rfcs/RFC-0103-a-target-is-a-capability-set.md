# RFC-0103 — A Target Is a Capability Set

- **Status:** **Implemented.** Five milestones and five pull requests, and the
  arc ends where §2 says it should: a target is a capability set nobody can
  relabel, the census filled every cell by running a program rather than by
  reading a comment, the floor walks each artifact's closure and prints the
  chain the author could not see, both fence arms and the floor spell one
  crossing, `vyrn why` learned the capability axis, and M4 ran the whole thing
  against `shelf` — where it accepted, refused, and surfaced two defects in how
  a result is spelled and none in what it decides. A milestone that fails its
  gate says so in this file.
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
| `fs` durability | `fsyncFile` | yes | yes | **no** — the canonical `Err` (`` cannot write `p` ``), exit 0, exactly as the `fs` row above |
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

[Amended after M0 — the `fs durability` row only. Its `wasi` and `browser` cells
read "the build is refused" when M0 ran, which is the regression M0 filed. The
direct backend lowers `fsyncFile` now — one `fd_sync` between the `open_at` and
the `fd_close`, the shape `writeFile` already had — so both cells were re-run
against a module that exists: `fsyncFile: ok` under `wasmtime --dir .`,
byte-identical to the interpreter and native legs, and
`` fsyncFile: cannot write `cap-fsync.tmp` `` in-page through `web/wasi-min.js`
(`census.html` loads `cap-fsync.wasm` now, having had no module to load before).
`examples/storage.vyrn` calls `fsyncFile`, so the parity corpus holds the row
instead of the census having to re-read it. Every other cell is M0's.]

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
   uses unconditionally, and `listDir` has none in either. [`fsyncFile` is
   lowered now — see the amendment under §2's table — so of the two only
   `listDir` still refuses; the finding stands on `listDir` alone.] The
   language already refuses a capability at compile time in two places, which is
   precedent M2 can point at rather than novelty M2 has to argue for.

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

### M2 — as landed

**Gate: met.** `examples/leak` is refused with the whole chain, every existing
example stays green, and the parity suite is untouched — 40 passed, including
`wasm_only_examples_trap_identically`, the test the extern decision was supposed
to cost.

#### The vocabulary, normative

Four capabilities, and nothing else is tracked:

| capability | carried by |
|---|---|
| `fs` | `readFile`, `readFileBytes`, `writeFile`, `renameFile`, `fsyncFile`, and the `logging { sink: file("…") }` DECLARATION |
| `stdin` | `readLine` |
| `args` | `args` |
| `extern` | an `extern fn` IMPORT declaration (not the call, not `export extern fn`) |

M0's table has more rows than this, and the ones it has that this does not are
absent for two opposite reasons. `stdout` / `stderr`, the clock, entropy and
threads are UNIVERSAL — every target answers yes, so a row for them would refuse
nothing and say nothing. `listDir` and `serveStream` are the other end (finding
5's second option, taken): no compiled target has them, so they stay outside the
floor and keep the refusals they already have — a missing lowering for `listDir`,
a runtime trap for `serveStream`. A capability set indexed by three targets
cannot say "every artifact lacks this", and inventing a fourth answer to say it
buys one diagnostic that two mechanisms already give.

The target sets are Rust constants in `vyrn_frontend::floor::capabilities`.
Nothing in `vyrn.json` reads them, writes them, or can argue with them — that is
the whole difference between this and the fence:

| target | has |
|---|---|
| `native` | `fs`, `stdin`, `args` |
| `wasi` | `fs`, `stdin`, `args` |
| `browser` | `extern` |

`wasi` and `browser` are the identical bytes under two hosts (finding 1), and
this table is where the two differ: a WASI host answers `path_open`, `fd_read`
and `args_get`; a page answers `NOENT`, EOF and an empty list, and IS the `vyrn`
import namespace an `extern` needs.

#### The findings, resolved

- **Finding 3 — `extern` under wasi fails at instantiation.** Taken as the shape
  of the whole check, not just of its `extern` row. A module carries a capability
  because the carrier is WRITTEN in it, never because a branch reaches it, and
  `extern` is carried by the DECLARATION for exactly the reason the census found:
  a program that never calls the import still cannot start.
- **Finding 4 — one `fs` reach fails silently.** `logging { sink: file(..) }` is
  a carrier, quoted in the diagnostic as the declaration it is
  (`` `logging { sink: file("app.log") }` needs `fs` ``) with line 0, because the
  AST keeps no line for it. It is the one capability in the vocabulary carried by
  something other than a call, and §2's "the union of the capabilities its calls
  carry" is corrected here rather than left to be re-derived.
- **Finding 5 — two capabilities no compiled target has.** Second option, above.

#### `fsyncFile` is `fs`, not a fifth row

M0 split it out because its answer differs: the direct backend had no lowering
for it, so `--target wasm` was refused outright. That is a missing lowering — a
filed regression against the wasm backend — and not a second capability. The
floor names the capability; the backend keeps its own refusal, and a `wasi`
artifact that calls `fsyncFile` passes the floor and is then refused by the
emitter, which is the correct division of labour between a capability rule and a
lowering gap.

The prediction came due. The lowering landed, the refusal went away, and the
floor did not change by one line: `fsyncFile` was already the `fs` row's fifth
name in `floor.rs`, and every capability test kept passing. A capability rule
that had been written against the gap would have had to be unwritten here.

#### The extern decision, as implemented, and what it actually cost

The floor refuses `extern` off the browser, and refuses it ONLY for a root that
IS a declared artifact's entry. A file no artifact names has no target, so it has
no floor, whatever sits above it in the directory tree. That single narrowing is
what the census's "one real cost" turned out not to cost:
`examples/externdemo.vyrn` declares no artifact, so it still builds natively and
still traps with the interpreter's wording, byte for byte. All three tests the
census listed as the real work pass untouched. The three generated-extern clients
are browser artifacts, and the browser has `extern`.

**One cost the census did not count, and it is a genuine catch.**
`vyrn-cli` `derived::a_name_declared_identically_in_two_modules_is_not_a_collision`
drove the `client(..)` generator from `server.vyrn` — the manifest's NATIVE
artifact — purely to assert type dedup. `client(..)` emits the `vyrnRpcCall`
extern; `rpcInProcess(..)` is the flavor a native root is supposed to use. So the
program under test would have trapped at its first stub call, and the floor said
so at check time:

```
generated by client("./server/api") at …/server.vyrn:24:0: artifact `server`
(native) cannot include `generated by client("./server/api") at …/server.vyrn`:
it imports a host function
  note: server.vyrn → generated by client("./server/api") at …/server.vyrn
   = `vyrnRpcCall` needs `extern`; target `native` has no host to import from
```

The test now drives the same generator from `client/boot.vyrn`, the manifest's
browser artifact, and asserts the same dedup. The count in M0 was four examples
and three tests; it was four examples and four tests, and the fourth is the only
one that was a program the language would have refused to run.

**And one the census got wrong in the other direction.** `std/time` and
`std/random` declare `hostNowMillis`, `hostMonotonicNanos` and `hostRandomSeed`
as `extern fn`, and they are not host imports at all: the C runtime shim
implements all three on EVERY target, which is what keeps a clock example a
three-way parity citizen. Reading `extern fn` as one thing would have put an
`extern` requirement into every native server that logs a timestamp. `extern fn`
is two things and only one is a capability, so the three names moved out of
`vyrn-codegen` into `vyrn_frontend::trap::HOST_EXTERNS` — the frontend has to be
able to tell an import from a shim call, and a second copy of the list is the
drift that file exists to end.

#### Where it runs, and the diagnostic

`vyrn_frontend::floor::objection` runs at the end of the loader's link, beside
the audience check, so `check`, `build`, `run` and the LSP all get it. Last in
the link, so the closure it walks is everything the artifact links — the modules
the source imports AND the runtime modules a builtin's desugar injected. The
chain is breadth-first from the entry, so the reported path is the SHORTEST one
that reaches the offending module.

The gate's own message, unedited:

```
examples/leak/server/db.vyrn:6:0: artifact `app` (browser) cannot include `server/db.vyrn`: it reaches the filesystem
  note: client/boot.vyrn → shared/format.vyrn → server/db.vyrn
   = `readFile` needs `fs`; target `browser` has no filesystem
   = call it through the wire instead: connect("../server/db")
```

The remedy names the module actually reached, spelled as the module that imports
it would spell it — RFC-0072's fixed `client("./server/api")` is replaced for
this one crossing. The fence's own remedy and `vyrn why --capability` are M3.

`examples/leak` is a project, not a file, so the parity corpus never sees it —
that loop reads `examples/*.vyrn` and `examples/<subdir>/server.vyrn`, and this
project's native entry is `server/main.vyrn`. `EXPECTED_CHECK_FAILURE` is a list
of single files and a project does not fit it, so the gate's assertion lives in
`compiler/vyrn-cli/tests/floor.rs` instead, which is where the rest of the
milestone's integration tests are.

#### The gate, measured

| evidence | result |
|---|---|
| `cargo test --release` (workspace) | 1725 passed, 0 failed, 69 ignored |
| `cargo test` (`vyrn-lsp`) | 83 passed, 0 failed, 4 ignored |
| `cargo test -p vyrn-cli --release --test parity -- --ignored` | 40 passed, 0 failed |
| `cargo fmt --check` (workspace + `vyrn-lsp`, `vyrn-genwasm`, `vyrn-play`) | clean |
| `vyrn check` on `fullstack`, `shelf`, `bin` (both entries each) and `externdemo.vyrn` | `ok` / exit 0, all seven |
| `vyrn check examples/leak/client/boot.vyrn` | refused, chain above, exit 1 |
| `vyrn check examples/leak/server/main.vyrn` | `ok` — one module, two artifacts, two answers |

**M3 — the remedy and the reframe.** `remedy()` replaced by the concrete
crossing; RFC-0072's document amended to the fence claim (accidental class,
not secrets); `vyrn why` learns the capability axis: `vyrn why --capability fs
<artifact>` prints every chain that pulls `fs` in. Gate: no diagnostic in the
tree names a path the project does not contain.

### M3 — as landed

**Gate: met.** No diagnostic in the tree names a path the project does not
contain. Every remedy the compiler can print is an interpolation now, and there
are three of them:

```
$ grep -rn 'call it through\|move the shared part' compiler/*/src/
compiler/vyrn-frontend/src/audience.rs:486:            "call it through `{}` instead",
compiler/vyrn-frontend/src/audience.rs:490:            "move the shared part of `{}` into a universal module and import that instead",
compiler/vyrn-frontend/src/floor.rs:318:            "\n   = call it through the wire instead: {}",
```

`server/api` still appears sixteen times under `compiler/*/src/` and every one
is a doc comment, a test fixture or a generated-source constant — no diagnostic
among them. The two that are about this change say what was deleted
(`audience.rs:478`, `floor.rs:344`).

#### The remedy, concrete

`audience::remedy` was three `&'static str`s; it now takes the edge. Both live
arms name the module the edge actually reached:

```
error: `app/routes/index.vyrn` is universal and cannot import `server/store.vyrn`,
       which is server-only
  note: audience `server` is declared by vyrn.json:audience.server — call it through
        `connect("../../server/store")` instead; the importer's own audience comes
        from path segment `app` (vyrn.json audience.universal)
```

```
error: `server/store.vyrn` is server-only and cannot import `client/boot.vyrn`,
       which is client-only
  note: audience `client` is declared by vyrn.json:audience.client — move the shared
        part of `client/boot.vyrn` into a universal module and import that instead;
        the importer's own audience comes from path segment `server`
        (vyrn.json audience.server)
```

The server arm and M2's floor line are the same function.
`vyrn_frontend::floor::crossing(importer, module)` is the one place that spells
a crossing, over M2's `spec_from`, and the fence and the floor both call it.
That is what makes the gate a property rather than a coincidence: there is no
second place where a path could be written down.

The client arm keeps its advice and drops its hint. `(`shared/`)` was a
parenthesised guess at a directory the project may not have; the module it names
instead is one the reader can open.

#### RFC-0072, amended

Two tagged amendments in `rfcs/RFC-0072-audience-and-derived-rpc.md`, plus an
**Amended by** line in its header. The claim:

> before: This is the improvement over the prior art. Nuxt's split is a bundler
> convention, so a server import reaching a component is a build-time surprise
> at best and **a leaked secret at worst**.

> after: … so a server import reaching a component is a build-time surprise.

with a note under it saying what audience does prevent (the accidental class),
why it can prevent nothing else (the compiler does not know what a secret is; a
label is configuration), that this makes it a fence with Gradle's guarantee, and
that RFC-0103 is the floor beneath it. The document's diagnostic sketch:

> before: `   = call it through `client("./server/api")` instead`

> after: `   = call it through `connect("../../server/store")` instead`

— the concrete crossing for that sketch's own edge, which is what the checker
now prints for it.

Nothing else in the document was rewritten. Its `server/ … never in the client
bundle` table is a statement about import edges, which is what the fence checks
and what it still delivers.

#### `vyrn why --capability`

The floor's refusal shows ONE chain, the shortest, because a refusal is read in
a hurry. `vyrn why --capability <fs|stdin|args|extern> <entry-or-artifact-name>`
answers the other question — where does the capability come from at all — with
EVERY chain, because deleting a hop off the shortest path removes nothing while
a second path still reaches the module.

```
$ cd examples/leak && vyrn why --capability fs app
N:/lang/examples/leak/client/boot.vyrn
  artifact: `app` (browser) — target `browser` has no filesystem
  `readFile` needs `fs` — server/db.vyrn:6
    client/boot.vyrn -> shared/format.vyrn -> server/db.vyrn

$ vyrn why --capability fs api
N:/lang/examples/leak/server/main.vyrn
  artifact: `api` (native) — target `native` has `fs`
  `readFile` needs `fs` — server/db.vyrn:6
    server/main.vyrn -> shared/format.vyrn -> server/db.vyrn

$ vyrn why --capability stdin app
N:/lang/examples/leak/client/boot.vyrn
  artifact: `app` (browser) — target `browser` has no stdin
  nothing in artifact `app`'s closure needs `stdin`

$ vyrn why --capability sockets app
error: unknown capability `sockets` (expected one of: fs, stdin, args, extern)

$ vyrn why --capability fs nope
error: `nope` is neither an artifact entry point nor an artifact name in
       …/examples/leak/vyrn.json (declared: api, app)
```

Four decisions worth writing down:

- **It answers for an artifact that HAS the capability too.** `api` is native
  and a native binary may read files; the question was where `fs` enters the
  closure, and refusing to answer it for the artifact that is allowed to would
  make the command a second copy of the check rather than a way to see the
  graph.
- **The argument resolves the way the floor resolves a root**: a path through
  `ArtifactMap::artifact_for` (file identity first, so two spellings of one file
  are one artifact), else a name in the manifest's map. The refusal lists the
  names that do exist.
- **It reads the sources, not a build** — the same reading `vyrn why <file>`
  does, through the same `project_imports`. The artifact you are asking about
  may well be the one that does not compile.
- **The vocabulary and the carriers both come from `vyrn_frontend::floor`**, the
  module the loader enforces with: `Capability::parse` against `CAPABILITIES`,
  and `floor::carried` per module. The report cannot drift from the check
  because it is the check's own reading.

Chain enumeration is bounded at 24 chains and depth 12, the bounds
`import_chains` already carries — a report that hangs is worse than one that
stops.

#### The gate, measured

| evidence | result |
|---|---|
| `cargo test --release` (workspace) | 1727 passed, 0 failed, 69 ignored |
| `cargo test` (`vyrn-lsp`) | 83 passed, 0 failed, 4 ignored |
| `cargo build --target wasm32-unknown-unknown` (`vyrn-play`), `cargo test` (`vyrn-genwasm`) | both COMPILE; genwasm 1 passed, 0 failed |
| `cargo test -p vyrn-cli --release --test parity -- --ignored --test-threads=1` | 40 passed, 0 failed |
| `cargo fmt --check` (workspace + `vyrn-lsp`, `vyrn-genwasm`, `vyrn-play`) | clean |
| the grep above | three remedies, three interpolations, no path |

**M4 — dogfood.** The fullstack example (`shelf`) declares both artifacts and
compiles with the floor on; one commit in its history introduces a leak and
shows the rejection. Gate: the leak commit's error message pasted into this
file, unedited.

### M4 — as landed

**Gate: met.** The leak commit's error is below, unedited, and the branch tip
checks clean. Two commits:

| commit | subject |
|---|---|
| `125e661` | a deliberate leak: a tag snapshot under `shared/` puts a filesystem read two hops from a browser entry nobody edited (RFC-0103 M4) |
| `c09f359` | the leak removed: the branch tip is green and the refusal stays in the history where the milestone asked for it (RFC-0103 M4) |

No compiler code was touched. This milestone runs M2's check and M3's report
against a real application and writes down what they say.

#### The leak, and the gate

`shared/snapshot.vyrn` is new and reads `.shelf-tags` with `readFile` — a tag
line beside the store, so a cold page can show the tag filter before the first
`books/browse` completes. `shared/util.vyrn`, which `client/boot.vyrn` already
imports for `splitTrim`, gains one import of it and one helper over it.
`client/boot.vyrn` is NOT touched by the leak commit: the browser entry's author
never wrote `readFile`, never named a server directory, and cannot see hop
three. That is the whole diagnostic's reason to exist, and it is what a real
change of this shape looks like — the leaking edit is four lines in a helper.

```
$ cd examples/shelf && vyrn check client/boot.vyrn
shared/snapshot.vyrn:11:0: artifact `client` (browser) cannot include `shared/snapshot.vyrn`: it reaches the filesystem
  note: client/boot.vyrn → shared/util.vyrn → shared/snapshot.vyrn
   = `readFile` needs `fs`; target `browser` has no filesystem
   = call it through the wire instead: connect("./snapshot")
```

`vyrn check server.vyrn` is `ok` in the same tree. One project, two artifacts,
two answers, in an application rather than in a fixture built to be refused.

`vyrn why --capability fs client` in the leaking tree finds the same chain from
the other direction (the branch was checked out in a worktree, so the absolute
prefix each `why` prints below is rewritten to the repository's own; nothing
else in any output on this page is touched):

```
N:/lang/examples/shelf/client/boot.vyrn
  artifact: `client` (browser) — target `browser` has no filesystem
  `readFile` needs `fs` — shared/snapshot.vyrn:11
    client/boot.vyrn -> shared/util.vyrn -> shared/snapshot.vyrn
```

#### The floor on `shelf`, before the leak

`examples/shelf/vyrn.json` has declared both artifacts explicitly since M1, so
the floor is armed for both entries. Both check clean, and all eight
`vyrn why --capability` answers say the same thing: `shelf` carries none of the
four capabilities in any module on disk.

```
$ vyrn why --capability fs server
N:/lang/examples/shelf/server.vyrn
  artifact: `server` (native) — target `native` has `fs`
  nothing in artifact `server`'s closure needs `fs`

$ vyrn why --capability fs client
N:/lang/examples/shelf/client/boot.vyrn
  artifact: `client` (browser) — target `browser` has no filesystem
  nothing in artifact `client`'s closure needs `fs`

$ vyrn why --capability extern server
N:/lang/examples/shelf/server.vyrn
  artifact: `server` (native) — target `native` has no host to import from
  nothing in artifact `server`'s closure needs `extern`

$ vyrn why --capability extern client
N:/lang/examples/shelf/client/boot.vyrn
  artifact: `client` (browser) — target `browser` has `extern`
  nothing in artifact `client`'s closure needs `extern`
```

`stdin` and `args` answer "nothing … needs" for both artifacts too.

#### What the dogfood surfaced

Four things, three of them corrections to what this document or its milestone
brief assumed.

**1. `shelf` never touches the filesystem.** The milestone was written expecting
the server artifact to carry `fs` through `std/storage`. It does not: the shelf
store is module state — three seed books in an `Array<Book>` — and nothing in
the project imports `std/storage` or names an I/O builtin. `examples/bin` is the
persistent dogfood app; `shelf` is the fullstack one. So the floor's answer for
the shipped `shelf` is "no capability required anywhere", which is the least
interesting true answer and had to be measured to be known.

**2. `vyrn why --capability` cannot see a generated module, and the check can.**
The report reads the project's SOURCES through `project_imports` /
`project_sources` (an M3 decision, made so the command answers for an artifact
that does not compile). Generated modules are produced by the loader, so they
are not on disk and never enter the report's graph. `shelf`'s client imports
`client("../server/api")`, whose generated module declares the `vyrnRpcCall`
`extern` — the very import M0 counted and M2 priced — and the report says
`nothing in artifact 'client's closure needs 'extern'`.

The check itself is not fooled. Retargeting that entry to `native` for one run
(dropping the `client` sugar key so the explicit artifact is the only reading of
the file) produces:

```
generated by client("../server/api") at client/boot.vyrn:37:0: artifact `app2` (native) cannot include `generated by client("../server/api") at client/boot.vyrn`: it imports a host function
  note: client/boot.vyrn → generated by client("../server/api") at client/boot.vyrn
   = `vyrnRpcCall` needs `extern`; target `native` has no host to import from
```

So the gate M3 claimed — "the report cannot drift from the check because it is
the check's own reading" — is true of the VOCABULARY and the CARRIERS, and false
of the GRAPH. `floor::carried` is shared; the module set is not. The report
under-reports exactly the capability whose carriers are written by generators
rather than by authors, which is the one class of leak nobody can find by
reading their own source. Filed, not fixed here.

**3. The fence answers first, so a leak into `server/` never reaches the
floor.** The brief asked for a chain shaped `client → shared → server module`.
In `shelf` that shape cannot produce a floor diagnostic, because `shelf` also
declares an `audience` map and the audience check runs per import EDGE during
the link while the floor runs on the finished graph. Putting the same
`readFile` helper in `server/snapshot.vyrn` and importing it from
`shared/util.vyrn` gives:

```
shared/util.vyrn:6:0: `shared/util.vyrn` is universal and cannot import `server/snapshot.vyrn`, which is server-only
  note: audience `server` is declared by vyrn.json:audience.server — call it through `connect("server/snapshot")` instead; the importer's own audience comes from path segment `shared` (vyrn.json audience.universal)
```

This is the correct outcome and worth stating plainly: where both layers are
declared, the fence catches the labelled crossings first and the floor is what
remains underneath. `examples/leak` declares no `audience` key, which is why it
demonstrates the floor alone. The leak that lands here is therefore in a place
the fence permits — `shared/` is universal by the project's own manifest, and a
tag-snapshot reader is a plausible thing to put there. Nothing about that path
is mislabelled. The file is still unreadable in a browser, and only the floor
says so. That is the RFC's thesis with the labels all correct.

**4. A remedy's spelling depends on the working directory `vyrn` was run
from — M3's gate is narrower than it claimed.** `floor::spec_from` compares two
module keys segment by segment and falls back to the module key itself when the
two share no FIRST segment. Module keys are as relative as the path the CLI was
handed (`audience::relative_to` says so), so the fallback fires whenever the
command is run from inside the project and the two files sit in different
top-level directories. M2's own example shows it:

```
$ cd examples/leak && vyrn check client/boot.vyrn
   = call it through the wire instead: connect("server/db")

$ cd .. && vyrn check leak/client/boot.vyrn
   = call it through the wire instead: connect("../server/db")
```

The second is right. The first names `shared/server/db` to a reader of
`shared/format.vyrn`, which is a module the project does not have — the exact
failure M3's gate was written to end, surviving in the one invocation a
developer is most likely to make. It is a real defect and it is recorded rather
than fixed, because M4 is the milestone that runs the thing and this file is
where a milestone says what it found. The fix belongs where `spec_from` can see
the project base.

M4 found no defect in the floor's own decision. The check accepted what should
be accepted and refused what should be refused, on an application, with the
fence live beside it — and both flaws it did surface are in how the result is
SPELLED, not in what it decides.

#### The gate, measured

| evidence | result |
|---|---|
| `vyrn check` at the leak commit (`examples/shelf/client/boot.vyrn`) | refused, the message above, exit 1 |
| `vyrn check` at the leak commit (`examples/shelf/server.vyrn`) | `ok`, exit 0 |
| `vyrn check` at the branch tip, both `shelf` entries | `ok` / exit 0, both |
| `vyrn test examples/shelf/client/boot.vyrn` at the tip | 3 passed, 0 failed |
| `cargo test --release` (workspace) | 1727 passed, 0 failed, 69 ignored — unchanged from M3 |
| `cargo fmt --check` (workspace + `vyrn-lsp`, `vyrn-genwasm`, `vyrn-play`) | clean |
| `vyrn fmt --check` on both files the leak commit touched | clean |
