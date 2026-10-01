#!/usr/bin/env bash
# Put prebuilt binaries for the checked-out commit into target/release, so no compile is needed.
# They come from the `edge` release that CI publishes for every commit on main
# (.github/workflows/release.yml). Exits non-zero, changing nothing, when they can't be used:
# local changes to the Rust code, an unsupported platform, or no build for this commit (yet).
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

[ "${ZEN_BUILD_FROM_SOURCE:-}" = "1" ] && exit 1
[ "$(uname -s)" = "Linux" ] && [ "$(uname -m)" = "x86_64" ] || exit 1
# Local edits to anything compiled must be built locally.
git diff --quiet HEAD -- crates Cargo.toml Cargo.lock 2>/dev/null || exit 1
[ -z "$(git ls-files --others --exclude-standard -- crates)" ] || exit 1

SHA=$(git rev-parse --short=12 HEAD)
OUT="$REPO/target/release"
if [ "$(cat "$OUT/.prebuilt" 2>/dev/null)" = "$SHA" ]; then exit 0; fi

# Release downloads live on the GitHub repo this checkout came from (override with ZEN_RELEASE_BASE).
ORIGIN=$(git remote get-url origin 2>/dev/null || true)
SLUG=$(echo "$ORIGIN" | sed -nE 's#^(https://github.com/|git@github.com:)([^/]+/[^/.]+)(\.git)?/?$#\2#p')
BASE="${ZEN_RELEASE_BASE:-${SLUG:+https://github.com/$SLUG/releases/download/edge}}"
[ -n "$BASE" ] || exit 1

NAME="zenbot-x86_64-linux-$SHA.tar.gz"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
curl -fsSL --retry 2 -o "$TMP/$NAME" "$BASE/$NAME" 2>/dev/null || exit 1
curl -fsSL --retry 2 -o "$TMP/$NAME.sha256" "$BASE/$NAME.sha256" 2>/dev/null || exit 1
(cd "$TMP" && sha256sum -c --quiet "$NAME.sha256") || { echo "checksum mismatch for $NAME" >&2; exit 1; }
mkdir -p "$TMP/x" "$OUT"
tar -xzf "$TMP/$NAME" -C "$TMP/x"
for b in zend zen zen-engine; do install -m 755 "$TMP/x/$b" "$OUT/$b"; done
echo "$SHA" > "$OUT/.prebuilt"
