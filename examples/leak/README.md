# leak

A browser bundle that would read a file three imports away, and the check that refuses it.

```
cd examples/leak
vyrn check client/boot.vyrn
```

The check fails, and its note names each import from `client/boot.vyrn` to `server/db.vyrn`:

```
server/db.vyrn:6:0: artifact `app` (browser) cannot include `server/db.vyrn`: it reaches the filesystem
```

- [client/boot.vyrn](client/boot.vyrn) imports one formatting helper and never names `server/`.
- [shared/format.vyrn](shared/format.vyrn) is pure, but re-exports the store's reader.
- [server/db.vyrn](server/db.vyrn) calls `readFile`. The project declares no `audience` key, so no path says it is server code; the capability does.
- [server/main.vyrn](server/main.vyrn) reaches the same module from the native artifact and compiles, because a binary has a filesystem.
