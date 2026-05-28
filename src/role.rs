use core::fmt;

pub const NET_ID: u16 = 0x4331;
pub const BROADCAST_ID: u8 = 0xff;
pub const GATEWAY_ID: u8 = 1;
pub const RELAY_ID: u8 = 2;
pub const SENSOR_ID: u8 = 3;
pub const DEMO_ZONE_ID: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeRole {
    Gateway = 1,
    Relay = 2,
    Sensor = 3,
}

impl NodeRole {
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Gateway),
            2 => Some(Self::Relay),
            3 => Some(Self::Sensor),
            _ => None,
        }
    }

    pub const fn node_id(self) -> u8 {
        match self {
            Self::Gateway => GATEWAY_ID,
            Self::Relay => RELAY_ID,
            Self::Sensor => SENSOR_ID,
        }
    }

    pub const fn parent_id(self) -> Option<u8> {
        match self {
            Self::Gateway => None,
            Self::Relay => Some(GATEWAY_ID),
            Self::Sensor => Some(RELAY_ID),
        }
    }

    pub const fn default_hop(self) -> u8 {
        match self {
            Self::Gateway => 0,
            Self::Relay => 1,
            Self::Sensor => 2,
        }
    }

    pub const fn default_slot(self) -> u8 {
        match self {
            Self::Gateway => 0,
            Self::Sensor => 1,
            Self::Relay => 2,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Relay => "relay",
            Self::Sensor => "sensor",
        }
    }
}

impl fmt::Display for NodeRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(all(
    feature = "gateway-node",
    not(any(feature = "relay-node", feature = "sensor-node"))
))]
pub const ACTIVE_ROLE: NodeRole = NodeRole::Gateway;

#[cfg(all(
    feature = "relay-node",
    not(any(feature = "gateway-node", feature = "sensor-node"))
))]
pub const ACTIVE_ROLE: NodeRole = NodeRole::Relay;

#[cfg(all(
    feature = "sensor-node",
    not(any(feature = "gateway-node", feature = "relay-node"))
))]
pub const ACTIVE_ROLE: NodeRole = NodeRole::Sensor;

#[cfg(not(any(
    all(
        feature = "gateway-node",
        not(any(feature = "relay-node", feature = "sensor-node"))
    ),
    all(
        feature = "relay-node",
        not(any(feature = "gateway-node", feature = "sensor-node"))
    ),
    all(
        feature = "sensor-node",
        not(any(feature = "gateway-node", feature = "relay-node"))
    ),
)))]
compile_error!("enable exactly one role feature: sensor-node, relay-node, or gateway-node");
