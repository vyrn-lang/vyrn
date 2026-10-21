# std/rpc.editor

Editor support for the generators `std/rpc` exports.

No program links this module, so a program that uses `std/rpc` pays
nothing for it. A tool finds it beside `std/rpc` by its name.

## vyrnSymbolMapsRpc

```vyrn
fn vyrnSymbolMapsRpc() -> String
```

The generators of `std/rpc` that bake a `symbolMap` export into what they
write, one name per line.

## vyrnDevServerRpc

```vyrn
fn vyrnDevServerRpc() -> String
```

The generators of `std/rpc` that mount a server surface, one name per
line: a root that calls one is a root `vyrn dev` serves.
