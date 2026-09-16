#!/usr/bin/env bash
# Run the shared Rust gates once, then run the independent milestone scenarios in
# parallel. Set SMART_REVIEW_VALIDATE_PARALLEL=0 for ordered output while debugging.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

if [ "${SMART_REVIEW_SKIP_CARGO:-0}" != "1" ]; then
  printf '\n== shared Cargo gates ==\n'
  cargo fmt --all --check \
    && cargo clippy --all-targets --all-features -- -D warnings \
    && cargo test --all-features \
    && cargo build --bin smart-review \
    || exit 1
fi

export SMART_REVIEW_SKIP_CARGO=1
VALIDATORS=(m0 m1 m2a m2b m3 m4 m5)

if [ "${SMART_REVIEW_VALIDATE_PARALLEL:-1}" = "0" ]; then
  for milestone in "${VALIDATORS[@]}"; do
    printf '\n######## %s ########\n' "$milestone"
    bash "$ROOT/scripts/validate/$milestone.sh" || exit 1
  done
  exit 0
fi

OUTPUT="$(mktemp -d)"
cleanup() { rm -rf "$OUTPUT"; }
trap cleanup EXIT

pids=()
for milestone in "${VALIDATORS[@]}"; do
  bash "$ROOT/scripts/validate/$milestone.sh" >"$OUTPUT/$milestone.log" 2>&1 &
  pids+=("$!")
done

failed=0
for index in "${!VALIDATORS[@]}"; do
  milestone="${VALIDATORS[$index]}"
  if wait "${pids[$index]}"; then
    code=0
  else
    code=$?
    failed=1
  fi
  printf '\n######## %s (exit %d) ########\n' "$milestone" "$code"
  cat "$OUTPUT/$milestone.log"
done

exit "$failed"
