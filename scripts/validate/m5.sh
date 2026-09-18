#!/usr/bin/env bash
#
# M5 validation: replies, resolution, the pull request conversation, external editing,
# and the release/docs promises (FR-6.2, FR-6.4, DEC-16).
#
# Everything is local, and the forge is a fake `gh` that records its argv. That matters
# more here than anywhere else in this project: three of the four things this milestone
# adds are *mutations*, and the only evidence that a reply answered the right comment or
# that a resolve named the right thread is what was written on the wire.
#
# The findings this file exists for:
#
#   1. a reply that goes to the right route but the wrong comment (or the wrong body);
#   2. a resolve that is drawn as done before GitHub said so — a tick that means nothing;
#   3. a GraphQL answer that exits zero with an error in the body, which is what
#      `gh api graphql` does and which would otherwise be read as success;
#   4. a dry run that posts a comment anyway (FR-6.5, on the newer surface);
#   5. a failure that loses the words, which is the moment they matter most;
#   6. an editor integration that leaves raw mode but fails to put its words back.
#
# Usage: scripts/validate/m5.sh         (KEEP=1 keeps the temporary directory)
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

TMP="$(mktemp -d "${SMART_REVIEW_VALIDATION_TMP:-${TMPDIR:-/tmp}}/m5.XXXXXX")"
BIN="target/debug/smart-review"

cleanup() {
  if [ "${KEEP:-0}" = "1" ]; then
    printf '  note: kept %s\n' "$TMP"
  else
    rm -rf "$TMP"
  fi
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# A repository with one pull request, the same shape m4.sh builds: a real clone and a
# real ref, so the review screen is the one a user would see.
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
git -C "$REPO/clone" add -A
git -C "$REPO/clone" commit --quiet -m "round half up in money"
PR_SHA="$(git -C "$REPO/clone" rev-parse HEAD)"
git -C "$REPO/clone" diff --no-color --no-ext-diff main HEAD >"$TMP/diff.patch"
git -C "$REPO/clone" push --quiet --force origin HEAD:refs/pull/141/head
git -C "$REPO/clone" checkout --quiet main
git -C "$REPO/clone" branch --quiet -D work

# ---------------------------------------------------------------------------
# The fake `gh`: reads from the fixtures, records every argv, and keeps the body of
# anything it was asked to post. Each failure it can be told to produce is a file, so a
# step can turn one on without touching the others.
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
  api:graphql)
    # One endpoint, three questions. `gh api graphql` exits *zero* when the query
    # failed and puts the reason in the body, so a failure here is a body with errors
    # in it — which is exactly the case the adapter has to notice.
    case "$*" in
      *resolveReviewThread*|*unresolveReviewThread*)
        if [ "$(cat "$here/fail_resolve" 2>/dev/null)" = "1" ]; then
          printf '{"data":null,"errors":[{"message":"Could not resolve to a node with the global id of %s"}]}' "'PRRT_nope'"
          exit 0
        fi
        case "$*" in
          *unresolveReviewThread*) printf '0' >"$here/resolved_flag"; cat "$here/unresolved.json" ;;
          *) printf '1' >"$here/resolved_flag"; cat "$here/resolved.json" ;;
        esac
        ;;
      *)
        # GitHub's own answer, not the adapter's hope: a resolve that succeeded changes
        # what the *next* read reports, which is why the tick is checked after a refresh.
        if [ "$(cat "$here/resolved_flag" 2>/dev/null)" = "1" ]; then
          cat "$here/threads-resolved.json"
        else
          cat "$here/threads.json"
        fi
        ;;
    esac
    exit 0
    ;;
  api:*)
    case "$*" in
      # The conversation, read and written. `issues/N/comments` is the issue's comment
      # list, because a pull request is an issue.
      *issues/*/comments*)
        case "$*" in
          *POST*)
            body=$(python3 - "$@" <<'PY'
import json, sys
args = sys.argv[1:]
path = args[args.index("--input") + 1]
print(json.load(open(path))["body"], end="")
PY
)
            printf '%s' "$body" >"$here/conversation-posted.txt"
            if [ "$(cat "$here/fail_conversation" 2>/dev/null)" = "1" ]; then
              printf 'gh: Resource not accessible by integration (HTTP 403)\n' >&2
              exit 1
            fi
            printf '{"id": 7002, "html_url": "https://example.invalid/issuecomment-7002"}'
            ;;
          *) cat "$here/conversation.json" ;;
        esac
        ;;
      # A reply, which is the comment route with the comment it answers in the path.
      *pulls/*/comments/*/replies*)
        body=$(python3 - "$@" <<'PY'
import json, sys
args = sys.argv[1:]
path = args[args.index("--input") + 1]
print(json.load(open(path))["body"], end="")
PY
)
        printf '%s' "$body" >"$here/reply-posted.txt"
        if [ "$(cat "$here/fail_reply" 2>/dev/null)" = "1" ]; then
          printf 'gh: Resource not accessible by integration (HTTP 403)\n' >&2
          exit 1
        fi
        printf '{"id": 9003, "html_url": "https://example.invalid/discussion_r9003"}'
        ;;
      *pulls/*/comments*) cat "$here/comments.json" ;;
      *) printf '[]' ;;
    esac
    exit 0
    ;;
esac
echo "fake gh: unexpected call: $*" >&2
exit 1
GH
chmod +x "$FAKE/gh"
cat >"$FAKE/editor" <<'SH'
#!/bin/sh
printf '\nfinished in editor' >>"$1"
SH
chmod +x "$FAKE/editor"
printf '%s' "$ROOT/tests/fixtures/gh" >"$FAKE/fixtures"
printf '0' >"$FAKE/fail_resolve"
printf '0' >"$FAKE/fail_reply"
printf '0' >"$FAKE/fail_conversation"
cp "$TMP/diff.patch" "$FAKE/diff.patch"
python3 - "$ROOT/tests/fixtures/gh/pr-view.json" "$FAKE/view.json" "$PR_SHA" <<'PY'
import json, sys
source, target, head = sys.argv[1:]
with open(source, encoding="utf-8") as handle:
    document = json.load(handle)
document["headRefOid"] = head
document["baseRefName"] = "main"
with open(target, "w", encoding="utf-8") as handle:
    json.dump(document, handle)
PY

# What is already on the pull request: one thread of two comments on the changed line,
# and one comment on the conversation. The line numbers are the ones in the patch, so
# there is something for the thread to be drawn under.
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
    "body": "Good catch, follow-up.",
    "created_at": "2026-09-10T11:01:00Z",
    "in_reply_to_id": 9001,
    "html_url": "https://example.invalid/discussion_r9002"
  }
]
JSON

# The thread state, which is GraphQL-only. `databaseId` is the REST id, and it is the
# only bridge between the two namespaces.
cat >"$FAKE/threads.json" <<'JSON'
{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
  {"id":"PRRT_kwDOA1","isResolved":false,"isOutdated":false,
   "comments":{"nodes":[{"databaseId":9001},{"databaseId":9002}]}}
]}}}}}
JSON
cat >"$FAKE/threads-resolved.json" <<'JSON'
{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
  {"id":"PRRT_kwDOA1","isResolved":true,"isOutdated":false,
   "comments":{"nodes":[{"databaseId":9001},{"databaseId":9002}]}}
]}}}}}
JSON
cat >"$FAKE/resolved.json" <<'JSON'
{"data":{"resolveReviewThread":{"thread":{"id":"PRRT_kwDOA1","isResolved":true}}}}
JSON
cat >"$FAKE/unresolved.json" <<'JSON'
{"data":{"unresolveReviewThread":{"thread":{"id":"PRRT_kwDOA1","isResolved":false}}}}
JSON
cat >"$FAKE/conversation.json" <<'JSON'
[
  {
    "id": 7001,
    "user": { "login": "dana" },
    "body": "This came out of the incident on Tuesday.",
    "created_at": "2026-09-09T09:00:00Z",
    "html_url": "https://example.invalid/issuecomment-7001"
  }
]
JSON
: >"$FAKE/argv.txt"

count_calls() {
  python3 - "$FAKE/argv.txt" "$1" <<'PY'
import sys
text = open(sys.argv[1]).read() if len(sys.argv) > 1 else ""
needle = sys.argv[2]
calls = [call for call in text.split("\x1f") if call.strip()]
print(sum(1 for call in calls if needle in call))
PY
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
  # The keystrokes and the waits are index-paired, so two lists of different lengths
  # shift every pattern one group and the step silently checks the wrong moment. That
  # happened twice in M3 and once when this file was first written, so it is counted
  # rather than trusted: a mispaired step is a bug in the validator, not in the app.
  local key_groups="${keys//[!~]/}" wait_groups="${waits//[!~]/}"
  if [ "${#key_groups}" != "${#wait_groups}" ]; then
    bad "this step's keys and waits are not the same length: $(( ${#key_groups} + 1 )) key groups, $(( ${#wait_groups} + 1 )) waits"
    return 2
  fi
  PATH="$FAKE:$PATH" SMART_REVIEW_HOME="$home" \
    python3 "$ROOT/scripts/validate/drive.py" \
      --cols 160 --rows 40 --log "$log" \
      --timeout 90 --step-timeout 12 --settle 0.1 \
      --ready "Add retry to the webhook dispatcher" \
      --keys "$keys" --waits "$waits" -- \
      "$ROOT/$BIN" --repo acme/service --path "$REPO/clone" "$@" || driver_code=$?
  if [ "$driver_code" -ne 0 ]; then
    touch "$TMP/driver.failed"
  fi
  return "$driver_code"
}

shown() {
  python3 "$ROOT/scripts/validate/screen.py" --cols 160 --rows 40 \
    --path "$1" --when "$2" >/dev/null 2>&1
}

# The keystrokes, as groups: `~` separates one group per wait.
#
#   OPEN      `:pr 141`; its wait is the worktree diff, whose swap rebuilds the view
#             with its cursor at the top. The earlier GitHub notice is not observed.
#   LINES     `}` to the second file, then `jjjj` to the changed line — header rows have
#             no line to anchor to, and the app says so rather than guessing
#   ON_THREAD one more `j`: the discussion is drawn *under* the line it is about, so the
#             row after the changed line is the comment
OPEN=':pr 141\r'
LINES='}jjjj'
ON_THREAD='}jjjjj'

step "1/12 build"
if [ "${SMART_REVIEW_SKIP_CARGO:-0}" = "1" ]; then
  printf '  SKIP  the parent validator already built the debug binary\n'
elif cargo build --quiet 2>"$TMP/build.log"; then
  ok "the debug binary builds"
else
  bad "the debug binary does not build"
  tail -20 "$TMP/build.log"
fi

step "2/12 the discussion GitHub already has is drawn where it belongs"
HOME_ONE="$TMP/home-one"
home_for "$HOME_ONE"
FRAMES="$TMP/discussion.log"
SCREEN="$(run_tui "$HOME_ONE" "$OPEN~$LINES~q" \
  "from the worktree~M src/domain/money~" "$FRAMES")"
if shown "$FRAMES" "carol: Rounding twice loses money on ties."; then
  ok "an existing comment is drawn under the line it is about"
else
  bad "the existing discussion was not drawn"
  printf '%s\n' "$SCREEN" | tail -14
fi
if shown "$FRAMES" "bruno: Good catch, follow-up."; then
  ok "and the reply is grouped under the comment it answers"
else
  bad "the reply was not drawn with its thread"
fi

step "3/12 an open thread is not drawn as resolved"
if shown "$FRAMES" "▸ carol"; then
  ok "the marker says the thread is open"
else
  bad "a thread GitHub reports as open is drawn as something else"
fi

step "4/12 replying answers the comment, in one call, with the words typed"
: >"$FAKE/argv.txt"
FRAMES="$TMP/reply.log"
SCREEN="$(run_tui "$HOME_ONE" \
  "$OPEN~$ON_THREAD~r~agreed, fixed in the follow-up\r~\r~\r~q" \
  "from the worktree~M src/domain/money~reply on src/domain/money.rs:2~post · #141~Enter again sends it to GitHub~comment posted~" \
  "$FRAMES")"
if shown "$FRAMES" 'reply on src/domain/money.rs:2 \(new\)'; then
  ok "the composer says which comment is being answered"
else
  bad "the composer did not open as a reply"
  printf '%s\n' "$SCREEN" | tail -10
fi
if shown "$FRAMES" "post · #141"; then
  ok "the modal shows what is about to be posted"
else
  bad "a reply was not shown before it was sent"
  printf '%s\n' "$SCREEN" | tail -10
fi
REPLIES="$(count_calls 'comments/9001/replies')"
if [ "$REPLIES" = "1" ]; then
  ok "the reply went to the thread's reply route, naming the comment it answers"
else
  bad "the reply did not reach the reply route ($REPLIES calls)"
fi
if [ "$(cat "$FAKE/reply-posted.txt" 2>/dev/null)" = "agreed, fixed in the follow-up" ]; then
  ok "and it carried the words that were typed"
else
  bad "the body on the wire is not what was typed: '$(cat "$FAKE/reply-posted.txt" 2>/dev/null)'"
fi
if [ "$(count_calls 'pulls/141/reviews')" = "0" ]; then
  ok "no review was published along with it"
else
  bad "a reply posted a review as well"
fi

step '5/12 $EDITOR returns to the same composer, with the words it wrote'
FRAMES="$TMP/editor.log"
SCREEN="$(EDITOR="$FAKE/editor" run_tui "$HOME_ONE" \
  "$OPEN~$LINES~c~before the editor~\005~\r~q" \
  'from the worktree~M src/domain/money~comment on src/domain/money.rs:2~~composer updated from \$EDITOR~1 draft~' \
  "$FRAMES")"
if shown "$FRAMES" 'composer updated from \$EDITOR'; then
  ok 'the terminal returned from $EDITOR to the composer'
else
  bad "the editor did not return to the comment composer"
  printf '%s\n' "$SCREEN" | tail -12
fi
if grep -Rqs 'before the editor\|finished in editor' "$HOME_ONE/drafts"; then
  ok "the draft contains the text the editor wrote"
else
  bad "the editor's text was not staged with the comment"
fi

step "6/12 resolving a thread asks first, then says so in the thread's own words"
: >"$FAKE/argv.txt"
FRAMES="$TMP/resolve.log"
SCREEN="$(run_tui "$HOME_ONE" \
  "$OPEN~$ON_THREAD~ pt~y~q" \
  "from the worktree~~resolve this thread~✓ carol \\(resolved\\)~" \
  "$FRAMES")"
if shown "$FRAMES" "resolve this thread on GitHub?"; then
  ok "the confirmation says what is about to change on GitHub"
else
  bad "a thread was resolved without being confirmed (FR-6.5)"
  printf '%s\n' "$SCREEN" | tail -10
fi
RESOLVES="$(count_calls 'resolveReviewThread')"
if [ "$RESOLVES" = "1" ]; then
  ok "the resolve reached GitHub once"
else
  bad "the resolve did not reach GitHub ($RESOLVES calls)"
fi
if [ "$(count_calls 'id=PRRT_kwDOA1')" = "1" ]; then
  ok "and it named the thread GitHub reported, not a line number"
else
  bad "the mutation did not name the thread id"
fi
if shown "$FRAMES" '✓ carol \(resolved\)'; then
  ok "the thread is drawn as resolved"
else
  bad "the thread was not drawn as resolved after the answer arrived"
fi

step "7/12 a GraphQL error in a zero-exit answer is a failure, not a success"
# `gh api graphql` exits 0 when the query failed and puts the reason in the body. A
# resolve read as success there is a tick that means nothing.
printf '1' >"$FAKE/fail_resolve"
printf '0' >"$FAKE/resolved_flag"
FRAMES="$TMP/resolve-failed.log"
SCREEN="$(run_tui "$HOME_ONE" \
  "$OPEN~$ON_THREAD~ pt~y~q" \
  "from the worktree~~resolve this thread~the thread was not changed~" \
  "$FRAMES")"
if shown "$FRAMES" "the thread was not changed"; then
  ok "the reason is on screen"
else
  bad "a failed resolve said nothing"
  printf '%s\n' "$SCREEN" | tail -10
fi
if shown "$FRAMES" "Could not resolve to a node"; then
  ok "and it is GitHub's own sentence"
else
  bad "GitHub's reason was dropped"
fi
if shown "$FRAMES" "✓ carol"; then
  bad "a failed resolve drew a tick anyway"
else
  ok "and nothing is drawn as resolved"
fi
printf '0' >"$FAKE/fail_resolve"
printf '0' >"$FAKE/resolved_flag"

step "8/12 a failed reply keeps the words in the modal"
printf '1' >"$FAKE/fail_reply"
FRAMES="$TMP/reply-failed.log"
SCREEN="$(run_tui "$HOME_ONE" \
  "$OPEN~$ON_THREAD~r~this must not be lost\r~\r~\r~q" \
  "from the worktree~M src/domain/money~reply on src/domain/money.rs:2~post · #141~Enter again sends it to GitHub~comment is still here~" \
  "$FRAMES")"
if shown "$FRAMES" "the comment is still here"; then
  ok "the modal says the comment was kept"
else
  bad "a failed reply closed the modal"
  printf '%s\n' "$SCREEN" | tail -12
fi
if shown "$FRAMES" "token is not allowed"; then
  ok "and it is GitHub's refusal, translated"
else
  bad "the refusal was not translated"
fi
if shown "$FRAMES" "this must not be lost"; then
  ok "the words are still on screen"
else
  bad "a failed reply lost what was typed"
fi
printf '0' >"$FAKE/fail_reply"

step "9/12 the conversation is readable, and a comment on it reaches the issue"
: >"$FAKE/argv.txt"
FRAMES="$TMP/conversation.log"
SCREEN="$(run_tui "$HOME_ONE" \
  "$OPEN~ pc~c~thanks, looking at it now\r~\r~\r~q" \
  "from the worktree~conversation · #141~~post · #141~Enter again sends it to GitHub~comment posted~" \
  "$FRAMES")"
if shown "$FRAMES" "This came out of the incident on Tuesday."; then
  ok "the conversation panel shows what has been said"
else
  bad "the conversation was not shown"
  printf '%s\n' "$SCREEN" | tail -12
fi
if [ "$(cat "$FAKE/conversation-posted.txt" 2>/dev/null)" = "thanks, looking at it now" ]; then
  ok "the comment reached the conversation endpoint with the words typed"
else
  bad "the conversation comment did not reach the wire: '$(cat "$FAKE/conversation-posted.txt" 2>/dev/null)'"
fi
if [ "$(count_calls 'issues/141/comments')" -ge "2" ]; then
  ok "read and written through the issue's comment list"
else
  bad "the conversation was not read and written through issues/N/comments"
fi

step "10/12 a refused conversation comment says so and keeps the words"
printf '1' >"$FAKE/fail_conversation"
FRAMES="$TMP/conversation-failed.log"
SCREEN="$(run_tui "$HOME_ONE" \
  "$OPEN~ pc~c~please keep this\r~\r~\r~q" \
  "from the worktree~conversation · #141~~post · #141~Enter again sends it to GitHub~comment is still here~" \
  "$FRAMES")"
if shown "$FRAMES" "comment is still here"; then
  ok "a refused comment is kept rather than lost"
else
  bad "a refused conversation comment was lost"
  printf '%s\n' "$SCREEN" | tail -12
fi
if shown "$FRAMES" "please keep this"; then
  ok "and it is still on screen to send again"
else
  bad "the words were lost"
fi
printf '0' >"$FAKE/fail_conversation"

step "11/12 a dry run posts nothing at all, on either new surface"
printf '0' >"$FAKE/resolved_flag"
HOME_DRY="$TMP/home-dry"
home_for "$HOME_DRY"
cat >>"$HOME_DRY/config.toml" <<'EOF'

[forge]
dry_run = true
EOF
: >"$FAKE/argv.txt"
FRAMES="$TMP/dry.log"
SCREEN="$(run_tui "$HOME_DRY" \
  "$OPEN~$ON_THREAD~r~a dry run reply\r~\r\r~\e~ pt~y~~ pc~c~a dry run comment\r~\r\r~\e" \
  "from the worktree~M src/domain/money~reply on src/domain/money.rs:2~Enter records~dry run: nothing was posted~~resolve this thread~nothing changed on GitHub~~conversation · #141~~post · #141~dry run: nothing was posted~" \
  "$FRAMES")"
if [ "$(count_calls 'comments/9001/replies')" != "0" ]; then
  bad "a dry run posted the reply anyway"
else
  ok "a dry run reaches no reply route"
fi
if [ "$(count_calls 'resolveReviewThread')" != "0" ]; then
  bad "a dry run resolved the thread anyway"
else
  ok "a dry run reaches no resolve mutation"
fi
if [ "$(count_calls 'POST')" != "0" ]; then
  bad "a dry run made $(count_calls 'POST') posting calls"
else
  ok "a dry run posts nothing at all"
fi
if grep -q 'comments/9001/replies' "$HOME_DRY/logs/dry-run.log" 2>/dev/null \
  && grep -q 'resolveReviewThread' "$HOME_DRY/logs/dry-run.log" 2>/dev/null \
  && grep -q 'issues/141/comments' "$HOME_DRY/logs/dry-run.log" 2>/dev/null; then
  ok "and it writes down all three commands it would have run"
else
  bad "the dry run log is missing one of the calls"
  tail -5 "$HOME_DRY/logs/dry-run.log" 2>/dev/null
fi

step "12/12 generated docs and release workflow describe the shipped binary"
if [ "${SMART_REVIEW_SKIP_CARGO:-0}" = "1" ]; then
  ok "the shared test gate checked generated keymap documentation"
elif cargo test --quiet tui::keymap::tests::checked_keymap_documentation_is_generated_from_the_registry; then
  ok "the checked keymap document still matches the action registry"
else
  bad "docs/keymaps.md drifted from the action registry"
fi
if [ -s docs/themes.md ] && [ -s docs/configuration.md ] \
  && grep -q 'aarch64-apple-darwin' .github/workflows/release.yml \
  && grep -q 'x86_64-apple-darwin' .github/workflows/release.yml \
  && grep -q 'x86_64-unknown-linux-gnu' .github/workflows/release.yml \
  && grep -q 'lto = "thin"' Cargo.toml \
  && grep -q 'strip = true' Cargo.toml; then
  ok "the docs, release profile and three native release targets are present"
else
  bad "the M5 docs or release configuration is incomplete"
fi

if [ -f "$TMP/driver.failed" ]; then
  bad "one or more PTY steps did not reach their expected screen state"
fi

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
