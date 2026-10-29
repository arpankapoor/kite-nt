//! Kite Connect HTTP response and request wire models.
//!
//! Field names intentionally mirror the [Kite Connect API documentation].
//!
//! [Kite Connect API documentation]: https://kite.trade/docs/connect/v3/

use bytes::Bytes;
use jiff::Timestamp;
use jiff::civil::{Date, DateTime};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use strum::EnumString;

use crate::execution::charges::KiteChargeBreakdown;
use crate::execution::models::{
    KiteOrderType, KiteOrderVariety, KitePlaceOrderRequest, KiteProduct, KiteTransactionType,
};
use crate::http::error::KiteException;

/// One row from Kite's daily instrument list.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteInstrument {
    /// Numerical identifier used for subscribing to live market quotes with the WebSocket API.
    pub instrument_token: u64,
    /// The numerical identifier issued by the exchange representing the instrument.
    pub exchange_token: u64,
    /// Exchange tradingsymbol of the instrument.
    #[serde(rename = "tradingsymbol")]
    pub trading_symbol: String,
    /// Name of the company (for equity instruments).
    pub name: Option<String>,
    /// Last traded market price.
    pub last_price: Decimal,
    /// Expiry date (for derivatives).
    pub expiry: Option<Date>,
    /// Strike (for options).
    pub strike: Option<Decimal>,
    /// Value of a single price tick.
    pub tick_size: Decimal,
    /// Quantity of a single lot.
    pub lot_size: u64,
    /// Instrument type: `EQ`, `FUT`, `CE`, or `PE`.
    pub instrument_type: KiteInstrumentType,
    /// Segment the instrument belongs to.
    pub segment: String,
    /// Exchange.
    pub exchange: String,
}

/// Instrument classifications present in Kite's instrument master.
#[derive(Clone, Copy, Debug, Deserialize, EnumString, Eq, PartialEq)]
pub enum KiteInstrumentType {
    /// Equity or index instrument.
    #[serde(rename = "EQ")]
    #[strum(serialize = "EQ")]
    Equity,
    /// Futures contract.
    #[serde(rename = "FUT")]
    #[strum(serialize = "FUT")]
    Future,
    /// Call option contract.
    #[serde(rename = "CE")]
    #[strum(serialize = "CE")]
    CallOption,
    /// Put option contract.
    #[serde(rename = "PE")]
    #[strum(serialize = "PE")]
    PutOption,
}

/// Historical candle intervals supported by Kite.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum KiteCandleInterval {
    /// One-minute candles.
    Minute,
    /// Three-minute candles.
    ThreeMinutes,
    /// Five-minute candles.
    FiveMinutes,
    /// Ten-minute candles.
    TenMinutes,
    /// Fifteen-minute candles.
    FifteenMinutes,
    /// Thirty-minute candles.
    ThirtyMinutes,
    /// Sixty-minute candles.
    SixtyMinutes,
    /// Daily candles.
    Day,
}

/// One candle returned by Kite's historical-data API.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(from = "RawHistoricalCandle")]
pub struct KiteHistoricalCandle {
    /// Candle start timestamp.
    pub timestamp: Timestamp,
    /// Opening price.
    pub open: Decimal,
    /// Highest price.
    pub high: Decimal,
    /// Lowest price.
    pub low: Decimal,
    /// Closing price.
    pub close: Decimal,
    /// Traded volume.
    pub volume: u64,
    /// Open interest, when requested and available.
    pub open_interest: Option<u64>,
}

/// A full market quote returned by Kite's [market-quotes API].
///
/// [market-quotes API]: https://kite.trade/docs/connect/v3/market-quotes/#retrieving-full-market-quotes
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteMarketQuote {
    /// Numerical identifier issued by the exchange representing the instrument.
    pub instrument_token: u32,
    /// Exchange timestamp of the quote.
    pub timestamp: Option<DateTime>,
    /// Last trade timestamp.
    pub last_trade_time: Option<DateTime>,
    /// Last traded market price.
    pub last_price: Decimal,
    /// Last traded quantity.
    pub last_quantity: u64,
    /// Total quantity of buy orders pending at the exchange.
    pub buy_quantity: u64,
    /// Total quantity of sell orders pending at the exchange.
    pub sell_quantity: u64,
    /// Volume traded for the day.
    pub volume: u64,
    /// Volume-weighted average traded price.
    pub average_price: Decimal,
    /// Open interest.
    pub oi: u64,
    /// Highest open interest recorded for the day.
    pub oi_day_high: u64,
    /// Lowest open interest recorded for the day.
    pub oi_day_low: u64,
    /// Absolute change from the previous close.
    pub net_change: Decimal,
    /// Current lower circuit limit.
    pub lower_circuit_limit: Decimal,
    /// Current upper circuit limit.
    pub upper_circuit_limit: Decimal,
    /// Open, high, low, and previous close prices.
    pub ohlc: KiteQuoteOhlc,
    /// Buy and sell market depth.
    pub depth: KiteMarketDepth,
}

/// An OHLC and last-traded-price snapshot returned by Kite's [market-quotes API].
///
/// [market-quotes API]: https://kite.trade/docs/connect/v3/market-quotes/#retrieving-ohlc-quotes
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteOhlcQuote {
    /// Numerical identifier issued by the exchange representing the instrument.
    pub instrument_token: u32,
    /// Last traded market price.
    pub last_price: Decimal,
    /// Open, high, low, and previous close prices.
    pub ohlc: KiteQuoteOhlc,
}

/// A last-traded-price snapshot returned by Kite's [market-quotes API].
///
/// [market-quotes API]: https://kite.trade/docs/connect/v3/market-quotes/#retrieving-ltp-quotes
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteLtpQuote {
    /// Numerical identifier issued by the exchange representing the instrument.
    pub instrument_token: u32,
    /// Last traded market price.
    pub last_price: Decimal,
}

/// Open, high, low, and previous close values in a full market quote.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteQuoteOhlc {
    /// Opening price.
    pub open: Decimal,
    /// Highest price for the day.
    pub high: Decimal,
    /// Lowest price for the day.
    pub low: Decimal,
    /// Previous closing price.
    pub close: Decimal,
}

/// Buy and sell depth in a full market quote.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteMarketDepth {
    /// Buy-side depth.
    pub buy: Vec<KiteMarketDepthLevel>,
    /// Sell-side depth.
    pub sell: Vec<KiteMarketDepthLevel>,
}

/// One price level in full market-quote depth.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteMarketDepthLevel {
    /// Price at this depth level.
    pub price: Decimal,
    /// Quantity available at this depth level.
    pub quantity: u64,
    /// Number of open orders at this depth level.
    pub orders: u64,
}

/// The authenticated user's Kite profile.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteUserProfile {
    /// Unique, permanent user ID registered with the broker and exchanges.
    pub user_id: String,
    /// User's registered role at the broker.
    pub user_type: String,
    /// User's email.
    pub email: String,
    /// User's real name.
    pub user_name: String,
    /// Shortened version of the user's real name.
    pub user_shortname: String,
    /// Broker ID.
    pub broker: String,
    /// Exchanges enabled for trading on the user's account.
    pub exchanges: Vec<String>,
    /// Margin product types enabled for the user.
    pub products: Vec<String>,
    /// Order types enabled for the user.
    pub order_types: Vec<String>,
    /// Full URL to the user's avatar, when present.
    pub avatar_url: Option<String>,
    /// User metadata supplied by Kite.
    pub meta: serde_json::Value,
}

/// Equity and commodity funds and margins for the authenticated user.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteUserMargins {
    /// Equity-segment margins.
    pub equity: KiteSegmentMargins,
    /// Commodity-segment margins.
    pub commodity: KiteSegmentMargins,
}

/// Margin balances for one trading segment.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteSegmentMargins {
    /// Whether the segment is enabled for the user.
    pub enabled: bool,
    /// Net cash balance available for trading.
    pub net: Decimal,
    /// Available margin components.
    pub available: KiteAvailableMargins,
    /// Utilised margin components.
    pub utilised: KiteUtilisedMargins,
}

/// Available margin components for a trading segment.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteAvailableMargins {
    /// Available cash.
    pub cash: Decimal,
    /// Opening balance at the start of the day.
    pub opening_balance: Decimal,
    /// Current available balance.
    pub live_balance: Decimal,
    /// Intraday pay-in amount.
    pub intraday_payin: Decimal,
    /// Ad-hoc margin credits.
    pub adhoc_margin: Decimal,
    /// Available collateral value.
    pub collateral: Decimal,
}

/// Utilised margin components for a trading segment.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteUtilisedMargins {
    /// Unrealised mark-to-market loss.
    pub m2m_unrealised: Decimal,
    /// Realised mark-to-market loss.
    pub m2m_realised: Decimal,
    /// Total debits against the account.
    pub debits: Decimal,
    /// SPAN margin blocked.
    pub span: Decimal,
    /// Option premium paid.
    pub option_premium: Decimal,
    /// Margin used by holding sales.
    pub holding_sales: Decimal,
    /// Exposure margin blocked.
    pub exposure: Decimal,
    /// Liquid collateral value.
    pub liquid_collateral: Decimal,
    /// Margin used for delivery positions.
    pub delivery: Decimal,
    /// Stock collateral value.
    pub stock_collateral: Decimal,
    /// Turnover-related margin usage.
    pub turnover: Decimal,
    /// Funds awaiting payout.
    pub payout: Decimal,
}

/// The `net` and `day` position sets returned by Kite.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KitePositions {
    /// Actual current net position portfolio.
    pub net: Vec<KitePosition>,
    /// Snapshot of the day's buying and selling activity.
    pub day: Vec<KitePosition>,
}

/// Position and profit-and-loss values for one instrument and product.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KitePosition {
    /// Exchange trading symbol.
    #[serde(rename = "tradingsymbol")]
    pub trading_symbol: String,
    /// Exchange.
    pub exchange: String,
    /// Numerical instrument identifier.
    pub instrument_token: u32,
    /// Position product.
    pub product: KiteProduct,
    /// Current net quantity.
    pub quantity: i64,
    /// Quantity carried overnight.
    pub overnight_quantity: i64,
    /// Contract multiplier.
    pub multiplier: Decimal,
    /// Average net position price.
    pub average_price: Decimal,
    /// Previous closing price.
    pub close_price: Decimal,
    /// Last traded price.
    pub last_price: Decimal,
    /// Net position value.
    pub value: Decimal,
    /// Total profit and loss.
    pub pnl: Decimal,
    /// Mark-to-market profit and loss.
    pub m2m: Decimal,
    /// Unrealised profit and loss.
    pub unrealised: Decimal,
    /// Realised profit and loss.
    pub realised: Decimal,
    /// Total buy quantity.
    pub buy_quantity: u64,
    /// Average buy price.
    pub buy_price: Decimal,
    /// Total buy value.
    pub buy_value: Decimal,
    /// Buy-side mark-to-market value.
    pub buy_m2m: Decimal,
    /// Buy quantity for the current day.
    pub day_buy_quantity: u64,
    /// Average buy price for the current day.
    pub day_buy_price: Decimal,
    /// Buy value for the current day.
    pub day_buy_value: Decimal,
    /// Total sell quantity.
    pub sell_quantity: u64,
    /// Average sell price.
    pub sell_price: Decimal,
    /// Total sell value.
    pub sell_value: Decimal,
    /// Sell-side mark-to-market value.
    pub sell_m2m: Decimal,
    /// Sell quantity for the current day.
    pub day_sell_quantity: u64,
    /// Average sell price for the current day.
    pub day_sell_price: Decimal,
    /// Sell value for the current day.
    pub day_sell_value: Decimal,
}

/// A holding returned by Kite.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteHolding {
    /// Exchange trading symbol.
    #[serde(rename = "tradingsymbol")]
    pub trading_symbol: String,
    /// Exchange.
    pub exchange: String,
    /// Numerical instrument identifier.
    pub instrument_token: u32,
    /// International Securities Identification Number.
    pub isin: String,
    /// Quantity awaiting settlement.
    pub t1_quantity: i64,
    /// Settled quantity.
    pub realised_quantity: i64,
    /// Total holding quantity.
    pub quantity: i64,
    /// Quantity used by open sell orders.
    pub used_quantity: i64,
    /// Quantity authorised for sale.
    pub authorised_quantity: i64,
    /// Holding quantity at the start of the day.
    pub opening_quantity: i64,
    /// Date and time of the sale authorisation.
    pub authorised_date: Option<DateTime>,
    /// Current market price used by Kite.
    pub price: Decimal,
    /// Average acquisition price.
    pub average_price: Decimal,
    /// Last traded price.
    pub last_price: Decimal,
    /// Previous closing price.
    pub close_price: Decimal,
    /// Profit and loss.
    pub pnl: Decimal,
    /// Absolute daily price change.
    pub day_change: Decimal,
    /// Daily percentage price change.
    pub day_change_percentage: Decimal,
    /// Holding product.
    pub product: KiteProduct,
    /// Quantity pledged as collateral.
    pub collateral_quantity: i64,
    /// Collateral classification.
    pub collateral_type: Option<String>,
    /// Whether Kite detected a holding discrepancy.
    pub discrepancy: bool,
    /// Quantity sold short from the holding.
    pub short_quantity: i64,
    /// Margin Trading Facility values.
    pub mtf: KiteMtfHolding,
}

/// Margin Trading Facility values attached to a holding.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteMtfHolding {
    /// Quantity funded through MTF.
    pub quantity: i64,
    /// MTF quantity used by open orders.
    pub used_quantity: i64,
    /// Average acquisition price.
    pub average_price: Decimal,
    /// Funded holding value.
    pub value: Decimal,
    /// Initial margin paid by the user.
    pub initial_margin: Decimal,
}

/// Order input for calculating charges.
#[derive(Clone, Debug, Serialize)]
pub struct KiteOrderChargesRequest {
    /// Order identifier used to group charges for a logical order.
    pub order_id: String,
    /// Exchange.
    pub exchange: String,
    /// Exchange trading symbol.
    #[serde(rename = "tradingsymbol")]
    pub trading_symbol: String,
    /// Buy or sell transaction type.
    pub transaction_type: KiteTransactionType,
    /// Order variety.
    pub variety: KiteOrderVariety,
    /// Order product.
    pub product: KiteProduct,
    /// Order type.
    pub order_type: KiteOrderType,
    /// Filled quantity.
    pub quantity: u64,
    /// Average fill price.
    pub average_price: Decimal,
}

/// Charges calculated by Kite for an order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteOrderCharges {
    /// Buy or sell transaction type.
    pub transaction_type: KiteTransactionType,
    /// Exchange trading symbol.
    #[serde(rename = "tradingsymbol")]
    pub trading_symbol: String,
    /// Exchange.
    pub exchange: String,
    /// Order variety.
    pub variety: KiteOrderVariety,
    /// Order product.
    pub product: KiteProduct,
    /// Order type.
    pub order_type: KiteOrderType,
    /// Filled quantity.
    pub quantity: u64,
    /// Fill price used for the calculation.
    pub price: Decimal,
    /// Itemised charges.
    pub charges: KiteChargeBreakdown,
}

/// Order input for calculating margins.
#[derive(Clone, Debug, Serialize)]
pub struct KiteOrderMarginRequest {
    /// Exchange.
    pub exchange: String,
    /// Exchange trading symbol.
    #[serde(rename = "tradingsymbol")]
    pub trading_symbol: String,
    /// Buy or sell transaction type.
    pub transaction_type: KiteTransactionType,
    /// Order variety.
    pub variety: KiteOrderVariety,
    /// Order product.
    pub product: KiteProduct,
    /// Order type.
    pub order_type: KiteOrderType,
    /// Order quantity.
    pub quantity: u64,
    /// Order price.
    pub price: Decimal,
    /// Trigger price for stop orders.
    pub trigger_price: Decimal,
}

/// Margin requirement calculated by Kite for an order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteOrderMargin {
    /// Margin calculation type returned by Kite.
    #[serde(rename = "type")]
    pub margin_type: String,
    /// Exchange trading symbol.
    #[serde(rename = "tradingsymbol")]
    pub trading_symbol: String,
    /// Exchange.
    pub exchange: String,
    /// SPAN margin requirement.
    pub span: Decimal,
    /// Exposure margin requirement.
    pub exposure: Decimal,
    /// Option premium requirement.
    pub option_premium: Decimal,
    /// Additional margin requirement.
    pub additional: Decimal,
    /// Bracket-order margin requirement.
    pub bo: Decimal,
    /// Cash component of the requirement.
    pub cash: Decimal,
    /// Value-at-risk margin requirement.
    pub var: Decimal,
    /// Profit and loss used in the calculation.
    pub pnl: KiteMarginPnl,
    /// Effective leverage.
    pub leverage: Decimal,
    /// Estimated order charges.
    pub charges: KiteChargeBreakdown,
    /// Total margin requirement.
    pub total: Decimal,
}

/// Realised and unrealised P&L included in an order-margin response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct KiteMarginPnl {
    /// Realised profit and loss.
    pub realised: Decimal,
    /// Unrealised profit and loss.
    pub unrealised: Decimal,
}

impl From<(&KitePlaceOrderRequest, KiteOrderVariety)> for KiteOrderMarginRequest {
    fn from((request, variety): (&KitePlaceOrderRequest, KiteOrderVariety)) -> Self {
        Self {
            exchange: request.exchange.clone(),
            trading_symbol: request.trading_symbol.clone(),
            transaction_type: request.transaction_type,
            variety,
            product: request.product,
            order_type: request.order_type,
            quantity: request.quantity,
            price: request.price.unwrap_or_default(),
            trigger_price: request.trigger_price.unwrap_or_default(),
        }
    }
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "status")]
pub(crate) enum KiteResponse<T, E = serde_json::Value> {
    #[serde(rename = "success")]
    Success { data: T },

    #[serde(rename = "error")]
    Error {
        message: String,
        error_type: KiteException,
        #[serde(default)]
        data: Option<E>,
    },
}

#[derive(Debug, Deserialize)]
pub(crate) struct KiteHistoricalCandleData {
    pub(crate) candles: Vec<KiteHistoricalCandle>,
}

/// Raw CSV bytes from Kite's instrument-master endpoint.
#[derive(Clone, Debug)]
pub struct KiteInstrumentCsv {
    bytes: Bytes,
}

#[derive(Deserialize)]
struct RawHistoricalCandle(
    Timestamp,
    Decimal,
    Decimal,
    Decimal,
    Decimal,
    u64,
    #[serde(default)] Option<u64>,
);

impl From<RawHistoricalCandle> for KiteHistoricalCandle {
    fn from(raw: RawHistoricalCandle) -> Self {
        let RawHistoricalCandle(timestamp, open, high, low, close, volume, open_interest) = raw;

        Self {
            timestamp,
            open,
            high,
            low,
            close,
            volume,
            open_interest,
        }
    }
}

impl KiteCandleInterval {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Minute => "minute",
            Self::ThreeMinutes => "3minute",
            Self::FiveMinutes => "5minute",
            Self::TenMinutes => "10minute",
            Self::FifteenMinutes => "15minute",
            Self::ThirtyMinutes => "30minute",
            Self::SixtyMinutes => "60minute",
            Self::Day => "day",
        }
    }

    /// Returns a request duration within Kite's interval limit.
    ///
    /// See <https://kite.trade/forum/discussion/15886/number-of-years-of-historical-data>.
    pub(crate) const fn maximum_request_days(self) -> i64 {
        match self {
            Self::Minute => 30,
            Self::ThreeMinutes | Self::FiveMinutes | Self::TenMinutes => 90,
            Self::FifteenMinutes | Self::ThirtyMinutes => 180,
            Self::SixtyMinutes => 365,
            // Kite's 2000-day limit counts both endpoint dates, so leave a small buffer.
            Self::Day => 1990,
        }
    }
}

impl KiteInstrumentCsv {
    /// Wraps downloaded instrument-master CSV bytes.
    pub fn new(bytes: Bytes) -> Self {
        Self { bytes }
    }

    /// Returns the unmodified CSV bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal::dec;
    use serde_json::{Value, json};

    use crate::common::consts::VENUE_NSE;
    use crate::http::error::KiteException;
    use crate::http::models::{
        KiteHistoricalCandleData, KiteHolding, KiteMarketQuote, KiteOrderCharges, KiteOrderMargin,
        KitePositions, KiteResponse, KiteUserMargins,
    };

    #[test]
    fn deserializes_success_response() {
        let response: KiteResponse<Value> = serde_json::from_value(json!({
            "status": "success",
            "data": {}
        }))
        .unwrap();

        assert_eq!(response, KiteResponse::Success { data: json!({}) });
    }

    #[test]
    fn deserializes_error_response() {
        let response: KiteResponse<Value> = serde_json::from_value(json!({
            "status": "error",
            "message": "Error message",
            "error_type": "GeneralException"
        }))
        .unwrap();

        assert_eq!(
            response,
            KiteResponse::Error {
                message: "Error message".to_owned(),
                error_type: KiteException::General,
                data: None,
            }
        );
    }

    #[test]
    fn deserializes_documented_historical_candle() {
        let data: KiteHistoricalCandleData = serde_json::from_value(json!({
            "candles": [[
                "2017-12-15T09:15:00+0530",
                1704.5,
                1705,
                1699.25,
                1702.8,
                2499
            ]]
        }))
        .unwrap();
        let candle = &data.candles[0];

        assert_eq!(candle.timestamp, "2017-12-15T03:45:00Z".parse().unwrap());
        assert_eq!(candle.open, dec!(1704.5));
        assert_eq!(candle.volume, 2499);
        assert_eq!(candle.open_interest, None);
    }

    #[test]
    fn deserializes_documented_historical_candle_with_open_interest() {
        let data: KiteHistoricalCandleData = serde_json::from_value(json!({
            "candles": [[
                "2019-12-04T09:15:00+0530",
                12009.9,
                12019.35,
                12001.25,
                12001.5,
                163275,
                13667775
            ]]
        }))
        .unwrap();
        let candle = &data.candles[0];

        assert_eq!(candle.close, dec!(12001.5));
        assert_eq!(candle.volume, 163275);
        assert_eq!(candle.open_interest, Some(13667775));
    }

    #[test]
    fn deserializes_documented_full_market_quote() {
        let quote: KiteMarketQuote = serde_json::from_value(json!({
            "instrument_token": 408065,
            "timestamp": null,
            "last_trade_time": "2021-06-08 15:45:52",
            "last_price": 1412.95,
            "last_quantity": 5,
            "buy_quantity": 0,
            "sell_quantity": 5191,
            "volume": 7360198,
            "average_price": 1412.47,
            "oi": 0,
            "oi_day_high": 0,
            "oi_day_low": 0,
            "net_change": 0,
            "lower_circuit_limit": 1250.70,
            "upper_circuit_limit": 1528.60,
            "ohlc": {
                "open": 1396,
                "high": 1421.75,
                "low": 1395.55,
                "close": 1389.65
            },
            "depth": {
                "buy": [{"price": 1412.90, "quantity": 445, "orders": 4}],
                "sell": [{"price": 1412.95, "quantity": 5191, "orders": 13}]
            }
        }))
        .unwrap();

        assert_eq!(quote.timestamp, None);
        assert_eq!(quote.lower_circuit_limit, dec!(1250.70));
        assert_eq!(quote.upper_circuit_limit, dec!(1528.60));
        assert_eq!(quote.ohlc.close, dec!(1389.65));
        assert_eq!(quote.depth.buy[0].price, dec!(1412.90));
        assert_eq!(quote.depth.sell[0].quantity, 5191);
    }

    #[test]
    fn deserializes_documented_user_margins() {
        let margins: KiteUserMargins = serde_json::from_value(json!({
            "equity": {
                "enabled": true,
                "net": 99725.05000000002,
                "available": {
                    "adhoc_margin": 0,
                    "cash": 245431.6,
                    "opening_balance": 245431.6,
                    "live_balance": 99725.05000000002,
                    "collateral": 0,
                    "intraday_payin": 0
                },
                "utilised": {
                    "debits": 145706.55,
                    "exposure": 38981.25,
                    "m2m_realised": 761.7,
                    "m2m_unrealised": 0,
                    "option_premium": 0,
                    "payout": 0,
                    "span": 101989,
                    "holding_sales": 0,
                    "turnover": 0,
                    "liquid_collateral": 0,
                    "stock_collateral": 0,
                    "delivery": 0
                }
            },
            "commodity": {
                "enabled": true,
                "net": 100661.7,
                "available": {
                    "adhoc_margin": 0,
                    "cash": 100661.7,
                    "opening_balance": 100661.7,
                    "live_balance": 100661.7,
                    "collateral": 0,
                    "intraday_payin": 0
                },
                "utilised": {
                    "debits": 0,
                    "exposure": 0,
                    "m2m_realised": 0,
                    "m2m_unrealised": 0,
                    "option_premium": 0,
                    "payout": 0,
                    "span": 0,
                    "holding_sales": 0,
                    "turnover": 0,
                    "liquid_collateral": 0,
                    "stock_collateral": 0,
                    "delivery": 0
                }
            }
        }))
        .unwrap();

        assert_eq!(margins.equity.available.cash, dec!(245431.6));
        assert_eq!(margins.equity.utilised.debits, dec!(145706.55));
        assert_eq!(margins.commodity.net, dec!(100661.7));
    }

    #[test]
    fn deserializes_documented_positions() {
        let positions: KitePositions = serde_json::from_value(json!({
            "net": [{
                "tradingsymbol": "GOLDGUINEA17DECFUT",
                "exchange": "MCX",
                "instrument_token": 53505799,
                "product": "NRML",
                "quantity": -3,
                "overnight_quantity": 0,
                "multiplier": 1,
                "average_price": 23340,
                "close_price": 23232,
                "last_price": 23355,
                "value": 69972,
                "pnl": -93,
                "m2m": -93,
                "unrealised": -93,
                "realised": 0,
                "buy_quantity": 1,
                "buy_price": 23388,
                "buy_value": 23388,
                "buy_m2m": 23388,
                "sell_quantity": 4,
                "sell_price": 23340,
                "sell_value": 93360,
                "sell_m2m": 93360,
                "day_buy_quantity": 1,
                "day_buy_price": 23388,
                "day_buy_value": 23388,
                "day_sell_quantity": 4,
                "day_sell_price": 23340,
                "day_sell_value": 93360
            }],
            "day": []
        }))
        .unwrap();

        assert_eq!(positions.net[0].quantity, -3);
        assert_eq!(positions.net[0].pnl, dec!(-93));
        assert!(positions.day.is_empty());
    }

    #[test]
    fn deserializes_documented_holding() {
        let holdings: Vec<KiteHolding> = serde_json::from_value(json!([{
            "tradingsymbol": "AARON",
            "exchange": VENUE_NSE,
            "instrument_token": 263681,
            "isin": "INE721Z01010",
            "product": "CNC",
            "price": 0,
            "quantity": 1,
            "used_quantity": 0,
            "t1_quantity": 0,
            "realised_quantity": 1,
            "authorised_quantity": 0,
            "authorised_date": "2025-01-17 00:00:00",
            "authorisation": {},
            "opening_quantity": 1,
            "short_quantity": 0,
            "collateral_quantity": 0,
            "collateral_type": "",
            "discrepancy": false,
            "average_price": 161,
            "last_price": 352.95,
            "close_price": 352.35,
            "pnl": 191.95,
            "day_change": 0.6,
            "day_change_percentage": 0.17028522775648244,
            "mtf": {
                "quantity": 1000,
                "used_quantity": 0,
                "average_price": 100,
                "value": 100000,
                "initial_margin": 0
            }
        }]))
        .unwrap();

        assert_eq!(holdings[0].trading_symbol, "AARON");
        assert_eq!(holdings[0].quantity, 1);
        assert_eq!(holdings[0].mtf.value, dec!(100000));
    }

    #[test]
    fn deserializes_documented_order_charges() {
        let charges: Vec<KiteOrderCharges> = serde_json::from_value(json!([
            {
                "transaction_type": "BUY",
                "tradingsymbol": "SBIN",
                "exchange": VENUE_NSE,
                "variety": "regular",
                "product": "CNC",
                "order_type": "MARKET",
                "quantity": 1,
                "price": 560,
                "charges": {
                    "transaction_tax": 0.56,
                    "transaction_tax_type": "stt",
                    "exchange_turnover_charge": 0.01876,
                    "sebi_turnover_charge": 0.00056,
                    "brokerage": 0,
                    "stamp_duty": 0,
                    "gst": {
                        "igst": 0.0033767999999999997,
                        "cgst": 0,
                        "sgst": 0,
                        "total": 0.0033767999999999997
                    },
                    "total": 0.5826968
                }
            },
            {
                "transaction_type": "SELL",
                "tradingsymbol": "GOLDPETAL23JULFUT",
                "exchange": "MCX",
                "variety": "regular",
                "product": "NRML",
                "order_type": "LIMIT",
                "quantity": 1,
                "price": 5862,
                "charges": {
                    "transaction_tax": 0.5862,
                    "transaction_tax_type": "ctt",
                    "exchange_turnover_charge": 0.152412,
                    "sebi_turnover_charge": 0.005862,
                    "brokerage": 1.7586,
                    "stamp_duty": 0,
                    "gst": {
                        "igst": 0.34503732,
                        "cgst": 0,
                        "sgst": 0,
                        "total": 0.34503732
                    },
                    "total": 2.84811132
                }
            },
            {
                "transaction_type": "BUY",
                "tradingsymbol": "NIFTY2371317900PE",
                "exchange": "NFO",
                "variety": "regular",
                "product": "NRML",
                "order_type": "LIMIT",
                "quantity": 100,
                "price": 1.5,
                "charges": {
                    "transaction_tax": 0,
                    "transaction_tax_type": "stt",
                    "exchange_turnover_charge": 0.07575,
                    "sebi_turnover_charge": 0.00015,
                    "brokerage": 20,
                    "stamp_duty": 0,
                    "gst": {
                        "igst": 3.613527,
                        "cgst": 0,
                        "sgst": 0,
                        "total": 3.613527
                    },
                    "total": 23.689427000000002
                }
            }
        ]))
        .unwrap();

        assert_eq!(charges.len(), 3);
        assert_eq!(charges[0].charges.total, dec!(0.5826968));
        assert_eq!(charges[1].charges.transaction_tax_type, "ctt");
        assert_eq!(charges[2].charges.brokerage, dec!(20));
    }

    #[test]
    fn deserializes_documented_order_margin() {
        let margins: Vec<KiteOrderMargin> = serde_json::from_value(json!([{
            "type": "equity",
            "tradingsymbol": "INFY",
            "exchange": VENUE_NSE,
            "span": 0,
            "exposure": 0,
            "option_premium": 0,
            "additional": 0,
            "bo": 0,
            "cash": 0,
            "var": 1498,
            "pnl": {
                "realised": 0,
                "unrealised": 0
            },
            "leverage": 1,
            "charges": {
                "transaction_tax": 1.498,
                "transaction_tax_type": "stt",
                "exchange_turnover_charge": 0.051681,
                "sebi_turnover_charge": 0.001498,
                "brokerage": 0.01,
                "stamp_duty": 0.22,
                "gst": {
                    "igst": 0.011372219999999999,
                    "cgst": 0,
                    "sgst": 0,
                    "total": 0.011372219999999999
                },
                "total": 1.79255122
            },
            "total": 1498
        }]))
        .unwrap();
        let margin = &margins[0];

        assert_eq!(margin.margin_type, "equity");
        assert_eq!(margin.trading_symbol, "INFY");
        assert_eq!(margin.var, dec!(1498));
        assert_eq!(margin.pnl.realised, dec!(0));
        assert_eq!(margin.charges.total, dec!(1.79255122));
        assert_eq!(margin.total, dec!(1498));
    }
}
