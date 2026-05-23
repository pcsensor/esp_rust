#!/usr/bin/env bash
set -euo pipefail

. "$HOME/export-esp.sh"
cargo run --release "$@"
