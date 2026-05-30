# 城市地下综合管廊传感网答辩 Demo 目标

## 背景

本仓库当前已从 ESP32-C3 Rust/Embassy 基础工程演进为一个可构建的三节点 LoRa 传感网答辩 demo。Demo 基于 `docs/城市地下综合管廊环境与结构安全监测传感网设计_修订版_排版.docx` 的设计思想，用 ESP32-C3、DX-LR32-433T22D 和 SHT40 展示小型管廊环境监测网络。

Demo 不追求工程级完整管廊系统，而是用 3 个 ESP32-C3 节点把设计文档中的关键点可视化展示出来：

- 线性分簇/星型混合拓扑的简化形态：传感节点 -> 中继节点 -> 网关节点。
- LoRa 自组织入网和父节点选择。
- 简单 TDMA 时隙调度，避免所有节点同时发送。
- 网关逐级下发时间同步，参考 FTSP 思想。
- 环境数据上报、告警快速上报、网关本地联动。

## 物料

### 传感节点

- ESP32-C3 Pro Mini
- SHT40 温湿度传感器
- DX-LR32-433T22D LoRa UART 模块

### 中继节点

- ESP32-C3 Pro Mini
- DX-LR32-433T22D LoRa UART 模块

### 网关节点

- ESP32-C3 Pro Mini
- DX-LR32-433T22D LoRa UART 模块
- 有源蜂鸣器，低电平响

## 总体实现决策

1. 一个仓库维护三种固件角色，通过 Cargo feature 或构建配置选择：
   - `sensor-node`
   - `relay-node`
   - `gateway-node`
2. 不依赖 OLED。当前 demo 以串口日志、LoRa 通信和网关蜂鸣器作为主要演示输出。
3. DX-LR32-433T22D 按透明 UART LoRa 模块使用。固件启动时会先用 Blocking UART 尝试 AT 配置，配置完成后切换为 Embassy async UART RX/TX 任务。
4. Demo 中的“自组网”“TDMA”“FTSP-like 时间同步”采用教学演示级实现，重点是协议流程清晰、日志可解释、现象可演示，不追求工程级时钟精度、低功耗和抗干扰。

## 当前已实现的功能

### 1. 统一数据帧

当前实现了一个小型二进制协议，包含：

- `net_id`
- `src_id`
- `dst_id` 或广播地址
- `node_role`
- `zone_id`
- `frame_type`
- `seq`
- `hop`
- `gateway_time_ms` 或 `node_time_ms`
- `payload`
- `crc`

`DATA/ALARM` payload 携带 `origin_id` 与 `origin_seq`，使网关在经过中继后仍能打印原始传感节点 ID、原始数据序号、中继转发序号、温湿度、告警状态、ACK/蜂鸣器动作，用于现场判断丢包发生在 `sensor -> relay` 还是 `relay -> gateway`。

支持以下帧类型：

- `HELLO`：节点发现/入网请求。
- `JOIN_ACK`：网关或中继回应入网，携带层级、父节点、TDMA 时隙。
- `SYNC`：网关校时报文，由中继逐级转发。
- `SCHEDULE`：TDMA 超帧和时隙配置，可与 `SYNC` 合并。
- `DATA`：传感节点周期上报温湿度。
- `ALARM`：传感节点异常快速上报。
- `ACK`：关键帧确认。
- `HEARTBEAT`：中继/节点在线状态。

### 2. 自组织入网

启动后：

1. 网关进入监听状态，周期广播 `SYNC/SCHEDULE`。
2. 中继节点广播 `HELLO`，收到网关 `JOIN_ACK` 后加入网络，记录父节点为网关。
3. 传感节点广播 `HELLO`，可收到中继或网关回应时，优先选择跳数更合理且近期 ACK 成功的父节点；在三节点 demo 中默认选择中继。
4. 节点周期输出入网状态日志，展示 `node_id`、`parent_id`、`hop`、`slot_id`。

DX-LR32 透明串口模式下当前不依赖 RSSI/LQI；三节点 demo 使用固定角色优先级和跳数关系保证 `sensor -> relay -> gateway` 拓扑稳定。

### 3. 简单 TDMA

实现网关控制的固定超帧，当前演示参数：

- 超帧周期：8 s。
- Slot 0：网关广播 `SYNC/SCHEDULE`。
- Slot 1：中继控制预留，用于同步转发、入网控制或后续配置扩展。
- Slot 2：传感节点重试 `HELLO`、使用最新 SHT40 样本向父节点发送 `DATA/ALARM` 首发。
- Slot 3：中继节点向网关转发上一 slot 收到的普通 `DATA`。
- Slot 4：`ALARM` ACK 超时后的重传 slot。
- Slot 5：中继节点发送 `HEARTBEAT`。
- Slot 6：传感节点发送 `HEARTBEAT`。
- Slot 7：静默/现场观察/后续配置预留。

每个 1 s slot 拆为 100 ms 前置 Guard、700 ms Active 发送窗口、200 ms 后置 Guard。节点只能在自己的常规 slot 的 Active 窗口内发送周期数据或重传，Guard 时间只接收不主动发送。`DATA` 与 `HEARTBEAT` 为周期性最佳努力发送，不要求 ACK；`ALARM` 使用跳到跳 ACK，传感节点等待中继 ACK，中继等待网关 ACK，未确认时在 Slot 4 最多重传 3 次。

### 4. 网关逐级时间同步

实现 FTSP-like 的简化时间同步：

1. 网关维护单调递增的 `gateway_time_ms`。
2. 网关在 Slot 0 广播 `SYNC`，携带独立递增的 `sync_seq`、`gateway_time_ms`、`schedule_version` 和 TDMA 超帧/Active/Guard 参数；`sync_seq` 不复用普通 frame `seq`。
3. 中继收到后记录本地接收时间，估算 `offset_ms = gateway_time_ms - local_time_ms`，记录相邻 SYNC 的 `offset_delta_ms` 作为轻量漂移观察值，再以 `hop + 1` 逐级转发。
4. 传感节点收到中继转发的 `SYNC` 后估算自身偏移，跟随网关下发的调度参数，并用平滑方式更新本地“网关时间”。
5. `DATA` 和 `ALARM` 同时携带节点本地计数器与校准后的网关时间，答辩时能说明“多节点数据可比较”的设计意图。

说明：LoRa UART 模块无法像裸 SX126x 驱动那样精确控制 PHY 层发送时间戳，因此这里展示的是 FTSP 的逐级同步思想，不承诺毫秒级工程精度。

### 5. 温湿度采集与告警联动

传感节点：

- 驱动 SHT40，周期读取温度和湿度。
- 正常状态按 TDMA 周期发送 `DATA`。
- 当温度或湿度超过配置阈值时发送 `ALARM`。
- 阈值建议先使用易演示参数，例如温度 >= 30 C 或湿度 >= 80%，后续按现场环境调整；解除告警使用回差，例如温度 < 29 C 且湿度 < 75% 才恢复正常。

中继节点：

- 接收传感节点 `DATA/ALARM`。
- 在自己的转发 slot 转发普通数据。
- 对 `ALARM` 返回跳到跳 ACK，并把告警帧排入中继转发 slot；如果中继到网关这一跳未确认，则在告警重传 slot 重试。普通 `DATA/HEARTBEAT` 不 ACK，体现“告警优先、周期数据可丢一帧”的 demo 策略。

网关节点：

- 接收经中继转发的数据。
- 串口打印拓扑状态、同步状态、TDMA slot、温湿度、告警等级；网关关键 `DATA/ALARM` 使用结构化 ASCII 演示日志，避免依赖 emoji、ANSI 颜色或真实日期时间。
- 每 30 s 打印一次网络统计，包含 `rx_data`、`rx_alarm`、`tx_ack`、`slot_violations`、`origin_seq_gap_total`、`last_sync`、`offset` 和 `drift`。
- 收到 `ALARM` 后拉低蜂鸣器 GPIO，使有源蜂鸣器响。
- 告警解除后的普通 `DATA` 到达网关后关闭蜂鸣器；重启也会清除本地告警状态。

## 演示脚本

答辩现场建议按以下顺序演示：

1. 只启动网关：串口显示 `gateway online`，开始广播 `SYNC/SCHEDULE`。
2. 启动中继：中继发送 `HELLO`，网关分配 `relay slot`，网关日志显示中继入网。
3. 启动传感节点：传感节点通过中继入网，网关日志显示 `sensor -> relay -> gateway`。
4. 正常上报：传感节点按 Slot 2 发送 SHT40 温湿度，中继 Slot 3 转发，网关显示带时间戳的数据。
5. 时间同步说明：展示各节点日志中的 `sync_seq`、`offset_ms`、`gateway_time_ms`。
6. 告警演示：用手捂住/靠近 SHT40 或调整阈值触发温湿度告警，网关收到 `ALARM` 后蜂鸣器响。
7. 恢复演示：环境恢复或清除告警后，网关关闭蜂鸣器，系统回到周期上报。

## 验收标准

- `cargo check --release` 通过。
- 三种角色都能构建出固件。
- 三个 ESP32-C3 烧录不同角色后，串口日志能清楚显示：
  - 自组织入网过程。
  - 父节点/跳数/时隙分配。
  - 网关逐级时间同步。
  - TDMA 周期发送和中继转发。
  - SHT40 温湿度数据到达网关。
  - 告警帧触发蜂鸣器。
- LoRa 模块配置参数在 README 或单独文档中记录，至少包括 UART 波特率、频点/信道、空中速率、发送功率、地址/网络号。

## 实现状态

### 阶段 1：硬件抽象与角色骨架

- 已建立三角色 Cargo feature：`gateway-node`、`relay-node`、`sensor-node`，且编译期要求只启用一个角色。
- GPIO/UART/I2C、LoRa 模块配置计划、SHT40 阈值和蜂鸣器配置集中在 `src/hardware.rs`。
- 网关蜂鸣器由独立 `buzzer_task` 驱动，GPIO10 低电平响。

### 阶段 2：LoRa UART 驱动与帧协议

- 已初始化 UART1 与 DX-LR32 通信。
- 已实现启动 AT 配置流程；若模块不响应 AT，会保留现有透明模式配置继续运行。
- 已实现帧编码、解码、CRC16、ACK payload、流式帧解码和基础日志。

### 阶段 3：自组织和 TDMA

- 已实现 `HELLO/JOIN_ACK`。
- 已实现固定 TDMA slot 分配和 slot-strict 接收检查。
- 已实现传感节点按 slot 首发 `DATA/ALARM`，中继按 slot 转发普通数据和告警数据。

### 阶段 4：时间同步

- 已实现 `SYNC/SCHEDULE`。
- 中继收到网关同步帧后排入 relay control slot 再转发，避免在网关同步 slot 内立即抢发。
- 已实现节点 offset 估算、平滑更新和 `offset_delta_ms` 漂移观察。
- `DATA/ALARM` 已携带校准后的 gateway time。

### 阶段 5：SHT40 与告警演示

- 已实现 SHT40 I2C 读取和 CRC8 校验；读取失败时使用演示样本。
- 已实现温湿度阈值判断、告警回差、告警跃迁 channel 和本地 pending ALARM 取消。
- 已实现 `ALARM` 跳到跳 ACK、最多 3 次重传和网关蜂鸣器联动。
- README 与 `docs/demo-runbook.md` 记录当前接线、烧录、演示步骤和期望日志。

## 当前默认硬件选择

- LoRa UART：ESP32-C3 UART1，TX GPIO21、RX GPIO20。
- DX-LR32 配置脚、复位脚、AUX、M0/M1 当前不接入 MCU；固件通过 UART AT 命令配置，失败则沿用模块现有透明模式配置。
- SHT40：传感节点 I2C0，SDA GPIO5、SCL GPIO4。
- 蜂鸣器：网关 GPIO10，低电平响。
- 角色选择：通过 Cargo feature 分别构建三种固件，不做运行时角色选择。

## 非目标

- 不实现云平台、MQTT、TLS、Web UI、GIS、工单系统。
- 不实现完整 LoRaWAN。
- 不实现工程级低功耗休眠、电池寿命估算和 OTA。
- 不实现多传感器融合，只用 SHT40 代表环境节点。
- 不实现真实管廊定位，只用 `zone_id` 和 `node_id` 表示分区定位思想。
