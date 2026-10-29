//! Kite Connect HTTP access and wire models.
//!
//! See the official [Kite Connect API documentation].
//!
//! [Kite Connect API documentation]: https://kite.trade/docs/connect/v3/

/// Authenticated Kite Connect HTTP client.
pub mod client;
pub(crate) mod consts;
/// Kite Connect HTTP and API errors.
pub mod error;
/// Kite Connect request and response models.
pub mod models;
/// Conversion of Kite wire data into Nautilus domain types.
pub mod parse;
pub(crate) mod response;
