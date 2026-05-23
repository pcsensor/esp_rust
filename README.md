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

当前项目需要 ESP Rust Xtensa 工具链。若本机还没有安装：

```sh
cargo install espup --locked
espup install
. "$HOME/export-esp.sh"
cargo install espflash --locked
```

安装后确认：

```sh
. "$HOME/export-esp.sh"
rustup toolchain list
cargo check
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

`.cargo/config.toml` 已设置默认 target 和 runner，`cargo run --release` 会调用：

```sh
espflash flash --monitor
```

如果有多个串口设备，直接指定端口：

```sh
espflash flash --monitor --port /dev/ttyACM0 target/xtensa-esp32s3-none-elf/release/esp32s3-n16r8-rust
```

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
