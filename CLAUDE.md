# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build and test commands

- `cargo check --release` — default build for the embedded target using the default `gateway-node` feature.
- `cargo check --release --no-default-features --features gateway-node` — check the gateway firmware.
- `cargo check --release --no-default-features --features relay-node` — check the relay firmware.
- `cargo check --release --no-default-features --features sensor-node` — check the sensor firmware.
- `cargo build --release --no-default-features --features <gateway-node|relay-node|sensor-node>` — produce the ELF for a specific role.
- `cargo test-host` — run host-side unit tests for the hardware-independent logic on macOS (`.cargo/config.toml` overrides the embedded default target and rebuilds std/test from source).
- `cargo fmt --check` — formatting check.
- `./scripts/check-demo.sh` — repo sanity check: formatting, embedded role builds, and script syntax.

## Flashing and running

- `.cargo/config.toml` sets the default build target to `riscv32imc-unknown-none-elf` and the runner to `probe-rs download --chip esp32c3`.
- `cargo run --release --no-default-features --features <role>` downloads firmware for that role to an ESP32-C3.
- `./scripts/run-release.sh gateway|relay|sensor` is the normal flash command during demo work; it runs the matching Cargo feature and then resets the chip.
- `./scripts/monitor.sh` attaches `probe-rs` to `target/riscv32imc-unknown-none-elf/release/esp32c3-rust` for live monitoring after a build.
- Host `cargo test` is not the normal path here; use `cargo test-host` because the repo default target is bare-metal.

## Toolchain constraints

- Rust toolchain is pinned in `rust-toolchain.toml` to `nightly-2026-05-04` with `rust-src` required.
- The project uses `build-std = ["core"]` for embedded builds.
- `build.rs` only emits `-Tlinkall.x` for `target_os = "none"`, so host-side tests can still link with the system toolchain.

## Architecture overview

This repo is a three-role ESP32-C3 firmware demo for a fixed LoRa topology:

- `gateway-node`
- `relay-node`
- `sensor-node`

Exactly one role feature must be enabled at a time. The default feature is `gateway-node`.

### Code split: pure protocol/demo logic vs hardware-specific runtime

The important design choice is that most network behavior is host-testable pure Rust, while HAL-dependent code is compiled only for embedded targets.

- `src/lib.rs` exposes the shared modules and gates `hardware`, `sensors`, and `transport` behind `target_os = "none"`.
- Host-testable logic lives mainly in:
  - `src/demo.rs` — node state machine, frame creation, join/sync handling, dedup, alarm latch behavior.
  - `src/protocol.rs` — on-wire frame format, payload encoders/decoders, CRC16, streaming frame decoder.
  - `src/tdma.rs` — TDMA schedule layout and FTSP-like time synchronization smoothing.
  - `src/relay.rs` — bounded relay store-and-forward queue.
  - `src/role.rs` — static role IDs, parent relationships, default slots, and compile-time feature selection.
  - `src/demo_log.rs` — structured gateway demo logging and stats.
- Embedded-only runtime lives in:
  - `src/main.rs` — Embassy/ESP runtime, UART/I2C setup, async task orchestration, per-slot behavior, pending ACK retries, and gateway buzzer signaling.
  - `src/transport.rs` — DX-LR32 UART transport and boot-time AT configuration.
  - `src/sensors.rs` — SHT40 reads with CRC8 validation.
  - `src/hardware.rs` — centralized GPIO assignments, LoRa module config plan, SHT40/buzzer thresholds.

When changing protocol, TDMA behavior, forwarding, ACK logic, or node state transitions, prefer editing the pure modules first and keep `main.rs` as orchestration glue.

### Network model

The demo intentionally enforces a fixed topology instead of a general mesh:

- gateway accepts only relay joins
- relay accepts only sensor joins
- sensor sends upward through relay

Static node IDs and role defaults are in `src/role.rs`:

- gateway = 1
- relay = 2
- sensor = 3
- broadcast = `0xff`
- network ID = `0x4331`

### Frame protocol

`src/protocol.rs` defines a compact binary frame with:

- magic/version
- net/src/dst/role/zone metadata
- frame type
- per-hop sequence number
- gateway-relative timestamp
- payload
- CRC16-CCITT

Frame types used by the demo:

- `HELLO`
- `JOIN_ACK`
- `SYNC`
- `SCHEDULE`
- `DATA`
- `ALARM`
- `ACK`
- `HEARTBEAT`

Important sequencing detail from `src/demo.rs`:

- generic frame `seq` is separate from `data_seq`, `heartbeat_seq`, and `sync_seq`
- this keeps `DATA/ALARM` origin sequence accounting stable even when control traffic is interleaved

`FrameStreamDecoder` in `src/protocol.rs` is the key RX-side primitive for UART LoRa input: it tolerates partial frames, sticky packets, leading noise, and buffer resynchronization on the frame magic byte.

### TDMA and time sync

`src/tdma.rs` defines the fixed demo schedule:

- 8 s superframe
- 8 slots of 1 s each
- per slot: 100 ms guard before, 700 ms active TX window, 200 ms trailing quiet time

Slot ownership is semantic, not dynamic:

- slot 0: gateway `SYNC/SCHEDULE`
- slot 1: relay control / deferred sync forward
- slot 2: sensor sampling plus `DATA/ALARM`
- slot 3: relay forwarding to gateway
- slot 4: alarm retry slot
- slot 5: relay heartbeat
- slot 6: sensor heartbeat
- slot 7: quiet/reserved

The clock model is gateway-authoritative. Non-gateway nodes are considered unsynced until they receive their first valid `SYNC`. `TimeSync::apply_sync()` uses a smoothed offset estimate and tracks `offset_delta_ms` for drift reporting.

A key behavioral rule is slot-strict reception after sync: `DemoNode::accepts_frame_slot()` rejects periodic traffic that claims to arrive outside the sender's assigned slot. Control/bootstrap frames are exempt.

### Runtime behavior in `src/main.rs`

`src/main.rs` now uses Embassy tasks with message passing instead of a single polling loop:

- `lora_rx_task` owns the async UART RX half plus `FrameStreamDecoder`, awaits `read_async`, and sends decoded frames to `RX_FRAMES`.
- the core loop owns `DemoNode`, protocol state, pending ACKs, relay buffers, gateway stats, and the async UART TX half.
- on sensor builds, `sensor_task` owns SHT40 plus `AlarmLatch`, publishes the latest sample through `SENSOR_LATEST`, and sends raised/cleared transitions through `ALARM_EVENTS`.
- on gateway builds, `buzzer_task` owns GPIO10 and waits for `BUZZER_SIG` actions from core.

Important runtime patterns:

- unsynced relay/sensor nodes do not follow TDMA yet; they stay RX-heavy and retry `HELLO` on a local timer
- once synced and joined, TX only happens in the slot active window
- RX is interrupt-driven and independent of TX timing, so UART FIFO draining does not stop during TDMA waits or sends
- core uses `select`/`select3` to wake on decoded frames, sensor alarm transitions, or the next TX/HELLO timer
- `last_tx_slot` ensures each slot's TX action runs at most once even if RX wakes core multiple times inside the same slot
- the relay defers forwarding into its scheduled slots instead of sending inline on receipt
- shared task communication uses `CriticalSectionRawMutex`; this is intentionally kept instead of `NoopRawMutex` while the firmware runs under `esp-rtos` and may interact with runtime-managed thread/interrupt contexts

### Reliability model

The demo is intentionally asymmetric:

- `ALARM` uses hop-by-hop ACK with bounded retry (`PendingAck` in `src/main.rs`)
- `DATA` and `HEARTBEAT` are periodic best-effort traffic
- best-effort transmissions are repeated twice with a short gap, and receivers de-duplicate on `(src_id, seq)`
- relay buffering for normal `DATA` is bounded FIFO in `src/relay.rs`; when full, the oldest buffered frame is dropped so newer samples survive

This means changes to reliability should be made carefully: there is currently only one pending ACK window, tuned for demo-scale alarm delivery rather than general queued reliable transport.

### Hardware assumptions

Central hardware defaults are in `src/hardware.rs`:

- LoRa UART: TX GPIO21, RX GPIO20
- SHT40: SDA GPIO5, SCL GPIO4
- gateway buzzer: GPIO10, active low

The code assumes a DX-LR32-433T22D LoRa module used as a transparent UART pipe. At boot, `src/transport.rs` tries to enter AT mode and apply the runtime config sequence from `DX_LR32_DEMO_AT_SEQUENCE`. If AT mode is unavailable, firmware logs a warning and continues with the module's existing transparent-mode config.

Keep LoRa module parameters aligned with the firmware assumptions in `src/hardware.rs` and README/demo runbook: 9600 baud, channel 0 / 433.15 MHz, 2148 bps air rate, 22 dBm, 64-byte packet size, key disabled, RSSI append disabled, LBT disabled.

### Sensor and alarm behavior

Only the sensor role reads SHT40 data, and the read happens in `sensor_task` roughly once per TDMA superframe. `src/sensors.rs` performs a high-precision measurement and verifies both CRC bytes. If an SHT40 read fails, `src/main.rs` falls back to a demo sample so the protocol demo can still run.

Alarm thresholds and hysteresis come from `Sht40Config::DEFAULT` in `src/hardware.rs`:

- raise at >= 30.00 C or >= 80.00%
- clear only after < 29.00 C and < 75.00%

The core sensor slot uses the latest sample published by `sensor_task`. Alarm raised/cleared transitions are delivered through `ALARM_EVENTS` so they are not overwritten by newer samples; clearing an alarm immediately cancels any pending local ALARM retry.

The gateway buzzer turns on for `ALARM` and turns off when a later normal `DATA` arrives. GPIO10 is owned by `buzzer_task`; core only sends `BUZZER_SIG` actions.

## Files worth reading first for behavioral changes

- `src/main.rs` — actual runtime scheduling and role behavior
- `src/demo.rs` — state machine and frame semantics
- `src/protocol.rs` — on-wire format and stream decoding
- `src/tdma.rs` — slot ownership and time sync rules
- `docs/demo-runbook.md` — expected field procedure and logs for the three-board demo

## Practical guidance for edits

- Prefer preserving the split between host-testable logic and embedded HAL code.
- When adding behavior that can be unit-tested, put it in `demo.rs`, `protocol.rs`, `tdma.rs`, or `relay.rs` rather than directly in `main.rs`.
- When validating logic changes locally, run `cargo test-host` plus the relevant `cargo check --release --no-default-features --features <role>` commands.
- For anything affecting the live demo flow, also review `docs/demo-runbook.md` because log wording and expected sequencing matter during board bring-up and presentation.
