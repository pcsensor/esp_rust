#!/usr/bin/env bash
set -euo pipefail

cargo fmt --check
cargo check --release
cargo check --release --no-default-features --features gateway-node
cargo check --release --no-default-features --features relay-node
cargo check --release --no-default-features --features sensor-node
bash -n scripts/run-release.sh
bash -n scripts/check-demo.sh
