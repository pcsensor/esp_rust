# Embassy 多任务当前状态

本文记录当前 `src/main.rs` 的 Embassy 多任务架构。原来的“单任务巨型事件循环 + 20 ms UART 轮询”模型已经被替换。

## 一、运行模型

ESP32-C3 是单核芯片，`#[esp_rtos::main]` 提供 Embassy executor。当前固件使用的是协作式 async 多任务：任务之间不会抢占 CPU，但会在 `.await` 点让出执行权。

`run(spawner)` 完成硬件初始化、DX-LR32 AT 配置、UART 异步化和任务派生后返回；`main()` 进入长周期 `Timer::after` idle loop，让 executor 继续调度已派生任务。

## 二、任务划分

| 任务 | 角色 | 拥有的资源 | 职责 |
|------|------|-----------|------|
| `lora_rx_task` | 全部 | `UartRx<'static, Async>` + `FrameStreamDecoder` | `read_async` 等待 UART 数据，流式解码完整 `Frame`，投递到 `RX_FRAMES`。 |
| `core_task` | 全部 | `UartTx<'static, Async>`、`DemoNode`、pending ACK、relay buffer、gateway stats | 独占协议状态；用 `select`/`select3` 等待 RX frame、传感告警跃迁或下一个 TX/HELLO 定时器；在 TDMA active window 内发送。 |
| `sensor_task` | `sensor-node` | `Sht40` + `AlarmLatch` | 每个 TDMA 超帧采样一次，发布最新样本，发送告警 raised/cleared 跃迁。 |
| `buzzer_task` | `gateway-node` | GPIO10 `Output` | 等待 `BUZZER_SIG` 并驱动低有效蜂鸣器。 |

实际任务数：

- gateway：`lora_rx_task` + `core_task` + `buzzer_task`
- relay：`lora_rx_task` + `core_task`
- sensor：`lora_rx_task` + `core_task` + `sensor_task`

## 三、任务通信

- `RX_FRAMES: Channel<CriticalSectionRawMutex, Frame, 8>`：RX 任务到 core。
- `SENSOR_LATEST: Signal<CriticalSectionRawMutex, EnvironmentSample>`：sensor 任务到 core，保存最新样本。
- `ALARM_EVENTS: Channel<CriticalSectionRawMutex, AlarmTransition, 4>`：sensor 任务到 core，保证 raised/cleared 跃迁不被覆盖。
- `BUZZER_SIG: Signal<CriticalSectionRawMutex, BuzzerAction>`：core 到 gateway buzzer 任务。

`DemoNode`、`PendingAck`、`RelayForwardBuffer`、`GatewayStats`、`pending_sync_forward` 和 `pending_alarm_forward` 都只归 `core_task` 所有，不跨任务共享，因此协议状态不需要锁。

## 四、UART 与 DX-LR32 初始化

启动顺序是：

1. 创建 Blocking UART1。
2. 调用 `configure_dx_lr32_module(&mut uart)`，通过 AT 命令尝试固定透明传输、信道、速率、功率、分包、密钥、RSSI 和 LBT 等参数。
3. 调用 `drain_uart(&mut uart)` 清空启动/配置残留字节。
4. 调用 `uart.into_async().split()` 得到异步 RX/TX 两半。
5. `LoraRx` 交给 `lora_rx_task`，`LoraTx` 交给 `core_task`。

AT 配置必须发生在 `into_async().split()` 之前，因为 AT 响应读取仍使用 Blocking UART 路径。

## 五、TDMA 与 core 调度

默认 schedule：

| Slot | 时间 | 用途 |
|---:|---:|---|
| 0 | 0-1 s | 网关广播 `SYNC/SCHEDULE` |
| 1 | 1-2 s | 中继控制 slot，用于转发已排队的 `SYNC` |
| 2 | 2-3 s | 传感节点 `HELLO` 重试、`DATA/ALARM` 首发 |
| 3 | 3-4 s | 中继向网关转发缓存的普通 `DATA` 和已排队的 `ALARM` |
| 4 | 4-5 s | `ALARM` ACK 超时后的重传 |
| 5 | 5-6 s | 中继 `HEARTBEAT` |
| 6 | 6-7 s | 传感节点 `HEARTBEAT` |
| 7 | 7-8 s | 静默/观察/后续配置预留 |

每个 1 s slot 拆成 100 ms 前置 Guard、700 ms Active、200 ms 后置 Guard。节点只在 Active 窗口内主动发送。

`core_task` 每轮重算当前 gateway time、slot 和下一次定时唤醒点：

- 未同步的 relay/sensor 不跟随 TDMA，只按本地 `HELLO_RETRY_INTERVAL_MS` 重试入网，同时继续接收帧。
- 已同步并 joined 后，core 等待 RX frame 或下一个 TX active window。
- sensor 构建额外用 `select3` 监听 `ALARM_EVENTS`。
- `last_tx_slot` 防止同一 slot 内因多次唤醒重复发送。

## 六、可靠性与告警

- 普通 `DATA` 和 `HEARTBEAT` 是 best-effort，不 ACK。
- best-effort 发送默认重复 2 次，接收端按 `(src_id, seq)` 去重。
- `ALARM` 使用 hop-by-hop ACK：sensor 等 relay ACK，relay 等 gateway ACK。
- pending ACK 当前是 demo 级单帧窗口，最多重传 3 次。
- sensor 告警 raised/cleared 由 `sensor_task` 产生，core 负责发送 `ALARM` 或取消本地 pending retry。
- gateway 收到 `ALARM` 后通过 `BUZZER_SIG` 打开蜂鸣器；后续收到普通 `DATA` 且本地处于告警状态时关闭蜂鸣器。

## 七、构建与回归

常规检查：

```sh
./scripts/check-demo.sh
```

该脚本执行：

- `cargo fmt --check`
- 默认 gateway build check
- gateway / relay / sensor 三角色 release check
- `scripts/run-release.sh` 与 `scripts/check-demo.sh` 的 shell 语法检查

纯逻辑单元测试使用：

```sh
cargo test-host
```

现场三板联调按 `docs/demo-runbook.md` 执行。

## 八、仍需人工实测的边界

- 三块板实际空口延迟、串口缓存和 1 s slot 长度是否满足现场环境。
- `RX_DEPTH`、best-effort 重发次数、slot active window 是否需要按现场干扰调整。
- `CriticalSectionRawMutex` 当前保守保留；确认运行上下文后可评估是否降级为 `NoopRawMutex`。
