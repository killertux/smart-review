#!/usr/bin/env bash
# Shared command-line contract for milestone validators (IR-15).

validation_mode() {
  case "$#:${1:-}" in
    0:)
      ;;
    1:--scenarios-only)
      export SMART_REVIEW_SKIP_CARGO=1
      ;;
    *)
      printf 'usage: %s [--scenarios-only]\n' "$0" >&2
      return 2
      ;;
  esac
}

stop_child() {
  local pid="${1:-}"
  if [ -n "$pid" ]; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
}
