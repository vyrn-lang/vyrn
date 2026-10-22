# shelf

A book shelf: add, rate, tag and delete books, in two languages, with every API document generated from one module.

```
cd examples/shelf
vyrn dev
```

## Layout

- [server.vyrn](server.vyrn) serves the RPC and Connect handlers, the OpenAPI and GraphQL documents, the page router and the theme stylesheet, all derived from the api directory.
- [server/api/books.vyrn](server/api/books.vyrn) is the whole RPC surface. `rpc("./server/api")` mounts each export at `/_/books/<name>`.
- [server/routes](server/routes) are server-rendered pages, which may read the store through the api module.
- [client/boot.vyrn](client/boot.vyrn) holds the browser's view state and renders the [app/widgets](app/widgets) island. It validates a new book with `fromJson` before the typed stub fires.
- [strings](strings) holds the translations that `i18n("../strings")` turns into typed functions.
