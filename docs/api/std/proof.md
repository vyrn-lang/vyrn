# std/proof

Comparisons the compiler proves where a call states them.
`prove` is compiler-known: the compiler reads it by this declaration.

## prove

```vyrn
fn prove(c: Bool) -> Unit
```

Asks the compiler to prove `c` where the call stands. `c` is one integer
comparison or several joined by `&&`. A `c` the prover cannot show is a
refusal that names the comparison and why, and after the call every later
check may assume `c`. The call checks nothing at run time.
