#!/usr/bin/env bash
#
# Review publishing validation: staging a review and the one call that posts it.
#
# Everything is local. The forge is a fake `gh` that records its argv and keeps a copy
# of anything passed with `--input`, so what is checked is what reached the wire rather
# than what the code that built it believed: one `POST .../reviews` call carrying the
# decision, the body and every comment, and no per-comment calls at all.
#
# The four things this feature can get wrong, and which each get a step here:
#
#   1. a comment that is never staged, or staged twice, or staged without text;
#   2. a review posted as N comments instead of one review (the whole reason the
#      batched endpoint is used);
#   3. a failure that loses the draft, which is the moment it matters most;
#   4. a dry run that sends something anyway (FR-6.5).
#
# Usage: scripts/validate/review-publishing.sh (KEEP=1 keeps temporary files)
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

TMP="$(mktemp -d "${SMART_REVIEW_VALIDATION_TMP:-${TMPDIR:-/tmp}}/review-publishing.XXXXXX")"
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
# shape chat.sh sets up, so the review screen is the one a user would see.
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
PR_SHA="$(git -C "$REPO/clone" rev-parse HEAD)"
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
    sed "s/\"number\": 141/\"number\": ${number:-141}/" "$here/view.json"
    exit 0 ;;
  pr:diff) cat "$here/diff.patch"; exit 0 ;;
  api:*)
    case "$*" in
      *graphql*) cat "$fixtures/graphql-count.json" ;;
      # The review endpoints. `comments` first: the review POST is a different path
      # and the two patterns would otherwise overlap on a substring.
      *pulls/*/comments*) cat "$here/comments.json" ;;
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
        release_marker=$(cat "$here/review_release_marker" 2>/dev/null)
        if [ -n "$release_marker" ]; then
          : >"$here/review-held"
          attempts=0
          while [ ! -f "$release_marker" ] && [ "$attempts" -lt 500 ]; do
            sleep 0.01
            attempts=$((attempts + 1))
          done
          if [ ! -f "$release_marker" ]; then
            printf 'fake gh: timed out waiting for review release marker\n' >&2
            exit 1
          fi
        fi
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
: >"$FAKE/review_release_marker"
cp "$TMP/diff.patch" "$FAKE/diff.patch"
python3 - "$FIXTURES/pr-view.json" "$FAKE/view.json" "$PR_SHA" <<'PY'
import json, sys
source, target, head = sys.argv[1:]
with open(source, encoding="utf-8") as handle:
    document = json.load(handle)
document["headRefOid"] = head
document["baseRefName"] = "main"
with open(target, "w", encoding="utf-8") as handle:
    json.dump(document, handle)
PY
# What GitHub already says about one of the changed lines, with a reply (FR-6.4). The
# line numbers are the ones in the patch above, so the thread has somewhere to land.
cat >"$FAKE/comments.json" <<'JSON'
[
  {
    "id": 9001,
    "user": { "login": "carol" },
    "path": "src/domain/money.rs",
    "line": 2,
    "side": "RIGHT",
    "body": "Rounding twice loses money on ties.",
    "created_at": "2026-09-10T10:01:00Z",
    "in_reply_to_id": null,
    "html_url": "https://example.invalid/discussion_r9001"
  },
  {
    "id": 9002,
    "user": { "login": "bruno" },
    "path": "src/domain/money.rs",
    "line": 2,
    "side": "RIGHT",
    "body": "Good catch, I will fix it in a follow-up.",
    "created_at": "2026-09-10T11:01:00Z",
    "in_reply_to_id": 9001,
    "html_url": "https://example.invalid/discussion_r9002"
  }
]
JSON
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

# Prove that the assertion used below distinguishes one dispatch from two. This is a
# synthetic fake-forge ledger only; no application or network process is started.
records_duplicate_review_dispatch() {
  : >"$FAKE/argv.txt"
  for _ in 1 2; do
    printf '%s\n' api -X POST repos/acme/service/pulls/141/reviews >>"$FAKE/argv.txt"
    printf '\037\n' >>"$FAKE/argv.txt"
  done
  [ "$(count_calls 'pulls/141/reviews')" = "2" ]
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
  local driver_code=0
  shift 4
  if [ -n "${DRIVE_STEP_NOTIFY:-}" ]; then
    PATH="$FAKE:$PATH" SMART_REVIEW_HOME="$home" \
      python3 "$ROOT/scripts/validate/drive.py" \
        --cols 160 --rows 40 --log "$log" \
        --timeout 90 --step-timeout 12 --settle 0.1 \
        --step-notify "$DRIVE_STEP_NOTIFY" \
        --ready "Add retry to the webhook dispatcher" \
        --keys "$keys" --waits "$waits" -- \
        "$ROOT/$BIN" --repo acme/service --path "$REPO/clone" "$@" || driver_code=$?
  else
    PATH="$FAKE:$PATH" SMART_REVIEW_HOME="$home" \
      python3 "$ROOT/scripts/validate/drive.py" \
        --cols 160 --rows 40 --log "$log" \
        --timeout 90 --step-timeout 12 --settle 0.1 \
        --ready "Add retry to the webhook dispatcher" \
        --keys "$keys" --waits "$waits" -- \
        "$ROOT/$BIN" --repo acme/service --path "$REPO/clone" "$@" || driver_code=$?
  fi
  if [ "$driver_code" -ne 0 ]; then
    touch "$TMP/driver.failed"
  fi
  return "$driver_code"
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
# Opening waits for the stable local-diff notice. Waiting for the earlier GitHub frame
# was both racy (it can be replaced between PTY reads) and wrong for navigation: the
# worktree swap rebuilds the view with its cursor at the top.
LINES='}jjjj'
COMMENT='c~the rounding is hidden behind a magic ten\r'
EXTRA='c~and this file has no callers\r'

step "1/8 build"
if [ "${SMART_REVIEW_SKIP_CARGO:-0}" = "1" ]; then
  printf '  SKIP  the parent validator already built the debug binary\n'
elif cargo build --quiet 2>"$TMP/build.log"; then
  ok "the debug binary builds"
else
  bad "the debug binary does not build"
  cat "$TMP/build.log" | tail -20
fi

step "2/8 a comment is staged on a line of the diff"
HOME_ONE="$TMP/home-one"
home_for "$HOME_ONE"
FRAMES="$TMP/stage.log"
SCREEN="$(run_tui "$HOME_ONE" "$OPEN~$LINES~c" "from the worktree~M src/domain/money~comment on" "$FRAMES")"
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
  "$OPEN~$LINES~c~the rounding is hidden behind a magic ten\r~j~c~and this file has no callers\r~ rd" \
  "from the worktree~M src/domain/money~comment on~1 comment staged~~comment on~2 comments staged~staged comments \\(2\\)" \
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

step "3/8 an empty comment is refused, not sent"
FRAMES="$TMP/empty.log"
SCREEN="$(run_tui "$HOME_ONE" "$OPEN~$LINES~c~\r" \
  "from the worktree~M src/domain/money~comment on~needs a body" "$FRAMES")"
if shown "$FRAMES" "needs a body"; then
  ok "an empty comment is refused with a reason"
else
  bad "an empty comment was not refused"
  printf '%s\n' "$SCREEN" | tail -8
fi

step "4/8 publishing sends one review, not N comments"
HOME_TWO="$TMP/home-two"
home_for "$HOME_TWO"
: >"$FAKE/argv.txt"
FRAMES="$TMP/publish.log"
SCREEN="$(run_tui "$HOME_TWO" \
  "$OPEN~$LINES~c~the rounding is hidden behind a magic ten\r~j~c~and this file has no callers\r~ rr~a~\r~\r" \
  "from the worktree~M src/domain/money~comment on~1 comment staged~~comment on~2 comments staged~publish review~approve —~Enter again~review posted" \
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

step "5/8 a refusal keeps the draft and explains itself"
HOME_THREE="$TMP/home-three"
home_for "$HOME_THREE"
printf '1' >"$FAKE/fail_review"
: >"$FAKE/argv.txt"
FRAMES="$TMP/refused.log"
SCREEN="$(run_tui "$HOME_THREE" \
  "$OPEN~$LINES~c~please take another look\r~ rr~a~\r~\r" \
  "from the worktree~M src/domain/money~comment on~1 comment staged~publish review~approve —~Enter again~is yours" \
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

step "6/8 the discussion that is already there"
FRAMES="$TMP/discussion.log"
SCREEN="$(run_tui "$HOME_ONE" "$OPEN~$LINES~q" \
  "from the worktree~M src/domain/money~" "$FRAMES")"
if shown "$FRAMES" "carol: Rounding twice loses money on ties."; then
  ok "an existing comment is drawn under the line it is about"
else
  bad "the existing discussion was not drawn"
  printf '%s\n' "$SCREEN" | tail -12
fi
if shown "$FRAMES" "bruno: Good catch"; then
  ok "a reply is drawn under the comment it answers"
else
  bad "the reply was not grouped with its comment"
fi

step "7/8 a dry run sends nothing and writes down what it would have sent"
HOME_DRY="$TMP/home-dry"
home_for "$HOME_DRY"
cat >>"$HOME_DRY/config.toml" <<'EOF'

[forge]
dry_run = true
EOF
: >"$FAKE/argv.txt"
FRAMES="$TMP/dry.log"
SCREEN="$(run_tui "$HOME_DRY" \
  "$OPEN~$LINES~c~a dry run must not post this\r~ rr~\r\r~\e" \
  "from the worktree~M src/domain/money~comment on~1 comment staged~Enter records~dry run: nothing was sent~" \
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

step "8/8 the second Enter sends once"
if records_duplicate_review_dispatch; then
  ok "the call ledger detects a deliberately duplicated review dispatch"
else
  bad "the call ledger would miss a duplicated review dispatch"
fi
HOME_SLOW="$TMP/home-slow"
home_for "$HOME_SLOW"
REVIEW_STEPS="$FAKE/review-steps"
mkdir -p "$REVIEW_STEPS"
printf '%s' "$REVIEW_STEPS/step-9.sent" >"$FAKE/review_release_marker"
rm -f "$FAKE/review-held" "$REVIEW_STEPS/step-9.sent"
: >"$FAKE/argv.txt"
FRAMES="$TMP/slow.log"
SCREEN="$(DRIVE_STEP_NOTIFY="$REVIEW_STEPS" run_tui "$HOME_SLOW" \
  "$OPEN~$LINES~c~one review only please\r~ rr~a~\r~\r~\r~q" \
  "from the worktree~M src/domain/money~comment on~1 comment staged~publish review~approve —~Enter again~sending~review posted~" \
  "$FRAMES")"
if [ -f "$FAKE/review-held" ]; then
  ok "the fake held the accepted review while the extra Enter was sent"
else
  bad "the review response was not held in flight"
fi
POSTS="$(count_calls 'pulls/141/reviews')"
if [ "$POSTS" = "1" ]; then
  ok "pressing Enter again while it was in flight posted nothing more"
else
  bad "in-flight Enter posted the review $POSTS times"
fi
: >"$FAKE/review_release_marker"

if [ -f "$TMP/driver.failed" ]; then
  bad "one or more PTY steps did not reach their expected screen state"
fi

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
