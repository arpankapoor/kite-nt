//! # Kite Connect adapter for NautilusTrader
//!
//! `kite-nt` provides [NautilusTrader] live data and execution clients for [Kite Connect]. The
//! clients share the underlying HTTP and WebSocket connections.
//!
//! > **Note**
//! >
//! > Independent project; not affiliated with, endorsed by, or supported by Nautech Systems Pty
//! > Ltd or its NautilusTrader project, nor by Zerodha Broking Ltd.
//!
//! The adapter covers instrument discovery, historical and live market data, account
//! reconciliation, and order management. Add it to a Nautilus node through
//! [`KiteDataClientFactory`](factories::KiteDataClientFactory) and
//! [`KiteExecutionClientFactory`](factories::KiteExecutionClientFactory), or use
//! [`KiteHttpClient`](http::client::KiteHttpClient) and
//! [`KiteWsClient`](websocket::client::KiteWsClient) directly.
//!
//! ## Getting started
//!
//! Direct the user to [`login_url`](login::login_url), then exchange the returned request token
//! with [`exchange_request_token`](login::exchange_request_token). Create shared clients and
//! register both adapter configurations with the live node:
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use kite_nt::{
//!     credential::{KiteApiKey, KiteApiSecret, KiteRequestToken, KiteSessionCredentials},
//!     config::{KiteDataClientConfig, KiteExecutionClientConfig},
//!     factories::{KiteDataClientFactory, KiteExecutionClientFactory},
//!     http::client::KiteHttpClient,
//!     login::{exchange_request_token, login_url},
//!     websocket::client::KiteWsClient,
//! };
//! use nautilus_common::enums::Environment;
//! use nautilus_core::string::secret::SecretString;
//! use nautilus_live::node::LiveNode;
//! use nautilus_model::identifiers::{AccountId, InstrumentId, TraderId};
//!
//! # async fn configure(request_token: &str) -> Result<(), Box<dyn std::error::Error>> {
//! let api_key = KiteApiKey::new(SecretString::from("api-key"));
//! let api_secret = KiteApiSecret::new(SecretString::from("api-secret"));
//!
//! // Send the user to this URL. After login, Kite redirects to the application's configured URL
//! // with a short-lived `request_token` query parameter.
//! let authorization_url = login_url(&api_key);
//! # let _ = authorization_url;
//!
//! // Exchange the returned request token immediately; it is short-lived and single-use.
//! let request_token = KiteRequestToken::new(SecretString::from(request_token));
//! let access_token = exchange_request_token(&api_key, &api_secret, &request_token).await?;
//! let session_credentials = Arc::new(KiteSessionCredentials::new(
//!     api_key,
//!     access_token,
//! ));
//! let http_client = KiteHttpClient::new(Arc::clone(&session_credentials))?;
//! let ws_client = KiteWsClient::new(session_credentials, None)?;
//! // Data and execution clients resolve instruments from the same universe.
//! let instrument_universe = vec![InstrumentId::from("NIFTY 50.NSE")];
//!
//! let data_config = KiteDataClientConfig {
//!     http_client: http_client.clone(),
//!     ws_client: ws_client.clone(),
//!     instrument_store: None,
//!     instrument_universe: instrument_universe.clone(),
//! };
//! let execution_config = KiteExecutionClientConfig {
//!     http_client,
//!     ws_client,
//!     account_id: AccountId::from("KITE-001"),
//!     instrument_store: None,
//!     instrument_universe,
//! };
//!
//! let _node = LiveNode::builder(TraderId::from("TRADER-001"), Environment::Live)?
//!     .add_data_client(None, Box::new(KiteDataClientFactory), Box::new(data_config))?
//!     .add_exec_client(
//!         None,
//!         Box::new(KiteExecutionClientFactory),
//!         Box::new(execution_config),
//!     )?
//!     .build()?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Execution behavior
//!
//! The execution client handles orders for `NSE`, `BSE`, `NFO`, and `BFO` instruments. It sends
//! regular orders and rejects unsupported Nautilus instructions such as post-only, reduce-only,
//! quote-quantity, and contingent orders. Set the [`KITE_PRODUCT_PARAM`] order parameter to `CNC`,
//! `MIS`, or `NRML` when the default `CNC` product is not appropriate; derivative orders require
//! `MIS` or `NRML`.
//!
//! [`KITE_PRODUCT_PARAM`]: execution::models::KITE_PRODUCT_PARAM
//!
//! [Kite Connect]: https://zerodha.com/products/api/
//! [NautilusTrader]: https://nautilustrader.io/

mod common;
/// Nautilus data and execution client configuration.
pub mod config;
/// Kite Connect authentication credentials.
pub mod credential;
mod data;
/// Kite open-interest custom data types.
pub mod data_types;
/// Kite order models and Zerodha commission calculation.
pub mod execution;
/// Nautilus client factories.
pub mod factories;
/// Kite Connect HTTP client, errors, and API models.
pub mod http;
/// Kite instrument loading, filtering, and metadata access.
pub mod instruments;
/// Kite Connect interactive login helpers.
pub mod login;
mod urls;
/// Kite WebSocket transport, messages, and decoding.
pub mod websocket;
