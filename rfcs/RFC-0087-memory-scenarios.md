# RFC-0087 — Every Memory Scenario, and What Handles It

- **Status:** **Census, closed; Phase 10 took the tail.** The arc it opened
  (RFC-0089, RFC-0090, RFC-0091, ten phases) is finished, and "The census,
  closed" at the end of this file is its final state: what closed, what did not,
  and what the arc found instead. Phase 10 closed §14 and §16 and left U4, so
  the memory suite reads eleven steady of twelve. Part I is correctness — is
  memory reclaimed. Part II is usability — can a person see the model. Part III
  is cost — what does it spend. The three ranked tables were the proposed work;
  each now carries its outcome.
- **Depends on:** RFC-0004 (the hybrid model), RFC-0011 (elements are a safe
  leak), RFC-0012 (the extern String ABI), RFC-0075 (linear streams),
  RFC-0077 M6 (the wasm allocator), RFC-0086 M1 (the compiler asks the type)
- **Evidence:** every claim below is either read from the named file or run.
  Runs are marked **(measured)**.

---

## Why a census

Parity is an **output** invariant. Memory is not output. A program that leaks
prints the same bytes as one that does not, so `parity: N checked, 0 failed` says
nothing about reclamation. The interpreter is worse than silent: its values are
Rust values, so it **cannot** leak, and it can never disagree with a backend that
does.

One test guards the whole model. `compiler/vyrn-cli/tests/memory.rs` asserts that
memory after N exported calls equals memory after 4N. It covers one shape.

So the model is decided by reading, and this document is that reading. It lists
every place a Vyrn program uses memory, what reclaims it today, and where the
answer is wrong or absent.

---

## The six strategies

| strategy | who reclaims | when |
|---|---|---|
| static data | nobody | never; correct |
| stack | the frame | function exit |
| owned heap (Path A) | `own::analyze` | block exit, `return`, `break`, `continue`, `?` |
| region arena | `__vyrn_region_exit` | `region { }` exit |
| linear obligation | `movecheck` | compile time; the program must say where |

There were six. **Path B — a slab of 65,536 generation-counted slots, reclaimed
by `release` — is deleted (RFC-0090 M4, Phase 8e), and §6 below records what it
was.** Five remain.

Every allocation belongs to exactly one. That is what stops a double free: a
concat inside a `region` draws from the arena and `own` skips it; a concat
outside draws from `malloc` and `own` decides it.

---

## 1. Values with no heap

| scenario | handling | issue |
|---|---|---|
| `Int64`, `Float64`, `Bool`, `Byte`, SIMD lanes, `Unit` | stack or register | none |
| a string literal | data segment | none. `free` on one is refused silently by the wasm allocator (below `HEAP_BASE`) |
| `ArrayN<T, N>` — `let a = [1, 2, 3]` | N words inline | **one syntax, three layouts.** `[1,2,3]` inline, `Array<T>` heap, `SmallArray<T,N>` inline-until-spill. Only the annotation separates them |
| a record or enum by value | copied | the heap inside it belongs to whoever produced it. §3 |

**The `ArrayN` issue already cost a heap.** `own::expr_type` first answered
`Array` for every `ArrayLit`; the drop site read three words off a stack slot and
freed the first, and `ifexpr.vyrn` exited `0xC0000374`. `expr_type` now refuses to
type an array literal at all, and `an_unannotated_array_literal_is_not_released`
pins it.

---

## 2. Owned heap — freed at block exit

The rule is two questions (RFC-0086 M1). `Analysis::transfers(e)` asks whether the
expression hands over a value nobody else holds. `Owned::release_kind(ty)` asks
how that type is released. Both are exhaustive matches with no `_` arm.

| scenario | release |
|---|---|
| `a + b`, `"..\{x}"`, `@str` | `free` the buffer |
| `Array<T>` buffer | `free` |
| `Map<String, V>` — two parallel buffers | `free` both |
| `SmallArray<T, N>` spilled buffer | `free`; null while inline, so the site is the same |
| `cell(v)` | release the slot, bump its generation, free the payload |
| a type with `impl Owned for T` | call the declared `release` |
| the result of a function that transfers | whichever of the above the type says |

Drops run **newest first**, in every engine. Early exits emit the same drops
mid-block (`emit_all_drops`).

### Issues

**2a. `transfers` says no to five expression forms.** `Match`, `IfExpr`, `Try`,
`TryConstruct`, `Spawn` and `Lambda` all answer `false`, so their results leak.
This is a deliberate under-approximation — wrong here means a leak, never a double
free — but RFC-0030 made `if` an expression and nothing revisited it. So this
leaks:

```vyrn
let s = if c { a + b } else { a + c }
```

**2b. `builtin_producers` is the last hand-written list.** RFC-0086 M1 deleted the
others. Six entries remain: `@concat`, `@str`, `cell`, `array`, `push`, `@keys`.
A builtin that allocates and is absent leaks — which is exactly how `@keys` was
found. It cannot move to the type, because `at(a, 0)` and `m.keys()` have the same
result type and opposite answers. It should move to the **function**: a signature
that says it transfers.

**2c. A `mut` String or `mut Ref` is never tracked.** An array, a map and a small
array are exempted because they are mutated in place and keep their identity. A
String is not, so `let mut s = a + "b"` leaks. **(measured: four `malloc`s, zero
`free`s.)**

**2d. `live` under-records binders** the way `str_vars` did before PR #63. Its
failure direction is a spurious escape, so it leaks rather than miscompiles. It is
the only walker in `own.rs` that does not use `Scopes<T>`.

**2e. `expr_type` is a second, weaker type pass.** The checker's table cannot be
reused: it is keyed `(line, name)` and two `let`s share a line, and
`check_and_synthesize` appends the `jsonenc`/`jsondec` bodies *after* the check,
so their strings have no entry. Sourcing from it would silently stop freeing in
every program that encodes JSON.

---

## 3. Escape — a leak on purpose

| scenario | why it escapes |
|---|---|
| stored in a record field | the record now holds it and outlives the block |
| stored in an array element or a map value | same. RFC-0011 states elements are a safe leak |
| stored in a cell by `set(c, v)` | the cell outlives the block |
| aliased — `let t = s` | two names, one buffer |
| passed to any function outside the safe-read list | the callee may retain it |
| captured by a lambda | a stored closure can run after the block exits |

Every one is correct: never a double free, never a use-after-free. Together they
are where the unbounded leaks live. The only tool is `drop x`, which is
whole-container and reclaims no element.

---

## 4. Overwrite — the sharpest gap

An assignment stores a new pointer over an old one. **Nothing releases the old
value.**

| scenario | measured |
|---|---|
| a module-state `String`: `acc = acc + x` | yes — every call leaks the previous buffer |
| a local `mut` String reassignment | yes |
| `r.field = v` | yes |
| `a[i] = v` | stated in `own.rs`, RFC-0011 |
| `m[k] = v` | same |

The global case, from emitted IR:

```llvm
%t7 = call ptr @__vyrn_malloc(i64 %t6)
call ptr @strcpy(ptr %t7, ptr %t1)     ; t1 is the OLD @g.acc
call ptr @strcat(ptr %t7, ptr %t2)
store ptr %t7, ptr @g.acc              ; the old buffer is now unreachable
```

This is the shape a server has. `examples/bin` and `examples/shelf` both rebuild
module state per request. RFC-0081's `str_append` fixes one instance of it — a
self-append spine on a *local* reallocates in place and keeps one buffer — and a
global does not qualify.

**How it should be handled.** An assignment to a slot whose type has a release
kind is a release of the old value followed by the store. The ordering trap is
that the old value is usually an operand of the new one, so the release must come
**after** the store, and only when the analysis proves the slot was the sole
owner — the same `droppable` condition, applied to a slot rather than a block.
The self-append path already solves the common instance by not allocating at all.

---

## 5. Region arena

`region { }` pushes a singly-linked allocation list. The block exit frees the
whole list. Nesting is capped at 64 and traps past it. The stack is
`thread_local`, so a `region` inside a `spawn` is self-contained.

Routing is **lexical**: `heap_alloc` draws from the arena when `region_depth > 0`.
`push` cannot, because arena memory has no `realloc`, so a growable array inside a
region still uses `malloc`.

**Issue.** The region is explicit and hand-placed. RFC-0004 asked for inferred and
invisible regions (Q3) and nothing was built. The case a region serves best — a
loop that builds many values with one lifetime — is the case `push` excludes.

---

## 6. ~~Generational references — Path B~~ — DELETED (RFC-0090 M4)

**This strategy no longer exists.** `cell(v)` took a slot from a fixed slab of
65,536; a `Ref<T>` was `{slot, gen}`; every `get`/`set` compared its generation
against the slot's. Phase 8e deleted the builtins, the type and the slab from all
three engines.

**What replaced it is `std/slots` — the same three facts, in Vyrn.** A `Handle<T>`
is slot + generation + container identity, and the check is an integer compare in
`std/slots.vyrn` that a reader can open. It measured **2.02x faster** than the
slab it replaces (RFC-0090 "M1 as landed"), because Path B boxed every payload
through the allocator and a `Slots` keeps its payloads flat in an `Array<T>`.

The aliasing case did not go away and is not the compiler's business any more.
`genref`, `freelist`, `linkedlist`, `tree`, `autorelease` and `slottable` — the
six corpus programs that were written against `Ref<T>` — all still run, on
`std/slots`, under three-way parity.

**The old issue, and what happened to it.** A use-after-release was a run-time
trap rather than a compile error, and RFC-0004 §5.2 accepted that price for
aliasing with no annotations. A dead-handle use is still a run-time trap. It
names `std/slots.vyrn:189` since U5 closed. What did change is that a `Slots`
also catches a handle used against the WRONG container, which one global slab
could not.

The compile-time answer for the aliased case is still region borrowing, and it is
still not designed.

---

## 7. Linear obligation

`Stream<T>` is acquired once and disposed exactly once, and `movecheck` proves it
at compile time. A stream parameter carries the obligation into the callee. This
is the **only** compile-time reclamation proof in the language.

**Issue.** It is three `Type::Stream` matches. A user's file handle, socket or
connection gets nothing.

**Closed by RFC-0086 M3.** The matches are a lookup in the table `impl Owned`
already feeds, so `impl MustUse for T` joins a declared type to the mechanism the
way `impl Owned` joins §2, and the diagnostics name that type. Half the
consolidation had already happened during this arc and was not recorded: by the
time M3 was picked up there was one `must_use`, resolving aliases, and what was
left was that its body was hardcoded.

Two things were found on the way, and both were the milestone being unusable
rather than optional. The checker refused `drop x` on **any** type declaring
`impl Owned`, so a declared release could only run on the automatic path — and
must-use says "dispose by name", so `drop` is the only terminal discharge there
is. And the interpreter's `Stmt::Drop` was a no-op, so a declared `release` ran
at block exit and not at an explicit `drop`; both compiling backends had asked
`release_kind` there all along. Neither had a victim while the other stood.

**One restriction stays `Stream`'s.** Nothing may store a stream (§7's own
reason: a cursor is aliased the moment it is stored), and a declared must-use
type may be stored freely — so a value put into a record field or an array
counts as discharged there and the obligation does not travel with the
container. That is representation against obligation, and it is recorded in U7
rather than fixed by widening a rule written for a cursor.

---

## 8. Program lifetime

Module state and top-level `let` (RFC-0013, RFC-0029) initialize in
`@__vyrn_globals_init` and are never freed. That is correct at process exit. For
reassignment see §4.

---

## 9. The extern boundary

| direction | ABI | ownership |
|---|---|---|
| import — Vyrn calls JS | `(ptr, len)` pair | JS reads; the module keeps the buffer |
| export param — JS calls Vyrn | one `ptr`, NUL-terminated | **the caller owns it.** `wasi-min.js` allocates with `__vyrn_malloc` and frees with `__vyrn_free` in a `finally`, so a caught `panic` still releases |
| export return | one `ptr` | **the caller owns it** since RFC-0089 M3b — `wasi-min.js` decodes, then frees |

**9a. A returned String leaks.** The decision is recorded in `wasi-min.js`:
ownership differs per function, a module-state field or a literal is borrowed, and
freeing one would be a use-after-free. Nothing crosses the boundary that says
which.

**How it should be handled.** The fact already exists. `own::owned_fns` holds it
per function. The wrapper already reads `hooks.exportReturns[name]` for the return
*type*; the same generated map can carry ownership, and the release becomes one
more line beside the argument release. This costs no new analysis.

**Closed by RFC-0089 M3b.** Nothing had to cross, because rule 3 removed the
question rather than answering it: a return is owned. The paragraph above is
wrong on two counts, and both mattered. `exportReturns` is **hand-written**, not
generated — four pages and one runtime spell it out — so it could not have
carried a compiler fact. And rule 3 did not yet hold: `check_return` let three
shapes lend a String, one of which was a live use-after-free between two Vyrn
functions. See RFC-0089 "M3b as landed". A literal needs no rule at all: Phase 2
gave it `cap = 0` and `free` ignores any pointer below `HEAP_BASE`.

**9b. An export that retains its String parameter is a use-after-free.** Five
programs did exactly `state = arg`. It was always wrong under RFC-0012 and it was
harmless only while nothing could free. Now the caller frees. All five copy, and
**nothing checks the rule.**

**How it should be handled.** A checker rule on `export extern fn`: a `String`
parameter may not reach a global, a record field, an array element, a map value or
a cell. `own`'s escape walker already computes precisely this set — it is the same
question with a different consequence.

**Closed by RFC-0089 M2 and M3b.** Rule 2 refuses the store, for every function
rather than only an exported one, since Phase 4b-2. M3b made the message name
`.copy()` alone inside an export, and refused the one escape hatch that was left:
`consume` on an extern String parameter compiled, and it read as ownership of a
buffer the page frees when the call returns.

---

## 10. Concurrency — **CLOSED** by RFC-0095 M1

`spawn` mallocs a frame holding the result slot and the arguments, and hands it to
a thread. The frame **was** never freed — stated in the lowering, because `join`
was idempotent and might run more than once. One leak per `spawn`, plus one Win32
event handle per `spawn`, bounded by the number of spawns:

| spawns | peak, before | handles, before | peak, after | handles, after |
|---|---|---|---|---|
| 1,000 | 4,248 KiB | 853 | 3,860 KiB | 71 |
| 20,000 | 5,924 KiB | 20,076 | 4,332 KiB | 72 |
| 50,000 | 8,312 KiB | 50,076 | 3,880 KiB | 72 |
| 200,000 | 20,252 KiB | 200,076 | 3,880 KiB | 72 |

**What closed it is ownership of the `Task<T>` value.** A `Task<T>` is linear
(RFC-0095 M1): `t.join()` consumes it and yields the result, `drop t` waits for it
and releases the result by its type, and a task that is never discharged is
refused. Because there is exactly one join, the join is where the storage goes
back — the frame, the task record and the event object, at one site. Both columns
above are now flat in the spawn count rather than linear in it.

The arena stack is `thread_local`, so a region inside a task is correct. A heap
value moved into a spawn escapes at the call site, so it is never double-freed.

**One row stays open under this heading, and it is not §10's.** An
`Array<Task<T>>` carries the obligation (RFC-0092 M4) but `drop ts` on one frees
the buffer and leaks the tasks in it, because a task's release is a wait and three
frees at a site holding the frame pointer, not a walk over bytes. The discharge
that works is `for t in consume ts`, joining each element, which is what the
diagnostic names. `Array<Stream<T>>` has had the identical hole since RFC-0075.

---

## 11. Failure paths

- **A trap ends the process.** No unwinding anywhere, so no drops run. Correct
  natively.
- **A `panic` a page catches leaves the instance alive.** The JS wrapper releases
  the argument buffers on both paths. Everything else the call allocated is
  orphaned while the module keeps running.
- **`return`, `break`, `continue` and `?` emit their drops.** The region stack is
  balanced on the same paths.

---

## 12. The allocators

| engine | allocator | reclaims |
|---|---|---|
| interpreter | Rust values | everything, always. **Cannot leak, so it is not evidence** |
| native | system `malloc`/`free` | as libc does |
| direct wasm | segregated free list, 116 classes, 8-byte header | per class only |

The wasm allocator uses four size steps per power of two, which caps the round-up
waste at 25%. Plain powers of two cost +90% on a leaking workload; four steps cost
+8.5%.

**Issue.** There is no coalescing and no splitting. A block is only ever reused at
its own class. A program with a phase change — many small allocations, then many
large — holds both peaks for its lifetime. Linear memory never shrinks. Both are
bounded rather than unbounded, and neither is measured.

Everything it refuses, it refuses **silently**: a pointer below `HEAP_BASE`, a
null, a header outside the class range. A wrong free is worse than no free, and
this backend has no sanitizer behind it.

---

## 13. What tests memory

`compiler/vyrn-cli/tests/memory.rs`, one assertion: memory after N exported calls
equals memory after 4N, read from `memory.buffer.byteLength` under Node through
the real `web/wasi-min.js`. It is a real gate — remove the release and it reports
5,242,880 against 20,709,376.

**How it should be handled.** The same steady-state assertion over a *set* of
shapes, and §2, §4, §9 and §10 above are the list: an `if`-expression result, a
global self-append, a record field overwrite, a returned String, a spawn. Each is
one exported entry point called N and 4N times. A leak makes the two numbers
differ; that is the whole harness, and it already exists.

---

## 14. A heap value inside a wrapper — the largest gap

`Option<T>`, `Result<T, E>` and `Validation<T>` are aggregates. `release_kind`
answers `None` for all three, and that answer is **correct**: an aggregate does not
own what it carries, and a row here would free a payload the producer still holds.

`transfers` answers `false` for `Expr::Match` and `Expr::Try`, and that answer is
**also correct**: neither form proves it hands over a fresh value.

Both halves are right and the result is that **nobody owns the String**:

```vyrn
fn pick(a: String, b: String) -> Option<String> {
    return Some(a + b)
}
```

The concat allocates. `pick` is not owned, because its return type is an `Option`.
The caller's `match` binds `s` to a borrow. **(measured: zero `free` in `main`.)**

The same holds for `?`, for `Result<String, E>`, for `Result<Array<T>, E>`, and for
every `Issue` string that RFC-0009's accumulating validation builds.

**This is the language's recommended style.** RFC-0005, RFC-0009 and RFC-0079 all
route failure through a wrapped value, so the idiomatic way to write a fallible
function that returns a String is also the leaking way.

**How it should be handled.** The aggregate needs a *payload* release rather than a
release of itself — the question `release_kind` cannot ask, because the answer
depends on which variant is live. That is a per-variant fact the enum already
carries at run time, and `CloseStream` is the precedent: a variant-aware release in
a runtime function, so the drop **site** stays straight-line. `transfers` then
answers `true` for a `Match` whose every arm transfers, which is decidable.

**Closed in two halves.** Phase 5 gave `Option` and `Result` a `Deep` release, so
`let o = pick(a, b)` reclaims. It did NOT close the row, because the recommended
spelling binds nothing: `if let Some(s) = pick(a, b)` matches a value with no name.

Phase 10a gave that statement the reclamation row a `let` gets, keyed by the
statement's own node address, with the arm binders bound to it. Every `return`,
store, capture and handover the move check already writes then lands on that row,
and a row with nothing written is a value the arms did not hand on. The release
runs on a drop frame of its own, so an arm that returns early releases it too.

The release is inline and per type, not the runtime function this section
predicted — Phase 5 settled that (see RFC-0089 "M3a as landed"), and the
`CloseStream` precedent did not survive contact.

Three holes were found, and all three are recorded at their sites in
`movecheck.rs`. The first two were found by writing the bug and reading the
emitted code; the third was found by CI, twenty-four runs later:

- **A binder handed to a lender.** `g = tagOf(j)` freed what the store had just
  kept, because a store of a CALL result records no move at all. A row whose
  value was passed to a function in the `lending` set is `Lent` now.
- **A borrow wrapped in a constructor.** `openRule(c)` is
  `for m in c.members { return Some(m) }`, and `returned_borrow` reads a returned
  PLACE — `Some(..)` is not one, so `openRule` was not a lender. `std/contract`
  read freed members and the `components` generator emitted a mangled spelling.
  **This is the shape Phase 5 recorded as the one nothing could see**, in its own
  words: "a struct literal is not one". The record is a lend and never a refusal,
  because refusing it would refuse `return Some(m)` over any loop element.
- **A scrutinee that is a parameter.** The row was minted on `place_key == 0`,
  and 0 answers two questions at once: an expression that names no place, and a
  place with no `let` row. A parameter is the second — it binds node 0 — so
  `if let Some(s) = v` over a plain `Option<String>` parameter released the
  caller's value. `showOpt` in `examples/argsdemo.vyrn` is exactly that, and the
  value was an `args()` element `opt` had lent it. The test is the one a `let`
  already asks before it is given a row: `names_a_place`.

  **Parity caught this and the local run did not**, for a reason worth keeping.
  Freeing a String returns the block whose first sixteen bytes are its
  `{ len, cap }` header. glibc writes the tcache link over exactly those bytes,
  so the next read of that String saw a pointer as its length; the Windows
  allocator leaves them alone, so every local run was clean and only the Linux
  job ever failed. The corrupt length is also why the symptom moved: `indexOf`
  scanned past the end and `substring` copied garbage, which reads as
  "byte offset 0 is inside a multi-byte UTF-8 character" — offset 0 is always a
  boundary, and the message came from `stringFromBytes` refusing the bytes, not
  from the boundary check. An earlier run of the same example died as an OOM
  instead, on the same corrupt length. **A use-after-free that only one
  allocator shows is still deterministic**, and reading it as flakiness cost
  twenty-four red runs.

---

## 15. Builtins that allocate and are not on the list

§2b is not hypothetical. `builtin_producers` has six entries. These allocate and
are absent:

| builtin | returns | status |
|---|---|---|
| `bytes(s)` | `Array<UInt8>` | **measured** — one allocation inside `__vyrn_str_bytes`, zero frees at the call site |
| `slice`, `stringFromBytes` | `Result<String, _>` | leaks through §14 as well |
| `readFile`, `readLine`, `readFileBytes` | `Result<..>` / `Option<..>` | same |

A routed builtin (`loader::routed_builtin`) resolves to a real std function, so the
fixpoint *does* see it — and then §14 discards it, because the return type is a
wrapper. Fixing §14 fixes most of this row.

`bytes` is the exception: it is an intrinsic with no declaration, so only the list
can answer.

---

## 16. Closure captures

A lambda's captured locals are packed into a heap block. From the lowering: *"The
block is never freed — the same safe leak every boxed enum payload already is."*

So a lambda evaluated in a loop allocates one block per iteration. RFC-0085 M4a
made a lazy field a stored closure, so a record with a lazy field carries one.

The leak is correct under RFC-0037: a defunctionalized closure can be stored and
run later, and nothing tracks when the last holder drops it. It is the same
question §14 asks about a variant payload, in a different aggregate.

**Closed by Phase 10b.** A `fn` value owns its capture block, so rule 1 moves it
and rule 4 releases it. The release is the three instructions the stream closer
already emitted at one site: read the payload word, test it against 0, `free`.

The copy rule 1 then demands is **derived over RFC-0037's defunctionalized
enum** — one `@__vyrn_fnval_copy` per module, a switch from tag to block size,
then one `malloc` and one `memcpy`. A copy SITE cannot measure that size, because
the size is a property of the tag and the tag is chosen at run time; the
defunctionalizer chose the tags and holds every one's capture types, so the
answer lives there. RFC-0091 M1's `Copy` protocol was named as the mechanism and
is not it: a `Copy` row is keyed by a type key and a `fn` type has none.

Both are **shallow** — the block, not what the captures point at. Two lambdas
over one String build two blocks holding one pointer, so a deep release would
free it twice. A captured String therefore still leaks, and `Gone::Captured`
already says why nothing else releases it either.

The corpus price was 22 sites, against the "the corpus copies them" the code
comment predicted: 16 take `consume`, 6 take `.copy()`. The 6 are the ones whose
source is `self.feed` or `r.run`, where an impl receiver cannot be declared
`consume` at all. `std/rpc`'s generated client takes `.copy()` for a different
reason: a client reuses one named callback across calls, and `consume` refuses
the second use. A named function's copy costs nothing — its payload is 0.

Writing the corpus fix found that `consume` did not parse in front of a
structural `fn` type: `parse_capability` required an identifier after the
keyword, so `run: consume fn() -> T` stopped at the `fn`. The convention was
unspellable for exactly the type this section is about.

---

## 17. The browser session amplifies every leak above

RFC-0067's soft navigation keeps the wasm instance **alive across navigations**. A
persistent island re-attaches its view with `mount(el)` and the same instance
serves every page for the whole session.

So a module leak is not per page load. It is per session, and it accumulates for as
long as the tab is open. A legacy island without `mount` is torn down and rebooted,
and JS reclaims the whole instance — so the *persistent* path, which is the
recommended one, is the one that accumulates.

This does not add a defect. It multiplies §4, §14, §15 and §16 by the length of a
user's visit.

---

## 18. Handled, and worth recording

So the following are not re-litigated:

- **Out of memory traps.** A `memory.grow` that fails returns -1; the loop tests
  and traps rather than spinning. The wording matches the native shim's byte for
  byte, because parity compares stderr.
- **Cell slot exhaustion traps.** The slab is 65536 and the bound is checked.
- **Region nesting past 64 traps**, and the region stack is balanced on `return`,
  `break`, `continue` and `?`.
- **The wasm allocator refuses a bad free silently** — below `HEAP_BASE`, null, or
  a header outside the class range. A wrong free is worse than no free.
- **A `push` frees its old buffer only after the element is stored.** Freeing at
  the growth broke `sha1.vyrn` from the seventeenth schedule word.

---

## The correctness gaps, ranked

The `outcome` column is the final state; the memory suite
(`compiler/vyrn-cli/tests/memory.rs`) is the authority behind every row that
names a test.

| # | gap | consequence | outcome |
|---|---|---|---|
| 1 | **§9b** an export may retain its String param | use-after-free | **CLOSED**, Phase 4b-2 — rule 2 refuses the store for every function, not only an exported one |
| 2 | **§14** a heap value inside `Option`/`Result`/`Validation` | unbounded leak **in the recommended style** | **CLOSED.** Phase 5 makes an `Option`/`Result` release its payload; Phase 10a gives the `if let` STATEMENT the reclamation row, so the unbound scrutinee is released too. `optionString` is steady |
| 3 | **§4** an overwrite never releases the old value | unbounded leak in the server shape | **CLOSED**, Phase 5 — both rows steady, and P1 with them |
| 4 | **§13** one memory test, one shape | every gap here is invisible | **CLOSED**, Phase 1 — twelve rows, and every phase since flipped one or failed |
| 5 | **§9a** a returned String leaks | unbounded, per call | **CLOSED**, Phase 6 — rule 3 makes a return owned, and RFC-0012 M3 makes the page know its type |
| 6 | **§7** linearity is hardcoded to `Stream` | a user resource gets no obligation | **OPEN.** RFC-0086 M3, untouched by this arc. The `must-use` row exists (Phase 4b) and only the compiler may write it |
| 7 | **§2a** six expression forms never transfer | leak | **CLOSED**, Phase 4c — the expression's form stopped deciding; the type decides |
| 8 | **§15** `bytes` and friends are not producers | leak | **CLOSED**, Phase 4c — same deletion; `views` is the remaining hand-written list, and it is the opposite direction |
| 9 | **§16** a closure capture block is never freed | leak per lambda evaluation | **CLOSED.** Phase 10b: a `fn` value owns its capture block, and the copy rule 1 demands is derived over RFC-0037's defunctionalized enum. `lambdaLoop` is steady |
| 10 | **§10** a spawn frame is never freed | bounded leak | **CLOSED**, RFC-0095 M1 — a `Task<T>` is linear, so `join` consumes it and `drop t` waits and discharges it, and the frame, the record and the operating-system handle go back at that one site. 200,000 spawns: 20,252 KiB and 200,076 handles before, 3,880 KiB and 72 handles after |
| 11 | **§5** regions are hand-placed | the model's best tool is rarely reachable | **OPEN.** RFC-0004 Q3, still undesigned. The arena survived Path B's deletion as Path A's second half |
| 12 | **§6** use-after-release traps at run time | not compile-time | **STRUCK.** Path B is deleted (RFC-0090 M4); there is no `release` to use after |

1 is a soundness defect and everything else is a leak. **§17 multiplies 2, 3, 8 and
9 by the length of a browser session**, because the instance survives navigation.
Of the four, 3 and 8 are closed, 2 is half, and 9 is the open tail.

Read 2, 9 and 14 together. All three are the same question — *who releases a heap
value held inside an aggregate the compiler copies* — asked about an enum payload, a
closure block and a `Some(..)`. One answer closes all three, and the language
already has it in one place: `Stream`'s release is variant-aware and lives in a
runtime function, so the drop site stays straight-line.

4 is what makes 2, 3, 5, 7, 8, 9 and 10 visible, and it is the cheapest item here.

---
---

# Part II — Clean and usability gaps

Part I asks whether memory is reclaimed. Part II asks whether a person can see the
model, learn it, and write against it. These are separate failures. A leak you can
see is a bug. A leak you cannot see is a design problem.

---

## U1. The model is invisible, and that is the root gap

Three bindings, one shape, opposite outcomes:

```vyrn
let s = a + b                      // freed at block exit
let mut s = a + b                  // leaks — a mut String is not tracked
let s = if c { a + b } else { a }  // leaks — an if-expression does not transfer
```

Nothing in the source says which. There is no diagnostic, no hover, no inlay hint
and no lens. `vyrn why` answers about audience and imports, not memory. The LSP
never reads `own::Ownership`.

**The compiler holds the exact answer.** `Ownership.droppable` maps each binding to
its release kind, and `owned_fns` says which functions transfer. Both are computed
for every build and shown to nobody.

Every gap below is worse for this one. A user cannot learn a rule they cannot
observe, so they never build the instinct the model needs.

**How it should be handled.** `vyrn why --memory <file>`, printing per binding
whether it is reclaimed and the reason it is not — aliased, escaped into a call,
`mut`, inside a region, or a type with no release. Then the same text as an LSP
hover. The analysis exists. This is a printer.

**CLOSED, in two halves.** Phase 1 built `vyrn why --memory`: per binding,
reclaimed or not, the release kind, and the reason when it is not. Phase 9 put
the same answer in the editor, off the same table:

- **hover** on a binding appends `memory: …` — reclaimed and how, moved and
  where, reclaimed by `drop`, static, or the reason it is not reclaimed
- **a token modifier** on the occurrence that takes the value, so the point where
  a value stops being live is coloured rather than worked out by reading
- **an inlay hint** at every move, naming where the value went

One table feeds all three, and `Fate::words` is the one wording — the shell and
the editor cannot say different things about the same binding, because they read
the same function. A binding whose type owns no heap gets no note: "NOT
reclaimed" is the wrong sentence about an `Int64`.

The answer is computed only when the checks ran and found nothing, which is what
keeps it off the keystroke path for the buffers that need it least. Measured:
`examples/graphql.vyrn` went 48.5 ms to 53.1 ms against RFC-0084's 97 ms budget.

The three lines this section opens with now read differently in the editor, and
two of them stopped being true on the way: since Phase 4c a `mut` String IS
tracked and an if-expression DOES transfer, because reclamation follows the type
and the expression's form stopped deciding. The gap was never only that the model
was invisible — it was that what you could not see was also wrong.

---

## U2. There is no way to copy

§9b says an exported function may not retain a borrowed String. The fix is to copy
it, and the language has no spelling for a copy. So the corpus writes this:

```vyrn
export extern fn onType(arg: String) {
    typed = arg + ""
}
```

`arg + ""` is a concatenation used for its allocation. It works because
`emit_str_concat` always allocates, which is an implementation fact rather than a
promise.

It is also **§4 in the same line**: every keystroke leaks the previous `typed`. The
only available fix for a use-after-free introduces an unbounded leak, and neither
is visible.

**How it should be handled.** A `copy` that means it. Then §9b's rule gets a
diagnostic that names the way out.

**Closed by RFC-0089 M1b, and the corpus by M3b.** `x.copy()` ships in all three
engines — structural, recursive, and refused on a type that declares its own
`Owned` row. See "M1b as landed" in RFC-0089 for the four decisions it needed. The
five `arg + ""` sites are `.copy()` as of Phase 6, and the diagnostic that refuses
the store names it.

---

## U3. ~~Path B never joined the subject-first surface~~ — CLOSED by deletion

`cell`, `get`, `set` and `release` were free functions in a language whose
surface redesign had moved everything else to `sq.push(x)`, `sq[j]`, `sq.length`
and `x.toString()`. Path B was in neither sweep.

**RFC-0090 M4 deleted all four names, and its replacement is subject-first by
construction**: `s[h]` and `s[h] = v` through `Index`, `for x in s` through
`Iterate`, `s.copy()` through `Copy`. The two that are not — `insert(s, v)` and
`remove(s, h)` — are free functions for a stated reason rather than an oversight:
an impl method's receiver cannot be `modify` (RFC-0091, "The generic-container
correction"). That is a live gap in the protocol surface, not in the memory
model, and it is recorded where it belongs.

The four names are a user's again. `fn get(..)` compiles.

---

## U4. `drop` is whole-container, so an element is unreclaimable

`drop x` releases a container. RFC-0011 states that elements are a safe leak. Put a
String in an array and **no mechanism in the language reclaims it** — not the
analysis, not `drop`, not a region, not a manual call. The value is unreachable and
permanent.

This is the largest silent restriction in the model, and it appears in no
diagnostic and no document.

**Half closed, Phase 8b.** A container that DECLARES what it owns reaches its
elements. `std/slots` says `impl<T> Owned for Slots<T>` and its release walks
the table and gives each payload back, because it knows every slot holds one
payload it owns — `insert` takes `consume T`, and the only other writer is a
store, which releases what it replaced. `drop v` where `v: T` is legal and the
monomorphized instance decides: a `free` for a `Slots<String>`, no instruction
at all for a `Slots<Int64>`.

**The other half stays open, and the restriction is now the right one.** A
built-in `Array<T>` releases no element and must not. An array cannot say
whether it owns its elements or views somebody else's, and `m.keys()` is the
view: a fresh buffer holding the map's own key pointers, which a per-element
release would free twice. So the answer moved from "no mechanism exists" to
"the mechanism is a declaration, and a built-in container has nothing to
declare it with". The memory suite holds both halves — `elementLeak` leaking on
a bare `Array<String>`, `slotsContainer` steady on the same String in a `Slots`.

**RFC-0092 corrects that second sentence.** A built-in row is SEEDED, not
declared, so what `Array<T>` lacks is not a declaration site but the proof that
its elements are owned. Three builtins break that proof and `m.keys()` is one of
them. A declaration stays the mechanism for a container with a liveness rule of
its own, which is what `Slots` has and `Array` does not.

---

## U5. The one runtime failure has no location — CLOSED

The message was `error: reference used after release`: no line, no binding name,
no origin, emitted from hand-written IR that no source position reached. RFC-0006
and RFC-0009 make diagnostics a stated feature of this language, and the memory
model's only failure message was below that bar.

**RFC-0090 M4 deleted that message with the mechanism. The replacement is better
in one way and no better in the other, and it is worth being exact about which.**
A dead handle now hits `panic("slots: handle is not alive")` written in
`std/slots.vyrn`. What improved: the wording belongs to a library author who can
change it, and it names the container rather than a slab nothing could name.

**What did NOT improve: there is still no line.** `panic` lowers to
`@.panic.fmt`, which is `error: %s`, and it carries no source position on any
engine. The gap moved from "the compiler's only memory failure has no location"
to "`panic` has no location", which is a smaller and more general problem than
the one this census opened with, and it belongs to RFC-0079 rather than here.

**This entry was written the other way first, and the check caught it.** Phase 8e
claimed `panic` carried a location because it is a language construct at a source
site. Running `examples/slots.vyrn` printed `error: slots: handle is not alive`
and nothing else. A claim about a diagnostic is worth running.

### Closed after the arc

`examples/slots.vyrn` now ends in
`error: slots: handle is not alive (std/slots.vyrn:189)`, on the interpreter, on
the native binary and on wasm, byte for byte.

**The loader stamps the site.** `panic(msg)` becomes `@panicAt(msg,
"file:line")` as a module is entered. That pass is the only one that knows both
halves: the parser knows the line and not the file, and every stage after the
loader knows neither, because a `place` projection is cloned into its access site
and a generic is cloned per instantiation. The site then travels as an ordinary
string literal in the argument list, which is why the three engines agree — they
read one fact rather than each deriving one — and why wasm can carry a location
at all, having no filesystem at run time.

**The site is where the `panic` is WRITTEN, including in a library.** A dead
handle reports `std/slots.vyrn:189` and not the `people[h]` that reached it. The
caller's line is reachable: `project::inline` receives the access-site line, and
a projection is inlined into the caller. It is not taken because it would be
uniform for projections and for nothing else — an ordinary library function is
not inlined and has no access site to name — and one construct would then mean
two things depending on how its callee happened to be written. RFC-0079 records
this as the open question it now is.

**Measured, because Phase 8d made trap-site size a stated property.** Corpus
wasm +0.59%, corpus text IR +0.33%, a native hello-world unchanged. The wasm site
costs nothing in code: the site is fused into the constant the shared `trap`
already receives, so `"\n"` becomes `" (std/slots.vyrn:189)\n"` and the three
calls stay three calls.

---

## U6. The arena excludes the case it is for

`region { }` is a block. `push` cannot draw from it, because arena memory has no
`realloc`. So the loop that builds many values with one lifetime — the case
RFC-0004 §4 names as the arena's reason to exist — is the case the arena cannot
serve.

What is left is String concatenation inside a lexical block. RFC-0004 Q3 asked for
inferred, invisible regions and nothing was designed.

---

## U7. `consume` and linearity look like one mechanism and are two — CLOSED

A user writes `consume` and gets move checking: use the value twice and the
compiler refuses. They get **no release obligation** — nothing requires them to
dispose of it and nothing frees it.

A `Stream<T>` gets the obligation and a user cannot declare one.

Read together, the surface says "declare intent with a capability", and then the
one capability that means ownership does half the job.

**The second half is closed by RFC-0086 M3.** `impl MustUse for T` gives a
user's type what `Stream<T>` had, out of the same table `impl Owned` feeds:
abandoned, disposed twice, and branches that disagree are all refused, a
must-use parameter carries the obligation into the callee, and the diagnostics
name the user's type. `examples/mustuse_abandoned.vyrn` pins all three.

**And the undecided question is decided: they stay two declarations.** They
answer different questions.

- `consume` is a **calling convention**. It says who owns this argument after
  the call, and it is a property of one parameter position.
- Must-use is an **obligation on a type**. It says this value must be disposed
  of by name, wherever it goes, whoever holds it.

A `String` is consumable and carries no obligation — nothing requires you to
name a `String` you were handed, and nothing should. A `Stream` carries an
obligation however any particular function takes it. Merging them would make
every `consume` parameter linear and every linear value a calling convention,
and neither implication is true. `examples/mustuse.vyrn` shows both doing their
separate work in one function: `consume` is why `drop t` is legal there, and the
obligation is why leaving without it is an error.

**What is still open is narrower than the gap was**, and it is not this
question. A declared must-use value stored into a record field or an array
counts as discharged, because a store names it; the obligation stops rather than
travelling with the container. `Stream` has no such hole because RFC-0075
forbids storing one at all — a **representation** rule, kept for streams and
deliberately not widened. Closing it needs the obligation to travel through a
place, which is the mechanism U4 and RFC-0091's projections also want.

---

## U8. You can declare a container you cannot read

RFC-0086 M1 shipped `impl Owned for T`, and `examples/ownedcontainer.vyrn` proves a
third party joins the mechanism with no compiler change.

Then: **any call escapes its receiver.** A method that reads the container removes
it from `droppable`, so the declared `release` never runs. The example observes its
container *through* `release`, because that is the only point where the container
is both alive and finished with.

A container you may build and may not use is not yet a feature. The rule applies to
built-ins too, so it is not a new asymmetry — but a built-in has an escape hatch
(`at`, `alen`, `@has` are on the safe-read list) and a user type has none.

**How it should be handled.** The safe-read list is a hand-written list, and
RFC-0086's rule applies to it. A method should declare that it reads its receiver
and does not retain it. That is one capability — `read` — which RFC-0004 §1 already
defines and which is surface-only today.

---

## U9. Three layouts, one syntax

```vyrn
let a = [1, 2, 3]                        // ArrayN — N words, inline, on the stack
let b: Array<Int64> = [1, 2, 3]          // three words around a heap buffer
let c: SmallArray<Int64, 4> = [1, 2, 3]  // inline until it spills
```

The annotation decides stack against heap, and nothing else does. This already
corrupted a heap inside the compiler. For a user it means one line means three
things, with three costs and three failure modes.

Each layout is justified. What is missing is that nothing tells you which one a
line got — U1 again.

---

## U10. The extern boundary cannot be declared

A user writes:

```vyrn
export extern fn greet(name: String) -> String
```

There is no way to say, and no way to find out, who owns either String. The
parameter rule is enforced by nothing (§9b). The return rule is decided by an
analysis with no surface (§9a). Both facts exist inside the compiler.

This is U1 at the one place where the answer is not merely useful but required,
because the other side of the boundary is a different language.

**Closed by RFC-0089 M3b, by removing the question.** Both answers are now fixed
by the rules rather than by an analysis: the caller owns the parameter (so an
export may not `consume` one, and may not store one without `.copy()`), and the
caller owns the return (so an export may not lend one). A reader does not have to
find out — there is only one answer, and the compiler says so when a program tries
the other.

**The type hint is closed too, by RFC-0012 M3 (Phase 9).** It was the last
convention at this boundary: `exportReturns` was hand-written per page, so an
export whose String return nobody named came back as a number — and since M3b the
same hint also decided the `free`, so a missed name leaked as well. A module now
carries a `vyrn:exports` custom section naming every `String`/`Bool` result, and
`wasi-min.js` reads it in the section walk it already ran. Five hand-written maps
are deleted. Nothing at this boundary is a convention now: the ownership is a
rule, and the type is in the module.

---

## The usability gaps, ranked

| # | gap | why it ranks here | outcome |
|---|---|---|---|
| 1 | **U1** the model is invisible | every other gap compounds; a user cannot learn it | **CLOSED.** `vyrn why --memory` (Phase 1), then hover + last-use modifier + move inlay hints (Phase 9), all off one table |
| 2 | **U2** no way to copy | the only fix for a use-after-free is a trick that leaks | **CLOSED.** `x.copy()` (RFC-0089 M1b), a protocol since RFC-0091 M1, named by every refusal, and applied by `vyrn fix` (Phase 9) |
| 3 | **U10** the extern boundary cannot be declared | the one place the answer is required | **CLOSED.** Ownership by rule (RFC-0089 M3b), type by the `vyrn:exports` section (RFC-0012 M3) |
| 4 | **U8** a declared container cannot be read | `impl Owned` is shipped and not yet usable | **CLOSED in practice.** Phase 4c deleted the escape walker that removed a read receiver from `droppable`, and `std/slots` is a declared container the corpus reads on every call |
| 5 | **U5** the failure message has no location | the worst moment to say the least | **CLOSED.** `error: slots: handle is not alive (std/slots.vyrn:189)` on all three engines. The loader stamps `panic(msg)` into `@panicAt(msg, "file:line")`, so the site travels as data in the AST |
| 6 | **U4** an element is unreclaimable | the largest silent restriction | **HALF, and the other half is correct.** A DECLARED container reaches its elements (Phase 8b); a built-in `Array<T>` cannot say whether it owns them, and `m.keys()` is the view a per-element release would free twice |
| 7 | **U7** `consume` and linearity are two things | the surface implies one | **CLOSED.** `impl MustUse for T` (RFC-0086 M3) gives a user's type the whole obligation, and the question is decided: they stay two declarations, because a calling convention and an obligation on a type are not the same fact |
| 8 | **U6** the arena excludes its own case | the model's best tool is unreachable | **OPEN.** RFC-0004 Q3, undesigned. Path B went; Path A's arena stayed |
| 9 | **U9** three layouts, one syntax | justified, but unannounced | **COVERED by U1** — hover names the layout, because it names the type |

**U3 left this table entirely.** "Path B is not subject-first" was rank 7; the
four names it was about are deleted (RFC-0090 M4) and the replacement is
subject-first by construction. Ten gaps, nine now.

**U5 did NOT leave with it, and then it did.** The plan said U3 and U5 both went
with Path B in Phase 8e. U3 did. U5 was narrowed: the message it named was gone,
and the missing line was still missing, one level down. Phase 8e checked by
running `examples/slots.vyrn` after a first draft claimed otherwise, and the
output was `error: slots: handle is not alive` and nothing else. It closed after
the arc, on its own, in the entry below.

**U1 and U2 change how the language feels.** U1 costs a printer over data that
already exists. U2 costs one builtin. Neither needs a new analysis, and between
them they turn the memory model from something a user guesses at into something
they can read and act on.

The two halves meet at one place. §13 says the compiler cannot see its own leaks;
U1 says the user cannot either. Both are asking `own::Ownership` to be output
rather than an internal fact — once as a test assertion, once as a printed line.

---
---

# Part III — Performance and memory usage

Part I asks whether memory is reclaimed. Part II asks whether a person can see the
model. Part III asks what it costs.

---

## P1. The same three lines are linear or quadratic, and nothing says which

Two loops. Identical source. One is a local, one is module state.

```vyrn
let mut s = ""            //  or:  g = ""
while i < n {
    s = s + "x"           //        g = g + "x"
    i = i + 1
}
```

| n = 160 000 | wall clock | peak working set |
|---|---|---|
| local `let mut s` | **0.095 s** | one buffer, grown in place |
| module state `g` | **4.92 s** | **12.2 GB** |

**(measured, native, same machine, same run.)** At n = 40 000 the gap is 0.111 s
against 0.400 s; four times the input costs the local build nothing measurable and
the global build **12.3×**. That is the quadratic.

The lowerings say why. RFC-0081's `str_append` claims the local: a shadow length
and capacity, `realloc` on growth, and a `memcpy` of only the new bytes — amortized
O(1). The global gets the general path: `strlen` on the old string, `strlen` on the
addend, `malloc`, `strcpy`, `strcat`. Three scans and a fresh buffer per iteration,
and the old buffer is dropped on the floor (§4).

**12.2 GB to build a 160 KB string.** The leak is not a footnote to the slowdown.
It *is* most of the slowdown, because every iteration touches cold pages.

`str_append` applies to a `let`-declared local that `append_candidates` cleared. A
module-state accumulator does not qualify, and a server accumulating a response
body is exactly a module-state accumulator.

---

## P2. A String has no length header

A Vyrn String is a bare NUL-terminated pointer. So:

- `s.byteLength` is a scan. RFC-0058 renamed it from `length` to say it counts
  bytes; it did not make it O(1).
- `a + b` scans both operands before it allocates.
- A drop site can recover a String's *length* and never its *capacity*, which is
  the reason RFC-0077 M6's allocator carries an 8-byte header at all. A headerless
  free sized from `strlen` would file a 1024-byte block on the 128-byte list.

`str_append` keeps a shadow length and capacity beside the eligible local, which is
the header, kept somewhere else, for one case. P1 is what the other cases cost.

**Closed by RFC-0089 M1a.** A String now carries `{len, cap}` in the two words in
front of its pointer. `byteLength` of a 10 KB string fell from 289 ns to 10 ns per
read, `a + b` from 29.1 to 19.0 µs per thousand, and the shadow became the header.
The pointer is still one word and still NUL-terminated, so nothing at the extern
boundary changed.

---

## P3. Every leak in Part I is also a performance defect

The measurement in P1 is a Part I leak (§4) presenting as a 52× slowdown. The same
applies to the rest:

- Before RFC-0077 M6, `domdemo.wasm` went from 3 pages to 279 over 20 000
  keystrokes. After: 4 to 4.
- §14 leaks a String per fallible call, and the recommended style is fallible.
- §17 means none of it resets at a page navigation.

A leak in a batch program is a footnote. A leak in the shapes Vyrn targets — a
server, an SPA island, an event loop — is the performance story.

---

## P4. The allocator is bounded, not tight

The wasm allocator is a segregated free list with four size steps per power of two,
which caps round-up waste at 25%. Plain powers of two cost +90% on a leaking
workload against +8.5% for four steps, so the class count was priced and chosen.

What it does not do:

- **No coalescing and no splitting.** A block is only ever reused at its own class.
  A program with a phase change — many small allocations, then many large — holds
  both peaks for its lifetime.
- **Linear memory never shrinks.** `memory.grow` only goes up.
- **8 bytes of header per block**, which is large against a short String.

All three are bounded rather than unbounded, and none is measured.

---

## P5. The generation check

RFC-0004 §5.1 measured the check free in steady state. §5.3 then measured how many
are necessary: **3 of 48 lowered sites elide, so 94% survive**. Each survivor is a
load, a compare and a branch on the hot path of a `get`.

The 94% is the corpus rather than the analysis — a `Ref<T>` is the aliasing tool,
so code reaches for one exactly where ownership cannot follow the value. But "free
in steady state" was measured once, at whole-program scale. Nothing measures the
check inside a tight loop over a `Ref`, which is where it would show.

---

## P6. The two backends reserve the cell slab differently

| backend | slab | cost |
|---|---|---|
| textual / LLVM | four static `[65536]` arrays in the IR prelude, unconditional | BSS in every binary; 36 references in a hello-world's `.ll` |
| direct wasm | **one lazy `malloc`** on the first `cell` | twelve bytes until used, and the comment says why: *"statically reserving 1 MiB would put a megabyte of zeroes in every module this backend emits, including `fib`"* |

The direct backend is right and the LLVM one predates the reasoning. BSS is
lazily faulted, so the native cost is image size rather than resident memory — but
it is an asymmetry with a known better answer already implemented next door.

**One stale comment found here.** `direct.rs`'s `cell_runtime` still says *"The
payload is NOT freed on release. This backend's allocator is a bump pointer."*
RFC-0077 M6 changed both facts — the allocator is a free list and the release does
call `free` on the payload, two lines below `cell_addr`. The code is correct; the
comment describes the version before it.

---

## P7. Correct, and recorded so it is not re-derived

- **Array growth doubles** — `0 → 4`, then `× 2`. Amortized O(1) per `push`, and the
  old buffer goes back to the allocator, so growth costs the two backends the same
  heap.
- **The doubling multiply is 64-bit before the allocation.** Wrapping first was the
  worst of the truncations: doubling a 2 GiB buffer asks for 4 GiB, wraps to 0, and
  the copy is sized from the *old* length, which fits — 2 GiB into a zero-byte
  block with no bounds check to trip.
- **Defunctionalized closures make no indirect calls.** The wasm table is identical
  to the pre-closure version.

---

## P8. Nothing benchmarks any of this

`vyrn bench` exists (RFC-0055) and CI runs benchmarks informationally (RFC-0063).
The corpus is `examples/simdbench.vyrn` — all SIMD — and `examples/benching.vyrn`,
which has `"hash to 1000"` and `"push 1000"`.

**No benchmark measures an allocation, a String concatenation, or a generation
check.** The three things the memory model costs are the three it does not time.

P1 was found by writing eight lines and running them twice.

---

## The performance gaps, ranked

| # | gap | measured | outcome |
|---|---|---|---|
| 1 | **P1** a module-state accumulator is quadratic and leaks | **52× slower, 12.2 GB for a 160 KB string** | **CLOSED**, Phase 5 — 5 ms and 4.6 MB. The in-place append whitelist reads the whole program, so module state is the same program as the local |
| 2 | **P3** Part I's leaks are the performance story | 3 → 279 pages, and P1 | **CLOSED for the rows that closed** — nine of twelve memory rows are steady |
| 3 | **P2** a String has no length header | every `byteLength` and every concat scans | **CLOSED**, Phase 2 — `{len, cap}` behind the pointer, so a String is still one word |
| 4 | **P8** nothing benchmarks the model | — | **CLOSED**, Phase 1 — `examples/membench.vyrn`, which every phase since has read |
| 5 | **P4** no coalescing, no splitting, no shrink | — | **OPEN.** Still unmeasured, and still the right order: measure first |
| 6 | **P6** the LLVM slab is static | BSS in every binary | **STRUCK.** Both slabs are deleted (RFC-0090 M4). The LLVM one cost nothing anyway — its four arrays were `zeroinitializer`, so the linker dropped them |
| 7 | **P5** 94% of generation checks survive | 3 of 48 elided | **STRUCK.** There is no generation check in any engine now, except in `std/slots.vyrn`, which is Vyrn. Phase 8d measured the elision three ways and found it had no customer |

**P1 and P8 are the pair.** P1 is a 52× cliff with an invisible trigger, and P8 is
why nobody found it. Four of the seven rows above would be numbers rather than
question marks if the fourth were done first.

The three parts converge on one sentence. **§13**: the compiler cannot see its own
leaks. **U1**: the user cannot see whether a value is reclaimed. **P8**: nothing
measures what any of it costs. Three faces of the same absence, and the data for
the first two already exists inside `own::Ownership`.

**All three closed first, and in that order** — Phase 1 built the memory suite,
the benchmark and `vyrn why --memory` in one PR, before any semantic change. Every
phase after it was decided by a number one of the three produced. That was the
plan's gate and it held: nothing in this arc was argued from reading alone.

---
---

# The census, closed

Nine phases, RFC-0089 + RFC-0090 + RFC-0091. What the three tables above came to.

## The score

- **Correctness (12 rows):** 8 closed after Phase 10 (6 at Phase 9), 1 struck,
  3 open — and 2 of the 3 are undesigned design questions (§5 regions, §7
  linearity as a declaration) rather than defects. §14's half became whole and
  §16 closed.
- **Usability (10 rows, 9 after U3 left):** 4 closed, 1 closed in practice, 1
  covered, 1 half, 1 narrowed, 2 open.
- **Performance (7 rows):** 4 closed, 2 struck, 1 open and correctly unmeasured.

The memory suite is the ledger: **eleven of twelve rows steady, one leaking**
after Phase 10 (nine and three when Phase 9 closed the census). The one leaking
row asserts that it leaks, so the day it stops is a test failure and not a
silence.

## What the arc found that the census did not name

Every phase corrected the brief that launched it. The load-bearing ones:

- **A three-word String would have moved a row this arc was required not to
  move.** M1a intended `{ptr, len, cap}`; an `Option` payload is one word, so it
  would have boxed, and `optionString` was already leaking. The header went behind
  the pointer instead.
- **Rule 2's real cost was measured per-file and was wrong.** 288 sites became 207
  linked, and 137 of the 207 needed no copy at all — 91 of them were rule 2 being
  wrong about a temporary. The number that looked like the design's price was
  mostly the measurement's error.
- **A self-referring type had no legal way out of a container.** Between M1b's
  `copy` refusal and rule 2's store refusal, `Json` and `VyxNode` were stuck;
  `for x in consume xs` is what unblocked them, and it was not in the design.
- **The initializedness fact the plan asked for does not exist to need.** Every
  place is initialized before it can be stored over, except a map key, which
  decides at run time. What a store has to know is who OWNS what is there.
- **Inlining a projection is free in instructions and not in node identity.** 118
  `.ll` files and 119 `.wasm` modules came out byte-identical, and the bug that
  found it was two inlines in one block sharing a binding name.
- **A guard is not what a guard costs.** Phase 8c blamed a 2.5x on a missing
  elision pass. Phase 8d found the real cause — every trap emitted three calls
  inline, 14,935 sites over the corpus, which made the function around them too
  expensive for LLVM to inline — and recovered the cost without touching the
  guard. Then it measured the elision three ways and found it had no customer.
- **A deletion is wider than the thing deleted.** Path B was estimated at 570–770
  lines and came out 1,714 across 24 files: the `Type::Ref` arms in fourteen
  files, the emission sites in two backends, 22 unit tests that existed to hold
  the mechanism up, one parity pin and four census rows.
- **The runtime surface is four primitives, not three.** malloc, **realloc**, free
  and memcpy. `realloc` is how an `Array` and a `String` grow in place, and the
  thesis never named it.
- **A release is only as safe as the lend record behind it.** Phase 10a released
  an `if let` scrutinee and produced a live use-after-free three times before it
  held: once because a store of a CALL result records no move, once because
  `returned_borrow` reads a returned PLACE and `Some(m)` is not one, and once
  because `place_key` answers 0 for a parameter as well as for a temporary. The
  first two were found by reading emitted code, not by parity — parity was
  byte-identical with the second bug in it. The third shipped, and CI's Linux
  parity job found it the same day; see §14 for why the local Windows run could
  not.

## The open tail

**Phase 10 closed two of the three.** `optionString` (§14) and `lambdaLoop` (§16)
are steady; what each took is written in its own section above, and both are
smaller than the tail predicted. One row is left, and it is a restriction rather
than a defect.

The two closed rows both corrected the entry that named them. §14 was recorded as
needing "arm-payload escape tracking" and the same widening that stopped deep
drop for records; what it actually needed was for the `if let` STATEMENT to carry
the reclamation row, after which the escape tracking that already exists writes
itself onto it. §16 was recorded as needing a copy derived over the
defunctionalized enum, which is exactly what it needed — and the rule-1 price that
comment predicted ("the corpus copies them") was 22 sites, most of them
`consume`.

**What did NOT close, and is still the wider gap.** A record, a user enum and a
fixed array still release nothing. Phase 10a closed one face of the projection
lend — a borrow wrapped in a constructor or a struct literal is recorded now — but
recording is not refusing, and `release_kind` cannot add a row for a record until
a returned projection is either refused or tracked through a store and through a
container. Phase 4b's reason for not refusing has been re-priced and is weaker
than it was: RFC-0091 M1 gave a self-referring type `impl Copy`, so `.copy()` on a
`Json` is writable now. What is not priced is the corpus cost of demanding it, and
that measurement is the next phase's first job.

**RFC-0092 designs that refusal and makes the measurement its gate.** It reads
this row, U4 and RFC-0086 M3's storage hole as one chain, and it reports that the
compiler already refuses a projection stored through a `let` and admits the same
projection stored inline.

**`elementLeak` (U4) — correctly stays.** A built-in `Array<T>` releases no
element and must not. An array cannot say whether it owns its elements or views
somebody else's, and `m.keys()` is the view: a fresh buffer holding the map's own
key pointers, which a per-element release would free twice. The answer moved from
"no mechanism exists" to "the mechanism is a declaration, and a built-in container
has nothing to declare it with" — and `slotsContainer` is the row that proves the
declaration works. This is a restriction, recorded, not a defect to chase.

Beside the three rows, **one** design question is open and undesigned: **§5/U6**
inferred regions (RFC-0004 Q3). **§10** the native spawn frame was the second and
is closed by RFC-0095 M1 — see below, where the measurement that kept it open now
carries its after column. **§7/U7** was the third and is closed: linearity is a
declaration (RFC-0086 M3), and `consume` and the obligation stay two declarations
because a calling convention and a property of a type are not the same fact.
**U5** was the fourth and is closed too: `panic` names the file and line it is
written at, on all three engines.

### §10, measured — and closed by RFC-0095 M1

A loop of `let t = spawn work(i)` then `t.join()`, native, peak working set:
1000 spawns 4392 KiB, 50000 spawns 8332 KiB, 200000 spawns 20120 KiB. That is
**81 bytes per spawn**, linear, and the Windows handle count rises by one per
spawn as well (19946 handles at 20000 spawns): the task record holds an event
object that is created per task and closed never.

The frame is about 32 of those 81 bytes. It cannot be freed at the first join.
`__vyrn_join` hands the frame POINTER back and the caller loads the result off
it, and `t.join()` twice is a legal program on all three engines — verified,
both joins answer 42. A flag and a free at the first join gives the second join
a dangling read.

Two shapes were considered and neither closes the row:

* **Copy the result into the task record at completion, then free the frame.**
  Sound, and idempotent forever, but it needs the result's size at the shim's
  ABI and it reclaims 32 of 81 bytes. The record itself still cannot be freed,
  so the growth stays linear — a smaller constant, an unchanged order, and a
  runtime the reader has to hold two lifetimes for.
* **Free at the last join.** This is the one that works, and it is the design
  change: it needs to know that no further join can happen, which is ownership
  of the `Task<T>` value.

**RFC-0095 M1 took the second one and the row is closed.** A `Task<T>` is linear,
so "free at the last join" is "free at *the* join": `t.join()` consumes the task
and a second one is a compile error, `drop t` waits and releases the result by its
type, and `drop t` is accepted where it used to be refused. Re-measured on the
same loop: 3,880 KiB and 72 handles at 200,000 spawns, against 20,252 KiB and
200,076 before — flat in both columns rather than linear in either.

Two facts the earlier reading had wrong, both found by building it:

* **The corpus DID join one task twice.** `examples/parallel.vyrn` said so in a
  comment — "join is idempotent — a second join re-reads the settled result" —
  and this section verified it. It reads each result once now.
* **`examples/branchtypes.vyrn` could not be fixed with a `drop`.** Its `None`
  arm leaked a handle, and `drop` is a statement while a `match` arm is an
  expression. Both arms join instead.

## What the model is now, in one paragraph

Five strategies, not six. A value is a value; a capability is a calling
convention; a function returns something the caller owns; a place owns its
contents. Reclamation follows the TYPE, and the expression's form decides nothing.
A store releases what it replaced. A copy is written `.copy()` and is never
implicit. A container declares what it owns and the compiler asks it. The runtime
underneath is malloc, realloc, free and memcpy, with a region arena beside it for
a block-scoped lifetime. Nothing allocates from a fixed table and nothing checks a
generation counter, except `std/slots.vyrn`, which is Vyrn and which any reader can
open.
