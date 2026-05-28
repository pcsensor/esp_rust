# 管廊传感网 Demo 联调步骤

## 目标

用三块 ESP32-C3 Pro Mini 和三个 DX-LR32-433T22D 模块演示固定链路：

```text
sensor-node -> relay-node -> gateway-node
```

演示内容覆盖自组织入网、TDMA、网关逐级时间同步、SHT40 温湿度上报、ACK/重传和网关蜂鸣器告警。

## 预检查

1. 确认三块 LoRa 模块参数一致（固件启动时会通过 AT 命令自动配置密钥，无需手动操作）：
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
DX-LR32: AT+OPENKEY0 → OK
DX-LR32: exited AT mode, module rebooting
role=gateway node_id=1 default_slot=0 hop=0
gateway online: broadcasting SYNC/SCHEDULE every TDMA superframe
tx SYNC seq=1 gateway_time=... slot=0 bytes=...
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
rx DATA via=2 temp=26.61C humidity=51.45% alarm=false gateway_time=...
```

每 5 秒 Slot 4 状态汇总：

```text
gateway status | uptime=...s sync_seq=... offset_ms=... alarm=false buzzer=GPIO10
```

告警时应看到：

```text
rx ALARM via=2 temp=31.20C humidity=85.00% alarm=true gateway_time=...
alarm active: buzzer on
```

恢复后应看到：

```text
rx DATA via=2 ... alarm=false ...
alarm cleared: buzzer off
```

### 中继

启动后应看到：

```text
DX-LR32: entered AT mode
DX-LR32: AT+OPENKEY0 → OK
DX-LR32: exited AT mode, module rebooting
role=relay node_id=2 default_slot=2 hop=1
relay searching: send HELLO seq=... bytes=...
rx JOIN_ACK: parent=1 hop=1 slot=2
rx SYNC sync_seq=... offset_ms=... -> forward bytes=...
```

传感节点入网时：

```text
rx sensor HELLO from=3 -> tx JOIN_ACK seq=... bytes=...
forward sensor HELLO to gateway: src=3 hop=1 bytes=...
```

正常数据中继：

```text
rx DATA from sensor=3 temp=26.61C humidity=51.45% -> ack_bytes=... buffered_for_relay_slot=true
relay slot active: buffered_data=... pending_ack=... parent=... hop=1 sync_seq=... offset_ms=...
relay slot tx buffered DATA sensor_seq=... relay_seq=... bytes=...
```

每 5 秒 Slot 4 状态：

```text
relay status | parent=Some(1) hop=1 slot=2 sync_seq=... offset_ms=...
```

### 传感节点

启动后应看到：

```text
DX-LR32: entered AT mode
DX-LR32: AT+OPENKEY0 → OK
DX-LR32: exited AT mode, module rebooting
role=sensor node_id=3 default_slot=1 hop=2
sensor searching: send HELLO seq=... bytes=... parent=None
rx JOIN_ACK: parent=2 hop=2 slot=1
rx SYNC: sync_seq=... offset_ms=...
tx DATA seq=... parent=Some(2) temp=26.61C humidity=51.45% gateway_time=... bytes=...
rx ACK from=2 acked_seq=... acked_type=DATA cleared_pending=true
```

每 5 秒 Slot 4 状态：

```text
sensor status | parent=Some(2) hop=2 slot=1 sync_seq=... offset_ms=...
```

如果 SHT40 未接好，传感节点会打印读取失败并使用演示样本，网络协议仍可演示。

## 演示顺序

1. 启动网关，确认 AT 配置成功、周期广播 `SYNC`。
2. 启动中继，确认网关接纳 relay 入网（`rx HELLO from=2 role=relay -> tx JOIN_ACK`）。
3. 启动传感节点，确认中继返回 `JOIN_ACK`，同时网关日志出现 `topology: sensor(3) -> relay(2) -> gateway(1)`。
4. 观察传感节点 `DATA`、中继转发、网关接收和 ACK。
5. 观察 Slot 4 每 10 秒的状态汇总（三节点各自的 sync/offset/拓扑信息）。
6. 捂住 SHT40 或靠近热源触发 `ALARM`，确认网关蜂鸣器响。
7. 恢复普通 `DATA`，确认蜂鸣器关闭。

## 已知边界

- 固件启动时通过 AT 命令自动关闭模块密钥（`AT+OPENKEY0`），若模块已预先配置为透明模式则此步骤无副作用。其余参数（信道、速率等级等）沿用模块出厂默认值。
- 当前可靠传输是单帧 pending-ACK 窗口（最多重传 3 次），适合答辩 demo，不适合高吞吐。
- 超帧周期 5 s，每个 slot 1 s，共 5 个 slot。现场可按 LoRa 空口延迟调整。
