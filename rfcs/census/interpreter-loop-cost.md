# Where an interpreted loop spends 167 nanoseconds

The CI census ranked the site's own test suite first of the changes worth
making, and could not say what was inside it. `vyrn test --profile` said `slice`
was 59 per cent of it. This file is the next question down: what a `slice`
iteration actually costs, and what came off it.

Everything here was measured, and two of the measurements said the opposite of
what the reasoning said.

## The counts

A temporary probe in `Interp::expr`, over `vyrn test site/export.vyrn`:

```
variable reads              500,451,689
  of those, a variant                 4
frames probed               860,691,511   (1.72 per read)
  fell through to globals        33,491
coerce calls                183,682,210
  answered no-op            179,316,749   (97.6%)
function calls                4,677,890
```

Four numbers decided four changes.

### 500,451,689 asked whether the name was a variant; four said yes

A nullary variant wins over a local of the same name, and it is observable:
`let Red = 7` binds a local and reading `Red` still gives the variant. So the
question has to be asked. Asking cost a `HashSet<String>` hash.

A 256-entry table of the first bytes any variant name starts with answers almost
every one with an array index. Exact, not a heuristic — a name whose first byte
no variant begins with cannot be a variant — so the rule's order and the
program's behaviour are unchanged.

### 1.72 frames probed per read, and most of the extra found nothing

`Stmt::Let` is the only statement that writes into the frame a block opens.
Every other binder — a `match` arm's payload, an `if let`, a `for in` variable —
pushes a frame of its own first, which was checked rather than assumed: all three
`scope.last_mut().insert` sites push immediately before.

So a `while` body that declares nothing opens a map nothing ever writes to.
Skipping it took 1.72 probes per read down to 1.30, and removed 208 million
lookups.

### 99.2 per cent of probes are into a frame of three bindings or fewer

The distribution, not a preference:

```
frame probes by frame size, total 652,922,153
  size 0:  15,320,274   ( 2.3% cumulative)
  size 1: 138,229,088   (23.5%)
  size 2:  16,685,143   (26.1%)
  size 3: 477,409,293   (99.2%)
```

A hash map is the wrong structure for three entries. Hashing a name to pick a
bucket costs more than comparing three names. `Frame` became a `Vec` and a linear
scan, and that single change was worth more than everything else here put
together.

### 97.6 per cent of coercions do nothing

Reaching that answer took three nested calls — `coerce`, `coercion_is_noop`,
`coercion_is_identity` — for a type whose first arm says yes. The arm is hoisted
and inlined; the list is copied verbatim so the two cannot disagree. Where the
calls come from:

```
let                 84,714,602
call parameter       8,108,161
call return          4,677,890
if-let binding       1,045,631
assignment             804,458
```

## What it added up to

Interleaved — built both ways, measured in one window — because an earlier
comparison against a twenty-minute-old baseline gave the wrong SIGN.

| | before | after |
| --- | --- | --- |
| `vyrn test site/export.vyrn` | 46.8 s | **31.1 s** |
| `vyrn run site/export.vyrn` | 62.0 s | **23.8 s** |

The export figure is against the start of the arc and includes the `Rc` work in
`rfcs/census/interpreter-value-copies.md`.

## The thing that did not work

`Frame` holds `Vec<(String, Slot)>`. A `Slot` is 96 bytes and a pair is 120, so
three entries span six cache lines and every name comparison in the scan reaches
a different one. Splitting into `names: Vec<String>` and `slots: Vec<Slot>` puts
three name headers in about one line, and should have been faster.

It was **4 per cent slower**: 30.8 s against 32.0 s, interleaved. Two vectors
mean two allocations per frame and two pointer chases, and `rposition` over one
vector then an index into another did not beat the fused scan. Reverted.

Written down because the reasoning was good and the answer was still no.

## What is left

- **653 million frame probes that do find something.** Resolving each `Expr::Var`
  to a `(depth, index)` at check time removes the scan entirely. It is an AST
  change, a checker pass, and a static depth that has to match the interpreter's
  dynamic pushes exactly or a program reads the wrong variable. The prize is
  now three length-compares per probe, which is much smaller than it was when
  this arc started.
- **`slice` is still the top function**, at 15.8 s of a 31 s step against 35.2 s
  of a 47 s one. Halved, still first. See
  `rfcs/census/slice-is-half-the-site-build.md` — the decision there is unchanged
  in kind and smaller in size.
- An empty interpreted loop iteration was 167 ns when this started. The reads
  inside it are what the four changes above went after.
