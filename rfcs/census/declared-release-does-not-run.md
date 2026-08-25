# A declared `Owned` release is reported and does not free

Status: **FIXED 2026-08-25, same day.** The diagnosis was one word in
`own::owns_heap`, and a second, smaller gap is left open below. The
reproduction and the ruled-out list are kept as written, because the thing that
was ruled out three times was not the thing.

## The cause: `owns_heap` ran out of depth and said "no"

```rust
fn go(ty: &Type, types: &HashMap<String, TypeDecl>, depth: usize) -> bool {
    if depth > 8 {
        return false;
    }
```

`type Tree = | Leaf | Node(Tree, Tree)` reaches itself, so the walk spent its
eight levels on `Tree` and answered that a type which is nothing but heap owns
none. Two things followed:

- `vyrn why --memory` reported "the return type Tree owns no heap".
- `Gen::release_enum` skips a variant whose payloads own nothing — so
  RFC-0096's `free_declared_boxes`, which exists precisely to free the block a
  wide payload travels in, skipped the one variant whose boxes needed it.

The emitted IR is where it became visible. `make` calls `__vyrn_malloc` twice
per `Node`; the declared release destructures with `inttoptr` and `load` and
frees neither box:

```llvm
me.arm.3:
  %t3 = extractvalue { i64, i64, i64 } %t1, 1
  %t4 = inttoptr i64 %t3 to ptr
  %t5 = load { i64, i64, i64 }, ptr %t4      ; the subtree comes out
  ...                                        ; the box never goes back
```

**The cycle is the answer, not the limit.** A type that reaches itself cannot be
stored inline — the representation has to box the recursive field to be finite —
and that box is heap whatever else the type holds. `owns_heap` keys a `seen`
stack on the type NAME now, the same shape `self_referring_past` a few functions
up already used, and a repeated name answers `true`.

### The numbers, after

| rounds | before | after |
| --- | --- | --- |
| 50,000 | 765.2 MB | **3.8 MB** |
| 200,000 | 3115.0 MB | **3.8 MB** |

Flat, which is what "not a leak" means. Three-way parity is 40/40, which is the
check that matters for a release change: freeing something twice is the failure
mode a memory fix has, and the corpus runs every example on three engines.

## Still open: a temporary is never released

`binarytrees` did not improve from the fix alone, because it wrote
`check(make(depth))`. A temporary has no binding for the release to attach to,
so the tree is built and abandoned. Binding it — `let t = make(depth)` — takes
depth 16 from **451.9 MB to 20.5 MB**, and 20 MB is about the live long-lived
tree.

`examples/binarytrees.vyrn` binds it now, and the game's own wording asks for
exactly that: trees are "built, checked and released one at a time". But the
language should not need the `let`. **A temporary of an owning type, passed to a
`read` parameter and dropped on the floor, is not released.** That is a second
defect and it is not fixed here.

---

The original report follows, as written.



## The claim

`vyrn why --memory` reports a binding of a self-referring type as reclaimed by a
declared release. Memory still grows without bound in the number of values
built.

## The reproduction

```vyrn
type Tree = | Leaf | Node(Tree, Tree)

fn give(v: consume Tree) -> Int64 {
    drop v
    return 0
}

impl Owned for Tree {
    fn release(consume self) {
        match consume self {
            Leaf => 0,
            Node(l, r) => give(l) + give(r),
        }
    }
}

fn make(depth: Int64) -> Tree {
    if depth == 0 { return Leaf }
    return Node(make(depth - 1), make(depth - 1))
}

fn check(t: Tree) -> Int64 {
    return match t { Leaf => 1, Node(l, r) => 1 + check(l) + check(r) }
}

let rounds = 200000

fn main() -> Int64 {
    let mut sum = 0
    let mut i = 0
    while i < rounds {
        let t = make(8)
        sum = sum + check(t)
        i = i + 1
    }
    print("\{sum}")
    return 0
}
```

The depth is FIXED. The live set is one tree of 511 nodes at any moment. Only
the number of allocations grows.

## What the compiler says

```
fn make(depth: Int64) -> Tree
  transfers: yes — the caller owns the result, and releases it by calling `Owned__Tree__release`

fn checkAll(depth: Int64, iterations: Int64) -> Int64
  line 75    t                reclaimed at block exit — calling `Owned__Tree__release`
```

## What happens

Peak working set, native build, Windows:

| rounds | no `impl Owned` | with `impl Owned` | with a NON-generic `give` |
| --- | --- | --- | --- |
| 50,000 | 787.1 MB | 765.2 MB | — |
| 200,000 | 3094.2 MB | 3115.0 MB | 3125.1 MB |

Four times the trees, four times the memory, in every column. The declaration
changes the report and does not change the behaviour.

## What was ruled out

- **Node size.** 50,000 rounds x 511 nodes = 25.5 M nodes in 787 MB is about 31
  bytes a node. Nodes are the size they should be; there are simply all of them.
- **The missing declaration.** Adding `impl Owned for Tree` is what RFC-0096 M2
  asks for on a self-referring type, and `own.rs`'s guard documents that without
  one the places leak by design. It was added. The numbers did not move.
- **The generic helper.** `give<T>(v: consume T)` monomorphized to `Tree` was
  the obvious suspect — a generic that erases the type would drop nothing. A
  non-generic `give(v: consume Tree)` leaks the same 3.1 GB. `std/html` uses the
  generic spelling (`htmlGive`), so this would have been a leak in std as well.
- **The temporary.** `check(make(depth))` gives the tree no binding for a
  release to attach to. Binding it — `let t = make(depth)` — makes
  `why --memory` report the release, and does not change the memory.

## What this means for the benchmark

RFC-0104 attributed binary-trees' 2.11x and its 2094 MB peak to "double payload
boxing, never freed". The second half is this, and it is not a benchmark
problem: any program that builds a self-referring value in a loop leaks it,
whatever it declares.

## What is NOT claimed

Where the release goes missing. Three candidates are untested: the declared
release may not be called at all; it may be called and free only the outer node;
or the recursion through `drop` inside the impl may not reach the boxes. Telling
them apart needs the emitted IR read, or a counter in the shim, and neither was
done. `examples/binarytrees.vyrn` keeps the declaration — it is what the
language asks for, and this file is why the memory does not follow.
