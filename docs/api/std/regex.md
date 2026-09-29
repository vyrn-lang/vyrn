# std/regex

std/regex: a regular expression that searches, counts and replaces.

`=~` only answers whether a whole string matches a constant pattern. The walk
is Vyrn, not a builtin, so every engine runs one source and agrees on offsets
and counts.

Supports literals, `.`, classes (`[abc]`, `[a-z]`, `[^>]`), alternation,
grouping, `*` `+` `?`, and `\` escapes. A Thompson NFA with no backtracking,
so the time is linear in the pattern size times the input for every pattern.
A search runs it as a lazy DFA: one table read per byte.
Refuses anchors, non-greedy repeats and counted repetition. It has no
backreferences or lookaround: `\1` is the byte `1`.

Leftmost-longest (POSIX): `a|ab` against `ab` matches `ab`, where a
backtracking engine answers `a`.

Offsets are bytes, and every match starts and ends on a character boundary:
`.` and a negated class match one whole UTF-8 character. A class that lists
a non-ASCII character is a set of its bytes.

## Regex

```vyrn
type Regex = { op: Array<Int64>, a: Array<Int64>, b: Array<Int64>, cls: Array<ByteSet>, start: Int64, classOf: Array<Int64>, classCount: Int64 }
```

## Match

```vyrn
type Match = { at: Int64, end: Int64 }
```

One match: a half-open byte range of the haystack.

## compile

```vyrn
fn compile(pattern: String) -> Result<Regex, String>
```

Compiles `pattern`, or says what is wrong with it. Compile once and search
often: the whole cost of a pattern is paid here.

## find

```vyrn
fn find(re: Regex, hay: String, from: Int64) -> Option<Match>
```

The first match at or after `from`, or `None`.

## countMatches

```vyrn
fn countMatches(re: Regex, hay: String) -> Int64
```

How many non-overlapping matches `re` has in `hay`: `aa` in `aaa` is one. An
empty match advances by one byte.

## replaceAll

```vyrn
fn replaceAll(re: Regex, hay: String, with: String) -> String
```

`hay` with every non-overlapping match replaced by `with`, taken literally:
there are no captures, so no `$1`.
