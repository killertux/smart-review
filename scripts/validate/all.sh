#!/usr/bin/env bash
# Run the shared Rust gates once, then run the small terminal/process smoke contracts
# in parallel. Set SMART_REVIEW_FULL_VALIDATION=1 to retain the historical broad PTY
# suite while IR-18's deterministic coverage is reviewed.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
source "$ROOT/scripts/validate/common.sh"
validation_mode "$@" || exit $?

HARNESS_STARTED="$(python3 -c 'import time; print(time.monotonic_ns())')"
printf '\n== terminal harness tests ==\n'
python3 "$ROOT/scripts/validate/test_harness.py" || exit 1
HARNESS_FINISHED="$(python3 -c 'import time; print(time.monotonic_ns())')"
HARNESS_MS="$(( (HARNESS_FINISHED - HARNESS_STARTED) / 1000000 ))"
RUST_TEST_MS="${SMART_REVIEW_RUST_TEST_MS:-}"

if [ "${SMART_REVIEW_SKIP_CARGO:-0}" != "1" ]; then
  printf '\n== shared Cargo gates ==\n'
  cargo fmt --all --check || exit 1
  cargo clippy --all-targets --all-features -- -D warnings || exit 1
  RUST_TEST_STARTED="$(python3 -c 'import time; print(time.monotonic_ns())')"
  cargo test --all-features || exit 1
  RUST_TEST_FINISHED="$(python3 -c 'import time; print(time.monotonic_ns())')"
  RUST_TEST_MS="$(( (RUST_TEST_FINISHED - RUST_TEST_STARTED) / 1000000 ))"
  cargo build --bin smart-review || exit 1
fi

export SMART_REVIEW_SKIP_CARGO=1
if [ "${SMART_REVIEW_FULL_VALIDATION:-0}" = "1" ]; then
  VALIDATOR_MODE="--scenarios-only"
  VALIDATORS=(
    shell
    pull-requests
    workspace-models
    analysis
    chat
    review-publishing
    review-collaboration
  )
else
  VALIDATOR_MODE="--smoke-only"
  VALIDATORS=(
    shell
    pull-requests
    workspace-models
    chat
    review-publishing
    review-collaboration
  )
fi

if [ "${SMART_REVIEW_VALIDATE_PARALLEL:-1}" = "0" ]; then
  for feature in "${VALIDATORS[@]}"; do
    printf '\n######## %s ########\n' "$feature"
    bash "$ROOT/scripts/validate/$feature.sh" "$VALIDATOR_MODE" || exit 1
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
      bash "$ROOT/scripts/validate/$feature.sh" "$VALIDATOR_MODE" \
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
    printf '### Deterministic test timing\n\n'
    printf '| Stage | Seconds |\n|---|---:|\n'
    printf '| Terminal harness unit tests | %d.%03d |\n' \
      "$(( HARNESS_MS / 1000 ))" "$(( HARNESS_MS % 1000 ))"
    if [ -n "$RUST_TEST_MS" ]; then
      printf '| Rust unit/scenario tests | %d.%03d |\n' \
        "$(( RUST_TEST_MS / 1000 ))" "$(( RUST_TEST_MS % 1000 ))"
    fi
    printf '\n'
    if [ "$VALIDATOR_MODE" = "--smoke-only" ]; then
      printf '### PTY smoke timing\n\n'
    else
      printf '### Full feature-validator timing\n\n'
    fi
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
