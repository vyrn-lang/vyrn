# Where the CI minutes go

Measured over the last 20 completed runs of each workflow. The cuts are the
owner's call; this census only measures.


## Do not shard the site suite (measured 2026-08-25)

The slowest step in this table is `The site's own tests` at 1018 s median, of
which 687 s is `vyrn test site/export.vyrn`. Splitting it across a CI matrix is
the obvious answer and it is the wrong one. `vyrn test --shard i/n` was built,
measured and reverted; this is why.

Locally, after RFC-0113:

| | |
| --- | --- |
| the whole suite, 34 tests | **16.15 s** |
| the slowest of four round-robin shards | **12.80 s** |
| loading and generating, no test selected | **0.32 s** |
| every test run ALONE, times added up | **88.7 s** |

The last two lines are the finding. Setup is not the fixed cost — it is a third
of a second. But thirty-four tests that take 88.7 s apart take 16.2 s together,
because they SHARE the render: the site's modules cache their parse and their
pages in module state, so the first test that needs a page pays for it and the
other thirty-three do not.

Sharding breaks exactly that. Four runners each pay the full render, so the
compute goes up fourfold to take 21 per cent off the wall clock — and 21 per
cent of 687 s is still nine minutes, which does not reach the goal it was for.

What follows for this step: its cost is one render, not thirty-four tests. It
gets faster when rendering gets faster — which is what took the export from
26.3 s to 12.75 s — or when it renders less. It does not get faster by being cut
into pieces, and a future reader reaching for a matrix should read this first.

## Part one — the step-level record

One row per step, per job, per operating system. Sorted by median, largest
first. Every step of every sampled run appears, including the zero-second
bookkeeping steps GitHub records (Set up job, post-steps, Complete job).

| workflow | job | os | step name | runs sampled | median seconds | p90 seconds | max seconds |
|---|---|---|---|---|---|---|---|
| site | build | ubuntu-latest | The site's own tests | 19 | 1018.0 | 1433.0 | 1449.0 |
| ci | three-way parity (interp == native == wasm) | ubuntu-latest | Run the parity harness | 20 | 464.5 | 479.0 | 483.0 |
| ci | tests (workspace) — windows-latest | windows-latest | Workspace tests | 20 | 279.0 | 307.0 | 334.0 |
| ci | tests (workspace) — ubuntu-latest | ubuntu-latest | Workspace tests | 20 | 208.0 | 234.0 | 235.0 |
| ci | tests (workspace) — macos-latest | macos-latest | Workspace tests | 20 | 155.0 | 187.0 | 197.0 |
| ci | tests (workspace) — ubuntu-24.04-arm | ubuntu-24.04-arm | Workspace tests | 20 | 140.0 | 144.0 | 145.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — windows-latest | windows-latest | LSP tests (excluded crate) | 11 | 134.0 | 377.0 | 440.0 |
| ci | benchmarks (regression gate) | ubuntu-latest | Run benches + compare to baseline | 3 | 125.0 | 129.0 | 129.0 |
| ci | checks (bench, LSP, docs, install) — windows-latest | windows-latest | LSP tests (excluded crate) | 9 | 125.0 | 146.0 | 146.0 |
| site | build | ubuntu-latest | Render every page | 19 | 91.0 | 249.0 | 250.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — windows-latest | windows-latest | Build vyrn (release) | 11 | 87.0 | 92.0 | 95.0 |
| release | build x86_64-windows | windows-latest | Build the CLI | 2 | 86.0 | 93.0 | 93.0 |
| ci | checks (bench, LSP, docs, install) — windows-latest | windows-latest | Build vyrn (release) | 9 | 83.0 | 86.0 | 86.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — macos-latest | macos-latest | LSP tests (excluded crate) | 11 | 70.0 | 207.0 | 246.0 |
| ci | checks (bench, LSP, docs, install) — macos-latest | macos-latest | LSP tests (excluded crate) | 9 | 65.0 | 85.0 | 85.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-latest | ubuntu-latest | LSP tests (excluded crate) | 11 | 64.0 | 136.0 | 217.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-latest | ubuntu-latest | LSP tests (excluded crate) | 9 | 64.0 | 66.0 | 66.0 |
| ci | cross-engine generation (interp == wasm) | ubuntu-latest | Every generator agrees under both engines (RFC-0076) | 20 | 60.5 | 67.0 | 75.0 |
| ci | checks (bench, LSP, docs, install) — windows-latest | windows-latest | Bench --check (deterministic, RFC-0063) | 9 | 60.0 | 63.0 | 63.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — windows-latest | windows-latest | Bench --check (deterministic, RFC-0063) | 11 | 59.0 | 59.0 | 62.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | LSP tests (excluded crate) | 11 | 50.0 | 136.0 | 137.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-latest | ubuntu-latest | Build vyrn (release) | 9 | 50.0 | 53.0 | 53.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | LSP tests (excluded crate) | 9 | 48.0 | 50.0 | 50.0 |
| site | build | ubuntu-latest | Build the CLI | 19 | 48.0 | 54.0 | 54.0 |
| ci | checks (bench, LSP, docs, install) — macos-latest | macos-latest | Build vyrn (release) | 9 | 47.0 | 56.0 | 56.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — macos-latest | macos-latest | Build vyrn (release) | 11 | 46.0 | 60.0 | 66.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-latest | ubuntu-latest | Build vyrn (release) | 11 | 46.0 | 50.0 | 61.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-latest | ubuntu-latest | Bench --check (deterministic, RFC-0063) | 9 | 44.0 | 47.0 | 47.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-latest | ubuntu-latest | Bench --check (deterministic, RFC-0063) | 11 | 43.0 | 45.0 | 46.0 |
| site | build | ubuntu-latest | The playground's own checks | 19 | 41.0 | 44.0 | 46.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Build vyrn (release) | 9 | 39.0 | 45.0 | 45.0 |
| release | build x86_64-linux | ubuntu-latest | Build the CLI | 2 | 39.0 | 42.0 | 42.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Build vyrn (release) | 11 | 38.0 | 41.0 | 43.0 |
| release | build aarch64-macos | macos-latest | Build the CLI | 2 | 37.5 | 42.0 | 42.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Bench --check (deterministic, RFC-0063) | 11 | 35.0 | 35.0 | 35.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Bench --check (deterministic, RFC-0063) | 9 | 35.0 | 35.0 | 35.0 |
| release | build aarch64-linux | ubuntu-24.04-arm | Build the CLI | 1 | 33.0 | 33.0 | 33.0 |
| ci | tests (workspace) — windows-latest | windows-latest | Build vyrn, and put the pinned test runner on PATH | 20 | 29.5 | 33.0 | 48.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — macos-latest | macos-latest | Bench --check (deterministic, RFC-0063) | 11 | 29.0 | 39.0 | 41.0 |
| ci | checks (bench, LSP, docs, install) — macos-latest | macos-latest | Bench --check (deterministic, RFC-0063) | 9 | 29.0 | 40.0 | 40.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — windows-latest | windows-latest | genwasm tests (excluded crate) | 11 | 24.0 | 117.0 | 118.0 |
| site | build | ubuntu-latest | The playground module | 19 | 21.0 | 24.0 | 26.0 |
| ci | benchmarks (regression gate) | ubuntu-latest | Build vyrn | 3 | 20.0 | 21.0 | 21.0 |
| ci | three-way parity (interp == native == wasm) | ubuntu-latest | Build vyrn | 20 | 19.0 | 24.0 | 32.0 |
| ci | tests (workspace) — ubuntu-latest | ubuntu-latest | Build vyrn, and put the pinned test runner on PATH | 20 | 18.0 | 21.0 | 33.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — windows-latest | windows-latest | Rust cache | 11 | 18.0 | 29.0 | 30.0 |
| ci | checks (bench, LSP, docs, install) — macos-latest | macos-latest | The install script installs, and refuses what it cannot verify | 18 | 17.5 | 36.0 | 37.0 |
| ci | tests (workspace) — ubuntu-24.04-arm | ubuntu-24.04-arm | Build vyrn, and put the pinned test runner on PATH | 20 | 16.0 | 21.0 | 37.0 |
| ci | tests (workspace) — macos-latest | macos-latest | Build vyrn, and put the pinned test runner on PATH | 20 | 16.0 | 18.0 | 20.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | genwasm tests (excluded crate) | 11 | 15.0 | 45.0 | 46.0 |
| ci | checks (bench, LSP, docs, install) — windows-latest | windows-latest | Rust cache | 9 | 14.0 | 21.0 | 21.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — macos-latest | macos-latest | genwasm tests (excluded crate) | 11 | 13.0 | 52.0 | 84.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-latest | ubuntu-latest | genwasm tests (excluded crate) | 11 | 12.0 | 38.0 | 61.0 |
| ci | three-way parity (interp == native == wasm) | ubuntu-latest | Universal-pages integration (RFC-0069) | 20 | 12.0 | 13.0 | 13.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Rust cache | 11 | 12.0 | 14.0 | 16.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-latest | ubuntu-latest | Rust cache | 11 | 11.0 | 17.0 | 18.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — macos-latest | macos-latest | Rust cache | 11 | 9.0 | 12.0 | 13.0 |
| ci | tests (workspace) — windows-latest | windows-latest | Rust cache | 20 | 9.0 | 13.0 | 18.0 |
| ci | checks (bench, LSP, docs, install) — macos-latest | macos-latest | Rust cache | 9 | 9.0 | 11.0 | 11.0 |
| ci | three-way parity (interp == native == wasm) | ubuntu-latest | The codegen integration tests that need a toolchain (RFC-0077) | 20 | 7.0 | 8.0 | 8.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-latest | ubuntu-latest | Rust cache | 9 | 7.0 | 8.0 | 8.0 |
| ci | benchmarks (regression gate) | ubuntu-latest | Rust cache | 3 | 6.0 | 8.0 | 8.0 |
| ci | tests (workspace) — windows-latest | windows-latest | Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 20 | 6.0 | 7.0 | 9.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — windows-latest | windows-latest | Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 11 | 6.0 | 8.0 | 12.0 |
| ci | checks (bench, LSP, docs, install) — windows-latest | windows-latest | Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 9 | 6.0 | 7.0 | 7.0 |
| site | deploy | ubuntu-latest | Run actions/deploy-pages@d6db90164ac5ed86f2b6aed7e0febac5b3c0c03e | 2 | 6.0 | 6.0 | 6.0 |
| release | build x86_64-windows | windows-latest | Run actions/checkout@v4 | 2 | 5.5 | 6.0 | 6.0 |
| ci | cross-engine generation (interp == wasm) | ubuntu-latest | Rust cache | 20 | 5.0 | 6.0 | 7.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Rust cache | 9 | 5.0 | 8.0 | 8.0 |
| site | build | ubuntu-latest | A minute with Vyrn, recorded | 19 | 5.0 | 11.0 | 12.0 |
| site | build | ubuntu-latest | The site's node tests | 19 | 5.0 | 9.0 | 10.0 |
| release | publish the release | ubuntu-latest | Publish | 2 | 5.0 | 7.0 | 7.0 |
| ci | checks (bench, LSP, docs, install) — windows-latest | windows-latest | The install script installs, and refuses what it cannot verify | 18 | 4.5 | 10.0 | 10.0 |
| ci | tests (workspace) — windows-latest | windows-latest | Formatting gate | 20 | 4.0 | 5.0 | 21.0 |
| ci | three-way parity (interp == native == wasm) | ubuntu-latest | Rust cache | 20 | 4.0 | 5.0 | 6.0 |
| ci | tests (workspace) — ubuntu-latest | ubuntu-latest | Rust cache | 20 | 3.0 | 5.0 | 6.0 |
| ci | tests (workspace) — ubuntu-latest | ubuntu-latest | Formatting gate | 20 | 3.0 | 5.0 | 6.0 |
| ci | tests (workspace) — macos-latest | macos-latest | Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 20 | 3.0 | 3.0 | 4.0 |
| ci | tests (workspace) — macos-latest | macos-latest | Rust cache | 20 | 3.0 | 4.0 | 4.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — windows-latest | windows-latest | Browser runtime tests (web/) and the extension's own | 11 | 3.0 | 6.0 | 7.0 |
| ci | checks (bench, LSP, docs, install) — macos-latest | macos-latest | Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 9 | 3.0 | 3.0 | 3.0 |
| ci | checks (bench, LSP, docs, install) — windows-latest | windows-latest | Browser runtime tests (web/) and the extension's own | 9 | 3.0 | 8.0 | 8.0 |
| site | build | ubuntu-latest | Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 19 | 3.0 | 4.0 | 4.0 |
| site | build | ubuntu-latest | Rust cache | 19 | 3.0 | 6.0 | 9.0 |
| release | build aarch64-linux | ubuntu-24.04-arm | Run actions/checkout@v4 | 1 | 3.0 | 3.0 | 3.0 |
| release | build x86_64-windows | windows-latest | Run the artifact before anyone else has to | 1 | 3.0 | 3.0 | 3.0 |
| ci | tests (workspace) — ubuntu-24.04-arm | ubuntu-24.04-arm | Rust cache | 20 | 2.5 | 5.0 | 13.0 |
| release | build aarch64-macos | macos-latest | Run actions/checkout@v4 | 2 | 2.5 | 3.0 | 3.0 |
| ci | tests (workspace) — ubuntu-latest | ubuntu-latest | Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 20 | 2.0 | 2.0 | 3.0 |
| ci | cross-engine generation (interp == wasm) | ubuntu-latest | Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 20 | 2.0 | 2.0 | 3.0 |
| ci | benchmarks (regression gate) | ubuntu-latest | Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 3 | 2.0 | 3.0 | 3.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — macos-latest | macos-latest | Set up job | 11 | 2.0 | 2.0 | 3.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — macos-latest | macos-latest | Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 11 | 2.0 | 3.0 | 3.0 |
| ci | tests (workspace) — ubuntu-24.04-arm | ubuntu-24.04-arm | Formatting gate | 20 | 2.0 | 3.0 | 6.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-latest | ubuntu-latest | Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 11 | 2.0 | 2.0 | 3.0 |
| ci | tests (workspace) — macos-latest | macos-latest | Set up job | 20 | 2.0 | 2.0 | 2.0 |
| ci | tests (workspace) — macos-latest | macos-latest | Pinned test-runner cache | 20 | 2.0 | 3.0 | 3.0 |
| ci | tests (workspace) — windows-latest | windows-latest | Post Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 20 | 2.0 | 2.0 | 3.0 |
| ci | three-way parity (interp == native == wasm) | ubuntu-latest | Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 20 | 2.0 | 2.0 | 2.0 |
| ci | three-way parity (interp == native == wasm) | ubuntu-latest | Pinned tool cache | 20 | 2.0 | 3.0 | 3.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 11 | 2.0 | 2.0 | 3.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — windows-latest | windows-latest | Post Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 11 | 2.0 | 2.0 | 2.0 |
| ci | checks (bench, LSP, docs, install) — macos-latest | macos-latest | Set up job | 9 | 2.0 | 2.0 | 2.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 9 | 2.0 | 2.0 | 2.0 |
| ci | checks (bench, LSP, docs, install) — windows-latest | windows-latest | Post Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 9 | 2.0 | 2.0 | 2.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-latest | ubuntu-latest | Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 9 | 2.0 | 3.0 | 3.0 |
| site | build | ubuntu-latest | Set up job | 19 | 2.0 | 2.0 | 2.0 |
| release | build x86_64-windows | windows-latest | Post Run actions/checkout@v4 | 2 | 2.0 | 2.0 | 2.0 |
| release | publish the release | ubuntu-latest | Run actions/download-artifact@v4 | 2 | 2.0 | 3.0 | 3.0 |
| release | build x86_64-linux | ubuntu-latest | Run actions/checkout@v4 | 2 | 1.5 | 2.0 | 2.0 |
| release | build x86_64-windows | windows-latest | Stage the archive | 2 | 1.5 | 2.0 | 2.0 |
| release | build aarch64-macos | macos-latest | Run actions/upload-artifact@v4 | 2 | 1.5 | 2.0 | 2.0 |
| ci | tests (workspace) — ubuntu-latest | ubuntu-latest | Set up job | 20 | 1.0 | 2.0 | 2.0 |
| ci | tests (workspace) — ubuntu-latest | ubuntu-latest | Pinned test-runner cache | 20 | 1.0 | 2.0 | 2.0 |
| ci | cross-engine generation (interp == wasm) | ubuntu-latest | Set up job | 20 | 1.0 | 2.0 | 2.0 |
| ci | benchmarks (regression gate) | ubuntu-latest | Set up job | 3 | 1.0 | 2.0 | 2.0 |
| ci | benchmarks (regression gate) | ubuntu-latest | Upload bench reports + the seedable baseline | 3 | 1.0 | 1.0 | 1.0 |
| ci | benchmarks (regression gate) | ubuntu-latest | Post Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 3 | 1.0 | 1.0 | 1.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — macos-latest | macos-latest | Browser runtime tests (web/) and the extension's own | 11 | 1.0 | 1.0 | 1.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — macos-latest | macos-latest | Post Rust cache | 11 | 1.0 | 1.0 | 13.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — macos-latest | macos-latest | Complete job | 11 | 1.0 | 2.0 | 2.0 |
| ci | tests (workspace) — ubuntu-24.04-arm | ubuntu-24.04-arm | Set up job | 20 | 1.0 | 2.0 | 2.0 |
| ci | tests (workspace) — ubuntu-24.04-arm | ubuntu-24.04-arm | Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 20 | 1.0 | 2.0 | 2.0 |
| ci | tests (workspace) — ubuntu-24.04-arm | ubuntu-24.04-arm | Pinned test-runner cache | 20 | 1.0 | 2.0 | 2.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-latest | ubuntu-latest | Set up job | 11 | 1.0 | 2.0 | 2.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-latest | ubuntu-latest | Browser runtime tests (web/) and the extension's own | 11 | 1.0 | 3.0 | 4.0 |
| ci | tests (workspace) — macos-latest | macos-latest | Formatting gate | 20 | 1.0 | 2.0 | 2.0 |
| ci | tests (workspace) — macos-latest | macos-latest | Post Pinned test-runner cache | 20 | 1.0 | 1.0 | 1.0 |
| ci | tests (workspace) — macos-latest | macos-latest | Post Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 20 | 1.0 | 1.0 | 1.0 |
| ci | tests (workspace) — macos-latest | macos-latest | Complete job | 20 | 1.0 | 1.0 | 2.0 |
| ci | tests (workspace) — windows-latest | windows-latest | Set up job | 20 | 1.0 | 2.0 | 2.0 |
| ci | tests (workspace) — windows-latest | windows-latest | Pinned test-runner cache | 20 | 1.0 | 2.0 | 4.0 |
| ci | three-way parity (interp == native == wasm) | ubuntu-latest | Set up job | 20 | 1.0 | 1.0 | 2.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Set up job | 11 | 1.0 | 1.0 | 2.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Browser runtime tests (web/) and the extension's own | 11 | 1.0 | 3.0 | 4.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — windows-latest | windows-latest | Set up job | 11 | 1.0 | 2.0 | 2.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — windows-latest | windows-latest | Post Rust cache | 11 | 1.0 | 1.0 | 15.0 |
| ci | checks (bench, LSP, docs, install) — macos-latest | macos-latest | Browser runtime tests (web/) and the extension's own | 9 | 1.0 | 2.0 | 2.0 |
| ci | checks (bench, LSP, docs, install) — macos-latest | macos-latest | Complete job | 9 | 1.0 | 2.0 | 2.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Set up job | 9 | 1.0 | 2.0 | 2.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Browser runtime tests (web/) and the extension's own | 9 | 1.0 | 4.0 | 4.0 |
| ci | checks (bench, LSP, docs, install) — windows-latest | windows-latest | Set up job | 9 | 1.0 | 1.0 | 1.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-latest | ubuntu-latest | Set up job | 9 | 1.0 | 2.0 | 2.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-latest | ubuntu-latest | Browser runtime tests (web/) and the extension's own | 9 | 1.0 | 3.0 | 3.0 |
| site | deploy | ubuntu-latest | Set up job | 2 | 1.0 | 1.0 | 1.0 |
| release | CI passed for this commit | ubuntu-latest | The tagged commit must have a green CI run | 1 | 1.0 | 1.0 | 1.0 |
| release | build aarch64-linux | ubuntu-24.04-arm | Stage the archive | 1 | 1.0 | 1.0 | 1.0 |
| release | build aarch64-linux | ubuntu-24.04-arm | Run actions/upload-artifact@v4 | 1 | 1.0 | 1.0 | 1.0 |
| release | build x86_64-linux | ubuntu-latest | Set up job | 2 | 1.0 | 1.0 | 1.0 |
| release | build x86_64-linux | ubuntu-latest | Run actions/upload-artifact@v4 | 2 | 1.0 | 1.0 | 1.0 |
| release | build x86_64-windows | windows-latest | Set up job | 2 | 1.0 | 1.0 | 1.0 |
| release | build x86_64-windows | windows-latest | Run actions/upload-artifact@v4 | 2 | 1.0 | 2.0 | 2.0 |
| release | build aarch64-macos | macos-latest | Set up job | 2 | 1.0 | 1.0 | 1.0 |
| release | build aarch64-macos | macos-latest | Run the artifact before anyone else has to | 1 | 1.0 | 1.0 | 1.0 |
| release | publish the release | ubuntu-latest | Set up job | 2 | 1.0 | 1.0 | 1.0 |
| release | publish the release | ubuntu-latest | Run actions/checkout@v4 | 2 | 1.0 | 1.0 | 1.0 |
| release | build x86_64-linux | ubuntu-latest | Stage the archive | 2 | 0.5 | 1.0 | 1.0 |
| release | build aarch64-macos | macos-latest | Post Run actions/checkout@v4 | 2 | 0.5 | 1.0 | 1.0 |
| release | build aarch64-macos | macos-latest | Complete job | 2 | 0.5 | 1.0 | 1.0 |
| ci | tests (workspace) — ubuntu-latest | ubuntu-latest | Post Pinned test-runner cache | 20 | 0.0 | 1.0 | 1.0 |
| ci | tests (workspace) — ubuntu-latest | ubuntu-latest | Post Rust cache | 20 | 0.0 | 0.0 | 1.0 |
| ci | tests (workspace) — ubuntu-latest | ubuntu-latest | Post Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 20 | 0.0 | 1.0 | 1.0 |
| ci | tests (workspace) — ubuntu-latest | ubuntu-latest | Complete job | 20 | 0.0 | 0.0 | 1.0 |
| ci | cross-engine generation (interp == wasm) | ubuntu-latest | Post Rust cache | 20 | 0.0 | 0.0 | 1.0 |
| ci | cross-engine generation (interp == wasm) | ubuntu-latest | Post Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 20 | 0.0 | 1.0 | 1.0 |
| ci | cross-engine generation (interp == wasm) | ubuntu-latest | Complete job | 20 | 0.0 | 0.0 | 0.0 |
| ci | benchmarks (regression gate) | ubuntu-latest | Assemble a seedable baseline | 3 | 0.0 | 0.0 | 0.0 |
| ci | benchmarks (regression gate) | ubuntu-latest | Post Rust cache | 3 | 0.0 | 0.0 | 0.0 |
| ci | benchmarks (regression gate) | ubuntu-latest | Complete job | 3 | 0.0 | 0.0 | 0.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — macos-latest | macos-latest | Docs drift gate (RFC-0065) | 11 | 0.0 | 0.0 | 1.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — macos-latest | macos-latest | The install script installs, and refuses what it cannot verify | 22 | 0.0 | 36.0 | 36.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — macos-latest | macos-latest | Post Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 11 | 0.0 | 1.0 | 1.0 |
| ci | tests (workspace) — ubuntu-24.04-arm | ubuntu-24.04-arm | Post Pinned test-runner cache | 20 | 0.0 | 1.0 | 1.0 |
| ci | tests (workspace) — ubuntu-24.04-arm | ubuntu-24.04-arm | Post Rust cache | 20 | 0.0 | 1.0 | 1.0 |
| ci | tests (workspace) — ubuntu-24.04-arm | ubuntu-24.04-arm | Post Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 20 | 0.0 | 1.0 | 1.0 |
| ci | tests (workspace) — ubuntu-24.04-arm | ubuntu-24.04-arm | Complete job | 20 | 0.0 | 0.0 | 1.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-latest | ubuntu-latest | Docs drift gate (RFC-0065) | 11 | 0.0 | 0.0 | 1.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-latest | ubuntu-latest | The install script installs, and refuses what it cannot verify | 22 | 0.0 | 1.0 | 1.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-latest | ubuntu-latest | Post Rust cache | 11 | 0.0 | 1.0 | 6.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-latest | ubuntu-latest | Post Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 11 | 0.0 | 1.0 | 1.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-latest | ubuntu-latest | Complete job | 11 | 0.0 | 0.0 | 0.0 |
| ci | tests (workspace) — macos-latest | macos-latest | Post Rust cache | 20 | 0.0 | 0.0 | 1.0 |
| ci | tests (workspace) — windows-latest | windows-latest | Post Pinned test-runner cache | 20 | 0.0 | 1.0 | 1.0 |
| ci | tests (workspace) — windows-latest | windows-latest | Post Rust cache | 20 | 0.0 | 1.0 | 1.0 |
| ci | tests (workspace) — windows-latest | windows-latest | Complete job | 20 | 0.0 | 0.0 | 1.0 |
| ci | three-way parity (interp == native == wasm) | ubuntu-latest | Populate ~/.vyrn/tools from vyrn.lock | 20 | 0.0 | 0.0 | 0.0 |
| ci | three-way parity (interp == native == wasm) | ubuntu-latest | Post Pinned tool cache | 20 | 0.0 | 0.0 | 1.0 |
| ci | three-way parity (interp == native == wasm) | ubuntu-latest | Post Rust cache | 20 | 0.0 | 0.0 | 0.0 |
| ci | three-way parity (interp == native == wasm) | ubuntu-latest | Post Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 20 | 0.0 | 1.0 | 1.0 |
| ci | three-way parity (interp == native == wasm) | ubuntu-latest | Complete job | 20 | 0.0 | 0.0 | 1.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Docs drift gate (RFC-0065) | 11 | 0.0 | 1.0 | 1.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | The install script installs, and refuses what it cannot verify | 22 | 0.0 | 0.0 | 1.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Post Rust cache | 11 | 0.0 | 1.0 | 7.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Post Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 11 | 0.0 | 1.0 | 1.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Complete job | 11 | 0.0 | 0.0 | 0.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — windows-latest | windows-latest | Docs drift gate (RFC-0065) | 11 | 0.0 | 0.0 | 1.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — windows-latest | windows-latest | The install script installs, and refuses what it cannot verify | 22 | 0.0 | 12.0 | 18.0 |
| ci | checks (bench, LSP, genwasm, docs, install) — windows-latest | windows-latest | Complete job | 11 | 0.0 | 0.0 | 0.0 |
| ci | checks (bench, LSP, docs, install) — macos-latest | macos-latest | Docs drift gate (RFC-0065) | 9 | 0.0 | 1.0 | 1.0 |
| ci | checks (bench, LSP, docs, install) — macos-latest | macos-latest | Post Rust cache | 9 | 0.0 | 1.0 | 1.0 |
| ci | checks (bench, LSP, docs, install) — macos-latest | macos-latest | Post Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 9 | 0.0 | 1.0 | 1.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Docs drift gate (RFC-0065) | 9 | 0.0 | 1.0 | 1.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | The install script installs, and refuses what it cannot verify | 18 | 0.0 | 0.0 | 0.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Post Rust cache | 9 | 0.0 | 0.0 | 0.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Post Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 9 | 0.0 | 0.0 | 0.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-24.04-arm | ubuntu-24.04-arm | Complete job | 9 | 0.0 | 1.0 | 1.0 |
| ci | checks (bench, LSP, docs, install) — windows-latest | windows-latest | Docs drift gate (RFC-0065) | 9 | 0.0 | 0.0 | 0.0 |
| ci | checks (bench, LSP, docs, install) — windows-latest | windows-latest | Post Rust cache | 9 | 0.0 | 0.0 | 0.0 |
| ci | checks (bench, LSP, docs, install) — windows-latest | windows-latest | Complete job | 9 | 0.0 | 1.0 | 1.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-latest | ubuntu-latest | Docs drift gate (RFC-0065) | 9 | 0.0 | 1.0 | 1.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-latest | ubuntu-latest | The install script installs, and refuses what it cannot verify | 18 | 0.0 | 1.0 | 1.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-latest | ubuntu-latest | Post Rust cache | 9 | 0.0 | 0.0 | 0.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-latest | ubuntu-latest | Post Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 9 | 0.0 | 1.0 | 1.0 |
| ci | checks (bench, LSP, docs, install) — ubuntu-latest | ubuntu-latest | Complete job | 9 | 0.0 | 1.0 | 1.0 |
| site | build | ubuntu-latest | The repository's own history | 19 | 0.0 | 1.0 | 1.0 |
| site | build | ubuntu-latest | Refresh the baked release | 19 | 0.0 | 2.0 | 2.0 |
| site | build | ubuntu-latest | The output directories | 19 | 0.0 | 0.0 | 1.0 |
| site | build | ubuntu-latest | The guide's programs | 19 | 0.0 | 1.0 | 1.0 |
| site | build | ubuntu-latest | Formatting | 19 | 0.0 | 0.0 | 0.0 |
| site | build | ubuntu-latest | Run actions/upload-pages-artifact@56afc609e74202658d3ffba0e8f6dda462b719fa | 19 | 0.0 | 1.0 | 2.0 |
| site | build | ubuntu-latest | Post Rust cache | 19 | 0.0 | 1.0 | 1.0 |
| site | build | ubuntu-latest | Post Run actions/checkout@11d5960a326750d5838078e36cf38b85af677262 | 19 | 0.0 | 1.0 | 1.0 |
| site | build | ubuntu-latest | Complete job | 19 | 0.0 | 0.0 | 0.0 |
| site | deploy | ubuntu-latest | Complete job | 2 | 0.0 | 0.0 | 0.0 |
| release | CI passed for this commit | ubuntu-latest | Set up job | 1 | 0.0 | 0.0 | 0.0 |
| release | CI passed for this commit | ubuntu-latest | Complete job | 1 | 0.0 | 0.0 | 0.0 |
| release | build aarch64-linux | ubuntu-24.04-arm | Set up job | 1 | 0.0 | 0.0 | 0.0 |
| release | build aarch64-linux | ubuntu-24.04-arm | The tag and the crate version must agree | 1 | 0.0 | 0.0 | 0.0 |
| release | build aarch64-linux | ubuntu-24.04-arm | Run the artifact before anyone else has to | 1 | 0.0 | 0.0 | 0.0 |
| release | build aarch64-linux | ubuntu-24.04-arm | Post Run actions/checkout@v4 | 1 | 0.0 | 0.0 | 0.0 |
| release | build aarch64-linux | ubuntu-24.04-arm | Complete job | 1 | 0.0 | 0.0 | 0.0 |
| release | build x86_64-linux | ubuntu-latest | The tag and the crate version must agree | 1 | 0.0 | 0.0 | 0.0 |
| release | build x86_64-linux | ubuntu-latest | Run the artifact before anyone else has to | 1 | 0.0 | 0.0 | 0.0 |
| release | build x86_64-linux | ubuntu-latest | Post Run actions/checkout@v4 | 2 | 0.0 | 0.0 | 0.0 |
| release | build x86_64-linux | ubuntu-latest | Complete job | 2 | 0.0 | 0.0 | 0.0 |
| release | build x86_64-windows | windows-latest | The tag and the crate version must agree | 1 | 0.0 | 0.0 | 0.0 |
| release | build x86_64-windows | windows-latest | Complete job | 2 | 0.0 | 0.0 | 0.0 |
| release | build aarch64-macos | macos-latest | The tag and the crate version must agree | 1 | 0.0 | 0.0 | 0.0 |
| release | build aarch64-macos | macos-latest | Stage the archive | 2 | 0.0 | 0.0 | 0.0 |
| release | publish the release | ubuntu-latest | Checksums | 2 | 0.0 | 0.0 | 0.0 |
| release | publish the release | ubuntu-latest | Post Run actions/checkout@v4 | 2 | 0.0 | 0.0 | 0.0 |
| release | publish the release | ubuntu-latest | Complete job | 2 | 0.0 | 0.0 | 0.0 |


## Part two — what each expensive step is for

Every step whose median exceeds 30 seconds. Medians come from part one. The
"what it proves" column quotes or summarises the comment in the workflow file
that justifies the step; the citation points at that comment.

| step (job, os) | median s | what it proves | what would go unnoticed if removed | cached today | cache key |
|---|---|---|---|---|---|
| The site's own tests (`build`, ubuntu) | 1018 | Every route answers as a document and as data, every navigation row points at an existing route, and each site module runs the test blocks declared in its source; the floor is read off the source so a block that stops being discovered fails (`.github/workflows/site.yml:179-207`) | Broken links, missing routes and silent test loss would ship to Pages | No. Its input is the working tree; nothing to cache | none |
| Run the parity harness (`three-way parity`, ubuntu) | 465 | Interpreter == native == wasm, byte for byte, over 40 programs (`.github/workflows/ci.yml:562-568`) | A backend divergence would reach main | Yes. `Swatinem/rust-cache`, `prefix-key: parity`, `workspaces: compiler` (`.github/workflows/ci.yml:518-525`) | `v0-rust-parity-Linux-x64-<hash>-<hash>` |
| Workspace tests (`tests (workspace)`, windows / ubuntu / macos / arm) | 279 / 208 / 155 / 140 | The 1753-test workspace suite in debug on every shipped platform (`.github/workflows/ci.yml:294-312`, matrix rationale `.github/workflows/ci.yml:192-203`) | A platform-specific regression would reach a published binary untested | Yes. `Swatinem/rust-cache`, default key, `workspaces: compiler` (`.github/workflows/ci.yml:215-227`) | `v0-rust-test-<OS>-<arch>-<hash>-<hash>` |
| LSP tests (excluded crate) (`checks`, windows) | 134 | The `vyrn-lsp` crate's own suite; the crate is excluded from the workspace, so no other step compiles it (`.github/workflows/ci.yml:426-433`) | LSP regressions would reach editors unseen | Yes. `Swatinem/rust-cache`, `prefix-key: checks`, three workspaces (`.github/workflows/ci.yml:347-358`) | `v0-rust-checks-<OS>-<arch>-<hash>-<hash>` |
| Run benches + compare to baseline (`benchmarks`, ubuntu) | 125 | Each bench still builds and runs; `--compare` against `bench/baseline.json` is the regression tripwire (`.github/workflows/ci.yml:660-675`, loop at `.github/workflows/ci.yml:691-713`). The baseline is still a placeholder, so today it proves only "still runs" (`.github/workflows/ci.yml:663-672`) | A bench that stopped running would pass unnoticed; once seeded, a perf regression would | Yes. `Swatinem/rust-cache`, `prefix-key: bench` (`.github/workflows/ci.yml:679-685`) | `bench-bench-Linux-x64-<hash>-<hash>` |
| Render every page (`build`, ubuntu) | 91 | Produces the artifact Pages publishes: one document and one data payload per route (`.github/workflows/site.yml:264-269`) | The deploy would publish nothing | No. Output depends on the whole tree | none |
| Build vyrn (release) (`checks`, windows) | 87 | A release-built interpreter, because `bench --check` runs every bench body under it and a debug interpreter is 4-7x slower (`.github/workflows/ci.yml:359-382`) | The bench step would slow by the measured 2.4x-or-worse factor | Yes. Same `checks` slot as above | `v0-rust-checks-...` |
| Build the CLI (`build`, ubuntu, site.yml) | 48 | The binary every later site step executes (`.github/workflows/site.yml:113-115`) | Nothing downstream would run | Yes. `Swatinem/rust-cache`, `prefix-key: site`, `compiler` + `compiler/vyrn-play` (`.github/workflows/site.yml:90-111`) | `site-site-Linux-x64-<hash>-<hash>` |
| The playground's own checks (`build`, ubuntu) | 41 | Span offsets, diagnostics shape and exit codes of `vyrn-play` on the host (`.github/workflows/site.yml:258-262`) | A broken playground diagnostic would ship inside `play.wasm`'s sibling build | Yes. Same `site` slot (`workspaces` lists `compiler/vyrn-play`) | `site-site-...` |
| Every generator agrees under both engines (`cross-engine generation`, ubuntu) | 61 | RFC-0076 acceptance: the wasm column matches the interpreter column byte for byte over every generator example, and the interpreter works with the wasm path disabled (`.github/workflows/ci.yml:630-642`) | A generator emitting divergent wasm would pass | Yes. `Swatinem/rust-cache`, `prefix-key: genwasm` (`.github/workflows/ci.yml:614-621`) | `genwasm-gen-engine-Linux-x64-<hash>-<hash>` |
| Bench --check (`checks`, windows / ubuntu / arm / macos) | 59 / 43 / 35 / 29 | The blocking half of bench-in-CI: each bench body still compiles and runs, deterministically, once (RFC-0063) (`.github/workflows/ci.yml:383-396`) | A bench that stopped compiling or running would fail only later, in the push-only bench job | Yes. Binary comes from the release build in the same job, covered by the `checks` slot | `v0-rust-checks-...` |
| Build the CLI (`build <platform>`, release.yml) | 86 / 39 / 38 / 33 | The four shipped binaries, built `--locked` per target, plus `vyrn-lsp` (`.github/workflows/release.yml:115-133`) | A release would ship nothing | **No. release.yml contains no `actions/cache` and no `Swatinem/rust-cache`.** | none |

## Part three — the cheap wins

One classification per expensive step.

**CACHEABLE**

- `Build the CLI` (release.yml, all four legs). No cache exists in the
  workflow at all. Correct key: `Swatinem/rust-cache@v2` with
  `workspaces: compiler` and `compiler/vyrn-lsp` (both are built,
  `--locked`, at `.github/workflows/release.yml:130-133`), one slot per
  target. Estimated saving: NOT MEASURED, and the measurement that exists
  says it is small — the cold release build in release.yml (39 s ubuntu
  median) is already close to the warm-cache release build in ci.yml's
  checks job (46 s ubuntu median, part one). Releases are also rare: 2
  completed runs in the sampled window. Low value, near-zero risk.

**PARALLELISABLE**

- `Run benches + compare to baseline` (`benchmarks`, 125 s median, 3
  samples). Serial `for` loop over the corpus at
  `.github/workflows/ci.yml:703-712`; thirteen independent processes
  (count per `.github/workflows/ci.yml:26-28`). Wall time at four-way:
  about 40 s (four waves of the largest benches). At eight-way: about
  20 s. Arithmetic: 125 s x (waves / 13), waves = ceil(13/P).
  Caveat that decides it: this loop also MEASURES (`--json` reports feed
  the seedable baseline, `.github/workflows/ci.yml:714-732`), and parallel
  execution on shared runners pollutes timing. `BENCH_THRESHOLD`
  documentation at `.github/workflows/ci.yml:162-175` already treats fleet
  spread as the enemy. This job also never extends CI wall time: it runs
  beside the other jobs and its median total is 160 s against a 514 s
  parity pole. Recommendation below is therefore NOT to do it.

Everything else measured over 30 s is one of:

**IRREDUCIBLE** — `The site's own tests` (interpreter-bound; the workflow's
own measurement attributes 687 s of the step to `vyrn test site/export.vyrn`,
32 blocks, `.github/workflows/site.yml:98-104`; the module loop around it is
already 20 s serial after memoisation, `.github/workflows/site.yml:196-204`),
`Run the parity harness` (one `#[test]` walking 40 programs;
`.github/workflows/ci.yml:102-105` names the harness itself as the only
remaining lever), `Workspace tests` (nextest already runs tests in parallel;
`.github/workflows/ci.yml:294-312`), `LSP tests` (compiling the excluded
crate is the cost; `.github/workflows/ci.yml:430-432`), `Build vyrn
(release)` and `Bench --check` (the release-binary lever was already taken;
`.github/workflows/ci.yml:83-96` records the 10-15x win), `Every generator
agrees` (cranelift compiles the guest; release chosen deliberately,
`.github/workflows/ci.yml:636-639`), `Render every page` (same program as
the tests, producing different output).

**MISPLACED** — none found. The two candidate misplacements were checked and
rejected by measurement recorded in the files themselves: moving the node
steps between platforms saves nothing visible (`.github/workflows/ci.yml:31-35`)
and a third CI job does not move the pole (`.github/workflows/ci.yml:323-327`).

**REDUNDANT** — none found. `Render every page` looks like a repeat of
`The site's own tests` (same program), but one asserts and one produces the
published bytes; removing either loses something the other does not cover.

## Part four — the arithmetic the owner asked for

Target: every job under one minute. Job medians are from the same sample as
part one. "Sum of IRREDUCIBLE" adds the medians of the steps classified
IRREDUCIBLE above, plus checkout/setup overhead where it is measured.

| job | median total | sum of IRREDUCIBLE | is one minute reachable | what would have to go |
|---|---|---|---|---|
| Site `build` (ubuntu) | 1238 s (n=19, range 536-1837) | about 1219 s | **NO** | Nothing. `The site's own tests` ALONE is 1018 s median, 17x the target, and it is the interpreter executing the site's own test blocks. One minute requires making that suite about 17x faster in the compiler. No workflow edit reaches it. |
| CI `three-way parity` (ubuntu) | 513.5 s | about 503 s | **NO** | Nothing. The harness step alone is 465 s median, and `.github/workflows/ci.yml:102-105` already states the remaining lever is the harness, not the workflow. |
| CI `tests (workspace)` windows / ubuntu / macos / arm | 336 / 238 / 189 / 168 s | about 320 / 222 / 173 / 152 s | **NO** on every platform | Nothing. The fastest leg (arm, 168 s) spends 140 s inside `Workspace tests` alone. |
| CI `checks` windows / macos / ubuntu / arm (current shape) | 350 / 211 / 190 / 161 s | about 304 / 218 / 200 / 141 s | **NO** | Nothing. Windows carries LSP 134 + bench 59 + release build 87 + genwasm 24. |
| CI `cross-engine generation` (ubuntu) | 72 s | 61 s (the test step) | **ONLY IF** the genwasm test itself drops below about 45 s | The test step is one `cargo test` invocation whose cost is cranelift compiling the guest corpus. No workflow lever exists; the 60.5 s median is already above the target on its own. |
| CI `benchmarks` (push-to-main only) | 160 s (n=3) | about 145 s | **NO** as measured — and irrelevant to wall clock: the job runs beside the others, so cutting it changes no run's finish time. | The timed corpus loop; parallelising it trades measurement quality for seconds nobody waits on (part three). |
| Release `build x86_64-windows` | 101 s | about 89 s | **ONLY IF** the double release build gets a warm cache or shrinks by about 41 s | `Build the CLI` is 86 s of it and is the only large uncached step in the three workflows. |
| Release `build` linux / macos / arm | 45.5 / 46.5 / 40 s | about 33-39 s | Effectively already there (medians under 60 s, n=2 and n=1) | Nothing. |
| Release `gate` / `publish`, Site `deploy` | 4 / 11.5 / 8.5 s | under 12 s | **YES** (already under) | Nothing. |

Blunt summary: the CI workflow's median wall time is the parity job at 514 s,
and the Site workflow's is the build job at 1238 s. Both floors are set by
the Vyrn interpreter executing test corpora — 40 parity programs in one case,
the site's export and module blocks in the other. One minute per job is not
reachable by editing these three files.

## Part five — the caches that already exist

Hit rates read from the logs of the sampled runs
(`gh run view <id> --log`, patterns "Cache hit for:", "No cache found.",
"Cache Size:"). Sizes are the median restore size over the sampled restores.

| cache | key | what it holds | hit rate over sampled runs | size |
|---|---|---|---|---|
| Swatinem/rust-cache, CI tests job x4 os | `v0-rust-test-<OS>-<arch>-<hash>-<hash>`, `workspaces: compiler` | debug target tree for the workspace suite | 20/20 runs | 27 MB |
| actions/cache, CI tests job x4 os | `vyrn-nextest-v2-${{ matrix.os }}-${{ hashFiles('vyrn.lock') }}`, path `~/.vyrn/tools` | pinned cargo-nextest binary | 20/20 runs | 10 MB |
| Swatinem/rust-cache, CI checks job x4 os | `v0-rust-checks-<OS>-<arch>-<hash>-<hash>`, `prefix-key: checks`, 3 workspaces | release vyrn-cli + LSP + genwasm target trees | 18/20 runs (see below) | 381 MB |
| Swatinem/rust-cache, CI parity job | `v0-rust-parity-Linux-x64-...`, `prefix-key: parity` | debug vyrn-cli target tree for the parity test | 20/20 runs | 28 MB |
| actions/cache, CI parity job | `vyrn-tools-v2-${{ hashFiles('vyrn.lock') }}`, path `~/.vyrn/tools` | wasmtime + wasi sysroot + builtins archive | 20/20 runs | 85 MB |
| Swatinem/rust-cache, CI gen-engine job | `genwasm-gen-engine-Linux-x64-...`, `prefix-key: genwasm` | vyrn-genwasm + wasmtime-crates target tree | 20/20 runs | 193 MB |
| Swatinem/rust-cache, CI bench job | `bench-bench-Linux-x64-...`, `prefix-key: bench` | debug vyrn-cli target tree | 3/3 runs (job ran in 3 of 20 runs; push-only trigger, `.github/workflows/ci.yml:650`) | 28 MB |
| Swatinem/rust-cache, Site build job | `site-site-Linux-x64-...`, `prefix-key: site`, `compiler` + `compiler/vyrn-play` | release vyrn-cli + vyrn-play trees | 18/18 runs with retrievable logs; 2 of 20 sampled runs had no retrievable log | 36 MB |

No cache in either workflow misses more than a quarter of the time. The one
flag worth recording, without crossing the threshold: the `checks` slot
missed on all four platforms in two consecutive runs on 2026-08-22
(runs 32590221751, 32593097307). The measured cost of those misses lands
almost entirely on `LSP tests (excluded crate)` — windows 377 s and 440 s
against a 134 s median — and pushes the affected job totals to 658 s and
757 s against a 350 s median. Those two runs ARE the p90/max columns for
that step and job in part one. `.github/workflows/ci.yml:330-335` predicts
exactly this failure mode (repository cache cap, 11 Rust cache slots) and
names the first remedy (merge the slot via rust-cache's `shared-key`). Two
events in twenty runs is not a trend; the number to watch is whether it
recurs.

Total restore footprint across the eleven slots observed in one green run:
about 2.1 GB of the 10 GB repository cache cap.

## The ten changes with the best seconds per unit of risk

`RECOMMENDATION, NOT A DECISION`

The measurement supports fewer than ten changes with a real lever. Ranked;
each row carries its number and its risk. Where no lever exists the honest
entry is absent, not padded.

1. **Decide the fate of the Site test suite's runtime in the compiler, not
   in CI.** 1018 s median, 17x the one-minute target, 85% of the Site job.
   The workflow already did its part: inputs before the long step
   (`.github/workflows/site.yml:163-177`), memoised modules
   (`.github/workflows/site.yml:196-204`). What remains is interpreter
   speed on `site/export.vyrn`'s 32 test blocks. Seconds available: up to
   about 950 per Site run. Risk: a compiler project, not an edit; the
   decision is the owner's.
2. **Watch the `checks` cache slot; act only if the misses recur.**
   0 s median cost, but each miss event costs 300-400 s on four jobs and
   produced every p90 outlier in part one. Remedy if it recurs, already
   written down at `.github/workflows/ci.yml:332-335`: merge the slot with
   the test job's via `shared-key`. Risk: near zero to watch; low to merge.
3. **Leave the bench job's timed loop serial.** Parallelising it saves
   about 85-105 s of runner time (part three arithmetic) and zero wall
   time, while polluting the measurements the loop exists to take. The best
   change here is the decision not to make one. Risk of changing: real.
4. **Add rust-cache to release.yml's build legs if releases become
   frequent.** Two runs in the window; measured headroom is small (cold
   39 s ubuntu versus warm-cache 46 s ubuntu elsewhere — the difference is
   inside run-to-run noise). Seconds: uncertain, bounded above by about
   40 s per windows leg. Risk: near zero. Value today: near zero.
5. **If the parity floor must move, it moves in the harness.** 465 s in one
   `#[test]` walking 40 programs. Sharding the walk across processes or
   threads is a harness change with a real payoff (roughly 350 s at
   four-way if the programs are independent) and an unmeasured assumption
   (independence, shared state). Measure before touching. Risk: medium.

No sixth change has a measured lever. Every remaining step over 30 s is
either already parallel (`Workspace tests` under nextest, `Bench --check`
under `xargs -P 4`), already cached with a hit rate of at least 18/20, or
bounded below by a compile the workflows cannot shrink.

## Method

Commands run, from this machine, against `vyrn-lang/vyrn`:

```
gh run list --workflow ci.yml --status completed --limit 20 --json databaseId,headSha,conclusion,createdAt
gh run list --workflow site.yml ...    # same shape
gh run list --workflow release.yml ... # same shape (2 completed runs exist)
gh api repos/vyrn-lang/vyrn/actions/runs/<id>/jobs --paginate   # per run
gh run view <id> --log                                            # cache lines
```

Sample: the last 20 completed runs of ci.yml (2026-08-22 to 2026-08-23) and
site.yml (2026-08-21 to 2026-08-23), and all 2 completed runs of release.yml
(2026-08-11, 2026-08-18). Durations are `completed_at - started_at` of each
step as reported by the jobs endpoint. Percentiles are over the sampled
runs of each (workflow, job, os, step) cell; p90 is the ceiling index.
`gh` returned full 40-character shas; nothing was filtered on a short sha.

Two shapes of the CI `checks` job appear in the window: the older
`checks (bench, LSP, docs, install)` (9 runs) and the current
`checks (bench, LSP, genwasm, docs, install)` (11 runs, adds the genwasm
test step). Both are reported. The `benchmarks` job triggers on push to
main only (`.github/workflows/ci.yml:650`) and appears in 3 of 20 runs.

One site.yml run (32608657677) has no server-side log left ("log not
found"), which is why site step counts are 19 and its cache row says 18/18.

No watcher was armed and no commit was pushed; every number here is read
from finished runs.

---

## Correction, made on verification

The medians in this census are stale. The 19 sampled Site runs mostly predate
the Site optimisation that merged in PR #267 on 2026-08-23, so the census costed
a workflow that no longer exists.

Verified against the newest completed Site run at `82234d6a`:

| what | this census says | actual, newest run |
| --- | --- | --- |
| Site `build` total | 1238 s median (n=19, range 536-1837) | **about 540 s** — the last five runs are 9, 10, 10, 12 and 10 minutes |
| `The site's own tests` | 1018 s median | **329 s** |
| `Render every page` | — | 67 s |
| `Build the CLI` | — | 49 s |
| `The playground's own checks` | — | 42 s |
| `The playground module` | — | 24 s |

Commands:

```
gh run list --workflow site.yml --status completed --limit 6 --json headSha,conclusion,createdAt,updatedAt
gh api repos/vyrn-lang/vyrn/actions/runs/<id>/jobs
```

**The verdict does not change.** One minute is still out of reach for the Site
build: 540 s is nine times the target, and `The site's own tests` at 329 s
exceeds it more than five times on its own. The census reaches the right answer
by the wrong arithmetic.

What does change is the size of the remaining prize. This census says the site
tests are 1018 s and therefore the only thing worth attacking. They are 329 s,
which is 61 per cent of the build rather than 82 per cent. `Render every page`,
`Build the CLI` and the two playground steps together are another 182 s, and
this census did not cost them at all.

Re-read part three against these numbers before acting on any ranking in it.

---

## The site export, measured from inside (added after the fact)

The census timed CI steps from the outside. `vyrn run site/export.vyrn out` is
one of them, and this is where its time goes. Measured on the same machine,
best of three, after the interpreter changes in
`rfcs/census/interpreter-value-copies.md` took the whole step from 62.03 s to
45.47 s.

| phase | seconds | printed? |
| --- | --- | --- |
| start-up before the first route line | 10.96 | no |
| the 80 route pages | about 27 | yes, one line each |
| the 57 markdown twins | 7.13 | **no** |
| the search index | about 1.0 | one line, at the end |
| the feed | under 0.1 | yes |

Two things worth saying.

**The markdown twins are 7.1 seconds and print nothing.** A reader of the log
sees the last route, then a ten-second pause, then `search.json`, and would
reasonably conclude the search index costs ten seconds. It costs one. The
slowest twin is `/docs/std/vyx` at 714 ms and there are 57 of them.

**`writeIndex` builds the index twice.** `site/export.vyrn:815` calls
`indexJson()`, which calls `rows()`, and `:821` calls `rows()` again to print
the count. `rows()` measured 740 ms. Passing the array it already has costs one
line and saves that.

Neither is edited here: `site/` is being changed on two other branches and this
would collide. Both are small and both are measured.

Phase timings came from a scratch module using `monotonic()` from `std/time`,
deleted after measuring. The start-up figure is wall time to the first printed
line, which is the program being compiled — generators included.
