# RFC-0087 — Every Memory Scenario, and What Handles It

- **Status:** Census. No milestone implemented. Part I is correctness — is memory
  reclaimed. Part II is usability — can a person see the model. Part III is cost —
  what does it spend. The three ranked tables are the proposed work.
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
| slab + generation (Path B) | `release` | explicit, or block exit |
| linear obligation | `movecheck` | compile time; the program must say where |

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

## 6. Generational references — Path B

`cell(v)` takes a slot from a fixed slab of 65536. A `Ref<T>` is `{slot, gen}`.
Every `get`/`set` compares its generation against the slot's. `release` bumps the
slot's generation and returns the slot to a free stack, so a stale reference fails
the comparison.

- Slot exhaustion traps. The bound is checked (`%oob` against 65536).
- **6% of lowered check sites are elided** (RFC-0004 §5.3): 3 of 48 across the
  corpus, all in one example. The other 45 are references that genuinely escape.
- A surviving check is now a signal — it marks a reference the compiler could not
  follow.

**Issue, and it is the known one.** A use-after-release is a **run-time trap**, not
a compile error. That is the price RFC-0004 §5.2 accepted for aliasing with no
annotations. The 94% number says the corpus reaches for a `Ref` exactly where
single ownership cannot follow the value, so the trap is doing real work rather
than covering for a weak analysis.

The compile-time answer for the aliased case is region borrowing, and it is not
designed. That is the open RFC.

---

## 7. Linear obligation

`Stream<T>` is acquired once and disposed exactly once, and `movecheck` proves it
at compile time. A stream parameter carries the obligation into the callee. This
is the **only** compile-time reclamation proof in the language.

**Issue.** It is three `Type::Stream` matches. A user's file handle, socket or
connection gets nothing. RFC-0086 M3 turns those matches into a lookup, so a
declared type joins the mechanism the way `impl Owned` joins §2.

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
| export return | one `ptr` | **leaks** |

**9a. A returned String leaks.** The decision is recorded in `wasi-min.js`:
ownership differs per function, a module-state field or a literal is borrowed, and
freeing one would be a use-after-free. Nothing crosses the boundary that says
which.

**How it should be handled.** The fact already exists. `own::owned_fns` holds it
per function. The wrapper already reads `hooks.exportReturns[name]` for the return
*type*; the same generated map can carry ownership, and the release becomes one
more line beside the argument release. This costs no new analysis.

**9b. An export that retains its String parameter is a use-after-free.** Five
programs did exactly `state = arg`. It was always wrong under RFC-0012 and it was
harmless only while nothing could free. Now the caller frees. All five copy, and
**nothing checks the rule.**

**How it should be handled.** A checker rule on `export extern fn`: a `String`
parameter may not reach a global, a record field, an array element, a map value or
a cell. `own`'s escape walker already computes precisely this set — it is the same
question with a different consequence.

---

## 10. Concurrency

`spawn` mallocs a frame holding the result slot and the arguments, and hands it to
a thread. **The frame is never freed** — stated in the lowering, because `join` is
idempotent and may run more than once. One leak per `spawn`, bounded by the number
of spawns.

The arena stack is `thread_local`, so a region inside a task is correct. A heap
value moved into a spawn escapes at the call site, so it is never double-freed —
and never freed.

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

| # | gap | consequence | cost |
|---|---|---|---|
| 1 | **§9b** an export may retain its String param | use-after-free | a checker rule over an existing walker |
| 2 | **§14** a heap value inside `Option`/`Result`/`Validation` | unbounded leak **in the recommended style** | variant-aware payload release; `CloseStream` is the precedent |
| 3 | **§4** an overwrite never releases the old value | unbounded leak in the server shape | slot-level ownership, plus release-after-store ordering |
| 4 | **§13** one memory test, one shape | every gap here is invisible | more entry points in one existing file |
| 5 | **§9a** a returned String leaks | unbounded, per call | emit `owned_fns` into the export map |
| 6 | **§7** linearity is hardcoded to `Stream` | a user resource gets no obligation | RFC-0086 M3 |
| 7 | **§2a** six expression forms never transfer | leak | extend `transfers`; §14 covers `Match` and `Try` |
| 8 | **§15** `bytes` and friends are not producers | leak | §14 covers the routed ones; `bytes` needs the list |
| 9 | **§16** a closure capture block is never freed | leak per lambda evaluation | the same question as §14, in another aggregate |
| 10 | **§10** a spawn frame is never freed | bounded leak | free at the first `join`, keep idempotence |
| 11 | **§5** regions are hand-placed | the model's best tool is rarely reachable | RFC-0004 Q3, undesigned |
| 12 | **§6** use-after-release traps at run time | not compile-time | region borrowing, undesigned |

1 is a soundness defect and everything else is a leak. **§17 multiplies 2, 3, 8 and
9 by the length of a browser session**, because the instance survives navigation.

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

---

## U3. Path B never joined the subject-first surface

```vyrn
let s = cell("hello")
set(s, get(s) + " world")
print(get(s))
```

`cell`, `get`, `set` and `release` are free functions. There are **zero**
method-form uses in `examples/` or `std/`.

The surface redesign moved collections to `sq.push(x)`, `sq[j]`, `sq.length` and
`t.join()`. RFC-0062 migrated the remaining builtins to `x.toString()` and
`s.byteLength`. Path B was in neither sweep, so the language's memory tool is
written in the one style the language rejected — on the line above `s.length`.

---

## U4. `drop` is whole-container, so an element is unreclaimable

`drop x` releases a container. RFC-0011 states that elements are a safe leak. Put a
String in an array and **no mechanism in the language reclaims it** — not the
analysis, not `drop`, not a region, not a manual call. The value is unreachable and
permanent.

This is the largest silent restriction in the model, and it appears in no
diagnostic and no document.

---

## U5. The one runtime failure has no location

```
error: reference used after release
```

No line, no binding name, no origin. The slab carries a per-slot pointer array
(`__vyrn_cell_src_arr`), but it serves stream cursors rather than diagnostics.

RFC-0006 and RFC-0009 make diagnostics a stated feature of this language:
structured, accumulating, translatable. The memory model's only failure message is
below that bar, and a user meets it at the moment they understand least.

---

## U6. The arena excludes the case it is for

`region { }` is a block. `push` cannot draw from it, because arena memory has no
`realloc`. So the loop that builds many values with one lifetime — the case
RFC-0004 §4 names as the arena's reason to exist — is the case the arena cannot
serve.

What is left is String concatenation inside a lexical block. RFC-0004 Q3 asked for
inferred, invisible regions and nothing was designed.

---

## U7. `consume` and linearity look like one mechanism and are two

A user writes `consume` and gets move checking: use the value twice and the
compiler refuses. They get **no release obligation** — nothing requires them to
dispose of it and nothing frees it.

A `Stream<T>` gets the obligation and a user cannot declare one.

Read together, the surface says "declare intent with a capability", and then the
one capability that means ownership does half the job. RFC-0086 M3 closes the
second half. Whether `consume` and `impl Owned` should be one declaration is
undecided, and should not stay undecided.

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

---

## The usability gaps, ranked

| # | gap | why it ranks here | cost |
|---|---|---|---|
| 1 | **U1** the model is invisible | every other gap compounds; a user cannot learn it | a printer over `Ownership` |
| 2 | **U2** no way to copy | the only fix for a use-after-free is a trick that leaks | one builtin, then a diagnostic names it |
| 3 | **U10** the extern boundary cannot be declared | the one place the answer is required | falls out of §9a and §9b |
| 4 | **U8** a declared container cannot be read | `impl Owned` is shipped and not yet usable | `read` as a real capability |
| 5 | **U5** the failure message has no location | the worst moment to say the least | the slot already carries a pointer array |
| 6 | **U4** an element is unreclaimable | the largest silent restriction | needs §14's answer first |
| 7 | **U3** Path B is not subject-first | inconsistent beside `s.length` | a co-naming sweep, RFC-0022 precedent |
| 8 | **U7** `consume` and linearity are two things | the surface implies one | a decision, then RFC-0086 M3 |
| 9 | **U6** the arena excludes its own case | the model's best tool is unreachable | RFC-0004 Q3, undesigned |
| 10 | **U9** three layouts, one syntax | justified, but unannounced | U1 covers it |

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

| # | gap | measured | cost to close |
|---|---|---|---|
| 1 | **P1** a module-state accumulator is quadratic and leaks | **52× slower, 12.2 GB for a 160 KB string** | §4's slot ownership, or extend `str_append` past locals |
| 2 | **P3** Part I's leaks are the performance story | 3 → 279 pages, and P1 | Part I's ranked table |
| 3 | **P2** a String has no length header | every `byteLength` and every concat scans | a header — which the allocator already pays for |
| 4 | **P8** nothing benchmarks the model | — | benchmarks for alloc, concat, and `get` |
| 5 | **P4** no coalescing, no splitting, no shrink | — | measure first; a phase-change workload decides |
| 6 | **P6** the LLVM slab is static | BSS in every binary | copy the direct backend's lazy slab |
| 7 | **P5** 94% of generation checks survive | 3 of 48 elided | needs a loop benchmark before it needs a change |

**P1 and P8 are the pair.** P1 is a 52× cliff with an invisible trigger, and P8 is
why nobody found it. Four of the seven rows above would be numbers rather than
question marks if the fourth were done first.

The three parts converge on one sentence. **§13**: the compiler cannot see its own
leaks. **U1**: the user cannot see whether a value is reclaimed. **P8**: nothing
measures what any of it costs. Three faces of the same absence, and the data for
the first two already exists inside `own::Ownership`.
