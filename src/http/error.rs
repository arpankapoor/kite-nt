use nautilus_core::correctness::CorrectnessError;
use nautilus_network::http::HttpClientError;
use nautilus_network::retry::RetryError;
use serde::Deserialize;
use thiserror::Error;

/// Exception category returned by Kite Connect.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub enum KiteException {
    /// Authentication or access-token failure.
    #[serde(rename = "TokenException")]
    Token,
    /// User-account failure.
    #[serde(rename = "UserException")]
    User,
    /// Order operation failure.
    #[serde(rename = "OrderException")]
    Order,
    /// Invalid request input.
    #[serde(rename = "InputException")]
    Input,
    /// Insufficient margin or related risk failure.
    #[serde(rename = "MarginException")]
    Margin,
    /// Holdings-related failure.
    #[serde(rename = "HoldingException")]
    Holding,
    /// Network failure reported by Kite.
    #[serde(rename = "NetworkException")]
    Network,
    /// Market-data failure.
    #[serde(rename = "DataException")]
    Data,
    /// Unclassified Kite failure.
    #[serde(rename = "GeneralException")]
    General,
}

/// Failure from a Kite Connect HTTP request or response conversion.
#[derive(Debug, Error)]
pub enum KiteHttpError {
    /// Nautilus HTTP client failure.
    #[error(transparent)]
    Client(#[from] HttpClientError),

    /// JSON response decoding failure.
    #[error(transparent)]
    Json(#[from] serde_json::Error),

    /// Form request encoding failure.
    #[error(transparent)]
    Form(#[from] serde_urlencoded::ser::Error),

    /// Instrument CSV decoding failure.
    #[error(transparent)]
    Csv(#[from] csv::Error),

    /// Nautilus model validation failure.
    #[error(transparent)]
    Correctness(#[from] CorrectnessError),

    /// Structured error returned by Kite Connect.
    #[error("HTTP {status}: {error_type:?}: {message}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Kite exception category.
        error_type: KiteException,
        /// Human-readable Kite error message.
        message: String,
    },

    /// Non-success response without a decodable Kite error body.
    #[error("unexpected HTTP status {0}")]
    HttpStatus(u16),

    /// Retry manager exhausted or aborted the operation.
    #[error(transparent)]
    Retry(#[from] RetryError),
}
