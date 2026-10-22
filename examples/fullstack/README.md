# fullstack

A server and a browser client that share one set of wire types, with no hand-written route or validation.

```
cd examples/fullstack
vyrn dev
```

## Layout

- [server.vyrn](server.vyrn) checks the RPC surface first and hands every other request to the file-based pages.
- [server/api/users.vyrn](server/api/users.vyrn) is an ordinary module. `rpc("./server/api")` mounts each export at `/_/users/<name>`, and `client("./server/api")` builds the browser stubs from its interface, so no procedure body reaches the bundle.
- [shared/wire.vyrn](shared/wire.vyrn) declares the validated types. `fromJson` runs every `where` clause on the server, so a bad `age` comes back as a structured 422.
- [server/pages](server/pages) are the server-rendered pages; [app/components](app/components) are the `.vyx` components the client renders.

`vyrn serve server.vyrn` runs the server alone.
