use nautilus_core::correctness::CorrectnessResult;
use nautilus_model::identifiers::{InstrumentId, Symbol, Venue};
use nautilus_model::instruments::Instrument;
use nautilus_model::types::{Price, Quantity};
use rust_decimal::Decimal;

pub(crate) fn parse_instrument_id(
    exchange: &str,
    trading_symbol: &str,
) -> CorrectnessResult<InstrumentId> {
    Ok(InstrumentId::new(
        Symbol::new_checked(trading_symbol)?,
        Venue::new_checked(exchange)?,
    ))
}

pub(crate) fn instrument_price<I: Instrument>(
    value: Decimal,
    instrument: &I,
) -> CorrectnessResult<Price> {
    Price::from_decimal_dp(value, instrument.price_precision())
}

pub(crate) fn instrument_quantity<I: Instrument>(
    value: Decimal,
    instrument: &I,
) -> CorrectnessResult<Quantity> {
    Quantity::from_decimal_dp(value, instrument.size_precision())
}
