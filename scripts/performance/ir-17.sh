#!/usr/bin/env bash
# Reproducible opt-in IR-17 workloads. This is intentionally outside the default gate:
# timings are meaningful only in release mode on an otherwise quiet reference machine.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

printf 'IR-17 reference workloads\n'
printf 'machine: %s\n' "$(uname -a)"
printf 'rustc: %s\n' "$(rustc --version)"
printf 'cargo: %s\n' "$(cargo --version)"

command=(cargo test --release ir_17_reference_ --all-features -- --ignored --nocapture --test-threads=1)
if [ -x /usr/bin/time ]; then
  case "$(uname -s)" in
    Darwin) /usr/bin/time -l "${command[@]}" ;;
    *) /usr/bin/time -v "${command[@]}" ;;
  esac
else
  "${command[@]}"
fi

printf '\nDeterministic IR-17 contracts\n'
cargo test ir_17_ --all-features -- --skip ir_17_reference_
