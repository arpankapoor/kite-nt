use nautilus_live::task::TaskSpawnError;
use nautilus_network::TransportError;
use nautilus_network::error::NetworkConfigError;
use thiserror::Error;
use tokio::sync::oneshot::error::RecvError;

/// Failure while controlling or decoding the Kite WebSocket feed.
#[derive(Debug, Error)]
pub enum KiteWsError {
    /// Background WebSocket handler task could not be started.
    #[error(transparent)]
    TaskSpawn(#[from] TaskSpawnError),

    /// The WebSocket handler stopped before returning a command result.
    #[error(transparent)]
    ResponseChannel(#[from] RecvError),

    /// WebSocket transport failure.
    #[error(transparent)]
    Transport(#[from] TransportError),

    /// Invalid network-client configuration.
    #[error(transparent)]
    NetworkConfig(#[from] NetworkConfigError),

    /// The shared feed handler is no longer accepting commands.
    #[error("Kite WebSocket feed handler has stopped")]
    HandlerStopped,

    /// A consumer of this kind is already connected.
    #[error("Kite WebSocket {0} consumer is already connected")]
    ConsumerAlreadyConnected(&'static str),

    /// A packet ended before its declared length.
    #[error("incomplete WebSocket frame: expected {expected} bytes, found {remaining}")]
    IncompleteFrame {
        /// Required byte count.
        expected: usize,
        /// Available byte count.
        remaining: usize,
    },

    /// Kite sent a packet length unsupported by its documented protocol.
    #[error("unsupported Kite WebSocket packet length {0}")]
    InvalidPacketLength(usize),

    /// Kite supplied an invalid UNIX timestamp.
    #[error("invalid {field} timestamp {value}")]
    InvalidTimestamp {
        /// Packet field containing the timestamp.
        field: &'static str,
        /// Invalid timestamp value.
        value: u32,
    },

    /// Bytes remained after all declared packets were decoded.
    #[error("Kite WebSocket frame has {0} trailing bytes")]
    TrailingBytes(usize),
}
