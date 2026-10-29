use nautilus_core::{Params, UnixNanos};
use nautilus_model::custom_data;
use nautilus_model::data::{Bar, BarType, DataType};
use nautilus_model::identifiers::InstrumentId;
use nautilus_model::instruments::InstrumentAny;
use nautilus_serialization::arrow_custom_data;
use serde_json::Value;

const BAR_TYPE: &str = "bar_type";
const INSTRUMENT_ID: &str = "instrument_id";

/// Nautilus custom-data type name for historical open interest.
pub const KITE_HISTORICAL_OPEN_INTEREST_TYPE: &str = stringify!(KiteHistoricalOpenInterest);
/// Nautilus custom-data type name for live open interest.
pub const KITE_OPEN_INTEREST_TYPE: &str = stringify!(KiteOpenInterest);

/// Kite open interest sampled alongside a historical candle.
///
/// This value is tied to a bar series. Unlike [`KiteOpenInterest`], the historical candle API
/// does not provide the session's open-interest high and low.
#[arrow_custom_data]
#[custom_data]
pub struct KiteHistoricalOpenInterest {
    /// Instrument associated with the sample.
    pub instrument_id: InstrumentId,
    /// Historical bar series associated with the sample.
    pub bar_type: BarType,
    /// Open contracts reported alongside the candle.
    pub open_interest: u64,
    /// Time of the corresponding candle.
    pub ts_event: UnixNanos,
    /// Time the adapter initialized the sample.
    pub ts_init: UnixNanos,
}

/// Kite open interest and its session range from a live [full-mode quote packet].
///
/// Unlike [`KiteHistoricalOpenInterest`], this is a live instrument update rather than a sample
/// associated with a historical bar.
///
/// [full-mode quote packet]: https://kite.trade/docs/connect/v3/websocket/#quote-packet-structure
#[arrow_custom_data]
#[custom_data]
pub struct KiteOpenInterest {
    /// Instrument associated with the update.
    pub instrument_id: InstrumentId,
    /// Current open contracts.
    pub open_interest: u64,
    /// Highest open interest reported for the session.
    pub day_high: u64,
    /// Lowest open interest reported for the session.
    pub day_low: u64,
    /// Exchange event time, when supplied by Kite.
    pub ts_event: UnixNanos,
    /// Time the adapter initialized the update.
    pub ts_init: UnixNanos,
}

/// Returns whether Kite reports meaningful open interest for the instrument.
pub fn supports_open_interest(instrument: &InstrumentAny) -> bool {
    matches!(
        instrument,
        InstrumentAny::FuturesContract(_) | InstrumentAny::OptionContract(_)
    )
}

impl KiteHistoricalOpenInterest {
    /// Returns the identified Nautilus data type for a bar series.
    pub fn data_type(bar_type: BarType) -> DataType {
        let metadata = Params::new(
            [
                (
                    INSTRUMENT_ID.to_owned(),
                    Value::String(bar_type.instrument_id().to_string()),
                ),
                (BAR_TYPE.to_owned(), Value::String(bar_type.to_string())),
            ]
            .into(),
        );
        DataType::new(
            KITE_HISTORICAL_OPEN_INTEREST_TYPE,
            Some(metadata),
            Some(bar_type.to_string()),
        )
    }
}

impl KiteOpenInterest {
    /// Returns the identified Nautilus data type for an instrument.
    pub fn data_type(instrument_id: InstrumentId) -> DataType {
        DataType::new(
            KITE_OPEN_INTEREST_TYPE,
            Some(Params::new(
                [(
                    INSTRUMENT_ID.to_owned(),
                    Value::String(instrument_id.to_string()),
                )]
                .into(),
            )),
            Some(instrument_id.to_string()),
        )
    }
}

/// Historical bars and their corresponding open-interest samples.
#[derive(Debug)]
pub struct KiteHistoricalData {
    /// Parsed historical bars.
    pub bars: Vec<Bar>,
    /// Open-interest samples returned with those bars.
    pub open_interest: Vec<KiteHistoricalOpenInterest>,
}

/// Registers Kite custom data for catalog serialization.
pub fn register_kite_custom_data() {
    nautilus_serialization::ensure_custom_data_registered::<KiteHistoricalOpenInterest>();
    nautilus_serialization::ensure_custom_data_registered::<KiteOpenInterest>();
}

#[cfg(test)]
mod tests {
    use nautilus_model::data::BarType;
    use nautilus_model::identifiers::InstrumentId;

    use crate::data_types::{KiteHistoricalOpenInterest, KiteOpenInterest};

    #[test]
    fn open_interest_topics_include_their_identity() {
        let instrument_id = InstrumentId::from("INFY.NSE");
        let bar_type = BarType::from("INFY.NSE-1-MINUTE-LAST-EXTERNAL");

        assert_eq!(
            KiteOpenInterest::data_type(instrument_id).topic(),
            "KiteOpenInterest.instrument_id=INFY.NSE.identifier=INFY.NSE",
        );
        assert_eq!(
            KiteHistoricalOpenInterest::data_type(bar_type).topic(),
            "KiteHistoricalOpenInterest.bar_type=INFY.NSE-1-MINUTE-LAST-EXTERNAL.instrument_id=INFY.NSE.identifier=INFY.NSE-1-MINUTE-LAST-EXTERNAL",
        );
    }
}
