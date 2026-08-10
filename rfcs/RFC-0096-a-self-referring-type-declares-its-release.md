# RFC-0096 — A Self-Referring Type Declares Its Release

- **Status:** **Complete. Landed, M1, M2 and M3.** The corpus row it was written
  to close reads 0, and so does the LINKED reading M1 left open. Two of M1's
  claims died to measurement: a declaration on the CYCLE, not on each leaking
  type, was enough; and a declared release on a user enum leaked its payload
  boxes on both compiling backends, which is a defect this RFC found and fixed.
  Two more of M2's died the same way — the two type keys were already one, and
  the blast radius was three double frees in three different shapes rather than
  one shape repeated. M3 closed the two leaks M2 measured out of its own row and
  found four defects doing it, two of which it recorded with numbers rather than
  fixing, because one of them holds the other shut.
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
building it and measured out of it rather than left in. **M3 below closed
both.**

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

---

## M3 as landed — the two leaks M2 measured out of its own row

Both are closed. Neither was the shape the brief named, and the second one cost
four defects rather than one.

### 1. `toJson` gets its return row, and so do ten more

`let s = toJson(x)` read `NOT reclaimed — the type unknown owns no heap`. It now
reads `reclaimed at block exit — freeing the String buffer`. The row is one
line. **The audit around it is the milestone.**

RFC-0094 folded four return types onto seeded signatures and stopped. The rest
of `checker::RESERVED` was never asked, and the reading it feeds is
all-or-nothing: a call with no row has no type at all, so the binding is left
alone. Eleven names allocate a result this language can spell and had no row;
seven more allocate and are held back, each for a reason about the TYPE.

| name | it answers | named before | after |
|---|---|:--:|---|
| `toJson` | `String` | no | **row added** — 3 corpus sites |
| `jsonSchema` | `String` | no | **row added** |
| `schemaOf` | `Schema` | no | **row added** — 1 corpus site |
| `args` | `Array<String>` | no | **row added** — 1 corpus site |
| `readLine` | `Option<String>` | no | **row added** |
| `readFile` | `Result<String, String>` | no | **row added** |
| `readFileBytes` | `Result<Array<UInt8>, String>` | no | **row added** |
| `writeFile` | `Result<Bool, String>` | no | **row added** |
| `renameFile` | `Result<Bool, String>` | no | **row added** |
| `fsyncFile` | `Result<Bool, String>` | no | **row added** |
| `stringFromBytes` | `Result<String, String>` | **WRONGLY** | **row corrected** — see below |
| `fromJson` | `Validation<T>` | no | excluded — `T` is the type NAME the caller wrote, which no signature can say |
| `value` | `Value` | no | excluded — it boxes the caller's buffer rather than copying it, so it LENDS and a row would double free |
| `@list` | `Array<E>` | no | excluded — `E` is the argument's element type, which one row cannot name any more than `at` can |
| `pullAt` | `Option<T>` | no | excluded — the element type comes from the expected type, so the checker already refuses the call without an annotation |
| `moduleInterface` | `ModuleInterface` | no | excluded — generation-only (RFC-0021); neither compiling backend lowers it, so who owns the result is written nowhere. **18 corpus sites**, all in `gen fn`s. **Reversed below — the exclusion was about lowering, not about the type** |
| `contractOf` | `ContractInfo` | no | excluded — the same, for RFC-0071's side. **Reversed below** |
| `listDir` | `Result<Array<String>, String>` | no | excluded — the same again. **Reversed below** |
| `at`, `atSet`, `bytes` | the receiver's | no | excluded — they LEND, which is RFC-0094's older rule |
| `blackBox`, `@pop`, `@swapRemove`, `@join` | a bare `T` | no | excluded — RFC-0094's other older rule |

Every other reserved name answers a scalar, `Unit`, a `Logger`, a vector or a
sum constructor, and none of those owns heap.

**`stringFromBytes` is what the audit was for.** RFC-0094 M1 wrote its return as
`String`; the checker's arm answers `Result<String, String>`. Two spellings of
one fact, which is exactly what that RFC exists to prevent, and they had drifted
in the milestone that removed the second lists. The declared reading therefore
released `let s = stringFromBytes(b)` as a String buffer, handing
`__vyrn_str_free` the aggregate's tag word: **the native binary SEGFAULTED**,
and `vyrn why --memory` said "reclaimed at block exit — freeing the String
buffer" about it. No annotated binding ever met it, which is why nothing did.
`every_allocating_builtin_answers_its_return_type` is the assertion that a row
cannot drift from the arm again.

**Corpus:** 3783 bindings, "not reclaimed" **2208 → 2184**; "the type owns no
heap" **2057 → 2033**. The linked reading M2 took to 0 stays 0.

### 2. The expression temporary — measured, then four defects deep

`"n" + i.toString()` allocates twice and names once. `own` keys every release on
a `Stmt::Let`, so the `@str` result has nothing to write a row against.

**The measurement, native, before anything moved.** A loop of
`"n" + i.toString()`, peak working set:

| turns | peak |
|---:|---:|
| 250,000 | 19.94 MB |
| 1,000,000 | 54.12 MB |
| 1,000,000, interpolation form `"n\{i}"` | 65.16 MB |

Not a steady state — about 48 bytes a turn. **The interpolation form leaks the
same way**, and the brief's guess that `finite.rs` might already free its holes
is wrong: `"a\{x}b\{y}"` desugars to `@concat` folded LEFT over five pieces, so
a two-hole template allocates six buffers and names one.

**The corpus shapes, counted:**

| shape | sites |
|---|---:|
| a `+` / `@concat` operand that is itself an allocating String expression | **2257** |
| a call argument that is the result of a call whose declared return owns heap | 930 |
| a call argument that is an allocating String expression | 632 |

**The fix closes the first row and leaves the other two open**, and the reason
is a rule rather than an appetite. `@concat`, a String `+`, `@str` and the
in-place append all COPY out of their operands, so an operand the expression
itself allocated is finished with the moment the copy is made. A general call
may RETAIN its argument — a `consume` parameter, a variant constructor — so
freeing after one needs the callee's signature at the site and across modules.
That is a milestone, not a line.

`own::str_temporary` is the one rule, read by both compiling backends at four
sites each. Three forms answer true: `@str`, `@concat`, and `+` where the
backend's own type check says `String`. Safety rests on the same argument
`ban_append_expr` already stands on — the lexer cannot produce a leading `@`, so
no user declaration can shadow either name. Inside a `region` it stands aside:
the buffer is the arena's, which is the partition `own` already states as
`Fate::Leaked(Leak::Region)`.

**After: 4.06 MB at 250,000 turns and 4.07 MB at 1,000,000.** Negative-tested by
making `str_temporary` answer `false` and re-measuring on the direct backend:
the `+` shape reads 1,638,400 bytes at 500 calls against 6,291,456 at 2,000, and
the interpolation shape the same; with the rule, 131,072 at both.

#### The four defects, none of them the one the brief expected

**1. `@str` of a String was the IDENTITY on the direct backend and a strdup on
the textual one.** One rule cannot answer for two engines that disagree about
who owns a rendered String, and the disagreement was already a latent double
free on wasm alone: `let t = "\{s}"` has one hole and no literal piece, so the
whole interpolation IS `@str(s)` with no `@concat` above it, and `t` and `s`
then released one buffer twice. Found by parity — `sha1.vyrn` printed
`TEST2: TEST2: 41c3bd26ebaae4aa1f95129e5e` on wasm and the right digest
everywhere else. The direct backend copies now, which is the cost native has
always paid.

**2. `@str`'s own argument leaked.** The copy is what makes the argument
finished with, and neither backend freed it — `"\{a + b}"` has leaked that
buffer for as long as the textual backend has strdup'd here. The same free, one
consumer further on.

**3. A local String accumulator is never released — RECORDED, then CLOSED.**
`let mut acc: String = ""` is the opening line of every accumulator in this
language. `own`'s static-data rule read the INITIALIZER, answered `Fate::Static`
for the whole binding, and the heap buffer the last `acc = acc + …` left was
never freed. Measured on the direct backend: **851,968 bytes after 500 calls and
3,211,264 after 2,000.**

**4. A `String` returned out of a `region` is freed by its caller —
PRE-EXISTING, then CLOSED.** Verified at `c6d9331`, with no `mut` and no part of
this milestone in the build:

```vyrn
fn viaString(n: Int64) -> String { region { return "n=" + n.toString() } return "" }
fn main() -> Int64 {
    let mut p = 0
    while p < 200 { let last = viaString(p) ; p = p + 1 }
    print("done") ; return 0
}
```

`parity.rs`'s own region test carried that shape and passed only because its
binding is a `mut` with a literal initializer — which is defect 3, holding defect
4 shut. Giving the accumulator its release turns the leak into the crash, so the
region defect is what comes first. Two leaks, one behind the other, and the order
is measured rather than assumed.

### Both are closed, and the order held

**Defect 4 is one word of layout.** The repro exits **`0xC0000374`,
`STATUS_HEAP_CORRUPTION`,** at `45b3740` — the exit code depends on the C
runtime, and the free is invalid rather than double. `__vyrn_region_alloc` wrote
its chain link in FRONT of the payload and returned `raw + 8`, so a `String`
header sat 8 bytes inside the block; `@__vyrn_str_free` frees `s - 16`, which is
`raw + 8`, which `malloc` never handed out. The arena's own exit never met that
because it freed by the link address.

**The link sits after the payload now**, so a block the arena hands out is
exactly what `__vyrn_malloc` returned. The chain is a chain of trailers — `{
next, base }` at the first 8-aligned address past the payload — and
`__vyrn_region_exit` reads the base out of the trailer. Nothing else moves: the
walk still stands off a `String` inside a region (`own`'s `Leak::Region`, and
`deep_release` one level down), the arena still frees at the closing brace what
no return carried out, and `region_pop` still hands the frame up on the return
path. That is the second of the brief's two directions — **the pop hands
ownership over explicitly** — rather than the first, and the reason is that a
boundary COPY answers only for a `String` the return names directly. An
`Array<String>` built in a region and returned carries arena buffers under a
`malloc`'d spine, and a deep copy of it would leave the spine with no owner. The
layout answers for every shape at once, in the runtime, at every depth.

**It costs 16 bytes an allocation where the front link cost 8**, plus the
alignment of the payload. Measured on the census's own deferral shape — a region
around a loop, 2,000,000 concatenations of a 13-byte result, nothing freed until
the closing brace — native peak working set: **100,876,288 B before,
132,497,408 B after, +31%.** The DEFERRAL is unchanged: still one `malloc` per
allocation, still every block held to the closing brace, so census §5a's 1023x
stays a statement about the mechanism rather than about this change. A side
vector of block pointers would buy the 8 bytes back for about 20 more lines of
IR, on a path the census measured as a loser with three corpus uses. It is not
worth them today.

Two shapes that look like the same defect are **not**. An in-place append inside
a region would `realloc` an arena block and dangle its trailer — `Stmt::Assign`
already refuses the fast path while `region_depth > 0`, and it still must. A
slot that still holds its literal releases nothing, because `@__vyrn_str_free`
reads a `cap` of 0 as "never `realloc`, never free" — which is what makes defect
3 the one line it was written as.

**Defect 3 is that one line.** `own::fate`'s static-data rule asks whether the
binding can CHANGE, not what it opened with:
`matches!(value, Expr::Str(_)) && !matches!(s, Stmt::Let { mutable: true, .. })`.
A `mut` binding is released by its slot's final value in all three engines
(Phase 8b), so the accumulator's grown buffer goes back at block exit and the
loop that never ran frees a literal, which frees nothing.

**The corpus count does not move, and that is the finding.** 2184 not reclaimed
at `45b3740`; 2186 with this branch's two added `Int64` bindings in
`examples/region.vyrn`, and **identical with the rule reverted**. `Fate::Static`
is counted as an answer, not as a leak, so the harness reads a leaking
accumulator as "static data" and always did. The row that can see it is the
memory suite's, which is why defect 3 needed one.

### The memory suite reads 20 rows, 20 steady

`exprTemporary` is the new row: a `+` chain, an interpolation whose hole is
itself a concatenation, and an in-place append whose operand is one — every byte
of it a String no binding names. `tag()` hands back a data-segment literal and
allocates nothing, so nothing else in the row can move it. Removing any one of
the four frees makes it grow.

Two tests read the textual backend's IR beside it, so neither engine can start
leaking alone: `a_temporary_inside_an_expression_is_freed` counts two frees for
`"n" + n.toString()`, and `every_interpolation_hole_is_freed` counts six for
`"a\{n}b\{n}c"` — two holes, three inner joins and the binding, with the three
literal pieces freeing nothing. That last count is what says the rule reads the
EXPRESSION rather than the type.

### What proves it

- **`let s = toJson(x)` reclaims**, and eleven builtins answer a return type
  that no rule could read before.
- **`stringFromBytes` no longer segfaults** on an unannotated binding.
- **Corpus 2208 → 2184 not reclaimed**, 2057 → 2033 "the type owns no heap".
- **Native peak 19.9/54.1 MB → 4.06/4.07 MB** at 250,000 and 1,000,000 turns.
- **Memory suite 20 rows, 20 steady**, each new row negative-tested by reverting
  its rule: `exprTemporary` by making `str_temporary` answer `false`,
  `localAccumulator` by dropping the `mut` clause — 8,323,072 B at 500 calls
  against 32,899,072 at 2,000.
- **The region repro exits 0 on all three engines**, and
  `examples/region.vyrn` carries it so parity holds the shape.
- **Three-way parity byte-identical including traps**, 36 tests, wasm column
  live. It is what found defect 1.
- **`genwasm` green** over every generator example, both engines byte-identical.
- Workspace `cargo test --workspace --no-fail-fast`, `vyrn-lsp` separately.
- RFC-0092's instrument reads `stores: 0`, `elem-store: 0`, `elem-return: 0`.
- `cargo fmt --check` and the corpus fmt gate clean.

### What is open, with its number

| what | count | why it is not here |
|---|---:|---|
| a call argument that is an owning call's result | 930 | a callee may retain its argument; freeing after one needs the signature at the site, cross-module |
| a call argument that is an allocating String expression | 632 | the same rule |
| the rest of a frame a `return` popped | unmeasured | a return out of a region hands over the value it carries and leaks what it does not — census §6's paragraph, which needs the escape analysis RFC-0004 defers |
| an arena block's 16-byte trailer | +31% peak on a deferring region | a side vector of block pointers buys 8 of it back for ~20 lines of IR, on a path with three corpus uses |
| `moduleInterface`, `contractOf`, `listDir` returns | 18 sites | ~~generation-only; no compiling backend lowers them, so the owner is written nowhere~~ **closed in the addendum below** |

Defects 3 and 4 are closed (PR #140 — the arena's link left the block, and the
accumulator's fate reads whether the binding can change); the first two rows
above are what closing them left.

---

## M3 addendum — the three the audit excluded for a reason about LOWERING

`moduleInterface`, `contractOf` and `listDir` were held back together, and the
reason recorded for all three was that no compiling backend lowers them. Every
other exclusion in the table above is a fact about the TYPE. This one was a fact
about an EMITTER, and an emitter cannot say what a call gives back.

**All three have rows.** `listDir` answers `Result<Array<String>, String>` the
way `readFile` answers `Result<String, String>`. `moduleInterface` and
`contractOf` answer `ModuleInterface` and `ContractInfo`, which are records the
parser INJECTS into every program, beside `Schema` and `Issue` — so the declared
reading resolves them like any other declared name and no signature had to
stretch. Where the calls in fact are, the reading was lying:

```
before   line 2  entries   NOT reclaimed — the type unknown owns no heap
         line 3  iface     NOT reclaimed — the type unknown owns no heap
after    line 2  entries   reclaimed at block exit — releasing what the Result<Array<String>, String> holds
         line 3  iface     reclaimed at block exit — releasing what the { functions: …, types: … } holds
```

Across `std/`: 2142 bindings, "not reclaimed" **1140 → 1118**, reclaimed 340 →
362. **Not 22 frees** — 22 sentences that are now true. Read the next paragraph
for why there is no free to emit.

### The boundedness argument, checked rather than assumed

The generation engine is the interpreter, and `interp::Val` is an ordinary Rust
enum — `Rc<String>`, `Box<Val>`, `Vec<Val>`. Rust drops the whole graph with the
frame that built it. The only thing that crosses out of a generation is the
generated SOURCE, and the cache that holds it is `gen_cache_get`/`gen_cache_put`
over `String`, checked at `loader.rs` — a `String` in, a `String` out, no value
handle. So the leak class these three names could have belonged to is **empty by
construction**, and the milestone here is facts, not frees. That is also why 31
corpus sites can gain a declared type with no byte of emitted code changing.

### What the rows DID change: movecheck can now see a generator body

Seeding `moduleInterface` gave `for t in iface.types` an element type for the
first time, so the move rule reached three pushes it had never been able to
read: `std/rpc.vyrn` stores `t.module` and `t.name` — fields of a loop variable —
into arrays that outlive the loop. `examples/rpc.vyrn` stopped generating until
each took the `.copy()` the diagnostic names. **A row is not only a release; it
is the type every other rule was missing.**

### The boundary is a gate now, not a comment

`moduleInterface` and `contractOf` have no runtime anywhere, and before this the
front end never said so. `vyrn check` answered `ok`; the refusal arrived from
the interpreter at RUN time, from the text-IR emitter as one sentence, and from
the direct backend as `direct backend: no lowering for the call
'moduleInterface' at line 2` — an emitter's internal words in a user's
diagnostic. Both are gated in the checker now, in the sentence RFC-0054's
`lex` already used: **``moduleInterface`` is only available during generation**.
One gate serves `vyrn check`, `vyrn run` and both backends, and the direct
backend's fallback is unreachable for them.

**`listDir` is deliberately NOT gated with them, and the brief that asked for it
was wrong.** `listDir` has a runtime: under `vyrn run` it lists the real
filesystem, which is why `COMPTIME_FORBIDDEN` omits it and why the interpreter
serves it. Only the two compiling backends lack a lowering, and each says so
itself. `list_dir_is_not_generation_only` pins that, beside
`the_reflection_builtins_are_refused_outside_a_generation` for the other two.

**Open, one line:** the direct backend still refuses `listDir` with `no lowering
for the call` where the text-IR backend words it properly. The front end cannot
help — the call is legal under `vyrn run` — so the fix belongs in the emitter,
and it is a message, not a defect.

**What proves it:** three-way parity byte-identical including traps (36 tests,
wasm column live); `genwasm` 11 green, both engines byte-identical over every
structured-result generator; workspace `cargo test --workspace --no-fail-fast`;
`vyrn-lsp` separately (78); memory suite 19 rows, 19 steady; `cargo fmt --check`
and `vyrn fmt --check` clean.

