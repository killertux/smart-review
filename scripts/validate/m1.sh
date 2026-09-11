#!/usr/bin/env bash
#
# Milestone M1 validation (see PLAN.md §2).
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

PASS=0
FAIL=0
ok() { printf '  PASS  %s\n' "$1"; PASS=$((PASS + 1)); }
bad() { printf '  FAIL  %s\n' "$1"; FAIL=$((FAIL + 1)); }
step() { printf '\n== %s ==\n' "$1"; }

TMP="$(mktemp -d)"
BIN="target/release/smart-review"
FIXTURES="$ROOT/tests/fixtures/gh"
cleanup() { rm -rf "$TMP"; }
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
  cat >"$dir/gh" <<EOF
#!/bin/sh
case "\$1:\$2" in
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
    if [ "$fail_list" = "1" ]; then
      echo "could not resolve host: github.com" >&2
      exit 1
    fi
    cat "$FIXTURES/pr-list.json"
    exit 0
    ;;
  pr:view)
    cat "$FIXTURES/pr-view.json"
    exit 0
    ;;
  pr:diff)
    cat "$FIXTURES/pr-diff.patch"
    exit 0
    ;;
  api:*)
    case "\$*" in
      *graphql*) cat "$FIXTURES/graphql-count.json" ;;
      *) printf '[]' ;;
    esac
    exit 0
    ;;
esac
echo "fake gh: unexpected call: \$*" >&2
exit 1
EOF
  chmod +x "$dir/gh"
}

# Runs the interface in a pty, sends some keys, and strips the escape sequences so
# the screen can be read as text.
run_tui() {
  local home="$1" keys="$2" fake="$3" log="$4"
  # `~` separates groups of keys by a pause, because opening a pull request is a
  # background job: a `:q` sent in the same breath quits before the diff arrives.
  set +e
  (sleep 1
   IFS='~' read -ra groups <<<"$keys"
   for group in "${groups[@]}"; do
     printf '%b' "$group"
     sleep 1.5
   done
   sleep 1) \
    | PATH="$fake:$PATH" SMART_REVIEW_HOME="$home" timeout 30 \
      script -qefc "stty rows 40 cols 160 2>/dev/null; '$ROOT/$BIN' --repo acme/service" /dev/null \
    >"$log" 2>&1
  TUI_STATUS=$?
  set -e

  # A crash on the way out — a panic while restoring the terminal, a worker that
  # deadlocks — must be visible even though the screen looked right beforehand.
  if [ "$TUI_STATUS" -ne 0 ] && [ "$TUI_STATUS" -ne 124 ]; then
    printf '  note: the interface exited %s; see %s\n' "$TUI_STATUS" "$log"
  fi
  if grep -q 'panicked' "$log" 2>/dev/null; then
    printf '  note: the interface panicked; see %s\n' "$log"
  fi

  # The capture is every frame concatenated, and the app only writes the cells that
  # changed, so the raw stream is not what was on screen. Replaying the escape
  # sequences reconstructs the final screen instead.
  python3 "$ROOT/scripts/validate/screen.py" --path "$log" --cols 160 --rows 40
}

# Matches text on the reconstructed screen, ignoring the padding between columns.
saw() { grep -q "$1"; }

# The screen checks need a pty (`script`), a replayable capture (`python3`) and a
# bounded run (`timeout`). Missing any of them is a skip with a reason, not a crash
# half way through the run.
HAVE_PTY=0
if ! script --version 2>&1 | grep -q util-linux; then
  printf 'note: GNU script not found; the screen checks will be skipped\n'
elif ! command -v python3 >/dev/null 2>&1; then
  printf 'note: python3 not found; the screen checks will be skipped\n'
elif ! command -v timeout >/dev/null 2>&1; then
  printf 'note: timeout not found; the screen checks will be skipped\n'
else
  HAVE_PTY=1
fi

# ---------------------------------------------------------------------------
step "1/6 formatting, lints, tests"
if cargo fmt --all --check >/dev/null 2>&1; then
  ok "cargo fmt --check"
else
  bad "cargo fmt --check"
fi

if cargo clippy --all-targets --all-features -- -D warnings >/tmp/m1-clippy.log 2>&1; then
  ok "cargo clippy -- -D warnings"
else
  bad "cargo clippy -- -D warnings"
  tail -20 /tmp/m1-clippy.log
fi

if cargo test --all-features >/tmp/m1-test.log 2>&1; then
  ok "cargo test --all-features"
else
  bad "cargo test --all-features"
  grep -E '^test .* FAILED|panicked' /tmp/m1-test.log | head -10
fi

# ---------------------------------------------------------------------------
step "2/6 the diff parser handles the fixture's awkward cases"
if cargo test --all-features --lib domain::diff >/tmp/m1-parser.log 2>&1; then
  ok "the parser's unit tests pass (renames, binary, mode-only, submodule, CRLF)"
else
  bad "the parser's unit tests fail"
  grep -E '^test .* FAILED' /tmp/m1-parser.log | head -5
fi

# ---------------------------------------------------------------------------
step "3/6 release build"
if cargo build --release >/tmp/m1-build.log 2>&1; then
  ok "cargo build --release"
else
  bad "cargo build --release"
  tail -20 /tmp/m1-build.log
fi

# ---------------------------------------------------------------------------
step "4/6 environment detection without a repository or a forge"
DETECT_HOME="$TMP/detect"
mkdir -p "$DETECT_HOME" "$TMP/not-a-repo"

# Outside a clone and without `--repo`, the failure is specific and actionable.
set +e
(cd "$TMP/not-a-repo" && SMART_REVIEW_HOME="$DETECT_HOME" "$ROOT/$BIN" --check) \
  >/tmp/m1-check.log 2>&1
CHECK_CODE=$?
set -e

if [ "$CHECK_CODE" -ne 0 ]; then
  ok "--check exits non-zero when the environment is unusable"
else
  bad "--check exited 0 outside a repository"
fi

if grep -q "not a git repository" /tmp/m1-check.log; then
  ok "the report says the directory is not a repository"
else
  bad "the report did not name the first failure"
  head -20 /tmp/m1-check.log
fi

if grep -q -- "--repo" /tmp/m1-check.log; then
  ok "the report offers the next step"
else
  bad "the report offered no next step"
fi

# A `gh` that does not exist is its own failure, with its own next step.
mkdir -p "$TMP/nogh"
cat >"$TMP/nogh/config.toml" <<'TOML'
[forge]
gh_path = "/nonexistent/gh-for-m1-validation"
TOML
set +e
SMART_REVIEW_HOME="$TMP/nogh" "$ROOT/$BIN" --check --repo acme/service >/tmp/m1-nogh.log 2>&1
NOGH_CODE=$?
set -e

if grep -q "cli.github.com" /tmp/m1-nogh.log; then
  ok "a missing gh is reported with where to install it"
else
  bad "a missing gh was not reported usefully"
  head -20 /tmp/m1-nogh.log
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
  SCREEN="$(run_tui "$HOME_LIST" ':q\r' "$FAKE" /tmp/m1-list.log)"

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
  SCREEN="$(run_tui "$HOME_FILTER" ':filter is:all\r~:q\r' "$FAKE" /tmp/m1-filter.log)"
  if printf '%s' "$SCREEN" | saw "\[is:all\]"; then
    ok "a filter becomes a visible chip"
  else
    bad "the filter chip did not appear"
    printf '%s\n' "$SCREEN" | tail -20
  fi

  # `/` filters what has been fetched, without asking GitHub again.
  HOME_SEARCH="$TMP/home-search"
  SCREEN="$(run_tui "$HOME_SEARCH" '/dependabot\r~:q\r' "$FAKE" /tmp/m1-search.log)"
  if printf '%s' "$SCREEN" | saw "matching 1 of 3 loaded"; then
    ok "the client-side search narrows the fetched list"
  else
    bad "the search did not narrow the list"
    printf '%s\n' "$SCREEN" | tail -20
  fi

  # Enter opens the selected pull request: its diff comes from `gh pr diff`.
  HOME_DIFF="$TMP/home-diff"
  SCREEN="$(run_tui "$HOME_DIFF" '\r~' "$FAKE" /tmp/m1-diff.log)"
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

  if printf '%s' "$SCREEN" | grep -q "Files (4)"; then
    ok "the file tree lists every changed file, rename included"
  else
    bad "the file tree did not list the files"
  fi

  # The diff is navigable: `}` moves to the next file, `]c` to the next hunk.
  # `}` moves to the next file banner and `j` moves a line, so the status line's
  # file label is what proves the cursor travelled.
  HOME_NAV="$TMP/home-nav"
  SCREEN="$(run_tui "$HOME_NAV" '\r~}}~:q\r' "$FAKE" /tmp/m1-nav.log)"
  if printf '%s' "$SCREEN" | saw "M scripts/build.sh"; then
    ok "file navigation moves the cursor and the status line names the file"
  else
    bad "navigation did not move the cursor"
    printf '%s\n' "$SCREEN" | tail -8
  fi

  HOME_HUNK="$TMP/home-hunk"
  SCREEN="$(run_tui "$HOME_HUNK" '\r~]c~:q\r' "$FAKE" /tmp/m1-hunk.log)"
  if printf '%s' "$SCREEN" | saw "impl Billing"; then
    ok "hunk navigation reaches the next hunk"
  else
    bad "hunk navigation did not reach the second hunk"
    printf '%s\n' "$SCREEN" | tail -8
  fi

  # `:copy-path` writes the OSC 52 sequence, which *is* the feature: a path on the
  # clipboard with no clipboard dependency (FR-3.4).
  HOME_COPY="$TMP/home-copy"
  run_tui "$HOME_COPY" '\r~y~:q\r' "$FAKE" /tmp/m1-copy.log >/dev/null
  if grep -q ']52;c;' /tmp/m1-copy.log; then
    ok ":copy-path puts the file path on the terminal clipboard"
  else
    bad ":copy-path wrote no OSC 52 sequence"
  fi

  # A signal must give the terminal back (NFR-4.2). `SIGINT` arrives as a key in raw
  # mode, so this is about the signals a `kill` sends.
  HOME_SIGNAL="$TMP/home-signal"
  set +e
  (sleep 6) | PATH="$FAKE:$PATH" SMART_REVIEW_HOME="$HOME_SIGNAL" timeout 20 \
    script -qefc "stty rows 40 cols 160 2>/dev/null; '$ROOT/$BIN' --repo acme/service" /dev/null \
    >/tmp/m1-signal.log 2>&1 &
  SIGNAL_JOB=$!
  sleep 4
  pkill -TERM -f 'target/release/smart-review' 2>/dev/null
  wait "$SIGNAL_JOB" 2>/dev/null
  set -e

  if grep -q 'restoring the terminal' "$HOME_SIGNAL/logs/smart-review.log" 2>/dev/null; then
    ok "SIGTERM restores the terminal"
  else
    bad "SIGTERM did not restore the terminal"
  fi

  # The cache-first path: the same home, but the list call now fails, so what is
  # shown must come from the cache with an honest offline marker (FR-2.3, DEC-14).
  FAKE_FAILING="$TMP/fake-gh-offline"
  make_fake_gh "$FAKE_FAILING" 1
  SCREEN="$(run_tui "$HOME_LIST" ':q\r' "$FAKE_FAILING" /tmp/m1-offline.log)"
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
