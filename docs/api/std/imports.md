# std/imports

std/imports: the import block of a generated module.

A generator that emits code over a module's types and procedures writes one
`import` line per declaring module, so each type keeps its identity. A
re-emitted copy would collide in the flat namespace.

```vyrn
import { importBlock, procImports } from "std/imports"

export gen fn server(contract: String) -> String {
    let iface = moduleInterface(contract)
    return importBlock(iface, contract, procImports(iface, false)) + "..."
}
```

## procImports

```vyrn
fn procImports(iface: ModuleInterface, procAlias: Bool) -> Array<String>
```

Returns the names `iface`'s procedures import under: each as is, or with
`procAlias` as `<name> as <name>__real`.

## importBlock

```vyrn
fn importBlock(iface: ModuleInterface, contract: String, procs: Array<String>) -> String
```

Returns one `import { ... } from "<module>"` line for each module that
declares a type of `iface.types`, in first-use order. The module named
`contract` comes first and also imports `procs`. An empty `contract` with
no `procs` imports the types alone.
