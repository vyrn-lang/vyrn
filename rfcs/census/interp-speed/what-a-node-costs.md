# What one interpreted operation costs, and how small the name lookup is

Measured against `combined`, after the four changes in
`rfcs/census/interpreter-loop-cost.md`. Every figure is the best of three runs of
a two- or three-million-iteration loop, so the per-operation number is a
difference between two programs that differ in one operation.

## The numbers

| operation | cost |
| --- | --- |
| read a variable | **about 20 ns** |
| one more statement, `a = a + b` (two reads, an add, an assignment) | **about 65 ns** |
| walk ONE more scope frame to find a name | **about 1 ns** |

The third row is the one that decides something.

## How the frame figure was taken

The same loop, with `a`, `b` and `i` always in the outermost frame, wrapped in 0,
1, 3 and 5 nested blocks that each declare a binding of their own. Every extra
block is one more frame every read has to walk past.

```
0 extra frames   156 ns/iteration
1 extra frame    159 ns/iteration    +3 ns
3 extra frames   169 ns/iteration   +13 ns
5 extra frames   176 ns/iteration   +20 ns
```

The body performs four reads per iteration, so five extra frames is twenty extra
probes for twenty nanoseconds. **A frame probe that misses costs about one
nanosecond.**

## What that says about slot resolution

Over `vyrn test site/export.vyrn` there are 500,451,689 reads and 652,922,153
probes. The probes past the first — the ones a `(depth, index)` stamp would
remove outright — number about 153 million. At a nanosecond each that is **about
0.15 seconds of a 31-second run.**

The successful scan costs more than nothing: 99.2 per cent of frames hold three
bindings or fewer, so finding a name is one to three `str` comparisons, each
rejecting on length before it compares a byte. Even generously, the whole
name-lookup cost is a small single-digit percentage.

**Slot resolution is not the lever.** It is an AST change, a checker pass, and a
static depth that has to agree with the interpreter's dynamic pushes or a program
silently reads the wrong variable — see `frames-audit.md`, where one of the seven
push sites builds its AST while the program runs. For two or three per cent.

## Where the time actually goes

Twenty nanoseconds to read a variable, when walking a frame costs one. The other
nineteen are the cost of interpreting a node at all: the recursive `expr` call,
the match over sixty-odd arms, the `Result` return, and the `Val` clone.

That is what a bytecode VM or a closure tree attacks, and it is why the next
question is that one and not this one.

## A correction to an earlier reading

An earlier measurement of this looked superlinear — the per-read cost seemed to
climb from 35 ns to 74 ns as an expression got longer, and that was reported as a
loose thread worth chasing. It was an arithmetic mistake in the measuring script,
which divided every step by two while the step sizes were two, two and four. The
corrected series is roughly flat at 19 to 27 ns per read.

A second hypothesis died with it. If deep expressions were expensive because of
recursion, then eight flat statements should beat one eight-deep expression. They
do not: eight statements cost 634 ns per iteration against 325 for the one deep
expression. Depth is cheap; statements are not.
