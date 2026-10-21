# std/http.editor

Editor support for the generators `std/http` exports.

No program links this module, so a program that uses `std/http` pays
nothing for it. A tool finds it beside `std/http` by its name.

## vyrnSymbolMapsHttp

```vyrn
fn vyrnSymbolMapsHttp() -> String
```

The generators of `std/http` that bake a `symbolMap` export into what they
write, one name per line.
