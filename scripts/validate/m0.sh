#!/usr/bin/env bash
#
# Milestone M0 validation (see PLAN.md §2).
#
# Checks that the repository is formatted, lint-clean, tested, and that the
# binary behaves the way the M0 requirements say it does. Exits non-zero if
# anything fails. No network access is required.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

PASS=0
FAIL=0
ok() { printf '  PASS  %s\n' "$1"; PASS=$((PASS + 1)); }
bad() { printf '  FAIL  %s\n' "$1"; FAIL=$((FAIL + 1)); }
step() { printf '\n== %s ==\n' "$1"; }

TMP_HOME="$(mktemp -d)"
BIN="target/release/smart-review"
cleanup() { rm -rf "$TMP_HOME"; }
trap cleanup EXIT

step "1/7 formatting"
if cargo fmt --all --check >/dev/null 2>&1; then
  ok "cargo fmt --check"
else
  bad "cargo fmt --check"
fi

step "2/7 lints"
if cargo clippy --all-targets --all-features -- -D warnings >/tmp/m0-clippy.log 2>&1; then
  ok "cargo clippy -- -D warnings"
else
  bad "cargo clippy -- -D warnings"
  tail -20 /tmp/m0-clippy.log
fi

step "3/7 tests"
if cargo test --all-features >/tmp/m0-test.log 2>&1; then
  ok "cargo test --all-features ($(grep -c '^test .* ok$' /tmp/m0-test.log) tests reported)"
else
  bad "cargo test --all-features"
  grep -E '^test .* FAILED|panicked' /tmp/m0-test.log | head -10
fi

step "4/7 release build"
if cargo build --release >/tmp/m0-build.log 2>&1; then
  ok "cargo build --release"
else
  bad "cargo build --release"
  tail -20 /tmp/m0-build.log
fi

if [ ! -x "$BIN" ]; then
  bad "$BIN was not produced; skipping the remaining checks"
  printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
  exit 1
fi

step "5/7 CLI surface"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
if "$BIN" --version | grep -q "$VERSION"; then
  ok "--version reports $VERSION"
else
  bad "--version did not report $VERSION"
fi

if "$BIN" --help | grep -q -- '--check'; then
  ok "--help documents --check"
else
  bad "--help does not document --check"
fi

step "6/7 first run, configuration robustness and layout"
set +e
SMART_REVIEW_HOME="$TMP_HOME" "$BIN" --check >/tmp/m0-check.log 2>&1
CHECK_CODE=$?
set -e

# No model configured is a warning, so the expected code is 1 (degraded).
if [ "$CHECK_CODE" -eq 0 ] || [ "$CHECK_CODE" -eq 1 ]; then
  ok "--check exits with $CHECK_CODE (0 ready / 1 degraded)"
else
  bad "--check exited with $CHECK_CODE; expected 0 or 1"
  cat /tmp/m0-check.log
fi

for name in config home keybinds llm log terminal theme; do
  if grep -q "$name" /tmp/m0-check.log; then
    ok "--check reports the $name check"
  else
    bad "--check does not report the $name check"
  fi
done

for dir in themes cache worktrees logs; do
  if [ -d "$TMP_HOME/$dir" ]; then
    ok "created $dir/"
  else
    bad "did not create $dir/"
  fi
done

if [ -f "$TMP_HOME/README.md" ]; then
  ok "wrote the home README.md"
else
  bad "did not write the home README.md"
fi

# Directories the app owns must not be readable by other users (NFR-3.1).
if [ "$(uname)" != "Windows_NT" ]; then
  PERMS="$(stat -c '%a' "$TMP_HOME/themes" 2>/dev/null || stat -f '%Lp' "$TMP_HOME/themes" 2>/dev/null)"
  if [ "$PERMS" = "700" ]; then
    ok "private directory mode (700)"
  else
    bad "theme directory mode is $PERMS, expected 700"
  fi
fi

# A malformed value must not stop startup: the rest of the file still applies.
cat >"$TMP_HOME/config.toml" <<'TOML'
[ui]
timeoutlen = "soon"
theme = "light"
TOML
set +e
SMART_REVIEW_HOME="$TMP_HOME" "$BIN" --check >/tmp/m0-bad-config.log 2>&1
BAD_CONFIG_CODE=$?
set -e
if [ "$BAD_CONFIG_CODE" -eq 1 ] && grep -q 'timeoutlen' /tmp/m0-bad-config.log; then
  ok "an unusable value is reported and the app still starts"
else
  bad "a bad value did not degrade gracefully (exit $BAD_CONFIG_CODE)"
  cat /tmp/m0-bad-config.log
fi

# Broken TOML is unrecoverable and must be a clean, explained failure (exit 2).
printf '[ui\ntheme =' >"$TMP_HOME/config.toml"
set +e
SMART_REVIEW_HOME="$TMP_HOME" "$BIN" --check >/tmp/m0-broken.log 2>&1
BROKEN_CODE=$?
set -e
if [ "$BROKEN_CODE" -eq 2 ] && grep -q 'not valid TOML' /tmp/m0-broken.log; then
  ok "invalid TOML fails cleanly with exit 2"
else
  bad "invalid TOML gave exit $BROKEN_CODE without an explanation"
  cat /tmp/m0-broken.log
fi

# Nothing may be written into the user's repository (FR-8.1).
if [ ! -e "$ROOT/.smart-review" ]; then
  ok "nothing was written inside the repository"
else
  bad "$ROOT/.smart-review was created"
fi

step "7/7 terminal lifecycle in a real pty"
# `script -qec` is GNU-only; BSD/macOS script has different flags, so skip there
# rather than report a false failure.
if script --version 2>&1 | grep -q util-linux; then
  PTY_HOME="$(mktemp -d)"
  set +e
  (sleep 1; printf ':q\r'; sleep 2) \
    | SMART_REVIEW_HOME="$PTY_HOME" timeout 20 script -qec "$ROOT/$BIN" /dev/null \
    >/tmp/m0-tui.log 2>&1
  TUI_CODE=$?
  set -e

  if [ "$TUI_CODE" -eq 0 ]; then
    ok "the interface starts and quits cleanly on :q"
  else
    bad "the interface did not quit cleanly (exit $TUI_CODE)"
  fi

  if grep -q 'shutting down normally' "$PTY_HOME/logs/smart-review.log" 2>/dev/null; then
    ok "the shutdown was logged"
  else
    bad "no clean shutdown was logged"
  fi

  if grep -q $'\x1b\[?1049l' /tmp/m0-tui.log; then
    ok "the alternate screen was left on exit"
  else
    bad "the alternate screen was not left on exit"
  fi

  if grep -q $'\x1b\[?1006l' /tmp/m0-tui.log; then
    ok "mouse capture was released on exit"
  else
    bad "mouse capture was not released on exit"
  fi

  rm -rf "$PTY_HOME"
else
  printf '  SKIP  a GNU `script` is not available, so the pty check was skipped\n'
fi

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
