# RFC-0079 — Failure Is a Value, and Crashing Is the Caller's Call

- **Status:** **Accepted.** M1 and M2 shipped; M3 not yet started.
- **Depends on:** RFC-0078 (which recorded this as open language question **B**
  and refused to decide it), RFC-0009 (`Validation`/`Issue` — the existing error
  model, and why this is not it), RFC-0060 (divergence-aware movecheck, which
  `Never` reuses rather than adds), RFC-0037 (match arms are expressions),
  RFC-0030 (`if` as an expression)
- **Supersedes:** RFC-0078's "B. An abort primitive" section, which priced a
  compiler-internal `@abort(kind)` over a closed catalogue. That design is
  **rejected here**, and the reason is recorded below rather than dropped.

## The problem, in one paragraph

`slice` is implemented three times by hand — an arm in the interpreter, a branch
in the textual backend, and its own lowering in the direct wasm backend. Three
hand-written copies of one function is the exact shape of the two defects this
month: `charCount` had three implementations, and `?`-out-of-a-region had two
lowerings of which one was wrong for six weeks. RFC-0078 moved twelve builtins
into Vyrn and could not move this one, because `slice` **traps** on a bad range
and Vyrn has no expression that terminates. `sliceV` exists in `std/strpred`,
was proven equal to the builtin over ten ranges in ten processes, and cannot be
swapped in because it returns `None` where the builtin exits 1.

## What this RFC decides

Three things, and they compose into a fourth for free.

### 1. Failure carries a reason the caller can match on

`slice` returns a `Result` over a payload enum, not an `Option`:

```vyrn
enum SliceError { OutOfRange(Int64), SplitsCharacter(Int64) }

export fn slice(s: String, start: Int64, end: Int64) -> Result<String, SliceError>
```

```vyrn
match line.slice(a, b) {
    Ok(v) => use(v),
    Err(SliceError.OutOfRange(i)) => report(i),
    Err(SliceError.SplitsCharacter(i)) => realign(i),
}
```

This is strictly better diagnostics than the status quo, not merely equal to it.
The builtin has two fixed strings — `error: slice index out of range` and
`error: slice splits a UTF-8 character` — and no way to tell you *which index*.
The enum carries it. Note also that `sliceV` returning `Option` **collapsed both
cases into one `None`**, which is why RFC-0078's "the swap is one wrapper line"
estimate was wrong; a `Result` is what makes it right.

`SliceError` is `std/`'s **first error enum** — there are none today — so it sets
the house pattern deliberately: small per-operation enums, not one shared error
type. `Issue` (RFC-0009) is not that pattern and is not being replaced; it models
*field validation* accumulated across a value, which is a different job.

### 2. Crashing takes a reason, and the caller writes it

```vyrn
panic("scanner produced a range it computed itself")
```

Type `Never`, unifies with any type, prints to stderr, exits 1.

### 3. Handle-or-default is one token

```vyrn
let name = cfg.slice(0, n) ?? "anonymous"
```

`??` pairs with the `?` that already means propagate. It works on `Option<T>` and
on `Result<T, E>`, and it discards the error — a caller who wants the reason uses
`match`, which is what (1) is for.

### The fourth thing, for free

`??` and `panic` compose, because `Never` unifies with anything:

```vyrn
return slice(sc.src, start, sc.pos) ?? panic("indices came from walking this string")
```

That is `unwrap`, and it needs no primitive of its own, no message of its own, and
no entry in any catalogue. The reason is written by the person who knows it, at
the site where they know it, and `grep -n "?? panic"` finds every place in a
program that chose to die.

## Why the RFC-0078 design was rejected

RFC-0078 priced a compiler-internal `@abort(kind)` selecting from the existing
`@.trap.*` catalogue, and argued against a user-callable `panic(msg)` on one
ground: parity compares stderr byte for byte across three engines, and
library-authored text on that channel is text the compiler no longer
single-sources.

**Requirement (1) dissolves that objection rather than overriding it.** If `slice`
returns a `Result`, then `std/` never panics — it returns errors, and only user
code chooses to crash. The compiler's catalogue stays compiler-only and stays the
single source for every trap it already owns. A `panic` message is deterministic:
the same program prints the same bytes on all three engines, so byte-parity holds
by construction. The hazard was never "arbitrary text exists"; it was "the same
failure has two authors", and returning a value instead of trapping means it has
one.

The internal-catalogue design also had a cost that only became visible against
requirement (1): it would have **grown** the catalogue to cover each routed
builtin, while pinning `slice` to two fixed strings that cannot name the index.
This design shrinks the catalogue's job instead — `slice` leaves it entirely.

## Why not `Option` everywhere and no crash at all

The laziest design is to have `slice` return `Option` and add nothing. It was
checked against the real call sites and it does not work. From
[`std/scan.vyrn`](../std/scan.vyrn):

```vyrn
export fn ident(sc: modify Scanner) -> String {
    let start = sc.pos
    while !atEnd(sc) && isWordByte(peek(sc)) { advance(sc) }
    return slice(sc.src, start, sc.pos)
}
```

`start` and `sc.pos` both came from walking that same string, so the range cannot
be bad. Under `Option`-only this function must either return `Option<String>` and
poison every caller up the scanner, or write a `match` whose `None` arm has
nothing to put in it. There are ~26 call sites with that shape, most of them
scanners, and `std/contract.vyrn`'s `trimSpaces` computes both indices by scanning
its own argument.

`Option`-only does not remove the need to crash. It **relocates** it into 26
places that each have to invent something for a case that cannot happen. The
design above puts it in one place per call site, spelled `?? panic("…")`, and the
26 sites become honest rather than evasive.

## The boundary, drawn deliberately

**`a[i]` stays a trap.** Element access returning a `Result` is unusable in a
loop, and the pressure to write `??` on every index would be constant. Rust hit
this exact wall and kept `v[i]` panicking beside `v.get(i)`; this RFC makes the
same call for the same reason. `at` and `@swapRemove` therefore stay in the
census as `Control`, and the census's `Control` row does not empty.

The line is: **a function may return its failure; an operator in a hot path may
not be made to.**

## Milestones

### M1 — `Never` and `panic` — **shipped**

The one real primitive. `Never` in the checker: a type that unifies with any
type, produced only by `panic`, and treated as divergent by movecheck — which is
reuse, not new machinery, because RFC-0060 already made movecheck
divergence-aware for `break` and `continue`.

`panic(msg: String) -> Never` in all three engines, each on an exit path that
already exists:

| engine | lowering |
|---|---|
| interpreter | `Err(msg)` on the trap path, same as every `@.trap.*` |
| textual | `fputs(msg, stderr)` + `exit(1)` |
| direct wasm | `fd_write(2, …)` + `proc_exit(1)` |

The compiler prefixes `error: ` and appends a newline, so the channel stays
uniform with the trap catalogue and a caller cannot accidentally produce a line
that does not look like the rest. Exit code 1, matching every existing trap.

Pin: a three-engine parity case whose program panics with a message containing a
non-ASCII byte and a `\{}` interpolation, proving the bytes agree.

**As landed.** `a_panic_says_the_same_bytes_on_all_three_engines` in
`compiler/vyrn-cli/tests/parity.rs`, in three programs: the message pin, a
`panic` two regions deep, and every join shape `Never` has to survive with the
panic NOT taken. Two things the plan above did not say:

- The textual backend needs **no special case in its `phi`**. The block ends in
  `unreachable` and a dead one opens, so a panicking arm reaches the merge as
  `poison`, which is valid at every LLVM type. What the joins did need is to stop
  *reporting* `Never` — one arm having no type is the whole change.
- The direct backend needs **no new runtime function**. It writes the three
  pieces and hands the last to `trap`, whose `proc_exit(1)` is the exit path
  every trap already takes; wasm's `unreachable` then makes the stack polymorphic
  so the arm owes its enclosing block no value.

`panic` joins the RFC-0078 census as a `Control` row (61 → 62), which is the row
`slice` leaves at M3.

### M2 — `??` — **shipped**

**This section was drafted as "parser sugar, and nothing else… zero backend
work", desugaring to `match a { Ok(v) => v, Err(_) => b }`. That is impossible,
and the correction is worth more than the original.** `Pattern` (`ast.rs`) is
exactly `Some | None | Ok | Err | Variant` — **there is no wildcard pattern**, so
`Err(_)` cannot be spelled, and `check_match` computes its expected tags
*by name* from the scrutinee's type. The parser holds no type information at all,
so it cannot choose between the `Option` shape and the `Result` shape.

The existing `?` does not solve this problem; it *avoids* it. `Expr::Try` is a
single type-agnostic node: the parser never picks a shape, the checker matches on
`Type::Option` / `Type::Result` to type the result, and every engine lowers it on
the representation the two sums **share** — both carry a tag in the interpreter,
and both are `{ i1, i64, i64 }` in codegen, so `gen_try` branches on field 0
without ever asking which sum it has.

So `??` keeps the instinct — desugar to `match`, and inherit drops, ownership,
validation and short-circuiting from it rather than restating any of them — and
pays for it with **two type-agnostic patterns**, unspellable in source and
produced only by this desugar:

```rust
Pattern::Success(String)   // the tag-1 arm: Some | Ok
Pattern::Failure(String)   // the tag-0 arm: None | Err
```

The parser then emits, with no type knowledge:

```
a ?? b   →   Expr::Match { scrutinee: a, arms: [Success(@v) => @v, Failure(@e) => b] }
```

Every site that must learn the two patterns is inside an existing dispatch:
`check_match`'s expected-tag pair and `binding_type`; `match_pattern` in the
interpreter (shared by `match`, `if let` and `while let`); `pattern_is_one` and
`pattern_binding` in the textual backend; `pattern_binds` in the direct backend.
Roughly fifteen lines across four files.

**The alternative was priced and rejected.** An `Expr::Nullish` node mirroring
`Expr::Try` needs four real lowerings *plus* about twelve one-line arms in
Expr-traversal helpers — drop analysis, escape analysis, predicate summaries.
That scattered-obligation shape is precisely what produced "`?` out of a region
had two lowerings and one was wrong for six weeks". `Expr::Match` is already
traversed everywhere, so the pattern-pair route adds no traversal obligation at
all.

Three details that are easy to miss:

- **`Pattern::Failure` needs a real binder on the `Result` path**, or the error
  payload leaks. On the `Option` path it binds nothing, which is what
  `check_match`'s existing per-arm `Option<&str>` binding seam is for.
- **`??` is maximal-munch, and that silently re-reads an existing spelling.**
  Today `x??` lexes as two `Tok::Question` and parses as `Try(Try(x))` — legal
  for a `Result<Option<T>, E>` in an `Option`-returning function. Afterwards that
  spelling must be written `(x?)?`. There are zero occurrences across `std/`,
  `examples/` and `compiler/`, so it costs nothing now; it is recorded here so it
  is not discovered later.
- **`vyrn fmt` costs one row** in the token-to-text table, which is what keeps
  RFC-0017's re-lex-equality invariant intact.

Precedence: binds tighter than `||` and `&&`, looser than comparison, so
`a ?? 0 == c` parses as `(a ?? 0) == c`. The binding-power table in `binop` is
flat and contiguous — `OrOr = 1`, `AndAnd = 2`, `EqEq = 3` — so there is **no
room** between `&&` and `==`, and every tier from 3 up shifts by one. That is
mechanical but it is not the one-line table insert this section originally
implied. `??` is also right-associative (`a ?? b ?? c` is `a ?? (b ?? c)`), which
the left-associative `binary()` loop cannot express as written; the standard fix
is to recurse at the same binding power rather than at `bp + 1`. Left
associativity is not merely unidiomatic here, it fails to typecheck: `a ?? b`
yields an unwrapped `T`, so `(a ?? b) ?? c` applies `??` to a non-sum.

M2 must land **after** M1, not beside it: four of the five files it touches are
M1's.

**As landed.** The pattern-pair design held exactly as written — the two patterns
are `Pattern::Success`/`Pattern::Failure` in `ast.rs`, the parser builds the
`match`, and no engine gained a node. `vyrn-cli/tests/nullish.rs` covers both
sums, chaining, precedence, and short-circuiting via an observable side effect;
`nullish_and_panic_say_the_same_bytes_on_all_three_engines` in `parity.rs` covers
the composition and asserts the wasm column actually ran, because
`three_engines` skips it silently when `wasmtime()` finds nothing.

Six things above were wrong, four of them cheaper than stated and two of them
stated backwards:

- **The precedence example contradicts the precedence rule.** "Looser than
  comparison" and "`a ?? 0 == c` parses as `(a ?? 0) == c`" cannot both hold —
  the second describes `??` binding *tighter* than `==`. The rule won, so
  `a ?? 0 == c` is `a ?? (0 == c)`. That grouping is a type error on an
  `Option<Int64>`, so the reading that is not wanted fails loudly at the check
  rather than quietly at runtime, which is what makes the choice safe.
- **No table shift was needed.** "Every tier from 3 up shifts by one" assumed
  `??` had to sit *in* `binop`'s table. It does not — it is not a `BinOp`, it is
  handled by hand in `binary()` and recurses at its own binding power for right
  associativity. Sharing power 3 with `EqEq` and shifting nothing was checked
  case by case against every `min_bp` the loop passes and produces the identical
  parse. Seventeen edited rows, zero behaviour change, so the rows stayed.
- **`binding_type` needed no change**, because `Success`/`Failure` resolve to the
  concrete tag string (`want[0]`/`want[1]`) before it is called.
- **Three sites the list above missed**, all of them one line: `tag_test` in the
  direct backend (beside `pattern_binds` — it decides tag 1 vs tag 0),
  `movecheck::pattern_bindings`, and the loader's two binder-scoping walks.
  `check_match`'s sibling `pattern_binders` (`if let`/`while let`) needed an
  exhaustiveness arm only, spelled `unreachable!` — `??` desugars to a `match`
  and nothing else.
- **`vyrn fmt` cost zero rows, not one.** The default spacing rule already emits
  `a ?? b` and re-lex equality already held. The row that was really added is in
  `token_name_and_text` — the RFC-0054 `lex()` builtin's table, not fmt's.
- **The `(x?)?` migration note is moot.** The spelling it worried about needs a
  `Result<Option<T>, E>`, and nested sums are refused outright ("nested
  Option/Result is not supported in v0.1"), so `Try(Try(x))` was unreachable on
  every type, not merely unused. The munch takes nothing away.

And one detail that is true but buys less than it claims: **`Failure`'s binder
does not stop a leak**, because nothing drops a `match` arm's payload binder.
`own.rs` makes only `let` bindings droppable, so a hand-written `Err(e)` arm does
not free `e` either — a discarded error payload is the same safe leak in both
spellings. The binder is still the right shape (it keeps the two arms symmetric,
and the checker's per-arm binding seam is where the `Option` path opts out), and
what it actually guarantees is pinned as such: `??` emits exactly the frees the
`match` a user would write by hand emits. When arm binders become droppable, both
spellings gain it together, which is the whole reason `??` is a `match`.

### M3 — `slice` becomes Vyrn

1. `std/strpred` exports `SliceError` and a `slice` returning
   `Result<String, SliceError>`, built from the existing `sliceV` body with the
   range check split so the two failures stay distinguishable.
2. The loader's `std/strpred` row gains `("slice", "strpred$slice")`, and its
   comment — which currently explains why `slice` is *not* routed — is rewritten
   to say what happened.
3. The builtin's three implementations delete: the interpreter arm, the textual
   backend's branch, and the direct backend's lowering. `@.trap.sliceoob` and
   `@.trap.slicesplit` delete with them.
4. The ~26 call sites in `std/` take `??` or `match`. The scanners take
   `?? panic("…")` with a reason naming why the range is known good.
5. `("slice", Control, …)` leaves the census and
   `nothing_is_both_censused_and_routed` enforces that it left.

One documented divergence rides along and should be recorded, not fixed:
[`std/strpred.vyrn`](../std/strpred.vyrn) notes that `sliceV` yields `None` for a
String containing a NUL where the builtin returns the substring, because
`stringFromBytes` rejects NUL (RFC-0014's rule). No program can construct such a
String — there is no `\0` escape, and the lexer now rejects a NUL in a literal
outright — so the case is unreachable from source.

## What this does not decide

**A user-facing `expect`-shaped API on `Result`.** `x ?? panic("…")` covers it in
one line and adds nothing to the type. If a method form is wanted later it is
library code, not a language change.

**Whether `Never` is spellable in a signature.** M1 produces it only from
`panic`; writing `fn f() -> Never` is a separate and much larger question about
divergent functions, and nothing here needs it.

**The raw-memory view** — RFC-0078's open question **A**, and still open. It is
the larger of the two and remains untouched.
