# Releasing Vyrn

One workflow publishes releases:
[`.github/workflows/release.yml`](../.github/workflows/release.yml). It runs on
one event and no other — a pushed tag matching `v*`. There is no schedule, no
branch trigger and no manual dispatch, so nothing publishes by accident.

## Tag format

```
v<major>.<minor>.<patch>[-<pre>.<n>]
```

| Tag | Published as |
|-----|--------------|
| `v0.1.0-alpha.1` | pre-release |
| `v0.1.0-rc.1` | pre-release |
| `v0.1.0` | full release |

The rule in the workflow is exactly one line: a tag containing `-` gets
`--prerelease`. While Vyrn is an alpha, every tag carries a suffix.

## Cut a release

1. Land everything you want in the release on `main`. Wait for CI to pass on
   that commit — the release workflow does not run the test suite.
2. Tag the commit and push the tag:

   ```bash
   git tag v0.1.0-alpha.1
   git push origin v0.1.0-alpha.1
   ```

3. Watch the run: `gh run watch`. It builds three platforms in parallel, then
   publishes. The matrix is `fail-fast: true` on purpose — a missing platform is
   a broken install command for everyone on it, so a partial matrix must never
   reach the publish job.
4. Check the release page. The install commands in the notes must work.

Nothing else needs seeding. The install scripts read the GitHub release API at
run time, so a new release is live the moment the workflow finishes.

## What gets published

Five assets per release:

```
vyrn-x86_64-linux.tar.gz
vyrn-aarch64-linux.tar.gz
vyrn-aarch64-macos.tar.gz
vyrn-x86_64-windows.zip
SHA256SUMS
```

`aarch64-linux` is not a niche: Docker on an Apple Silicon Mac defaults to
`linux/arm64`, so without it `install.sh` had nothing to offer inside an
ordinary container.

The names carry no version. That keeps them predictable for a script and lets
`https://github.com/vyrn-lang/vyrn/releases/latest/download/<name>` work once a
full release exists.

Each archive holds one directory, named after the archive:

```
vyrn-x86_64-linux/
├── vyrn          the driver
├── std/          the standard library
├── web/          the browser-side runtimes `vyrn dev` serves
├── README.md
├── LICENSE-MIT   the two licences, either of which the user may choose
├── LICENSE-APACHE
├── THIRD-PARTY-NOTICES.md  the one piece of third-party code the binary carries
└── VERSION       the tag
```

`std/` and `web/` must stay siblings of the binary's directory. `vyrn` finds
them by walking up from its own path (`std_root` and `web_root` in
`compiler/vyrn-cli/src/main.rs`), so both the extracted archive and the
installed layout (`~/.vyrn/bin/vyrn` beside `~/.vyrn/std`) resolve correctly.

`SHA256SUMS` is `sha256sum -c` format, sorted by filename. The install scripts
refuse to install any archive whose line is missing from it.

The archive ships `LICENSE-MIT`, `LICENSE-APACHE` and `THIRD-PARTY-NOTICES.md`
from the repository root. A user who has only the archive has the terms; they
are not a link back to GitHub. The notice matters for the binary in particular:
`vyrn build` emits Björn Höhrmann's UTF-8 DFA table into every artifact it
produces, so the notice has to reach whoever ends up holding one.

## What a user needs on their machine

| Command | Extra tooling |
|---------|---------------|
| `vyrn run`, `check`, `test`, `bench`, `fmt`, `doc` | none |
| `vyrn build --target wasm` | none |
| `vyrn build` (native) | `clang` on `PATH`, or `$CLANG` |
| the three-way parity harness | a `wasmtime` binary via `$VYRN_WASMTIME` |

## Install scripts

[`install.sh`](../install.sh) and [`install.ps1`](../install.ps1) live at the
repository root and are served from `main`:

```
https://raw.githubusercontent.com/vyrn-lang/vyrn/main/install.sh
https://raw.githubusercontent.com/vyrn-lang/vyrn/main/install.ps1
```

They are served from the branch, not from a release. A change to either script
goes live when it merges to `main` — tagging is not involved.

Both scripts resolve the newest release through
`GET /repos/<repo>/releases?per_page=1`, not through `/releases/latest`.
`/releases/latest` hides pre-releases, and every alpha is a pre-release, so it
would report "nothing published" for the whole alpha.

Both scripts verify sha256 before they unpack, and stop with a message and a
non-zero exit if the archive is absent from `SHA256SUMS`, if `SHA256SUMS` is
missing, or if the hash differs. They install nothing in those cases.

Environment overrides, in both: `VYRN_VERSION` (install a specific tag),
`VYRN_INSTALL_DIR` (default `~/.vyrn`), `VYRN_REPO`, and `VYRN_API` /
`VYRN_DOWNLOAD` (used to point the scripts at a local server for testing).

## Redo a release

A published tag is public. Prefer a new tag — `-alpha.2` costs nothing. If you
must redo one:

```bash
gh release delete v0.1.0-alpha.1 --yes
git push --delete origin v0.1.0-alpha.1
git tag -d v0.1.0-alpha.1
```

Then tag again. Anyone who installed the first one keeps it; the scripts do not
re-check.

## Add a platform

Add one row to the `build` matrix with `os`, `name`, `target` and `bin`, then
teach both install scripts the new `uname`/`PROCESSOR_ARCHITECTURE` case. The
archive name is `vyrn-<name>.tar.gz` (or `.zip` on Windows). Candidates:
`aarch64-linux` (`ubuntu-22.04-arm`) and `x86_64-macos`.

## Known gaps

- The archives are not signed. The checksum proves the bytes match the release;
  it does not prove who built them.

## The version, in one place

`vyrn --version` (and `-V`) prints `vyrn <CARGO_PKG_VERSION>` and exits 0. The
number lives in `compiler/vyrn-cli/Cargo.toml` and nowhere else: the archive's
`VERSION` file is still the tag, and the release workflow's first step fails if
the tag and that line disagree. **So raising the tag means raising the crate
version in the same commit.** The other workspace crates stay `0.0.0` — none is
published and nothing reads them.
