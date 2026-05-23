# ESP32-S3 N16R8 Rust Hello World

第一阶段目标：搭建 ESP32-S3 N16R8 的 `esp-hal` + Embassy 项目骨架，并跑通 `hello world`。

## 当前技术线

- 芯片：ESP32-S3 N16R8
- Rust target：`xtensa-esp32s3-none-elf`
- HAL：`esp-hal`
- Async runtime：`esp-rtos` 的 Embassy 集成
- 串口输出：`esp-println`
- panic/backtrace：`esp-backtrace`

## 环境准备

当前项目需要 ESP Rust Xtensa 工具链。项目根目录的 `rust-toolchain.toml` 指定了自定义 `esp` toolchain：

```toml
[toolchain]
channel = "esp"
components = ["rust-src"]
```

如果本机还没有安装 `esp` toolchain，直接在项目目录执行 `cargo check` 会失败：

```text
error: custom toolchain 'esp' specified in override file '.../rust-toolchain.toml' is not installed
```

本机环境安装过程如下。

### 1. 安装 espup

因为项目目录已经指定 `esp` toolchain，而此时该 toolchain 尚未安装，所以安装全局工具时需要显式使用 stable toolchain：

```sh
RUSTUP_TOOLCHAIN=stable cargo install espup --locked
```

本次安装版本：

```text
espup v0.17.1
```

### 2. 安装 ESP Rust Xtensa 工具链

执行：

```sh
espup install
```

本次安装内容包括：

- Xtensa Rust 1.95.0.0 toolchain
- `rust-src` component
- `xtensa-esp-elf` GCC
- Xtensa LLVM
- RISC-V Rust targets for stable toolchain

安装成功后，`espup` 会生成环境变量导出脚本：

```sh
/Users/pcsensor/export-esp.sh
```

每次打开新终端后，进入本项目开发前需要加载：

```sh
. "$HOME/export-esp.sh"
```

确认 `esp` toolchain 已安装：

```sh
rustup toolchain list
```

应能看到类似输出：

```text
stable-aarch64-apple-darwin (default)
esp (active)
```

### 3. 安装 espflash

烧录和串口 monitor 使用 `espflash`。同样建议显式使用 stable toolchain 安装：

```sh
RUSTUP_TOOLCHAIN=stable cargo install espflash --locked
```

本次安装版本：

```sh
espflash --version
```

```text
espflash 4.4.0
```

### 4. 验证项目构建

加载 ESP 环境变量后执行：

```sh
. "$HOME/export-esp.sh"
cargo check
```

`esp-hal` 会提示建议使用 release profile。推荐继续验证 release profile：

```sh
. "$HOME/export-esp.sh"
cargo check --release
cargo build --release
```

本机已验证：

```text
cargo check --release
Finished `release` profile [optimized] target(s)

cargo build --release
Finished `release` profile [optimized] target(s)
```

生成的 ELF 路径：

```sh
target/xtensa-esp32s3-none-elf/release/esp32s3-n16r8-rust
```

## 构建与烧录

每次打开新终端后先加载 ESP 工具链环境：

```sh
. "$HOME/export-esp.sh"
```

```sh
cargo check
cargo run --release
```

也可以直接使用项目脚本，它会自动加载 ESP 工具链环境：

```sh
./scripts/run-release.sh
```

项目脚本使用当前用户 home 下的 `export-esp.sh`：

```sh
. "$HOME/export-esp.sh"
```

`.cargo/config.toml` 已设置默认 target 和 runner，`cargo run --release` 会调用：

```sh
espflash flash --monitor
```

如果有多个串口设备，直接指定端口：

```sh
espflash flash --monitor --port /dev/ttyACM0 target/xtensa-esp32s3-none-elf/release/esp32s3-n16r8-rust
```

## VSCode / rust-analyzer 兼容性说明

某些较新的 `rust-analyzer` 版本在打开本项目时，可能会出现类似下面的日志：

```text
`cargo metadata` failed
error: unexpected argument '--lockfile-path' found
```

### 问题成因

- 本项目通过 `rust-toolchain.toml` 固定使用 `esp` 工具链。
- `rust-analyzer` 在分析项目时会调用 `cargo metadata`。
- 某些新版 `rust-analyzer` 会附带 `--lockfile-path` 参数。
- 但 Espressif 提供的 `esp` 工具链中的 Cargo 并不支持这个参数。
- 结果就是 IDE 日志里持续出现 warning，并且可能退化为 `--no-deps` 模式，影响依赖解析、跳转和补全体验。

### 解决方案：使用稍旧的 rust-analyzer

推荐安装一个较早、与当前 ESP 工具链兼容性更好的 `rust-analyzer`，例如：

```sh
rustup toolchain install nightly-2026-05-04-aarch64-apple-darwin --component rust-analyzer
```

如果希望使用更通用的写法，也可以：

```sh
rustup toolchain install nightly-2026-05-04 --component rust-analyzer
```

安装后查询实际二进制路径：

```sh
rustup which rust-analyzer --toolchain nightly-2026-05-04-aarch64-apple-darwin
```

然后在项目的 `.vscode/settings.json` 中指定这个旧版 `rust-analyzer`：

```json
{
    "rust-analyzer.server.path": "/path/to/rust-analyzer",
    "rust-analyzer.cargo.target": "xtensa-esp32s3-none-elf",
    "rust-analyzer.check.allTargets": false
}
```

本项目当前可用示例：

```json
{
    "rust-analyzer.server.path": "/Users/pcsensor/.rustup/toolchains/nightly-2026-05-04-aarch64-apple-darwin/bin/rust-analyzer",
    "rust-analyzer.cargo.target": "xtensa-esp32s3-none-elf",
    "rust-analyzer.check.allTargets": false
}
```

修改后请在 VSCode 中执行：

1. `Cmd + Shift + P`
2. `Rust Analyzer: Restart Server`

如果 warning 仍未消失，再执行一次：

1. `Cmd + Shift + P`
2. `Developer: Reload Window`

## 串口排查

如果烧录失败：

```sh
ls -l /dev/ttyACM* /dev/ttyUSB*
groups
```

常见处理：

- 开发板未出现为串口设备：重新插拔 USB，或按住 `BOOT` 后复位进入下载模式。
- 权限不足：把当前用户加入系统串口权限组，例如 `dialout`，然后重新登录。
- 多个串口设备：使用 `espflash flash --monitor --port <PORT> ...` 指定端口。
