//! Node roles and fixed demo topology identifiers.
//!
//! The demo topology is gateway -> relay -> sensor. Exactly one role feature is
//! selected at build time for firmware images, while host tests can construct
//! any role directly.

use core::fmt;

/// Demo network identifier written into every protocol frame.
pub const NET_ID: u16 = 0x4331;
/// Broadcast destination accepted by every node.
pub const BROADCAST_ID: u8 = 0xff;
/// Fixed gateway node ID for the demo topology.
pub const GATEWAY_ID: u8 = 1;
/// Fixed relay node ID for the demo topology.
pub const RELAY_ID: u8 = 2;
/// Fixed sensor node ID for the demo topology.
pub const SENSOR_ID: u8 = 3;
/// Fixed demo zone ID written into every frame.
pub const DEMO_ZONE_ID: u8 = 1;

/// Role of a node in the fixed three-node demo topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeRole {
    /// Root node and time authority.
    Gateway = 1,
    /// Middle hop between the sensor and gateway.
    Relay = 2,
    /// Leaf node that produces environment samples.
    Sensor = 3,
}

impl NodeRole {
    /// Decode the compact wire value used in frame headers.
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Gateway),
            2 => Some(Self::Relay),
            3 => Some(Self::Sensor),
            _ => None,
        }
    }

    /// Fixed node ID assigned to this role.
    pub const fn node_id(self) -> u8 {
        match self {
            Self::Gateway => GATEWAY_ID,
            Self::Relay => RELAY_ID,
            Self::Sensor => SENSOR_ID,
        }
    }

    /// Preferred upstream parent in the demo topology.
    pub const fn parent_id(self) -> Option<u8> {
        match self {
            Self::Gateway => None,
            Self::Relay => Some(GATEWAY_ID),
            Self::Sensor => Some(RELAY_ID),
        }
    }

    /// Default route hop count used before receiving JOIN_ACK.
    pub const fn default_hop(self) -> u8 {
        match self {
            Self::Gateway => 0,
            Self::Relay => 1,
            Self::Sensor => 2,
        }
    }

    /// Default TDMA data slot for this role.
    pub const fn default_slot(self) -> u8 {
        match self {
            Self::Gateway => 0,
            Self::Sensor => 2,
            Self::Relay => 3,
        }
    }

    /// Lowercase role label used in logs.
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
