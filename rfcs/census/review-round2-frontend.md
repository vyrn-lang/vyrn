# Round-two review, the frontend crate: 44 findings triaged

A code review on 2026-08-29 produced 95 findings over the compiler. This table
is the verdict on the 44 that name a file in `compiler/vyrn-frontend`. Each row
says whether the defect is closed, whether it was never a defect, or whether it
still runs on `main`, and gives the evidence.

Most rows are closed. Commit `907354be` (2026-08-22, "round two of the audit")
answered the same census and fixed 94 of the 95 findings it saw. The code it
left carries a comment at each site that names the defect it closed. This table
re-checks every row against `main` rather than trusting that message: a repro
program for each behaviour a reader can reach from the command line, and the
code for the rest.

One finding was live. `F2-039` is fixed in this branch.

## Method

- Branch `triage-frontend` off `origin/main` at `1b6523c3`. Driver:
  `compiler/target/release/vyrn.exe`, built from that tree.
- A verdict of `fixed` names the code that closes the finding, or the repro
  that shows the behaviour is gone. Every non-termination row (`F2-018`,
  `F2-019`, `F2-033`, `F2-036`, `F2-046`) ran its repro under a timeout first.
  All five end in a diagnostic, none in a hang and none in a stack overflow.
- A verdict of `not a defect` names the rule the finding misread.
- A verdict of `live` names the program that shows it.

## The table

| id | severity | verdict | evidence | commit |
| --- | --- | --- | --- | --- |
| F2-011 | medium | fixed | `unpack_tool` takes a per-hash gate file before staging, so two cold unpacks of one tool serialize. `place_staged_tool` runs under it | `907354be` |
| F2-017 | high | fixed | `declared_owned_in` keys `seen` on the full shape and enters only `Named`/`App`. Repro: `{ a: Option<Int64>, b: Option<Ring> }.copy()` is refused, naming `Ring` | `907354be` |
| F2-018 | high | fixed | `MAX_ASSIGNABLE_DEPTH` caps `assignable_d`. `NodeA` against `NodeB` answers a type error in milliseconds | `907354be` |
| F2-019 | medium | fixed | `contains_heap` goes through `reaches`, which threads the declaration heads already visited. A recursive record stored inside a `region` diagnoses | `907354be` |
| F2-020 | medium | fixed | `let a: Int32 = -2147483648`, `Int8 = -128` and `Int16 = -32768` all compile and run | `907354be` |
| F2-021 | medium | fixed | `x == -5` on an `Int8` compiles, and `x - 5` answers `-2` | `907354be` |
| F2-022 | medium | not a defect | the rule it enforces is gone. RFC-0126 section 8.7 removed "no nested Option/Result" because all three engines already ran the type. `examples/nestedsum.vyrn` is the corpus case | - |
| F2-023 | high | fixed | the zero-match arm picks the candidate whose protocol the parameter's bound names. `fn use<T: Q>(x: T)` dispatches to `Q` and answers 2 | `907354be` |
| F2-024 | low | fixed | `MapLit` reuses `first_val` for entry 0, and the dispatched receiver is solved rather than typed again in `check_declared_call` | `907354be` |
| F2-025 | low | fixed | pass 1 of `check_declared_call` runs `prove_coercion` and `prove_string_interpolation` on the substituted parameter | `907354be` |
| F2-026 | medium | fixed | `GlobalRef::expr` hits the callee name of a `Call`, so a generator that only ever called a fn-typed global no longer passes the purity walk | `907354be` |
| F2-027 | medium | fixed | `shape_matches` satisfies a `let` member with an arity-0 export of the member's type, the rule `std/contract` applies | `907354be` |
| F2-028 | medium | fixed | `shape_of` emits the accessor snippet `export fn <name>() -> <ty>`, never `export let` | `907354be` |
| F2-029 | medium | fixed | the schema import refuses an optional property that carries its own constraints, so `Option<User.nick>` is never built | `907354be` |
| F2-030 | high | fixed | `convert` refuses a `multipleOf` that is not positive, with a test over the zero and negative divisors | `907354be` |
| F2-031 | medium | fixed | `P::num` is the RFC 8259 grammar and `P::ws` is the four JSON spaces. `numbers_and_whitespace_are_strict_json` pins `01`, `-007`, `1.`, `.5` and NBSP | `907354be` |
| F2-032 | high | fixed | `Parser::obj` carries a `HashSet` side-set, so the duplicate-key check is linear | `907354be` |
| F2-033 | high | fixed | `DFA_STATE_BUDGET` bounds the subset construction. The 21-state blowup pattern is refused in under a second, not a 1 GB table | `907354be` |
| F2-034 | medium | fixed | `escape_class` maps `n`, `r`, `t`, `v` and `f` to their control characters, as ECMA-262 does | `907354be` |
| F2-035 | medium | fixed | `splice_float` emits plain decimal digits and appends `.0`, so no spliced float can carry an exponent | `a1817c90` |
| F2-036 | medium | fixed | `PARSE_DEPTH_BUDGET` caps `ReParser`. 5,000 nested groups answer "pattern nests more than 250 levels deep" | `907354be` |
| F2-037 | medium | fixed | the `privates` filter carries `!d.is_extern`, so an `extern fn` keeps its host-ABI spelling | `907354be` |
| F2-038 | medium | fixed | the hidden-original check exempts a name whose only evidence is an argument-bearing call and which is a protocol-method surface name | `907354be` |
| F2-039 | medium | live, fixed here | two modules declaring a private `fetch` under a protocol that declares `fetch` made the module's own `w.fetch()` read `fetch__from0(w)`, and the refusal named a symbol nobody wrote | `f8e2e356` |
| F2-040 | high | fixed | pass 2 extends an importer's rewrite map only when the importer imports the enum itself. A consumer's own `JStr` is left alone | `907354be` |
| F2-041 | high | fixed | `im.places` is walked by the rename, the namespace resolver, the reference collector and the visibility check | `907354be` |
| F2-042 | medium | fixed | the reference walk is `RefNames` over the shared body visitor, and every arm pushes the expression's own line | `907354be` |
| F2-043 | medium | fixed | the same gate file as `F2-011`. The two findings are one defect | `907354be` |
| F2-044 | medium | fixed | `inline` substitutes in place only when `uses_outside_lambdas` is also 1, so a parameter read under a lambda binds a temporary | `907354be` |
| F2-045 | high | fixed | the desugar diagnoses a collected predicate whose base is not exactly a record, and names the merge and the variant | `907354be` |
| F2-046 | high | fixed | `postfix` adds its link count to `self.depth` before it builds the chain. 100,000 chained fields answer "nesting exceeds 1024 levels" | `907354be` |
| F2-047 | medium | fixed | `protocol_decl` and `impl_block` restore `type_aliases` around an inner function, so no `?` exit leaks one | `907354be` |
| F2-048 | high | fixed | there is one scanner. `lex` and `lex_with_trivia` both read `scan`, whose nested-string arm counts newlines | `907354be` |
| F2-049 | high | fixed | `consume er.node` then `drop er` is refused, naming the line the field was taken on | `907354be` |
| F2-050 | high | fixed | `sink(if c { d.title } else { "" })` is refused, with the two-way menu | `907354be` |
| F2-051 | high | fixed | a lambda block that shadows a moved binding no longer revives it. The later use is refused | `907354be` |
| F2-052 | medium | fixed | `for x in xs { consume s; continue }` is refused as a consumption inside a loop | `907354be` |
| F2-053 | medium | fixed | `for i in xs { consume er.node }` is refused. The reuse check compares the take's root against the scope | `907354be` |
| F2-054 | high | fixed | `predicate_length_bounds` and `collect_string_constraints` both step with `saturating_add` and `saturating_sub` | `907354be` |
| F2-055 | medium | fixed | `call_keeps` is gone. RFC-0125 moved the capture judgment into the kernel, and the cell the finding names exists nowhere in the crate | `907354be` |
| F2-092 | medium | fixed | an arm binder anchors at its own pattern spelling, pinned by `an_arm_binder_anchors_at_its_own_pattern_not_an_earlier_arm_use` | `907354be` |
| F2-093 | medium | fixed | `same_path` strips the project directory from whichever side carries it, so a deeper file that merely shares the tail stays a different file | `907354be` |
| F2-094 | medium | fixed | `display_path` replaces every inner `GEN_SEP` and strips only the last, so no U+001F reaches a diagnostic | `907354be` |
| F2-095 | medium | fixed | `is_member_position` tests whether the preceding token is a dot on the same line, so `u . age` reads as a member | `907354be` |

## What F2-039 was

Two modules each declare a private `fetch`. Name privacy (RFC-0046 section 3)
minted `fetch__from0` for one of them and rewrote that module's references. A
method call `w.fetch()` parses as `Call { fetch, args: [w] }`, which is the
shape a free call `fetch(w)` has, so the rewrite renamed the method call too.
The checker then had no protocol method to dispatch to, and refused the module
with "`fetch__from0` argument 1 expects Int64, found W".

The checker already refuses a free declaration whose name is a protocol method:
"method names dispatch to impls before free functions, so this declaration
could never run". The rename hid that refusal from the second module to declare
the name. The privacy pass now reads the same `method_surface` set the
hidden-original check beside it reads, and skips such a name. No program loses
an acceptance, because the declaration the rename was rescuing could never run.
The regression is pinned by
`a_protocol_method_name_is_never_renamed_apart_for_privacy` in
`compiler/vyrn-frontend/tests/loader_run.rs`.
