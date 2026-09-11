# Review round two: the CLI, the tools and the site

Twenty-nine findings of the 2026-08-29 review fall outside the frontend and the
codegen crates. Twenty-seven were already fixed. Two were live on `main` and are
fixed here.

One commit closed twenty-five of them: `907354be`, "round two of the audit: 95
findings investigated, 94 fixed and one deferred with its reason in the code".
Its body names most of these by their symptom. Two more closed on their own
after it: `site/app/markdown.vyrn` went with the backstage in `79752db5`, and
`site/app/editors.vyrn` records its own fix in a comment.

The two live findings share a shape. Both are state that outlives the run it
belongs to: a kill timer held per mount, and one latch serving two facts. The
audit commit never touched `site/public/play.js` or `site/public/widgets.js`, so
the playground claim in its body covers neither file.

Verdicts: 29 fixed, 0 not a defect, 0 not reached. Of the 29, two were fixed on
this branch.

| id | severity | verdict | evidence | commit |
|---|---|---|---|---|
| F2-009 | high | fixed | `update` refuses an unpinned spec under `--locked` before the resolver sees it, with `run \`vyrn update <name>\` once online to pin it` | `907354be` |
| F2-061 | high | fixed | `quote()` single-quotes on POSIX and backtick-escapes `` ` ``, `$` and `"` on Windows | `907354be` |
| F2-067 | high | fixed | the walk collects `MAX_RENAME_FILES + 1` and the refusal is `files.len() > MAX_RENAME_FILES` | `907354be` |
| F2-068 | high | fixed | `collect_css_rules` keeps `sel_start` across a comment that follows selector text, and resets it only for a comment that stands alone | `907354be` |
| F2-074 | high | fixed | `site/app/markdown.vyrn` no longer exists; `79752db5` deleted the backstage and the 2,863 lines only it reached | `79752db5` |
| F2-075 | high | fixed | `stripCssComments` returns what it has scanned when a comment never closes, instead of pushing `keep` past the end | `907354be` |
| F2-076 | high | fixed | no `raw.githubusercontent.com` install command is left in `site/app/editors.vyrn`; the file records the fix where the command used to be | `907354be` |
| F2-010 | medium | fixed | `routes` takes the first positional anywhere after the subcommand, so `vyrn routes --json app.vyrn` names a file | `907354be` |
| F2-056 | medium | fixed | `ci.yml` runs `cargo test --manifest-path vyrn-genwasm/Cargo.toml` in the checks job | `907354be` |
| F2-059 | medium | LIVE, fixed | `stopWorker` did not clear `runTimer`; three tests in `site/test/playlifecycle.test.mjs` fail on the parent commit and pass here | `15105e9c` |
| F2-060 | medium | LIVE, fixed | `arm(true)` returned at the `armed` latch and readiness replayed `thenRun`; the third test in the same file proves it | `9c6b4c27` |
| F2-062 | medium | fixed | `fd_fdstat_get` answers `ERRNO_BADF` for every fd that is not 0, 1 or 2 | `907354be` |
| F2-063 | medium | fixed | `same_file` lowercases the whole path on Windows, which is what `norm_path_key` does | `907354be` |
| F2-064 | medium | fixed | a flat binding is pinned by `mounts_import`, not by an exact generator argument; `bare_is_ours` has six tests | `907354be` |
| F2-065 | medium | fixed | `wanted()` admits a mapped symbol on decl and file; the origin line is gone from the filter | `907354be` |
| F2-066 | medium | fixed | `mounts_import` takes `overlays` and reads an open buffer before the file | `907354be` |
| F2-071 | medium | fixed | `cross_origin_body` gates every served request on Host and on Origin when a client sends one | `907354be` |
| F2-073 | medium | fixed | `demo.vyrn` holds `corrupt`, `demoError()` names it, and `missingDemo()` refuses the build on it | `907354be` |
| F2-077 | medium | fixed | `eyebrow()` answers `Releases` with no kind when `hasRelease()` is false | `907354be` |
| F2-078 | medium | fixed | no workflow or page names `fresh.js`; `site/release.txt` mentions it once, in the past tense, as deleted | `907354be` |
| F2-012 | low | fixed | no `let _ = save_lock` is left; every call site takes the failure | `907354be` |
| F2-013 | low | fixed | `--maps` is stripped for every subcommand except `run`, whose tail belongs to the program | `907354be` |
| F2-014 | low | fixed | `write_response_vary` writes no `Content-Length` on a 204, per RFC 9110 section 8.6 | `907354be` |
| F2-015 | low | fixed | `enclosing_open_tag` jumps past `-->`, with a test on `<!-- don't -->` | `907354be` |
| F2-016 | low | fixed | `ident_at` uses `is_alphanumeric`, which is the lexer's own class | `907354be` |
| F2-057 | low | fixed | `site.yml` pins `Swatinem/rust-cache` to the same SHA every `ci.yml` job uses | `907354be` |
| F2-058 | low | fixed | `Cleanup` deletes the `Path` value when the run created it, and restores the saved kind otherwise | `907354be` |
| F2-069 | low | fixed | `externdemo.html` no longer interpolates export names; one `innerHTML` is left and it carries an exit code | `907354be` |
| F2-072 | low | fixed | `MAX_BODY` bounds the declared length before a byte is read | `907354be` |

## The two live findings

**F2-059, the playground's kill timer.** `runTimer` was a per-mount binding
cleared only by `finish`, which every completed run reaches. Every path that
abandons a live run reaches `stopWorker` instead: a second `Run`, Ctrl-Enter in
the editor, `Reset`. The first run's timeout stayed armed. Five seconds after
that run started it terminated whatever was running by then, wrote "Stopped
after 5 seconds. The program was still running." and set the status to
"Stopped" - over a second run that had already succeeded. The timer now lives
beside the worker it kills and `stopWorker` clears it. `finish`'s own
`clearTimeout` goes with it; all five callers reach `finish` through
`stopWorker`.

**F2-060, the dropped Run press.** `armHeroEditor` held one latch for two facts:
that the compiler module is loading, and that a reader is owed a run. Hover and
focus arm with no press owed. A Run press during the load window found the latch
set and returned, and readiness replayed `thenRun`, false on those two paths.
The press did nothing. `runOnReady` now records the owed press before the latch
returns, and readiness replays that.

Neither defect is reachable from node: both need a DOM, a Worker and a clock.
`site/test/playlifecycle.test.mjs` checks the rule each fix states, in the one
place it is stated. All three tests fail against the parent commit's sources.

## The gate table

| gate | result |
|---|---|
| `cargo fmt --all --check` | pass |
| `cargo fmt --manifest-path vyrn-lsp/Cargo.toml --check` | pass |
| `cargo build --release -p vyrn-cli` | pass, no warning |
| `cargo test -p vyrn-cli` | 645 passed, 0 failed, 47 ignored, over 88 targets |
| `cargo test --manifest-path vyrn-lsp/Cargo.toml` | 23 + 77 passed, 0 failed, 5 ignored |
| `node --test "web/test/*.test.mjs" "editor/vscode/test/*.test.mjs"` | 29 passed, 0 failed |
| `node --test "site/test/*.test.mjs"` | 55 passed, 4 failed |

The four site failures name `out/play.wasm`, which the playground's own build
step writes. That step is `compiler/vyrn-play`, and it did not run here. Every
test that reads `site/public/play.js` passes.

No census moved. `cli_census.rs` tiles `compiler/vyrn-cli/src/main.rs`, and this
branch does not touch it. Nothing under `site/public` carries a line count.
