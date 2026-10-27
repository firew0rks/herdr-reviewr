#!/usr/bin/env bash
# herdr `[[build]]` step: download the prebuilt herdr-reviewr binary for this platform from the
# matching GitHub Release into the plugin's bin/ dir. Runs on `herdr plugin install` (a managed
# checkout); `herdr plugin link` skips the build step — for a local checkout, build from source
# with `cargo install --path .`.
#
# The build runs with the plugin checkout as the working directory, so we resolve the plugin root
# from this script's location rather than $HERDR_PLUGIN_ROOT (build commands may not receive the
# runtime env). At runtime the pane command reads $HERDR_PLUGIN_ROOT/bin/herdr-reviewr.
set -euo pipefail

NAME="herdr-reviewr"
REPO="firew0rks/herdr-reviewr"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="$ROOT/bin"

# The release tag matches the manifest version, so a checkout always pulls its own release.
VERSION="$(grep -m1 '^version' "$ROOT/herdr-plugin.toml" | sed -E 's/.*"([^"]+)".*/\1/')"
TAG="fork-v${VERSION}"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) asset="$NAME-macos-aarch64" ;;
  Linux-x86_64) asset="$NAME-linux-x86_64" ;;
  *)
    echo "$NAME: no prebuilt binary for $(uname -s)-$(uname -m) — build from source with 'cargo install --path .'" >&2
    exit 1
    ;;
esac

base="https://github.com/${REPO}/releases/download/${TAG}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

sha() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | awk '{print $1}'; }
# Release assets can 404 for a few minutes after publishing, so curl retries on 404 too.
# gh first: it carries auth, which a private fork's assets would need.
dl() {
  if command -v gh >/dev/null && gh release download "$TAG" -R "$REPO" -p "$(basename "$1")" -O "$2" --clobber 2>/dev/null; then
    return
  fi
  curl -fsSL --retry 5 --retry-delay 3 --retry-all-errors "$1" -o "$2"
}

echo "$NAME: downloading $asset ($TAG)"
dl "$base/$asset.sha256" "$tmp/$asset.sha256"
dl "$base/$asset" "$tmp/$asset"
[ "$(sha "$tmp/$asset")" = "$(awk '{print $1}' "$tmp/$asset.sha256")" ] || { echo "$NAME: checksum mismatch" >&2; exit 1; }

mkdir -p "$BIN_DIR"
install -m 0755 "$tmp/$asset" "$BIN_DIR/$NAME"
echo "$NAME: installed $BIN_DIR/$NAME"

# Stable launch paths (specs/herdr-host.md, Install paths): symlinks into the installed
# plugin, never copies, so a launch after an uninstall fails loudly instead of running a
# stale build. An existing symlink re-points on every install — the hash-suffixed plugin
# root moves — but anything else at the path is left alone: a user's own binary there
# (`cargo install --root ~/.local`) must survive, and `ln -sfn` onto a real directory
# would nest inside it. The binary is already installed, so a skipped or failed link
# warns without failing the install. This build may run in a staging checkout herdr
# renames afterwards, so the links aim at the runtime root when herdr provides one, and
# every action re-points them at the live root regardless (herdr/pane.sh).
LINK_ROOT="${HERDR_PLUGIN_ROOT:-$ROOT}"
link_binary() {
  if mkdir -p "$1" 2>/dev/null && { [ -L "$1/$NAME" ] || [ ! -e "$1/$NAME" ]; } &&
    ln -sfn "$LINK_ROOT/bin/$NAME" "$1/$NAME" 2>/dev/null; then
    echo "$NAME: linked $1/$NAME"
  else
    echo "$NAME: warning: could not link $1/$NAME" >&2
  fi
}
link_binary "$HOME/.local/state/herdr/plugins/persiyanov.reviewr/bin"
if [ -d "$HOME/.local/bin" ]; then
  link_binary "$HOME/.local/bin"
fi
