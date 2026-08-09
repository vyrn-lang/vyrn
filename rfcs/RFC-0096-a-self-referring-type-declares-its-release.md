# RFC-0096 — A Self-Referring Type Declares Its Release

- **Status:** **Complete. Landed, M1 and M2.** The corpus row it was written to
  close reads 0, and so does the LINKED reading M1 left open. Two of M1's claims
  died to measurement: a declaration on the CYCLE, not on each leaking type, was
  enough; and a declared release on a user enum leaked its payload boxes on both
  compiling backends, which is a defect this RFC found and fixed. Two more of
  M2's died the same way — the two type keys were already one, and the blast
  radius was three double frees in three different shapes rather than one shape
  repeated.
- **Depends on:** RFC-0086 M1 (`impl Owned for T`, the declared row), RFC-0089
  rule 4 (releasing a value releases its places), RFC-0092 M2/M3 (the container
  and aggregate element walks), RFC-0093 M1/M2 (the take and the hole),
  `rfcs/census-regions.md` measurement 1 (the count).
- **Principle:** the walk is structural, so a cycle has no bottom. A declaration
  IS a bottom, and it is a bottom for every type above it as well as for itself.

---

## The question

`rfcs/census-regions.md` measurement 1 classified every heap-owning corpus
binding with no release rule. At `c17b07c` the corpus harness read:

```
bindings: 3773 — 2267 not reclaimed
   63  the type has no release rule
```

The census named the class and the answer in one sentence: **"The real leaks are
a declaration away, not a region away."** `Owned::release_kind` reads a declared
`impl Owned for T` **before** the self-referring guard, so a declaration closes
what the structural walk cannot enter.

**All 63 are self-referring, re-derived rather than trusted.** The census
measured 58 of an earlier 79; two milestones landed in between. Every one of the
63 is in two files:

| file | bindings | types |
|---|---:|---|
| `std/vyx.vyrn` | 56 | `VyxComp`, `VyxRegistry`, `VyxParse`, `VyxGroup`, `VyxBody`, `VyxOne`, `VyxNodeR`, `VyxNode` |
| `std/graphql.vyrn` | 7 | `GqlQuery`, `GqlSet` |

Every one of them reaches itself through exactly one type: `VyxNode` in the first
file, `GqlSel` in the second.

---

## What the brief expected, and what the measurement said

The brief expected one `impl Owned` per leaking type — ten of them, hand-written,
each walking its own fields.

**Two were enough.** The guard `release_kind` carries asks whether a type reaches
ITSELF. That is the wrong question for a release. The right question is whether
the cycle has a **declaration on it**, because the walk emits a CALL at a
declared type rather than expanding into it, and a call is a bottom. So:

- `Owned::unbounded` replaces `self_referring` at the four guard sites in
  `release_kind` (the `Array`, `SmallArray`, `Map` and record/enum/fixed-array
  arms). It is the same walk with one extra rule: stop at a name that declared
  `impl Owned`.
- `impl Owned for VyxNode` and `impl Owned for GqlSel` are the two declarations.
- Everything above them — a record holding a `VyxNode`, an `Array<GqlSel>`, an
  `Array<VyxComp>` two hops up — gets its structural row back for free.

The soundness argument is one line. `unbounded(root) == None` means no cycle
without a declaration on it is reachable from `root`; every path the walk takes
below `root` is a suffix of a path from `root`, so no sub-walk can meet one
either. The static walk therefore terminates, and the run-time recursion is the
user's own function over a finite value.

**A `match` arm is an expression and `drop` is a statement**, so an enum's
release cannot write `drop` in an arm. Both declarations hand each payload to a
one-line helper — `fn vyxGive<T>(v: consume T) -> Int64 { drop v ; return 0 }` —
which reads the row the INSTANCE has (RFC-0090 M1, Phase 8b). One helper releases
a `String`, an `Array<String>`, an `Array<VyxAttr>` and an `Array<VyxNode>`, and
the last of those is where the recursion lives. `GqlSel` is a record, so it uses
`std/slots`'s shape instead: `let subs = consume self.subs ; drop subs`, four
times.

---

## Two defects the change found

### 1. A declared release on a user enum leaked its payload boxes

A wide enum payload is BOXED — one `malloc` per payload slot at construction,
which `unbox_payload` loads out of. The structural walk frees that block in
`release_enum`. A **declared** release never reaches it: the function is handed
the enum by value, gives its payloads back by name, and no Vyrn surface names the
block at all.

Measured: 16 bytes per node, on both compiling backends. It read steady at 500
calls and plain at 32,000 — `131072` bytes against `589824` for a fixture that
allocates one node a call.

`Gen::free_declared_boxes` (textual backend) and `Fn_::free_declared_boxes`
(direct wasm backend) close it. Both run **after** the declared call, because the
call reads out of the blocks. `release_enum` grew a `payloads: bool` so the two
walks are one shape rather than two.

**The direct backend is the one the memory suite sees.** The fix went into the
textual backend first and the row did not move, which is what named the second
site.

### 2. A field READ assigned to a local, then handed on

`std/vyx.vyrn`'s `vyxProcessElemInner` wrote:

```vyrn
let g = vyxGroupNodes(kids, fileId)
childNodes = g.nodes
```

`childNodes` is a `mut` local that later goes into a `VNElem`. RFC-0092 M1
refuses a projection stored into a place that outlives the call, and it refuses
`return out` where `out` was assigned from one — verified, that minimal program
is still refused. It does not refuse this shape: the store is into a local, and
the local escapes later.

While `VyxGroup` had no release rule the site was harmless. The moment it got one
it was a double free, and `genwasm` reported it as a trapped generator with a
20-frame backtrace inside `std/vyx`'s recursive parser — a corrupted heap read as
structure. `childNodes = consume g.nodes` is the fix, and it is one word.

**This is a hole in the rule, not in the fix.** The rule refuses a projection
that is stored and then returned in one expression; it does not follow one
through a local. It is recorded here rather than closed, because closing it is an
alias analysis and RFC-0089 deleted the last one on purpose. The corpus has
exactly one site, and the eyes that found it are the ones that would find the
next.

---

## The depth bound, measured

A release of a tree N deep is N native frames. Measured on Windows with the
default 1 MiB stack, over a synthetic chain of `Link(String, Array<Chain>)`:

| walk | survives | overflows |
|---|---:|---:|
| the declared release | 10,000 | 11,000 |
| an ordinary recursive Vyrn walk over the same chain | 15,000 | 20,000 |

The release costs about 1.5x the stack of a hand-written walk per level. **It
does not add a class of limit**: any recursive Vyrn function over a recursive
value has the same bound, and `std/json`'s `copyJson` has had it since RFC-0091
M1. A `.vyx` template nests as deep as its author's markup and a GraphQL query as
deep as its selection set — two orders of magnitude under the bound. The number
is documented rather than engineered away; a worklist release would move the
recursion into an `Array<T>` and buy nothing this corpus can spend.

---

## What proves it

- **The corpus row is gone.** `the type has no release rule` reads **0**, from 63.
  "not reclaimed" falls **2267 to 2207**; "reclaimed/moved/dropped/discharged/
  static" rises **1506 to 1571**.
- **The memory suite reads 17 rows, 17 steady.** `selfReferring` is the new row:
  four ~900-byte Strings a call in a tree, under a record that only reaches one.
  Removing the `impl` makes it grow; so does removing the declared stop from the
  guard; so does removing either box free — verified by removing each.
- **Three-way parity byte-identical including traps**, 36 tests, wasm column live.
- **`genwasm` green** over every generator example, both engines byte-identical.
  It is the test that found defect 2.
- **Workspace `cargo test --workspace`, `vyrn-lsp` separately.** RFC-0092's
  instrument still reads `stores: 0`, `elem-store: 0`, `elem-return: 0`.

---

## What M1 left open — closed by M2 below

**31 bindings, linked, all of them `Json` or `Html`.** The corpus harness parses
each file ALONE, so it cannot resolve an imported `Json` and counts those
bindings under "the type owns no heap". `vyrn why --memory`, which links, reports
31: 10 `Html`, 11 `Json`/`json$Json`, 9 `GqlOut`/`GqlVal`/`GqlArgs` and 1
`RpcClientTypes`, every one of them reaching `Json` or `Html`.

They are the same class and the same two-declaration fix, one module over. They
are not in this change for two measured reasons:

1. **`Json` has two type keys.** The linked report names both `Json` and
   `json$Json`, because `std/json` is an INJECTED runtime module and the linker
   renames its declarations by prefix — the same arrangement that broke
   `impl Copy for Json` in RFC-0092 M3. A declared row is keyed by ONE type key.
   That has to be settled before the declaration means anything.
2. **The blast radius is the whole corpus.** `Json` flows through `std/http`,
   `std/rpc`, `std/jsondec`, `std/openapi` and `std/connect`. Defect 2 above is
   what one such site costs, and it took a full `genwasm` run to find one in two
   files.

The number is written down so the next change starts from it.

---

## M2 as landed — `Json` and `Html` declare their release

**The linked reading is 0.** Re-derived at `12255e4` before anything moved, it
is **33** rather than 31: `Html` had risen from 10 to 12 over the two commits
between the two measurements, and every other family matched to the binding
(11 `Json`/`json$Json`, 9 `GqlOut`/`GqlVal`/`GqlArgs`, 1 `RpcClientTypes`).
`impl Owned for Json` and `impl Owned for Html` closed all 33. **There are no
survivors.**

The reading is `vyrn why --memory` over `examples/` and `std/`, counting the
line `nothing releases the type T yet`: 203 files answer, 13 need a project root
and are skipped, both times.

### The brief was wrong about the prerequisite

M1 recorded two type keys, `Json` and `json$Json`, and said "that has to be
settled before the declaration means anything". **It was already settled**, by
the patch RFC-0092 M3 landed at `loader.rs:1958`, and the patch is neither
subsumed nor in need of generalizing — it IS the general rule:

- the rename reaches the impl **head**, because `rewrite_module_refs` calls
  `rewrite_type(&mut im.ty)` like every other type position, so
  `Owned::new` reads the key `json$Json` off a renamed head;
- the rename reaches the flattened **method**, because the M3 patch walks
  `m.program.impls` for EVERY protocol and mints
  `impl_method_name(P, "json$Json", m)` rather than the prefixed mangling the
  general loop would produce.

So `Owned` needed no new code at all. The two spellings are not two keys in one
link — they are one key in two link modes. A program that mentions `toJson` gets
`std/json` INJECTED and every binding in that link reads `json$Json`; a program
that only imports it by hand reads `Json`; a program that does BOTH gets the
reserved spellings anyway, because the loader sets `m.injected` whether or not
the load performed the visit. Neither option (a) nor option (b) was needed and
neither was taken.

**What was missing was a test.** No test named the patch: it was covered only
through `impl Copy for Json`, four tests away.
`a_declared_impl_in_an_injected_module_is_reached_in_both_link_modes`
(`vyrn-cli/tests/json.rs`) runs a hand-import-only program and a
hand-import-plus-`toJson` program, asserts each RUNS and that `vyrn why --memory`
names the declared release under that link mode's key —
`Owned__Json__release` and `Owned__json$Json__release`. Reverting the patch
fails it, with `call to unknown function Copy__json$Json__copy` from inside
`std/json` itself.

### The two declarations

`Json` is an enum and takes `std/vyx`'s shape, one helper per payload
(`jsonGive`). `Html` is an enum and takes the same. **One thing the M1 write-up
got wrong about its own code**: it says an enum's release cannot write `drop` in
an arm and therefore binds the match to a `let given`. The binding is
unnecessary — a bare `match` STATEMENT runs its arms and needs no name, verified
by running one. The two new impls use the bare form, which is why the unlinked
corpus reads 3778 bindings and 2207 not reclaimed, exactly its baseline; the
`let given` form would have added two scalar bindings to the count.

### The blast radius: three sites, in three different shapes

M1 predicted five modules' worth of defect-2 sites. It named the right hazard
and the wrong shape: only ONE of the three is defect 2's shape, and the other
two are shapes M1 had not met. Each was found by a different pair of eyes, and
none of them by the memory suite.

**1. A field read through a local** — defect 2 again, in `std/rpc.vyrn`'s
`client(dir)`:

```vyrn
let types = rpcClientTypes(mods)
…
let mut syms: Array<Symbol> = types.symbols
```

`RpcClientTypes` had no release rule and has one now, so reading the field out
and then handing `syms` to `symbolMapFn` freed one buffer twice.
`consume types.symbols` is the fix, one word. **`genwasm` found it**, as a
trapped generator: `fullstack/client/boot.vyrn: exit 0 (interp) vs 1 (wasm)`.
`genwasm` was run first, before anything else, exactly as M1 said to.

**2. A borrowed parameter stored into a returned value** — `std/html.vyrn`'s
differ, and the largest of the three. `diff` builds an `Array<PatchOp>`, and a
`PatchOp` holds an `Array<Int64>` path and an `Html`:

```vyrn
fn diffNode(path: Array<Int64>, old: Html, new: Html) -> Array<PatchOp> {
    if !sameKind(old, new) {
        return [OpReplace(path, new)]      // both borrowed
    }
```

`path` goes into EVERY op of a loop, so a positional diff of n removals put one
borrowed buffer into n ops. While `Html` was self-referring, `PatchOp` reached
it and had no release rule either, so nothing ever freed an op and the sharing
was invisible. The declaration gave `PatchOp` its structural row back and the
sharing became a multiple free. Nine sites, nine `.copy()` calls — `path.copy()`
at every construction and `.copy()` on the node at the four that store one.
**Parity found it**, as `examples/patchdemo.vyrn` failing NATIVELY with
`error: out of memory`: a corrupted allocator, not a wrong answer.

**3. A map takes its key, and the key belonged to a snapshot** — the
SYNTHESIZED `Map<String, V>` decoder in `jsondec.rs`:

```vyrn
for f in fieldsOf(v) { … m[f.key] = x … }
```

`fieldsOf` returns a copy, the `for` owns that snapshot and releases it
(RFC-0092 M5), and a map store takes the key pointer and copies nothing. Both
owned one buffer. `f.key.copy()` is the fix. **Parity found it too**, and as the
quietest failure of the three: `examples/mapdemo.vyrn` printed
`decoded total=4` where the interpreter printed `8` — one of the two keys read
back freed, so its lookup missed and returned `-1`. No trap, no crash, one wrong
number in the middle of a passing program.

A scan of the nine modules that touch `Json` or `Html` for the read-through-a-
local shape finds three more textual matches and none is a defect: two read from
a BORROWED parameter (`std/rpc`'s `cfg.pinKeys`, `std/graphql`'s `sel.key`),
which the frame never releases, and the rest read `.length`.

### The memory suite reads 18 rows, 18 steady

`injectedJson` is the new row: a `Json` object built and copied every call, in a
program that both hand-imports `std/json` and mentions `toJson`, so the row's
bindings carry the injected key `json$Json`. It is negative-tested three ways.

- Removing `impl Owned for Json` makes it grow — 2.3 MB at 500 calls against
  9.0 MB at 2000.
- Disabling `free_declared_boxes` in the DIRECT backend makes it grow — 131 072
  bytes against 262 144. **Defect 1's fix does hold for an injected module's
  enum**, and it holds for the reason it is name-agnostic: the walk resolves the
  type structurally and looks each variant up in the linked program's own table,
  so a renamed variant is looked up under its renamed name. The row carries 36
  nodes rather than 2 on purpose — a boxed payload is 16 bytes, and two of them
  a call hide inside one 64 KiB page.
- The row also runs the **composition** the brief asked for: `Json` declares
  `Copy` as well, so the tree and its copy are released once each.

**Two shapes in the row leak for reasons outside this milestone**, found by
building it and measured out of it rather than left in:

- `let s = toJson(x)` **leaks its String** unless the binding is annotated.
  `toJson` carries no row in `prelude::returns`, so the declared-types reading
  cannot name its result and `own` leaves the binding alone. That is RFC-0094's
  class, one builtin further on.
- `"n" + i.toString()` leaks the `toString` temporary. A temporary handed
  straight into an operator has no binding to own it.

### Copy and release compose, on three engines

The memory suite sees one engine. `examples/copy.vyrn` builds a `Json`, copies
it and lets both go; `examples/htmltree.vyrn` does the same to an `Html` tree.
Both are ordinary parity citizens, so the shape runs under the interpreter, the
native binary and wasm, byte-identical. **A double free frees**, so the memory
suite cannot see one — parity and `genwasm` are the eyes, and `genwasm` is what
saw the one there was.

### What proves it

- **Linked reading 33 → 0**, no survivors.
- **Unlinked corpus**: "the type has no release rule" stays **0**. Over the same
  files as the baseline the reading IMPROVES — 3778 bindings and **2204** not
  reclaimed against 2207, because `consume types.symbols` took two bindings out
  of "escaped into a call" and one out of "it names somebody else's value".
  With the two new parity examples counted the corpus is 3783 bindings and 2208
  not reclaimed: five new bindings, four reclaimed and one not, and that one is
  the harness's own blindness — it parses each file ALONE, so an imported `Json`
  or `Html` is a name it cannot resolve and it files the binding under "the type
  owns no heap". That is the same blindness this milestone exists to answer, and
  the linked reading of those same files is 0.
- **Memory suite 18 rows, 18 steady**, with three negative tests on the new row.
- **Three-way parity byte-identical including traps**, 36 tests, wasm column
  live.
- **`genwasm` green** over every generator example, both engines byte-identical
  — after it found the one double free.
- Workspace `cargo test --workspace --no-fail-fast`, `vyrn-lsp` separately, the
  serve suite, `universal_pages --ignored`.
- RFC-0092's instrument reads `stores: 0`, `elem-store: 0`, `elem-return: 0`.
- `cargo fmt --check` and `vyrn fmt --check` clean.
