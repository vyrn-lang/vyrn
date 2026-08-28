# std/regex

A regular expression that SEARCHES — counts, finds and replaces.

WHY THIS EXISTS (RFC-0112). The language has had a regex engine since
RFC-0005, reached through `=~`, and it answers exactly one question: does
this whole string match this compile-time-constant pattern? It has no
search, no count, no position and no substitution, so `regex-redux` — which
counts nine patterns across a sequence and then rewrites it five times —
could not be written. `rfcs/bench-0104/regexredux-1000.expected` sat in the
corpus with no program beside it.

WHY IT IS VYRN AND NOT A BUILTIN. `rfcs/census/blocked-regex.md` measured
the split: the expensive half of a regex engine (parsing, determinisation,
the table format) is already written in Rust and reusable, and the CHEAP-
looking half, the walk, is where the parity risk is. A searching walker as a
builtin would be spelled three times — Rust for the interpreter, LLVM IR for
the textual backend, wasm for the direct one — and the three would have to
agree on offsets and counts byte for byte, not merely on a `Bool`. Written
here it is spelled ONCE and the three engines run the same source. That is
the whole argument, and it is the standing rule about backends applied
exactly.

WHAT IT SUPPORTS. Literals, `.`, classes (`[abc]`, `[a-z]`, `[^>]`),
alternation, grouping, and the three repeats `*` `+` `?`. Escapes with `\`.
The linear-time guarantee that comes with a Thompson construction is kept:
there is no backtracking here, so there is no pattern that takes
exponential time, which is the whole reason to prefer this shape.

WHAT IT DOES NOT SUPPORT, and will refuse rather than mis-answer:
backreferences, lookaround, anchors (`^`, `$`), non-greedy `*?`, and
counted repetition `{m,n}`. The first two are incompatible with the
linear-time guarantee — that is why RE2, Go and Rust drop them too. The
other three are ordinary work nobody has needed yet.

MATCH SEMANTICS: leftmost-longest, which is POSIX's rule and RE2's
`POSIX` mode. `a|ab` against `ab` matches `ab` here, where a backtracking
engine would answer `a`. Stated because it is a real difference and a
caller porting a pattern from Perl should know which one they have.

## Regex

```vyrn
type Regex = { op: Array<Int64>, a: Array<Int64>, b: Array<Int64>, sets: Array<UInt8>, start: Int64, first: Array<UInt8>, firstKnown: Bool }
```

A compiled pattern.

`sets` holds one 32-byte bitmap per class, so class `k` owns bytes
`k * 32 .. k * 32 + 32` and byte `c` is in it when bit `c % 8` of
`sets[k * 32 + c / 8]` is set. A bitmap rather than 256 booleans because a
pattern with twenty classes is 640 bytes this way and 5,120 the other.

## Match

```vyrn
type Match = { at: Int64, end: Int64 }
```

One match: a half-open byte range of the haystack.

## compile

```vyrn
fn compile(pattern: String) -> Result<Regex, String>
```

Compile `pattern`, or say what is wrong with it.

The whole cost of a pattern is paid here, so a caller that scans with the
same pattern many times compiles once and searches often — which is what
`regex-redux` does nine times over one sequence.

## find

```vyrn
fn find(re: Regex, hay: String, from: Int64) -> Option<Match>
```

The first match at or after `from`, or `None`.

## countMatches

```vyrn
fn countMatches(re: Regex, hay: String) -> Int64
```

How many non-overlapping matches `re` has in `hay`.

Non-overlapping and leftmost: a match consumes what it covered, so
`aa` in `aaa` is one match and not two. An empty match advances by one
byte, which is what stops a pattern like `a*` looping forever.

## replaceAll

```vyrn
fn replaceAll(re: Regex, hay: String, with: String) -> String
```

`hay` with every non-overlapping match replaced by `with`.

`with` is literal: there is no `$1` and no backreference, because there are
no captures to name. That is deliberate and not an omission — see the module
note on what a linear-time engine gives up.
