use std::any::Any;
use std::sync::Arc;

use nautilus_common::factories::ClientConfig;
use nautilus_model::identifiers::{AccountId, InstrumentId};

use crate::http::client::KiteHttpClient;
use crate::instruments::KiteInstrumentStore;
use crate::websocket::client::KiteWsClient;

/// Configuration used by [`KiteDataClientFactory`](crate::factories::KiteDataClientFactory).
#[derive(Clone, Debug)]
pub struct KiteDataClientConfig {
    /// Shared authenticated HTTP client.
    pub http_client: KiteHttpClient,
    /// Shared WebSocket client.
    pub ws_client: KiteWsClient,
    /// Optional storage for dated instrument-master snapshots.
    pub instrument_store: Option<Arc<dyn KiteInstrumentStore>>,
    /// Instruments made available to the Nautilus data client.
    pub instrument_universe: Vec<InstrumentId>,
}

/// Configuration used by
/// [`KiteExecutionClientFactory`](crate::factories::KiteExecutionClientFactory).
#[derive(Clone, Debug)]
pub struct KiteExecutionClientConfig {
    /// Shared authenticated HTTP client.
    pub http_client: KiteHttpClient,
    /// Shared WebSocket client.
    pub ws_client: KiteWsClient,
    /// Nautilus account identifier assigned to the Kite account.
    pub account_id: AccountId,
    /// Optional storage for dated instrument-master snapshots.
    pub instrument_store: Option<Arc<dyn KiteInstrumentStore>>,
    /// Instruments made available to the Nautilus execution client.
    pub instrument_universe: Vec<InstrumentId>,
}

impl ClientConfig for KiteDataClientConfig {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl ClientConfig for KiteExecutionClientConfig {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
