# bin

A paste bin: write a paste, get a link, read it back as a page or as raw text.

```
cd examples/bin
vyrn dev
```

## Layout

- [server.vyrn](server.vyrn) is the native artifact. It composes the RPC handler, the OpenAPI document, both page trees and the theme stylesheet.
- [server/api/pastes.vyrn](server/api/pastes.vyrn) is the whole RPC surface. `rpc("./server/api")` mounts each export at `/_/pastes/<name>`; no file declares a route.
- [app/routes](app/routes) holds universal pages: rendered on the server and compiled into the browser bundle, so a soft navigation paints the next page itself.
- [server/routes](server/routes) holds the raw endpoint, which is server-only and never bundled.
- [client/boot.vyrn](client/boot.vyrn) is the browser artifact: the create form, validated with `fromJson` before the typed stub fires.

The manifest's `audience` key decides which tree may reach which artifact, so a page under `app/` cannot read the store.
