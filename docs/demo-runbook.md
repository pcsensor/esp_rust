# 管廊传感网 Demo 联调步骤

## 目标

用三块 ESP32-C3 Pro Mini 和三个 DX-LR32-433T22D 模块演示固定链路：

```text
sensor-node -> relay-node -> gateway-node
```

演示内容覆盖自组织入网、TDMA、网关逐级时间同步、SHT40 温湿度上报、ACK/重传和网关蜂鸣器告警。

## 预检查

1. 确认三块 LoRa 模块参数一致（固件启动时会尝试通过 AT 命令关闭密钥；若模块没有响应 AT，会沿用已有透明传输配置继续运行）：
   - 模块：DX-LR32-433T22D
   - 频段：433.15 MHz（信道 CHANNEL 00）
   - UART：9600 baud / 8N1
   - 空中速率：2148 bps（LEVEL 2）
   - 发射功率：22 dBm
   - 透明传输模式（MODE 0）
   - 密钥关闭（OPENKEY 0）
2. 确认 ESP32-C3 接线：
   - LoRa RX 接 ESP32-C3 GPIO21
   - LoRa TX 接 ESP32-C3 GPIO20
   - SHT40 SDA 接传感节点 GPIO5
   - SHT40 SCL 接传感节点 GPIO4
   - 有源蜂鸣器接网关 GPIO10，低电平响
3. 运行工程检查：

```sh
./scripts/check-demo.sh
```

## 烧录

分别连接三块板，烧录对应角色：

```sh
./scripts/run-release.sh gateway
./scripts/run-release.sh relay
./scripts/run-release.sh sensor
```

## 期望日志

### 网关

启动后应看到：

```text
DX-LR32: entered AT mode
DX-LR32: AT+OPENKEY0 -> OK
DX-LR32: exited AT mode, module rebooting
role=gateway node_id=1 default_slot=0 hop=0
gateway online: broadcasting SYNC/SCHEDULE every TDMA superframe
tx SYNC frame_seq=... sync_seq=1 gateway_time=... slot=0 bytes=...
```

如果模块未进入 AT 模式，也可以接受以下日志，表示固件继续使用模块现有配置：

```text
DX-LR32: AT mode unavailable, keep existing transparent-mode config
```

中继入网时应看到：

```text
rx HELLO from=2 role=relay -> tx JOIN_ACK seq=... bytes=...
```

传感节点入网时（中继转发 HELLO 通知网关）应看到：

```text
topology: sensor(3) -> relay(2) -> gateway(1)  hop=1
```

正常数据上报时应看到：

```text
rx DATA origin=3 origin_seq=... via=2 relay_seq=... temp=26.61C humidity=51.45% alarm=false gateway_time=...
```

告警时应看到：

```text
rx ALARM origin=3 origin_seq=... via=2 relay_seq=... temp=31.20C humidity=85.00% alarm=true gateway_time=...
alarm active: buzzer on
```

恢复后应看到：

```text
rx DATA origin=3 origin_seq=... via=2 relay_seq=... alarm=false ...
alarm cleared: buzzer off
```

### 中继

启动后应看到：

```text
DX-LR32: entered AT mode
DX-LR32: AT+OPENKEY0 -> OK
DX-LR32: exited AT mode, module rebooting
role=relay node_id=2 default_slot=3 hop=1
relay searching: send HELLO seq=... bytes=...
rx JOIN_ACK: parent=1 hop=1 slot=3
rx SYNC sync_seq=... offset_ms=... -> forward bytes=...
```

传感节点入网时：

```text
rx sensor HELLO from=3 -> tx JOIN_ACK seq=... bytes=...
forward sensor HELLO to gateway: src=3 hop=1 bytes=...
```

正常数据中继：

```text
rx DATA origin=3 origin_seq=... from=3 temp=26.61C humidity=51.45% -> buffered_for_relay_slot
relay slot active: buffered_data=... pending_ack=... parent=... hop=1 sync_seq=... offset_ms=...
relay slot tx buffered DATA sensor_seq=... relay_seq=... ack_required=false bytes=...
```

告警中继：

```text
rx ALARM origin=3 origin_seq=... from=3 temp=31.20C humidity=85.00% alarm=true -> ack_bytes=... relay_seq=... immediate_forward_bytes=...
rx ACK from=1 acked_seq=... acked_type=ALARM cleared_pending=true
```

### 传感节点

启动后应看到：

```text
DX-LR32: entered AT mode
DX-LR32: AT+OPENKEY0 -> OK
DX-LR32: exited AT mode, module rebooting
role=sensor node_id=3 default_slot=2 hop=2
sensor searching: send HELLO seq=... bytes=... parent=None
rx JOIN_ACK: parent=2 hop=2 slot=2
rx SYNC: sync_seq=... offset_ms=...
tx DATA seq=... parent=Some(2) temp=26.61C humidity=51.45% gateway_time=... bytes=...
```

告警时传感节点应收到中继的跳到跳 ACK；如果没收到，会在 Slot 4 重传最多 3 次：

```text
sensor alarm raised: temp=31.20C humidity=85.00% thresholds=3000cC/8000c%
tx ALARM seq=... parent=Some(2) temp=31.20C humidity=85.00% gateway_time=... bytes=...
rx ACK from=2 acked_seq=... acked_type=ALARM cleared_pending=true
```

告警解除使用回差，温度低于 29.00 C 且湿度低于 75.00% 后才恢复普通 `DATA`：

```text
sensor alarm cleared: temp=28.90C humidity=70.00% clear_thresholds=2900cC/7500c%
```

如果 SHT40 未接好，传感节点会打印读取失败并使用演示样本，网络协议仍可演示。

## 演示顺序

1. 启动网关，确认 AT 配置成功或明确降级为现有透明模式配置，然后周期广播 `SYNC`。
2. 启动中继，确认网关接纳 relay 入网（`rx HELLO from=2 role=relay -> tx JOIN_ACK`）。
3. 启动传感节点，确认中继返回 `JOIN_ACK`，同时网关日志出现 `topology: sensor(3) -> relay(2) -> gateway(1)`。
4. 观察传感节点 `DATA`、中继 Slot 3 转发、网关接收；普通 `DATA` 不 ACK。
5. 观察 Slot 5/6 的中继和传感节点 `HEARTBEAT`，确认节点在线状态。
6. 捂住 SHT40 或靠近热源触发 `ALARM`，确认网关蜂鸣器响。
7. 恢复普通 `DATA`，确认蜂鸣器关闭。

## 已知边界

- 固件启动时尝试通过 AT 命令关闭模块密钥（`AT+OPENKEY0`）；如果没有 `Entry AT` 响应，会保留现有透明模式配置继续演示。其余参数（信道、速率等级等）沿用模块出厂默认值或人工配置值。
- 当前可靠传输是单帧 `ALARM` pending-ACK 窗口（最多重传 3 次），普通 `DATA/HEARTBEAT` 是周期性最佳努力发送。
- 超帧周期 8 s，每个 slot 1 s，共 8 个 slot。现场可按 LoRa 空口延迟调整。
- `sync_seq` 是独立同步序号，不等于普通 frame `seq`；ACK、DATA、HEARTBEAT 不会造成 `sync_seq` 跳号。
- 告警使用回差：触发阈值为 30.00 C / 80.00%，解除阈值为 29.00 C / 75.00%。

## TDMA Slot 表

| Slot | 用途 |
|---:|---|
| 0 | 网关广播 `SYNC/SCHEDULE` |
| 1 | 中继控制预留 |
| 2 | 传感节点 `HELLO` 重试、采样、`DATA/ALARM` 首发 |
| 3 | 中继转发缓存的普通 `DATA` |
| 4 | `ALARM` ACK 超时重传 |
| 5 | 中继 `HEARTBEAT` |
| 6 | 传感节点 `HEARTBEAT` |
| 7 | 静默/观察/后续配置预留 |

## 丢包定位

网关 `DATA/ALARM` 日志同时包含：

- `origin`：原始传感节点 ID，三节点 demo 中应为 `3`。
- `origin_seq`：传感节点生成数据时的原始序号。
- `via`：网关实际收到的上一跳，三节点 demo 中应为中继 `2`。
- `relay_seq`：中继转发时生成的新序号。

判断方法：

- 中继日志出现某个 `origin_seq`，网关没有对应 `origin_seq`：大概率是 `relay -> gateway` 丢包。
- 传感节点日志出现某个 `tx DATA/ALARM seq`，中继没有对应 `origin_seq`：大概率是 `sensor -> relay` 丢包。
- 网关 `origin_seq` 连续但 `relay_seq` 跳号：说明中继还发送了其他帧，例如 `SYNC/HEARTBEAT/ACK`，不是 DATA 丢包。
