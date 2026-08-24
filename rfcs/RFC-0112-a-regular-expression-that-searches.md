# RFC-0112 — A Regular Expression That Searches

- **Status:** Accepted. Implemented in this branch.
- **Closes:** the `p-regex` gap named in
  [RFC-0104](RFC-0104-a-benchmark-is-a-claim-about-a-gap.md):408-416.
- **Evidence:** [rfcs/census/blocked-regex.md](census/blocked-regex.md).

## The gap in one sentence

`=~` answers whether a whole string matches a compile-time-constant pattern,
and `regex-redux` needs the other three questions: how many, where, and with
what replaced.

## What it looked like

`=~` takes only a string-literal pattern, compiles it to a DFA at compile time,
and answers one `Bool` for the whole string. It has no search, no count, no
position, no capture and no substitution, and `std/strings`' `replace` takes a
literal `from`. So a program could ask "is this entire sequence a run of
`[acgt]`" and could not ask "how many times does `agggtaaa|tttaccct` occur in
it" — which is nine of the fifteen things `regex-redux` does.

`rfcs/bench-0104/regexredux-1000.expected` stayed committed with no program
beside it.

## What is added

`std/regex`, written in Vyrn:

```vyrn
export fn compile(pattern: String) -> Result<Regex, String>
export fn count(re: Regex, hay: String) -> Int64
export fn find(re: Regex, hay: String, from: Int64) -> Option<Match>
export fn replaceAll(re: Regex, hay: String, with: String) -> String
```

A Thompson NFA held as flat integer arrays, simulated without backtracking. The
compile cost is paid once per pattern, which is what a program scanning one
sequence with nine patterns wants.

## Why Vyrn and not a builtin

This is the decision the census set up, and it is the standing rule about
backends applied directly.

`rfcs/census/blocked-regex.md` measured the split and found the two halves have
opposite shapes. **The expensive half is already written and reusable**:
parsing, expansion budgets, determinisation and the table format all exist in
`vyrn-frontend/src/regex.rs` and would need no change to serve a search.
**The cheap-looking half is where the parity risk is**: all three existing
runners are built around the anchored assumption, so a searching walker would
have to be spelled three times — Rust for the interpreter, LLVM IR for the
textual backend, wasm for the direct one — and those three would have to agree
on offsets and counts byte for byte, not merely on a `Bool`. `direct.rs` already
carries a comment complaining about the triplication of the full-match walk.

Written in Vyrn the walker is spelled ONCE and the three engines run the same
source. Parity is not something to test for afterwards; it is a property of
there being one implementation. The three-way corpus confirms it rather than
establishing it.

The cost is speed, and it is real. The census priced an interpreted scan at
about 1.5 MB/s, so the game's 5,000,000-base size would take roughly a minute
under the interpreter and less compiled. As a competitive benchmark entry that
loses. As a closed gap with correct output it passes, and the corpus size —
where the fixture lives — is instant.

## What it supports, and what it refuses

Supported: literals, `.`, classes (`[abc]`, `[a-z]`, `[^>]`), alternation,
grouping, the three repeats `*` `+` `?`, and `\` escapes.

Refused, by name, at compile time:

| refused | why |
|---|---|
| backreferences, lookaround | incompatible with the linear-time guarantee — the reason RE2, Go and Rust drop them too |
| anchors (`^`, `$`) | ordinary work, nobody has needed them |
| non-greedy (`*?`, `+?`) | ordinary work, nobody has needed it |
| counted repetition (`{m,n}`) | ordinary work, nobody has needed it |

The bar is deliberately the RE2/Go/Rust set: give up backreferences and
lookaround, keep the linear-time guarantee, and say so. The catastrophic case
is a property of backtracking, not of ambition, and there is no backtracking
here.

**Match semantics are leftmost-longest** — POSIX's rule, and RE2's `POSIX` mode.
`a|ab` against `ab` matches `ab` here where a backtracking engine answers `a`.
Stated in the module's own doc comment as well, because a caller porting a
pattern from Perl needs to know which one they have. None of `regex-redux`'s
fifteen patterns can tell the difference — every alternative is equal-length or
mutually exclusive — so the choice is free here and is made on the grounds that
it is the one a Thompson walk gives naturally.

## The first-byte filter

A naive search attempts a match at every position. `compile` walks the split
graph from the start state without consuming anything and records the bytes
that can begin a match, as a 32-byte bitmap; the search skips any position
whose byte is not in it. For a DNA pattern over a sequence that is three
quarters not-`a`, that is most of the work.

A pattern that can match the empty string has no such set — every position
begins a match — and `firstKnown: false` says exactly that rather than an empty
bitmap quietly meaning "nothing matches".

## What is NOT built

- **No generator.** The census ranked a `gen fn` composed with this walker
  first, for compile-time pattern validation. It is a strict addition on top of
  what is here — the walker does not change — and it can be built when
  something wants it. Nothing does yet: all fifteen of `regex-redux`'s patterns
  are literals of its source and could be validated at compile time, but a
  refusal at run time from a literal pattern is a panic on the first line of
  `main`, which is not a class of bug worth new machinery to move.
- **No captures.** `replaceAll` takes a literal replacement with no `$1`,
  because there are no groups to number. Adding capture slots to the walk is
  the standard Pike-VM extension and is not needed by anything today.
- **No change to `=~`.** It is still an anchored full match against a
  compile-time pattern, still checked at compile time, and still the right tool
  for a validation predicate. Two things that look alike answer different
  questions, and the module's doc comment says which is which.

## Parity

`examples/regexredux.vyrn` is in the three-way corpus with
`examples/regexredux.stdin`, and its output equals
`rfcs/bench-0104/regexredux-1000.expected` — nine counts and three lengths —
under the interpreter, the native binary and wasm.

`compiler/vyrn-cli/tests/benchgame.rs` now lists all TEN game programs, and
gained a gate the corpus never had: `no_fixture_is_left_without_a_program`. Two
fixtures sat unpaired for the whole of M1 and M2. That was deliberate and
written down, but nothing enforced it, so a third could have joined them and
only a reader of RFC-0104 would have known.
