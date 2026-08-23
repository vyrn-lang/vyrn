# Census — Refinement subsumption: skipping checks already guaranteed

If one type forbids the characters `a` and `b`, and another forbids only `a`, a value of the first satisfies the second. Can the compiler know that and skip the check? What about other contracts, such as a condition function?

This is refinement typing and subsumption. The question has two halves: what languages let a type carry a predicate, and which of them decide containment between two predicates automatically — and at what cost, and with what fallback when the decider cannot finish.

---

## Liquid Haskell

Liquid Haskell adds refinement types to Haskell by annotating every type with a logical predicate over its variables:

```haskell
{-@ type Nat  = {v:Int | 0 <= v} @-}
{-@ type Pos  = {v:Int | 0 <  v} @-}
```

A value of `Pos` is a `Nat` because `0 < v` implies `0 <= v`. Liquid Haskell proves this by sending the implication to an SMT solver (Z3 by default). The solver checks the logical formula `forall v. (0 < v) => (0 <= v)`, which is in the decidable fragment of linear integer arithmetic (LIA). The solver returns "valid", and Liquid Haskell records the subtyping judgment.

The predicate class is **decidable SMT theories**: linear arithmetic over integers and reals, uninterpreted functions, booleans, algebraic data type constructors, and sets/maps (via Z3's array theory). A predicate outside these theories — arbitrary function calls, string operations, non-linear arithmetic — is rejected at the annotation level. Liquid Haskell forbids it rather than falling back to runtime.

Subsumption is automatic. The user writes the refinements; the solver proves the implications. The user does not write proofs for subtyping.

Compile-time cost: every type-checking query generates SMT constraints and calls Z3. Liquid Haskell caches verification conditions across runs, but a fresh check of a large codebase takes minutes to tens of minutes. The solver is the dominant cost.

When the solver cannot decide: Liquid Haskell reports an error. There is no runtime fallback. A refinement that the solver cannot prove is a failed verification, not an unverified assumption. The user must weaken the refinement, add lemmas (annotated `{-@ lemma :: ... @-}` with a proof body), or mark the function `lazy` or `unsafe` to bypass.

Key references: Rondon, Kawaguchi, Jhala, "Liquid Types" (PLDI 2008, https://goto.ucsd.edu/~nvazou/Liquid_Types_PLDI08.pdf); Vazou et al., "LiquidHaskell: Experience with Refinement Types in Haskell" (Haskell Symposium 2014, https://arxiv.org/abs/1503.00148); the Liquid Haskell documentation (https://ucsd-progsys.github.io/liquidhaskell/).

|property|answer|
|---|---|
|predicate class|SMT-decidable theories: LIA, uninterpreted functions, ADTs, sets|
|subsumption|automatic, via Z3|
|solver needed|yes (Z3)|
|compile-time cost|high — solver calls per type-checking query|
|solver cannot decide|hard error, no runtime fallback|

---

## F\*

F\* is a proof-oriented language from Microsoft Research and Inria. Its type system combines dependent types, refinement types, and effect systems. Refinements are written as ordinary predicate functions:

```fstar
type nat = x:int{x >= 0}
type pos = x:int{x >  0}
```

A `pos` value is a `nat` because the implication `x > 0 => x >= 0` holds. F\* proves this by sending the verification condition to Z3. The SMT solver discharges it in the same theory Liquid Haskell uses: linear arithmetic, uninterpreted functions, inductive types.

F\* goes further than Liquid Haskell: the user can write explicit proof terms when the solver cannot finish. A lemma is a function whose body is a proof:

```fstar
val lemma_pos_is_nat : pos -> nat
let lemma_pos_is_nat x = x
```

The solver proves the obligation automatically for this trivial case. For harder theorems (cryptographic invariants, protocol safety), the user supplies proof scripts that guide the solver or construct terms by hand.

The predicate class is the same SMT-decidable fragment, extended with F\*'s own dependent type machinery. Non-linear arithmetic, string operations, and arbitrary function calls are outside the automatic fragment. F\* can still verify them if the user supplies lemmas.

Subsumption is automatic for the SMT fragment. The user writes the types; Z3 proves the implications. For harder properties, the user writes proofs.

Compile-time cost: high. Every function body generates verification conditions sent to Z3. The F\* standard library takes minutes to verify. Caching and incremental checking help, but the solver is always in the loop.

When the solver cannot decide: the user writes a proof term or a lemma. If the property is genuinely undecidable for the SMT fragment, the user must either weaken the specification or mark the function `admit` (which leaves a hole).

Key references: Swamy et al., "Dependent Types and Multi-Monadic Effects in F\*" (ICFP 2016, https://www.fstar-lang.org/papers/mumon/); the F\* tutorial (https://www.fstar-lang.org/tutorial/); Bhargavan et al., "Implementing and Proving the TLS 1.3 Record Layer" (IEEE S&P 2017, https://eprint.iacr.org/2018/047).

|property|answer|
|---|---|
|predicate class|SMT-decidable theories + dependent types; user lemmas for harder|
|subsumption|automatic for SMT fragment; user proofs for the rest|
|solver needed|yes (Z3)|
|compile-time cost|high — solver per verification condition|
|solver cannot decide|user writes proof term, or admits the obligation|

---

## Dafny

Dafny is a verification-aware programming language from Microsoft Research. It compiles to C#, Go, Java, or JavaScript, and verifies programs against specifications using Z3.

Dafny does not have refinement types in the type-system sense. Its predicates are preconditions, postconditions, invariants, and decreases clauses — all written as ordinary boolean expressions:

```dafny
type Nat = x | x >= 0
function IsPrime(n: Nat): bool
  requires n >= 2
```

The `type x | predicate` form is a subset type. A value of a narrower subset type flows into a wider one when the implication holds. Dafny sends the implication to Z3, which proves it in linear arithmetic (and other supported theories).

Dafny requires the user to write proofs for non-trivial obligations. A `lemma` is a method whose body establishes a claim, and the user can call it. For simple implications (`x > 0 => x >= 0`), the solver handles it automatically.

The predicate class is SMT-decidable: linear and non-linear arithmetic, sequences, sets, maps, and user-defined functions (treated as uninterpreted by the solver, with axioms from their definitions). Non-linear arithmetic is supported but can be slow or incomplete.

Subsumption of subset types is automatic when the implication is in the solver's decidable fragment. The user does not write a proof for `x > 0 => x >= 0`.

Compile-time cost: high. Each method generates verification conditions for Z3. Dafny's autosolver caches results, but large programs take minutes to hours.

When the solver cannot decide: the user writes a lemma. Dafny's lemma mechanism is first-class — a lemma is a method that proves a property, and calling it adds the property to the solver's context. If the property is outside the solver's reach, the user constructs the proof by hand.

Key references: Leino, "Dafny: An Automatic Program Verifier for Practical Verification of Imperative Programs" (2010, https://pm229.win.tue.nl/papers/leino2010dafny.pdf); the Dafny documentation (https://dafny.org/latest/); Leino, "Automating Theorem Proving with SMT" (2014, https://dl.acm.org/doi/10.1145/2594324).

|property|answer|
|---|---|
|predicate class|SMT-decidable theories: arithmetic, sequences, sets, maps|
|subsumption|automatic for subset types when implication is decidable; lemmas otherwise|
|solver needed|yes (Z3)|
|compile-time cost|high — solver per verification condition|
|solver cannot decide|user writes a lemma; or marks `{:axiom}`|

---

## ATS

ATS (Applied Type System) combines dependent types with linear logic. Its statics (type system) and dynamics (runtime) are separated, and the statics can express refinements over indices:

```ats
typedef Nat = [n:int | n >= 0] int(n)
typedef Pos = [n:int | n >  0] int(n)
```

A `Pos` value is a `Nat` because the constraint `n > 0` implies `n >= 0`. ATS discharges this constraint with its built-in constraint solver, which handles linear arithmetic over integers. The solver is simpler than Z3 — it is a purpose-built arithmetic solver, not a general SMT engine.

ATS goes further than the others in one direction: it can attach proofs to values. A proof is a linear resource that witnesses a property, and the type system tracks its consumption. This lets ATS verify properties that the arithmetic solver cannot — memory safety, protocol conformance, array bounds — by requiring the user to thread proof objects through code.

The predicate class is **linear integer arithmetic** in the constraint solver, plus user-constructed proofs for anything else. String operations, non-linear arithmetic, and arbitrary functions are outside the solver's automatic fragment.

Subsumption is automatic for linear arithmetic constraints. For other properties, the user constructs and threads proof terms.

Compile-time cost: moderate. The constraint solver is lightweight (no external SMT process). The cost grows with proof complexity, not with solver calls.

When the solver cannot decide: the user writes a proof. ATS proofs are linear resources — they are consumed when used, which prevents reuse but also prevents forgetting. The compiler rejects code that does not discharge all proof obligations.

Key references: Xi, "Dependent Types for Practical Programming" (1999, https://www.cs.bu.edu/~hwxi/DML/); Xi, "Applied Type System" (2003, https://www.cs.bu.edu/~hwxi/ATS/); the ATS documentation (http://ats-lang.sourceforge.net/DOCUMENT/INT2PROGINATS/).

|property|answer|
|---|---|
|predicate class|linear integer arithmetic (solver); user proofs for the rest|
|subsumption|automatic for arithmetic constraints; user proofs otherwise|
|solver needed|yes, but lightweight (purpose-built, not Z3)|
|compile-time cost|moderate — lightweight solver, no external process|
|solver cannot decide|user writes linear proof terms|

---

## Ada and SPARK subtype predicates

Ada 2012 introduced subtype predicates. A subtype can carry a predicate that every value must satisfy:

```ada
subtype Port    is Integer range 1 .. 65535;
subtype LowPort is Integer range 1 .. 1024
  with Dynamic_Predicate => LowPort >= 1 and LowPort <= 1024;
```

A value of `LowPort` (range 1..1024) is a valid `Port` (range 1..65535) because the range is narrower. Ada handles this through range constraints, which are a built-in form of refinement. Range containment is decidable — it is interval arithmetic.

Ada distinguishes two kinds of predicate:

- **Static_Predicate**: the predicate must be a static expression (comparisons, ranges, membership tests). The compiler can evaluate it at compile time and reason about it. Subsumption between two static predicates is decidable when both reduce to range constraints.
- **Dynamic_Predicate**: the predicate may contain arbitrary expressions, including function calls. The compiler cannot reason about it at compile time. It is checked at runtime on every assignment, parameter passing, and type conversion.

SPARK (the Ada subset for formal verification) adds proof support. SPARK sends predicate obligations to GNATprove, which uses an SMT solver (CVC5 or Z3) to discharge them. SPARK can prove that a `Dynamic_Predicate` holds at a given point, and can reason about subsumption between predicates in the SMT fragment.

The key distinction for the owner's example: Ada's `Static_Predicate` is the closest industrial precedent. A subtype with a static predicate can be compared at compile time when both predicates are range constraints. A `Dynamic_Predicate` with a function call cannot — it is runtime only.

SPARK bridges the gap: it can prove `Dynamic_Predicate` implications using the solver, but only for the SMT-decidable fragment. An arbitrary function call in a predicate is treated as an uninterpreted function, and the solver proves what it can from the function's contract.

Compile-time cost: Ada with `Static_Predicate` — near zero (compile-time range checks). SPARK with solver — high (solver calls per verification condition).

When the solver cannot decide: SPARK reports "checks might fail" and inserts runtime checks. Ada without SPARK always inserts runtime checks for `Dynamic_Predicate`. The runtime check is the fallback.

Key references: Ada 2012 Reference Manual, §3.2.4 (https://www.ada-auth.org/standards/2zarm/html/AA-3-2-4.html); SPARK User Guide, "Type Invariant and Predicate" (https://docs.adacore.com/spark2014-docs/html/ug/); Moy et al., "Testing or Formal Verification: DO-178C Alternatives and Their Complementation" (2013, https://www.adacore.com/uploads/technicalPapers/TestingOrFormalVerification-ERTS2012.pdf).

|property|answer|
|---|---|
|predicate class|Static_Predicate: ranges, membership (decidable). Dynamic_Predicate: arbitrary expressions (runtime)|
|subsumption|automatic for range constraints (Ada); solver for SPARK's SMT fragment|
|solver needed|no (Ada); yes (SPARK)|
|compile-time cost|near zero (Ada static); high (SPARK solver)|
|solver cannot decide|runtime check inserted (Ada/SPARK fallback)|

---

## TypeScript template literal types and branded types

TypeScript template literal types can express character-level constraints without a solver:

```typescript
type HexDigit = "0"|"1"|"2"|"3"|"4"|"5"|"6"|"7"|"8"|"9"|"a"|"b"|"c"|"d"|"e"|"f"
type NoAB     = Exclude<HexDigit, "a"|"b">
type HexNoAB  = `${HexDigit}${NoAB}`
```

A value of `HexNoAB` (no `a` or `b` in the second position) is a value of `HexDigit${HexDigit}` (any two hex digits) because the former is a subset of the latter. TypeScript decides this through **structural set operations** on finite unions of string literals. No solver is involved.

The predicate class is **finite unions of string-literal types and template literal patterns**. TypeScript's type system computes these through algorithmic union, intersection, and exclusion operations. The operations are decidable because the domains are finite.

This is exactly the owner's example, limited to finite domains. A type that forbids `a` and `b` is a subtype of one that forbids only `a`, because the set of permitted values is smaller. TypeScript proves this by set algebra, not by a solver.

What TypeScript cannot do: infinite domains, arithmetic predicates, regex patterns, or arbitrary condition functions. There is no `type Port = number where x >= 1 && x <= 65535`. Branded types (nominal subtyping via intersection) get part of the way:

```typescript
type Port = number & { __brand: "Port" }
function makePort(n: number): Port {
  if (n < 1 || n > 65535) throw new Error()
  return n as Port
}
```

The brand gives nominal identity but no compile-time reasoning about the predicate. A `LowPort` brand is not a subtype of a `Port` brand. The compiler treats them as unrelated nominal types. The runtime check in the constructor is the only enforcement.

Subsumption: automatic for finite unions of string literals (set algebra). Not available for branded types or numeric predicates.

Compile-time cost: low. The set operations are polynomial in the size of the unions. TypeScript's type checker does not call a solver.

When the system cannot decide: TypeScript falls back to its `as` cast, which is an unchecked assertion. There is no runtime check generated by the type system — the brand's constructor check is user-written, not compiler-generated.

Key references: TypeScript Handbook, "Template Literal Types" (https://www.typescriptlang.org/docs/handbook/2/template-literal-types.html); Wadler, "Theorems for free!" context on structural subtyping; the TypeScript issue on branded types (https://github.com/microsoft/TypeScript/issues/202).

|property|answer|
|---|---|
|predicate class|finite unions of string literals; template literal patterns|
|subsumption|automatic via set algebra (finite unions); none for branded/numeric|
|solver needed|no|
|compile-time cost|low — polynomial set operations|
|system cannot decide|user `as` cast (unchecked); brand constructor is user-written runtime check|

---

## Rust newtypes plus `TryFrom`

Rust newtypes give nominal identity without any compile-time predicate reasoning:

```rust
struct Port(i32);
struct LowPort(i32);

impl TryFrom<i32> for Port {
    type Error = ();
    fn try_from(n: i32) -> Result<Port, ()> {
        if n >= 1 && n <= 65535 { Ok(Port(n)) } else { Err(()) }
    }
}
```

A `LowPort` is not a `Port` in Rust's type system. They are distinct nominal types. The compiler does not know that `LowPort`'s range is a subset of `Port`'s range. The user must write an explicit conversion, or implement `From<LowPort> for Port` by hand.

The predicate class is **none at the type level**. Predicates live in `TryFrom`/`TryInto` implementations, which are runtime checks. The type system provides no subsumption between newtypes.

Subsumption: not automatic. The user writes `impl From<LowPort> for Port` if they want the conversion to be infallible, or `impl TryFrom<LowPort> for Port` if it might fail. The compiler trusts the implementation — there is no proof that the `From` is sound.

Compile-time cost: zero (no solver, no predicate analysis). The cost is in runtime checks at conversion boundaries.

When the system cannot decide: it does not try. Every conversion is an explicit `From`/`TryFrom` implementation. The type system never reasons about predicates.

Key references: Rust API documentation, `std::convert::TryFrom` (https://doc.rust-lang.org/std/convert/trait.TryFrom.html); the Rustonomicon on newtypes (https://doc.rust-lang.org/nomicon/branded structs.html); RFC 2451, "Re-Rebalancing Coherence" (https://github.com/rust-lang/rfcs/blob/master/text/2451-re-rebalancing-coherence.md).

|property|answer|
|---|---|
|predicate class|none at type level; runtime checks in `TryFrom`|
|subsumption|none; user writes explicit `From`/`TryFrom`|
|solver needed|no|
|compile-time cost|zero|
|system cannot decide|N/A — system does not attempt predicate reasoning|

---

## Clojure spec and Elixir guards

Clojure spec and Elixir guards represent the run-time-only end of the spectrum. Neither language has a static type system that reasons about predicates.

### Clojure spec

```clojure
(s/def ::port (s/and int? #(<= 1 % 65535)))
(s/def ::low-port (s/and int? #(<= 1 % 1024)))
```

A `::low-port` value satisfies `::port` because `<= 1 % 1024` implies `<= 1 % 65535`. Clojure spec does not know this. There is no static analysis. Validation runs at the point where `s/valid?` or `s/conform` is called.

The predicate class is **any Clojure function**. The predicate is a first-class function, so the system can express anything. The cost is that nothing is decidable at compile time — there is no compile time analysis of specs.

Subsumption: not available. The user must validate against the target spec explicitly. There is no mechanism to say `::low-port` is a subtype of `::port`.

Compile-time cost: zero. No analysis.

When the system cannot decide: it does not try. Validation is always runtime.

Key reference: Clojure spec guide (https://clojure.org/guides/spec).

### Elixir guards

```elixir
defmodule Port do
  defguard is_port(n) when is_integer(n) and n >= 1 and n <= 65535
  defguard is_low_port(n) when is_integer(n) and n >= 1 and n <= 1024
end
```

Elixir guards are a restricted set of expressions allowed in function clauses and `when` keywords. The guard set is deliberately limited to comparisons, type checks, and a few built-in functions. This restriction exists so the compiler can reason about guard clauses for pattern matching exhaustiveness — but Elixir does not perform subsumption analysis between guards.

A `is_low_port` value satisfies `is_port`, but the compiler does not know this. The user must call `is_port` explicitly.

The predicate class is **the guard expression set**: comparisons, type checks, `is_*` guards, and a fixed list of built-in functions (https://hexdocs.pm/elixir/pattern-matching.html#guards). This is a decidable subset, but Elixir does not use it for subsumption — only for pattern matching.

Subsumption: not available.

Compile-time cost: zero (no subsumption analysis).

Key reference: Elixir documentation, "Guards" (https://hexdocs.pm/elixir/guards.html).

|property|answer|
|---|---|
|predicate class|Clojure spec: any function. Elixir guards: restricted expression set|
|subsumption|not available in either|
|solver needed|no|
|compile-time cost|zero (no analysis)|
|system cannot decide|N/A — no static analysis attempted|

---

## Refined types in Scala

Scala's type system supports path-dependent types and a limited form of refinement via structural types and phantom types. The `refined` library (by Euclid Labs / Alexander Kühn) adds refinement types as a library:

```scala
import eu.timepit.refined._
import eu.timepit.refined.api.Refined
import eu.timepit.refined.numeric._

type Port    = Int Refined Interval.Closed[1, 65535]
type LowPort = Int Refined Interval.Closed[1, 1024]
```

The `refined` library checks predicates at compile time for literal values using macros, and at runtime for non-literal values. A `LowPort` value is a `Port` because the interval [1,1024] is contained in [1,65535].

The library provides a `RefType` abstraction and an `Validate` typeclass. Subsumption between two refined types is not automatic — the library does not provide a `LowPort <: Port` subtyping relationship. Scala's type system treats `Int Refined Interval.Closed[1,1024]` and `Int Refined Interval.Closed[1,65535]` as unrelated types. The user must write an explicit conversion, which the library can validate at compile time for literal values or at runtime for others.

The predicate class depends on what the user provides through `Validate` instances. The library ships with numeric intervals, string predicates (regex, length), boolean combinations, and collection predicates. Non-literal values always fall back to runtime validation.

Subsumption: not automatic at the type level. Compile-time validation of literal values is available through macros. The macro evaluates the predicate against the literal and produces a compile error if it fails.

Compile-time cost: low for macro validation (one macro expansion per literal). No solver. No subsumption analysis between types.

When the system cannot decide: runtime validation. The library provides `refineV` which returns `Either[String, Refined[Int, ...]]`, so the user handles validation failures at runtime.

Key references: the `refined` library (https://github.com/fthomas/refined); Kühn, "Refined Types for Scala" (https://github.com/fthomas/refined/blob/master/modules/core/shared/src/main/scala/eu/timepit/refined/api/RefType.scala); the Scala reference on path-dependent types (https://docs.scala-lang.org/tour/path-dependent-types.html).

|property|answer|
|---|---|
|predicate class|library-defined `Validate` instances: intervals, regex, length, boolean combos|
|subsumption|not automatic at type level; macro validation for literals|
|solver needed|no|
|compile-time cost|low — macro expansion per literal|
|system cannot decide|runtime validation via `refineV` returning `Either`|

---

## The decidability line

The owner's example — "if one type forbids the characters `a` and `b`, and another forbids only `a`, a value of the first satisfies the second" — is a question about **language containment**. The set of strings not containing `a` or `b` is a subset of the set of strings not containing `a`. This is decidable.

### What is decidable

**Regular language containment is decidable.** If a type's predicate defines a regular language (a `value =~ "pattern"` clause, or a character-set exclusion expressible as a character class or a finite union of string literals), then the question "is language A a subset of language B?" is decidable. The standard algorithm builds DFAs for both languages, computes the product of one with the complement of the other, and checks for a reachable accepting state. If none exists, A is contained in B. This is PSPACE-complete in the general case, but polynomial for typical refinement patterns.

This is the result Vyrn's RFC-0020 already uses. `compiler/vyrn-frontend/src/regex.rs:834-885` implements `contains(sup: &Dfa, sub: &Dfa) -> Result<(), String>` — DFA product with complement, BFS for a shortest witness. The function returns `Ok(())` when `sub` is contained in `sup`, and `Err(witness)` with a counterexample otherwise.

**Interval (range) containment is decidable.** If a type's predicate is a range constraint (`value >= lo && value <= hi`), then the question "is range A a subset of range B?" is decidable by interval arithmetic: `[a_lo, a_hi] ⊆ [b_lo, b_hi]` iff `a_lo >= b_lo && a_hi <= b_hi`. This is constant time.

**Boolean combinations of decidable predicates are decidable** when the combination preserves the structure. A conjunction of two regular languages is regular (intersection, also a DFA product). A conjunction of two range constraints is a range. A conjunction of a regular language and a range constraint is a more complex question — the two domains (strings and numbers) do not interact, so they stay decidable in their respective domains.

### What is not decidable

**An arbitrary condition function is not decidable.** A predicate `fn isEven(n: Int) -> Bool` defines the set of even integers. The question "is the set defined by `isEven` a subset of the set defined by `isMultipleOfFour`?" requires reasoning about the function bodies. For arbitrary Turing-complete functions, this is equivalent to the halting problem (Rice's theorem). No algorithm can decide it for all pairs of functions.

This is the line the owner asked for, stated plainly:

|predicate class|decidable?|how|
|---|---|---|
|regular language (regex, char set)|yes|DFA product + complement (PSPACE-complete, polynomial for typical patterns)|
|integer interval (range constraint)|yes|interval arithmetic (constant time)|
|integer linear arithmetic (LIA)|yes|Presburger arithmetic is decidable (Cooper's algorithm; SMT solvers handle it)|
|finite union of string literals|yes|set algebra (polynomial)|
|conjunction of same-class predicates|yes|intersection in the same domain|
|arbitrary function `fn(T) -> Bool`|no|Rice's theorem: equivalent to the halting problem|
|non-linear arithmetic|no in general|Hilbert's tenth problem: undecidable for integers|
|string length + regex combined|depends|each domain decidable separately; cross-domain conjunction stays decidable if independent|

### Where each language put the line

|language|predicate class for subsumption|decidable?|who decides|
|---|---|---|---|
|Liquid Haskell|SMT theories (LIA, uninterpreted functions, ADTs)|yes (SMT fragment)|Z3 solver, automatic|
|F\*|SMT theories + dependent types|yes (SMT fragment)|Z3 solver; user proofs for the rest|
|Dafny|SMT theories (arithmetic, sequences, sets)|yes (SMT fragment)|Z3 solver; user lemmas for the rest|
|ATS|linear integer arithmetic|yes|purpose-built solver; user linear proofs for the rest|
|Ada (Static_Predicate)|ranges, membership|yes|compiler range analysis|
|Ada (Dynamic_Predicate)|arbitrary expressions|no|runtime check always|
|SPARK|SMT theories|yes (SMT fragment)|Z3/CVC5 solver; runtime check fallback|
|TypeScript|finite unions of string literals|yes|set algebra, no solver|
|TypeScript branded|none|N/A|no subsumption|
|Rust newtypes|none|N/A|no subsumption|
|Clojure spec|any function|no|no static analysis|
|Elixir guards|restricted expressions|yes (but unused for subsumption)|no subsumption analysis|
|Scala refined|library `Validate` instances|yes for literals (macro)|no type-level subsumption|

Every language that ships automatic subsumption restricts the predicate class to a decidable fragment. Liquid Haskell, F\*, Dafny, and SPARK use SMT solvers and accept the compile-time cost. Ada's `Static_Predicate` uses range analysis. TypeScript uses set algebra on finite unions. ATS uses a lightweight arithmetic solver and pushes the rest to user-constructed linear proofs.

Every language that accepts arbitrary function predicates (Clojure spec, Ada `Dynamic_Predicate` without SPARK, Rust `TryFrom`) does not attempt subsumption at all. The check is runtime only.

No language in the survey attempts automatic subsumption over arbitrary condition functions. The undecidability result is universal, and every production system respects it by restricting the predicate class or falling back to runtime.

---

## What Vyrn has today

Vyrn has validated types (RFC-0003). A validated type is a scalar base plus a `where` predicate over `value`:

```vyrn
type Age  = Int64 where value >= 18
type Port = Int64 where value >= 1 && value <= 65535
```

The predicate must be call-free and const-analyzable. The checker rejects predicates that contain calls: `compiler/vyrn-frontend/src/checker.rs:2865-2872` (`if consteval::contains_call(pred)`). This restriction confines predicates to the compile-time-evaluable fragment: comparisons, boolean operators, arithmetic, `value.byteLength`, and `value =~ "literal"`.

### What a validated type can express today

A validated type's predicate can express:

1. **Numeric range constraints** (`value >= N`, `value <= N`, `value > N`, `value < N`, and `&&` conjunctions). `compiler/vyrn-frontend/src/types.rs:396-430` (`predicate_bounds`) extracts `(min, max)` from these.

2. **Numeric divisibility** (`value % K == 0`). `compiler/vyrn-frontend/src/types.rs:443-469` (`predicate_multiple_of`) extracts the divisor.

3. **String length constraints** (`value.byteLength >= N`, `value.byteLength <= N`). `compiler/vyrn-frontend/src/types.rs:471-499` (`predicate_length_bounds`) extracts `(minLength, maxLength)`.

4. **String regex patterns** (`value =~ "literal"`). `compiler/vyrn-frontend/src/types.rs:501-518` (`predicate_pattern`) extracts the pattern. The pattern compiles to a DFA: `compiler/vyrn-frontend/src/regex.rs:1149` (`compile`). Multiple patterns in a conjunction intersect: `compiler/vyrn-frontend/src/regex.rs:830-832` (`intersect`).

5. **Cross-field predicates** on records (`type T = { lo: Int, hi: Int where hi >= lo }`). `compiler/vyrn-frontend/src/types.rs:1610-1630` (`predicate_binds`) binds record fields by name in the predicate scope.

6. **Boolean conjunctions** (`&&`) of the above. Disjunctions are accepted syntactically but are not fully captured by the reflection model — `compiler/vyrn-frontend/src/types.rs:1062-1126` (`collect_constraints`) returns `false` for disjunctions, and the JSON Schema emitter documents the true predicate in a `$comment` instead.

### What the compiler does with predicates today

**Compile-time proof for constants**: when a compile-time-constant value flows into a validated type, the checker evaluates the predicate via `consteval::eval` and rejects provably-false values. `compiler/vyrn-frontend/src/checker.rs:2243-2288` (`prove_coercion`). This is RFC-0003's "if the compiler can prove it, there is no runtime cost."

**Runtime check at value boundaries**: when a non-constant value flows into a validated type, the checker's `coercible` function (`compiler/vyrn-frontend/src/checker.rs:2223-2234`) allows the flow when the source is structurally assignable to the base. The codegen backends emit a runtime validation call. `compiler/vyrn-frontend/src/finite.rs:42-51` documents that the interpreter keeps its runtime validation in place (a guaranteed no-op on proven values), while codegen skips it on a proof.

**DFA-based containment for string interpolations** (RFC-0020): when a string interpolation or a finite-string variable flows into a validated string type whose predicate is a pure `value =~ "..."` conjunction, the checker proves containment by DFA product. `compiler/vyrn-frontend/src/finite.rs:254-299` (`prove_string_flow`). The DFA containment check is `compiler/vyrn-frontend/src/regex.rs:834-885` (`contains`). This is the one place Vyrn already does subsumption: it proves that the language of the source is a subset of the language of the target, and skips the runtime check if so.

**Reflection**: `schemaOf(TypeName)` returns a `Schema` record with the type's name, base, bounds, pattern, and length. `compiler/vyrn-frontend/src/types.rs:563-614` (`schema_struct_lit`). The `Schema` type and `Issue` type are declared in `std/` and used by `std/contract:checkContract` for module-level contract checking. `compiler/vyrn-frontend/src/schema_reflect.rs:28-35` describes the `ContractInfo` / `MemberInfo` reflection used by `std/contract`.

### Whether two validated types can be compared today

**No.** The checker's `assignable` function is strict for predicated named types:

`compiler/vyrn-frontend/src/checker.rs:2126-2134`:
```
// `assignable` is the STRICT relation: a predicated named type admits
// only itself here. Value boundaries use `coercible`, which adds the
// automatic-validation rule on top.
if let Type::Named(n) = to {
    if let Some(d) = self.types.get(n) {
        if d.predicate.is_some() {
            return matches!(from, Type::Named(m) if m == n);
        }
    }
}
```

A value of type `LowPort` (range 1..1024) is **not** assignable to `Port` (range 1..65535). The checker requires the names to match. The `coercible` function allows a raw `Int64` to flow into `Port` by running the predicate at runtime, but it does not compare two predicated types.

The DFA containment in `finite.rs` is the exception. It compares two string types when both have pure-regex predicates. It does not handle numeric ranges, mixed predicates, or non-regex string predicates.

The infrastructure to extract bounds from predicates already exists: `predicate_bounds` (`types.rs:402`), `predicate_length_bounds` (`types.rs:474`), `predicate_multiple_of` (`types.rs:444`), and `predicate_pattern` (`types.rs:503`). These are used for reflection (schema emission, JSON Schema generation) but not for subsumption. A subsumption check could build on them, but no such check exists today.

### Summary of what would have to change

To answer the owner's question, the compiler would need a subsumption relation between two validated types: given `from: Named(A)` and `to: Named(B)`, where both have predicates over the same base, prove that `pred_A => pred_B`. The existing DFA containment (`regex.rs:contains`) handles one case (pure-regex string types). The existing `predicate_bounds` handles another case in principle (range containment) but is not wired to `assignable`. The general case — arbitrary boolean combinations of ranges, lengths, patterns, and divisibility — needs a decision procedure, and the predicate class determines which one is sound.

---

## The options

RECOMMENDATION, NOT A DECISION.

### Option 1 — Range containment for numeric predicates

Add a subsumption check for numeric validated types whose predicates reduce to interval constraints. When `assignable(Named(A), Named(B))` is called and both A and B have numeric predicates, extract `(min_A, max_A)` and `(min_B, max_B)` via `predicate_bounds`, and return true when `min_A >= min_B && max_A <= max_B` (with `None` treated as unbounded). This is interval arithmetic — constant time, no solver, no DFA.

Handle `multipleOf` similarly: if A has `multipleOf(k_A)` and B has `multipleOf(k_B)`, then A is a subtype of B only when `k_B` divides `k_A` (or B has no `multipleOf` constraint). If A has `multipleOf(4)` and B has `multipleOf(2)`, every multiple of 4 is a multiple of 2.

Predicates outside the interval/multipleOf form (e.g. `value % 2 == 0 && value > 5`) would fall back to the current strict rule: same name only. The compiler does not attempt to prove them.

|design|one-sentence description|parser cost|checker cost|lowering cost|what breaks|who else does it|
|---|---|---|---|---|---|---|
|Option 1: range containment|Two numeric validated types are compared by interval arithmetic on their extracted bounds; same base required|none|new `numeric_subsumes` function called from `assignable_d` when both sides are `Named` with numeric predicates; constant time|none — subsumption only affects whether a runtime check is emitted, and codegen already skips checks on proven values|nothing — this only loosens `assignable`, which currently rejects cross-type flows; existing programs that pass today still pass|Ada `Static_Predicate` (range constraints); Scala `refined` (intervals, though not automatic at type level)|

### Option 2 — Extend DFA containment to all string predicates

Generalize the existing RFC-0020 DFA containment from interpolation flows to direct type-to-type subsumption. When `assignable(Named(A), Named(B))` is called and both A and B are string types with pure-regex predicates, use `regex::contains(dfa_B, dfa_A)` to decide. This is what `finite.rs:prove_string_flow` already does for interpolation flows — the change is to call it from `assignable_d` for a named-to-named flow, not just from `prove_string_interpolation`.

The predicate class stays the same: pure `value =~ "literal"` conjunctions. A string type with a length clause or a non-regex clause falls back to the strict rule.

|design|one-sentence description|parser cost|checker cost|lowering cost|what breaks|who else does it|
|---|---|---|---|---|---|---|
|Option 2: DFA subsumption for strings|Two regex-validated string types are compared by DFA product containment, reusing the existing `regex::contains`|none|call `regex::contains` from `assignable_d` for named-to-named string types; polynomial in DFA size (already bounded by `DFA_STATE_BUDGET = 8192`)|none — codegen already consults `string_flow_proven`|nothing — loosens `assignable`; the existing containment is already proven sound|TypeScript (finite unions via set algebra, not DFA, but same decidable class)|

### Option 3 — Combined syntactic subsumption (ranges + regex + length)

Combine Options 1 and 2, and add length-bounds containment. When `assignable(Named(A), Named(B))` is called and both have the same base:

- Numeric base: compare intervals via `predicate_bounds`, compare `multipleOf` via divisibility.
- String base: if both are pure-regex, use `regex::contains`. If both have length bounds, compare `(minLen_A, maxLen_A)` against `(minLen_B, maxLen_B)`. If both, check each independently.
- Mixed predicates (e.g. regex + length on the same type): check each clause independently. The conjunction is contained if every clause of A implies the corresponding clause of B.

This covers the three predicate classes Vyrn can express today (ranges, lengths, regex) without a solver. The fallback for predicates outside these classes remains the strict rule.

The soundness argument: each clause is checked independently in its own decidable domain. A conjunction `P_A && Q_A` implies `P_B && Q_B` when `P_A => P_B` and `Q_A => Q_B`. This holds when the clauses are independent (no cross-clause interaction). For Vyrn's current predicates, this is always true: a range clause and a length clause constrain different properties of `value`, and a regex clause and a length clause constrain different properties too. The one case where independence fails is a regex clause that implies a length bound (e.g. `"abc"` implies length 3), but that is a refinement, not a weakening — if the regex is contained, the length is already satisfied.

|design|one-sentence description|parser cost|checker cost|lowering cost|what breaks|who else does it|
|---|---|---|---|---|---|---|
|Option 3: combined syntactic subsumption|Compare ranges, lengths, and regex patterns independently across same-base validated types, using interval arithmetic, length-bounds comparison, and DFA containment respectively|none|new `subsumes(decl_from, decl_to)` called from `assignable_d`; constant time for ranges/lengths, polynomial for regex; fallback to strict for unrecognized forms|none|nothing — loosens `assignable` only|Ada Static_Predicate (ranges); TypeScript (regex/finite unions); no single language combines all three without a solver|

### Option 4 — SMT solver integration

Integrate an SMT solver (Z3 or a lighter alternative) to discharge general subsumption obligations. When `assignable(Named(A), Named(B))` is called, encode both predicates as SMT formulas and ask the solver to prove `pred_A => pred_B`. This covers arbitrary boolean combinations of arithmetic, string operations, and (with theory extensions) regex containment.

The predicate class expands to the SMT-decidable fragment: linear arithmetic, uninterpreted functions, string theory, and regex theory (Z3's `seq.regex` since 2019). Non-linear arithmetic and arbitrary function calls in predicates remain rejected (Vyrn already rejects calls in predicates, so this is not a new restriction).

|design|one-sentence description|parser cost|checker cost|lowering cost|what breaks|who else does it|
|---|---|---|---|---|---|---|
|Option 4: SMT solver|Encode both predicates as SMT formulas and ask Z3 to prove the implication|none — predicates are already call-free expressions|high — Z3 call per subsumption query; caching essential; cold builds of large programs measured in minutes in Liquid Haskell/F\*/Dafny|none — solver result affects only check emission|the build gains a hard dependency on an external solver process or library; `vyrn run` on a bare file must either bundle the solver or fall back to runtime; the `interp == native == wasm` invariant must account for solver nondeterminism (Z3 is deterministic for the same input, but version and configuration matter)|Liquid Haskell, F\*, Dafny, SPARK|

### Option 5 — User-declared subsumption with compiler verification

Let the user declare subsumption relationships explicitly, and verify them where the compiler can:

```vyrn
subsumes LowPort Port
```

The declaration says "every `LowPort` is a `Port`." The compiler checks it using whatever decision procedure is available: range containment for numeric types, DFA containment for string types, and constant evaluation for literal witnesses. If the compiler cannot prove it, the declaration is rejected — no unchecked subsumption.

This is the ATS model: the user states the relationship, the system verifies it with its available tools, and what it cannot verify it refuses. The difference from Option 3 is that the user makes the relationship explicit, so the compiler is not trying to discover subsumption between every pair of types — it only checks what the user declares.

|design|one-sentence description|parser cost|checker cost|lowering cost|what breaks|who else does it|
|---|---|---|---|---|---|---|
|Option 5: user-declared subsumption|The user writes `subsumes A B`; the compiler verifies it using range/regex/constant analysis and refuses what it cannot prove|new `subsumes` declaration in the parser; new AST node|new verification pass: range containment, DFA containment, or constant-witness search, depending on the predicate class|none — verified subsumption feeds `assignable` like any other subtyping rule|a new declaration kind in the language; users must declare relationships they want the compiler to use|ATS (user-constructed proofs); Rust `From`/`TryFrom` (user-declared, but unchecked)|
