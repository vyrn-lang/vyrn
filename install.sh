#!/bin/sh
# Install the Vyrn alpha on Linux or macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/vyrn-lang/vyrn/main/install.sh | sh
#
# It downloads the archive for this machine from the newest GitHub release,
# checks it against that release's SHA256SUMS, and unpacks it under ~/.vyrn.
# It refuses to install anything it cannot verify.
#
# Environment overrides:
#   VYRN_VERSION      install this tag instead of the newest release
#   VYRN_INSTALL_DIR  install here instead of ~/.vyrn
#   VYRN_REPO         owner/name to download from (used by the test harness)

set -eu

REPO="${VYRN_REPO:-vyrn-lang/vyrn}"
DIR="${VYRN_INSTALL_DIR:-$HOME/.vyrn}"
API="${VYRN_API:-https://api.github.com}"
DL="${VYRN_DOWNLOAD:-https://github.com}"

die() { printf 'vyrn install: %s\n' "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }

# --- what to download -------------------------------------------------------

os=$(uname -s)
arch=$(uname -m)
case "$os/$arch" in
  Linux/x86_64|Linux/amd64)      asset=vyrn-x86_64-linux.tar.gz ;;
  # `uname -m` says `aarch64` on a Linux kernel and `arm64` on Darwin; both
  # spellings appear on Linux in the wild (some musl/BusyBox userlands, and
  # containers started with `--platform linux/arm64`), so accept either. This is
  # the Docker-on-Apple-Silicon case: the default platform there is linux/arm64,
  # and before the aarch64-linux release stanza this line fell through to "build
  # from source" for an ordinary audience.
  Linux/aarch64|Linux/arm64)     asset=vyrn-aarch64-linux.tar.gz ;;
  Darwin/arm64|Darwin/aarch64)   asset=vyrn-aarch64-macos.tar.gz ;;
  *)
    die "no published build for $os $arch.
  The alpha ships Linux x86_64, Linux arm64 and macOS arm64. On anything else,
  build from source: https://github.com/$REPO#build-from-source" ;;
esac

# --- how to download --------------------------------------------------------

if command -v curl >/dev/null 2>&1; then
  fetch()   { curl -fsSL "$1"; }            # to stdout, nonzero on HTTP error
  fetch_to(){ curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch()   { wget -qO- "$1"; }
  fetch_to(){ wget -qO "$2" "$1"; }
else
  die "need curl or wget on PATH"
fi

if command -v sha256sum >/dev/null 2>&1; then
  sum() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
  sum() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
  die "need sha256sum or shasum on PATH to verify the download"
fi

# --- which release ----------------------------------------------------------

if [ -n "${VYRN_VERSION:-}" ]; then
  tag="$VYRN_VERSION"
else
  # per_page=1 gives the newest release INCLUDING pre-releases, which
  # /releases/latest deliberately hides — and every alpha is a pre-release.
  # One release in the response means one "tag_name", so this stays a sed.
  json=$(fetch "$API/repos/$REPO/releases?per_page=1") || json=''
  tag=$(printf '%s' "$json" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)
  [ -n "$tag" ] || die "no release has been published for $REPO yet.
  Build from source instead:
    git clone https://github.com/$REPO.git
    cd vyrn/compiler && cargo build --release -p vyrn-cli"
fi

base="$DL/$REPO/releases/download/$tag"
say "vyrn $tag -> $DIR"

# --- download and verify ----------------------------------------------------

tmp=$(mktemp -d "${TMPDIR:-/tmp}/vyrn-install.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM

fetch_to "$base/$asset" "$tmp/$asset" ||
  die "cannot download $base/$asset"
fetch_to "$base/SHA256SUMS" "$tmp/SHA256SUMS" ||
  die "release $tag has no SHA256SUMS asset; refusing to install unverified bytes"

# sha256sum writes "<hex>  name" in text mode and "<hex> *name" in binary mode;
# accept both, so a SHA256SUMS produced on any host still verifies here.
want=$(awk -v f="$asset" '{ n = $2; sub(/^\*/, "", n); if (n == f) print $1 }' "$tmp/SHA256SUMS")
[ -n "$want" ] ||
  die "$asset is not listed in the SHA256SUMS of $tag; refusing to install unverified bytes"

got=$(sum "$tmp/$asset")
[ "$want" = "$got" ] || {
  rm -f "$tmp/$asset"
  die "checksum mismatch for $asset
  expected $want
  got      $got
  The download was discarded. Nothing was installed."
}
say "sha256 ok: $got"

# --- unpack -----------------------------------------------------------------

tar xzf "$tmp/$asset" -C "$tmp"
stage="$tmp/${asset%.tar.gz}"
[ -x "$stage/vyrn" ] || die "archive $asset has no vyrn binary in $(basename "$stage")/"

# `vyrn` finds std/ and web/ by walking up from its own path, so the binary
# goes in $DIR/bin and the trees stay its siblings one level up. $DIR/cache
# (remote imports, RFC-0010) is left alone.
mkdir -p "$DIR/bin"
rm -rf "$DIR/std" "$DIR/web" "$DIR/bin/vyrn" "$DIR/bin/vyrn-lsp"
mv "$stage/std" "$stage/web" "$DIR/"
mv "$stage/vyrn" "$DIR/bin/vyrn"
chmod +x "$DIR/bin/vyrn"
# The language server goes BESIDE the driver, which is where the VS Code
# extension looks for it (editor/vscode/extension.js): `vyrn-lsp` on PATH, or
# next to the `vyrn` that is on PATH. Guarded, so this script still installs an
# older archive that predates it.
if [ -f "$stage/vyrn-lsp" ]; then
  mv "$stage/vyrn-lsp" "$DIR/bin/vyrn-lsp"
  chmod +x "$DIR/bin/vyrn-lsp"
fi
if [ -f "$stage/VERSION" ]; then mv "$stage/VERSION" "$DIR/VERSION"; fi
if [ -f "$stage/README.md" ]; then mv "$stage/README.md" "$DIR/README.md"; fi

say "installed $DIR/bin/vyrn"

case ":${PATH}:" in
  *":$DIR/bin:"*) ;;
  *)
    say ""
    say "Add it to your PATH:"
    say "    export PATH=\"$DIR/bin:\$PATH\""
    say "(put that line in ~/.profile, ~/.bashrc or ~/.zshrc)"
    ;;
esac

say ""
say "vyrn run, check, test and build --target wasm need nothing else."
say "vyrn build (a native binary) needs clang on PATH."
