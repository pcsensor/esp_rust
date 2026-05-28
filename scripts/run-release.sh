#!/usr/bin/env bash
set -euo pipefail

role="${1:-gateway}"
shift || true

case "$role" in
  gateway|gateway-node)
    feature="gateway-node"
    ;;
  relay|relay-node)
    feature="relay-node"
    ;;
  sensor|sensor-node)
    feature="sensor-node"
    ;;
  *)
    echo "usage: $0 [gateway|relay|sensor] [extra cargo args...]" >&2
    exit 2
    ;;
esac

cargo run --release --no-default-features --features "$feature" "$@"
probe-rs reset --chip esp32c3
