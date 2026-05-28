#!/usr/bin/env bash
set -euo pipefail

elf="target/riscv32imc-unknown-none-elf/release/esp32c3-rust"

if [ ! -f "$elf" ]; then
    echo "ELF not found at $elf — build first: ./scripts/run-release.sh gateway" >&2
    exit 1
fi

echo "attaching to ESP32-C3 (Ctrl+C to exit) ..."
probe-rs attach --chip esp32c3 "$elf"
