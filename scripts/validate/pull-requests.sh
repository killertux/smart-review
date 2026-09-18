#!/usr/bin/env bash
#
# Pull request browsing, filtering, diff reading, and navigation validation.
#
# Checks that PR browsing and diff reading work end to end without touching the
# network: a fake `gh` answers the calls the app makes, and the checks assert what
# the screen shows. Exits non-zero if anything fails.
#
# The fake is put first on PATH rather than configured by path, because the app
# resolves `gh` the way a user's shell would.

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

TMP="$(mktemp -d "${SMART_REVIEW_VALIDATION_TMP:-${TMPDIR:-/tmp}}/pull-requests.XXXXXX")"
BIN="target/debug/smart-review"
FIXTURES="$ROOT/tests/fixtures/gh"
cleanup() {
  if [ "${KEEP:-0}" = "1" ]; then
    printf '  note: kept %s\n' "$TMP"
  else
    rm -rf "$TMP"
  fi
}
trap cleanup EXIT

# Captured before anything runs, so the last step can tell whether the run wrote into
# the checkout rather than comparing two reads taken at the same moment.
BEFORE="$(git status --porcelain --ignored=no | sort)"

# ---------------------------------------------------------------------------
# A fake `gh` that answers the probes and the PR calls from the fixtures.
#
# `FAIL_LIST=1` makes the list call fail while the probes still answer, which is
# what "the network is gone" looks like to the app.
# ---------------------------------------------------------------------------
make_fake_gh() {
  local dir="$1" fail_list="${2:-0}"
  mkdir -p "$dir"

  # The heredoc is quoted, so nothing in the script is expanded when it is *written*:
  # the fake reads its own directory at run time instead. An unquoted heredoc expanded
  # `$(...)` and `$FIXTURES` with the validator's own values, which is how `pr view`
  # ended up answering 141 for every number, and how the patch case worked by luck.
  cat >"$dir/gh" <<'GH'
#!/bin/sh
here=$(dirname "$0")
fixtures=$(cat "$here/fixtures")
case "$1:$2" in
  --version:*)
    echo "gh version 2.45.0 (2025-07-18)"
    exit 0
    ;;
  auth:status)
    echo "github.com" >&2
    echo "  ✓ Logged in to github.com account tester (keyring)" >&2
    echo "  - Token scopes: 'gist', 'read:org', 'repo'" >&2
    exit 0
    ;;
  pr:list)
    if [ "$(cat "$here/fail_list")" = "1" ]; then
      echo "could not resolve host: github.com" >&2
      exit 1
    fi
    cat "$fixtures/pr-list.json"
    exit 0
    ;;
  pr:view)
    # Answer for the number that was asked for: otherwise `:pr 138` opens a detail
    # saying 141, and the diff that follows is the wrong one.
    number=$(printf '%s' "$*" | sed -n 's/.*view \([0-9][0-9]*\).*/\1/p')
    sed "s/\"number\": 141/\"number\": ${number:-141}/" "$fixtures/pr-view.json"
    exit 0
    ;;
  pr:diff)
    # Pull request 138 has a long diff, so the screen checks have something that
    # actually overflows the pane.
    case "$*" in
      *138*) cat "$fixtures/pr-diff-large.patch" ;;
      *) cat "$fixtures/pr-diff.patch" ;;
    esac
    exit 0
    ;;
  api:*)
    case "$*" in
      *graphql*) cat "$fixtures/graphql-count.json" ;;
      *) printf '[]' ;;
    esac
    exit 0
    ;;
esac
echo "fake gh: unexpected call: $*" >&2
exit 1
GH
  chmod +x "$dir/gh"
  printf '%s' "$FIXTURES" >"$dir/fixtures"
  printf '%s' "$fail_list" >"$dir/fail_list"
}

# Runs the interface in a pty, sends key groups and waits for each group's effect
# before sending the next, then prints the final screen.
#
# `keys` and `waits` are parallel `~`-separated lists: the driver sends keys[i],
# polls the replayed screen until waits[i] appears, and moves on. An empty wait
# just settles briefly. The full capture still lands in `log`, so the transient
# popups that `:q` closes can be replayed from it (see `shown` in analysis.sh).
run_tui() {
  local home="$1" keys="$2" waits="$3" fake="$4" log="$5"
  local driver_code=0
  PATH="$fake:$PATH" SMART_REVIEW_HOME="$home" \
    python3 "$ROOT/scripts/validate/drive.py" \
      --cols 160 --rows 40 --log "$log" \
      --ready "Add retry to the webhook dispatcher" \
      --keys "$keys" --waits "$waits" -- \
      "$ROOT/$BIN" --repo acme/service || driver_code=$?
  if [ "$driver_code" -ne 0 ]; then
    touch "$TMP/driver.failed"
  fi
  if grep -q 'panicked' "$log" 2>/dev/null; then
    printf '  note: the interface panicked; see %s\n' "$log"
  fi
  return "$driver_code"
}

# Matches text on the reconstructed screen, ignoring the padding between columns.
saw() { grep -q "$1"; }

# The screen checks need Python's PTY support and replay helper. Missing Python is a
# skip with a reason, not a crash half way through the run.
HAVE_PTY=0
if ! command -v python3 >/dev/null 2>&1; then
  printf 'note: python3 not found; the screen checks will be skipped\n'
else
  HAVE_PTY=1
fi

# ---------------------------------------------------------------------------
step "1/6 formatting, lints, tests"
TESTS_PASSED=0
if [ "${SMART_REVIEW_SKIP_CARGO:-0}" = "1" ]; then
  printf '  SKIP  shared formatting, lint and test gates already passed\n'
  TESTS_PASSED=1
else
  if cargo fmt --all --check >/dev/null 2>&1; then
    ok "cargo fmt --check"
  else
    bad "cargo fmt --check"
  fi

  if cargo clippy --all-targets --all-features -- -D warnings >"$TMP/pull-requests-clippy.log" 2>&1; then
    ok "cargo clippy -- -D warnings"
  else
    bad "cargo clippy -- -D warnings"
    tail -20 "$TMP/pull-requests-clippy.log"
  fi

  if cargo test --all-features >"$TMP/pull-requests-test.log" 2>&1; then
    ok "cargo test --all-features"
    TESTS_PASSED=1
  else
    bad "cargo test --all-features"
    grep -E '^test .* FAILED|panicked' "$TMP/pull-requests-test.log" | head -10
  fi
fi

# ---------------------------------------------------------------------------
step "2/6 the diff parser handles the fixture's awkward cases"
if [ "$TESTS_PASSED" = "1" ]; then
  ok "the full test gate includes parser cases (renames, binary, mode-only, submodule, CRLF)"
elif cargo test --all-features --lib domain::diff >"$TMP/pull-requests-parser.log" 2>&1; then
  ok "the parser's focused tests pass despite another test failure"
else
  bad "the parser's unit tests fail"
  grep -E '^test .* FAILED' "$TMP/pull-requests-parser.log" | head -5
fi

# ---------------------------------------------------------------------------
step "3/6 build"
if [ "${SMART_REVIEW_SKIP_CARGO:-0}" = "1" ]; then
  printf '  SKIP  the parent validator already built the debug binary\n'
elif cargo build >"$TMP/pull-requests-build.log" 2>&1; then
  ok "cargo build"
else
  bad "cargo build"
  tail -20 "$TMP/pull-requests-build.log"
fi

# ---------------------------------------------------------------------------
step "4/6 environment detection without a repository or a forge"
DETECT_HOME="$TMP/detect"
mkdir -p "$DETECT_HOME" "$TMP/not-a-repo"

# Outside a clone and without `--repo`, the failure is specific and actionable.
set +e
(cd "$TMP/not-a-repo" && SMART_REVIEW_HOME="$DETECT_HOME" "$ROOT/$BIN" --check) \
  >"$TMP/pull-requests-check.log" 2>&1
CHECK_CODE=$?
set -e

if [ "$CHECK_CODE" -ne 0 ]; then
  ok "--check exits non-zero when the environment is unusable"
else
  bad "--check exited 0 outside a repository"
fi

if grep -q "not a git repository" "$TMP/pull-requests-check.log"; then
  ok "the report says the directory is not a repository"
else
  bad "the report did not name the first failure"
  head -20 "$TMP/pull-requests-check.log"
fi

if grep -q -- "--repo" "$TMP/pull-requests-check.log"; then
  ok "the report offers the next step"
else
  bad "the report offered no next step"
fi

# A `gh` that does not exist is its own failure, with its own next step.
mkdir -p "$TMP/nogh"
cat >"$TMP/nogh/config.toml" <<'TOML'
[forge]
gh_path = "/nonexistent/gh-for-pull-requests-validation"
TOML
set +e
SMART_REVIEW_HOME="$TMP/nogh" "$ROOT/$BIN" --check --repo acme/service >"$TMP/pull-requests-nogh.log" 2>&1
NOGH_CODE=$?
set -e

if grep -q "cli.github.com" "$TMP/pull-requests-nogh.log"; then
  ok "a missing gh is reported with where to install it"
else
  bad "a missing gh was not reported usefully"
  head -20 "$TMP/pull-requests-nogh.log"
fi

if [ "$NOGH_CODE" -eq 2 ]; then
  ok "an unusable environment exits 2"
else
  bad "expected exit 2 for a missing gh, got $NOGH_CODE"
fi

# ---------------------------------------------------------------------------
step "5/6 the list, the diff and the filters on screen"

if [ "$HAVE_PTY" -eq 0 ]; then
  printf '  SKIP  no pty tool available (needs GNU script, python3 and timeout)\n'
else
  FAKE="$TMP/fake-gh"
  make_fake_gh "$FAKE"

  HOME_LIST="$TMP/home-list"
  SCREEN="$(run_tui "$HOME_LIST" ':q\r' '' "$FAKE" "$TMP/pull-requests-list.log")"

  # The list is the headline feature: rows, markers and an honest count.
  if printf '%s' "$SCREEN" | grep -q "Add retry to the webhook dispatcher"; then
    ok "the pull request list renders"
  else
    bad "the list did not render"
    printf '%s\n' "$SCREEN" | tail -25
  fi

  if printf '%s' "$SCREEN" | saw "showing 3 of 3"; then
    ok "the status line says how many are shown and how many exist"
  else
    bad "the status line did not report the count"
  fi

  if printf '%s' "$SCREEN" | grep -q "acme/service"; then
    ok "the detected repository is named"
  else
    bad "the repository was not named"
  fi

  # The cache is written during that run, so the next check can rely on it.
  if [ -n "$(find "$HOME_LIST/cache" -name '*.json' 2>/dev/null | head -1)" ]; then
    ok "the list was cached on disk"
  else
    bad "nothing was cached"
  fi

  # `:filter is:all` re-asks GitHub with a different query, and the chips change.
  HOME_FILTER="$TMP/home-filter"
  SCREEN="$(run_tui "$HOME_FILTER" ':filter is:all\r~:q\r' '\[is:all\]~' "$FAKE" "$TMP/pull-requests-filter.log")"
  if printf '%s' "$SCREEN" | saw "\[is:all\]"; then
    ok "a filter becomes a visible chip"
  else
    bad "the filter chip did not appear"
    printf '%s\n' "$SCREEN" | tail -20
  fi

  # `/` filters what has been fetched, without asking GitHub again.
  HOME_SEARCH="$TMP/home-search"
  SCREEN="$(run_tui "$HOME_SEARCH" '/dependabot\r~:q\r' 'matching 1 of 3 loaded~' "$FAKE" "$TMP/pull-requests-search.log")"
  if printf '%s' "$SCREEN" | saw "matching 1 of 3 loaded"; then
    ok "the client-side search narrows the fetched list"
  else
    bad "the search did not narrow the list"
    printf '%s\n' "$SCREEN" | tail -20
  fi

  # Enter opens the selected pull request: its diff comes from `gh pr diff`.
  HOME_DIFF="$TMP/home-diff"
  SCREEN="$(run_tui "$HOME_DIFF" '\r~' 'impl Invoice~' "$FAKE" "$TMP/pull-requests-diff.log")"
  if printf '%s' "$SCREEN" | grep -q "impl Invoice"; then
    ok "opening a pull request shows its diff"
  else
    bad "the diff did not render"
    printf '%s\n' "$SCREEN" | tail -25
  fi

  # The awkward cases in the fixture all get their own row rather than a blank.
  if printf '%s' "$SCREEN" | grep -q "binary file, not shown"; then
    ok "a binary file says so instead of showing nothing"
  else
    bad "the binary placeholder is missing"
  fi

  if printf '%s' "$SCREEN" | grep -q "mode changed"; then
    ok "a mode-only change says so"
  else
    bad "the mode-only placeholder is missing"
  fi

  if printf '%s' "$SCREEN" | grep -q "patch (4)"; then
    ok "the file tree lists every changed file, rename included"
  else
    bad "the file tree did not list the files"
  fi

  # The diff is navigable: `}` moves to the next file, `]c` to the next hunk.
  # `}` moves to the next file banner and `j` moves a line, so the status line's
  # file label is what proves the cursor travelled.
  HOME_NAV="$TMP/home-nav"
  SCREEN="$(run_tui "$HOME_NAV" '\r~}}~:q\r' 'impl Invoice~M scripts/build\.sh~' "$FAKE" "$TMP/pull-requests-nav.log")"
  if printf '%s' "$SCREEN" | saw "M scripts/build.sh"; then
    ok "file navigation moves the cursor and the status line names the file"
  else
    bad "navigation did not move the cursor"
    printf '%s\n' "$SCREEN" | tail -8
  fi

  HOME_HUNK="$TMP/home-hunk"
  # Hunk movement is synchronous and changes only cell styling when both hunks already
  # fit. Settle that key, then assert the final selected hunk instead of accepting the
  # stale `impl Billing` text that was visible before `]c` (IR-15).
  SCREEN="$(run_tui "$HOME_HUNK" '\r~]c~:q\r' 'impl Invoice~~' "$FAKE" "$TMP/pull-requests-hunk.log")"
  if printf '%s' "$SCREEN" | saw "impl Billing"; then
    ok "hunk navigation reaches the next hunk"
  else
    bad "hunk navigation did not reach the second hunk"
    printf '%s\n' "$SCREEN" | tail -8
  fi

  # The wheel scrolls the *text*. Opening a PR whose diff is longer than the pane and
  # rolling down must move what is displayed: a wheel that only walks a selection down
  # the screen looks broken, and it looked broken here twice.
  HOME_WHEEL="$TMP/home-wheel"
  SCREEN="$(run_tui "$HOME_WHEEL" ':pr 138\r' 'line 001 of the invoice' "$FAKE" "$TMP/pull-requests-wheel-before.log")"
  if printf '%s' "$SCREEN" | saw "line 001 of the invoice"; then
    ok "a long diff starts at the top"
  else
    bad "the long diff did not render from the top"
    printf '%s\n' "$SCREEN" | tail -8
  fi

  HOME_WHEEL2="$TMP/home-wheel2"
  SCREEN="$(run_tui "$HOME_WHEEL2" ':pr 138\r~\033[<65;60;12M\033[<65;60;12M\033[<65;60;12M' 'line 001 of the invoice~' "$FAKE" "$TMP/pull-requests-wheel.log")"
  if printf '%s' "$SCREEN" | saw "line 001 of the invoice"; then
    bad "the wheel did not scroll the diff"
    printf '%s\n' "$SCREEN" | tail -8
  else
    ok "the wheel scrolls the diff text"
  fi

  # And a click lands on the row it was aimed at: the second file in the tree.
  HOME_CLICK="$TMP/home-click"
  SCREEN="$(run_tui "$HOME_CLICK" ':pr 141\r~\033[<0;8;5M' 'impl Invoice~A docs/logo\.png' "$FAKE" "$TMP/pull-requests-click.log")"
  if printf '%s' "$SCREEN" | saw "A docs/logo.png"; then
    ok "a click on a tree row opens that file"
  else
    bad "the click did not open the file under the pointer"
    printf '%s\n' "$SCREEN" | tail -8
  fi

  # `:copy-path` writes the OSC 52 sequence, which *is* the feature: a path on the
  # clipboard with no clipboard dependency (FR-3.4).
  HOME_COPY="$TMP/home-copy"
  run_tui "$HOME_COPY" '\r~y~:q\r' 'impl Invoice~copied~' "$FAKE" "$TMP/pull-requests-copy.log" >/dev/null
  if grep -q ']52;c;' "$TMP/pull-requests-copy.log"; then
    ok ":copy-path puts the file path on the terminal clipboard"
  else
    bad ":copy-path wrote no OSC 52 sequence"
  fi

  # A signal must give the terminal back (NFR-4.2). The PTY driver signals the exact
  # child process as soon as the list is visible; a broad `pkill` was slow and could
  # terminate another validator running concurrently.
  HOME_SIGNAL="$TMP/home-signal"
  set +e
  PATH="$FAKE:$PATH" SMART_REVIEW_HOME="$HOME_SIGNAL" \
    python3 "$ROOT/scripts/validate/drive.py" \
      --cols 160 --rows 40 --log "$TMP/pull-requests-signal.log" \
      --ready "Add retry to the webhook dispatcher" -- \
      "$ROOT/$BIN" --repo acme/service >/dev/null
  SIGNAL_CODE=$?
  set -e

  if [ "$SIGNAL_CODE" -eq 0 ] \
     && grep -q 'restoring the terminal' "$HOME_SIGNAL/logs/smart-review.log" 2>/dev/null; then
    ok "SIGTERM restores the terminal"
  else
    bad "SIGTERM did not restore the terminal"
  fi

  # The cache-first path: the same home, but the list call now fails, so what is
  # shown must come from the cache with an honest offline marker (FR-2.3, DEC-14).
  FAKE_FAILING="$TMP/fake-gh-offline"
  make_fake_gh "$FAKE_FAILING" 1
  # The first key group is empty: it waits for the offline state to be reached, and only
  # then quits. Quitting immediately is a race — the cached page is painted before the
  # fetch is even attempted, so the indicator the check wants appears *after* the first
  # frame, and an app that has already exited never reaches it.
  SCREEN="$(run_tui "$HOME_LIST" '~:q\r' 'offline~' "$FAKE_FAILING" "$TMP/pull-requests-offline.log")"
  if printf '%s' "$SCREEN" | grep -q "Add retry to the webhook dispatcher"; then
    ok "a cached list is shown when the network is gone"
  else
    bad "the cached list was not used"
    printf '%s\n' "$SCREEN" | tail -20
  fi

  if printf '%s' "$SCREEN" | grep -q "offline"; then
    ok "the offline state is visible rather than silent"
  else
    bad "the offline indicator is missing"
  fi
fi

if [ -f "$TMP/driver.failed" ]; then
  bad "one or more PTY steps did not reach their expected screen state"
fi

# ---------------------------------------------------------------------------
step "6/6 nothing was written inside the repository"
AFTER="$(git status --porcelain --ignored=no | sort)"
if [ "$BEFORE" = "$AFTER" ]; then
  ok "the validation run wrote nothing into the repository"
else
  bad "the validation run modified the repository"
  diff <(printf '%s\n' "$BEFORE") <(printf '%s\n' "$AFTER") | head -10
fi

if [ -d "$ROOT/target" ]; then
  ok "build output stays in target/"
fi

# The application owns its own home; nothing may appear in the checkout (FR-8.1).
if [ ! -e "$ROOT/.smart-review" ]; then
  ok "no application state was created in the checkout"
else
  bad "the application wrote state into the checkout"
fi

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
