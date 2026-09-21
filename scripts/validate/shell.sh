#!/usr/bin/env bash
#
# Shell, CLI, configuration robustness, and terminal lifecycle validation.
#
# Checks that the repository is formatted, lint-clean, tested, and that the
# binary behaves the way the shell requirements say it does. Exits non-zero if
# anything fails. No network access is required.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
source "$ROOT/scripts/validate/common.sh"
validation_mode "$@" || exit $?

PASS=0
FAIL=0
ok() { printf '  PASS  %s\n' "$1"; PASS=$((PASS + 1)); }
bad() { printf '  FAIL  %s\n' "$1"; FAIL=$((FAIL + 1)); }
step() { printf '\n== %s ==\n' "$1"; }

TMP_HOME="$(mktemp -d "${SMART_REVIEW_VALIDATION_TMP:-${TMPDIR:-/tmp}}/shell.XXXXXX")"
BIN="${SMART_REVIEW_BIN:-$ROOT/target/debug/smart-review}"
cleanup() {
  if [ "${KEEP:-0}" = "1" ]; then
    printf '  note: kept %s\n' "$TMP_HOME"
  else
    rm -rf "$TMP_HOME"
  fi
}
trap cleanup EXIT

if [ "${SMART_REVIEW_SKIP_CARGO:-0}" = "1" ]; then
  step "1-4/7 shared Cargo gates"
  printf '  SKIP  formatting, lints, tests and build already passed in the parent validator\n'
else
  step "1/7 formatting"
  if cargo fmt --all --check >/dev/null 2>&1; then
    ok "cargo fmt --check"
  else
    bad "cargo fmt --check"
  fi

  step "2/7 lints"
  if cargo clippy --all-targets --all-features -- -D warnings >"$TMP_HOME/shell-clippy.log" 2>&1; then
    ok "cargo clippy -- -D warnings"
  else
    bad "cargo clippy -- -D warnings"
    tail -20 "$TMP_HOME/shell-clippy.log"
  fi

  step "3/7 tests"
  if cargo test --all-features >"$TMP_HOME/shell-test.log" 2>&1; then
    ok "cargo test --all-features ($(grep -c '^test .* ok$' "$TMP_HOME/shell-test.log") tests reported)"
  else
    bad "cargo test --all-features"
    grep -E '^test .* FAILED|panicked' "$TMP_HOME/shell-test.log" | head -10
  fi

  step "4/7 build"
  if cargo build >"$TMP_HOME/shell-build.log" 2>&1; then
    ok "cargo build"
  else
    bad "cargo build"
    tail -20 "$TMP_HOME/shell-build.log"
  fi
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
SMART_REVIEW_HOME="$TMP_HOME" "$BIN" --check >"$TMP_HOME/shell-check.log" 2>&1
CHECK_CODE=$?
set -e

# `--check` runs the same detection the interface does (FR-1.2), so on a machine
# without an authenticated `gh` the honest answer is 2 — the app cannot read a pull
# request at all. What matters here is that a report is produced and the code is one of
# the three documented ones.
case "$CHECK_CODE" in
  0 | 1 | 2) ok "--check exits with $CHECK_CODE (0 ready / 1 degraded / 2 unusable)" ;;
  *)
    bad "--check exited with $CHECK_CODE; expected 0, 1 or 2"
    cat "$TMP_HOME/shell-check.log"
    ;;
esac

if grep -q 'repository' "$TMP_HOME/shell-check.log"; then
  ok "the report says which repository was found, or why not"
else
  bad "the report did not mention the repository"
fi

for name in config home keybinds llm log terminal theme; do
  if grep -q "$name" "$TMP_HOME/shell-check.log"; then
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
SMART_REVIEW_HOME="$TMP_HOME" "$BIN" --check >"$TMP_HOME/shell-bad-config.log" 2>&1
BAD_CONFIG_CODE=$?
set -e
# The point is that the unusable value is *reported* and a report is still produced,
# not which of the three codes the run lands on: detection may fail for unrelated
# reasons on the machine running this.
if [ "$BAD_CONFIG_CODE" -ne 124 ] && grep -q 'timeoutlen' "$TMP_HOME/shell-bad-config.log"; then
  ok "an unusable value is reported and the app still starts"
else
  bad "a bad value did not degrade gracefully (exit $BAD_CONFIG_CODE)"
  cat "$TMP_HOME/shell-bad-config.log"
fi

# The warning itself has to name the file (FR-8.6). The doctor's config line
# always mentions the path, so match the prefix only a prefixed warning has.
if grep -q "$TMP_HOME/config.toml: config:" "$TMP_HOME/shell-bad-config.log"; then
  ok "the warning names the configuration file"
else
  bad "the warning does not name the configuration file"
  cat "$TMP_HOME/shell-bad-config.log"
fi

# Broken TOML is unrecoverable and must be a clean, explained failure (exit 2).
printf '[ui\ntheme =' >"$TMP_HOME/config.toml"
set +e
SMART_REVIEW_HOME="$TMP_HOME" "$BIN" --check >"$TMP_HOME/shell-broken.log" 2>&1
BROKEN_CODE=$?
set -e
if [ "$BROKEN_CODE" -eq 2 ] && grep -q 'not valid TOML' "$TMP_HOME/shell-broken.log"; then
  ok "invalid TOML fails cleanly with exit 2"
else
  bad "invalid TOML gave exit $BROKEN_CODE without an explanation"
  cat "$TMP_HOME/shell-broken.log"
fi

# Nothing may be written into the user's repository (FR-8.1).
if [ ! -e "$ROOT/.smart-review" ]; then
  ok "nothing was written inside the repository"
else
  bad "$ROOT/.smart-review was created"
fi

step "7/7 terminal lifecycle in a real pty"
if command -v python3 >/dev/null 2>&1; then
  PTY_HOME="$(mktemp -d "$TMP_HOME/pty.XXXXXX")"
  RESIZE_STEPS=""
  TUI_KEYS=' ~\e~:bogus\r~:q\r'
  TUI_WAITS='leader~~not a command~'
  if validation_smoke_only; then
    RESIZE_STEPS='79x23=terminal too small~120x40=smart-review'
    TUI_KEYS=':theme light\r~ ~\e~:bogus\r~:q\r'
    TUI_WAITS='theme: light~leader~~not a command~'
  fi
  set +e
  # The driver advances on visible postconditions and an actual quiet PTY rather than
  # paying a fixed sleep after every key.
  SMART_REVIEW_HOME="$PTY_HOME" python3 "$ROOT/scripts/validate/drive.py" \
    --cols 120 --rows 40 --log "$TMP_HOME/shell-tui.log" --ready 'smart-review' \
    --resize-steps "$RESIZE_STEPS" \
    --keys "$TUI_KEYS" --waits "$TUI_WAITS" -- \
    "$BIN" >"$TMP_HOME/shell-tui-screen.log"
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

  # Prove the screen was painted at all, otherwise the checks below would pass
  # vacuously on an empty render.
  if python3 "$ROOT/scripts/validate/screen.py" --cols 120 --rows 40 \
      --path "$TMP_HOME/shell-tui.log" --when 'smart-review[\s\S]*NORMAL|NORMAL[\s\S]*smart-review' \
      >/dev/null 2>&1; then
    ok "the interface painted the header and the status line"
  else
    bad "the interface rendered nothing"
  fi

  if python3 "$ROOT/scripts/validate/screen.py" --cols 120 --rows 40 \
      --path "$TMP_HOME/shell-tui.log" --when leader >/dev/null 2>&1; then
    ok "the leader menu appears without waiting for the timeout"
  else
    bad "the leader menu did not appear on the key press"
  fi

  if python3 "$ROOT/scripts/validate/screen.py" --cols 120 --rows 40 \
      --path "$TMP_HOME/shell-tui.log" --when bogus >/dev/null 2>&1; then
    ok "an unknown command is reported on screen"
  else
    bad "an unknown command produced no visible output"
  fi

  if python3 "$ROOT/scripts/validate/screen.py" --cols 120 --rows 40 \
      --path "$TMP_HOME/shell-tui.log" --when 'not a command' >/dev/null 2>&1; then
    ok "the unknown command error names the problem"
  else
    bad "the unknown command error is missing"
  fi

  if grep -q $'\x1b\[?1049l' "$TMP_HOME/shell-tui.log"; then
    ok "the alternate screen was left on exit"
  else
    bad "the alternate screen was not left on exit"
  fi

  if grep -q $'\x1b\[?1006l' "$TMP_HOME/shell-tui.log"; then
    ok "mouse capture was released on exit"
  else
    bad "mouse capture was not released on exit"
  fi

  if validation_smoke_only && [ "$TUI_CODE" -eq 0 ]; then
    ok "SIGWINCH reflows to the minimum-size warning and back"
  elif validation_smoke_only; then
    bad "the resize smoke did not render the minimum-size warning"
  fi

  if validation_smoke_only; then
    set +e
    SMART_REVIEW_HOME="$PTY_HOME" python3 "$ROOT/scripts/validate/drive.py" \
      --cols 120 --rows 40 --log "$TMP_HOME/shell-restore.log" --ready 'smart-review' \
      --keys ':theme\r~\e~:q\r' --waits '\* light~~' -- \
      "$BIN" >"$TMP_HOME/shell-restore-screen.log"
    RESTORE_CODE=$?
    set -e
    if [ "$RESTORE_CODE" -eq 0 ] \
        && grep -q 'theme = "light"' "$PTY_HOME/state.toml" 2>/dev/null; then
      ok "startup restores the state saved by the previous terminal session"
    else
      bad "the second terminal session did not restore the saved theme"
    fi
  fi

else
  printf '  SKIP  python3 is not available, so the pty check was skipped\n'
fi

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
