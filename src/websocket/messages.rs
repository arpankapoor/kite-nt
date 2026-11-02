//! Kite WebSocket protocol requests, decoded packets, and adapter events.
//!
//! Packet fields follow Kite's [WebSocket streaming documentation].
//!
//! [WebSocket streaming documentation]: https://kite.trade/docs/connect/v3/websocket/

use bytes::Bytes;
use jiff::Timestamp;
use nautilus_core::UnixNanos;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use strum::{FromRepr, VariantArray};

use crate::execution::models::KiteOrderUpdate;

/// Subscription modes supported by the Kite WebSocket API.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, VariantArray)]
#[serde(rename_all = "lowercase")]
pub enum KiteWebSocketMode {
    /// Last traded price only.
    Ltp,
    /// Quote fields excluding market depth.
    Quote,
    /// Quote fields including market depth.
    Full,
}

/// A last-traded-price packet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KiteLtp {
    /// Numerical instrument identifier from the Kite instrument list.
    pub instrument_token: u32,
    /// Last traded price.
    pub last_price: Decimal,
}

/// A quote packet excluding market depth.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KiteQuote {
    /// Numerical instrument identifier from the Kite instrument list.
    pub instrument_token: u32,
    /// Last traded price.
    pub last_price: Decimal,
    /// Last traded quantity.
    pub last_quantity: u32,
    /// Average traded price.
    pub average_price: Decimal,
    /// Volume traded for the day.
    pub volume: u32,
    /// Total buy quantity.
    pub total_buy_quantity: u32,
    /// Total sell quantity.
    pub total_sell_quantity: u32,
    /// Open price of the day.
    pub open: Decimal,
    /// High price of the day.
    pub high: Decimal,
    /// Low price of the day.
    pub low: Decimal,
    /// Close price.
    pub close: Decimal,
}

/// A full quote packet including market depth.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KiteFullQuote {
    /// Quote fields common to quote and full modes.
    pub quote: KiteQuote,
    /// Last traded timestamp.
    pub last_trade_timestamp: Option<Timestamp>,
    /// Open interest.
    pub open_interest: u32,
    /// Open-interest day high.
    pub open_interest_day_high: u32,
    /// Open-interest day low.
    pub open_interest_day_low: u32,
    /// Exchange timestamp.
    pub exchange_timestamp: Option<Timestamp>,
    /// Five bid market-depth entries.
    pub bids: [KiteDepthLevel; 5],
    /// Five offer market-depth entries.
    pub offers: [KiteDepthLevel; 5],
}

/// A quote packet for an index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KiteIndexQuote {
    /// Numerical index token.
    pub instrument_token: u32,
    /// Last traded price.
    pub last_price: Decimal,
    /// High of the day.
    pub high: Decimal,
    /// Low of the day.
    pub low: Decimal,
    /// Open of the day.
    pub open: Decimal,
    /// Close of the day.
    pub close: Decimal,
    /// Price change.
    pub price_change: Decimal,
}

/// A full quote packet for an index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KiteIndexFullQuote {
    /// Index quote fields.
    pub quote: KiteIndexQuote,
    /// Exchange timestamp.
    pub exchange_timestamp: Option<Timestamp>,
}

/// One bid or offer entry in the market depth.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KiteDepthLevel {
    /// Quantity available at this level.
    pub quantity: u32,
    /// Price at this level.
    pub price: Decimal,
    /// Number of orders at this level.
    pub orders: u16,
}

/// Decoded market data from a Kite binary quote packet.
#[derive(Clone, Debug, Eq, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "Keeping full quotes inline avoids a heap allocation for every full-mode tick"
)]
pub enum KiteMarketData {
    /// Last-traded-price data.
    Ltp(KiteLtp),
    /// Tradeable-instrument quote data without market depth.
    Quote(KiteQuote),
    /// Tradeable-instrument quote data with market depth.
    Full(KiteFullQuote),
    /// Index quote data.
    IndexQuote(KiteIndexQuote),
    /// Full index quote data.
    IndexFull(KiteIndexFullQuote),
}

/// Market-data events delivered by the shared WebSocket feed.
#[derive(Debug)]
pub enum KiteWsDataEvent {
    /// One or more decoded quote packets.
    MarketData {
        /// Decoded market data.
        data: Vec<KiteMarketData>,
        /// Time at which the WebSocket frame was received.
        ts_init: UnixNanos,
    },
    /// The WebSocket transport reconnected.
    Reconnected,
}

/// Execution events delivered by the shared WebSocket feed.
#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "Order updates are low volume, so avoiding a second heap allocation is preferable"
)]
pub enum KiteWsExecutionEvent {
    /// An order postback received as a text message.
    OrderUpdate {
        /// Kite order update.
        update: KiteOrderUpdate,
        /// Time at which the WebSocket frame was received.
        ts_init: UnixNanos,
    },
    /// Broker message or alert.
    Message(String),
    /// The WebSocket transport reconnected.
    Reconnected,
}

/// Kind of payload stored in a raw WebSocket frame.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, FromRepr, PartialEq)]
pub enum KiteWsFrameKind {
    /// Binary market-data payload.
    Binary = 1,
    /// Text update payload.
    Text = 2,
}

/// A raw WebSocket payload and its receipt timestamp.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KiteWsFrame {
    /// Time at which the WebSocket frame was received.
    pub ts_init: UnixNanos,
    /// Payload kind.
    pub kind: KiteWsFrameKind,
    /// Unmodified WebSocket payload.
    pub payload: Bytes,
}

/// A decoded Kite WebSocket message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KiteWsMessage {
    /// One or more binary quote packets.
    MarketData(Vec<KiteMarketData>),
    /// Order postback.
    OrderUpdate(Box<KiteOrderUpdate>),
    /// Error response.
    Error(String),
    /// Broker message or alert.
    Message(String),
    /// Instrument metadata update.
    InstrumentsMeta {
        /// Entity tag identifying the instrument-master version.
        etag: String,
        /// Number of instruments in the instrument master.
        count: u64,
    },
    /// Unrecognized text message preserved for forward compatibility.
    RawText(Bytes),
    /// The WebSocket transport reconnected.
    Reconnected,
}

impl KiteMarketData {
    /// Returns the numerical instrument token carried by the packet.
    #[must_use]
    pub const fn instrument_token(&self) -> u32 {
        match self {
            Self::Ltp(ltp) => ltp.instrument_token,
            Self::Quote(quote) => quote.instrument_token,
            Self::Full(full) => full.quote.instrument_token,
            Self::IndexQuote(quote) => quote.instrument_token,
            Self::IndexFull(full) => full.quote.instrument_token,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub(crate) enum KiteWsTextMessage {
    Order(Box<KiteOrderUpdate>),
    Error(String),
    Message(String),
    InstrumentsMeta { etag: String, count: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "a", content = "v", rename_all = "lowercase")]
pub(crate) enum KiteWsRequest {
    Subscribe(Vec<u32>),
    Unsubscribe(Vec<u32>),
    Mode(KiteWebSocketMode, Vec<u32>),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::common::consts::VENUE_NSE;
    use crate::execution::models::{KiteOrderType, KiteProduct};
    use crate::websocket::messages::{KiteWebSocketMode, KiteWsRequest, KiteWsTextMessage};

    #[test]
    fn serializes_subscription_requests() {
        assert_eq!(
            serde_json::to_value(KiteWsRequest::Subscribe(vec![408_065, 884_737])).unwrap(),
            json!({"a": "subscribe", "v": [408_065, 884_737]}),
        );
        assert_eq!(
            serde_json::to_value(KiteWsRequest::Unsubscribe(vec![408_065])).unwrap(),
            json!({"a": "unsubscribe", "v": [408_065]}),
        );
    }

    #[test]
    fn serializes_mode_request() {
        assert_eq!(
            serde_json::to_value(KiteWsRequest::Mode(KiteWebSocketMode::Full, vec![408_065]))
                .unwrap(),
            json!({"a": "mode", "v": ["full", [408_065]]}),
        );
    }

    #[test]
    fn deserializes_documented_text_messages() {
        let order = json!({
            "type": "order",
            "data": {
                "user_id": "AB1234",
                "unfilled_quantity": 0,
                "app_id": 1234,
                "checksum": "checksum",
                "placed_by": "AB1234",
                "order_id": "220303000308932",
                "exchange_order_id": "1000000001482421",
                "parent_order_id": null,
                "status": "COMPLETE",
                "status_message": null,
                "status_message_raw": null,
                "order_timestamp": "2022-03-03 09:24:25",
                "exchange_update_timestamp": "2022-03-03 09:24:25",
                "exchange_timestamp": "2022-03-03 09:24:25",
                "variety": "regular",
                "exchange": VENUE_NSE,
                "tradingsymbol": "SBIN",
                "instrument_token": 779521,
                "order_type": "MARKET",
                "transaction_type": "BUY",
                "validity": "DAY",
                "product": "CNC",
                "quantity": 1,
                "disclosed_quantity": 0,
                "price": 0,
                "trigger_price": 0,
                "average_price": 470,
                "filled_quantity": 1,
                "pending_quantity": 0,
                "cancelled_quantity": 0,
                "market_protection": 0,
                "meta": {},
                "tag": null,
                "guid": "XXXXXX"
            }
        });
        let KiteWsTextMessage::Order(order) = serde_json::from_value(order).unwrap() else {
            panic!("expected an order update");
        };
        assert_eq!(order.order_id, "220303000308932");
        assert_eq!(order.instrument_token, 779_521);
        assert_eq!(order.order_type, KiteOrderType::Market);
        assert_eq!(order.product, KiteProduct::CashAndCarry);
        assert_eq!(order.unfilled_quantity, 0);

        assert_eq!(
            serde_json::from_value::<KiteWsTextMessage>(
                json!({"type": "error", "data": "Token is invalid"})
            )
            .unwrap(),
            KiteWsTextMessage::Error("Token is invalid".to_owned()),
        );
        assert_eq!(
            serde_json::from_value::<KiteWsTextMessage>(
                json!({"type": "message", "data": "Market is closed"})
            )
            .unwrap(),
            KiteWsTextMessage::Message("Market is closed".to_owned()),
        );
    }
}
