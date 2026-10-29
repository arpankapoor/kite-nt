//! Shared Kite WebSocket access, wire messages, and decoding.
//!
//! See Kite's [WebSocket streaming documentation] for the wire protocol.
//!
//! [WebSocket streaming documentation]: https://kite.trade/docs/connect/v3/websocket/

/// Shared Kite WebSocket client.
pub mod client;
/// Kite WebSocket transport and packet errors.
pub mod error;
/// Kite WebSocket wire messages and adapter events.
pub mod messages;
/// Kite WebSocket frame decoding and Nautilus normalization.
pub mod parse;

pub(crate) mod handler;
