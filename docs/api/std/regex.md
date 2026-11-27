# std/regex

A regular expression that searches, counts and replaces.

`=~` only answers whether a whole string matches a constant pattern. The walk
is Vyrn, not a builtin, so every engine runs one source and agrees on offsets
and counts.

The language is `std/regex.syntax`, the one `=~` reads, and a pattern means what
it means there: `.` is any character but `\n` and `\r`. `\d`, `\w`, `\s` and `\h`
are shorthands, their capitals negate them, and `\N` is any character but `\n`. `\t`,
`\n`, `\r`, `\f`, `\v`, `\a`, `\e` and `\cX` are control characters, and `\xHH`,
`\x{H..}`, `\uHHHH`, `\u{H..}` and `\o{O..}` are code points. `\R` is a line
break, `\Q` quotes the text up to `\E` or the end, and a backslash before ASCII
punctuation makes it literal. Every other escape is refused. `{m,n}` repeats a
term `m` to `n` times, each count at most 255, and a `?` after a repeat or a
count makes it lazy. `(?:..)` groups without a name, `(?<n>..)`, `(?P<n>..)`
and `(?'n'..)` name a group, and `(?#..)` is a comment.
The flags `i` (ASCII case folding), `s` (`.` takes `\n` and `\r`) and `m` hold
for the rest of a group after `(?ims-ims)`, or inside `(?ims-ims:..)`. `^`, `$`
and every other assertion are refused, and the empty pattern matches the empty
string. A Thompson NFA with no backtracking, so the time is linear in the pattern
size times the input for every pattern. A search runs it as two lazy DFAs, one
forward to a match's end and one back to its start: one table read per byte. It
has no backreferences or lookaround.

Leftmost-first, as Perl, PCRE and RE2 match: of the matches that start
leftmost, the one the pattern prefers, so an earlier alternative wins, a greedy
repeat takes as much as it can and a lazy one as little. `a|ab` against `ab`
matches `a`.

A class is a set of code points: it unites its members, ranges, POSIX classes
such as `[:alpha:]` and nested classes, and `&&`, `--` and `~~` intersect,
subtract and take the symmetric difference of the unions beside them. A literal,
a class and `.` each match one whole character, so offsets are bytes and every
match starts and ends on a character boundary.

## Regex

```vyrn
type Regex = { fwd: Program, rev: Program }
```

A compiled pattern: `fwd` is the pattern after a loop over any byte, so a forward
pass finds where the leftmost match ends, and `rev`, its reverse, reads that match
back to its start.

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

The first match at or after `from`, or `None`. It reads `hay` from `from` to the
match's end, and back to its start.

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
