# ESP32-C3 Rust Hello World

第一阶段目标：搭建 ESP32-C3 的 `esp-hal` + Embassy 项目骨架，并跑通 `hello world`。

## 第二阶段：管廊传感网答辩 Demo

当前代码已切到 `GOAL.md` 中的三节点 demo 工程骨架。三种固件角色通过 Cargo feature 选择，同一时间只能启用一个角色：

```sh
cargo check --release --no-default-features --features gateway-node
cargo check --release --no-default-features --features relay-node
cargo check --release --no-default-features --features sensor-node
```

默认 feature 是 `gateway-node`，所以 `cargo run --release` 会构建网关节点。

烧录某个角色时使用同样的 feature：

```sh
cargo run --release --no-default-features --features gateway-node
cargo run --release --no-default-features --features relay-node
cargo run --release --no-default-features --features sensor-node
```

也可以使用脚本：

```sh
./scripts/run-release.sh gateway
./scripts/run-release.sh relay
./scripts/run-release.sh sensor
```

完整检查当前 demo 构建：

```sh
./scripts/check-demo.sh
```

默认演示接线先按代码中的集中配置记录，实际接线确认后再调整：

| 模块 | 默认 GPIO | 说明 |
|---|---:|---|
| DX-LR32 UART TX | GPIO21 | ESP32-C3 UART1 TX，接 LoRa 模块 RX |
| DX-LR32 UART RX | GPIO20 | ESP32-C3 UART1 RX，接 LoRa 模块 TX |
| SHT40 SDA | GPIO5 | 仅传感节点使用 |
| SHT40 SCL | GPIO4 | 仅传感节点使用 |
| 有源蜂鸣器 | GPIO10 | 仅网关节点使用，低电平响 |

LoRa 默认配置计划：模块 `DX-LR32-433T22D`，UART 9600 baud、433 MHz、channel 23、空中速率 2400 bps、发射功率 22 dBm、NetID `0x4331`。当前代码启动时会打印这组参数；模块仍按 `manual-before-boot` 处理，也就是先用模块工具或手册命令配置好，再运行固件。运行时写配置命令要等 `docs/DX-LR32-433T22D模组_串口UART_应用指导.pdf` 的命令格式和模块模式脚确认后再打开。

当前已实现的工程代码：

- 统一 LoRa 帧格式：`HELLO`、`JOIN_ACK`、`SYNC/SCHEDULE`、`DATA`、`ALARM`、`ACK`、`HEARTBEAT`，带 CRC16。
- 三节点固定演示拓扑：网关只接纳中继节点入网，中继只接纳传感节点入网，保证答辩时稳定展示 `sensor -> relay -> gateway`。
- 简化 TDMA 参数：10 s 超帧、5 个 2 s slot。
- FTSP-like 时间同步状态：节点维护 `offset_ms`，收到 `SYNC` 后平滑更新。
- DX-LR32 UART transport：通过 UART1 发送编码后的帧。
- LoRa UART 接收轮询：主循环使用 `read_ready()` 非阻塞检查串口；transport 内部带流式组帧缓存，可处理半帧、粘包和前导噪声字节。网关/中继/传感节点会按角色处理 `HELLO`、`JOIN_ACK`、`SYNC`、`DATA`、`ALARM`、`ACK`。
- 在线心跳与确认：已入网的中继/传感节点在维护 slot 发送 `HEARTBEAT`，上级节点返回 `ACK`；传感数据、告警和心跳的 ACK payload 会解析并打印。节点侧维护一个单帧 pending-ACK 窗口，`DATA`、`ALARM`、`HEARTBEAT` 未确认时会在维护 slot 最多重传 3 次。
- SHT40 读取：传感节点通过 I2C0 读取温湿度，CRC8 校验失败或 I2C 失败时退回演示样本。
- 网关蜂鸣器：GPIO10 默认高电平关闭；收到 `ALARM` 后拉低响铃，后续收到普通 `DATA` 时解除告警并关闭蜂鸣器。

当前仍需继续完成的部分：

- 中继转发、告警 ACK 和蜂鸣器联动已接入代码路径，仍需要三块板实测确认 DX-LR32 空口/串口时序、实际延迟和 TDMA slot 长度。
- 可靠传输当前是 demo 级单帧窗口；如果现场需要更高吞吐，再扩展为队列和按帧类型优先级调度。
- DX-LR32 的模块参数写入/查询命令需要按 PDF 手册和实际模块模式确认；当前代码已把配置计划集中到 `src/hardware.rs` 并在启动日志中打印，便于人工核对。

现场三节点联调步骤见 [docs/demo-runbook.md](docs/demo-runbook.md)。

## 当前技术线

- 芯片：ESP32-C3
- Rust target：`riscv32imc-unknown-none-elf`
- 工具链：官方 Rust nightly（RISC-V 原生支持，无需 Xtensa 定制工具链）
- HAL：`esp-hal`
- Async runtime：`esp-rtos` 的 Embassy 集成
- 串口输出：`esp-println`
- panic/backtrace：`esp-backtrace`
- 烧录：默认 `probe-rs`（支持调试）；可选 `espflash`

## 环境准备

ESP32-C3 采用 RISC-V 架构，`riscv32imc-unknown-none-elf` 是 Rust 官方原生支持的 target。因此**无需**安装 Espressif 定制的 Xtensa `esp` 工具链，直接使用官方 Rust 工具链即可。

项目根目录的 `rust-toolchain.toml` 已固定编译器版本：

```toml
[toolchain]
channel = "nightly-2026-05-04"
components = ["rust-src"]
```

### 1. 安装工具链

```sh
rustup toolchain install nightly-2026-05-04 --component rust-src
rustup component add rust-analyzer --toolchain nightly-2026-05-04-aarch64-apple-darwin
```

> 本项目启用了 `build-std = ["core"]`，因此必须安装 `rust-src`。

确认工具链：

```sh
rustup toolchain list
```

应能看到：

```text
stable-aarch64-apple-darwin (default)
nightly-2026-05-04-aarch64-apple-darwin
```

### 2. 安装 probe-rs

日常烧录、串口 monitor 和**断点调试**使用 `probe-rs`：

```sh
cargo install probe-rs-tools --locked
```

验证版本：

```sh
probe-rs --version
```

ESP32-C3 内置 **USB-Serial-JTAG** 控制器，直接通过 USB 连接即可被 `probe-rs` 识别为调试器，无需额外硬件。

更多用法见 [probe-rs 文档](https://probe.rs/docs/tools/probe-rs/)。

### 3. （可选）安装 espflash

如果只需要快速烧录和查看日志，也可使用 `espflash`：

```sh
cargo install espflash --locked
```

验证版本：

```sh
espflash --version
```

### 4. 验证项目构建

```sh
cargo check --release
cargo build --release
```

生成的 ELF 路径：

```sh
target/riscv32imc-unknown-none-elf/release/esp32c3-rust
```

## 构建与烧录

### 使用 probe-rs（默认）

`.cargo/config.toml` 已设置默认 runner：

```toml
[target.riscv32imc-unknown-none-elf]
runner = "probe-rs run --chip ESP32-C3"
```

直接运行：

```sh
cargo run --release
```

等效于：

```sh
probe-rs run --chip ESP32-C3 target/riscv32imc-unknown-none-elf/release/esp32c3-rust
```

如需指定调试器：

```sh
probe-rs run --chip ESP32-C3 --probe <VID:PID> target/riscv32imc-unknown-none-elf/release/esp32c3-rust
```

也可以使用项目脚本：

```sh
./scripts/run-release.sh gateway
./scripts/run-release.sh relay
./scripts/run-release.sh sensor
```

### 使用 espflash（备用）

如需临时使用 `espflash` 运行，可覆盖 runner：

```sh
CARGO_TARGET_RISCV32IMC_UNKNOWN_NONE_ELF_RUNNER="espflash flash --monitor" cargo run --release
```

或在 `.cargo/config.toml` 中将 `runner` 修改为 `espflash flash --monitor`。

## VSCode / rust-analyzer 配置

由于使用官方 Rust 工具链，`rust-analyzer` 与标准 Cargo 完全兼容，不再出现 `--lockfile-path` 等参数错误。

项目已配置 `.vscode/settings.json`：

```json
{
    "rust-analyzer.server.path": "/Users/pcsensor/.rustup/toolchains/nightly-2026-05-04-aarch64-apple-darwin/bin/rust-analyzer",
    "rust-analyzer.cargo.target": "riscv32imc-unknown-none-elf",
    "rust-analyzer.check.allTargets": false,
    "rust-analyzer.procMacro.server": "/Users/pcsensor/.rustup/toolchains/nightly-2026-05-04-aarch64-apple-darwin/libexec/rust-analyzer-proc-macro-srv"
}
```

修改后请在 VSCode 中执行：

1. `Cmd + Shift + P`
2. `Rust Analyzer: Restart Server`

## 串口 / 调试器排查

如果烧录失败：

```sh
# 查看串口设备
ls -l /dev/ttyACM* /dev/ttyUSB*

# 查看 probe-rs 识别的调试器
probe-rs list

# 查看当前用户权限
groups
```

常见处理：

- 开发板未被识别：重新插拔 USB；对于 ESP32-C3，确保连接的是 USB 接口而非 UART 接口。
- 权限不足：把当前用户加入系统串口权限组（如 `dialout`），然后重新登录。
- `probe-rs` 找不到设备：使用 `probe-rs list` 查看可用调试器，并用 `--probe <VID:PID>` 指定。
- 使用 `espflash` 时多个串口设备：使用 `espflash flash --monitor --port <PORT> ...` 指定端口。

## 工具链与烧录方案对比

| 方案 | 适用场景 | 优点 | 缺点 |
|---|---|---|---|
| **probe-rs** | 日常开发、烧录、调试 | 支持断点、单步、查看寄存器；烧录后自动 monitor；通用工具 | 对 ESP 的 JTAG 支持仍在完善 |
| **espflash** | 仅需烧录和查看日志 | 配置简单；自动处理下载模式；monitor 输出直接 | 不支持硬件断点调试 |

推荐：**默认使用 `probe-rs`**，既满足日常烧录和日志查看，也保留断点调试能力。仅需快速烧录时可使用 `espflash`。
