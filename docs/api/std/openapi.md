# std/openapi

std/openapi -- the contract as an OpenAPI 3.1 document, built from
generator imports. `openapi(contract)` reflects the contract with
`moduleInterface` and returns a module exporting `openapiJson() -> String`.

  import { openapi } from "std/openapi"
  import { openapiJson } from openapi("./contract")

The document holds one `POST /rpc/<proc>` path per procedure in declaration
order, with a `200` response and the `422` Issues shape `std/rpc` returns, and
one `components/schemas` entry per type in the reachable closure,
sorted by name. Each schema body is a runtime `jsonSchema()` call, because
`moduleInterface` carries only a scalar's shallow bounds; each is a compile-time
constant, so the document is byte-stable. `std/json` writes it pretty-printed.
No callbacks, webhooks or auth schemes.

## openapi

```vyrn
fn openapi(contract: String) -> String
```
