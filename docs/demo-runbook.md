# 管廊传感网 Demo 联调步骤

## 目标

用三块 ESP32-C3 Pro Mini 和三个 DX-LR32-433T22D 模块演示固定链路：

```text
sensor-node -> relay-node -> gateway-node
```

演示内容覆盖自组织入网、TDMA、网关逐级时间同步、SHT40 温湿度上报、ACK/重传和网关蜂鸣器告警。

## 预检查

1. 确认三块 LoRa 模块参数一致：
   - 模块：DX-LR32-433T22D
   - 频段：433 MHz
   - UART：9600 baud
   - 信道：23
   - 空中速率：2400 bps
   - 发射功率：22 dBm
   - NetID：0x4331
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
role=gateway node_id=1 default_slot=0 hop=0
gateway online: broadcasting SYNC/SCHEDULE every TDMA superframe
tx SYNC ...
rx HELLO from=2 role=relay -> tx JOIN_ACK ...
rx DATA via=2 ...
```

告警时应看到：

```text
rx ALARM ...
alarm active: buzzer on
```

恢复后应看到：

```text
rx DATA ...
alarm cleared: buzzer off
```

### 中继

启动后应看到：

```text
role=relay node_id=2 default_slot=2 hop=1
relay searching: send HELLO ...
rx JOIN_ACK: parent=1 hop=1 slot=2
rx SYNC ... -> forward bytes=...
rx DATA from sensor=3 ... -> ack_bytes=... forward_bytes=...
```

### 传感节点

启动后应看到：

```text
role=sensor node_id=3 default_slot=1 hop=2
sensor searching: send HELLO ...
rx JOIN_ACK: parent=2 hop=2 slot=1
rx SYNC: sync_seq=... offset_ms=...
tx DATA ...
rx ACK ... cleared_pending=true
```

如果 SHT40 未接好，传感节点会打印读取失败并使用演示样本，网络协议仍可演示。

## 演示顺序

1. 启动网关，确认周期广播 `SYNC`。
2. 启动中继，确认网关只接纳 relay 入网。
3. 启动传感节点，确认由中继返回 `JOIN_ACK`。
4. 观察传感节点 `DATA`、中继转发、网关接收和 ACK。
5. 捂住 SHT40 或降低阈值触发 `ALARM`，确认网关蜂鸣器响。
6. 恢复普通 `DATA`，确认蜂鸣器关闭。

## 已知边界

- DX-LR32 运行时配置命令尚未写入固件，需要按模块手册确认命令格式和配置脚连接。
- 当前可靠传输是单帧 pending-ACK 窗口，适合答辩 demo，不适合高吞吐。
- TDMA slot 长度先用 2 s，现场可按 LoRa 空口延迟和串口日志调整。
