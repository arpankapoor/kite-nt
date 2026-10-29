use std::collections::HashSet;

use jiff::civil::{Date, Time};
use nautilus_core::UnixNanos;
use nautilus_core::correctness::{CorrectnessError, CorrectnessResult};
use nautilus_core::params::Params;
use nautilus_core::time::get_atomic_clock_realtime;
use nautilus_model::data::{Bar, BarType};
use nautilus_model::enums::{AssetClass, BarAggregation, OptionKind, PriceType};
use nautilus_model::instruments::{
    Equity, FuturesContract, IndexInstrument, Instrument, InstrumentAny, OptionContract,
};
use nautilus_model::types::{Currency, Price, Quantity};
use rust_decimal::{Decimal, dec};

use crate::common::consts::{INDIA_TIME_ZONE, INSTRUMENT_TOKEN_INFO_KEY, VENUE_BSE};
use crate::common::parse::{instrument_price, instrument_quantity, parse_instrument_id};
use crate::data_types::{KiteHistoricalData, KiteHistoricalOpenInterest, supports_open_interest};
use crate::http::models::{
    KiteCandleInterval, KiteHistoricalCandle, KiteInstrument, KiteInstrumentCsv, KiteInstrumentType,
};
use crate::instruments::{INDEX_DERIVATIVE_ALIASES, KiteInstrumentFilter, underlying_venue};

const INDICES_SEGMENT: &str = "INDICES";
const KITE_PRICE_PRECISION: u8 = 2;

const KNOWN_INDEX_FUTURE_UNDERLYINGS: &[(&str, &str)] = &[(VENUE_BSE, "FOCIT")];

/// Parses a Kite instrument-master CSV and optionally retains matching records.
pub fn parse_instruments_csv(
    csv: &KiteInstrumentCsv,
    filter: Option<&KiteInstrumentFilter<'_>>,
) -> Result<Vec<InstrumentAny>, csv::Error> {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(csv.as_bytes());
    let mut context = DerivativeClassificationContext::default();
    let mut kite_instruments = Vec::new();

    let records = reader.deserialize::<KiteInstrument>();
    for record in records {
        let instrument = record?;
        context.add_underlying_instrument(&instrument);
        if filter.is_none_or(|filter| filter(&instrument)) {
            kite_instruments.push(instrument);
        }
    }
    context.add_index_underlying_aliases();

    let ts_init = get_atomic_clock_realtime().get_time_ns();
    let mut instruments = Vec::with_capacity(kite_instruments.len());
    let mut skipped = 0;

    for instrument in &kite_instruments {
        match parse_instrument(instrument, &context, ts_init) {
            Ok(instrument) => instruments.push(instrument),
            Err(error) => {
                skipped += 1;
                tracing::warn!(
                    instrument_token = instrument.instrument_token,
                    trading_symbol = %instrument.trading_symbol,
                    exchange = %instrument.exchange,
                    segment = %instrument.segment,
                    instrument_type = ?instrument.instrument_type,
                    error = %error,
                    "Skipping Kite instrument",
                );
            }
        }
    }

    if skipped > 0 {
        tracing::warn!(
            loaded = instruments.len(),
            skipped,
            "Completed Kite instrument conversion with skipped instruments",
        );
    }

    Ok(instruments)
}

pub(crate) fn parse_historical_candles(
    candles: Vec<KiteHistoricalCandle>,
    instrument: &InstrumentAny,
    bar_type: BarType,
    ts_init: UnixNanos,
) -> KiteHistoricalData {
    let mut bars = Vec::with_capacity(candles.len());
    let mut open_interest = Vec::with_capacity(candles.len());
    let include_open_interest = supports_open_interest(instrument);
    for candle in candles {
        let timestamp = UnixNanos::from(candle.timestamp);

        if include_open_interest && let Some(value) = candle.open_interest {
            open_interest.push(KiteHistoricalOpenInterest::new(
                instrument.id(),
                bar_type,
                value,
                timestamp,
                ts_init,
            ));
        }

        match parse_historical_candle(&candle, instrument, bar_type, timestamp, ts_init) {
            Ok(bar) => bars.push(bar),
            Err(error) => {
                tracing::warn!(%error, instrument_id = %instrument.id(), %bar_type, "Skipping invalid Kite historical candle");
            }
        }
    }
    KiteHistoricalData {
        bars,
        open_interest,
    }
}

fn parse_historical_candle(
    candle: &KiteHistoricalCandle,
    instrument: &InstrumentAny,
    bar_type: BarType,
    timestamp: UnixNanos,
    ts_init: UnixNanos,
) -> CorrectnessResult<Bar> {
    let open = instrument_price(candle.open, instrument)?;
    let high = instrument_price(candle.high, instrument)?;
    let low = instrument_price(candle.low, instrument)?;
    let close = instrument_price(candle.close, instrument)?;
    let volume = instrument_quantity(Decimal::from(candle.volume), instrument)?;
    Bar::new_checked(bar_type, open, high, low, close, volume, timestamp, ts_init).map_err(
        |error| CorrectnessError::PredicateViolation {
            message: format!(
                "invalid historical candle at {}: open={}, high={}, low={}, close={}: {error}",
                candle.timestamp, candle.open, candle.high, candle.low, candle.close,
            ),
        },
    )
}

pub(crate) fn candle_interval(bar_type: BarType) -> CorrectnessResult<KiteCandleInterval> {
    if !bar_type.is_standard()
        || !bar_type.is_externally_aggregated()
        || bar_type.spec().price_type != PriceType::Last
    {
        return Err(CorrectnessError::PredicateViolation {
            message: format!("unsupported Kite bar type {bar_type}"),
        });
    }

    let specification = bar_type.spec();
    match (specification.step.get(), specification.aggregation) {
        (1, BarAggregation::Minute) => Ok(KiteCandleInterval::Minute),
        (3, BarAggregation::Minute) => Ok(KiteCandleInterval::ThreeMinutes),
        (5, BarAggregation::Minute) => Ok(KiteCandleInterval::FiveMinutes),
        (10, BarAggregation::Minute) => Ok(KiteCandleInterval::TenMinutes),
        (15, BarAggregation::Minute) => Ok(KiteCandleInterval::FifteenMinutes),
        (30, BarAggregation::Minute) => Ok(KiteCandleInterval::ThirtyMinutes),
        (1, BarAggregation::Hour) => Ok(KiteCandleInterval::SixtyMinutes),
        (1, BarAggregation::Day) => Ok(KiteCandleInterval::Day),
        _ => Err(CorrectnessError::PredicateViolation {
            message: format!("unsupported Kite bar type {bar_type}"),
        }),
    }
}

/// Classifies derivatives from the cash and index instruments in Kite's CSV.
/// An equity future matches its cash instrument, while the `NIFTY` futures alias matches the
/// `NIFTY 50.NSE` index.
#[derive(Default)]
struct DerivativeClassificationContext {
    /// Cash equity underlyings stored as `(exchange, trading symbol)`.
    equity_underlyings: HashSet<(String, String)>,
    /// Index underlyings stored as `(exchange, symbol)`, including futures aliases.
    index_underlyings: HashSet<(String, String)>,
}

impl DerivativeClassificationContext {
    #[cfg(test)]
    fn from_instruments(instruments: &[KiteInstrument]) -> Self {
        let mut context = Self::default();
        for instrument in instruments {
            context.add_underlying_instrument(instrument);
        }
        context.add_index_underlying_aliases();
        context
    }

    fn add_underlying_instrument(&mut self, instrument: &KiteInstrument) {
        if instrument.segment == INDICES_SEGMENT {
            self.index_underlyings.insert((
                instrument.exchange.clone(),
                instrument.trading_symbol.clone(),
            ));
        } else if instrument.instrument_type == KiteInstrumentType::Equity {
            self.equity_underlyings.insert((
                instrument.exchange.clone(),
                instrument.trading_symbol.clone(),
            ));
        }
    }

    fn add_index_underlying_aliases(&mut self) {
        for &(underlying_venue, derivative_underlying, index_symbol) in INDEX_DERIVATIVE_ALIASES {
            let index_key = (underlying_venue.to_owned(), index_symbol.to_owned());

            if self.index_underlyings.contains(&index_key) {
                self.index_underlyings.insert((
                    underlying_venue.to_owned(),
                    derivative_underlying.to_owned(),
                ));
            }
        }

        for &(underlying_venue, future_underlying) in KNOWN_INDEX_FUTURE_UNDERLYINGS {
            self.index_underlyings
                .insert((underlying_venue.to_owned(), future_underlying.to_owned()));
        }
    }

    fn derivative_asset_class(&self, instrument: &KiteInstrument) -> CorrectnessResult<AssetClass> {
        if !matches!(
            instrument.instrument_type,
            KiteInstrumentType::Future
                | KiteInstrumentType::CallOption
                | KiteInstrumentType::PutOption
        ) {
            return Err(CorrectnessError::PredicateViolation {
                message: format!(
                    "expected a derivative instrument, received {:?}",
                    instrument.instrument_type
                ),
            });
        }

        let underlying_venue = underlying_venue(&instrument.exchange).ok_or_else(|| {
            CorrectnessError::PredicateViolation {
                message: format!("unsupported futures exchange {}", instrument.exchange),
            }
        })?;
        let underlying = derivative_underlying(instrument)?;
        let key = (underlying_venue.to_owned(), underlying.to_owned());

        if self.index_underlyings.contains(&key) {
            return Ok(AssetClass::Index);
        }
        if instrument.exchange == "CDS" {
            if parse_currency_pair(underlying).is_some() {
                return Ok(AssetClass::FX);
            }
            if is_debt_underlying(underlying) {
                return Ok(AssetClass::Debt);
            }

            return Err(CorrectnessError::PredicateViolation {
                message: format!("could not classify CDS derivative underlying {underlying}"),
            });
        }
        if matches!(instrument.exchange.as_str(), "MCX" | "NCO") {
            return Ok(AssetClass::Commodity);
        }
        if self.equity_underlyings.contains(&key) {
            return Ok(AssetClass::Equity);
        }

        Err(CorrectnessError::PredicateViolation {
            message: format!(
                "could not classify derivative underlying {underlying}.{underlying_venue}"
            ),
        })
    }
}

fn parse_instrument(
    instrument: &KiteInstrument,
    context: &DerivativeClassificationContext,
    ts_init: UnixNanos,
) -> CorrectnessResult<InstrumentAny> {
    if instrument.segment == INDICES_SEGMENT {
        return parse_index_instrument(instrument, ts_init).map(InstrumentAny::IndexInstrument);
    }

    match instrument.instrument_type {
        KiteInstrumentType::Equity => {
            parse_equity_instrument(instrument, ts_init).map(InstrumentAny::Equity)
        }
        KiteInstrumentType::Future => parse_futures_instrument(instrument, context, ts_init)
            .map(InstrumentAny::FuturesContract),
        KiteInstrumentType::CallOption | KiteInstrumentType::PutOption => {
            parse_option_instrument(instrument, context, ts_init).map(InstrumentAny::OptionContract)
        }
    }
}

fn parse_index_instrument(
    instrument: &KiteInstrument,
    ts_init: UnixNanos,
) -> CorrectnessResult<IndexInstrument> {
    if instrument.segment != INDICES_SEGMENT {
        return Err(CorrectnessError::PredicateViolation {
            message: format!(
                "expected an index instrument, received segment {}",
                instrument.segment
            ),
        });
    }

    let instrument_id = parse_instrument_id(&instrument.exchange, &instrument.trading_symbol)?;
    let raw_symbol = instrument_id.symbol;
    // Kite reports zero tick and lot sizes for non-tradable spot indices. Nautilus requires
    // positive increments, so retain two decimal places for prices and whole-number sizes.
    let price_increment = Price::from_decimal_dp(dec!(0.01), KITE_PRICE_PRECISION)?;
    let size_increment = Quantity::from(1);

    IndexInstrument::build_checked(
        instrument_id,
        raw_symbol,
        Currency::INR(),
        price_increment.precision,
        size_increment.precision,
        price_increment,
        size_increment,
        None,
        Some(instrument_info(instrument)),
        ts_init,
        ts_init,
    )
}

fn parse_futures_instrument(
    instrument: &KiteInstrument,
    context: &DerivativeClassificationContext,
    ts_init: UnixNanos,
) -> CorrectnessResult<FuturesContract> {
    if instrument.instrument_type != KiteInstrumentType::Future {
        return Err(CorrectnessError::PredicateViolation {
            message: format!(
                "expected a futures instrument, received {:?}",
                instrument.instrument_type
            ),
        });
    }

    let asset_class = context.derivative_asset_class(instrument)?;
    let underlying = derivative_underlying(instrument)?;
    let expiry = instrument
        .expiry
        .ok_or_else(|| CorrectnessError::PredicateViolation {
            message: "futures instrument has no expiry date".to_owned(),
        })?;
    let instrument_id = parse_instrument_id(&instrument.exchange, &instrument.trading_symbol)?;
    let raw_symbol = instrument_id.symbol;
    let price_increment = parse_price_increment(instrument.tick_size)?;
    let lot_size = Quantity::from_decimal(Decimal::from(instrument.lot_size))?;
    let currency = derivative_currency(asset_class, underlying)?;
    let expiration_ns = derivative_expiration_ns(expiry)?;

    FuturesContract::build_checked(
        instrument_id,
        raw_symbol,
        asset_class,
        None,
        underlying.into(),
        UnixNanos::default(),
        expiration_ns,
        currency,
        price_increment.precision,
        price_increment,
        Quantity::from(1),
        lot_size,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(instrument_info(instrument)),
        ts_init,
        ts_init,
    )
}

fn parse_option_instrument(
    instrument: &KiteInstrument,
    context: &DerivativeClassificationContext,
    ts_init: UnixNanos,
) -> CorrectnessResult<OptionContract> {
    let option_kind = match instrument.instrument_type {
        KiteInstrumentType::CallOption => OptionKind::Call,
        KiteInstrumentType::PutOption => OptionKind::Put,
        instrument_type => {
            return Err(CorrectnessError::PredicateViolation {
                message: format!("expected an option instrument, received {instrument_type:?}"),
            });
        }
    };
    let asset_class = context.derivative_asset_class(instrument)?;
    let underlying = derivative_underlying(instrument)?;
    let expiry = instrument
        .expiry
        .ok_or_else(|| CorrectnessError::PredicateViolation {
            message: "option instrument has no expiry date".to_owned(),
        })?;
    let strike = instrument
        .strike
        .ok_or_else(|| CorrectnessError::PredicateViolation {
            message: "option instrument has no strike price".to_owned(),
        })?;
    let instrument_id = parse_instrument_id(&instrument.exchange, &instrument.trading_symbol)?;
    let raw_symbol = instrument_id.symbol;
    let strike_price = Price::from_decimal(strike)?;
    let price_increment = parse_price_increment(instrument.tick_size)?;
    let lot_size = Quantity::from_decimal(Decimal::from(instrument.lot_size))?;
    let currency = derivative_currency(asset_class, underlying)?;
    let expiration_ns = derivative_expiration_ns(expiry)?;

    OptionContract::build_checked(
        instrument_id,
        raw_symbol,
        asset_class,
        None,
        underlying.into(),
        option_kind,
        strike_price,
        currency,
        UnixNanos::default(),
        expiration_ns,
        price_increment.precision,
        price_increment,
        Quantity::from(1),
        lot_size,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(instrument_info(instrument)),
        ts_init,
        ts_init,
    )
}

fn parse_equity_instrument(
    instrument: &KiteInstrument,
    ts_init: UnixNanos,
) -> CorrectnessResult<Equity> {
    if instrument.instrument_type != KiteInstrumentType::Equity {
        return Err(CorrectnessError::PredicateViolation {
            message: format!(
                "expected an equity instrument, received {:?}",
                instrument.instrument_type
            ),
        });
    }
    if instrument.segment == INDICES_SEGMENT {
        return Err(CorrectnessError::PredicateViolation {
            message: "index record cannot be parsed as an equity".to_owned(),
        });
    }

    let instrument_id = parse_instrument_id(&instrument.exchange, &instrument.trading_symbol)?;
    let raw_symbol = instrument_id.symbol;
    let price_increment = parse_price_increment(instrument.tick_size)?;
    let lot_size = Quantity::from_decimal(Decimal::from(instrument.lot_size))?;

    Equity::build_checked(
        instrument_id,
        raw_symbol,
        None,
        Currency::INR(),
        price_increment.precision,
        price_increment,
        Some(lot_size),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(instrument_info(instrument)),
        ts_init,
        ts_init,
    )
}

fn instrument_info(instrument: &KiteInstrument) -> Params {
    Params::new(
        [(
            INSTRUMENT_TOKEN_INFO_KEY.to_owned(),
            serde_json::Value::from(instrument.instrument_token),
        )]
        .into(),
    )
}

fn derivative_underlying(instrument: &KiteInstrument) -> CorrectnessResult<&str> {
    instrument
        .name
        .as_deref()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| CorrectnessError::PredicateViolation {
            message: "derivative instrument has no underlying name".to_owned(),
        })
}

fn derivative_currency(asset_class: AssetClass, underlying: &str) -> CorrectnessResult<Currency> {
    if asset_class != AssetClass::FX {
        return Ok(Currency::INR());
    }

    parse_currency_pair(underlying)
        .map(|(_, quote_currency)| quote_currency)
        .ok_or_else(|| CorrectnessError::PredicateViolation {
            message: format!("invalid currency pair {underlying}"),
        })
}

fn derivative_expiration_ns(expiry: Date) -> CorrectnessResult<UnixNanos> {
    // Kite supplies only the expiry date, so use its final instant in India to keep the contract
    // valid throughout its stated expiry day.
    let expiration = expiry
        .to_datetime(Time::MAX)
        .to_zoned(INDIA_TIME_ZONE.clone())
        .map_err(|error| CorrectnessError::PredicateViolation {
            message: format!("invalid derivative expiry {expiry}: {error}"),
        })?;
    Ok(UnixNanos::from(expiration.timestamp()))
}

fn parse_price_increment(tick_size: Decimal) -> CorrectnessResult<Price> {
    Price::from_decimal_dp(tick_size, KITE_PRICE_PRECISION.max(tick_size.scale() as u8))
}

fn parse_currency_pair(symbol: &str) -> Option<(Currency, Currency)> {
    if symbol.len() != 6 || !symbol.is_ascii() {
        return None;
    }

    let (base_currency, quote_currency) = symbol.split_at_checked(3)?;

    Some((base_currency.parse().ok()?, quote_currency.parse().ok()?))
}

fn is_debt_underlying(symbol: &str) -> bool {
    // Overnight Mumbai Interbank Offered Rate futures.
    if symbol == "ONMIBOR" {
        return true;
    }
    // Treasury bills are prefixed by their tenor in days, for example `91DTB`.
    if let Some(prefix) = symbol.strip_suffix("DTB") {
        return !prefix.is_empty() && prefix.chars().all(|character| character.is_ascii_digit());
    }
    // Government securities combine a coupon without its decimal point and a maturity year,
    // for example `633GS2035` represents the 6.33% government security maturing in 2035.
    if let Some((coupon, maturity)) = symbol.split_once("GS") {
        return !coupon.is_empty()
            && !maturity.is_empty()
            && coupon.chars().all(|character| character.is_ascii_digit())
            && maturity.chars().all(|character| character.is_ascii_digit());
    }

    false
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use jiff::Timestamp;
    use jiff::civil::Date;
    use nautilus_model::data::BarType;
    use nautilus_model::data::bar::BAR_SPEC_1_MINUTE_LAST;
    use nautilus_model::enums::{AggregationSource, AssetClass, OptionKind};
    use nautilus_model::instruments::stubs::futures_contract_es;
    use nautilus_model::instruments::{Instrument, InstrumentAny};
    use nautilus_model::types::{Currency, Price, Quantity};
    use rust_decimal::{Decimal, dec};

    use crate::common::consts::{VENUE_BSE, VENUE_NSE};
    use crate::http::models::{
        KiteHistoricalCandle, KiteInstrument, KiteInstrumentCsv, KiteInstrumentType,
    };
    use crate::http::parse::{
        DerivativeClassificationContext, parse_equity_instrument, parse_futures_instrument,
        parse_historical_candle, parse_historical_candles, parse_instrument, parse_instruments_csv,
        parse_option_instrument,
    };
    use crate::instruments::instrument_token;

    const INSTRUMENTS_CSV: &[u8] = b"\
instrument_token,exchange_token,tradingsymbol,name,last_price,expiry,strike,tick_size,lot_size,instrument_type,segment,exchange
408065,1594,INFY,INFOSYS,0,,,0.05,1,EQ,NSE,NSE
5720322,22345,NIFTY15DECFUT,,78.0,2015-12-31,,0.05,75,FUT,NFO-FUT,NFO
5720578,22346,NIFTY159500CE,,23.0,2015-12-31,9500,0.05,75,CE,NFO-OPT,NFO
";

    const EQUITY_FUTURE_CSV: &[u8] = b"\
instrument_token,exchange_token,tradingsymbol,name,last_price,expiry,strike,tick_size,lot_size,instrument_type,segment,exchange
408065,1594,INFY,INFOSYS,0,,,0.05,1,EQ,NSE,NSE
264713,1034,INFY26OCTFUT,INFY,0,2026-10-29,,0.05,400,FUT,NFO-FUT,NFO
";

    #[test]
    fn parses_documented_instrument_csv() {
        let csv = KiteInstrumentCsv::new(Bytes::from_static(INSTRUMENTS_CSV));

        let instruments = parse_instruments_csv(&csv, None).unwrap();
        let infy = instruments
            .iter()
            .find(|instrument| instrument.raw_symbol().as_str() == "INFY")
            .unwrap();

        assert!(matches!(infy, InstrumentAny::Equity(_)));
        assert_eq!(instrument_token(infy).unwrap(), 408_065);
    }

    #[test]
    fn normalizes_kite_price_precision() {
        let mut record = instrument(
            "HINDALCO",
            Some("HINDALCO INDUSTRIES"),
            VENUE_NSE,
            VENUE_NSE,
            KiteInstrumentType::Equity,
        );
        record.tick_size = dec!(0.1);

        let instrument = parse_equity_instrument(&record, 42.into()).unwrap();

        assert_eq!(instrument.price_increment(), Price::from("0.10"));
        assert_eq!(instrument.price_precision(), 2);
    }

    #[test]
    fn parses_only_selected_instruments() {
        let csv = KiteInstrumentCsv::new(Bytes::from_static(INSTRUMENTS_CSV));
        let filter = |instrument: &KiteInstrument| instrument.trading_symbol == "INFY";

        let instruments = parse_instruments_csv(&csv, Some(&filter)).unwrap();
        let [instrument] = instruments.as_slice() else {
            panic!("expected one selected instrument");
        };

        assert_eq!(instrument.raw_symbol().as_str(), "INFY");
    }

    #[test]
    fn classifies_selected_future_using_filtered_underlying() {
        let csv = KiteInstrumentCsv::new(Bytes::from_static(EQUITY_FUTURE_CSV));
        let filter =
            |instrument: &KiteInstrument| instrument.instrument_type == KiteInstrumentType::Future;

        let instruments = parse_instruments_csv(&csv, Some(&filter)).unwrap();
        let [InstrumentAny::FuturesContract(future)] = instruments.as_slice() else {
            panic!("expected one selected future");
        };

        assert_eq!(future.asset_class, AssetClass::Equity);
    }

    #[test]
    fn classifies_equity_and_index_futures_from_classification_context() {
        let context = DerivativeClassificationContext::from_instruments(&[
            instrument(
                "INFY",
                Some("INFOSYS"),
                VENUE_NSE,
                VENUE_NSE,
                KiteInstrumentType::Equity,
            ),
            instrument(
                "NIFTY 50",
                Some("NIFTY 50"),
                "INDICES",
                VENUE_NSE,
                KiteInstrumentType::Equity,
            ),
            instrument(
                "SENSEX",
                Some("SENSEX"),
                "INDICES",
                VENUE_BSE,
                KiteInstrumentType::Equity,
            ),
            instrument(
                "NIFTY FPI 150",
                Some("NIFTY FPI 150"),
                "INDICES",
                VENUE_NSE,
                KiteInstrumentType::Equity,
            ),
        ]);

        assert_eq!(
            context
                .derivative_asset_class(&instrument(
                    "INFY26JULFUT",
                    Some("INFY"),
                    "NFO-FUT",
                    "NFO",
                    KiteInstrumentType::Future,
                ))
                .unwrap(),
            AssetClass::Equity
        );
        assert_eq!(
            context
                .derivative_asset_class(&instrument(
                    "NIFTY26JULFUT",
                    Some("NIFTY"),
                    "NFO-FUT",
                    "NFO",
                    KiteInstrumentType::Future,
                ))
                .unwrap(),
            AssetClass::Index
        );
        assert_eq!(
            context
                .derivative_asset_class(&instrument(
                    "SENSEX5026JULFUT",
                    Some("SENSEX50"),
                    "BFO-FUT",
                    "BFO",
                    KiteInstrumentType::Future,
                ))
                .unwrap(),
            AssetClass::Index
        );
        assert_eq!(
            context
                .derivative_asset_class(&instrument(
                    "FOCIT26JULFUT",
                    Some("FOCIT"),
                    "BFO-FUT",
                    "BFO",
                    KiteInstrumentType::Future,
                ))
                .unwrap(),
            AssetClass::Index
        );
        assert_eq!(
            context
                .derivative_asset_class(&instrument(
                    "NIFTYFPI26OCT1690PE",
                    Some("NIFTYFPI"),
                    "NFO-OPT",
                    "NFO",
                    KiteInstrumentType::PutOption,
                ))
                .unwrap(),
            AssetClass::Index
        );
    }

    #[test]
    fn rejects_an_unknown_futures_underlying() {
        let context = DerivativeClassificationContext::from_instruments(&[]);
        let future = instrument(
            "UNKNOWN26JULFUT",
            Some("UNKNOWN"),
            "NFO-FUT",
            "NFO",
            KiteInstrumentType::Future,
        );

        assert!(context.derivative_asset_class(&future).is_err());
    }

    #[test]
    fn classifies_commodity_and_commodity_index_futures() {
        let context = DerivativeClassificationContext::from_instruments(&[instrument(
            "MCXBULLDEX",
            Some("MCXBULLDEX"),
            "INDICES",
            "MCX",
            KiteInstrumentType::Equity,
        )]);

        assert_eq!(
            context
                .derivative_asset_class(&instrument(
                    "MCXBULLDEX26AUGFUT",
                    Some("MCXBULLDEX"),
                    "MCX-FUT",
                    "MCX",
                    KiteInstrumentType::Future,
                ))
                .unwrap(),
            AssetClass::Index
        );
        assert_eq!(
            context
                .derivative_asset_class(&instrument(
                    "GOLD26AUGFUT",
                    Some("GOLD"),
                    "MCX-FUT",
                    "MCX",
                    KiteInstrumentType::Future,
                ))
                .unwrap(),
            AssetClass::Commodity
        );
        assert_eq!(
            context
                .derivative_asset_class(&instrument(
                    "COPPER26AUGFUT",
                    Some("COPPER"),
                    "NCO-FUT",
                    "NCO",
                    KiteInstrumentType::Future,
                ))
                .unwrap(),
            AssetClass::Commodity
        );
    }

    #[test]
    fn classifies_currency_and_debt_futures() {
        let context = DerivativeClassificationContext::from_instruments(&[]);

        for underlying in ["633GS2035", "91DTB", "ONMIBOR"] {
            assert_eq!(
                context
                    .derivative_asset_class(&instrument(
                        &format!("{underlying}26JULFUT"),
                        Some(underlying),
                        "CDS-FUT",
                        "CDS",
                        KiteInstrumentType::Future,
                    ))
                    .unwrap(),
                AssetClass::Debt
            );
        }
        assert_eq!(
            context
                .derivative_asset_class(&instrument(
                    "USDINR26JULFUT",
                    Some("USDINR"),
                    "CDS-FUT",
                    "CDS",
                    KiteInstrumentType::Future,
                ))
                .unwrap(),
            AssetClass::FX
        );
        assert!(
            context
                .derivative_asset_class(&instrument(
                    "UNKNOWN26JULFUT",
                    Some("UNKNOWN"),
                    "CDS-FUT",
                    "CDS",
                    KiteInstrumentType::Future,
                ))
                .is_err()
        );
    }

    #[test]
    fn parses_fx_future_with_quote_currency() {
        let context = DerivativeClassificationContext::from_instruments(&[]);
        let mut future = instrument(
            "EURUSD26JULFUT",
            Some("EURUSD"),
            "CDS-FUT",
            "CDS",
            KiteInstrumentType::Future,
        );
        future.expiry = Some(Date::new(2026, 7, 30).unwrap());

        let parsed = parse_futures_instrument(&future, &context, 42.into()).unwrap();

        assert_eq!(parsed.currency, Currency::USD());
        assert_eq!(parsed.asset_class, AssetClass::FX);
        assert_eq!(
            instrument_token(&InstrumentAny::FuturesContract(parsed)).unwrap(),
            42,
        );
    }

    #[test]
    fn parses_call_and_put_options_with_strike_and_quote_currency() {
        let context = DerivativeClassificationContext::from_instruments(&[]);

        for (instrument_type, option_kind) in [
            (KiteInstrumentType::CallOption, OptionKind::Call),
            (KiteInstrumentType::PutOption, OptionKind::Put),
        ] {
            let mut option = instrument(
                "EURUSD26JUL125CE",
                Some("EURUSD"),
                "CDS-OPT",
                "CDS",
                instrument_type,
            );
            option.expiry = Some(Date::new(2026, 7, 30).unwrap());
            option.strike = Some(dec!(1.25));

            let parsed = parse_option_instrument(&option, &context, 42.into()).unwrap();

            assert_eq!(parsed.option_kind, option_kind);
            assert_eq!(parsed.strike_price, Price::from("1.25"));
            assert_eq!(parsed.currency, Currency::USD());
            assert_eq!(
                instrument_token(&InstrumentAny::OptionContract(parsed)).unwrap(),
                42,
            );
        }
    }

    #[test]
    fn dispatches_equity_records_and_retains_instrument_tokens() {
        let equity = instrument(
            "INFY",
            Some("INFOSYS"),
            VENUE_NSE,
            VENUE_NSE,
            KiteInstrumentType::Equity,
        );
        let index = instrument(
            "NIFTY 50",
            Some("NIFTY 50"),
            "INDICES",
            VENUE_NSE,
            KiteInstrumentType::Equity,
        );
        let context = DerivativeClassificationContext::from_instruments(&[]);

        let parsed_equity = parse_instrument(&equity, &context, 42.into()).unwrap();
        let parsed_index = parse_instrument(&index, &context, 42.into()).unwrap();

        assert!(matches!(&parsed_equity, InstrumentAny::Equity(_)));
        assert!(matches!(&parsed_index, InstrumentAny::IndexInstrument(_)));

        for parsed in [parsed_equity, parsed_index] {
            assert_eq!(instrument_token(&parsed).unwrap(), 42);
        }
    }

    #[test]
    fn converts_valid_historical_candles_and_skips_invalid_ones() {
        let equity = instrument(
            "INFY",
            Some("INFOSYS"),
            VENUE_NSE,
            VENUE_NSE,
            KiteInstrumentType::Equity,
        );
        let context = DerivativeClassificationContext::from_instruments(&[]);
        let instrument = parse_instrument(&equity, &context, 42.into()).unwrap();
        let bar_type = BarType::new(
            instrument.id(),
            BAR_SPEC_1_MINUTE_LAST,
            AggregationSource::External,
        );
        let timestamp = Timestamp::new(1_700_000_000, 0).unwrap();
        let candle = KiteHistoricalCandle {
            timestamp,
            open: dec!(1000.00),
            high: dec!(1010.00),
            low: dec!(990.00),
            close: dec!(1005.00),
            volume: 12_345,
            open_interest: Some(678),
        };

        let ts_event = 1_700_000_000_000_000_000_u64.into();
        let bar =
            parse_historical_candle(&candle, &instrument, bar_type, ts_event, 99.into()).unwrap();

        assert_eq!(bar.bar_type, bar_type);
        assert_eq!(bar.open, Price::from("1000.00"));
        assert_eq!(bar.high, Price::from("1010.00"));
        assert_eq!(bar.low, Price::from("990.00"));
        assert_eq!(bar.close, Price::from("1005.00"));
        assert_eq!(bar.volume, Quantity::from(12_345));
        assert_eq!(bar.ts_event, 1_700_000_000_000_000_000_u64);
        assert_eq!(bar.ts_init, 99);

        let invalid_candle = KiteHistoricalCandle {
            high: dec!(990.00),
            ..candle.clone()
        };
        let error =
            parse_historical_candle(&invalid_candle, &instrument, bar_type, ts_event, 99.into())
                .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid historical candle at 2023-11-14T22:13:20Z: open=1000.00, high=990.00, low=990.00, close=1005.00: high >= open"
        );

        let data = parse_historical_candles(
            vec![candle.clone(), invalid_candle, candle.clone()],
            &instrument,
            bar_type,
            99.into(),
        );
        assert_eq!(data.bars.len(), 2);
        assert!(data.open_interest.is_empty());

        let future = InstrumentAny::FuturesContract(futures_contract_es(None, None));
        let future_bar_type = BarType::new(
            future.id(),
            BAR_SPEC_1_MINUTE_LAST,
            AggregationSource::External,
        );
        let data = parse_historical_candles(vec![candle], &future, future_bar_type, 99.into());
        assert_eq!(data.open_interest.len(), 1);
        assert_eq!(data.open_interest[0].open_interest, 678);
    }

    fn instrument(
        trading_symbol: &str,
        name: Option<&str>,
        segment: &str,
        exchange: &str,
        instrument_type: KiteInstrumentType,
    ) -> KiteInstrument {
        KiteInstrument {
            instrument_token: 42,
            exchange_token: 0,
            trading_symbol: trading_symbol.to_owned(),
            name: name.map(str::to_owned),
            last_price: Decimal::ZERO,
            expiry: None,
            strike: None,
            tick_size: dec!(0.05),
            lot_size: 1,
            instrument_type,
            segment: segment.to_owned(),
            exchange: exchange.to_owned(),
        }
    }
}
