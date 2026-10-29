//! Kite order and trade wire models.
//!
//! Field names intentionally mirror Kite's [orders API].
//!
//! [orders API]: https://kite.trade/docs/connect/v3/orders/

use jiff::civil::{DateTime, Time};
use nautilus_model::enums::{OrderSide, OrderType, TimeInForce};
use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, Serialize};
use strum::EnumString;

use crate::http::error::KiteException;

/// Nautilus order parameter used to override Kite's product for an order.
pub const KITE_PRODUCT_PARAM: &str = "kite_product";

/// Kite order varieties.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum KiteOrderVariety {
    /// Regular order.
    #[serde(rename = "regular")]
    Regular,
    /// After Market Order.
    #[serde(rename = "amo")]
    Amo,
    /// Cover Order.
    #[serde(rename = "co")]
    Cover,
    /// Iceberg Order.
    #[serde(rename = "iceberg")]
    Iceberg,
    /// Auction Order.
    #[serde(rename = "auction")]
    Auction,
}

/// Kite order types.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum KiteOrderType {
    /// Market order.
    #[serde(rename = "MARKET")]
    Market,
    /// Limit order.
    #[serde(rename = "LIMIT")]
    Limit,
    /// Stoploss order.
    #[serde(rename = "SL")]
    StopLimit,
    /// Stoploss-market order.
    #[serde(rename = "SL-M")]
    StopMarket,
}

/// Kite transaction direction.
#[derive(Clone, Copy, Debug, Deserialize, EnumString, Eq, PartialEq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
#[strum(serialize_all = "UPPERCASE")]
pub enum KiteTransactionType {
    /// Buy transaction.
    Buy,
    /// Sell transaction.
    Sell,
}

/// Kite product used for an order or position.
#[derive(Clone, Copy, Debug, Default, Deserialize, EnumString, Eq, PartialEq, Serialize)]
pub enum KiteProduct {
    /// Cash & Carry for equity.
    #[serde(rename = "CNC")]
    #[strum(serialize = "CNC")]
    #[default]
    CashAndCarry,
    /// Normal for futures and options.
    #[serde(rename = "NRML")]
    #[strum(serialize = "NRML")]
    Normal,
    /// Margin Intraday Squareoff for futures and options.
    #[serde(rename = "MIS")]
    #[strum(serialize = "MIS")]
    Intraday,
    /// Margin Trading Facility.
    #[serde(rename = "MTF")]
    #[strum(serialize = "MTF")]
    MarginTradingFacility,
    /// Cover Order.
    #[serde(rename = "CO")]
    #[strum(serialize = "CO")]
    Cover,
}

/// Kite order-validity values.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum KiteValidity {
    /// Regular order.
    #[serde(rename = "DAY")]
    Day,
    /// Immediate or Cancel.
    #[serde(rename = "IOC")]
    ImmediateOrCancel,
    /// Order validity in minutes.
    #[serde(rename = "TTL")]
    TimeToLive,
}

impl From<OrderSide> for KiteTransactionType {
    fn from(value: OrderSide) -> Self {
        match value {
            OrderSide::Buy => Self::Buy,
            OrderSide::Sell => Self::Sell,
        }
    }
}

impl From<KiteTransactionType> for OrderSide {
    fn from(value: KiteTransactionType) -> Self {
        match value {
            KiteTransactionType::Buy => Self::Buy,
            KiteTransactionType::Sell => Self::Sell,
        }
    }
}

impl From<KiteOrderType> for OrderType {
    fn from(value: KiteOrderType) -> Self {
        match value {
            KiteOrderType::Market => Self::Market,
            KiteOrderType::Limit => Self::Limit,
            KiteOrderType::StopLimit => Self::StopLimit,
            KiteOrderType::StopMarket => Self::StopMarket,
        }
    }
}

impl From<KiteValidity> for TimeInForce {
    fn from(value: KiteValidity) -> Self {
        match value {
            KiteValidity::Day => Self::Day,
            KiteValidity::ImmediateOrCancel => Self::Ioc,
            KiteValidity::TimeToLive => Self::Gtd,
        }
    }
}

/// Status reported by Kite for an order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KiteOrderStatus {
    /// The order is open at the exchange.
    Open,
    /// The order has been filled completely.
    Complete,
    /// The order has been cancelled.
    Cancelled,
    /// The order has been rejected.
    Rejected,
    /// The backend has received the order request.
    PutOrderRequestReceived,
    /// The order is awaiting validation by the RMS (Risk Management System).
    ValidationPending,
    /// The order is awaiting registration at the exchange.
    OpenPending,
    /// Modified order values are awaiting validation by the RMS.
    ModifyValidationPending,
    /// Modified order values are awaiting registration at the exchange.
    ModifyPending,
    /// The order is awaiting a fill based on its trigger price.
    TriggerPending,
    /// The cancellation request is awaiting registration at the exchange.
    CancelPending,
    /// The backend has received an after-market order request.
    AmoRequestReceived,
    /// The requested modification has been applied.
    Modified,
    /// A WebSocket postback indicating an order-book change.
    Update,
    /// A status not currently documented by Kite.
    Other(String),
}

/// An order record returned by Kite.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteOrderRecord {
    /// Unique order identifier.
    pub order_id: String,
    /// Parent order identifier, when applicable.
    pub parent_order_id: Option<String>,
    /// Exchange-issued order identifier.
    pub exchange_order_id: Option<String>,
    /// Whether the order has been modified.
    pub modified: bool,
    /// User ID that placed the order.
    pub placed_by: String,
    /// Order variety.
    pub variety: KiteOrderVariety,
    /// Current order status.
    pub status: KiteOrderStatus,
    /// Exchange trading symbol.
    #[serde(rename = "tradingsymbol")]
    pub trading_symbol: String,
    /// Exchange on which the order was placed.
    pub exchange: String,
    /// Numerical instrument identifier.
    pub instrument_token: u32,
    /// Buy or sell transaction type.
    pub transaction_type: KiteTransactionType,
    /// Order type.
    pub order_type: KiteOrderType,
    /// Product used for the order.
    pub product: KiteProduct,
    /// Order validity.
    pub validity: KiteValidity,
    /// Time-to-live in minutes for TTL orders.
    pub validity_ttl: Option<u32>,
    /// Limit price.
    pub price: Decimal,
    /// Requested quantity.
    pub quantity: u64,
    /// Trigger price for stop orders.
    pub trigger_price: Decimal,
    /// Average execution price.
    pub average_price: Decimal,
    /// Quantity awaiting execution.
    pub pending_quantity: u64,
    /// Executed quantity.
    pub filled_quantity: u64,
    /// Quantity disclosed to the market.
    pub disclosed_quantity: u64,
    /// Time at which the order was registered by Kite.
    pub order_timestamp: DateTime,
    /// Time at which the order was registered by the exchange.
    pub exchange_timestamp: Option<DateTime>,
    /// Time of the most recent exchange update.
    pub exchange_update_timestamp: Option<DateTime>,
    /// Human-readable status message.
    pub status_message: Option<String>,
    /// Raw status message from the exchange.
    pub status_message_raw: Option<String>,
    /// Cancelled quantity.
    pub cancelled_quantity: u64,
    /// Auction number for auction orders.
    pub auction_number: Option<String>,
    /// Additional order metadata.
    #[serde(default)]
    pub meta: serde_json::Value,
    /// Primary order tag.
    pub tag: Option<String>,
    /// All order tags.
    pub tags: Option<Vec<String>>,
    /// Kite request GUID.
    pub guid: Option<String>,
    /// Market-protection percentage or Kite's automatic-protection sentinel.
    #[serde(default)]
    pub market_protection: i32,
}

/// An order update delivered by Kite's WebSocket API.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteOrderUpdate {
    /// Unique order identifier.
    pub order_id: String,
    /// Exchange-issued order identifier.
    pub exchange_order_id: Option<String>,
    /// Parent order identifier, when applicable.
    pub parent_order_id: Option<String>,
    /// User ID that placed the order.
    pub placed_by: String,
    /// Kite application ID, when placed through an API application.
    pub app_id: Option<u64>,
    /// Current order status.
    pub status: Option<KiteOrderStatus>,
    /// Human-readable status message.
    pub status_message: Option<String>,
    /// Raw status message from the exchange.
    pub status_message_raw: Option<String>,
    /// Exchange trading symbol.
    #[serde(rename = "tradingsymbol")]
    pub trading_symbol: String,
    /// Numerical instrument identifier.
    pub instrument_token: u32,
    /// Exchange on which the order was placed.
    pub exchange: String,
    /// Order type.
    pub order_type: KiteOrderType,
    /// Buy or sell transaction type.
    pub transaction_type: KiteTransactionType,
    /// Order validity.
    pub validity: KiteValidity,
    /// Order variety.
    pub variety: KiteOrderVariety,
    /// Product used for the order.
    pub product: KiteProduct,
    /// Average execution price.
    pub average_price: Decimal,
    /// Quantity disclosed to the market.
    pub disclosed_quantity: u64,
    /// Limit price.
    pub price: Decimal,
    /// Requested quantity.
    pub quantity: u64,
    /// Executed quantity.
    pub filled_quantity: u64,
    /// Unfilled quantity.
    pub unfilled_quantity: u64,
    /// Quantity awaiting execution.
    pub pending_quantity: u64,
    /// Cancelled quantity.
    pub cancelled_quantity: u64,
    /// Trigger price for stop orders.
    pub trigger_price: Decimal,
    /// Kite user ID, when included in the postback.
    pub user_id: Option<String>,
    /// Kite account ID, when included in the postback.
    pub account_id: Option<String>,
    /// Time at which the order was registered by Kite.
    pub order_timestamp: DateTime,
    /// Time of the most recent exchange update.
    pub exchange_update_timestamp: Option<DateTime>,
    /// Time at which the order was registered by the exchange.
    pub exchange_timestamp: Option<DateTime>,
    /// Postback checksum.
    pub checksum: String,
    /// Additional order metadata.
    pub meta: serde_json::Value,
    /// Order tag.
    pub tag: Option<String>,
    /// Kite request GUID.
    pub guid: Option<String>,
    /// Market-protection percentage or Kite's automatic-protection sentinel.
    pub market_protection: i32,
}

/// A trade record returned by `GET /trades` or `GET /orders/:order_id/trades`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteTradeRecord {
    /// Unique trade identifier.
    pub trade_id: String,
    /// Order that produced the trade.
    pub order_id: String,
    /// Exchange-issued order identifier.
    pub exchange_order_id: Option<String>,
    /// Exchange trading symbol.
    #[serde(rename = "tradingsymbol")]
    pub trading_symbol: String,
    /// Exchange on which the trade occurred.
    pub exchange: String,
    /// Numerical instrument identifier.
    pub instrument_token: u32,
    /// Buy or sell transaction type.
    pub transaction_type: KiteTransactionType,
    /// Product used for the order.
    pub product: KiteProduct,
    /// Average execution price.
    pub average_price: Decimal,
    /// Executed quantity.
    #[serde(alias = "filled")]
    pub quantity: u64,
    /// Trade fill timestamp.
    pub fill_timestamp: DateTime,
    /// Order timestamp.
    pub order_timestamp: Time,
    /// Exchange timestamp.
    pub exchange_timestamp: DateTime,
}

/// Parameters for placing a Kite order.
#[derive(Clone, Debug, Serialize)]
pub struct KitePlaceOrderRequest {
    /// Exchange trading symbol.
    #[serde(rename = "tradingsymbol")]
    pub trading_symbol: String,
    /// Exchange on which to place the order.
    pub exchange: String,
    /// Buy or sell transaction type.
    pub transaction_type: KiteTransactionType,
    /// Order type.
    pub order_type: KiteOrderType,
    /// Order quantity.
    pub quantity: u64,
    /// Product to use for the order.
    pub product: KiteProduct,
    /// Limit price.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<Decimal>,
    /// Trigger price for stop orders.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_price: Option<Decimal>,
    /// Quantity disclosed to the market.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disclosed_quantity: Option<u64>,
    /// Order validity.
    pub validity: KiteValidity,
    /// Time-to-live in minutes for TTL orders.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validity_ttl: Option<u32>,
    /// Number of iceberg legs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iceberg_legs: Option<u8>,
    /// Quantity in each iceberg leg.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iceberg_quantity: Option<u64>,
    /// Auction number for auction orders.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auction_number: Option<String>,
    /// Market-protection percentage or Kite's automatic-protection sentinel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_protection: Option<i32>,
    /// Whether Kite should automatically slice orders exceeding the freeze quantity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autoslice: Option<bool>,
    /// Exchange-provided algorithm identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub algo_id: Option<String>,
    /// User-defined order tag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

/// Successful place-order response, including autoslice child results.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KitePlaceOrderData {
    /// Primary order identifier.
    pub order_id: String,
    /// Results for autoslice child orders.
    #[serde(default)]
    pub children: Vec<KiteOrderSliceResult>,
}

/// Result of placing one autoslice child order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub enum KiteOrderSliceResult {
    /// Successfully placed child order.
    Order {
        /// Child order identifier.
        order_id: String,
    },
    /// Child order rejected by Kite.
    Error {
        /// Rejection details.
        error: KiteOrderSliceError,
    },
}

/// Error returned for an individual autoslice child order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteOrderSliceError {
    /// HTTP-style error code.
    pub code: u16,
    /// Kite exception category.
    pub error_type: KiteException,
    /// Human-readable error message.
    pub message: String,
    /// Additional error data.
    pub data: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct KiteOrderIdData {
    pub order_id: String,
}

impl KiteOrderVariety {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Regular => "regular",
            Self::Amo => "amo",
            Self::Cover => "co",
            Self::Iceberg => "iceberg",
            Self::Auction => "auction",
        }
    }
}

impl<'de> Deserialize<'de> for KiteOrderStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match String::deserialize(deserializer)?.as_str() {
            "OPEN" => Self::Open,
            "COMPLETE" => Self::Complete,
            "CANCELLED" => Self::Cancelled,
            "REJECTED" => Self::Rejected,
            "PUT ORDER REQ RECEIVED" => Self::PutOrderRequestReceived,
            "VALIDATION PENDING" => Self::ValidationPending,
            "OPEN PENDING" => Self::OpenPending,
            "MODIFY VALIDATION PENDING" => Self::ModifyValidationPending,
            "MODIFY PENDING" => Self::ModifyPending,
            "TRIGGER PENDING" => Self::TriggerPending,
            "CANCEL PENDING" => Self::CancelPending,
            "AMO REQ RECEIVED" => Self::AmoRequestReceived,
            "MODIFIED" => Self::Modified,
            "UPDATE" => Self::Update,
            status => Self::Other(status.to_owned()),
        })
    }
}

/// Parameters for modifying a Kite order.
#[derive(Clone, Debug, Serialize)]
pub struct KiteModifyOrderRequest {
    /// Child order identifier for iceberg orders.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    /// Parent order identifier for iceberg orders.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_order_id: Option<String>,
    /// Replacement order type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_type: Option<KiteOrderType>,
    /// Replacement quantity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantity: Option<u64>,
    /// Replacement limit price.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<Decimal>,
    /// Replacement trigger price.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_price: Option<Decimal>,
    /// Replacement disclosed quantity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disclosed_quantity: Option<u64>,
    /// Replacement validity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validity: Option<KiteValidity>,
    /// Replacement market-protection value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_protection: Option<i32>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::common::consts::VENUE_NSE;
    use crate::execution::models::{
        KiteModifyOrderRequest, KiteOrderRecord, KiteOrderSliceResult, KiteOrderStatus,
        KiteOrderType, KiteOrderVariety, KitePlaceOrderData, KitePlaceOrderRequest, KiteProduct,
        KiteTradeRecord, KiteTransactionType, KiteValidity,
    };

    #[test]
    fn serializes_documented_regular_order_request() {
        let request = KitePlaceOrderRequest {
            trading_symbol: "ACC".to_owned(),
            exchange: VENUE_NSE.to_owned(),
            transaction_type: KiteTransactionType::Buy,
            order_type: KiteOrderType::Market,
            quantity: 1,
            product: KiteProduct::Intraday,
            price: None,
            trigger_price: None,
            disclosed_quantity: None,
            validity: KiteValidity::Day,
            validity_ttl: None,
            iceberg_legs: None,
            iceberg_quantity: None,
            auction_number: None,
            market_protection: Some(-1),
            autoslice: None,
            algo_id: None,
            tag: None,
        };

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "tradingsymbol": "ACC",
                "exchange": VENUE_NSE,
                "transaction_type": "BUY",
                "order_type": "MARKET",
                "quantity": 1,
                "product": "MIS",
                "validity": "DAY",
                "market_protection": -1
            }),
        );
    }

    #[test]
    fn deserializes_documented_rest_order() {
        let order: KiteOrderRecord = serde_json::from_value(json!({
            "placed_by": "XXXXXX",
            "order_id": "100000000000000",
            "exchange_order_id": "200000000000000",
            "parent_order_id": null,
            "status": "COMPLETE",
            "status_message": null,
            "status_message_raw": null,
            "order_timestamp": "2021-05-31 15:20:28",
            "exchange_update_timestamp": "2021-05-31 15:20:28",
            "exchange_timestamp": "2021-05-31 15:20:28",
            "variety": "regular",
            "modified": false,
            "exchange": VENUE_NSE,
            "tradingsymbol": "IOC",
            "instrument_token": 415745,
            "order_type": "LIMIT",
            "transaction_type": "BUY",
            "validity": "DAY",
            "product": "CNC",
            "quantity": 1,
            "disclosed_quantity": 0,
            "price": 109.4,
            "trigger_price": 0,
            "average_price": 109.4,
            "filled_quantity": 1,
            "pending_quantity": 0,
            "cancelled_quantity": 0,
            "market_protection": 0,
            "meta": {},
            "tag": "connect test order",
            "tags": ["connect test order"],
            "guid": "XXXXXX"
        }))
        .unwrap();

        assert_eq!(order.variety, KiteOrderVariety::Regular);
        assert_eq!(order.status, KiteOrderStatus::Complete);
        assert_eq!(order.order_type, KiteOrderType::Limit);
        assert_eq!(order.transaction_type, KiteTransactionType::Buy);
        assert_eq!(order.product, KiteProduct::CashAndCarry);
        assert_eq!(order.tags, Some(vec!["connect test order".to_owned()]));
    }

    #[test]
    fn deserializes_documented_order_history_entry() {
        let order: KiteOrderRecord = serde_json::from_value(json!({
            "average_price": 0,
            "cancelled_quantity": 0,
            "disclosed_quantity": 0,
            "exchange": VENUE_NSE,
            "exchange_order_id": null,
            "exchange_timestamp": null,
            "filled_quantity": 0,
            "instrument_token": 1,
            "order_id": "171229000724687",
            "order_timestamp": "2017-12-29 11:06:52",
            "order_type": "LIMIT",
            "parent_order_id": null,
            "pending_quantity": 1,
            "placed_by": "DA0017",
            "price": 300,
            "product": "CNC",
            "quantity": 1,
            "status": "PUT ORDER REQ RECEIVED",
            "status_message": null,
            "tag": null,
            "tradingsymbol": "SBIN",
            "transaction_type": "BUY",
            "trigger_price": 0,
            "validity": "DAY",
            "variety": "regular",
            "modified": false
        }))
        .unwrap();

        assert_eq!(order.status, KiteOrderStatus::PutOrderRequestReceived);
        assert_eq!(order.market_protection, 0);
        assert_eq!(order.meta, serde_json::Value::Null);
    }

    #[test]
    fn preserves_an_unknown_order_status() {
        let status: KiteOrderStatus = serde_json::from_value(json!("NEW STATUS")).unwrap();

        assert_eq!(status, KiteOrderStatus::Other("NEW STATUS".to_owned()));
    }

    #[test]
    fn deserializes_documented_trade_response() {
        let trade: KiteTradeRecord = serde_json::from_value(json!({
            "trade_id": "10000000",
            "order_id": "200000000000000",
            "exchange_order_id": "300000000000000",
            "tradingsymbol": "SBIN",
            "exchange": VENUE_NSE,
            "instrument_token": 779521,
            "transaction_type": "BUY",
            "product": "CNC",
            "average_price": 420.65,
            "quantity": 1,
            "fill_timestamp": "2021-05-31 09:16:39",
            "order_timestamp": "09:16:39",
            "exchange_timestamp": "2021-05-31 09:16:39"
        }))
        .unwrap();

        assert_eq!(trade.quantity, 1);
        assert_eq!(trade.transaction_type, KiteTransactionType::Buy);
        assert_eq!(trade.product, KiteProduct::CashAndCarry);
    }

    #[test]
    fn serializes_documented_modify_order_request() {
        let request = KiteModifyOrderRequest {
            order_id: None,
            parent_order_id: None,
            order_type: Some(KiteOrderType::Market),
            quantity: Some(3),
            price: None,
            trigger_price: None,
            disclosed_quantity: None,
            validity: Some(KiteValidity::Day),
            market_protection: Some(-1),
        };

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "order_type": "MARKET",
                "quantity": 3,
                "validity": "DAY",
                "market_protection": -1
            }),
        );
    }

    #[test]
    fn deserializes_mixed_autoslice_results() {
        let data: KitePlaceOrderData = serde_json::from_value(json!({
            "order_id": "1914227164488687616",
            "children": [
                { "order_id": "1914227164488687616" },
                { "order_id": "1914227164534824960" },
                { "order_id": "1914227164580962304" },
                {
                    "error": {
                        "code": 400,
                        "error_type": "MarginException",
                        "message": "Insufficient funds. Required margin is 228365.92 but available margin is 228358.50.",
                        "data": null
                    }
                },
                { "order_id": "1914227164681625600" }
            ]
        }))
        .unwrap();

        assert_eq!(data.order_id, "1914227164488687616");
        assert_eq!(data.children.len(), 5);
        assert!(matches!(
            data.children[0],
            KiteOrderSliceResult::Order { .. }
        ));
        assert!(matches!(
            data.children[3],
            KiteOrderSliceResult::Error { .. }
        ));
        assert!(matches!(
            data.children[4],
            KiteOrderSliceResult::Order { .. }
        ));
    }
}
