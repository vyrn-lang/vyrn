# std/contract

std/contract — check a module against a module contract (RFC-0071).

A `contract` declaration states which exports a module may have, with their
types, optionality and documentation. `checkContract` compares one against a
module's `moduleInterface` reflection and returns RFC-0009 `Issue`s, so a
generator replaces name-hunting and source-scanning with one call:

    import { checkContract } from "std/contract"

    gen fn pages(dir: String) -> String {
        let iface = moduleInterface(pageFile)
        let issues = checkContract(iface, contractOf(Page))
        ...
    }

Everything here is ordinary library code over two ordinary records. The
compiler knows the *declaration form* and nothing else — which is what lets a
third-party generator declare its own contract and get the same behaviour
without a compiler change.

The five conditions RFC-0071 enumerates, all reported and none silent:

  - required member absent            -> `contract.missing`
  - member type mismatch              -> `contract.type`
  - unknown export, close to a member -> `contract.unknown.didYouMean`
  - unknown export, not close         -> `contract.unknown`
  - open-rule shape mismatch          -> `contract.open`

**The export surface this reads.** `ModuleInterface` reflects a module's
exported FUNCTIONS (RFC-0021 / RFC-0031); `export let` does not exist yet, so
a value member (`let head: Head`) can today only be *absent*, or found under
the name of a function — which is itself a type mismatch and is reported as
one. When RFC-0071 M2 lands `export let`, the reflected values join
`moduleExports` below and every rule here applies unchanged.

## checkContract

```vyrn
fn checkContract(iface: ModuleInterface, c: ContractInfo) -> Array<Issue>
```

Check `iface` (a module's reflected surface) against contract `c`, returning
one RFC-0009 `Issue` per problem — in a fixed order: the contract's members
in declaration order first, then the module's unrecognized exports in
reflection order. Deterministic, because a generator bakes these into a
diagnostic and byte-stability is the whole game.

An empty result means the module satisfies the contract.
