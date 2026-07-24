# RFC-0071 — Module Contracts: Conventions You Can See

- **Status:** Draft
- **Depends on:** RFC-0021 (`gen fn`, comptime interpreter, `moduleInterface`),
  RFC-0031 (`moduleInterface` reachable type closure, `TypeInfo.module`),
  RFC-0026 / RFC-0039 (`std/ui` pages, `std/vyx` components — the two
  generators whose conventions this RFC retrofits)
- **Evidence (user):** "does variables/function like `head` would be suggested
  by LSP? with what mechanism they are reinforced" — and, earlier, "I don't
  have anything hardcoded and want everything to be generic/universal", "I'm
  okay with naming conventions that can make it cleaner, but I want still user
  be able to control it, full autocomplete and language integration."

---

## The problem, measured

Two conventions in the shipped fullstack layer are enforced by mechanisms the
editor cannot see.

**`head` is scanner-parsed.** `std/vyx.vyrn:2739` parses a `head { … }` block
out of a `.vyx` `<script>` by scanning source text. It is not a declaration, so
it has no symbol, no type, no doc, and no definition site. The LSP cannot
complete it, hover it, or jump to it. A misspelling (`heaad { … }`) surfaces as
a parse error that never mentions `head`.

**`load` is name-matched.** `std/ui.vyrn:595` records `UiPageInfo.hasLoad` by
looking for an export literally named `load`. A page that declares `fn laod()`
compiles cleanly and **silently renders with no data**. Silent degradation is
the single most damaging failure mode identified in the DX survey that
motivated this arc; we ship an instance of it.

Both names are also **hardcoded in std**, so a third-party pages generator
cannot participate: it would have to invent its own scan, with its own bugs.
The repo's own crutch audit (2026-07-18) found exactly this correlation —
scanner-based generators carried six miscompiles; reflection-based generators
carried none.

## The model

A **module contract** is a declaration that states which exports a module may
have, with their types, optionality, and documentation. It is ordinary library
code. Nothing in it is known to the compiler beyond the declaration form itself.

```vyrn
/// std/ui — what a page module may export.
export contract Page {
    /// Document head contributions for this page.
    fn head() -> Head = noHead()

    /// This page's data, resolved before render. `.lazy()` opts into
    /// render-then-fill; without it the page blocks until the data lands.
    fn data() -> Query<T> = noQuery()
}
```

Members are **functions**, not values. RFC-0029 makes every top-level `let`
module-private in every module — `export let` is a named parse error
(`parser.rs:766`), and cross-module access goes through accessors by design.
That rule is right and this RFC does not reopen it:

- A page satisfies the contract with `export fn head() -> Head { … }`, which is
  the accessor pattern RFC-0029 prescribes, at a cost of one `return`.
- `data` wants to be a function regardless. A module-level binding is evaluated
  at module-init, and RFC-0069 already hit that hazard — the linker initializes
  every reachable module global, which would trap on store file I/O at wasm
  boot. Page data is per-request and must not run at load time.

This does not reintroduce the "magic functions masked as ordinary functions"
objection that motivated the whole arc. That objection was about *undeclared*
names discovered by scanning. These are declared members of a readable contract:
completed, hovered, type-checked, and loud on a typo. Declaring the convention
is precisely the difference.

The `let` member form stays in the grammar and is inert until `export let`
exists — a module can only ever be reported as missing it. It is kept so the
grammar does not need reopening if that rule ever changes.

One declaration serves three consumers:

1. **The generator** checks a module against it, through the `moduleInterface`
   reflection it already performs — replacing name-hunting and source-scanning.
2. **The LSP** resolves it to offer completion, hover, go-to-def, and
   did-you-mean diagnostics, because every member is a real symbol.
3. **You** replace it. A different pages generator declares a different
   contract and gets identical editor support with no LSP change.

`contract` is to a module what `protocol` (RFC-0002 §5) is to a type. The
parallel is deliberate and so is the vocabulary: the thing an application used
to hand-write as `contract.vyrn` becomes a declaration std provides and the
compiler checks (see RFC-0072, which deletes that file).

## Declaration form

```
contract-decl := "export"? "contract" Ident "{" member* "}"
member        := doc? "let" Ident ":" Type ("=" Expr)?                   // value member (inert)
               | doc? "fn"  Ident "(" params ")" "->" Type ("=" Expr)?   // function member
               | doc? "fn"  "*"   "(" params ")" "->" Type               // open rule
```

- A member with a default (`= Expr`) is **optional**; the default is used when
  the module does not export it.
- A member without a default is **required**; its absence is an error naming
  the contract and the member.
- Type parameters appearing in member types (`Query<T>`) are open: the module
  may export any instantiation. `T` is bound per member, not per contract.
- A member name may be declared **more than once**. The repeats are alternative
  signatures: the module satisfies the member by matching any one of them, and
  the generator learns which one it got from reflection.

### Multi-shape members

M2 established that one signature per member is not enough, in two places at
once.

**`head` needs the page's data.** `head { title: pasteTitle(data) }` is a real
capability of the block form being replaced: the title of `/p/{id}` *is* the
paste's title. A zero-argument `fn head() -> Head` cannot express it, and no
single signature can, because page shapes genuinely vary — data, params, both,
or neither. Removing the scanner while removing that capability would be a
downgrade, not a migration.

**`data` needs laziness in the type.** `Query<T>` carries a runtime `lazy` flag,
but laziness decides the *view's type* (`PageData<T>` versus `T`), so the
generator has to read the `lazy(…)` call out of the body — a scan, which is the
practice this RFC exists to end.

Alternative signatures answer both:

```vyrn
export contract Page {
    /// Document head contributions. Takes whatever the page's view takes.
    fn head() -> Head = noHead()
    fn head(d: T) -> Head
    fn head(p: P) -> Head
    fn head(p: P, d: T) -> Head

    /// This page's data. The return TYPE decides the render strategy:
    /// `Query` blocks until the data lands, `Lazy` renders then fills in.
    fn data() -> Query<P, T> = noQuery()
    fn data() -> Lazy<P, T>
}
```

Two consequences worth stating plainly.

`Query<P, T>` gains the params type as an open parameter. `Params` is declared
**per page**, in the page's own module, which is exactly why `std/ui` could not
name it — so it must be open, resolved at the use site like `T`. This is what
lets `query(|p: Params| …)` typecheck.

`Lazy<P, T>` being a distinct type rather than a flag moves the last scan onto
reflection. The generator asks what `data` returns instead of reading how its
body was written, so `vyxScriptDataIsLazy` — the scan that replaced
`vyxUnlazy` — is deleted rather than relocated.

The house rule for `head`: **it takes what the view takes.** Four alternatives
is the complete set, and enumerating them keeps the member closed.

### Closed and open contracts

A contract is **closed** by default: an export not named by the contract is a
diagnostic. This is what makes typo detection total — if every legal name is
enumerated, a near-miss is decidable.

A contract with an **open rule** (`fn *`) admits arbitrarily-named exports that
match the rule's shape:

```vyrn
/// std/rpc — every export is a procedure; inputs and outputs must be
/// serializable. Names here are arbitrary, so there is nothing to misspell and
/// nothing to enumerate.
export contract Api {
    fn *(_: Serializable) -> Serializable
}
```

The design rule: **closed where names carry meaning, open where they do not.**
`Page` is closed — `head` and `data` mean something, so a fourth name is a
mistake. `Api` is open — procedure names are the application's vocabulary, so
enumerating them would be absurd, and the total rule ("every export is a
procedure") replaces enumeration.

An open rule weakens typo protection *for the open slot only*: any name is
legal there, so `laod` would be accepted as a procedure named `laod`. That is
correct for `Api` and would be wrong for `Page`, which is why `Page` is closed.

## Attachment: which contract applies to which module

Contracts attach by directory role, declared in `vyrn.json`:

```json
{
  "roles": {
    "api":     "std/rpc:Api",
    "routes":  "std/ui:Page",
    "widgets": "std/vyx:Component"
  }
}
```

A module under a `routes` path segment is checked against `std/ui:Page`. The
mapping is data — four strings any other tool can read and reproduce. There is
no compiler-side table of blessed directory names.

Role attachment composes with the audience rule in RFC-0072: audience says
*who runs it*, role says *what it is*, and they are separate path segments.

## Checking

Checking runs in the generator, over `moduleInterface`, at comptime — the same
reflection `std/ui` already uses for `UiPageInfo`. `std/contract` provides it
once so every generator gets identical behaviour:

```vyrn
import { checkContract } from "std/contract"

gen fn pages(dir: String) -> Module {
    let iface = moduleInterface(pageFile)
    let issues = checkContract(iface, contractOf(Page))   // Array<Issue>, RFC-0009 shape
    …
}
```

`checkContract` emits an `Issue` for each of:

| condition | diagnostic |
|---|---|
| required member absent | `page must export \`data\` (contract \`Page\`, std/ui)` |
| member type mismatch | `\`head\` must be \`Head\`, found \`String\`` |
| unknown export, close to a member | `unknown page export \`laod\` — did you mean \`data\`?` |
| unknown export, not close | `unknown page export \`helper\` (contract \`Page\` is closed)` |
| open rule shape mismatch | `procedure \`sync\` takes \`File\`, which is not serializable` |

"Close to" is Damerau–Levenshtein distance ≤ 2 against the member names, the
same threshold the existing hint diagnostics use.

The fourth row is the one that matters most for the stated goal: an unrecognized
export is **reported**, never ignored. Silence is what makes a convention feel
like magic; a closed contract has no silent path.

### Private helpers

A page needs local helpers. They are simply not exported — module-private
declarations are outside the contract entirely and are never checked against it.
The closed rule applies to the module's *public* surface, which is exactly the
surface the generator consumes.

## LSP integration

Because members are declarations, four capabilities fall out of machinery that
already exists (RFC-0050 references, RFC-0051 hover quality, RFC-0064 CodeLens).

**Completion.** At module scope in a file whose role resolves to a contract, the
LSP offers each member as a snippet, ordered required-first:

```
head    Head        Document head contributions for this page.
data    Query<T>    This page's data, resolved before render.
```

Snippet body is the full declaration (`export fn head() -> Head { return $0 }`), so the
type is right before the user types anything.

**Hover.** Hovering `head` in a page shows its type, its `///` doc, and
`member of contract Page (std/ui)`.

**Go-to-def.** Jumps to the member in the contract declaration.

**Diagnostics.** `checkContract`'s issues surface as ordinary diagnostics at the
offending export's span, with the did-you-mean quick-fix wired to a rename edit.

The LSP needs one new resolution step: file path → role → contract, reading the
same `vyrn.json` key the generator reads. No contract knowledge is compiled into
the server.

## Migration

Both current conventions move onto declarations. This is a breaking surface
change to `.vyx` pages, executed with a deprecation window.

**`head { … }` → `export fn head()`.** The block form (`std/vyx.vyrn:2739`)
becomes a value:

```vyrn
// before — scanner-parsed block, invisible to the editor
head {
    module "/app.js"
}

// after — an ordinary declaration with a type
export fn head() -> Head { return Head { modules: ["/app.js"] } }
```

`Head` is a record in `std/ui`: `{ title: Option<String>, modules: Array<String>,
stylesheets: Array<String>, meta: Array<Meta> }`. The head-emission path in
`std/ui` (`document(title, head, body)`) is unchanged; only its input changes
from a scanned structure to a reflected value. The scanner
(`vyxParseHead` and friends) is deleted.

**`fn load()` → `export fn data()`.** The name-matched loader becomes a `Query`
value, which also absorbs RFC-0070's `lazy` marker:

```vyrn
// before — magic name; `lazy` is a scanned-and-stripped keyword
lazy fn load() -> Array<Paste> { return listPastes().pastes }

// after — a declaration; `lazy` is a method on the value
export fn data() -> Query<Array<Paste>> { return query(|p| api.pastes.list()).lazy() }
```

`Query<T>` is a `std/ui` record describing a deferred call: the closure, and
whether it is lazy. `vyxUnlazy` (the RFC-0070 scan-and-strip) is deleted.
`UiPageInfo.hasLoad` becomes `hasData`, sourced from contract checking rather
than name matching.

The `params` argument moves from an ambient loader parameter to the query
closure's declared parameter (`|p| … p.id`), which is what makes it typed — see
RFC-0073 for the generated `Params` record and its rename behaviour.

**Deprecation window.** For one release, `head { }` and `fn load()` continue to
work and emit a hint diagnostic naming the replacement, with a quick-fix that
performs the rewrite. `vyrn fmt` gains a `--migrate-contracts` pass that applies
both rewrites across a tree. After the window, the scanner paths are removed.

## Multi-fetch, and why `Page` stays closed

A page with two independent fetches does not need two exports and does not need
an open rule. One closure returning a record covers it:

```vyrn
export fn data() -> Query<HomeData> {
    return query(|p| HomeData {
        pastes: api.pastes.list().pastes,
        total:  api.pastes.count(),
    }).lazy()
}
```

This is strictly better than two `Query` exports: it is one closure, therefore
one combined round trip, and `HomeData` is an ordinary record in `shared/wire/`
that participates in completion, rename, and JSON codec generation like any
other type. Closing the contract cost nothing and bought total typo detection.

## What this does not do

- It does not validate member *bodies*. A `data` query that calls a procedure
  wrongly is caught by the ordinary type checker, not by contract checking.
- It does not make `Api`'s open slot typo-proof. Nothing can; procedure names
  are free-form by construction.
- It does not add runtime cost. Contracts are comptime-only; nothing about a
  contract survives into the emitted module.

## Milestones

- **M1 — declaration + checking.** `contract` declaration form in the frontend
  (parse, resolve, type members). `std/contract:checkContract` over
  `moduleInterface`. Diagnostics with did-you-mean. No generator changes yet;
  a test-only contract exercises the path.
- **M2 — `Page` and `Component`.** Declare both in std. Retrofit `std/ui` and
  `std/vyx` to check against them. `head` and `data` land as declarations, with
  the old forms alive behind a deprecation window.
- **M2b — the two prerequisites.** Neither was visible when this document was
  written; both block finishing the migration.
  - **A warning channel.** `Severity::Warning` exists as a variant
    (`diagnostics.rs:16`) but nothing ever produces one: `load()` returns
    `Result<Program, Vec<Diagnostic>>`, so there is no success-path diagnostic
    at all. Deprecation notices are therefore emitted as `//@deprecated`
    comments in generated output — visible in an artifact nobody reads. A real
    warning must thread through the loader, the CLI's print sites, and the LSP
    before any deprecation, quick-fix, or `fmt --migrate-contracts` can do its
    job. This is prerequisite work, not polish, and it is generally useful well
    beyond this RFC.
  - **Multi-shape members and `Lazy<P, T>`**, per the section above — without
    them, deleting `vyxParseHead` removes a capability, and the laziness scan
    only changes its name.
- **M2c — finish the migration.** `fmt --migrate-contracts`; migrate the
  remaining examples; delete `vyxParseHead` and `vyxScriptDataIsLazy`; close the
  window.
- **M3 — `Api`.** Declare the open contract; wire it in RFC-0072's `api` role.
  Serializability checking for procedure inputs and outputs. Note M2 already
  shipped the variadic open rule (`fn *(..)`) that `Api` needs, for `Component`.
- **M4 — LSP.** Role→contract resolution; completion, hover, go-to-def,
  quick-fix. `vyrn why --contract <file>` prints the resolved contract and every
  export's status.

## M1 — as landed

Eight places where the implementation is not what this document said, and why.

**`contract` is contextual, not reserved.** `std/rpc`, `std/connect`,
`std/openapi` and `std/graphql` each take a parameter literally named `contract`
(`gen fn rpcServer(contract: String)`), and applications name variables that way
too. Reserving the word would have broken all four std modules on day one, for
nothing. `contract` starts a declaration only in `contract <Ident> {` position —
the same trick `gen fn` / `extern fn` / `test "…"` / `bench "…"` already use.

**`checkContract(iface, contractOf(Page))`, not `checkContract(iface, Page)`.**
A contract is comptime-only, so a bare declaration name evaluating to a value
would need matching magic in the checker, the interpreter *and* the code
generator, and could be silently shadowed by a local named `Page`.
`contractOf(Name)` is the exact shape of `schemaOf(TypeName)`, which has carried
compile-time reflection since RFC-0003. It also reads honestly: the reflection is
visible at the call site.

**Member type parameters are recognized by spelling.** The RFC leaves them
undeclared and open per member, which means there is no list for the parser to
consult. The rule is the corpus's own convention made load-bearing: a single
uppercase ASCII letter, optionally followed by digits (`T`, `R`, `T1`). Anything
longer is a named type the checker must resolve — so `Haed` is still an error,
which is what keeps a typo in a *contract* as loud as a typo in a module.

**Checking reads exported functions only.** `moduleInterface` reflects functions
and types; `export let` does not exist yet (RFC-0029 makes module state
module-private). So a `let` member can today only be reported *absent*, or
type-mismatched against a same-named function. Every other rule is already live.
M2's `export let` joins the same list and nothing in `std/contract` changes.

**"module must export `data`", not "page must export `data`".** The role word
comes from `vyrn.json` role attachment, which is M2/M3. The message otherwise
matches the table, including the contract's declaring module.

**There was no did-you-mean implementation to reuse.** This document says the
`≤ 2` threshold is "the same threshold the existing hint diagnostics use"; the
repo's hints are all exact-match, and no edit distance existed anywhere. One
canonical `editDistance` (Damerau–Levenshtein, optimal-string-alignment) now
lives in `std/strings`, so the next consumer has something to reuse.

**`contractOf` has no native or wasm lowering.** Reaching it at runtime is a
compile error naming the reason, exactly as `moduleInterface` already does.
"Nothing about a contract survives into the emitted module" is enforced, not
merely intended — and the comptime-only property is what keeps interp == native
== wasm untouched.

**`ContractInfo.module` is an import specifier.** A generator is re-loaded as its
own root, so a contract declared *in* the generator (the normal case — `std/ui`
will declare `Page` and `std/ui`'s generator will check it) would otherwise carry
no module at all and every message would lose half its meaning. Contracts are
restamped with the specifier a reader could type (`std/ui`, `./gen`) when the
generator's private copy of the program is prepared.

## M2 — as landed

Nine places where the implementation is not what this document said, and why.

**`head` cannot see the page's data, so `head { title: … }` outlives the window
for dynamic titles.** The block form is an expression context evaluated with the
page's `params` and loaded `data` in scope (`vyxEmitHeadFns` binds them in a
prelude). `fn head() -> Head` takes no arguments and therefore cannot. Giving it
one does not help either: a page's shape varies — data, params, both, neither —
and one closed member has one signature, so whichever arity is chosen leaves a
real page unable to write `head` at all. `bin/routes/p/[id].vyx` is exactly that
page and it keeps the block. Closing this needs the member to be declarable at
more than one shape, which is a contract-grammar question, not a `std/ui` one.

**`Query<T>`'s deferred call takes no argument.** `query(|p| …)` needs a `p`
whose type is the page's own `Params`, and `Query` is declared in `std/ui`, which
cannot name it. `Query<P, T>` would push the params type into every page's
`data()` signature (and invent one for pages that have none), which is worse than
the problem. So the deferred call is `fn() -> T`, and a page whose data depends
on its route parameters keeps `fn load(p: Params)` for now. RFC-0073's generated
`Params` is what makes the closure's parameter typeable.

**`.lazy()` is `lazy(q)`.** Protocol impls are limited to scalars and enums
(`impl P for Query<Int64>` is a named refusal), so there is no method to hang on
a generic record. The combinator reads `lazy(query(|| …))`, which is the same
value and the same evaluation order.

**Laziness is still read from source.** `Query.lazy` is an ordinary runtime
field, but whether a page is lazy decides its *view type* (`PageData<T>` vs `T`)
and therefore must be known at generation time. The generator reads the `lazy(`
call in `data`'s body — the same scan `lazy fn load()` needed, moved one line in.
Making it type-level (`Query` vs a distinct lazy type) is the honest fix and is
follow-up work.

**`Head` carries `scripts` as well as `meta`.** The `head { … }` block can emit a
classic `script "…"`; dropping it would have made the replacement strictly less
expressive than the thing it deprecates. `meta` is additive and renders as
`<meta name content>`.

**A page builds a `Head` with combinators, not a partial record literal.**
`Head { modules: ["/app.js"] }` is not valid Vyrn — a record literal names every
field — so `noHead()` plus `withTitle`/`withStylesheet`/`withModule`/`withScript`/
`withMeta` is the spelling. The full literal works too.

**There is no non-fatal diagnostic channel from a generator, so the deprecation
notice is a directive comment.** A generator reports by emitting an identifier
line that fails to parse; that is fatal by construction, which is the opposite of
a deprecation hint. `load()` returns `Result<Program, Vec<Diagnostic>>` with no
success-path diagnostics, so a `Severity::Warning` would have to be threaded
through the loader, five CLI call sites and the LSP. The generated module carries
`//@deprecated RFC-0071: …` instead — greppable, and visible in `vyrn emit-gen`.
Wiring warnings end to end is the prerequisite M2b inherits.

**`laod` lands in the "not close" row.** This document's own example claims a
did-you-mean; Damerau-Levenshtein `laod`→`data` is 3, and `load` is no longer a
member for it to be one transposition from. It is still an error naming the
export — which is the substantive claim — just under `contract.unknown` rather
than `contract.unknown.didYouMean`. `dta` gets the suggestion.

**`Component` had nothing to name, and the open rule had to grow `(..)`.** The
components generator requires a `<template>` with exactly one root and nothing
else; a `.vyx` component's `<script>` is entirely module-private, so a component
has no hand-written export surface and there is no convention there to declare.
The truthful contract is the total one — every export of a components module is a
view returning `Html` — and views take zero, one or four props, which the open
rule could not express, because `matchesSignature` compares arity. `fn *(..) -> R`
now means "any parameters"; it is the notation this document already used in
prose, and `std/rpc`'s `Api` (M3) needs it for the same reason. The check runs
over the surface `components` is about to emit, since that surface exists nowhere
else before it is text.

Also landed, unplanned: `unify` learns function types when the pattern mentions a
type parameter, so `type Query<T> = { run: fn() -> T, … }` infers `T` from the
stored closure. Generic records with function-typed fields were simply not
constructible before — the record-literal path was the one inference site with no
`Type::Fn` arm.

## Acceptance

- `fn laod()` in a page is an **error** naming `data`, not a silent no-data page.
- `export fn head() -> String` is a type error against the contract's `Head`.
- Typing at module scope in a `.vyx` `<script>` offers `head` and `data` with
  their docs.
- Hover on `head` names contract `Page` and `std/ui`; go-to-def lands on it.
- A user-authored generator with its own contract gets identical completion with
  zero LSP changes — proven by an example generator in the test suite.
- Interp == native == wasm parity unaffected (comptime-only change), and the
  bin example's SSR bytes are byte-identical across the migration.
