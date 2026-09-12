#!/usr/bin/env bash
#
# M2b validation: the analysis path, from a diff to an ordered review (FR-3.5,
# FR-4.1-4.4, FR-4.6).
#
# Everything is local: a fake catalog server (so the app routes through the
# OpenAI-compatible passthrough, DEC-17) and a scripted fake provider
# (`fake_llm.py`) that answers with the analysis the prompt asked for, with prose, or
# with nothing. The provider records every request it is sent, which is how the
# privacy rules of FR-4.6 are checked on the wire rather than on trust: the diff and
# the changed files must be there, and the repository's `.env` must not be.
#
# Usage: scripts/validate/m2b.sh        (KEEP=1 keeps the temporary directory)
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

# ---------------------------------------------------------------------------
# The fake catalog: one provider, reached through the passthrough, with the
# reasoning options the thinking tests need.
# ---------------------------------------------------------------------------
mkdir -p "$TMP/srv"
python3 - "$TMP/srv/api.json" "$LLM_URL" <<'PY'
import json, sys
path, api = sys.argv[1], sys.argv[2]
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
import sys, urllib.request
urllib.request.urlopen(f"http://127.0.0.1:{sys.argv[1]}/api.json", timeout=1).read(16)
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
FINANCE_API_KEY=super-secret-value-that-must-never-be-sent
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

[llm.active.reasoning]
type = "toggle"
value = true
EOF
}

# A panel is usually closed by the keystrokes that quit the app, so a check about one
# replays to the moment its text was on screen. Grepping the raw capture cannot do it:
# ratatui writes only the cells that changed, so the spaces between words are often
# never written at all and a phrase greps as "Moneynw roundshalfup".
shown() {
  python3 "$ROOT/scripts/validate/screen.py" --cols 160 --rows 40 \
    --path "$1" --when "$2" >/dev/null 2>&1
}

run_tui() {
  local home="$1" keys="$2" waits="$3" log="$4"
  PATH="$FAKE:$PATH" SMART_REVIEW_HOME="$home" FAKE_API_KEY=sk-fake-validation \
    python3 "$ROOT/scripts/validate/drive.py" \
      --cols 160 --rows 40 --log "$log" \
      --ready "Add retry to the webhook dispatcher" \
      --keys "$keys" --waits "$waits" -- \
      "$ROOT/$BIN" --repo acme/service --path "$REPO/clone"
}

step "1/6 build"
if cargo build --quiet 2>"$TMP/build.log"; then
  ok "the debug binary builds"
else
  bad "the debug binary does not build"
  sed -n '1,20p' "$TMP/build.log"
  printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
  exit 1
fi

HOME_MAIN="$TMP/home"
make_home "$HOME_MAIN"

step "2/6 the first analysis"
# `<leader>a` twice: the first press is the FR-4.6 opt-in notice, the second sends.
FRAMES="$TMP/confirm.log"
SCREEN="$(run_tui "$HOME_MAIN" ':pr 141\r~ a~:q\r' 'money\.rs~Nothing has been sent yet~' "$FRAMES")"
if [ ! -s "$TMP/requests.jsonl" ]; then
  ok "the first press sent nothing at all"
else
  bad "the first press called the provider before asking"
fi

if shown "$FRAMES" "Nothing has been sent yet"; then
  ok "the first press asks before sending anything"
else
  bad "nothing asked for confirmation before the first send"
fi
if shown "$FRAMES" "This would send ~[0-9]* tokens"; then
  ok "the estimate names the size and the model"
else
  bad "no size estimate was shown before the first send"
fi
# And now the second press, which sends it. The counter is reset so that the checks
# below are about this run's requests.
FRAMES="$TMP/analysis.log"
: >"$TMP/requests.jsonl"
SCREEN="$(run_tui "$HOME_MAIN" ':pr 141\r~ a~ a~:q\r' 'money\.rs~Nothing has been sent yet~Money now rounds half up~' "$FRAMES")"
if shown "$FRAMES" "Money now rounds half up"; then
  ok "the analysis panel shows the summary"
else
  bad "the panel never showed the analysis"
  printf '%s\n' "$SCREEN" | tail -12
fi

if [ -f "$HOME_MAIN/cache/analysis/github.com/acme/service/pr-141/"*.json ] 2>/dev/null; then
  ok "the analysis is cached under the pull request"
else
  bad "the analysis was not cached"
  printf '  --- cache tree\n'
  find "$HOME_MAIN/cache" -maxdepth 5 2>/dev/null | head -12
fi

if grep -q "repaired\|first answer was not usable" "$HOME_MAIN/logs/smart-review.log" 2>/dev/null; then
  bad "a good answer was repaired"
else
  ok "a good answer needed no repair"
fi

step "3/6 what was sent"
if [ -s "$TMP/requests.jsonl" ]; then
  ok "the provider was called"
else
  bad "the provider was never called"
fi

WIRE="$(python3 - "$TMP/requests.jsonl" <<'PY'
import json, sys
requests = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
blob = json.dumps(requests)
checks = [
    ("the diff reaches the provider", "-" in blob and "+" in blob and "money.rs" in blob),
    ("the changed file's contents reach the provider", "(cents + 5) / 10 * 10" in blob),
    ("the commit messages reach the provider", "Subtract the discount from gross" in blob),
    ("the repository conventions reach the provider", "Always use thiserror" in blob),
    ("the first convention file wins over the README", "Install with cargo" not in blob),
    ("the analysis asked for streaming", any(r["body"].get("stream") for r in requests)),
    ("the system prompt asks for one JSON object", "one JSON object" in blob),
    ("the prompt asks for a review plan", "review_plan" in blob),
]
failed = 0
for name, passed in checks:
    print(("  PASS  " if passed else "  FAIL  ") + name)
    failed += 0 if passed else 1
# The privacy rule is its own check, and its own sentence.
secrets = [s for s in ("super-secret-value-that-must-never-be-sent",) if s in blob]
if secrets:
    print("  FAIL  a committed .env value reached the provider")
    failed += 1
else:
    print("  PASS  a committed .env was never sent")
PY
)"
printf '%s\n' "$WIRE" | grep -v '^$'
PASS=$((PASS + $(printf '%s\n' "$WIRE" | grep -c '^  PASS')))
FAIL=$((FAIL + $(printf '%s\n' "$WIRE" | grep -c '^  FAIL')))

step "4/6 the ordered review"
SCREEN="$(run_tui "$HOME_MAIN" ':pr 141\r~ a~:q\r' 'money\.rs~Money now rounds half up~' "$TMP/order.log")"
if printf '%s' "$SCREEN" | grep -q "recommended order"; then
  ok "the tree is in the recommended order"
else
  bad "the tree never entered the recommended order"
  printf '%s\n' "$SCREEN" | tail -12
fi
if printf '%s' "$SCREEN" | grep -q "1\. domain"; then
  ok "the plan groups are shown with their position"
else
  bad "the plan groups are missing"
fi
if shown "$TMP/order.log" "arithmetic everythi"; then
  ok "each group explains why it is read there"
else
  bad "the group rationale is missing"
fi
if printf '%s' "$SCREEN" | grep -q "plan ·\|/2 plan"; then
  ok "both orders' positions are shown"
else
  bad "the positions in both orders are missing"
fi
# `o` switches to the path order and says so.
SCREEN="$(run_tui "$HOME_MAIN" ':pr 141\r~ a~\e~o~:q\r' 'money\.rs~Money now rounds half up~~path order~' "$TMP/order-toggle.log")"
if printf '%s' "$SCREEN" | grep -q "path order"; then
  ok "o switches to the path order"
else
  bad "o did not switch the order"
  printf '%s\n' "$SCREEN" | tail -12
fi

step "5/6 a cache hit with no network, and what the analysis corrected"
# Stopping the provider proves the panel came from the cache (FR-4.3).
kill "$LLM_PID" 2>/dev/null
LLM_PID=""
sleep 0.5
: >"$TMP/requests.jsonl"
SCREEN="$(run_tui "$HOME_MAIN" ':pr 141\r~ a~:q\r' 'money\.rs~Money now rounds half up~' "$TMP/cached.log")"
if shown "$TMP/cached.log" "Money now rounds half up"; then
  ok "the cached analysis is used with the provider down"
else
  bad "the cached analysis was not used offline"
  printf '%s\n' "$SCREEN" | tail -12
fi
if [ ! -s "$TMP/requests.jsonl" ]; then
  ok "a cache hit made no request"
else
  bad "a cache hit still called the provider"
fi
if shown "$TMP/cached.log" "not in this change"; then
  ok "the file the analysis invented is reported as dropped"
else
  bad "the invented path was not reported"
fi
if shown "$TMP/cached.log" "unclassified"; then
  ok "a file the plan forgot is accounted for"
else
  printf '  note: every changed file was in the plan; the safety net had nothing to do\n'
fi

step "6/6 the paths that must not be silent"
# The provider is answered by a script again, this time with prose.
python3 "$ROOT/scripts/validate/fake_llm.py" "$LLM_PORT" "$TMP/mode" "$TMP/repair.jsonl" \
  >"$TMP/llm-repair.log" 2>&1 &
LLM_PID=$!
sleep 0.5
echo prose-then-good >"$TMP/mode"
: >"$TMP/repair.jsonl"
HOME_REPAIR="$TMP/home-repair"
make_home "$HOME_REPAIR"
SCREEN="$(run_tui "$HOME_REPAIR" ':pr 141\r~ a~ a~:q\r' 'money\.rs~Nothing has been sent yet~Money now rounds half up~' "$TMP/repair.log")"
if shown "$TMP/repair.log" "Money now rounds half up"; then
  ok "prose was repaired into a usable analysis"
else
  bad "the repair path did not produce an analysis"
  printf '%s\n' "$SCREEN" | tail -12
fi
REPAIRS="$(grep -c "could not be used" "$TMP/repair.jsonl" 2>/dev/null | head -1)"
if [ "${REPAIRS:-0}" -ge 1 ]; then
  ok "the retry carried the reason the first answer failed"
else
  bad "the retry did not explain the failure"
fi
if shown "$TMP/repair.log" "needed a second attempt|after a retry"; then
  ok "the interface says a retry happened"
else
  bad "the interface did not mention the retry"
fi

# And now prose twice: the answer must be shown, not swallowed (FR-4.1).
echo prose >"$TMP/mode"
HOME_BAD="$TMP/home-bad"
make_home "$HOME_BAD"
SCREEN="$(run_tui "$HOME_BAD" ':pr 141\r~ a~ a~:q\r' 'money\.rs~Nothing has been sent yet~could not be used~' "$TMP/bad.log")"
if shown "$TMP/bad.log" "could not be used"; then
  ok "an unusable answer is reported with its reason"
else
  bad "an unusable answer was swallowed"
  printf '%s\n' "$SCREEN" | tail -12
fi
if shown "$TMP/bad.log" "I looked at the diff"; then
  ok "the model's own text is shown"
else
  bad "the raw text was not shown"
fi
if [ -z "$(find "$HOME_BAD/cache/analysis" -name '*.json' 2>/dev/null)" ]; then
  ok "nothing unusable was stored as an analysis"
else
  bad "an unusable answer was cached as an analysis"
fi

# The budget: a tiny context must elide files and say so rather than fail.
echo good >"$TMP/mode"
HOME_SMALL="$TMP/home-small"
MAX_CONTEXT=6000 make_home "$HOME_SMALL"
SCREEN="$(run_tui "$HOME_SMALL" ':pr 141\r~ a~ a~:context\r~:q\r' 'money\.rs~Nothing has been sent yet~Money now rounds half up~AGENTS\.md~' "$TMP/small.log")"
if shown "$TMP/small.log" "elided|truncated|reduced"; then
  ok "a budget that does not fit is reported rather than hidden"
else
  bad "the budget did not report what it dropped"
  printf '%s\n' "$SCREEN" | tail -12
fi
if shown "$TMP/small.log" " context "; then
  ok ":context shows the inspector"
else
  bad ":context did not open the inspector"
fi
if shown "$TMP/small.log" "AGENTS.md"; then
  ok "the inspector names what is included"
else
  bad "the inspector does not list what is included"
fi

printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
