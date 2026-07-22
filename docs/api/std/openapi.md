# std/openapi

std/openapi — the contract as an OpenAPI 3.1 document (RFC-0038), a library
entirely on RFC-0021 generator imports. One `gen fn`, `openapi(contract)`,
reflects the contract with `moduleInterface` and RETURNS a synthesized module
exporting `openapiJson() -> String`. The compiler knows nothing about OpenAPI:
everything below is comptime-pure Vyrn string building (the `css()` /
`rpcSchema()` precedent).

  import { openapi } from "std/openapi"
  import { openapiJson } from openapi("./contract")
  // openapiJson() -> String : a deterministic OpenAPI 3.1 document

The emitted `openapiJson()`:
  - `openapi: "3.1.0"`, `info` (title from the contract's module base name,
    a deterministic `version`);
  - one `paths` entry per procedure IN DECLARATION ORDER, describing the
    `POST /rpc/<proc>` surface `std/rpc` serves: a `requestBody` `$ref` to the
    request type's schema, a `200` `$ref` to the response type's schema, and
    the `422` request-validation Issues shape (`std/rpc`'s validation status);
  - `components/schemas`, one entry per type in the RFC-0031 reachable
    closure (SORTED by name, so imported wire types appear deterministically),
    each the type's `jsonSchema()` (RFC-0003) with an injected `$id` so its
    self-contained `#/$defs/..` references resolve within that component.

WRITER (RFC-0059): the emitted `openapiJson()` no longer hand-CONCATENATES the
JSON document (the old `acc = acc + "…"` writer-by-concat with a private
`oaEscBody` escaper). It now builds a `std/json` `Json` TREE and `emitPretty`s
it — the canonical writer owns escaping and layout. The document envelope
(`openapi`/`info`/`paths`/the `$ref`s/the `422` shape) is baked as compact JSON
constants and `parseJson`d into the tree; the schema BODIES stay runtime
`jsonSchema()` calls (the `rpcSchema()` precedent), because the full recursive
JSON Schema of a type is not available from `moduleInterface` reflection
(`ParamInfo.schema` carries only a scalar's shallow bounds — gap recorded in
RFC-0038). Each `jsonSchema()` is a compile-time constant string, so the whole
document is deterministic and byte-stable. The document is now PRETTY-PRINTED
(2-space indent) rather than compact — a deliberate, documented change from the
old concat document (RFC-0059 As-landed); it remains a valid, semantically
identical OpenAPI 3.1 spec (the exports parse-back test pins the structure).

Scope: a DOCUMENT, not a runtime. No callbacks/webhooks/auth schemes.

Inspect the synthesized module with:  vyrn emit-gen <file>

## openapi

```vyrn
fn openapi(contract: String) -> String
```

`openapi(contract)` — emit a module exporting `openapiJson() -> String`, an
OpenAPI 3.1 document describing the contract's `POST /rpc/*` surface.
