# dsl

A project's own language: `*.calc` ledgers, read by the grammar `calc.syntax` through the module `calc`.

```
cd examples/dsl
vyrn run main.vyrn
```

`vyrn.json` maps `*.calc` to `./calc`, so the editor reads [orders.calc](orders.calc) through [calc.vyrn](calc.vyrn): it colours the file from the grammar and refuses a broken line in the sentence [calc.errors/en.json](calc.errors/en.json) holds. [calc.editor.vyrn](calc.editor.vyrn) also refuses an account the ledger does not know, and shows what the ledger pays an account on hover. [main.vyrn](main.vyrn) prints the total of `orders.calc`, and the total of a ledger it writes as a literal, which `calc.paid` takes as a `LedgerText`: the check reads that literal with the grammar and refuses a broken line in it.
