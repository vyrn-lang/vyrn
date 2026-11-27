# std/int64

The functions of `Int64`, its home: `Int64.parse(s)` calls them with no
import.

## parse

```vyrn
fn parse(s: String) -> Option<Int64>
```

`s` as a decimal `Int64`: an optional `-`, then digits, all consumed;
anything else is `None`. `acc * 10 + d` wraps, so `"9223372036854775808"`
is `Int64.min` (`examples/numbytes.vyrn`). It reads no `+`.
