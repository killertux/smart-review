#!/usr/bin/env bash
#
# M2a validation: the workspace, the model catalog, the credentials file and the
# picker (FR-3.1, FR-3.2, FR-4.5, FR-4.7, FR-4.8).
#
# The catalog is served by a local HTTP server rather than models.dev, so the checks
# are offline and deterministic, and so that one of them can be "a provider this build
# cannot reach is hidden". An optional last step runs against the *published* feed,
# because that is what actually broke the picker once: a single provider publishing
# `"min": -1` made the whole document unreadable. The normal suite remains offline.
#
# Usage: scripts/validate/m2a.sh
#        KEEP=1 keeps temporary files; SMART_REVIEW_LIVE_TESTS=1 also probes models.dev.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

PASS=0
FAIL=0
ok() { printf '  PASS  %s\n' "$1"; PASS=$((PASS + 1)); }
bad() { printf '  FAIL  %s\n' "$1"; FAIL=$((FAIL + 1)); }
step() { printf '\n== %s ==\n' "$1"; }

TMP="$(mktemp -d)"
BIN="target/debug/smart-review"
FIXTURES="$ROOT/tests/fixtures/gh"
MODELS="$ROOT/tests/fixtures/models/providers.json"

SERVER_PID=""
cleanup() {
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null
  # `KEEP=1` leaves the temporary directory behind, for when a check fails and the
  # reason is in the app's log rather than on the screen.
  if [ "${KEEP:-0}" = "1" ]; then
    printf '  note: kept %s\n' "$TMP"
  else
    rm -rf "$TMP"
  fi
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# A catalog server on a port nobody else is using.
# ---------------------------------------------------------------------------
PORT="$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)"
mkdir -p "$TMP/srv"
cp "$MODELS" "$TMP/srv/api.json"
( cd "$TMP/srv" && exec python3 -m http.server "$PORT" --bind 127.0.0.1 ) >"$TMP/server.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 40); do
  if python3 - "$PORT" <<'PY' 2>/dev/null
import sys, urllib.request
urllib.request.urlopen(f"http://127.0.0.1:{sys.argv[1]}/api.json", timeout=1).read(16)
PY
  then break; fi
  sleep 0.25
done

CATALOG_URL="http://127.0.0.1:$PORT/api.json"

# ---------------------------------------------------------------------------
# A fake `gh`, as in m1.sh: a quoted heredoc, and data files read from its own
# directory so no check ever rewrites a committed fixture.
# ---------------------------------------------------------------------------
make_fake_gh() {
  local dir="$1"
  mkdir -p "$dir"
  cat >"$dir/gh" <<'GH'
#!/bin/sh
here=$(dirname "$0")
fixtures=$(cat "$here/fixtures")
case "$1:$2" in
  --version:*) echo "gh version 2.45.0 (2025-07-18)"; exit 0 ;;
  auth:status)
    echo "github.com" >&2
    echo "  ✓ Logged in to github.com account tester (keyring)" >&2
    exit 0 ;;
  pr:list) cat "$fixtures/pr-list.json"; exit 0 ;;
  pr:view)
    number=$(printf '%s' "$*" | sed -n 's/.*view \([0-9][0-9]*\).*/\1/p')
    sed "s/\"number\": 141/\"number\": ${number:-141}/" "$here/view.json"
    exit 0 ;;
  pr:diff) cat "$fixtures/pr-diff.patch"; exit 0 ;;
  api:*)
    case "$*" in
      *graphql*) cat "$fixtures/graphql-count.json" ;;
      *) printf '[]' ;;
    esac
    exit 0 ;;
esac
echo "fake gh: unexpected call: $*" >&2
exit 1
GH
  chmod +x "$dir/gh"
  cp "$FIXTURES/pr-view.json" "$dir/view.json"
  printf '%s' "$FIXTURES" >"$dir/fixtures"
}

make_home() {
  local home="$1"
  mkdir -p "$home"
  cat >"$home/config.toml" <<EOF
[ui]
theme = "dark"

[catalog]
url = "$CATALOG_URL"
ttl_hours = 24
EOF
}

# Keys are sent in groups separated by `~`, each followed by a wait for the group's
# effect to appear on screen. The drive is the same as m1.sh's: see the comment there.
run_tui() {
  local home="$1" keys="$2" waits="$3" fake="$4" log="$5" step_timeout="${6:-15}"
  local driver_code=0
  PATH="$fake:$PATH" SMART_REVIEW_HOME="$home" \
    python3 "$ROOT/scripts/validate/drive.py" \
      --cols 160 --rows 40 --log "$log" \
      --ready "Add retry to the webhook dispatcher" \
      --step-timeout "$step_timeout" \
      --keys "$keys" --waits "$waits" -- \
      "$ROOT/$BIN" --repo acme/service ${EXTRA_ARGS:-} || driver_code=$?
  if [ "$driver_code" -ne 0 ]; then
    touch "$TMP/driver.failed"
  fi
  if [ -f "$home/logs/smart-review.log" ] && grep -q 'panicked' "$home/logs/smart-review.log"; then
    printf '  note: the interface panicked; see %s\n' "$home/logs/smart-review.log"
  fi
}

saw() { grep -q "$1"; }

# Whether a pattern was ever on screen, even for one frame. The final screen is not
# enough for anything that expires or that the next keypress takes away — a
# confirmation is both, and a notice is the first.
shown() {
  python3 "$ROOT/scripts/validate/screen.py" --cols 160 --rows 40 \
    --path "$1" --when "$2" >/dev/null 2>&1
}

step "1/7 build"
if [ "${SMART_REVIEW_SKIP_CARGO:-0}" = "1" ]; then
  printf '  SKIP  the parent validator already built the debug binary\n'
elif cargo build --quiet 2>"$TMP/build.log"; then
  ok "the debug binary builds"
else
  bad "the debug binary does not build"
  sed -n '1,20p' "$TMP/build.log"
  printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
  exit 1
fi

FAKE="$TMP/fake"
make_fake_gh "$FAKE"

step "2/7 the catalog adapter"
HOME_CATALOG="$TMP/home-catalog"
make_home "$HOME_CATALOG"
SCREEN="$(run_tui "$HOME_CATALOG" ' m~' 'providers,~' "$FAKE" "$TMP/catalog.log")"

if printf '%s' "$SCREEN" | saw "providers,"; then
  ok "the catalog is fetched and summarised"
else
  bad "the catalog summary never appeared"
  printf '%s\n' "$SCREEN" | tail -6
fi

if [ -f "$HOME_CATALOG/cache/models.json" ]; then
  ok "the catalog is cached on disk"
else
  bad "the catalog was not cached"
fi

if grep -q '"fetched_at"' "$HOME_CATALOG/cache/models.json" 2>/dev/null; then
  ok "the cache records when it was fetched"
else
  bad "the cache has no timestamp"
fi

step "3/7 the picker"
HOME_PICKER="$TMP/home-picker"
make_home "$HOME_PICKER"
SCREEN="$(run_tui "$HOME_PICKER" ' m~' 'providers,~' "$FAKE" "$TMP/picker.log")"

for expected in "DeepSeek" "OpenRouter" "Anthropic"; do
  if printf '%s' "$SCREEN" | saw "$expected"; then
    ok "the picker lists $expected"
  else
    bad "the picker does not list $expected"
  fi
done

if printf '%s' "$SCREEN" | saw "watsonx\|Watsonx"; then
  bad "the picker offers a provider this build cannot reach"
else
  ok "a provider this build cannot reach is hidden"
fi

if printf '%s' "$SCREEN" | saw "openai-compatible"; then
  ok "the passthrough route is shown"
else
  bad "the passthrough route is not shown"
fi

# The fixture contains the published `"min": -1` budget bound, which as a u32 made
# the whole catalog unreadable. Every provider being listed is the check for it.
if printf '%s' "$SCREEN" | saw "unreadable"; then
  bad "some of the fixture could not be read"
else
  ok "the whole fixture is readable"
fi

step "4/7 choosing a model, a key and a thinking mode"
HOME_FLOW="$TMP/home-flow"
make_home "$HOME_FLOW"
# provider (filtered) → model (filtered) → thinking "on" → key → Enter.
SCREEN="$(run_tui "$HOME_FLOW" \
  ' m~deep\r~pro\r~\r~sk-validate-key~:q\r' \
  'providers,~~~~~deepseek/deepseek-v4-pro' "$FAKE" "$TMP/flow.log")"

if grep -q 'provider = "deepseek"' "$HOME_FLOW/config.toml" 2>/dev/null; then
  ok "the chosen provider is written to config.toml"
else
  bad "config.toml does not name the chosen provider"
  printf '  --- config.toml\n'
  sed -n '1,20p' "$HOME_FLOW/config.toml" 2>/dev/null
fi
if grep -q 'model = "deepseek-v4-pro"' "$HOME_FLOW/config.toml" 2>/dev/null; then
  ok "the chosen model is written to config.toml"
else
  bad "config.toml does not name the chosen model"
fi
if grep -q 'reasoning' "$HOME_FLOW/config.toml" 2>/dev/null; then
  ok "the thinking setting is written to config.toml"
else
  bad "the thinking setting is missing from config.toml"
fi

if [ -f "$HOME_FLOW/credentials.toml" ]; then
  ok "the key is stored in credentials.toml"
else
  bad "the key was not stored"
fi
MODE="$(ls -l "$HOME_FLOW/credentials.toml" 2>/dev/null | cut -c1-10)"
if [ "$MODE" = "-rw-------" ]; then
  ok "credentials.toml is mode 0600"
else
  bad "credentials.toml has mode $MODE"
fi
if grep -q 'sk-validate-key' "$HOME_FLOW/config.toml" 2>/dev/null \
   || grep -q 'sk-validate-key' "$HOME_FLOW/state.toml" 2>/dev/null; then
  bad "the key was written outside credentials.toml"
else
  ok "the key appears in no other file"
fi
if grep -rq 'sk-validate-key' "$HOME_FLOW/logs/" 2>/dev/null; then
  bad "the key was logged"
else
  ok "the key was not logged"
fi
if printf '%s' "$SCREEN" | grep -q 'sk-validate-key'; then
  bad "the key was echoed to the screen"
else
  ok "the key is never echoed"
fi

# Comments and unknown keys in the user's config survive the write-back (DEC-19).
HOME_KEEP="$TMP/home-keep"
make_home "$HOME_KEEP"
cat >>"$HOME_KEEP/config.toml" <<'EOF'

# my own notes: this comment must survive
[review]
my_custom_key = "kept"
EOF
run_tui "$HOME_KEEP" ' m~deep\r~pro\r~\r~sk-second-key~:q\r' \
  'providers,~~~~~deepseek/deepseek-v4-pro' "$FAKE" "$TMP/keep.log" >/dev/null
if grep -q 'my own notes' "$HOME_KEEP/config.toml" 2>/dev/null; then
  ok "a comment in config.toml survives the write-back"
else
  bad "the write-back destroyed the user's comments"
  sed -n '1,24p' "$HOME_KEEP/config.toml"
fi
if grep -q 'my_custom_key' "$HOME_KEEP/config.toml" 2>/dev/null; then
  ok "an unknown key survives the write-back"
else
  bad "the write-back destroyed an unknown key"
fi
if [ -f "$HOME_KEEP/config.toml.bak" ]; then
  ok "the previous config is backed up before the write"
else
  bad "no backup was written before changing config.toml"
fi

step "5/7 the worktree"
# A worktree needs a real repository, a real remote and a real pull request ref, so
# this builds them with git and points the app at the clone with `--path`.
HOME_WS="$TMP/home-ws"
make_home "$HOME_WS"
REPO="$TMP/repo"
mkdir -p "$REPO/origin.git"
git init --bare --quiet -b main "$REPO/origin.git"
git clone --quiet "$REPO/origin.git" "$REPO/clone"
git -C "$REPO/clone" config user.email t@example.com
git -C "$REPO/clone" config user.name Test
printf 'pub fn one() {}\n' >"$REPO/clone/src.rs"
git -C "$REPO/clone" add src.rs
git -C "$REPO/clone" commit --quiet -m one
git -C "$REPO/clone" push --quiet origin main
git -C "$REPO/clone" checkout --quiet -b work
printf 'pub fn one() {}\npub fn two() {}\n' >"$REPO/clone/src.rs"
git -C "$REPO/clone" add src.rs
git -C "$REPO/clone" commit --quiet -m two
# Captured before the push: the clone has no ref to resolve afterwards, and
# `git rev-parse` echoes an argument it cannot resolve rather than failing loudly.
PR_SHA="$(git -C "$REPO/clone" rev-parse HEAD)"
git -C "$REPO/clone" push --quiet --force origin HEAD:refs/pull/142/head
git -C "$REPO/clone" checkout --quiet main
git -C "$REPO/clone" branch --quiet -D work
HEAD_BEFORE="$(git -C "$REPO/clone" rev-parse HEAD)"

# The fake `gh` must report the head SHA the fixture repository actually has, or the
# app would compare a workspace against a commit that never existed.
python3 - "$FAKE/view.json" "$PR_SHA" <<'PY'
import json, sys
path, head = sys.argv[1], sys.argv[2]
assert head and len(head) == 40, f"the head SHA did not resolve: {head!r}"
document = json.load(open(path))
document["headRefOid"] = head
document["baseRefName"] = "main"
json.dump(document, open(path, "w"), indent=1)
PY

EXTRA_ARGS="--path $REPO/clone" run_tui "$HOME_WS" '\r~q' 'src\.rs~' "$FAKE" "$TMP/ws.log" >"$TMP/ws.screen"

STORE_KEY="h-6769746875622e636f6d--o-61636d65--r-73657276696365"
WORKTREE="$HOME_WS/worktrees/checkouts/$STORE_KEY/pr-142"
STORE="$HOME_WS/worktrees/git/$STORE_KEY/repo.git"
if [ -d "$WORKTREE" ]; then
  ok "a worktree is created under SMART_REVIEW_HOME"
else
  bad "no worktree was created at $WORKTREE"
fi
if [ -f "$WORKTREE/src.rs" ] && grep -q 'pub fn two' "$WORKTREE/src.rs"; then
  ok "the worktree holds the pull request's code"
else
  bad "the worktree does not hold the pull request's code"
fi
if git -C "$STORE" rev-parse --verify --quiet "refs/smart-review/$STORE_KEY/pr-142/head" >/dev/null; then
  ok "the fetched head is kept in the app-owned namespaced ref"
else
  bad "the head ref was not created"
fi
if [ "$(git -C "$REPO/clone" rev-parse HEAD)" = "$HEAD_BEFORE" ]; then
  ok "the user's checkout did not move"
else
  bad "the user's HEAD moved"
fi
if [ -z "$(git -C "$REPO/clone" status --porcelain)" ]; then
  ok "the user's working tree is still clean"
else
  bad "the user's working tree was touched"
  git -C "$REPO/clone" status --porcelain
fi
if [ ! -e "$REPO/clone/.git/worktrees" ]; then
  ok "the source clone has no app worktree bookkeeping"
else
  bad "the app wrote worktree bookkeeping into the source clone"
fi
if grep -q 'from the worktree' "$HOME_WS/logs/smart-review.log" 2>/dev/null; then
  ok "the diff is read from the worktree"
else
  bad "the diff was never read from the worktree"
  grep -E 'from the|materialised' "$HOME_WS/logs/smart-review.log" | tail -3
fi
if printf '%s' "$(cat "$TMP/ws.screen")" | saw "src.rs"; then
  ok "the review screen shows the worktree's file"
else
  bad "the review screen never showed the worktree's diff"
fi

# Cleaning from a different directory still works because the app-owned object store
# owns both the worktree registration and its refs.
#
# It asks first (FR-6.5): the removal is local and irreversible, so the command opens a
# confirmation and the *second* key is the one that does it. The default answer is
# nothing.
SCREEN="$(run_tui "$HOME_WS" ':workspace clean --all\r~\033~q' 'are you sure~~' "$FAKE" "$TMP/clean-ask.log")"
if shown "$TMP/clean-ask.log" "are you sure"; then
  ok ":workspace clean asks before removing worktrees"
else
  bad "destructive local actions must confirm"
  printf '%s\n' "$SCREEN" | tail -6
fi
if [ -d "$WORKTREE" ]; then
  ok "asking removes nothing on its own"
else
  bad "the worktree was removed before the question was answered"
fi
SCREEN="$(run_tui "$HOME_WS" ':workspace clean --all\r~\r~q' 'are you sure~removed 1 worktree~' "$FAKE" "$TMP/clean.log")"
if printf '%s' "$SCREEN" | saw "removed 1 worktree"; then
  ok ":workspace clean removes the worktree once confirmed"
else
  bad ":workspace clean did not report a removal"
  printf '%s\n' "$SCREEN" | tail -6
fi
if [ -d "$WORKTREE" ]; then
  bad "the worktree directory is still there"
else
  ok "the worktree directory is gone"
fi

step "6/7 offline behaviour"
# Stopping the server proves the cache is what answered, and it happens after every
# check that needs the network rather than in the middle of them.
kill "$SERVER_PID" 2>/dev/null
wait "$SERVER_PID" 2>/dev/null
SERVER_PID=""
SCREEN="$(run_tui "$HOME_CATALOG" ' m~' 'providers,~' "$FAKE" "$TMP/catalog-offline.log")"
if printf '%s' "$SCREEN" | saw "cached"; then
  ok "a cached catalog is used when the server is gone"
else
  bad "the cached catalog was not used offline"
  printf '%s\n' "$SCREEN" | tail -6
fi

step "7/7 the live catalog"
# The published feed is what broke the picker once, so the check that matters runs the
# app against models.dev itself rather than a fixture that agrees with the parser. It is
# deliberately opt-in: the normal suite is hermetic and cannot wait on a third party.
if [ "${SMART_REVIEW_LIVE_TESTS:-0}" = "1" ]; then
  HOME_LIVE="$TMP/home-live"
  mkdir -p "$HOME_LIVE"
  cat >"$HOME_LIVE/config.toml" <<'EOF'
[ui]
theme = "dark"

[catalog]
url = "https://models.dev/api.json"
ttl_hours = 24
EOF
  SCREEN="$(run_tui "$HOME_LIVE" ' m~' 'providers,|could not be fetched~' "$FAKE" "$TMP/live.log" 40)"
  if printf '%s' "$SCREEN" | saw "providers,"; then
    ok "the published catalog is readable"
    if printf '%s' "$SCREEN" | saw "unreadable"; then
      printf '  note: the published catalog contained entries this build could not read\n'
    fi
  elif printf '%s' "$SCREEN" | saw "could not be fetched"; then
    printf '  note: models.dev is unreachable from here; the live-catalog check was skipped\n'
  else
    bad "the published catalog could not be read"
    printf '%s\n' "$SCREEN" | tail -8
  fi
else
  printf '  SKIP  live models.dev probe (set SMART_REVIEW_LIVE_TESTS=1 to opt in)\n'
fi

if [ -f "$TMP/driver.failed" ]; then
  bad "one or more PTY steps did not reach their expected screen state"
fi

printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
