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
    let head: Head = Head { }

    /// This page's data, resolved before render. `.lazy()` opts into
    /// render-then-fill; without it the page blocks until the data lands.
    let data: Query<T> = noQuery()
}
```

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
member        := doc? "let" Ident ":" Type ("=" Expr)?          // value member
               | doc? "fn"  Ident "(" params ")" "->" Type       // function member
               | doc? "fn"  "*"   "(" params ")" "->" Type       // open rule
```

- A member with a default (`= Expr`) is **optional**; the default is used when
  the module does not export it.
- A member without a default is **required**; its absence is an error naming
  the contract and the member.
- Type parameters appearing in member types (`Query<T>`) are open: the module
  may export any instantiation. `T` is bound per member, not per contract.

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
    let issues = checkContract(iface, Page)      // Array<Issue>, RFC-0009 shape
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

Snippet body is the full declaration (`export let head = Head { $0 }`), so the
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

**`head { … }` → `export let head`.** The block form (`std/vyx.vyrn:2739`)
becomes a value:

```vyrn
// before — scanner-parsed block, invisible to the editor
head {
    module "/app.js"
}

// after — an ordinary declaration with a type
export let head = Head { modules: ["/app.js"] }
```

`Head` is a record in `std/ui`: `{ title: Option<String>, modules: Array<String>,
stylesheets: Array<String>, meta: Array<Meta> }`. The head-emission path in
`std/ui` (`document(title, head, body)`) is unchanged; only its input changes
from a scanned structure to a reflected value. The scanner
(`vyxParseHead` and friends) is deleted.

**`fn load()` → `export let data`.** The name-matched loader becomes a `Query`
value, which also absorbs RFC-0070's `lazy` marker:

```vyrn
// before — magic name; `lazy` is a scanned-and-stripped keyword
lazy fn load() -> Array<Paste> { return listPastes().pastes }

// after — a declaration; `lazy` is a method on the value
export let data = query(|p| api.pastes.list()).lazy()
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
export let data = query(|p| HomeData {
    pastes: api.pastes.list().pastes,
    total:  api.pastes.count(),
}).lazy()
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
  `std/vyx` to check against them. `head` and `data` land as declarations with
  the deprecation window and `fmt --migrate-contracts`. Delete `vyxParseHead`
  and `vyxUnlazy` behind the window.
- **M3 — `Api`.** Declare the open contract; wire it in RFC-0072's `api` role.
  Serializability checking for procedure inputs and outputs.
- **M4 — LSP.** Role→contract resolution; completion, hover, go-to-def,
  quick-fix. `vyrn why --contract <file>` prints the resolved contract and every
  export's status.

## Acceptance

- `fn laod()` in a page is an **error** naming `data`, not a silent no-data page.
- `export let head = "Title"` is a type error against `Head`.
- Typing at module scope in a `.vyx` `<script>` offers `head` and `data` with
  their docs.
- Hover on `head` names contract `Page` and `std/ui`; go-to-def lands on it.
- A user-authored generator with its own contract gets identical completion with
  zero LSP changes — proven by an example generator in the test suite.
- Interp == native == wasm parity unaffected (comptime-only change), and the
  bin example's SSR bytes are byte-identical across the migration.
