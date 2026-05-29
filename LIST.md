# Embassy 多任务重构方案

将当前 `src/main.rs` 中的「单任务巨型事件循环」重构为真正的 Embassy 多任务架构。

目标（硬约束）：

1. 性能与任务配合只能更好：用中断驱动的异步 UART 取代 20ms 轮询，RX 不再受 TX/TDMA 时序阻塞，告警/控制帧延迟更低、UART FIFO 不再有老化/溢出风险。
2. 现有功能全部保留，不阉割：JOIN/SYNC/SCHEDULE/DATA/ALARM/ACK/HEARTBEAT、TDMA 严格时隙、FTSP 平滑同步、slot-strict 接收、hop-by-hop ALARM ACK+重传、relay 缓冲转发、best-effort 重发去重、传感器告警闩锁+迟滞、网关蜂鸣器、网关统计日志 —— 全部不变。
3. 分阶段重构，每阶段独立可测、可回滚；每阶段结束都跑回归。

---

## 一、现状分析

`src/main.rs::run()` 是单个 async 函数里的一个大 `loop`，串行做完所有事：

- 计算当前 slot → `service_rx`（`drain_to_decoder` + 最多 4 帧 `handle_received_frame`）
- 网关周期统计上报
- 若已同步且已 join：`enter_tx_window` 等到时隙活动窗口，再做该 slot 的角色 TX
- 内层 `loop` 每 `RX_POLL_INTERVAL_MS=20ms` 轮询一次 RX，直到下一个 slot 边界

关键痛点：

- RX 靠 `read_ready()` 轮询，且在 `enter_tx_window`/`send_best_effort` 的 `await` 期间完全停摆 → 帧只能积压在硬件 FIFO，存在老化/溢出风险，这也是现在要 20ms 高频轮询的根因。
- TX 时序与 RX 处理耦合在同一循环，互相挤占。
- 所有状态（`DemoNode`、`PendingAck`、`RelayForwardBuffer`、`pending_sync_forward`、`pending_alarm_forward`、`GatewayStats`、`gateway_alarm_active`）都在一个栈帧里用 `&mut` 共享。

`LoraUartTransport`（`src/transport.rs`）持有一个 `Uart<'_, Blocking>`，RX/TX 共用同一外设句柄。

---

## 二、目标架构

ESP32-C3 单核，`#[esp_rtos::main]` 提供单个 thread-mode executor；所有任务用 `#[embassy_executor::task]` 创建，由传入 `main` 的 `Spawner` 派生（协作式调度，无抢占）。

### 任务划分（按角色编译裁剪）

| 任务 | 角色 | 拥有的资源 | 职责 |
|------|------|-----------|------|
| `lora_rx_task` | 全部 | `UartRx<'static, Async>` + `FrameStreamDecoder` | `read_async` → 喂解码器 → 把完整 `Frame` 投递到 `RX_FRAMES` 通道。纯 RX，中断驱动，无轮询。 |
| `core_task`（大脑） | 全部 | `UartTx<'static, Async>`(经 `LoraTx`)、`DemoNode` 及全部协议状态 | `select(RX_FRAMES.receive(), 时隙定时器)`：收到帧立刻处理（含回 ACK/JOIN_ACK），到 TX 窗口做该 slot 的角色发送。 |
| `sensor_task` | sensor | `I2c`/`Sht40` + `AlarmLatch` | 周期采样、更新闩锁，向 core 发布最新样本与告警跃迁。 |
| `buzzer_task` | gateway | `Output`(GPIO10) | 等 `BUZZER_SIG`，驱动蜂鸣器引脚。 |

### 通信原语（`static`，const 构造）

- `RX_FRAMES: Channel<CriticalSectionRawMutex, Frame, RX_DEPTH>` —— RX 任务 → core。
- `BUZZER_SIG: Signal<CriticalSectionRawMutex, BuzzerAction>` —— core → buzzer（gateway）。
- `SENSOR_LATEST: Signal<.., EnvironmentSample>` + `ALARM_EVENTS: Channel<.., AlarmTransition, 4>` —— sensor → core。

> **RawMutex 选择**：所有任务跑在同一 thread-mode executor、协作式调度，理论上 `NoopRawMutex` 即可且更省。但 esp-rtos 同时管理线程，为稳妥起见统一用 `CriticalSectionRawMutex`；确认无抢占/中断访问后可在收尾阶段降级为 `NoopRawMutex` 做优化。

### 核心设计要点（为什么这样最优）

1. **状态不共享、用消息传递**：`DemoNode` 及所有协议状态只归 `core_task` 独占 → **完全不需要给节点状态加锁**，没有锁竞争、没有「跨 await 持锁」隐患。RX 任务只产出 `Frame`，core 单点消费。
2. **TX 单一所有者**：只有 `core_task` 持有 `UartTx`（回 ACK 也在 core 内做）→ 无 TX 争用。
3. **RX 与 TX 时序解耦**：core 在时隙窗口 `await` 发送期间，`lora_rx_task` 独立 `read_async` 继续收帧入通道，**FIFO 不再老化**，可彻底删掉 20ms 轮询。
4. **`select` 同时兼顾低延迟与 TDMA**：core 用 `embassy_futures::select` 在「通道有帧」与「到达本 slot 的 TX 窗口」之间择一唤醒——帧一到立即处理，时隙一到精确发送。
5. **异步 UART API 已确认**（esp-hal 1.1.1 `src/uart/mod.rs`）：`Uart::into_async()`(1397)、`split()`(2012)、`UartRx::read_async`(1146)、`UartTx::write_async`(711)、`flush_async`(745)，均标注 cancellation-safe，可安全用于 `select`。
6. **初始化顺序**：AT 配置 `configure_dx_lr32_module` 必须在 **Blocking** 模式、`split`/`into_async` **之前**完成（它依赖 `read_until` 的轮询读）；配置完再 `into_async().split()` 分发给两个任务。
7. **`'static` 要求**：spawned task 参数需 `'static`。`esp_hal::init` 返回的外设单例是 `'static`，故 `Uart<'static, Async>` 可达；如遇生命周期问题，用 `static_cell::StaticCell` 固化句柄/缓冲。

---

## 三、分阶段实施

每个阶段的回归门禁（统一记为 **GATE**）：

```
cargo fmt --check
cargo test-host
cargo check --release --no-default-features --features gateway-node
cargo check --release --no-default-features --features relay-node
cargo check --release --no-default-features --features sensor-node
./scripts/check-demo.sh
```

涉及运行时行为的阶段，额外做 **三板联调**（参照 `docs/demo-runbook.md`），比对 Phase 0 基线日志。

---

### Phase 0 —— 基线与回归护栏

- 跑一遍 **GATE**，记录全绿。
- 按 `docs/demo-runbook.md` 跑一次完整三板 demo，**抓取基线日志**：join、SYNC/offset/drift、DATA(origin_seq 连续性)、ALARM→ACK→蜂鸣器 On、normal DATA→蜂鸣器 Off、HEARTBEAT、slot 违例丢弃。作为后续行为对照基准。
- `Cargo.toml` 增加依赖 `embassy-futures`（提供 `select`），确认构建仍全绿。
- **退出标准**：GATE 全绿；基线日志归档。无功能改动。

---

### Phase 1 —— Transport 拆分为 TX/RX 两半（Blocking，单任务，行为不变）

纯结构重构，先不引入异步与多任务，降低风险。

- `src/transport.rs`：把 `LoraUartTransport` 拆为
  - `LoraTx<'d>{ uart: UartTx<'d, Blocking> }`：`send_frame`（沿用现在的 encode + write_all + flush）。
  - `LoraRx<'d>{ uart: UartRx<'d, Blocking>, decoder: FrameStreamDecoder }`：`drain_to_decoder` / `next_decoded_frame`（沿用 `read_ready`+`read` 非阻塞抽取）。
- `main.rs::run()`：AT 配置后 `uart.split()` 得到两半；循环逻辑、slot 行为、日志**完全不变**，只是把对单一 transport 的调用换成对两半的调用。`service_rx`/`handle_received_frame` 改为接收 `&mut LoraTx`（发送）与 `&mut LoraRx`（接收）。
- **退出标准**：行为/日志逐条一致；GATE 全绿；三板冒烟。可独立合入。

---

### Phase 2 —— UART 异步化 + 独立 `lora_rx_task`（核心阶段）

本阶段拿下最大收益。建议拆两小步合一次测：

**2a：异步化 + RX 任务产帧**
- AT 配置后 `let (rx, tx) = uart.into_async().split();`（→ `UartRx/UartTx<'static, Async>`）。
- 新增 `static RX_FRAMES: Channel<CriticalSectionRawMutex, Frame, RX_DEPTH>`（`RX_DEPTH` 初定 8）。
- `spawner.spawn(lora_rx_task(rx))`：循环 `read_async(&mut buf)` → `decoder.push_bytes` → 循环 `next_frame` → `RX_FRAMES.sender().send(frame).await`。中断驱动，**删除 `RX_POLL_INTERVAL_MS` 轮询模型**。
- `LoraTx` 的 `send_frame` 改为 async（`write_async`+`flush_async`）。

**2b：core 改为 select 调度**
- `core_task` 独占 `LoraTx` + `DemoNode` + 全部协议状态。主循环：
  ```text
  loop {
    重算 当前 slot / 是否 follow_schedule / 下一个 TX 窗口时刻
    select {
      frame = RX_FRAMES.receive()        => handle_received_frame(frame).await  // 含回 ACK/JOIN_ACK
      _     = Timer 到 TX 窗口/HELLO 重试 => 执行该 slot 的角色 TX
      _     = 网关统计 fallback ticker    => 周期上报（无流量时也按时触发）
    }
  }
  ```
- 用 `last_serviced_slot` 去重，保证每个 slot 的 TX 只触发一次。
- 未同步的 relay/sensor 仍走「HELLO 本地定时重试」分支（`HELLO_RETRY_INTERVAL_MS` 保留），只是改由 select 的定时器臂驱动。
- 删除 `service_rx`、内层 20ms 轮询 loop、`MAX_RX_FRAMES_PER_LOOP`、`RX_POLL_INTERVAL_MS`。
- **节点状态无需加锁**（core 独占）。
- **风险点校验**：`enter_tx_window` 的 guard 等待期间 RX 由独立任务继续收帧；slot-strict 接收（`accepts_frame_slot`）、`PendingAck` 重传（slot 4）、relay 延迟转发（slot 3）、SYNC 转发（slot 1）逻辑保持不动，只是搬进 core_task。
- **退出标准**：功能逐条对齐基线（join/sync/data/alarm/heartbeat/slot 违例丢弃、ALARM 重传与 ACK 清除、蜂鸣器开关）；RX 延迟更低、无 FIFO 溢出告警；GATE 全绿；三板联调比对基线日志。

---

### Phase 3 —— 传感器独立任务（sensor-node）

- `spawner.spawn(sensor_task(i2c))`：任务独占 `Sht40` + `AlarmLatch`。
- 按 **每超帧（~8s）** 节拍采样（与现采样频率一致），失败回退 `EnvironmentSample::normal()`；用 `Sht40Config::DEFAULT` 阈值更新闩锁。
- 向 core 发布：最新样本 `SENSOR_LATEST: Signal<EnvironmentSample>`；告警跃迁 `ALARM_EVENTS: Channel<AlarmTransition, 4>`（用 Channel 保证 Raised/Cleared 不丢）。
- `core_task` 在 sensor_slot：取 `SENSOR_LATEST` 最新样本构造 DATA/ALARM（保持 ALARM 单发+`PendingAck`、DATA best-effort 重发）；消费 `ALARM_EVENTS`：打印 raised/cleared 日志、Cleared 时 `pending_ack.cancel_if_matches(Alarm)`。
- **保真要点**：闩锁迟滞（≥30.00℃/≥80.00% 触发，<29.00℃ 且 <75.00% 清除）、raised/cleared 日志文案、清除即取消 ALARM 重传 —— 全部逐条对齐。
- **退出标准**：硬件验证 升温触发 ALARM→网关 ACK→蜂鸣器 On→降温清除→取消重传→下一条 DATA 蜂鸣器 Off；GATE 全绿。

---

### Phase 4 —— 蜂鸣器与网关统计任务（gateway-node，解耦润色）

- `spawner.spawn(buzzer_task(buzzer))`：等 `BUZZER_SIG: Signal<BuzzerAction>` 驱动 GPIO10（低有效）。core 在「收到 ALARM」「收到 normal DATA 且当前告警」「SYNC slot 置高」处改为 `BUZZER_SIG.signal(..)`，把 GPIO 从 core 逻辑剥离。
- 网关统计：可保留在 core（成本低），或将周期上报移入 `Ticker` 任务读 `Mutex<GatewayStats>`。优先保留在 core 以避免引入共享锁；仅当确有收益再拆。
- **退出标准**：蜂鸣器开关时机与基线一致；GATE 全绿；冒烟。

---

### Phase 5 —— 调优、清理、文档

- 调参：`RX_DEPTH`、`read_async` 缓冲大小、各通道深度、必要时任务 arena/栈；确认无通道满/丢帧/溢出日志。
- 评估把 `CriticalSectionRawMutex` 降级为 `NoopRawMutex`（确认全在单 executor 内访问后）。
- 删除遗留常量与死代码路径；`cargo fmt`。
- 更新 `CLAUDE.md` 的「Runtime behavior in src/main.rs」一节（不再是单循环，改述多任务模型）；如有日志文案变化同步 `docs/demo-runbook.md`。
- **最终回归**：GATE 全绿 + 完整三板 demo 与 Phase 0 基线日志逐条比对。

---

## 四、风险登记

| 风险 | 缓解 |
|------|------|
| 异步 UART 与 AT 配置冲突 | AT 配置在 Blocking、`into_async` 之前完成（见设计要点 6）。 |
| spawned task 的 `'static` 生命周期 | 用 `esp_hal::init` 的 `'static` 外设单例；必要时 `StaticCell`。 |
| `select` 下 slot TX 重复触发 | `last_serviced_slot` 去重。 |
| 通道积压/满 | `RX_DEPTH` 留余量；core 单点快速消费；监控满载日志。 |
| 告警跃迁丢失（Phase 3） | 跃迁走 `Channel` 而非 `Signal`，保证不覆盖丢事件。 |
| 采样从「时隙内即时」变为「周期发布」的新鲜度 | 采样节拍对齐超帧；温湿度 demo 对 ~8s 新鲜度无感；硬件验证告警时延。 |
| 行为回归 | 每阶段 GATE + 三板联调比对 Phase 0 基线日志。 |

## 五、依赖与文件改动概览

- `Cargo.toml`：新增 `embassy-futures`（`select`）；如需 `static-cell`。
- `src/transport.rs`：拆 `LoraTx` / `LoraRx`，TX 改 async（Phase 1–2）。
- `src/main.rs`：`lora_rx_task` / `core_task` / `sensor_task` / `buzzer_task` 与 `static` 通道、select 调度（Phase 2–4）。
- `src/sensors.rs`：基本不变，仅由 `sensor_task` 持有（Phase 3）。
- `src/demo.rs` / `protocol.rs` / `tdma.rs` / `relay.rs`：纯逻辑保持不动（核心收益正源于此前的「纯逻辑/HAL 分离」已经做好）。
- 文档：`CLAUDE.md`、`docs/demo-runbook.md`（Phase 5）。
