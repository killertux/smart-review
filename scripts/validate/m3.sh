#!/usr/bin/env bash
#
# M3 validation: the conversation, over the same bundle an analysis sends (FR-5.1-5.4).
#
# Everything is local, and the same harness as M2b: a fake catalog (so the app routes
# through the OpenAI-compatible passthrough) and a scripted provider that answers chat
# with prose, a path reference and a `[general]` sentence, or dribbles an answer out
# slowly so it can be stopped halfway. Which is the point: this milestone is about a
# *conversation*, so what is checked is what reaches the provider and what comes back
# into the pane — the correction, the partial answer, the multi-turn history, and the
# fact that a committed `.env` still never leaves.
#
# Usage: scripts/validate/m3.sh         (KEEP=1 keeps the temporary directory)
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

CATALOG_PID=""
LLM_PID=""
cleanup() {
  [ -n "$CATALOG_PID" ] && kill "$CATALOG_PID" 2>/dev/null
  [ -n "$LLM_PID" ] && kill "$LLM_PID" 2>/dev/null
  if [ "${KEEP:-0}" = "1" ]; then
    printf '  note: kept %s\n' "$TMP"
  else
    rm -rf "$TMP"
  fi
}
trap cleanup EXIT

free_port() {
  python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
}

CATALOG_PORT="$(free_port)"
LLM_PORT="$(free_port)"
LLM_URL="http://127.0.0.1:$LLM_PORT/v1"
# Somewhere nothing answers: `dead` is the provider whose request cannot succeed.
DEAD_PORT="$(free_port)"
DEAD_URL="http://127.0.0.1:$DEAD_PORT/v1"

# ---------------------------------------------------------------------------
# The fake catalog: one provider, reached through the passthrough, with the
# reasoning options the thinking tests need.
# ---------------------------------------------------------------------------
mkdir -p "$TMP/srv"
python3 - "$TMP/srv/api.json" "$LLM_URL" "$DEAD_URL" <<'PY'
import json, sys
path, api, dead_api = sys.argv[1], sys.argv[2], sys.argv[3]
document = {
    "fake": {
        "id": "fake",
        "name": "Fake Provider",
        "env": ["FAKE_API_KEY"],
        "api": api,
        "doc": "https://example.invalid/fake",
        "models": {
            "fake-analysis-1": {
                "id": "fake-analysis-1",
                "name": "Fake Analysis 1",
                "family": "fake",
                "reasoning": True,
                "reasoning_options": [
                    {"type": "toggle"},
                    {"type": "effort", "values": ["low", "high"]},
                ],
                "tool_call": True,
                "limit": {"context": 40000, "output": 4000},
                "cost": {"input": 0.1, "output": 0.2},
                "release_date": "2026-01-01",
            }
        },
    }
}

# Two more, for step 8. `deepseek` is a provider the pinned `llm` crate has a *native*
# backend for, and that backend implements no streaming: the route is native, cannot
# stream, and has to reach this same local server through the OpenAI-compatible
# passthrough to answer at all. This is the shape of the bug a real DeepSeek user hit,
# reproduced offline.
#
# `dead` points at a port nothing is listening on: the request fails, and what is being
# checked is that the pane *says so* instead of waiting forever.
document["deepseek"] = {
    "id": "deepseek",
    "name": "DeepSeek",
    "env": ["DEEPSEEK_API_KEY"],
    "api": api,
    "doc": "https://example.invalid/deepseek",
    "models": {
        "deepseek-v4-pro": {
            "id": "deepseek-v4-pro",
            "name": "DeepSeek V4 Pro",
            "family": "deepseek",
            "limit": {"context": 40000, "output": 4000},
            "cost": {"input": 0.1, "output": 0.2},
        }
    },
}
document["dead"] = {
    "id": "dead",
    "name": "Unreachable",
    "env": ["DEAD_API_KEY"],
    "api": dead_api,
    "doc": "https://example.invalid/dead",
    "models": {
        "dead-1": {
            "id": "dead-1",
            "name": "Unreachable 1",
            "family": "dead",
            "limit": {"context": 40000, "output": 4000},
        }
    },
}
json.dump(document, open(path, "w"), indent=1)
PY

( cd "$TMP/srv" && exec python3 -m http.server "$CATALOG_PORT" --bind 127.0.0.1 ) \
  >"$TMP/catalog-server.log" 2>&1 &
CATALOG_PID=$!

# ---------------------------------------------------------------------------
# The scripted provider.
# ---------------------------------------------------------------------------
echo good >"$TMP/mode"
: >"$TMP/requests.jsonl"
python3 "$ROOT/scripts/validate/fake_llm.py" "$LLM_PORT" "$TMP/mode" "$TMP/requests.jsonl" \
  >"$TMP/llm-server.log" 2>&1 &
LLM_PID=$!

for _ in $(seq 1 40); do
  if python3 - "$CATALOG_PORT" "$LLM_PORT" <<'PY' 2>/dev/null
import socket, sys, urllib.request
urllib.request.urlopen(f"http://127.0.0.1:{sys.argv[1]}/api.json", timeout=1).read(16)
with socket.create_connection(("127.0.0.1", int(sys.argv[2])), timeout=1):
    pass
PY
  then break; fi
  sleep 0.25
done

CATALOG_URL="http://127.0.0.1:$CATALOG_PORT/api.json"

# ---------------------------------------------------------------------------
# A repository with a pull request in it, conventions, and a committed secret.
# ---------------------------------------------------------------------------
REPO="$TMP/repo"
mkdir -p "$REPO/origin.git"
git init --bare --quiet -b main "$REPO/origin.git"
git clone --quiet "$REPO/origin.git" "$REPO/clone" 2>/dev/null
git -C "$REPO/clone" config user.email t@example.com
git -C "$REPO/clone" config user.name Test

cat >"$REPO/clone/AGENTS.md" <<'EOF'
# Repository conventions

Always use thiserror for errors. Never round money with f64.
EOF
cat >"$REPO/clone/README.md" <<'EOF'
# Service

Install with cargo. This README must not reach the provider: AGENTS.md wins.
EOF
cat >"$REPO/clone/.env" <<'EOF'
FINANCE_API_KEY=old-secret-diff-sentinel-that-must-never-be-sent
EOF
mkdir -p "$REPO/clone/src/domain" "$REPO/clone/tests"
cat >"$REPO/clone/src/domain/money.rs" <<'EOF'
pub fn round(cents: i64) -> i64 {
    cents
}
EOF
cat >"$REPO/clone/tests/money.rs" <<'EOF'
#[test]
fn it_rounds() {
    assert_eq!(1, 1);
}
EOF
# A generated file, large enough that a small context budget has to reduce or elide
# something. Its content is dull on purpose: what is being tested is the budget, not
# the reader.
python3 - "$REPO/clone/src/domain/generated.rs" <<'PY'
import sys
with open(sys.argv[1], "w") as handle:
    for index in range(6000):
        handle.write(f"pub const GENERATED_{index}: u32 = {index};\n")
PY
git -C "$REPO/clone" add -A
git -C "$REPO/clone" commit --quiet -m "initial"
git -C "$REPO/clone" push --quiet origin main

git -C "$REPO/clone" checkout --quiet -b work
cat >"$REPO/clone/.env" <<'EOF'
FINANCE_API_KEY=new-secret-diff-sentinel-that-must-never-be-sent
EOF
cat >"$REPO/clone/src/domain/money.rs" <<'EOF'
pub fn round(cents: i64) -> i64 {
    (cents + 5) / 10 * 10
}
EOF
cat >"$REPO/clone/tests/money.rs" <<'EOF'
#[test]
fn it_rounds() {
    assert_eq!(round(5), 10);
}
EOF
python3 - "$REPO/clone/src/domain/generated.rs" <<'PY'
import sys
with open(sys.argv[1], "a") as handle:
    handle.write("pub const GENERATED_EXTRA: u32 = 1;\n")
PY
git -C "$REPO/clone" add -A
git -C "$REPO/clone" commit --quiet -m "round half up in money"
PR_SHA="$(git -C "$REPO/clone" rev-parse HEAD)"
git -C "$REPO/clone" push --quiet --force origin HEAD:refs/pull/141/head
git -C "$REPO/clone" checkout --quiet main
git -C "$REPO/clone" branch --quiet -D work

# ---------------------------------------------------------------------------
# The fake `gh`, as in m1.sh: a quoted heredoc, data read from its own directory.
# ---------------------------------------------------------------------------
FAKE="$TMP/fake"
mkdir -p "$FAKE"
cat >"$FAKE/gh" <<'GH'
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
  pr:diff) cat "$here/diff.patch"; exit 0 ;;
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
chmod +x "$FAKE/gh"
cp "$FIXTURES/pr-view.json" "$FAKE/view.json"
printf '%s' "$FIXTURES" >"$FAKE/fixtures"
git -C "$REPO/clone" diff --no-color "main" "$PR_SHA" >"$FAKE/diff.patch"
python3 - "$FAKE/view.json" "$PR_SHA" "$FAKE/diff.patch" <<'PY'
import json, re, sys
path, head, patch = sys.argv[1], sys.argv[2], sys.argv[3]
document = json.load(open(path))
document["headRefOid"] = head
document["baseRefName"] = "main"
document["headRefName"] = "rounding"
# The counts have to describe the same change as the patch, or the metadata block sent
# to the provider would contradict the diff beside it.
text = open(patch).read()
files = re.findall(r"^\+\+\+ b/(.+)$", text, re.M)
document["files"] = [
    {"path": name, "additions": 1, "deletions": 1} for name in files
]
document["additions"] = sum(1 for line in text.splitlines() if line.startswith("+") and not line.startswith("+++"))
document["deletions"] = sum(1 for line in text.splitlines() if line.startswith("-") and not line.startswith("---"))
document["changedFiles"] = len(files)
json.dump(document, open(path, "w"), indent=1)
PY

# ---------------------------------------------------------------------------
# A home that is ready to analyse: a catalog, a chosen model, a key in the
# environment (which shadows the file, FR-4.5).
# ---------------------------------------------------------------------------
make_home() {
  local home="$1"
  mkdir -p "$home"
  # Already agreed to send this repository's context (FR-4.6): the notice is once per
  # repository, and re-testing it in every step would make every step depend on the
  # confirmation's timing rather than on the chat.
  cat >"$home/state.toml" <<'EOF'
analysis_opt_in = ["github.com/acme/service"]
EOF
  cat >"$home/config.toml" <<EOF
[ui]
theme = "dark"

[review]
context_lines = 3

[catalog]
url = "$CATALOG_URL"
ttl_hours = 24

[llm]
max_context_tokens = ${MAX_CONTEXT:-100000}
max_file_bytes = ${MAX_FILE_BYTES:-262144}

[llm.active]
provider = "fake"
model = "fake-analysis-1"
EOF
}

# A panel is usually closed by the keystrokes that quit the app, so a check about one
# replays to the moment its text was on screen. Grepping the raw capture cannot do it:
# ratatui writes only the cells that changed, so the spaces between words are often
# never written at all and a phrase greps as "Moneynw roundshalfup".
# Every step keeps the requests it made: a check that fails three steps later is much
# easier to explain with the wire log from the step that made it than from the last one.
keep_requests() {
  cp "$TMP/requests.jsonl" "$TMP/$1-requests.jsonl" 2>/dev/null || true
}

shown() {
  python3 "$ROOT/scripts/validate/screen.py" --cols 160 --rows 40 \
    --path "$1" --when "$2" >/dev/null 2>&1
}

run_tui() {
  local home="$1" keys="$2" waits="$3" log="$4"
  local driver_code=0
  # Every provider in the fake catalog gets a key: which variable holds it is the
  # catalog's business (FR-4.5), and a step that fails for want of a key would be
  # testing the credential lookup instead of what it names.
  PATH="$FAKE:$PATH" SMART_REVIEW_HOME="$home" \
    FAKE_API_KEY=sk-fake-validation DEEPSEEK_API_KEY=sk-fake-deepseek \
    DEAD_API_KEY=sk-fake-dead \
    python3 "$ROOT/scripts/validate/drive.py" \
      --cols 160 --rows 40 --log "$log" \
      --ready "Add retry to the webhook dispatcher" \
      --keys "$keys" --waits "$waits" -- \
      "$ROOT/$BIN" --repo acme/service --path "$REPO/clone" || driver_code=$?
  if [ "$driver_code" -ne 0 ]; then
    touch "$TMP/driver.failed"
  fi
}

# ---------------------------------------------------------------------------
# The keystrokes a chat run needs, as groups: `~` separates one group per wait, and
# each group's wait is what proves the previous group landed.
#
#   OPEN     `:pr 141` and then Tab, which walks the review screen's three stops
#   TYPE(x)  type a question without sending it
#   SEND     press Enter
#   BACK     Esc, which stops an answer if one is running and otherwise leaves the pane
# ---------------------------------------------------------------------------
OPEN=':pr 141\r~\t'
OPEN_WAIT='money\.rs~'
# The exits are `:q` from the command line rather than `q`, because `q` is a letter in
# the compose box — and a validator that cannot leave a pane cannot check it.
#
# The four groups below are the chat's fixed shape, and each has its own wait: `~` in a
# *wait* list is a group boundary, so a list that is one short silently moves every
# pattern one group earlier (which is how the first version of this checked the
# confirmation screen for an answer that had not been asked for yet).
ASK='why does it round?\r~\r'
# The footer of a *finished* answer, which is what distinguishes it from the estimate
# line of the confirmation (that one says "tokens (" and never "tokens · ").
ASK_WAIT='Nothing has been sent yet~tokens · ~'

step "1/8 build"
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

HOME_MAIN="$TMP/home"
make_home "$HOME_MAIN"

step "2/8 the pane, and typing in it"
# Letters are letters, including the ones bound in normal mode: a question with a `?`
# in it must not open the help popup.
FRAMES="$TMP/typing.log"
SCREEN="$(run_tui "$HOME_MAIN" "$OPEN~what does round do? k?" "$OPEN_WAIT~what does round do\? k\?" "$FRAMES")"
if printf '%s' "$SCREEN" | grep -q "what does round do? k?"; then
  ok "the letters typed in the compose box stayed letters"
else
  bad "the compose box did not take the text"
  printf '%s\n' "$SCREEN" | tail -10
fi
if printf '%s' "$SCREEN" | grep -Fq "[5 Ask]"; then
  ok "the tab bar exposes Ask as the conversation destination"
else
  bad "the Ask tab is not visible"
fi
if printf '%s' "$SCREEN" | grep -q "INSERT"; then
  ok "the status line says the keyboard is in insert mode"
else
  bad "the status line does not say insert mode"
fi
# A newline is added rather than sent, and what was typed survives the keystroke.
FRAMES="$TMP/newline.log"
SCREEN="$(run_tui "$HOME_MAIN" "$OPEN~first line\nsecond line" "$OPEN_WAIT~second line" "$FRAMES")"
if shown "$FRAMES" "2 lines; Enter sends"; then
  ok "the newline was added instead of sending the question"
else
  bad "the modified Enter did not add a line"
  printf '%s\n' "$SCREEN" | tail -8
fi
if [ ! -s "$TMP/requests.jsonl" ]; then
  ok "typing, and adding a line to it, sent nothing"
else
  bad "typing sent something"
fi

step "3/8 the first question asks before it sends"
# A home that has never agreed, which is the state the notice exists for (FR-4.6).
HOME_FRESH="$TMP/fresh"
make_home "$HOME_FRESH"
rm -f "$HOME_FRESH/state.toml"
: >"$TMP/requests.jsonl"
FRAMES="$TMP/confirm.log"
SCREEN="$(run_tui "$HOME_FRESH" "$OPEN~why does it round?\r" \
  "$OPEN_WAIT~Press Enter again to send" "$FRAMES")"
if [ ! -s "$TMP/requests.jsonl" ]; then
  ok "the first question sent nothing at all"
else
  bad "the first question called the provider before asking"
fi
if shown "$FRAMES" "Nothing has been sent yet"; then
  ok "the first question asks before sending anything"
else
  bad "no confirmation was asked for the first send"
fi
if shown "$FRAMES" "This would send ~[0-9,]* tokens"; then
  ok "the estimate names the size of the request"
else
  bad "no size estimate was shown before the first send"
fi
if shown "$FRAMES" "Press Enter again to send"; then
  ok "the confirmation says how to agree"
else
  bad "the confirmation does not say what to press"
fi
# Esc, before agreeing, sends nothing — and the app is stopped by the driver rather than
# by `:q`, because `:` is a letter in a compose box.
run_tui "$HOME_FRESH" "$OPEN~why does it round?\r~\e" \
  "$OPEN_WAIT~Press Enter again to send~" "$TMP/decline.log" >/dev/null
if [ ! -s "$TMP/requests.jsonl" ]; then
  ok "declining sent nothing"
else
  bad "declining sent the question anyway"
fi
keep_requests step3

step "4/8 the question, the context and the answer"
echo chat >"$TMP/mode"
: >"$TMP/requests.jsonl"
FRAMES="$TMP/chat.log"
SCREEN="$(run_tui "$HOME_MAIN" "$OPEN~why does it round?\r" \
  "$OPEN_WAIT~You asked: \"why does it round\?\"" "$FRAMES")"
if shown "$FRAMES" "You asked: \"why does it round\?\""; then
  ok "the answer streams into the pane, quoting the question it answers"
else
  bad "the answer never appeared"
  printf '%s\n' "$SCREEN" | tail -14
fi
if shown "$FRAMES" "in this change: src/domain/money.rs"; then
  ok "a path the answer names is shown as being in this change"
else
  bad "the reference to the changed file is missing"
fi
if shown "$FRAMES" "tokens · ~"; then
  ok "the answer reports what it cost"
else
  bad "the per-answer usage is missing"
fi
if shown "$FRAMES" "integer division truncates"; then
  ok "a general-knowledge sentence is shown"
else
  bad "the general-knowledge sentence is missing"
fi
# The `[general]` marker is the model's own claim about what it grounded; the pane
# removes the marker and styles the line instead, so the check is that the marker is
# not rendered as text.
if shown "$FRAMES" "\[general\]"; then
  bad "the [general] marker was rendered in the pane"
else
  ok "the [general] marker is not shown as text"
fi
CHAT_DIR="$HOME_MAIN/chats/github.com/acme/service/pr-141"
FILES="$(find "$CHAT_DIR" -name '*.json' 2>/dev/null | grep -vc index.json)"
if [ "$FILES" = "1" ]; then
  ok "the conversation is stored under the pull request"
else
  bad "expected one conversation file, found $FILES"
  find "$HOME_MAIN/chats" 2>/dev/null | head -8
fi
if grep -q '"role": *"user"' "$CHAT_DIR"/*.json 2>/dev/null; then
  ok "the question itself is part of the stored conversation"
else
  bad "the stored conversation has no question"
fi

step "5/8 what was sent"
if [ -s "$TMP/requests.jsonl" ]; then
  ok "the provider was asked"
else
  bad "the provider was never asked"
fi

WIRE="$(python3 - "$TMP/requests.jsonl" <<'PY'
import json, sys
requests = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
blob = json.dumps(requests)
first = requests[0]["body"] if requests else {}
messages = first.get("messages", [])
def text_of(message):
    content = message.get("content")
    return content if isinstance(content, str) else json.dumps(content)
system = " ".join(text_of(m) for m in messages if m.get("role") == "system")
last = messages[-1] if messages else {}
checks = [
    ("the chat asked for streaming", bool(first.get("stream"))),
    ("the chat asked for token usage in the stream",
     (first.get("stream_options") or {}).get("include_usage") is True),
    ("the context is in the system prompt, not in a message", "<<<CONTEXT" in system),
    ("the diff reaches the provider", "money.rs" in system and "+" in system),
    ("the changed file's contents reach the provider", "(cents + 5) / 10 * 10" in system),
    ("the commit message reaches the provider", "Subtract the discount from gross" in system),
    ("the repository conventions reach the provider", "Always use thiserror" in system),
    ("the README does not override the conventions", "Install with cargo" not in blob),
    ("the question is the last message", "why does it round?" in text_of(last)),
    ("the question is a user message", last.get("role") == "user"),
    ("nothing was sent as the assistant's own words yet",
     all(m.get("role") != "assistant" for m in messages)),
    ("the system prompt says what the model may not do",
     "no way for you to read files" in system),
    ("the system prompt asks for the general-knowledge marker", "[general]" in system),
]
failed = 0
for name, passed in checks:
    print(("  PASS  " if passed else "  FAIL  ") + name)
    failed += 0 if passed else 1
if any(secret in blob for secret in (
    "old-secret-diff-sentinel-that-must-never-be-sent",
    "new-secret-diff-sentinel-that-must-never-be-sent",
)):
    print("  FAIL  a committed .env value reached the provider")
    failed += 1
else:
    print("  PASS  a committed .env was never sent in a chat request")
PY
)"
printf '%s\n' "$WIRE" | grep -v '^$'
PASS=$((PASS + $(printf '%s\n' "$WIRE" | grep -c '^  PASS')))
FAIL=$((FAIL + $(printf '%s\n' "$WIRE" | grep -c '^  FAIL')))
keep_requests step5

step "6/8 a second question, and stopping one"
# The second question must carry the first exchange: that is what makes this a
# conversation rather than a series of unrelated questions (FR-5.2, FR-5.3). The answer
# quotes the question, which is how the wait tells this answer from the one already in
# the pane.
: >"$TMP/requests.jsonl"
run_tui "$HOME_MAIN" "$OPEN~and the discount?\r" \
  "$OPEN_WAIT~You asked: \"and the discount" "$TMP/twice.log" >/dev/null
SECOND="$(python3 - "$TMP/requests.jsonl" <<'PY'
import json, sys
requests = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
if not requests:
    print("  FAIL  the second question was never sent")
    raise SystemExit(0)
second = json.dumps(requests[0]["body"])
checks = [
    ("the second question is its own request", len(requests) >= 1),
    ("the second request carries the first question", "why does it round?" in second),
    ("the second request carries the first answer", "rounds half up for positive" in second),
    ("the second question comes after it",
     second.index("and the discount?") > second.index("why does it round?")),
    ("the context is not repeated into the history", second.count("CONTEXT>>>") <= 1),
]
failed = 0
for name, passed in checks:
    print(("  PASS  " if passed else "  FAIL  ") + name)
    failed += 0 if passed else 1
PY
)"
printf '%s\n' "$SECOND" | grep -v '^$'
PASS=$((PASS + $(printf '%s\n' "$SECOND" | grep -c '^  PASS')))
FAIL=$((FAIL + $(printf '%s\n' "$SECOND" | grep -c '^  FAIL')))

# A slow answer, stopped halfway: the text that arrived stays, and it is marked.
echo chat-slow >"$TMP/mode"
FRAMES="$TMP/stop.log"
run_tui "$HOME_MAIN" "$OPEN~explain the rounding\r~\e" \
  "$OPEN_WAIT~You asked: \"explain the rounding~model \\(stopped\\)" "$FRAMES" >/dev/null
if shown "$FRAMES" "model \\(stopped\\)"; then
  ok "a stopped answer is marked as stopped"
else
  bad "the pane did not say the answer was stopped"
fi
if shown "$FRAMES" "explain the rounding"; then
  ok "the question whose answer was stopped is still there"
else
  bad "stopping the answer lost the question"
fi
if grep -q '"partial": *true' "$CHAT_DIR"/*.json 2>/dev/null; then
  ok "the stored conversation says the answer was stopped"
else
  bad "the stored conversation does not mark the partial answer"
fi
keep_requests step6

step "7/8 the conversation survives, and can be exported"
# Opening the pull request again reads the conversation back from the store: no typing,
# no question, and what was said last is on screen.
echo chat >"$TMP/mode"
FRAMES="$TMP/restart.log"
SCREEN="$(run_tui "$HOME_MAIN" "$OPEN" 'money\.rs~explain the rounding' "$FRAMES")"
if printf '%s' "$SCREEN" | grep -q "explain the rounding"; then
  ok "opening the pull request again shows the conversation"
else
  bad "the conversation did not survive"
  printf '%s\n' "$SCREEN" | tail -12
fi
SESSIONS="$(find "$HOME_MAIN/chats" -name '*.json' 2>/dev/null | grep -vc index.json)"
if [ "$SESSIONS" = "1" ]; then
  ok "one conversation was stored, under the pull request"
else
  bad "expected one stored conversation, found $SESSIONS"
  find "$HOME_MAIN/chats" 2>/dev/null | head -8
fi
if [ -f "$CHAT_DIR/index.json" ]; then
  ok "the list is stored beside it"
else
  bad "the conversation list was not written"
fi
: >"$TMP/requests.jsonl"
# `:chat list`: the compose box owns the keyboard, so leaving it is part of using the
# command line — the first version of this typed `:chat list` into the question.
FRAMES="$TMP/list.log"
SCREEN="$(run_tui "$HOME_MAIN" "$OPEN~\e~:chat list\r" "$OPEN_WAIT~~session\\(s\\)" "$FRAMES")"
if printf '%s' "$SCREEN" | grep -q "session(s)"; then
  ok ":chat list lists the conversations"
else
  bad ":chat list showed nothing"
  printf '%s\n' "$SCREEN" | tail -8
fi
if [ ! -s "$TMP/requests.jsonl" ]; then
  ok "leaving the compose box and listing sent no question"
else
  bad "a command typed after leaving the box was sent as a question"
fi
# The transcript goes to `exports/`, not to `cache/`: a transcript the user asked for is
# not disposable (FR-8.5).
FRAMES="$TMP/export.log"
run_tui "$HOME_MAIN" "$OPEN~\e~:chat export md\r" "$OPEN_WAIT~~wrote the transcript" "$FRAMES" >/dev/null
EXPORTED="$(find "$HOME_MAIN/exports" -name '*.md' 2>/dev/null | head -1)"
if [ -n "$EXPORTED" ]; then
  ok ":chat export wrote a transcript under exports/"
else
  bad ":chat export wrote nothing"
fi
if [ -n "$EXPORTED" ] && grep -q "why does it round?" "$EXPORTED"; then
  ok "the transcript holds the first question"
else
  bad "the transcript is missing the first question"
fi
if [ -n "$EXPORTED" ] && grep -q "and the discount?" "$EXPORTED"; then
  ok "the transcript holds the second question"
else
  bad "the transcript is missing the second question"
fi
if [ -n "$EXPORTED" ] && grep -q "## model" "$EXPORTED"; then
  ok "the transcript holds the answers"
else
  bad "the transcript is missing the answers"
fi
if [ -n "$EXPORTED" ] && grep -q "rounds half up for positive" "$EXPORTED"; then
  ok "the transcript holds the answer text, in full"
else
  bad "the transcript's answers are incomplete"
fi
step "8/8 a provider this crate cannot stream, and one that does not answer"
# The user's report, offline. `deepseek` has a native backend in the pinned crate which
# implements no streaming, so the old code asked for the structured stream, got the
# crate's refusal, and — because `is_current_job` did not know the chat slot — dropped
# the failure on the floor: the pane said "asking deepseek-v4-pro" forever.
#
# Both halves of that are checked here. The answer must arrive (through the passthrough,
# which streams), and a provider that genuinely fails must *say* so.

deepseek_home() {
  local home="$1"
  make_home "$home"
  python3 - "$home/config.toml" <<'PY'
import sys
path = sys.argv[1]
text = open(path).read()
text = text.replace('provider = "fake"', 'provider = "deepseek"')
text = text.replace('model = "fake-analysis-1"', 'model = "deepseek-v4-pro"')
open(path, "w").write(text)
PY
}

HOME_DEEPSEEK="$TMP/home-deepseek"
deepseek_home "$HOME_DEEPSEEK"
echo chat >"$TMP/mode"
: >"$TMP/requests.jsonl"
FRAMES="$TMP/deepseek.log"
SCREEN="$(run_tui "$HOME_DEEPSEEK" "$OPEN~why does it round?\r" \
  "$OPEN_WAIT~You asked: \"why does it round\?\"" "$FRAMES")"
if printf '%s' "$SCREEN" | grep -qi "not supported for this provider"; then
  bad "the answer never came: the crate's refusal was shown instead"
  printf '%s\n' "$SCREEN" | tail -8
elif printf '%s' "$SCREEN" | grep -q "rounds half up for positive"; then
  ok "a provider with no streaming of its own still answered"
else
  bad "the question produced no answer"
  printf '%s\n' "$SCREEN" | tail -12
fi
# And it *streamed*: the passthrough request asks for `stream`, so the answer arrives in
# pieces rather than as one block. Without this the check above would also pass if the
# fallback had been a single un-streamed request — which is what Groq and Mistral get.
if python3 - "$TMP/requests.jsonl" <<'PY'
import json, sys
bodies = [json.loads(line)["body"] for line in open(sys.argv[1]) if line.strip()]
streamed = [b for b in bodies if b.get("stream") and b.get("model") == "deepseek-v4-pro"]
sys.exit(0 if streamed else 1)
PY
then
  ok "the provider that cannot stream was reached through /chat/completions"
else
  bad "the streaming fallback did not use the compatible route"
  head -4 "$TMP/requests.jsonl"
fi
keep_requests step8-deepseek

# A provider that cannot be reached at all: the failure has to be *visible*. This is the
# other half of the same bug — before it, this run sat at "asking dead-1" until the
# timeout, with the reason only in the log.
dead_home() {
  local home="$1"
  make_home "$home"
  python3 - "$home/config.toml" <<'PY'
import sys
path = sys.argv[1]
text = open(path).read()
text = text.replace('provider = "fake"', 'provider = "dead"')
text = text.replace('model = "fake-analysis-1"', 'model = "dead-1"')
open(path, "w").write(text)
PY
}

HOME_DEAD="$TMP/home-dead"
dead_home "$HOME_DEAD"
FRAMES="$TMP/dead.log"
# The wait names the failure itself: if the pane never says it, the step times out
# rather than reporting a passing run that proves nothing.
SCREEN="$(run_tui "$HOME_DEAD" "$OPEN~why does it round?\r" "$OPEN_WAIT~failed: " "$FRAMES")"
# Asserted on the *pane*, not on the notice: a notice says "the question failed" and
# expires after six seconds, so grepping for "failed: " would be satisfied by a flash the
# user may never read — and it was, when the pane itself drew nothing.
if shown "$FRAMES" "model \\(failed\\)"; then
  ok "the chat pane marks the failed request"
else
  bad "the pane did not mark the failure"
  printf '%s\n' "$SCREEN" | tail -10
fi
# "Still asking" is about the *last* frame, not about any frame: `shown` answers "was
# this ever on screen", and it certainly was, a second before the failure.
if printf '%s' "$SCREEN" | grep -qi "asking dead-1"; then
  bad "the pane is still saying it is asking"
else
  ok "the pane stopped saying it was asking"
fi
# The reason in the transport's own words, which is what a person acts on.
if shown "$FRAMES" "error sending request"; then
  ok "the reason is shown in the pane, not just the word failed"
else
  bad "the pane shows no reason"
  printf '%s\n' "$SCREEN" | tail -10
fi
keep_requests step8-dead

# The same failure on the *analysis* path, which is a different slot in the reducer and
# a different pane: the user reported this one too ("nor can I see anything in the
# analysis"). A dead provider has to leave the panel saying so rather than "asking the
# provider" forever.
FRAMES="$TMP/dead-analysis.log"
SCREEN="$(run_tui "$HOME_DEAD" ':pr 141\r~ a~ a' \
  'money\.rs~~the provider did not answer' "$FRAMES")"
if shown "$FRAMES" "the provider did not answer"; then
  ok "a failed analysis is shown in the panel"
else
  bad "the analysis panel did not report the failure"
  printf '%s\n' "$SCREEN" | tail -10
fi
if shown "$FRAMES" "failed: "; then
  ok "the panel names the failure in its title"
else
  bad "the panel title does not say the analysis failed"
fi
if printf '%s' "$SCREEN" | grep -q "asking the provider"; then
  bad "the panel is still saying it is asking the provider"
else
  ok "the panel stopped saying it was asking"
fi
keep_requests step8-dead-analysis

if [ -f "$TMP/driver.failed" ]; then
  bad "one or more PTY steps did not reach their expected screen state"
fi

printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
[ "$FAIL" = "0" ]
