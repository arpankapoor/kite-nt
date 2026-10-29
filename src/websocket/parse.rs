use std::sync::Arc;

use bytes::{Buf, Bytes, TryGetError};
use jiff::Timestamp;
use nautilus_core::UnixNanos;
use nautilus_core::correctness::{CorrectnessError, CorrectnessResult};
use nautilus_model::data::{
    BookOrder, CustomData, Data, IndexPriceUpdate, OrderBookDepth, QuoteTick,
};
use nautilus_model::enums::OrderSide;
use nautilus_model::instruments::{IndexInstrument, Instrument, InstrumentAny};
use nautilus_network::RECONNECTED;
use rust_decimal::Decimal;
use tracing::{debug, warn};

use crate::common::parse::{instrument_price, instrument_quantity};
use crate::data_types::{KiteOpenInterest, supports_open_interest};
use crate::websocket::error::KiteWsError;
use crate::websocket::messages::{
    KiteDepthLevel, KiteFullQuote, KiteIndexFullQuote, KiteIndexQuote, KiteLtp, KiteMarketData,
    KiteQuote, KiteWsFrame, KiteWsFrameKind, KiteWsMessage, KiteWsTextMessage,
};

const LTP_PACKET_LENGTH: usize = 8;
const INDEX_QUOTE_PACKET_LENGTH: usize = 28;
const INDEX_FULL_PACKET_LENGTH: usize = 32;
const QUOTE_PACKET_LENGTH: usize = 44;
const FULL_PACKET_LENGTH: usize = 184;

const SEGMENT_MASK: u32 = 0xff;
const CDS_SEGMENT: u32 = 3;
const BCD_SEGMENT: u32 = 6;
const NCO_SEGMENT: u32 = 12;

/// Selects the normalized records produced from a full-mode market-data packet.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct KiteMarketDataSelection {
    pub(crate) quotes: bool,
    pub(crate) depth: bool,
    pub(crate) open_interest: bool,
}

impl KiteMarketDataSelection {
    /// Selects quotes, order-book depth, and open interest.
    pub(crate) const ALL: Self = Self {
        quotes: true,
        depth: true,
        open_interest: true,
    };

    pub(crate) const fn is_empty(self) -> bool {
        !self.quotes && !self.depth && !self.open_interest
    }
}

/// Decodes a raw Kite WebSocket frame into a typed message.
pub fn parse_frame(frame: &KiteWsFrame) -> Result<KiteWsMessage, KiteWsError> {
    match frame.kind {
        KiteWsFrameKind::Binary => {
            parse_binary_message(&frame.payload).map(KiteWsMessage::MarketData)
        }
        KiteWsFrameKind::Text if frame.payload.as_ref() == RECONNECTED.as_bytes() => {
            Ok(KiteWsMessage::Reconnected)
        }
        KiteWsFrameKind::Text => Ok(parse_text_message(frame.payload.clone())),
    }
}

/// Converts one decoded Kite market-data packet into all applicable Nautilus records.
pub fn normalize_market_data(
    data: &KiteMarketData,
    instrument: &InstrumentAny,
    ts_init: UnixNanos,
) -> CorrectnessResult<Vec<Data>> {
    normalize_selected_market_data(data, instrument, ts_init, KiteMarketDataSelection::ALL)
}

pub(crate) fn normalize_selected_market_data(
    data: &KiteMarketData,
    instrument: &InstrumentAny,
    ts_init: UnixNanos,
    selection: KiteMarketDataSelection,
) -> CorrectnessResult<Vec<Data>> {
    if let InstrumentAny::IndexInstrument(index) = instrument {
        return parse_index_price(data, index, ts_init)
            .map(|update| update.map(Data::IndexPrice).into_iter().collect());
    }

    let KiteMarketData::Full(quote) = data else {
        return Ok(Vec::new());
    };
    let mut updates = Vec::with_capacity(3);

    if selection.quotes
        && let Some(quote) = parse_quote_tick(quote, instrument, ts_init)?
    {
        updates.push(Data::Quote(quote));
    }
    if selection.depth {
        updates.push(Data::BookDepth(Box::new(parse_order_book_snapshot(
            quote, instrument, ts_init,
        )?)));
    }
    if selection.open_interest && supports_open_interest(instrument) {
        updates.push(parse_open_interest(quote, instrument, ts_init)?);
    }

    Ok(updates)
}

fn parse_open_interest(
    quote: &KiteFullQuote,
    instrument: &InstrumentAny,
    ts_init: UnixNanos,
) -> CorrectnessResult<Data> {
    let ts_event = quote
        .exchange_timestamp
        .map(UnixNanos::from)
        .unwrap_or(ts_init);
    let instrument_id = instrument.id();
    let open_interest = KiteOpenInterest::new(
        instrument_id,
        u64::from(quote.open_interest),
        u64::from(quote.open_interest_day_high),
        u64::from(quote.open_interest_day_low),
        ts_event,
        ts_init,
    );
    let data_type = KiteOpenInterest::data_type(instrument_id);

    Ok(Data::Custom(CustomData::new(
        Arc::new(open_interest),
        data_type,
    )))
}

fn parse_binary_message(bytes: &[u8]) -> Result<Vec<KiteMarketData>, KiteWsError> {
    if bytes.len() < 2 {
        return Ok(Vec::new());
    }

    let mut cursor = bytes;
    let packet_count = usize::from(read_u16(&mut cursor)?);
    let mut messages = Vec::with_capacity(packet_count);

    for _ in 0..packet_count {
        let packet_length = usize::from(read_u16(&mut cursor)?);
        messages.push(parse_packet(take(&mut cursor, packet_length)?)?);
    }

    if !cursor.is_empty() {
        return Err(KiteWsError::TrailingBytes(cursor.len()));
    }

    Ok(messages)
}

fn parse_text_message(payload: Bytes) -> KiteWsMessage {
    match serde_json::from_slice(&payload) {
        Ok(KiteWsTextMessage::Order(update)) => KiteWsMessage::OrderUpdate(update),
        Ok(KiteWsTextMessage::Error(message)) => KiteWsMessage::Error(message),
        Ok(KiteWsTextMessage::Message(message)) => KiteWsMessage::Message(message),
        Ok(KiteWsTextMessage::InstrumentsMeta { etag, count }) => {
            KiteWsMessage::InstrumentsMeta { etag, count }
        }
        Err(error) => {
            warn!(
                %error,
                payload_length = payload.len(),
                "Preserving unrecognized Kite WebSocket text message"
            );
            debug!(
                payload = %String::from_utf8_lossy(&payload),
                "Unrecognized Kite WebSocket text message payload"
            );
            KiteWsMessage::RawText(payload)
        }
    }
}

fn parse_index_price(
    data: &KiteMarketData,
    instrument: &IndexInstrument,
    ts_init: UnixNanos,
) -> CorrectnessResult<Option<IndexPriceUpdate>> {
    let (last_price, exchange_timestamp) = match data {
        KiteMarketData::Ltp(ltp) => (ltp.last_price, None),
        KiteMarketData::IndexQuote(quote) => (quote.last_price, None),
        KiteMarketData::IndexFull(full) => {
            (full.quote.last_price, full.exchange_timestamp.as_ref())
        }
        KiteMarketData::Quote(_) | KiteMarketData::Full(_) => return Ok(None),
    };
    let value = instrument_price(last_price, instrument)?;
    let ts_event = exchange_timestamp
        .copied()
        .map(UnixNanos::from)
        .unwrap_or(ts_init);

    Ok(Some(IndexPriceUpdate::new(
        instrument.id(),
        value,
        ts_event,
        ts_init,
    )))
}

fn parse_quote_tick(
    quote: &KiteFullQuote,
    instrument: &InstrumentAny,
    ts_init: UnixNanos,
) -> CorrectnessResult<Option<QuoteTick>> {
    let Some(bid) = quote.bids.iter().find(|level| level.quantity != 0) else {
        return Ok(None);
    };
    let Some(offer) = quote.offers.iter().find(|level| level.quantity != 0) else {
        return Ok(None);
    };
    let bid_price = instrument_price(bid.price, instrument)?;
    let ask_price = instrument_price(offer.price, instrument)?;
    let bid_size = instrument_quantity(Decimal::from(bid.quantity), instrument)?;
    let ask_size = instrument_quantity(Decimal::from(offer.quantity), instrument)?;
    let ts_event = quote
        .exchange_timestamp
        .map(UnixNanos::from)
        .unwrap_or(ts_init);

    QuoteTick::new_checked(
        instrument.id(),
        bid_price,
        ask_price,
        bid_size,
        ask_size,
        ts_event,
        ts_init,
    )
    .map(Some)
    .map_err(|error| CorrectnessError::PredicateViolation {
        message: format!("invalid Kite quote: {error}"),
    })
}

fn parse_order_book_snapshot(
    quote: &KiteFullQuote,
    instrument: &InstrumentAny,
    ts_init: UnixNanos,
) -> CorrectnessResult<OrderBookDepth> {
    let (bids, bid_counts) = parse_book_side(&quote.bids, OrderSide::Buy, instrument)?;
    let (asks, ask_counts) = parse_book_side(&quote.offers, OrderSide::Sell, instrument)?;
    let ts_event = quote
        .exchange_timestamp
        .map(UnixNanos::from)
        .unwrap_or(ts_init);

    OrderBookDepth::new_checked(
        instrument.id(),
        bids,
        asks,
        bid_counts,
        ask_counts,
        0,
        0,
        ts_event,
        ts_init,
    )
    .map_err(|error| CorrectnessError::PredicateViolation {
        message: format!("invalid Kite order book depth: {error}"),
    })
}

fn parse_packet(packet: &[u8]) -> Result<KiteMarketData, KiteWsError> {
    let mut cursor = packet;
    let instrument_token = read_u32(&mut cursor)?;
    let divisor = price_divisor(instrument_token);

    match packet.len() {
        LTP_PACKET_LENGTH => Ok(KiteMarketData::Ltp(KiteLtp {
            instrument_token,
            last_price: read_price(&mut cursor, divisor)?,
        })),
        INDEX_QUOTE_PACKET_LENGTH => parse_index_quote(&mut cursor, instrument_token, divisor)
            .map(KiteMarketData::IndexQuote),
        INDEX_FULL_PACKET_LENGTH => {
            let quote = parse_index_quote(&mut cursor, instrument_token, divisor)?;
            let exchange_timestamp = read_timestamp(&mut cursor, "exchange")?;
            Ok(KiteMarketData::IndexFull(KiteIndexFullQuote {
                quote,
                exchange_timestamp,
            }))
        }
        QUOTE_PACKET_LENGTH => {
            parse_quote(&mut cursor, instrument_token, divisor).map(KiteMarketData::Quote)
        }
        FULL_PACKET_LENGTH => {
            let quote = parse_quote(&mut cursor, instrument_token, divisor)?;
            let last_trade_timestamp = read_timestamp(&mut cursor, "last trade")?;
            let open_interest = read_u32(&mut cursor)?;
            let open_interest_day_high = read_u32(&mut cursor)?;
            let open_interest_day_low = read_u32(&mut cursor)?;
            let exchange_timestamp = read_timestamp(&mut cursor, "exchange")?;
            let bids = read_depth_levels(&mut cursor, divisor)?;
            let offers = read_depth_levels(&mut cursor, divisor)?;

            Ok(KiteMarketData::Full(KiteFullQuote {
                quote,
                last_trade_timestamp,
                open_interest,
                open_interest_day_high,
                open_interest_day_low,
                exchange_timestamp,
                bids,
                offers,
            }))
        }
        length => Err(KiteWsError::InvalidPacketLength(length)),
    }
}

fn parse_quote(
    cursor: &mut &[u8],
    instrument_token: u32,
    divisor: i64,
) -> Result<KiteQuote, KiteWsError> {
    Ok(KiteQuote {
        instrument_token,
        last_price: read_price(cursor, divisor)?,
        last_quantity: read_u32(cursor)?,
        average_price: read_price(cursor, divisor)?,
        volume: read_u32(cursor)?,
        total_buy_quantity: read_u32(cursor)?,
        total_sell_quantity: read_u32(cursor)?,
        open: read_price(cursor, divisor)?,
        high: read_price(cursor, divisor)?,
        low: read_price(cursor, divisor)?,
        close: read_price(cursor, divisor)?,
    })
}

fn parse_index_quote(
    cursor: &mut &[u8],
    instrument_token: u32,
    divisor: i64,
) -> Result<KiteIndexQuote, KiteWsError> {
    Ok(KiteIndexQuote {
        instrument_token,
        last_price: read_price(cursor, divisor)?,
        high: read_price(cursor, divisor)?,
        low: read_price(cursor, divisor)?,
        open: read_price(cursor, divisor)?,
        close: read_price(cursor, divisor)?,
        price_change: read_price(cursor, divisor)?,
    })
}

fn read_depth_levels(cursor: &mut &[u8], divisor: i64) -> Result<[KiteDepthLevel; 5], KiteWsError> {
    Ok([
        read_depth_level(cursor, divisor)?,
        read_depth_level(cursor, divisor)?,
        read_depth_level(cursor, divisor)?,
        read_depth_level(cursor, divisor)?,
        read_depth_level(cursor, divisor)?,
    ])
}

fn read_depth_level(cursor: &mut &[u8], divisor: i64) -> Result<KiteDepthLevel, KiteWsError> {
    let level = KiteDepthLevel {
        quantity: read_u32(cursor)?,
        price: read_price(cursor, divisor)?,
        orders: read_u16(cursor)?,
    };
    take(cursor, 2)?;
    Ok(level)
}

/// Returns the price divisor encoded by Kite's exchange segment byte.
///
/// See the official PyKiteConnect implementation:
/// <https://github.com/zerodha/pykiteconnect/blob/f5531bd2ed3f64e00407eead2a465225f455bef3/kiteconnect/ticker.py#L726-L734>.
const fn price_divisor(instrument_token: u32) -> i64 {
    match instrument_token & SEGMENT_MASK {
        CDS_SEGMENT => 10_000_000,
        BCD_SEGMENT | NCO_SEGMENT => 10_000,
        _ => 100,
    }
}

fn read_u16(buffer: &mut &[u8]) -> Result<u16, KiteWsError> {
    buffer.try_get_u16().map_err(incomplete_frame)
}

fn read_u32(buffer: &mut &[u8]) -> Result<u32, KiteWsError> {
    buffer.try_get_u32().map_err(incomplete_frame)
}

fn read_price(buffer: &mut &[u8], divisor: i64) -> Result<Decimal, KiteWsError> {
    Ok(Decimal::from(read_u32(buffer)?) / Decimal::from(divisor))
}

fn read_timestamp(
    buffer: &mut &[u8],
    field: &'static str,
) -> Result<Option<Timestamp>, KiteWsError> {
    let value = read_u32(buffer)?;
    if value == 0 {
        return Ok(None);
    }

    Timestamp::from_second(i64::from(value))
        .map(Some)
        .map_err(|_| KiteWsError::InvalidTimestamp { field, value })
}

fn parse_book_side(
    levels: &[KiteDepthLevel; 5],
    side: OrderSide,
    instrument: &InstrumentAny,
) -> CorrectnessResult<(Vec<BookOrder>, Vec<u32>)> {
    let mut orders = Vec::with_capacity(levels.len());
    let mut counts = Vec::with_capacity(levels.len());

    for level in levels.iter().filter(|level| level.quantity != 0) {
        orders.push(BookOrder::new(
            side,
            instrument_price(level.price, instrument)?,
            instrument_quantity(Decimal::from(level.quantity), instrument)?,
            0,
        ));
        counts.push(u32::from(level.orders));
    }

    Ok((orders, counts))
}

fn take<'a>(buffer: &mut &'a [u8], length: usize) -> Result<&'a [u8], KiteWsError> {
    let remaining = buffer.len();
    let (taken, rest) = buffer
        .split_at_checked(length)
        .ok_or(KiteWsError::IncompleteFrame {
            expected: length,
            remaining,
        })?;
    *buffer = rest;
    Ok(taken)
}

const fn incomplete_frame(error: TryGetError) -> KiteWsError {
    KiteWsError::IncompleteFrame {
        expected: error.requested,
        remaining: error.available,
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use nautilus_model::data::Data;
    use nautilus_model::instruments::InstrumentAny;
    use nautilus_model::instruments::stubs::{equity_aapl, futures_contract_es};
    use rust_decimal::Decimal;

    use crate::data_types::KiteOpenInterest;
    use crate::websocket::messages::{
        KiteDepthLevel, KiteFullQuote, KiteMarketData, KiteQuote, KiteWsMessage,
    };
    use crate::websocket::parse::{normalize_market_data, parse_text_message};

    #[test]
    fn preserves_unrecognized_text_messages() {
        let payload = Bytes::from_static(br#"{"type":"new_type","data":{}}"#);

        assert_eq!(
            parse_text_message(payload.clone()),
            KiteWsMessage::RawText(payload),
        );
    }

    #[test]
    fn normalizes_only_populated_depth_without_equity_open_interest() {
        let instrument = InstrumentAny::Equity(equity_aapl());
        let mut data = full_quote();
        let KiteMarketData::Full(quote) = &mut data else {
            unreachable!();
        };
        quote.bids[0] = KiteDepthLevel {
            quantity: 10,
            price: Decimal::new(100, 0),
            orders: 2,
        };
        quote.offers[0] = KiteDepthLevel {
            quantity: 11,
            price: Decimal::new(101, 0),
            orders: 3,
        };

        let normalized = normalize_market_data(&data, &instrument, 42.into()).unwrap();
        assert_eq!(normalized.len(), 2);
        let depth = normalized
            .iter()
            .find_map(|data| match data {
                Data::BookDepth(depth) => Some(depth),
                _ => None,
            })
            .expect("expected normalized depth");

        assert_eq!(depth.bids.len(), 1);
        assert_eq!(depth.asks.len(), 1);
        assert_eq!(depth.bid_counts.as_slice(), [2]);
        assert_eq!(depth.ask_counts.as_slice(), [3]);
    }

    #[test]
    fn normalizes_derivative_open_interest() {
        let instrument = InstrumentAny::FuturesContract(futures_contract_es(None, None));
        let normalized = normalize_market_data(&full_quote(), &instrument, 42.into()).unwrap();

        let Data::Custom(custom) = &normalized[1] else {
            panic!("expected normalized open interest");
        };
        let open_interest = custom
            .data
            .as_any()
            .downcast_ref::<KiteOpenInterest>()
            .expect("custom data should contain Kite open interest");
        assert_eq!(open_interest.open_interest, 123);
        assert_eq!(open_interest.day_high, 150);
        assert_eq!(open_interest.day_low, 100);
        assert_eq!(open_interest.ts_event, 42);
    }

    fn full_quote() -> KiteMarketData {
        let empty_levels = || {
            std::array::from_fn(|_| KiteDepthLevel {
                quantity: 0,
                price: Decimal::ZERO,
                orders: 0,
            })
        };
        KiteMarketData::Full(KiteFullQuote {
            quote: KiteQuote {
                instrument_token: 1,
                last_price: Decimal::ZERO,
                last_quantity: 0,
                average_price: Decimal::ZERO,
                volume: 0,
                total_buy_quantity: 0,
                total_sell_quantity: 0,
                open: Decimal::ZERO,
                high: Decimal::ZERO,
                low: Decimal::ZERO,
                close: Decimal::ZERO,
            },
            last_trade_timestamp: None,
            open_interest: 123,
            open_interest_day_high: 150,
            open_interest_day_low: 100,
            exchange_timestamp: None,
            bids: empty_levels(),
            offers: empty_levels(),
        })
    }
}
