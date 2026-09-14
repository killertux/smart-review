#!/usr/bin/env bash
#
# M4 validation: staging a review, and the one call that posts it (FR-6.1-6.5).
#
# Everything is local. The forge is a fake `gh` that records its argv and keeps a copy
# of anything passed with `--input`, so what is checked is what reached the wire rather
# than what the code that built it believed: one `POST .../reviews` call carrying the
# decision, the body and every comment, and no per-comment calls at all.
#
# The four things this milestone can get wrong, and which each get a step here:
#
#   1. a comment that is never staged, or staged twice, or staged without text;
#   2. a review posted as N comments instead of one review (the whole reason the
#      batched endpoint is used);
#   3. a failure that loses the draft, which is the moment it matters most;
#   4. a dry run that sends something anyway (FR-6.5).
#
# Usage: scripts/validate/m4.sh         (KEEP=1 keeps the temporary directory)
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

cleanup() {
  if [ "${KEEP:-0}" = "1" ]; then
    printf '  note: kept %s\n' "$TMP"
  else
    rm -rf "$TMP"
  fi
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# The repository the app reads: a real clone with a real pull request ref, the same
# shape m3.sh sets up, so the review screen is the one a user would see.
# ---------------------------------------------------------------------------
REPO="$TMP/repo"
mkdir -p "$REPO/origin.git"
git init --bare --quiet -b main "$REPO/origin.git"
git clone --quiet "$REPO/origin.git" "$REPO/clone" 2>/dev/null
git -C "$REPO/clone" config user.email t@example.com
git -C "$REPO/clone" config user.name Test

mkdir -p "$REPO/clone/src/domain"
cat >"$REPO/clone/src/domain/money.rs" <<'EOF'
pub fn round(cents: i64) -> i64 {
    cents
}
EOF
git -C "$REPO/clone" add -A
git -C "$REPO/clone" commit --quiet -m "initial"
git -C "$REPO/clone" push --quiet origin main

git -C "$REPO/clone" checkout --quiet -b work
cat >"$REPO/clone/src/domain/money.rs" <<'EOF'
pub fn round(cents: i64) -> i64 {
    (cents + 5) / 10 * 10
}
EOF
printf 'pub fn currency(cents: i64) -> String {\n    format!("{cents}")\n}\n' \
  >"$REPO/clone/src/domain/amount.rs"
git -C "$REPO/clone" add -A
git -C "$REPO/clone" commit --quiet -m "round half up in money"
# The patch the forge would send, generated from the same commits: the fake `gh` serves
# it for `pr diff`, and the local worktree diff is the same change — so which of the two
# the app reads does not change what is on screen, and the validator cannot pass or fail
# on that race.
git -C "$REPO/clone" diff --no-color --no-ext-diff main HEAD >"$TMP/diff.patch"
git -C "$REPO/clone" push --quiet --force origin HEAD:refs/pull/141/head
git -C "$REPO/clone" checkout --quiet main
git -C "$REPO/clone" branch --quiet -D work

# ---------------------------------------------------------------------------
# The fake `gh`: it records every argv, answers the reads from the shared fixtures,
# and keeps a copy of the review payload — because the adapter deletes the file it
# passed, and the copy is the only thing left to assert against.
# ---------------------------------------------------------------------------
FAKE="$TMP/fake"
mkdir -p "$FAKE"
cat >"$FAKE/gh" <<'GH'
#!/bin/sh
here=$(dirname "$0")
fixtures=$(cat "$here/fixtures")
printf '%s\n' "$@" >>"$here/argv.txt"
printf '\037\n' >>"$here/argv.txt"
case "$1:$2" in
  --version:*)
    echo "gh version 2.45.0 (2025-07-18)"
    exit 0
    ;;
  auth:status)
    echo "github.com" >&2
    echo "  ✓ Logged in to github.com account tester (keyring)" >&2
    exit 0
    ;;
  pr:list) cat "$fixtures/pr-list.json"; exit 0 ;;
  pr:view)
    number=$(printf '%s' "$*" | sed -n 's/.*view \([0-9][0-9]*\).*/\1/p')
    sed "s/\"number\": 141/\"number\": ${number:-141}/" "$fixtures/pr-view.json"
    exit 0 ;;
  pr:diff) cat "$here/diff.patch"; exit 0 ;;
  api:*)
    case "$*" in
      *graphql*) cat "$fixtures/graphql-count.json" ;;
      # The review endpoints. `comments` first: the review POST is a different path
      # and the two patterns would otherwise overlap on a substring.
      *pulls/*/comments*) printf '[]' ;;
      *pulls/*/reviews*)
        prev=""
        for arg in "$@"; do
          if [ "$prev" = "--input" ] && [ -f "$arg" ]; then
            cp "$arg" "$here/review.json"
          fi
          prev="$arg"
        done
        if [ "$(cat "$here/fail_review" 2>/dev/null)" = "1" ]; then
          printf 'gh: Validation Failed (HTTP 422)\n' >&2
          printf '{"message":"Validation Failed","errors":[{"message":"Can not approve your own pull request"}]}\n' >&2
          exit 1
        fi
        sleep "$(cat "$here/review_delay" 2>/dev/null || echo 0)"
        printf '{"id": 4242, "html_url": "https://example.invalid/review/4242"}'
        ;;
      *) printf '[]' ;;
    esac
    exit 0
    ;;
esac
echo "fake gh: unexpected call: $*" >&2
exit 1
GH
chmod +x "$FAKE/gh"
printf '%s' "$FIXTURES" >"$FAKE/fixtures"
printf '0' >"$FAKE/fail_review"
printf '0' >"$FAKE/review_delay"
cp "$TMP/diff.patch" "$FAKE/diff.patch"
: >"$FAKE/argv.txt"

# How many calls mentioned `$1` at all. Counted by splitting on the unit separator the
# fake writes after each call, so an argument that merely *contains* the string is one
# call — which is the number that matters ("how many times was the review posted?").
count_calls() {
  python3 - "$FAKE/argv.txt" "$1" <<'PY'
import sys
text = open(sys.argv[1]).read() if len(sys.argv) > 1 else ""
needle = sys.argv[2]
calls = [call for call in text.split("") if call.strip()]
print(sum(1 for call in calls if needle in call))
PY
}

# Runs a python assertion against a file, and says whether it held.
check_json() {
  python3 - "$@" 2>"$TMP/check.log"
}

home_for() {
  local home="$1"
  mkdir -p "$home"
  cat >"$home/config.toml" <<'EOF'
[ui]
theme = "dark"
EOF
}

run_tui() {
  local home="$1" keys="$2" waits="$3" log="$4"
  shift 4
  PATH="$FAKE:$PATH" SMART_REVIEW_HOME="$home" \
    python3 "$ROOT/scripts/validate/drive.py" \
      --cols 160 --rows 40 --log "$log" \
      --timeout 180 --step-timeout 20 --settle 3 \
      --ready "Add retry to the webhook dispatcher" \
      --keys "$keys" --waits "$waits" -- \
      "$ROOT/$BIN" --repo acme/service --path "$REPO/clone" "$@"
}

shown() {
  python3 "$ROOT/scripts/validate/screen.py" --cols 160 --rows 40 \
    --path "$1" --when "$2" >/dev/null 2>&1
}

draft_file() {
  printf '%s/drafts/github.com/acme/service/pr-141.json' "$1"
}

# The keystrokes, as groups: `~` separates one group per wait.
#
#   OPEN     `:pr 141`, then `}` to jump to the second file and `jjj` to reach a line
#            of its hunk. Header rows have no line number to anchor a comment to, and
#            the app says so rather than guessing — which is how this navigator was
#            written: `jj` alone landed on a header and the composer refused.
#   COMMENT  `c`, type, Enter — staged, which is local and reversible
#   PANEL    ` rd`, the staged comments
#   MODAL    ` rr`, then `a` to choose approve, then Enter twice
OPEN=':pr 141\r'
# Nothing between the two: an empty key group with an empty wait is a pause, and the
# pause is what lets the app finish replacing the forge's diff with the local one. That
# swap rebuilds the view, and a view that is rebuilt has its cursor back at the top —
# so navigating before it lands means commenting on the wrong file.
SETTLE='' 
LINES='}jjjj'
COMMENT='c~the rounding is hidden behind a magic ten\r'
EXTRA='c~and this file has no callers\r'

step "1/7 build"
if cargo build --quiet 2>"$TMP/build.log"; then
  ok "the debug binary builds"
else
  bad "the debug binary does not build"
  cat "$TMP/build.log" | tail -20
fi

step "2/7 a comment is staged on a line of the diff"
HOME_ONE="$TMP/home-one"
home_for "$HOME_ONE"
FRAMES="$TMP/stage.log"
SCREEN="$(run_tui "$HOME_ONE" "$OPEN~$SETTLE~$LINES~c" "from the github~~M src/domain/money~comment on" "$FRAMES")"
if shown "$FRAMES" "comment on src/domain/money.rs"; then
  ok "the composer names the file and side the comment will land on"
else
  bad "the composer did not open on a line"
  printf '%s\n' "$SCREEN" | tail -8
fi
if printf '%s' "$SCREEN" | grep -q "Enter stages it"; then
  ok "the composer says which key stages the comment"
else
  bad "the composer does not say what Enter does"
fi

# ---------------------------------------------------------------------------
# Three comments, two files, and a range: the shape the batched call exists for.
# ---------------------------------------------------------------------------
FRAMES="$TMP/three.log"
SCREEN="$(run_tui "$HOME_ONE" \
  "$OPEN~$SETTLE~$LINES~c~the rounding is hidden behind a magic ten\r~j~c~and this file has no callers\r~ rd" \
  "from the github~~M src/domain/money~comment on~1 comment staged~M src/domain/money~comment on~2 comments staged~staged comments \\(2\\)" \
  "$FRAMES")"
if shown "$FRAMES" "staged comments \\(2\\)"; then
  ok "two comments are staged and listed"
else
  bad "the draft panel did not show the staged comments"
  printf '%s\n' "$SCREEN" | tail -10
fi
if [ -f "$(draft_file "$HOME_ONE")" ]; then
  ok "the draft is on disk before anything is published"
else
  bad "staging wrote no draft, so a crash would lose the review"
fi
if check_json "$(draft_file "$HOME_ONE")" <<'PY'
import json, sys
draft = json.load(open(sys.argv[1]))
comments = draft.get("comments", [])
assert len(comments) == 2, f"expected 2 comments, got {len(comments)}"
first = comments[0]
assert first["path"] == "src/domain/money.rs", first
assert first["side"] == "new", first
assert first["line"] >= 1, first
bodies = " ".join(comment["body"] for comment in comments)
assert "magic ten" in bodies, bodies
assert "no callers" in bodies, bodies
assert all(comment["path"] == "src/domain/money.rs" for comment in comments), comments
assert all(comment["line"] >= 1 for comment in comments), comments
assert draft.get("head_sha"), "the commit the comments were written against is recorded"
PY
then
  ok "the staged comments carry file, side, line and body"
else
  bad "the stored draft is not what was typed"
  cat "$TMP/check.log"
fi

step "3/7 an empty comment is refused, not sent"
FRAMES="$TMP/empty.log"
SCREEN="$(run_tui "$HOME_ONE" "$OPEN~$SETTLE~$LINES~c~\r" \
  "money.rs~comment on~needs a body" "$FRAMES")"
if shown "$FRAMES" "needs a body"; then
  ok "an empty comment is refused with a reason"
else
  bad "an empty comment was not refused"
  printf '%s\n' "$SCREEN" | tail -8
fi

step "4/7 publishing sends one review, not N comments"
HOME_TWO="$TMP/home-two"
home_for "$HOME_TWO"
: >"$FAKE/argv.txt"
FRAMES="$TMP/publish.log"
SCREEN="$(run_tui "$HOME_TWO" \
  "$OPEN~$SETTLE~$LINES~c~the rounding is hidden behind a magic ten\r~j~c~and this file has no callers\r~ rr~a~\r~\r" \
  "from the github~~M src/domain/money~comment on~1 comment staged~M src/domain/money~comment on~2 comments staged~publish review~approve —~Enter again~review posted" \
  "$FRAMES")"
if shown "$FRAMES" "approve — this unblocks the pull request"; then
  ok "the modal names the verdict it is about to give"
else
  bad "the modal did not show the decision"
fi
if shown "$FRAMES" "and this file has no callers"; then
  ok "every comment is shown verbatim before it is sent"
else
  bad "the modal did not show the staged comments"
fi
POSTS="$(count_calls 'pulls/141/reviews')"
if [ "$POSTS" = "1" ]; then
  ok "the review reached GitHub in exactly one call"
else
  bad "the review was not one call ($POSTS calls to the reviews endpoint)"
fi
# The whole point of the batched call: no comment is created on its own. A per-comment
# API has to name the comment endpoint, so its absence is the proof.
MUTATIONS="$(count_calls 'POST')"
if [ "$MUTATIONS" = "1" ]; then
  ok "and it was the only thing that changed anything"
else
  bad "$MUTATIONS mutating calls: a batched review is one"
fi
if check_json "$FAKE/review.json" <<'PY'
import json, sys
body = json.load(open(sys.argv[1]))
assert body["event"] == "APPROVE", body
assert len(body["comments"]) == 2, body
assert body["comments"][0]["side"] == "RIGHT", body
assert body["comments"][0]["path"] == "src/domain/money.rs", body
assert body.get("commit_id"), body
PY
then
  ok "the payload carries the decision and both comments"
else
  bad "the payload is not what the modal showed"
  cat "$TMP/check.log"
fi
if [ -f "$(draft_file "$HOME_TWO")" ]; then
  bad "the published draft is still on disk, so it would be sent twice"
else
  ok "the draft is cleared once the review is posted"
fi

step "5/7 a refusal keeps the draft and explains itself"
HOME_THREE="$TMP/home-three"
home_for "$HOME_THREE"
printf '1' >"$FAKE/fail_review"
: >"$FAKE/argv.txt"
FRAMES="$TMP/refused.log"
SCREEN="$(run_tui "$HOME_THREE" \
  "$OPEN~$SETTLE~$LINES~c~please take another look\r~ rr~a~\r~\r" \
  "from the github~~M src/domain/money~comment on~1 comment staged~publish review~approve —~Enter again~is yours" \
  "$FRAMES")"
if shown "$FRAMES" "is yours"; then
  ok "your own pull request is explained rather than quoted"
else
  bad "the refusal was not translated"
  printf '%s\n' "$SCREEN" | tail -8
fi
if shown "$FRAMES" "nothing was posted"; then
  ok "the modal says nothing was posted"
else
  bad "the modal does not say whether anything was sent"
fi
if [ -f "$(draft_file "$HOME_THREE")" ]; then
  ok "the draft survives a refused publish"
else
  bad "a refused publish lost the draft"
fi
printf '0' >"$FAKE/fail_review"

step "6/7 a dry run sends nothing and writes down what it would have sent"
HOME_DRY="$TMP/home-dry"
home_for "$HOME_DRY"
cat >>"$HOME_DRY/config.toml" <<'EOF'

[forge]
dry_run = true
EOF
: >"$FAKE/argv.txt"
FRAMES="$TMP/dry.log"
SCREEN="$(run_tui "$HOME_DRY" \
  "$OPEN~$SETTLE~$LINES~c~a dry run must not post this\r~ rr~\r~\r~q" \
  "from the github~~M src/domain/money~comment on~1 comment staged~publish review~DRY RUN~Enter records~dry run: nothing was sent~" \
  "$FRAMES")"
if [ "$(count_calls 'pulls/141/reviews')" != "0" ]; then
  bad "a dry run posted the review anyway"
else
  ok "a dry run reaches no review endpoint"
fi
if [ -f "$HOME_DRY/logs/dry-run.log" ]; then
  ok "the dry run writes the commands it would have run"
else
  bad "a dry run said nothing anywhere"
fi
if grep -q 'pulls/141/reviews' "$HOME_DRY/logs/dry-run.log" 2>/dev/null; then
  ok "the recorded command names the review endpoint"
else
  bad "the recorded command is not the one that would have run"
  cat "$HOME_DRY/logs/dry-run.log" 2>/dev/null | tail -5
fi
if [ -f "$(draft_file "$HOME_DRY")" ]; then
  ok "nothing was sent, so the draft is still here"
else
  bad "a dry run cleared the draft"
fi

step "7/7 the second Enter sends once"
HOME_SLOW="$TMP/home-slow"
home_for "$HOME_SLOW"
printf '2' >"$FAKE/review_delay"
: >"$FAKE/argv.txt"
FRAMES="$TMP/slow.log"
SCREEN="$(run_tui "$HOME_SLOW" \
  "$OPEN~$SETTLE~$LINES~c~one review only please\r~ rr~a~\r~\r~\r~q" \
  "from the github~~M src/domain/money~comment on~1 comment staged~publish review~approve —~Enter again~review posted~~" \
  "$FRAMES")"
POSTS="$(count_calls 'pulls/141/reviews')"
if [ "$POSTS" = "1" ]; then
  ok "pressing Enter again while it was in flight posted nothing more"
else
  bad "in-flight Enter posted the review $POSTS times"
fi
printf '0' >"$FAKE/review_delay"

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
