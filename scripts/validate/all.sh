#!/usr/bin/env bash
# Run the shared Rust gates once, then run the independent feature scenarios in
# parallel. Set SMART_REVIEW_VALIDATE_PARALLEL=0 for ordered output while debugging.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
source "$ROOT/scripts/validate/common.sh"
validation_mode "$@" || exit $?

printf '\n== terminal harness tests ==\n'
python3 "$ROOT/scripts/validate/test_harness.py" || exit 1

if [ "${SMART_REVIEW_SKIP_CARGO:-0}" != "1" ]; then
  printf '\n== shared Cargo gates ==\n'
  cargo fmt --all --check \
    && cargo clippy --all-targets --all-features -- -D warnings \
    && cargo test --all-features \
    && cargo build --bin smart-review \
    || exit 1
fi

export SMART_REVIEW_SKIP_CARGO=1
VALIDATORS=(
  shell
  pull-requests
  workspace-models
  analysis
  chat
  review-publishing
  review-collaboration
)

if [ "${SMART_REVIEW_VALIDATE_PARALLEL:-1}" = "0" ]; then
  for feature in "${VALIDATORS[@]}"; do
    printf '\n######## %s ########\n' "$feature"
    bash "$ROOT/scripts/validate/$feature.sh" --scenarios-only || exit 1
  done
  exit 0
fi

if [ -n "${SMART_REVIEW_VALIDATION_OUTPUT:-}" ]; then
  OUTPUT="$SMART_REVIEW_VALIDATION_OUTPUT"
  mkdir -p "$OUTPUT"
  OWN_OUTPUT=0
else
  OUTPUT="$(mktemp -d)"
  OWN_OUTPUT=1
fi
cleanup() {
  if [ "$OWN_OUTPUT" -eq 1 ]; then
    rm -rf "$OUTPUT"
  fi
}
trap cleanup EXIT

pids=()
for feature in "${VALIDATORS[@]}"; do
  (
    mkdir -p "$OUTPUT/tmp/$feature"
    started="$(python3 -c 'import time; print(time.monotonic_ns())')"
    SMART_REVIEW_VALIDATION_TMP="$OUTPUT/tmp/$feature" KEEP="$((1 - OWN_OUTPUT))" \
      bash "$ROOT/scripts/validate/$feature.sh" --scenarios-only \
      >"$OUTPUT/$feature.log" 2>&1
    code=$?
    finished="$(python3 -c 'import time; print(time.monotonic_ns())')"
    printf '%s %s\n' "$code" "$(( (finished - started) / 1000000 ))" \
      >"$OUTPUT/$feature.result"
    exit "$code"
  ) &
  pids+=("$!")
done

failed=0
for index in "${!VALIDATORS[@]}"; do
  feature="${VALIDATORS[$index]}"
  wait "${pids[$index]}" || true
  if [ -f "$OUTPUT/$feature.result" ]; then
    read -r code duration_ms <"$OUTPUT/$feature.result"
  else
    code=1
    duration_ms=0
  fi
  if [ "$code" -ne 0 ]; then
    failed=1
  fi
  printf '\n######## %s (exit %d, %d.%03ds) ########\n' \
    "$feature" "$code" "$(( duration_ms / 1000 ))" "$(( duration_ms % 1000 ))"
  cat "$OUTPUT/$feature.log"
done

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    printf '### Feature scenario timing\n\n'
    printf '| Validator | Result | Seconds |\n|---|---:|---:|\n'
    for index in "${!VALIDATORS[@]}"; do
      feature="${VALIDATORS[$index]}"
      if [ -f "$OUTPUT/$feature.result" ]; then
        read -r code duration_ms <"$OUTPUT/$feature.result"
      else
        code=1
        duration_ms=0
      fi
      printf '| %s | %d | %d.%03d |\n' \
        "$feature" "$code" "$(( duration_ms / 1000 ))" "$(( duration_ms % 1000 ))"
    done
  } >>"$GITHUB_STEP_SUMMARY"
fi

exit "$failed"
