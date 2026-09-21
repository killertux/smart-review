#!/usr/bin/env bash
# Hermetic contract for the curl-pipe installer: fake release, real archive and checksum.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP="$(mktemp -d "${SMART_REVIEW_VALIDATION_TMP:-${TMPDIR:-/tmp}}/installer.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

case "$(uname -s):$(uname -m)" in
  Linux:x86_64|Linux:amd64) TARGET="x86_64-unknown-linux-gnu" ;;
  Darwin:x86_64|Darwin:amd64) TARGET="x86_64-apple-darwin" ;;
  Darwin:arm64|Darwin:aarch64) TARGET="aarch64-apple-darwin" ;;
  *) printf 'installer test does not support this host\n' >&2; exit 1 ;;
esac

mkdir -p "$TMP/assets/payload" "$TMP/fake-bin" "$TMP/home"
VERSION="$(sed -n '/^\[package\]/,/^\[/s/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
test -n "$VERSION"
cat >"$TMP/assets/payload/smart-review" <<SH
#!/bin/sh
printf 'smart-review $VERSION\n'
SH
chmod +x "$TMP/assets/payload/smart-review"
ARCHIVE="smart-review-$TARGET.tar.gz"
tar -C "$TMP/assets/payload" -czf "$TMP/assets/$ARCHIVE" smart-review

if command -v sha256sum >/dev/null 2>&1; then
  HASH="$(sha256sum "$TMP/assets/$ARCHIVE" | awk '{ print $1 }')"
else
  HASH="$(shasum -a 256 "$TMP/assets/$ARCHIVE" | awk '{ print $1 }')"
fi
printf '%s  %s\n' "$HASH" "$ARCHIVE" >"$TMP/assets/SHA256SUMS"

cat >"$TMP/fake-bin/curl" <<'SH'
#!/bin/sh
output=
url=
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) output=$2; shift 2 ;;
    *) url=$1; shift ;;
  esac
done
[ -n "$output" ] && [ -n "$url" ] || exit 2
cp "$INSTALLER_ASSETS/${url##*/}" "$output"
SH
chmod +x "$TMP/fake-bin/curl"

if ! cat "$ROOT/install.sh" | PATH="$TMP/fake-bin:$PATH" \
    HOME="$TMP/home" SMART_REVIEW_INSTALL_DIR="$TMP/bin" \
    INSTALLER_ASSETS="$TMP/assets" sh >"$TMP/install.log" 2>&1; then
  cat "$TMP/install.log" >&2
  exit 1
fi

test -x "$TMP/bin/smart-review"
test "$("$TMP/bin/smart-review" --version)" = "smart-review $VERSION"
grep -q "Installed smart-review to $TMP/bin/smart-review" "$TMP/install.log"

rm -f "$TMP/bin/smart-review"
printf '%064d  %s\n' 0 "$ARCHIVE" >"$TMP/assets/SHA256SUMS"
if cat "$ROOT/install.sh" | PATH="$TMP/fake-bin:$PATH" \
    HOME="$TMP/home" SMART_REVIEW_INSTALL_DIR="$TMP/bin" \
    INSTALLER_ASSETS="$TMP/assets" sh >"$TMP/corrupt.log" 2>&1; then
  printf 'installer accepted an archive with the wrong checksum\n' >&2
  exit 1
fi
grep -q 'checksum verification failed' "$TMP/corrupt.log"
test ! -e "$TMP/bin/smart-review"

printf 'installer contract: ok (%s)\n' "$TARGET"
