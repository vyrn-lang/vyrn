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

    /// This page's data. The return TYPE decides both the render strategy and
    /// whether the deferred call sees the route parameters.
    fn data() -> Query<T> = noQuery()
    fn data() -> Lazy<T>
    fn data() -> ParamQuery<P, T>
    fn data() -> ParamLazy<P, T>
}
```

Two consequences worth stating plainly.

**Params-ness rides the NAME, not a type argument.** The obvious spelling is
`Query<P, T>` — the params type as an open parameter, since `Params` is declared
per page and `std/ui` therefore cannot name it. But that spelling forces *every*
page to name a params type, including the ones that have none: a paramless
`index.vyx` would have to invent a `Params` purely to satisfy a parameter it
never uses, which is exactly the objection that made `Query<P, T>` wrong in the
first place. So the fact goes where the laziness fact was already going — into
the name. Four types, four alternatives, and a page names a params type only if
it has one. `paramQuery(fetchPaste)` typechecks because `P` is solved from the
named loader's own parameter.

`Lazy<T>` being a distinct type rather than a flag moves the last scan onto
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
change to `.vyx` pages, made all at once.

**There is no deprecation window and no migration tooling.** Vyrn is pre-1.0
with no users and no third-party code: every page that exists lives in this
repo. A compatibility window would exist to protect code that does not exist,
and would cost two parallel implementations of every page form, kept alive and
tested, for nobody. The old forms are migrated and deleted in the same change.

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
from a scanned structure to a reflected value. The scanner is removed from the
PAGE path. It survives for layouts and error pages, which have no contract to be
a member of — see M2c below.

**`fn load()` → `export fn data()`.** The name-matched loader becomes a `Query`
value, which also absorbs RFC-0070's `lazy` marker:

```vyrn
// before — magic name; `lazy` is a scanned-and-stripped keyword
lazy fn load() -> Array<Paste> { return listPastes().pastes }

// after — a declaration; `lazy` is a method on the value
export fn data() -> Query<Array<Paste>> { return query(|p| api.pastes.list()).lazy() }
```

`Query<T>` is a `std/ui` record describing a deferred call. `vyxUnlazy` (the
RFC-0070 scan-and-strip) is deleted, along with every `fn load` scanner beside
it. `UiPageInfo.hasData` is sourced from contract checking rather than name
matching; `load` survives only as the name of the wrapper `std/vyx` GENERATES
around a `.vyx` page's `data` accessor, which no author writes.

The `params` argument moves from an ambient loader parameter to the query
closure's declared parameter (`|p| … p.id`), which is what makes it typed — see
RFC-0073 for the generated `Params` record and its rename behaviour.

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
  - **A warning channel.** `Severity::Warning` existed as a variant
    (`diagnostics.rs:16`) but nothing ever produced one: `load()` returns
    `Result<Program, Vec<Diagnostic>>`, so there was no success-path diagnostic
    at all. Built in M2b Part A and threaded through the loader, the CLI print
    sites and the LSP, with `--deny-warnings` for CI. Its only consumer was the
    deprecation notice, which M2c deletes — so it is retained on its own merits
    (a compiler with no way to say "this compiled, but"), not because this RFC
    still needs it.
  - **Multi-shape members and `Lazy<P, T>`**, per the section above — without
    them, deleting `vyxParseHead` removes a capability, and the laziness scan
    only changes its name.
- **M2c — one form, not two.** Migrate the remaining examples by hand; delete
  the page-path `head { … }` scanner, `vyxUnlazy` and the deprecation machinery
  that carried the old forms. One way to write a page. **Landed** — see below.
- **M3 — `Api`.** Declare the open contract; wire it in RFC-0072's `api` role.
  Serializability checking for procedure inputs and outputs. Note M2 already
  shipped the variadic open rule (`fn *(..)`) that `Api` needs, for `Component`.
- **M4 — LSP.** Role→contract resolution; completion, hover, go-to-def,
  quick-fix. `vyrn why --contract <file>` prints the resolved contract and every
  export's status. **Landed** — see below.

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

## M2b — as landed

Eight places where the implementation is not what this document said, and why.

**`Query<P, T>` is four types, not two.** The spelling this document originally
chose forces *every* page to name a params type, including the ones that have
none, so the params-ness went into the NAME instead: `Query<T>`, `Lazy<T>`,
`ParamQuery<P, T>`, `ParamLazy<P, T>`. M2c folded that back into "Multi-shape
members" above, so the document and the code now agree; the reasoning is recorded
there rather than here.

**`head`'s four alternatives are three shapes.** Under open member type
parameters `fn head(d: T) -> Head` and `fn head(p: P) -> Head` are the same
signature, so declaring both is documentation, not discrimination —
`matchedMember` cannot tell them apart and always reports the earlier. Both are
declared anyway, because the four-line block is the clearest statement of the
house rule, and the generator distinguishes the one-argument case by the declared
parameter TYPE: a page's route parameters are always its own `Params` and nothing
else can be. That is still a question about a declaration, which is the whole
distinction this RFC turns on.

**A `fn` parameter could not solve a type parameter.** `paramQuery(run: fn(P) ->
T)` is the first function in the corpus where a `fn` parameter is the ONLY place
a type parameter occurs; every previous one (`map<T, U>(xs: Array<T>, f: fn(T) ->
U)`) had an ordinary argument pinning it first, and `check_fn_arg` only ever
unified the function value's RETURN. So the call could not typecheck at all —
`P` stayed `P` and every comparison against it failed. `check_fn_arg` now unifies
the parameter types too. This is a general inference fix, not a contracts one.

**A closure cannot annotate its parameter, so `data` takes a named function.**
`paramQuery(|p: Params| …)` is not Vyrn — a lambda's parameters are typed by the
signature they flow into, never by the author. With the inference fix a bare
`paramQuery(fetchPaste)` works and reads better, so the migrated page passes its
loader by name. `query(|| …)` still works for the paramless forms, where nothing
needs solving.

**`vyxParseHead` is NOT deleted, and pairing it with `vyxScriptDataIsLazy` in the
M2c list was a mistake.** `head { … }` is not only a page form: `layout.vyx` and
`error.vyx` use it too, through `vyxBuildLayoutModule`, and a LAYOUT has no
contract — `Page` is about pages. Both `bin` and `shelf` have a layout with a head
block, and neither is deprecated. Deleting the parser needs a `Layout` contract
to exist first, which is not in this RFC. `vyxScriptDataIsLazy` IS gone, and with
it `vyxDeclBody`'s only caller on the page path — laziness is now a type.

**A `.vyrn` page's deprecation warning has no line.** `ModuleInterface` reflects
functions with no source position, so `std/ui` cannot say WHERE a page's `fn
load` is; re-reading the page's source to find out would put back the scanner
this RFC removes. The directive therefore carries `-` (the explicit "no position"
marker) and the message names the module. A `.vyx` page gets a real file:line,
because there the generator is already holding the source.

**The `-` marker had to be taught to the loader.** The inherited M2b work emitted
it and the directive parser kept it as the first word of the message, so an
unpositioned warning read `- \`fn load\` is deprecated`. It is now consumed.

**`doc` and `fmt` never reach `load_program`, so they print no warnings.** The
one-print-site design holds for every command that BUILDS a program (`check`,
`run`, `emit-ir`, `build`, `test`, `bench`, `serve`, `dev`). `fmt` is a token
rewriter, `doc` renders over sources, and `emit-gen` prints the generated text —
where a `//@deprecated` directive is already visible verbatim. Three commands
that never load cannot warn, and should not.

## M2c — as landed

Six places where the implementation is not what this document said, and why.

**There were three examples to migrate, not four.** The M2c milestone said
"there are four"; the compiler's own warnings named three page modules
(`examples/pages/users/[id]`, `examples/fullstack/pages/users/[id]`,
`examples/shelf/routes/books/[id]`), all `.vyrn`, all `fn load(p: Params) ->
Validation<Data>`. `examples/rpc` and `examples/rpcsplit` have no pages at all —
they are `rpcInProcess` dogfoods. Every migrated page's SSR output is
byte-identical.

**`vyxParseHead` is still not deleted, and this is the second time that entry has
been wrong.** M2b already recorded why: `head { … }` is also a LAYOUT and ERROR
page form (`vyxBuildLayoutModule`, `vyxBuildErrorModule`), and a layout has no
contract — `Page` is about pages. Both `bin` and `shelf` have a layout with a
head block. What M2c could delete, and did, is the block on the PAGE path: the
`vyxStripHead` call in `vyxBuildPageModule`, the `VYX_HEAD_AND_HEAD_BLOCK`
conflict it needed, and the deprecation notice. A `head { … }` in a page now
reaches the compiled body as source and fails there. Deleting the parser outright
needs a `Layout` contract, which is not in this RFC.

**`load` did not disappear; it stopped being a name anyone writes.** A `.vyx`
page is compiled into a module, and the router calls that module's `load()`.
`vyxBuildPageModule` still emits one — a single runner call over the `data`
accessor the page declared. So the deletion is of every SCANNER
(`vyxUnlazy`, `vyxScriptHasLoad`, `vyxScriptLoadIsLazy`,
`vyxScriptLoadHasParams`, `vyxScriptLoadRet`) and of `std/ui`'s
`uiHasFn(iface, "load")` name match. `export fn load` in a `.vyx` page is now an
unknown export against a closed contract, which is louder than the deprecation it
replaced.

**The `//@deprecated` directive was renamed, not deleted.** It is a generator's
only way to say something non-fatal, and the milestone says to keep the warning
channel. Deleting it would have left `Diagnostic::warning`, `load_warned`,
`--deny-warnings` and the LSP's warning publication with no producer and no
possible test — a capability that cannot be exercised is a capability that rots.
It is `//@warning` now, which is what it always was; nothing about the mechanism
was ever specific to deprecation. `origin::deprecations` is `origin::warnings`.

**The migration exposed a native-only miscompile, and parity forced it to be
fixed here.** `paramQuery(run: fn(P) -> T)` is the shape where a type parameter
occurs ONLY inside a `fn` parameter's own parameter list. M2b fixed the CHECKER
for it (`check_fn_arg` learned to unify parameter types); codegen's
`gen_ho_call` still solved the callee's type parameters from the target's RETURN
alone. So `P` survived monomorphization as a bare `Type::Param`, which lowers to
`void` — an `alloca void`, a `void` argument, and a dispatcher keyed on a
signature no construction registers ("internal: invalid function value"). The
interpreter was unaffected, so it was invisible until a parity citizen
(`pagesdemo.vyrn`) used the shape. `gen_ho_call` now solves from the target's
declared parameters too; `examples/fnvalstore.vyrn` carries the regression.

**One SSR byte changed, deliberately.** `examples/fullstack/pages/users/[id].vyrn`
renders a sentence about itself that named `load` in a `<code>` element. Leaving
it would have kept the migration byte-identical at the cost of shipping a page
that describes a form that no longer exists. It says `data` now, and that single
line is the only SSR difference across all four examples.

## M4 — as landed

Seven places where the implementation is not what this document said, and why.

**Role attachment falls back to the generator call site, because no project
writes a `roles` key.** This document specifies `vyrn.json`'s `roles` map and
RFC-0072 owns its general form; the key is read exactly as written here and wins
whenever present. But not one project in the repo has one, so a fallback that
did nothing would have shipped a milestone that never fires. The fallback asks
the source the same question the generator does: a root module says `import {
route } from pages("./routes")`, so `./routes` is the pages directory, and the
contract is the one exported by the module the generator itself was imported
from (`std/ui`). No blessed directory names and no table of generator names —
the directory comes from the call and the contract comes from the import. A
generator module exporting zero or several contracts is skipped, because then
there is nothing unambiguous to resolve.

**There is one blessed-name table, and it is the chrome stems.** A `routes/`
directory holds pages and ALSO `layout.vyx` and `error.vyx`, which M2b and M2c
both record are not pages — a layout has no contract to be a member of. Role
attachment is by directory, so without an exception list the editor would offer
`head` and `data` inside a layout, which is exactly the misfire this milestone
exists to prevent. `Role::except` defaults to `["layout", "error"]`, mirroring
`std/ui`'s own `uiScanAll` chrome test, and a project overrides it per role with
the object form `{ "contract": "std/ui:Page", "except": [...] }`. A `Layout`
contract — not in this RFC — deletes the table.

**Completion offers one item per SHAPE, not one per member.** This document's
completion sketch shows two rows, one per member, from before multi-shape
members existed. `data` is declared at four shapes whose snippets differ in the
return type, and that type is the entire decision the member encodes
(`Query`/`Lazy` × params or not). Collapsing them would have made the completion
list say less than the contract does. All four are offered, labelled `data` so
the prefix matches, distinguished by `detail`, ordered required-first then
declaration order.

**A snippet's parameters are tabstops, including their types.** The document's
example (`export fn head() -> Head { return $0 }`) is exactly what the
zero-argument shape inserts. But a member's type parameters are open, and a real
page writes `fn head(d: Array<Paste>)`, not `fn head(d: T)` — so a parameterised
shape inserts `export fn head(${1:t}: ${2:T}) -> Head { return $0 }`, seeded with
the contract's own spelling. The shape is right immediately and tabbing fills in
what only the page knows.

**Go-to-definition wins only at module scope, which is what makes it safe.**
"Jumps to the member in the contract declaration" and "jumps to the page's own
`fn head`" are both right answers, in different places. The rule that separates
them is brace depth: on the declaration's own name the ordinary resolution is a
self-jump (the cursor is already on what it resolves to), so the contract is
strictly more useful; inside a body, `head()` is a call and keeps resolving to
the page. The same gate governs hover's contract note and completion, and it is
one lex with no parse — `vyrn_frontend::at_module_scope`.

**The quick-fix is computed from the contract, not from a diagnostic message.**
This document says the did-you-mean is "wired to a rename edit", which reads as
parsing the diagnostic. A generator's diagnostic text is its own business —
`std/ui` bakes issues into `PAGES_CONTRACT__<fid>__<key>__<path>` identifier
lines — and reading that in the server would compile a generator into it. The
action is instead derived from the same two facts the generator uses: an export
the closed contract does not name, within the same Damerau-Levenshtein threshold.
The client's diagnostics are attached by RANGE overlap, so the lightbulb still
appears on the squiggle without the server understanding a word of it. It renames
the declaration only; a page that calls its own misspelled accessor internally
needs the second edit by hand.

**`vyrn why --contract` reports; it does not gate.** A `.vyrn` page's own surface
carries the router's entry point (`page`/`respond`), which `Page` does not name,
so an unknown-export line is expected there today and RFC-0072 owns closing it.
Exiting non-zero on that would make the command call every working `.vyrn` page
broken. It exits 0 whenever it could answer, 1 when the file is in no role, and
prints an objection count naming the generator as the actual gate. The status
computation needed a Rust twin of `std/contract`'s `typeMatches`/`matchesSignature`
and of `std/strings:editDistance`; the alternative was running the comptime
interpreter on every keystroke, which is not an editor. The edit-distance twins
are pinned together by a test that RUNS the Vyrn one and compares.

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
