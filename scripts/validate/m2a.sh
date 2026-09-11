#!/usr/bin/env bash
#
# M2a validation: the workspace, the model catalog, the credentials file and the
# picker (FR-3.1, FR-3.2, FR-4.5, FR-4.7, FR-4.8).
#
# The catalog is served by a local HTTP server rather than models.dev, so the checks
# are offline and deterministic, and so one of them can be "the picker hid the
# providers this build cannot reach". The fake `gh` is the same one M1 uses.
#
# Usage: scripts/validate/m2a.sh
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
# A fake `gh`, as in m1.sh: quoted heredoc, data read from its own directory.
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
    # The view is a copy taken by the caller, so the committed fixture is never
    # rewritten by a check.
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

run_tui() {
  local home="$1" keys="$2" fake="$3" log="$4" settle="${5:-1.5}"
  set +e
  (sleep 1.5
   IFS='~' read -ra groups <<<"$keys"
   for group in "${groups[@]}"; do
     printf '%b' "$group"
     sleep "$settle"
   done
   sleep 1) \
    | PATH="$fake:$PATH" SMART_REVIEW_HOME="$home" timeout 30 \
      script -qefc "stty rows 40 cols 160 2>/dev/null; '$ROOT/$BIN' --repo acme/service" /dev/null \
    >"$log" 2>&1
  set -e
  if [ -f "$home/logs/smart-review.log" ] && grep -q 'panicked' "$home/logs/smart-review.log"; then
    printf '  note: the interface panicked; see %s\n' "$home/logs/smart-review.log"
  fi
  python3 "$ROOT/scripts/validate/screen.py" --path "$log" --cols 160 --rows 40
}

saw() { grep -q "$1"; }

step "1/5 build"
if cargo build --release --quiet 2>"$TMP/build.log"; then
  ok "the release binary builds"
else
  bad "the release binary does not build"
  sed -n '1,20p' "$TMP/build.log"
  step "5/5 offline behaviour"
# A second run inside the TTL must not have to fetch: stopping the server proves it.
kill "$SERVER_PID" 2>/dev/null
SERVER_PID=""
sleep 0.5
SCREEN="$(run_tui "$HOME_CATALOG" ' m~' "$FAKE" "$TMP/catalog-offline.log" 3)"
if printf '%s' "$SCREEN" | saw "cached"; then
  ok "a cached catalog is used when the server is gone"
else
  bad "the cached catalog was not used offline"
  printf '%s\n' "$SCREEN" | tail -6
fi


printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
  exit 1
fi

step "2/5 the catalog adapter"
HOME_CATALOG="$TMP/home-catalog"
make_home "$HOME_CATALOG"
FAKE="$TMP/fake"
make_fake_gh "$FAKE"
SCREEN="$(run_tui "$HOME_CATALOG" ' m~\033~:catalog refresh\r~q' "$FAKE" "$TMP/catalog.log" 2.5)"

if printf '%s' "$SCREEN" | saw "providers"; then
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

step "3/5 the picker"
HOME_PICKER="$TMP/home-picker"
make_home "$HOME_PICKER"
# Wait for the first-run picker, which shows the providers the build can reach.
SCREEN="$(run_tui "$HOME_PICKER" ' m~' "$FAKE" "$TMP/picker.log" 3)"
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

step "4/5 choosing a model, a key and a thinking mode"
HOME_FLOW="$TMP/home-flow"
make_home "$HOME_FLOW"
# provider (filtered) → model (filtered) → thinking "on" → key → Enter.
SCREEN="$(run_tui "$HOME_FLOW" \
  ' m~deep\r~pro\r~\r~sk-validate-key~:q\r' "$FAKE" "$TMP/flow.log" 2)"

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

# Comments and unknown keys in the user's config survive the write-back (DEC-19).
HOME_KEEP="$TMP/home-keep"
make_home "$HOME_KEEP"
cat >>"$HOME_KEEP/config.toml" <<'EOF'

# my own notes: this comment must survive
[review]
my_custom_key = "kept"
EOF
SCREEN="$(run_tui "$HOME_KEEP" \
  ' m~deep\r~pro\r~\r~sk-second-key~:q\r' "$FAKE" "$TMP/keep.log" 2)"
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

step "4/5 the worktree"
# The workspace needs a real repository, a real remote and a real pull request
# ref, so this builds them with git and points the app at the clone through
# `--path`. The fake `gh` still answers the PR calls.
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
# Captured before the push, because the clone has no ref to resolve afterwards:
# `git rev-parse` echoes an argument it cannot resolve rather than failing loudly,
# which is how this check came to compare against the literal text of a ref name.
PR_SHA="$(git -C "$REPO/clone" rev-parse HEAD)"
git -C "$REPO/clone" push --quiet --force origin HEAD:refs/pull/142/head
git -C "$REPO/clone" checkout --quiet main
git -C "$REPO/clone" branch --quiet -D work

HEAD_BEFORE="$(git -C "$REPO/clone" rev-parse HEAD)"
# The fake gh must report the head SHA the fixture repository actually has,
# otherwise the app would compare a workspace against a commit that never existed.
python3 - "$FAKE/view.json" "$PR_SHA" <<'PY'
import json, sys
path, head = sys.argv[1], sys.argv[2]
assert head and len(head) == 40, f"the head SHA did not resolve: {head!r}"
document = json.load(open(path))
document["headRefOid"] = head
document["baseRefName"] = "main"
json.dump(document, open(path, "w"), indent=1)
PY

set +e
(sleep 1; printf '\r'; sleep 12; printf 'q') \
  | PATH="$FAKE:$PATH" SMART_REVIEW_HOME="$HOME_WS" timeout 40 \
    script -qefc "stty rows 40 cols 160 2>/dev/null; '$ROOT/$BIN' --repo acme/service --path '$REPO/clone'" /dev/null \
  >"$TMP/ws.log" 2>&1
set -e

WORKTREE="$HOME_WS/worktrees/acme-service/pr-142"
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
if git -C "$REPO/clone" rev-parse --verify --quiet refs/smart-review/acme-service/pr-142/head >/dev/null; then
  ok "the fetched head is kept in a namespaced ref"
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

SCREEN="$(python3 "$ROOT/scripts/validate/screen.py" --path "$TMP/ws.log" --cols 160 --rows 40)"
if printf '%s' "$SCREEN" | saw "src.rs"; then
  ok "the review screen shows the file from the worktree"
else
  bad "the review screen never showed the diff"
  printf '%s\n' "$SCREEN" | tail -6
fi

# `:workspace clean` removes it, and `:doctor` reports what is left.
SCREEN="$(run_tui "$HOME_WS" ':workspace clean --all\r~q' "$FAKE" "$TMP/clean.log" 2)"
if printf '%s' "$SCREEN" | saw "removed 1 worktree"; then
  ok ":workspace clean removes the worktree"
else
  bad ":workspace clean did not report a removal"
  printf '%s\n' "$SCREEN" | tail -6
fi
if [ -d "$WORKTREE" ]; then
  bad "the worktree directory is still there"
else
  ok "the worktree directory is gone"
fi

step "5/5 offline behaviour"
# A second run inside the TTL must not have to fetch: stopping the server proves it.
kill "$SERVER_PID" 2>/dev/null
SERVER_PID=""
sleep 0.5
SCREEN="$(run_tui "$HOME_CATALOG" ' m~' "$FAKE" "$TMP/catalog-offline.log" 3)"
if printf '%s' "$SCREEN" | saw "cached"; then
  ok "a cached catalog is used when the server is gone"
else
  bad "the cached catalog was not used offline"
  printf '%s\n' "$SCREEN" | tail -6
fi


printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
