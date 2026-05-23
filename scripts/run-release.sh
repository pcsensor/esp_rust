#!/usr/bin/env bash
set -euo pipefail

. /home/pcsensor/export-esp.sh
cargo run --release "$@"
