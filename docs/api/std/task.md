# std/task

Waiting for the host: `sleep`, the one host wait.

A function that reaches a wait waits itself, and each call of it is
marked `await` (language.md, Waiting).

## sleep

```vyrn
fn sleep(ms: Int64) -> Unit
```

Waits `ms` milliseconds while the host runs other tasks.
