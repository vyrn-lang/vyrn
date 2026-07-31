# RFC-0079 — Failure Is a Value, and Crashing Is the Caller's Call

- **Status:** **Accepted.** M1 shipped; M2–M3 not yet started.
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

### M2 — `??`

Parser sugar, and nothing else. `a ?? b` desugars to

```vyrn
match a { Ok(v) => v, Err(_) => b }      // Result
match a { Some(v) => v, None => b }      // Option
```

which means **zero backend work** and short-circuiting that is correct by
construction — `b` sits in an arm that only the failing tag reaches. Both engines
and both backends already lower `match`, so `??` inherits drops, ownership and
validation from it rather than restating them.

Precedence: binds tighter than `||` and looser than comparison, so
`a ?? b == c` parses as `(a ?? b) == c`. Right-associative, so `a ?? b ?? c` is
`a ?? (b ?? c)`.

M2 is independent of M1 and can land in either order; only M3 needs both.

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
