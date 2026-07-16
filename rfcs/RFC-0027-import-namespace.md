# RFC-0027 — `import * as ns`: Namespaced Imports

- **Status:** Implemented — see `compiler/` and the three-way parity corpus
  (`examples/namespace.vyrn` + `examples/lib/{shapes,metrics}.vyrn`)
- **Depends on:** RFC-0010 (modules — the flat import namespace this fixes),
  RFC-0022 (import aliasing — the co-naming trick this obsoletes for its
  worst cases), RFC-0021 (generator imports — synthesized modules must be
  namespaceable too)
- **Evidence (three independent hits in one arc):** the RFC-0026 pages
  router imports many same-named `page`/`Params`/`load` exports and can
  only do it by abusing RFC-0022 co-naming (declaring inert local dummies
  to force renames); `.vyx` component export names collide with the app's
  flat namespace (`List.vyx` → `list` is a reserved builtin, `Issues.vyx`
  collided with a root local); and the deferred pages-`.vyx` bridge needs
  it. The generator-library composition rule ("prefix all internals") is
  the same disease at the std level.

---

## Surface

```vyrn
import * as api from "./api"
import * as ui from pages("./pages")        // generator imports too

fn main() -> Int64 {
    let u = api.getUser(api.GetUserReq { id: 7 })   // expression + record ctor
    let r: api.User = u                              // type position
    return 0
}
```

- `import * as ns from <source>` binds ONE name, `ns`, in the importing
  module. **None of the module's exports enter the flat namespace** — that
  is the entire point.
- `ns.member` is legal wherever the underlying export is: calls, record
  construction, type annotations, generic arguments, protocol bounds,
  `fromJson(ns.User, s)`-style type-name arguments, match patterns on an
  imported enum's variants (`ns.Color.Red` — see below).
- Composes with selective imports: a module may both `import { getUser }
  from "./api"` and `import * as api from "./api"` (they resolve to the
  same decls).

## Semantics (locked)

- **A namespace is a compile-time name, not a value.** `let x = api`,
  passing `api` to a function, or `api` alone in expression position are
  errors ("namespace `api` is not a value"). No runtime representation
  exists in any backend.
- **Resolution order:** at a use `head.rest…`, if `head` resolves to a
  local binding/param, it is field/method access (unchanged today). Only
  otherwise, if `head` names an in-scope namespace, the pair resolves as a
  qualified name. A local binding may therefore shadow a namespace —
  discouraged via a warning-class diagnostic, but well-defined.
- **Namespace name collisions:** binding two namespaces to one name, or a
  namespace over an existing top-level decl name, is a load error (same
  wording family as duplicate imports today).
- **Visibility:** `ns.member` reaches EXPORTED decls only — the same
  surface a selective import could reach. Nothing new is exposed.
- **Enum variants:** for an exported enum `type Color = | Red | Green`,
  construction and patterns accept `ns.Color.Red`. Bare `Red` still works
  when the enum itself is selectively imported — unchanged.
- **Transitivity:** namespaces do not re-export. `import * as a from "./a"`
  then `a.b.thing` (where `./a` itself namespaced `./b`) is an error —
  namespaces are one level deep, per-module.

## Mechanism (why this is cheap)

The loader already has exactly the right pass: RFC-0022's
`resolve_aliases` folds aliased names into the flat namespace BEFORE
register/visibility/merge so everything downstream stays alias-unaware.
Namespaced imports extend that pass:

1. Parse `import * as ns` into the module header (new `ImportNs` form).
2. During resolution, rewrite every `ns.member` qualified reference (a new
   `QualifiedName` node produced by name-resolution, NOT by the parser —
   the parser keeps emitting plain member-access; resolution reinterprets
   it when `head` names a namespace) to the foreign decl's program-wide
   symbol — the same renamed-symbol mechanics co-naming uses today
   (`member__fromN`).
3. Downstream (checker, interp, codegen, codec, LSP analysis) sees
   ordinary resolved names. **Zero backend changes; parity inherited.**

The router's inert-dummy trick and the `.vyx` collision renames are then
deleted where they buy nothing (std/ui and std/vyx migrate to namespacing
their imports of user modules where it simplifies — generated code is the
first consumer).

## Editor & tooling

- LSP: completion after `ns.` (exports of that module), hover shows the
  original decl with "— via namespace `ns`", go-to-def jumps into the
  module (cross-file machinery exists). `fmt` treats `import * as ns` as a
  header line like existing imports.
- Redeploy vyrn-lsp.exe after landing (standing rule).

## Out of scope

Re-exports (`export * from`), nested namespaces, namespace values /
first-class modules, renaming members at the namespace boundary
(`ns.foo as bar` — selective imports already do this), wildcard VALUE
imports (`import * from` — never; the flat namespace is the disease).

---

## Implementation notes & decisions (as landed)

- **AST / parser.** `ImportDecl` gained `namespace: Option<String>` (`names`
  empty when set); `import * as ns from <source>` reuses the existing import
  source parser, so string paths AND generator calls (`import * as ui from
  pages("./pages")`) both work. Plain member-access cannot spell three
  positions, so the parser was extended minimally to emit EXISTING nodes with a
  dotted spelling the loader strips: `ns.Type { .. }` → `StructLit` named
  `"ns.Type"`; `ns.Type` / `ns.Box<T>` in type position → `Type::Named`/`App`
  named `"ns.Type"`; `ns.Color.Red` match patterns → `Pattern::Variant("ns.
  Color.Red", ..)`. Everything else already parsed: `ns.fn(x)` and
  `ns.Enum.Variant(x)` are method-call sugar (`Call` whose first arg is the
  `ns`/`ns.Enum` receiver), and `ns.member` / `ns.Enum.Variant` are `Field`
  chains.

- **Where resolution hooks in.** All reinterpretation lives in the loader's
  `resolve_aliases` pass (the RFC-0022 mechanism), BEFORE register/visibility/
  merge, so the checker/interp/codegen/codec/LSP stay namespace-unaware —
  **zero backend changes**. Order: collect + validate namespace bindings →
  co-naming rename decisions (RFC-0022) → **namespace rename decisions** → build
  rewrite maps → apply foreign renames → apply alias rewrites + normalize
  imports → **Pass 5: `NsResolver`** walks each namespaced module scope-aware and
  folds every `ns.member` use to the resolved program-wide symbol.

- **Renames only on collision.** A namespaced module's export is renamed to a
  fresh `member__fromN` symbol only when its name is declared by ≥2 modules
  (otherwise it keeps its name — no churn). `ns.member` and any selective
  importer of the same decl both resolve to that symbol via the existing
  co-naming machinery, which is what lets two namespaced modules export the same
  name and coexist (the whole point).

- **Resolution-order subtlety.** `ns.fn(x)` and `someFn(ns.Type, x)` parse
  IDENTICALLY (`Call` with a `Field`/`Var` first arg). They are told apart by a
  per-module **exported-enum-variant set**: `Call{name, [ns.Enum, ..]}` is
  variant construction only when `name` is a variant of that namespaced module's
  enums; otherwise `ns.Type` is a type-name argument and the `Field` arm rewrites
  it. `NsResolver` tracks locals (params, `let`, `for`, lambda params, match
  binds) so a local binding shadows a namespace (well-defined, per the locked
  semantics). Diagnostics landed as specified: namespace-is-not-a-value,
  duplicate/colliding namespace names, exported-only (`no exported member`),
  and one-level-deep (`a.b.thing` → `no exported member \`b\``).

- **Shadowing diagnostic.** The diagnostic layer is error-only (no warning
  class), so the "discouraged" shadowing case is simply well-defined and silent
  rather than warned — a local binding shadows a namespace, as documented here.

- **Consumers migrated.** The std/ui pages router (`std/ui.vyrn`) now emits
  `import * as p<idx> from "<page>"` and references `p<idx>.page` /
  `p<idx>.Params { .. }` / `p<idx>.load(p)`, deleting the RFC-0022 inert-dummy
  co-naming trick (`fn page()`/`type Params`/… no longer emitted). The `.vyx`
  consumers were left alone: the current corpus already sidesteps flat-namespace
  collisions by naming (components like `Listing`/`IssuePanel`, and the fullstack
  client imports only the root `app`), so namespacing them would be churn with no
  benefit — the router was the genuine win. `examples/namespace.vyrn` is the new
  parity citizen (expression, type, generic-arg, enum-variant construction +
  patterns, type-name argument, and a namespaced generator import).

- **Editor.** `analyze_linked` indexes each `import * as ns` binding and the
  target module's EXPORTED decls (parsed from the module's own source, so names/
  lines are the module's — a collision rename never leaks into the editor):
  completion after `ns.` offers the exports (noted "— via namespace `ns`"),
  hover on `ns.member` shows the decl with that note, go-to-definition jumps into
  the source module, and hovering the `ns` binding shows a "not a value" summary.
  A synthesized generator namespace has no readable file, so its `ns.` offers
  nothing (graceful). `fmt` prints `import * as ns from ..` as a stable header
  line (re-lex invariant holds). `editor/vscode/server/vyrn-lsp.exe` was rebuilt
  (release) and redeployed.

- **Known limitation.** Enum variants are global (not renamed), so two
  DIFFERENT namespaced modules exporting an enum with the same variant name
  (`ns1.A.Red` and `ns2.B.Red`) would collide on the bare `Red` — the same
  pre-existing flat-variant limitation, not made worse. No corpus case hits it.
