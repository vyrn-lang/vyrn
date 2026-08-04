# RFC-0073 — Generator Symbol Maps: Rename Across the Boundary

- **Status:** Implemented. M1 landed (reflection origins, `std/symbolmap`,
  `client()`/`rpc()` maps, `vyrn emit-gen --maps`); M2 struck; M3 landed (LSP
  hover, go-to-def, route lenses); M4 landed (cross-boundary rename,
  `vyrn routes --json`, `http()` maps)
- **Depends on:** RFC-0033 (`//@origin` directives), RFC-0048 (vyx origins),
  RFC-0053 (generated error mapping), RFC-0050 (LSP references),
  RFC-0071 (contracts — members are the symbols this RFC maps),
  RFC-0072 (derived paths — the metadata this RFC surfaces on hover)
- **Evidence (user):** "full autocomplete and language integration", "does
  variables/function like `head` would be suggested by LSP?"

---

## The problem

`//@origin` maps generated *lines* back to source lines, which is enough for
diagnostics: an error inside emitted code reports at the author's cursor. It is
not enough for anything symbol-shaped.

Today, `api.pastes.list()` in a page is a call into a generated module. The LSP
can resolve it (cross-file hover and go-to-def over generated modules already
ship), but:

- **Rename does not cross the boundary.** Renaming `list` in
  `server/api/pastes.vyrn` does not touch `api.pastes.list()` call sites,
  because references stop at the generated module.
- **Derived facts are invisible.** RFC-0072 derives a wire path for every
  procedure. Nothing shows it in the editor, so the convention is something you
  remember rather than something you see.
- **Route params are stringly-typed.** A page reaches URL parameters through a
  string lookup, so a typo is a runtime miss, not a compile error.

A convention you cannot see is magic. This RFC makes the derived layer
inspectable and refactorable.

## The model

Generators emit, alongside code, a **symbol map**: for each exported symbol of
the generated module, the source declaration it stands for, plus any derived
metadata.

```
generated symbol            origin declaration                    derived
──────────────────────────  ────────────────────────────────────  ─────────────────────
api.pastes.list             server/api/pastes.vyrn:8:11 (list)    POST /_/pastes/list
api.pastes.byId             server/api/pastes.vyrn:9:11 (byId)    POST /_/pastes/byId
api.pastes.PasteList        shared/wire/paste.vyrn:12:13          —
Params.id                   app/routes/p/[id].vyx  (route seg)    —
```

This is `//@origin` promoted from line granularity to symbol granularity, and
extended with a derived-facts slot. The emission point is the same: generators
already know both halves at the moment they emit.

### Representation

A sibling `.map.json` per generated module, in the generator cache next to the
emitted source, keyed by the same content hash so it invalidates together:

```json
{
  "module": "client(./server/api)/pastes",
  "symbols": [
    { "name": "list",
      "origin": { "file": "server/api/pastes.vyrn", "line": 8, "col": 11, "name": "list" },
      "derived": { "kind": "rpc", "method": "POST", "path": "/_/pastes/list", "source": "convention" } },
    { "name": "byId",
      "origin": { "file": "server/api/pastes.vyrn", "line": 9, "col": 11, "name": "byId" },
      "derived": { "kind": "rpc", "method": "POST", "path": "/_/pastes/byId", "source": "convention" } }
  ]
}
```

JSON, not a bespoke format, so `vyrn routes`, the LSP, and any third-party tool
read the same file. `derived` is open — `std/http` writes cache and ETag policy
into it (RFC-0074), `std/ui` writes the route pattern.

`std/symbolmap` provides the emit helper so every generator produces the same
shape:

```vyrn
import { symbol, emitMap } from "std/symbolmap"

gen fn client(dir: String) -> Module {
    …
    emitMap([
        symbol("list", originOf(iface, "list"), rpcDerived(method, path, source)),
        …
    ])
}
```

## Typed route parameters

`std/ui` gains a generated `Params` record per dynamic route, derived from the
filename, with its fields mapped back to the path segments that produced them:

```
app/routes/p/[id].vyx        →  Params { id: String }
app/routes/[org]/[repo].vyx  →  Params { org: String, repo: String }
```

```vyx
export fn data() -> Query<Paste> { return query(|p| api.pastes.byId(IdReq { id: p.id })) }
```

`p.` completes to exactly the declared segments. Renaming the file to
`[pasteId].vyx` makes `p.id` a type error at that character, and the symbol map
gives the diagnostic a source: *"`Params` has no field `id`; this route declares
`pasteId` (app/routes/p/[pasteId].vyx)"*.

This replaces the string lookup form entirely. In a REST projection the same
check runs against the procedure's input type: `get("/{id}", byId)` requires
`IdReq` to have an `id` field, and `{ID}` is an error listing the available
fields.

> **This section describes work that happened without it.** `Params` is
> *declared* and checked against the segments (`std/ui.vyrn:1035`), not
> generated — a filename carries a segment's name and not its type, and the
> corpus uses both `Int64` and `String`, so the declaration is the only place
> the type is stated. REST placeholders are checked by a generated
> `String where value =~ …` refinement type, which `std/http`'s header notes
> "needs neither RFC-0073's symbol map nor a compiler rule". The string-lookup
> form no longer exists. See "M2 — read against what shipped".

## LSP capabilities

**Rename.** `textDocument/rename` on a procedure declaration collects
references through the symbol map: every generated symbol whose `origin` is the
renamed declaration is itself renamed, and its own references follow. The edit
spans source files only — generated modules are regenerated, never edited.

> **There was nothing to extend, and the new name has to be predicted rather
> than looked up.** No rename provider existed at all before M4, and the map
> cannot supply the generated symbol's NEW name — the module carrying it does not
> exist until the edit lands. See "M4 — as landed".

The payoff is the one the DX survey says no TypeScript framework can offer.
Because the client is generated from a checked declaration and then typechecked
as ordinary code, a missed call site is a **build error**, not a silent `any`.
Rename is therefore a convenience on top of a guarantee, rather than the only
thing standing between you and a runtime failure.

**Hover.** Hovering a procedure declaration shows its derived wire facts:

```
fn list() -> PasteList

POST /_/pastes/list · derived from server/api/pastes.vyrn
GET  /pastes        · explicit, server/api/pastes.http.vyrn:8 (cache 60s, etag)
```

Hovering a generated symbol at a call site shows the origin declaration's own
doc comment, not the stub's.

**CodeLens.** Above each `api` export, its derived path, click-to-open — the
same lens machinery as RFC-0064's dev entry and RFC-0055's bench lenses.

**Go-to-def.** Already works; the symbol map makes it land on the *declaration*
rather than the generated stub, which is what a reader wants.

> **It did not already work.** A generated symbol had no jumpable file at all —
> a namespace member carried none and an imported name carried a banner — so
> go-to-definition returned nothing rather than the stub. The map is not a
> refinement here; it is the whole of it. See "M3 — as landed".

## `vyrn routes`

The symbol map is the source of truth, so the command is a formatter over
existing data rather than a second implementation:

```
POST  /_/pastes/list       server/api/pastes.vyrn::list        convention
POST  /_/pastes/byId       server/api/pastes.vyrn::byId        convention
POST  /_/pastes/create     server/api/pastes.vyrn::create      convention
GET   /pastes              server/api/pastes.http.vyrn:8       explicit   cache=60 etag
GET   /pastes/{id}         server/api/pastes.http.vyrn:9       explicit   cache=3600 etag
POST  /pastes              server/api/pastes.http.vyrn:10      explicit   201
GET   /p/{id}              app/routes/p/[id].vyx               convention
GET   /                    app/routes/index.vyx                convention
```

`--json` emits the merged map for external tooling.

> **The table is a second CHANNEL, not a second implementation, and it is not
> read from the map.** It reads the `//@route` comment directives RFC-0072 M3 had
> the mounting generator emit; `--json` reads the maps and unions the two. A
> third channel evaluates the arguments of the program's `mount(..)` call, which
> is where the `explicit` rows above come from — a projection's paths are
> written, so nothing derives them and no generator can declare them.
> `std/ui` emits none of the three, so the PAGE rows above are still rows this
> command has never printed. See "M4 — as landed".

## Cache interaction

Generator output is content-addressed and cached (RFC-0021), keyed including the
generator name. Symbol maps join the cached artifact set under the same key: a
cache hit restores code and map together, so the LSP never sees a map that
disagrees with the code beside it. A map without its code, or the reverse, is a
cache-integrity error that forces regeneration rather than a silent skip.

## What this does not do

- It does not make generated modules editable. They remain build artifacts;
  rename rewrites sources and regenerates.
- It does not cover *values* — only symbols. A derived string embedded in a
  procedure body is not tracked.
- It does not extend to remote pinned modules, which have no local source to
  rename into. Hover and go-to-def over those keep today's behaviour.

## Milestones

- **M1 — format + emit.** `std/symbolmap`, the `.map.json` shape, cache
  integration. `client()` and `rpc()` emit maps. **Two prerequisites this
  document does not name, and one thing it should not build; see below.**

### M1 — what it actually needs

**Prerequisite: reflection has no origins.** `originOf(iface, "list")` above
cannot be written, because `FnInfo` is
`{ name, params, ret, retSchema, retUncodable, mutates }` — no file, no line, no
column. The whole RFC is "promote `//@origin` from line granularity to symbol
granularity", and the symbol half is not in the reflection the generators read.

That is an addition rather than a problem: `retUncodable` arrived with RFC-0071
M3 and `mutates` with RFC-0074 M4a, both by the same route. `FnInfo.origin`
is the third, and `TypeInfo` needs one too — the sketch above maps
`api.pastes.PasteList` back to `shared/wire/paste.vyrn:12:13`.

**The thing not to build: a second generator output.** The sketch writes
`gen fn client(dir: String) -> Module` and `emitMap([...])`. There is no
`Module`; a `gen fn` returns `String`, the emitted source, and adding a second
artifact means a new generator protocol, a new cache entry to keep in step, and
a new way for the two to disagree.

**The map is the module.** Emit it as an ordinary exported function —
`symbolMap() -> String`, returning the JSON — and every one of those problems
disappears: the cache already keys the module by content hash, so a map that
lives *inside* the module cannot go stale relative to it, and "cache
integration" stops being a milestone item. The LSP already runs generators as
compiled wasm (RFC-0076), so reading it costs a call.

`std/symbolmap` then provides the **builder**, not an emitter: `symbol(..)` and
`mapJson(..)` produce the string every generator bakes in, so the shape is
shared for the reason the sketch wanted — one library, one format.

The sibling `.map.json` still exists, and is written by the CLI on request
rather than by the generator. That keeps the RFC's actual requirement — a
third-party tool reads JSON, not Vyrn — without making a generator responsible
for a file it cannot invalidate.

### M1 — as landed

`FnInfo` and `TypeInfo` carry an `Origin`, `std/symbolmap` builds the document,
`client()` and `rpc()` bake a `symbolMap()` into what they emit, and
`vyrn emit-gen --maps` prints it as JSON. Six places where the implementation is
not what this document said, and why.

**The origin cost a lexer pass, not a span rewrite.** The AST carries a `line`
per declaration and no column at all — `symbols.rs` says why: threading spans
through every node construction site is high churn for something two consumers
want, so the LSP recovers a declaration's name column from the lexer's per-token
`(line, col)` instead. Reflection now does the same, once per module rather than
once per lookup: `Origins` lexes each module the reflected link read and indexes
the first identifier of each `(line, name)`. The sources were already in hand —
`gen_module_interface_lit` records every module the link touched so a closure
type's defining file joins the generator's cache inputs (RFC-0031) — so the index
is built from the same reads, and a module reflected is a module indexed by
construction. Re-lexing is also what keeps a comment or a string containing the
name from being mistaken for it, which a substring search would not.

**`Origin` carries a `name`, and it is not a restatement.** The record is
`{ file, line, col, name }`, where `name` is the DECLARATION's name — routinely
not the generated symbol's. `client()` exports `pastesCreate` and `rpc()`
dispatches to `rpcHandlePastesCreate`; both stand for `create` in
`server/api/pastes.vyrn`. That is the sketch's own `"name": "list"` field, and it
is what lets a consumer holding one origin render `pastes.vyrn:28:15 (create)`
without also holding the `FnInfo` it came from.

**The map-inside-the-module decision held, and it removed work rather than
adding it.** No new generator protocol, no second cache entry, no atomicity rule
to enforce: the map is an export of the module, so a cache hit that restores the
code restores the map, and the "cache integration" milestone item and the
"partial artifact set forces regeneration" acceptance line are both moot. The
generator side is one call — `symbolMapFn(module, symbols)` appended to the
emitted source — and the JSON is baked as a string literal through an RFC-0054
code quote, so the compiler's own escaping does the second layer rather than a
hand-rolled escaper free to disagree with the lexer.

**The CLI surface is `vyrn emit-gen --maps`, and it writes no file.** The
smallest thing that satisfies the requirement — a third-party tool reads JSON,
not Vyrn — is a flag on the command that already runs every generator and already
banners its output. It prints one compact document per line, banners on stderr,
so `> api.map.json` produces the sibling file without this command inventing a
NAME for it. That is the part worth not guessing: a name would have to be a slug
of a generator CALL (`client("../server/api")`), and `vyrn routes --json` in M4
will decide how the maps are addressed with the merged table in hand. Reading is
a parse, not a run: the map is a string literal the generator baked in.

**`rpc()`'s mapped symbols are its internal handlers, which the sketch's "each
exported symbol" does not cover.** The router exports exactly one function,
`rpcHandle`, so mapping only exports would map nothing on the server side. Each
`rpcHandlePastesCreate` stands for exactly one declaration, and both maps name
that declaration at the same file, line and column — which is the property the
cross-boundary rename in M4 needs, and is under test. `client()` maps its
procedure stubs AND its re-emitted types: a re-emitted `type` has lost its file
in the generated source, so the map is the only place that still says
`PasteList` came from `shared/wire/paste.vyrn`. That is the sketch's third row,
and the reason `TypeInfo` needed an origin as well as `FnInfo`.

**It found a comptime-purity bug that had nothing to do with symbol maps.**
`std/symbolmap` reaches `std/json`'s `emit`, whose body is
`JArr(items) => emitArr(items)` — and the purity analysis collected `let` and
`for` binders as locals but never a match arm's pattern binding. So `items` read
as a reference to module state, and in the one example that happens to declare
`let mut items` (`examples/rpcsplit`) every generator reaching `emit` became
impure. Naming a binder after a global in a module it cannot see is a
coincidence, not an effect. The fix is scoped rather than flat — an arm's binders
shadow inside that arm and not in its siblings — and `if let` was missing the
same thing.

- **M2 — typed `Params`.** ~~Generated per-route `Params` records with mapped
  fields; placeholder checking in REST projections; the string-lookup form
  removed.~~ **Struck.** All three are already done, moot, or impossible — see
  below.

### M2 — read against what shipped

**Field names are already checked against the segments.** `std/ui.vyrn:1035`
does it at generation time through `moduleInterface`, and the module's own
header has said so since it was written: *"the field NAMES must match the
segments exactly."* Rename `[id].vyx` to `[pasteId].vyx` and the declaration
stops matching — which is precisely the behaviour this milestone was written to
add.

**Placeholder checking in REST projections shipped without this RFC**, and
`std/http`'s header says why in as many words: a procedure's generated parameter
type is a `String where value =~ …` admitting exactly the placeholders its input
record has fields for, so `byId("/{ID}")` is a checker error at the call site and
*"needs neither RFC-0073's symbol map nor a compiler rule."* That is a better
mechanism than the one proposed here — a refinement type the checker already
understands, rather than a generated table something has to consult.

**The string-lookup form is already gone.** There is nothing left in `std/` or
the corpus to remove.

**And the one item that is not done cannot be.** "A generated `Params` record
derived from the filename" would have to invent the field *types*, and a
filename does not carry them: `[id]` says a segment is named `id` and nothing
about whether it is an `Int64` or a `String`. The corpus uses both. So the
declaration is not redundancy to be generated away — **it is the only place the
type is stated**, and the check against the filename is what keeps it honest.

The milestone was written before any of this existed and reads as though the
declaration were the problem. It is the answer.

**What genuinely survives** is the diagnostic's *source*: today the mismatch is
reported by the generator, and M1's symbol map could let it name the file and
line that declares the conflicting segment. That is a note on M3 (LSP read),
not a milestone of its own.
- **M3 — LSP read.** Hover with derived facts, go-to-def onto declarations,
  CodeLens with paths.

### M3 — as landed

All three shipped. The document was wrong about the easiest of them, and the
hardest one turned out to be a question about *acquisition* rather than about
hover.

**"Go-to-def already works" was false, and it was the clearest win here.** It
returned NOTHING. A namespace member of a generated module is built from the
synthesized source, which has no file, so `namespace_members` set `file: None`
and `resolve` reported `definition: false`; a selectively imported name carried
the generated module's BANNER as its file, which `Url::from_file_path` rejects.
Either way `api.pastesCreate` was a dead end. The map supplies the file, the
line and — the part the AST could not have given even if it had a file — the
COLUMN, so the jump lands on the four characters of `byId` rather than on the
head of its line. Both import forms are fixed at once, because both are
rewritten by one pass over the symbols an analysis already built.

**Hover proved to be two different problems wearing one name.** At a CALL SITE
the map is free: the generated source is in hand, the map is a string literal
inside it, and the origin's `///` is one read of a file the reader is about to
be sent to anyway. That hover now shows the stub's own signature (it is what you
call, callback and all), the DECLARATION's doc, the derived route, and where the
declaration is written.

At the DECLARATION it is not free, and this is the thing the document does not
say: **`server/api/pastes.vyrn` reaches no generator.** It is what a generator
reads, not what reads one, so the file open in the editor cannot know what
`create` is mounted at — only a root that calls `rpc(..)` or `client(..)` can.
So the facts are acquired the way RFC-0049 acquires a `.vyx`'s owner, pointed
the other way: consult the roots already analyzed (in a full-stack window the
server root or the client boot usually is), and otherwise analyze the roots near
the file that TEXTUALLY call a map-emitting generator, nearest first. One such
probe costs ~840 ms in `examples/bin` and every hover after it costs under a
millisecond, because the winning root's map covers the whole api directory and
is cached for all of it at once.

**The cache's only invalidation is a root being re-analyzed, and that is
enough.** Clearing it on every `.vyrn` edit was the first version and it was
wrong: it made an edit-then-hover cycle pay the probe again and again. A cached
entry can go stale only by its declaration MOVING, and the hover matches on the
declaration's name AND line, so a stale entry makes a route note quietly
disappear until the mounting root is re-analyzed — it never appears on the wrong
declaration. Installing a root refreshes the facts for every module it maps, so
the accurate answer arrives by the same door the diagnostics do.

**CodeLens is not a server capability in this project, and it stays that way.**
Every lens the editor shows — the run lens, RFC-0064's dev entry, RFC-0055's
bench lenses — is built in `extension.js`, and the semantic ones ask the server
through a custom request. So the route lens does too: `vyrn/routeLenses` answers
`{ line, title, method, path, source }` per procedure, from the same cache the
hover reads. One lens per DECLARATION and not per generated symbol, because a
procedure is mapped twice (the client's stub and the server's handler) and both
name it at the same place — the very agreement M1 put under test. The lens
carries no command: a POST endpoint is not something a click can usefully open,
and the lens exists to make a derived fact visible.

**The reader is a parse of the module's TAIL.** `symbolMapFn` appends the
declaration last, so `vyrn_frontend::symbolmap` finds `export fn symbolMap()`
textually and lexes only what follows — a generated client is tens of kilobytes
and the LSP reads maps on every keystroke. `vyrn emit-gen --maps` now goes
through the same function, so there is one reader rather than one per consumer.
Keystroke latency is unchanged: 27.8 ms → 28.1 ms on `examples/bin`'s client
root, and a `.vyx` in the same app measures 184 ms both before and after the
change (RFC-0076's budget is about a `.vyx`, and that number is the app's, not
this milestone's).

**What M2 left as a note on M3 is still a note.** The `Params`/segment mismatch
is reported by `std/ui` at generation time and could name the declaring file and
line through the map; nothing here needed it, and a generator diagnostic is not
an LSP read.
- **M4 — rename.** Cross-boundary rename with regeneration; `vyrn routes` and
  `--json` over the merged map.

### M4 — as landed

`textDocument/rename` (with `prepareRename`) and `vyrn routes --json` both ship,
and the acceptance line runs against `examples/bin` rather than a fixture. Five
things the document did not anticipate.

**There was no rename provider to extend — and the machinery to build one on
already existed.** RFC-0050 shipped `references`, which resolves the binding
under a cursor and returns its ACTUAL occurrences: member-position tokens
attributed to their receiver, bare tokens excluded where an in-scope local
shadows them, comments never lexed at all. That is exactly a rename's reference
collection, minus one thing — it is keyed by a CURSOR, and a cross-file rename
knows a NAME. So the last branch of `references` became `references_to(analysis,
name, qualifiers)` and `references` now calls it; there is one shadow rule and
one member rule, not two. `qualifiers` is the addition: a name reached as
`store.listPastes` counts when `store` names the declaring module and an
unrelated `other.listPastes` does not, which a cursor-keyed query never had to
decide.

**The reference set is bounded by imports, and that is what makes a token match
safe.** Vyrn re-exports nothing implicitly, so a name can only occur in a module
that imports the module declaring it — which the candidate file's own parsed
imports settle, resolved through `loader::resolve_spec`. Within such a file the
occurrences are lexer TOKENS, so `recentRows`, four prose mentions of "recent" in
comments, and `"recent"` inside a string literal all stay put while the import
binding and the one call move. A `.vyx` contributes its `<script>` body, whose
lines map back to the file by addition.

**The declaration → generated name direction has to be PREDICTED, not read.** M3
inverted stub → declaration by looking the stub up in a map that already existed.
This direction has no map to read: the module carrying `pastesRecent` will not
exist until the rename lands and the loader regenerates. So the new name is
derived the way the generator derives it — a prefix the generator chose, then
`capFirst` of the declaration — which covers `pastesCreate`, `rpcHandlePastesCreate`,
`PathCreate` and a same-named re-export with one rule. A generated name that does
not end in the declaration's name REFUSES the whole rename with the reason,
because a rename that skipped one symbol would leave a call site pointing at
something nothing generates any more, and the build error would name the wrong
file.

**M3's route-facts cache answers a different question than a rename asks.** It
probes mounting roots and stops at the FIRST that claims the file, which settles
"what is this mounted at" and is why a declaration hover costs one probe. A
rename asks "what else is named after this", and stopping early is how you
rewrite the client's call sites and leave the server's: a procedure in
`examples/bin` is mapped by three generators in three different roots. So rename
reads every mounting root and unions, and the M3 path is untouched — the
keystroke and hover budgets are unchanged (`.vyrn` 183–188 ms and `.vyx`
477–494 ms with the generator cache off, hover 1.04 s cold and under a
millisecond warm, all identical before and after; a rename costs ~500 ms once).

**Two things the milestone found that were not in it.**

*The REST projection had no map at all.* `http("./pastes")` re-exports each
procedure under the DECLARATION's own name, so nothing in the emitted source
records that the projection's `create` is `pastes.vyrn`'s `create` — and renaming
one without the other breaks the projection. `http()` now emits a map (origins
only: a projection's paths are written in the projection file, not derived), and
that also gives its symbols M3's hover and go-to-definition for free.

*Which immediately exposed an M1 bug.* M1 named the map function `symbolMap` in
every generated module, invisible while exactly one generator emitted one. A
top-level name in Vyrn is program-wide, so a server root linking both
`rpc("./server/api")` and a projection's `http("./pastes")` stopped compiling the
moment the second map existed. The declaration now carries a slug of the
generator call (`symbolMapHttpPastes`) and the reader matches the prefix. The bug
was latent before this milestone and would have been hit by the second
map-emitting generator whenever it arrived.

**And what a partial rename does, since it is the thing the design leans on.**
Verified rather than argued: applying only the declaration's edit to
`examples/bin` and building gives `server/api/pastes.http.vyrn:11: generated by
http("./pastes") ... does not define 'create'` on the server side and `namespace
'api' ... has no exported member 'pastesCreate'` on the client. A build error
naming the call site, on both sides of the boundary — never a silent `any`, and
never a runtime 404. What the rename cannot reach is therefore loud: a `.vyx`
TEMPLATE expression (only the script body is lexed; a template calls the page's
own view helpers, so the corpus has none) and the WIRE itself, since renaming a
procedure moves its derived path and no external HTTP client is in the tree.

**`vyrn routes` was not a formatter over the map, and `--json` only half is.**
The text table reads `//@route` COMMENT DIRECTIVES that RFC-0072 M3 had the
mounting generator emit. That is not a second implementation — the derivation
happens once, in the generator, and both artifacts come out of the same route
list — but it is a second CHANNEL, and the channels carry different things: a
directive has nowhere to put the declaration's file, line and column. So `--json`
reads the maps, which is what makes "`vyrn routes --json` and the LSP agree"
true by construction, and UNIONS them with the directives so a future generator
emitting only one is not silently dropped. Today the union is the same set.
`std/ui` emitted neither at the time, so page routes were in neither the table nor
the JSON — the document's example table showed rows no version of this command had
ever printed. `std/ui` now emits directives (see the note below), so the table has
them; it still emits no map, so their `origin` in `--json` is `null`.

> **The `explicit` rows now print; the page rows still do not.** Both channels
> above read what a GENERATOR wrote, and a hand-written projection has no
> generator — so `examples/bin` printed three of the eight paths it answers. A
> third channel closes it by reading the `Route`/`Live`/`Socket` values the
> program hands `std/http`'s `mount`, evaluated from that call's argument list.
> It carries no origin, so those rows' `origin` is `null` in `--json`, which is
> the state this section already describes rather than a new one. Pages are a
> producer of a different kind: `std/ui` hands `mount` nothing at all — a page
> router is a `fn(Request) -> Response` that always answers, mounted in
> `examples/bin` behind an ordinary `if req.path.startsWith("/raw/")` — so the
> generator knows the tree-relative path and not the prefix the app serves it
> under, and a directive it emitted would be wrong. Printing pages means giving
> `std/ui` a route list, not teaching this command another format.

> **Pages, and both reasons they were absent were mistaken.** `std/ui` was given
> its route list — `routes()`, one `Route` per page — and with it the `//@route`
> directives the first channel already reads, so the command still knows nothing
> about pages and `examples/bin` prints twelve rows instead of eight. A page
> router does always answer, but its PAGES do not: each route matches one pattern
> and declines the rest, and the 404 that always answers is the tree's fallback,
> which stays in the composition root's `None` arm. And the prefix was never
> unknown. A page router matches `req.path` WHOLE against its own tree's segments,
> so a tree cannot be re-hung under a prefix at all; the `startsWith("/raw/")`
> above was a hand-written dispatch guard standing in front of a tree that
> contains `raw/[id].vyrn` and derives `/raw/{id}` by itself. The guard is gone
> and the tree is a group.

## Acceptance

- Renaming `list` → `recent` in `server/api/pastes.vyrn` updates every
  `api.pastes.list()` call site across `app/` and `client/` in one edit.
- Deleting a procedure that is still called is a build error naming the call
  sites — never a silent `any`, never a runtime 404.
- Hover on a procedure shows its derived path and whether it is convention or
  override.
- Renaming `[id].vyx` to `[pasteId].vyx` produces a type error at `p.id` naming
  the new segment.
- `vyrn routes --json` and the LSP agree, because both read the same maps.
- Generator cache hits restore code and map atomically; a partial artifact set
  forces regeneration.
