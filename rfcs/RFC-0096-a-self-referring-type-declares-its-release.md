# RFC-0096 — A Self-Referring Type Declares Its Release

- **Status:** **Complete. Landed.** The corpus row it was written to close reads
  0. Two of its own claims died to measurement: a declaration on the CYCLE, not
  on each leaking type, was enough; and a declared release on a user enum leaked
  its payload boxes on both compiling backends, which is a defect this RFC found
  and fixed.
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

## What stays open

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
