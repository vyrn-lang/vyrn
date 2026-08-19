#!/bin/sh
# Execute `install.sh` against a fake release, including the case it exists for.
#
#   sh install-test.sh
#
# `install.sh` promises three things a reader cannot check by reading: it
# installs, it VERIFIES against SHA256SUMS, and it refuses anything it cannot
# verify. Until this harness nothing ran it — the checksum refusal the README
# advertises had never executed, and `install.sh`'s own header documented a
# `VYRN_REPO` hook "used by the test harness" that did not exist.
#
# No GitHub and no credentials: `VYRN_DOWNLOAD` already exists to point the
# download at somewhere else, and `VYRN_VERSION` skips the release-listing call.
# The somewhere else is python's `http.server` over a directory laid out like
# the release CDN — an http:// origin rather than a `file://` one, because the
# real path is HTTP and because one URL spelling works on all three platforms.

set -eu

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/vyrn-install-test.XXXXXX")
srv=

cleanup() {
  code=$?
  if [ -n "$srv" ]; then
    kill "$srv" 2>/dev/null || true
    wait "$srv" 2>/dev/null || true
  fi
  # Never let tidying decide the verdict: a server that still holds its cwd
  # open (Windows) must not turn four green checks into a red run.
  rm -rf "$tmp" 2>/dev/null || true
  exit "$code"
}
trap cleanup EXIT INT TERM

tag=v0.0.0-test
rel="$tmp/dl/vyrn-lang/vyrn/releases/download/$tag"
mkdir -p "$rel"

fail() { printf 'install-test: %b\n' "$*" >&2; exit 1; }

# --- a release, staged the way release.yml stages one ------------------------

stage="$tmp/stage"
mkdir -p "$stage/std" "$stage/web"
printf '#!/bin/sh\necho vyrn-stub\n' > "$stage/vyrn"
chmod +x "$stage/vyrn"
printf '#!/bin/sh\necho vyrn-lsp-stub\n' > "$stage/vyrn-lsp"
chmod +x "$stage/vyrn-lsp"
printf 'export fn x() -> Int64 { return 0 }\n' > "$stage/std/strings.vyrn"
printf '// browser runtime\n' > "$stage/web/vyrn-dom.js"
printf '%s\n' "$tag" > "$stage/VERSION"

# One staged tree under every asset name the script may ask for, so the harness
# does not need a second copy of install.sh's uname mapping — a second copy of a
# mapping is how the two spellings drift apart.
for name in vyrn-x86_64-linux vyrn-aarch64-linux vyrn-aarch64-macos; do
  ln -sfn "$stage" "$tmp/$name" 2>/dev/null || cp -r "$stage" "$tmp/$name"
  (cd "$tmp" && tar czhf "$rel/$name.tar.gz" "$name")
  rm -rf "$tmp/$name"
done

sums() {
  if command -v sha256sum >/dev/null 2>&1; then (cd "$rel" && sha256sum vyrn-*.tar.gz > SHA256SUMS)
  else (cd "$rel" && shasum -a 256 vyrn-*.tar.gz > SHA256SUMS); fi
}
sums

# --- serve it ----------------------------------------------------------------

py=python3
command -v "$py" >/dev/null 2>&1 || py=python
command -v "$py" >/dev/null 2>&1 || fail "need python3 (or python) to serve the fake release"
port=$(( 40000 + ( $$ % 20000 ) ))
( cd "$tmp/dl" && "$py" -m http.server "$port" --bind 127.0.0.1 >/dev/null 2>&1 ) &
srv=$!
i=0
until curl -fsS "http://127.0.0.1:$port/" >/dev/null 2>&1 ||
      wget -q -O- "http://127.0.0.1:$port/" >/dev/null 2>&1; do
  i=$((i + 1))
  [ "$i" -gt 100 ] && fail "the fake release server did not come up on $port"
  sleep 0.1
done

run_install() {
  VYRN_VERSION="$tag" \
  VYRN_DOWNLOAD="http://127.0.0.1:$port" \
  VYRN_INSTALL_DIR="$1" \
  sh "$here/install.sh"
}

# --- 1. it installs ----------------------------------------------------------

dir="$tmp/home-ok"
out=$(run_install "$dir" 2>&1) || fail "the happy path failed:\n$out"
printf '%s\n' "$out" | grep -q 'sha256 ok' || fail "no checksum line:\n$out"
[ -x "$dir/bin/vyrn" ] || fail "no binary at \$DIR/bin/vyrn"
[ "$("$dir/bin/vyrn")" = "vyrn-stub" ] || fail "the installed binary does not run"
# std/ and web/ are SIBLINGS of bin/, which is the walk-up rule `vyrn` uses to
# find them; an installer that puts them anywhere else ships a broken tree.
[ -f "$dir/std/strings.vyrn" ] || fail "std/ is not beside bin/"
[ -f "$dir/web/vyrn-dom.js" ] || fail "web/ is not beside bin/"
# The language server lands beside the driver, which is the whole reason the VS
# Code extension can find it without a setting.
[ -x "$dir/bin/vyrn-lsp" ] || fail "no server at \$DIR/bin/vyrn-lsp"
[ "$(cat "$dir/VERSION")" = "$tag" ] || fail "VERSION does not carry the tag"
echo "ok: installs, and the tree is shaped the way vyrn walks up for it"

# --- 2. it refuses bytes that do not match the checksum ----------------------

# Tampered AFTER SHA256SUMS was written: exactly the case the file exists for.
for f in "$rel"/vyrn-*.tar.gz; do printf 'tampered' >> "$f"; done
dir="$tmp/home-tampered"
if out=$(run_install "$dir" 2>&1); then
  fail "a tampered archive INSTALLED:\n$out"
fi
printf '%s\n' "$out" | grep -q 'checksum mismatch' || fail "wrong refusal:\n$out"
[ ! -e "$dir/bin/vyrn" ] || fail "it refused and installed anyway"
echo "ok: a tampered archive is refused, and nothing is installed"

# --- 3. it refuses a release with no SHA256SUMS ------------------------------

sums                       # re-sign the tampered archives: they are valid now
rm -f "$rel/SHA256SUMS"
dir="$tmp/home-nosums"
if out=$(run_install "$dir" 2>&1); then
  fail "an unverifiable release INSTALLED:\n$out"
fi
printf '%s\n' "$out" | grep -q 'refusing to install unverified bytes' ||
  fail "wrong refusal:\n$out"
[ ! -e "$dir/bin/vyrn" ] || fail "it refused and installed anyway"
echo "ok: a release with no SHA256SUMS is refused"

# --- 4. it refuses when the asset is not LISTED in SHA256SUMS ----------------

printf 'deadbeef  some-other-file.tar.gz\n' > "$rel/SHA256SUMS"
dir="$tmp/home-unlisted"
if out=$(run_install "$dir" 2>&1); then
  fail "an unlisted asset INSTALLED:\n$out"
fi
printf '%s\n' "$out" | grep -q 'not listed in the SHA256SUMS' || fail "wrong refusal:\n$out"
echo "ok: an asset missing from SHA256SUMS is refused"

echo "install.sh: 4 checks passed"
