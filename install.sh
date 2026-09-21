#!/bin/sh

# Install the newest smart-review release for this machine.

set -eu

REPOSITORY="killertux/smart-review"
INSTALL_DIR="${SMART_REVIEW_INSTALL_DIR:-${HOME:?HOME is not set}/.local/bin}"
RELEASE_URL="https://github.com/$REPOSITORY/releases/latest/download"

fail() {
    printf 'smart-review installer: %s\n' "$1" >&2
    exit 1
}

require() {
    command -v "$1" >/dev/null 2>&1 \
        || fail "missing required command '$1'; install it and run the installer again"
}

case "$(uname -s):$(uname -m)" in
    Linux:x86_64 | Linux:amd64)
        TARGET="x86_64-unknown-linux-gnu"
        ;;
    Darwin:x86_64 | Darwin:amd64)
        TARGET="x86_64-apple-darwin"
        ;;
    Darwin:arm64 | Darwin:aarch64)
        TARGET="aarch64-apple-darwin"
        ;;
    *)
        fail "unsupported platform $(uname -s)/$(uname -m); build from source with Cargo instead"
        ;;
esac

require curl
require tar

TEMP_ROOT="${TMPDIR:-/tmp}"
TEMP_DIR="$(mktemp -d "$TEMP_ROOT/smart-review-install.XXXXXX")" \
    || fail "could not create a temporary directory; check TMPDIR and try again"
DESTINATION_TEMP=
cleanup() {
    rm -rf "$TEMP_DIR"
    if [ -n "$DESTINATION_TEMP" ]; then
        rm -f "$DESTINATION_TEMP"
    fi
}
trap cleanup EXIT HUP INT TERM

ARCHIVE="smart-review-$TARGET.tar.gz"
CHECKSUMS="SHA256SUMS"

curl --proto '=https' --tlsv1.2 -fsSL \
    "$RELEASE_URL/$ARCHIVE" -o "$TEMP_DIR/$ARCHIVE" \
    || fail "could not download $ARCHIVE; check the latest release and try again"
curl --proto '=https' --tlsv1.2 -fsSL \
    "$RELEASE_URL/$CHECKSUMS" -o "$TEMP_DIR/$CHECKSUMS" \
    || fail "could not download release checksums; check the latest release and try again"

EXPECTED="$(awk -v archive="$ARCHIVE" '$2 == archive || $2 == "*" archive { print $1; exit }' "$TEMP_DIR/$CHECKSUMS")"
[ -n "$EXPECTED" ] \
    || fail "the release checksum file does not contain $ARCHIVE; report the release as incomplete"

if command -v sha256sum >/dev/null 2>&1; then
    ACTUAL="$(sha256sum "$TEMP_DIR/$ARCHIVE" | awk '{ print $1 }')"
elif command -v shasum >/dev/null 2>&1; then
    ACTUAL="$(shasum -a 256 "$TEMP_DIR/$ARCHIVE" | awk '{ print $1 }')"
else
    fail "missing 'sha256sum' or 'shasum'; install one and run the installer again"
fi

[ "$ACTUAL" = "$EXPECTED" ] \
    || fail "checksum verification failed; remove the download and try again"

tar -xzf "$TEMP_DIR/$ARCHIVE" -C "$TEMP_DIR" \
    || fail "could not extract $ARCHIVE; download the release archive manually"
[ -f "$TEMP_DIR/smart-review" ] \
    || fail "the release archive has no smart-review binary; report the release as incomplete"

mkdir -p "$INSTALL_DIR" \
    || fail "could not create $INSTALL_DIR; choose a writable SMART_REVIEW_INSTALL_DIR"
DESTINATION_TEMP="$INSTALL_DIR/.smart-review-install.$$"
cp "$TEMP_DIR/smart-review" "$DESTINATION_TEMP" \
    || fail "could not write to $INSTALL_DIR; choose a writable SMART_REVIEW_INSTALL_DIR"
chmod 0755 "$DESTINATION_TEMP" \
    || fail "could not make the downloaded binary executable; check $INSTALL_DIR permissions"
mv -f "$DESTINATION_TEMP" "$INSTALL_DIR/smart-review" \
    || fail "could not replace $INSTALL_DIR/smart-review; check its permissions"
DESTINATION_TEMP=

printf 'Installed smart-review to %s/smart-review\n' "$INSTALL_DIR"
case ":${PATH:-}:" in
    *":$INSTALL_DIR:"*) ;;
    *) printf 'Add %s to PATH, then run: smart-review --check\n' "$INSTALL_DIR" ;;
esac
