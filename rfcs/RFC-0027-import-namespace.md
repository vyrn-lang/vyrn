# RFC-0027 — `import * as ns`: Namespaced Imports

- **Status:** Draft (design locked)
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
