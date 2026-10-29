use std::collections::HashSet;
use std::fmt::Debug;
use std::io;

use jiff::Timestamp;
use jiff::civil::Date;
use nautilus_core::correctness::{CorrectnessError, CorrectnessResult};
use nautilus_model::identifiers::InstrumentId;
use nautilus_model::instruments::{Instrument, InstrumentAny};
use thiserror::Error;

use crate::common::consts::{
    INDIA_TIME_ZONE, INSTRUMENT_TOKEN_INFO_KEY, VENUE_BFO, VENUE_BSE, VENUE_NFO, VENUE_NSE,
};
use crate::http::client::KiteHttpClient;
use crate::http::error::KiteHttpError;
use crate::http::models::{KiteInstrument, KiteInstrumentCsv};
use crate::http::parse::parse_instruments_csv;

pub(crate) const INDEX_DERIVATIVE_ALIASES: &[(&str, &str, &str)] = &[
    (VENUE_NSE, "NIFTY", "NIFTY 50"),
    (VENUE_NSE, "BANKNIFTY", "NIFTY BANK"),
    (VENUE_NSE, "FINNIFTY", "NIFTY FIN SERVICE"),
    (VENUE_NSE, "MIDCPNIFTY", "NIFTY MID SELECT"),
    (VENUE_NSE, "NIFTYNXT50", "NIFTY NEXT 50"),
    (VENUE_NSE, "NIFTYFPI", "NIFTY FPI 150"),
    (VENUE_BSE, "SENSEX50", "SENSEX"),
];

/// Predicate applied to each raw instrument-master record before conversion.
pub type KiteInstrumentFilter<'a> = dyn Fn(&KiteInstrument) -> bool + Send + Sync + 'a;

/// Failure while loading instruments from Kite or a configured snapshot store.
#[derive(Debug, Error)]
pub enum KiteInstrumentLoadError {
    /// Kite HTTP request or instrument conversion failed.
    #[error(transparent)]
    Http(#[from] KiteHttpError),

    /// Snapshot storage failed.
    #[error(transparent)]
    Store(#[from] io::Error),

    /// A stored instrument CSV could not be decoded.
    #[error(transparent)]
    Csv(#[from] csv::Error),
}

/// Stores complete raw Kite instrument-master snapshots without imposing a storage backend.
#[async_trait::async_trait]
pub trait KiteInstrumentStore: Debug + Send + Sync {
    /// Loads the snapshot for `date`, or returns `None` when it is absent.
    async fn load_snapshot(&self, date: Date) -> io::Result<Option<KiteInstrumentCsv>>;

    /// Stores the complete snapshot for `date`.
    async fn store_snapshot(&self, date: Date, snapshot: &KiteInstrumentCsv) -> io::Result<()>;
}

/// Returns the lowercase ASCII-alphanumeric key used to match Kite symbols.
pub fn normalize_instrument_symbol(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

/// Builds a predicate that matches the venue and normalized symbol of supplied instrument IDs.
pub fn exact_instrument_filter(
    instrument_ids: impl IntoIterator<Item = InstrumentId>,
) -> impl Fn(&KiteInstrument) -> bool + Send + Sync + 'static {
    let keys = instrument_ids
        .into_iter()
        .map(|id| {
            (
                id.venue.as_str().to_owned(),
                normalize_instrument_symbol(id.symbol.as_str()),
            )
        })
        .collect::<HashSet<_>>();

    move |instrument| {
        keys.contains(&(
            instrument.exchange.clone(),
            normalize_instrument_symbol(&instrument.trading_symbol),
        ))
    }
}

/// Builds the venue and symbol key used to match requested futures underlyings.
///
/// For example, `NIFTY 50.NSE` maps to `(NSE, nifty)`.
pub fn requested_futures_underlying_key(instrument_id: &InstrumentId) -> (String, String) {
    let venue = instrument_id.venue.as_str();
    let symbol = normalize_instrument_symbol(instrument_id.symbol.as_str());
    let underlying = INDEX_DERIVATIVE_ALIASES
        .iter()
        .find_map(|&(underlying_venue, derivative_symbol, index_symbol)| {
            (venue == underlying_venue && symbol == normalize_instrument_symbol(index_symbol))
                .then(|| normalize_instrument_symbol(derivative_symbol))
        })
        .unwrap_or(symbol);

    (venue.to_owned(), underlying)
}

/// Maps a derivative venue to the venue of its underlying instrument.
pub fn underlying_venue(derivative_venue: &str) -> Option<&'static str> {
    match derivative_venue {
        VENUE_NFO => Some(VENUE_NSE),
        VENUE_BFO => Some(VENUE_BSE),
        "CDS" => Some("CDS"),
        "MCX" => Some("MCX"),
        "NCO" => Some("NCO"),
        _ => None,
    }
}

/// Loads today's stored instrument master, fetching and storing it when absent.
pub async fn load_instruments(
    http_client: &KiteHttpClient,
    store: Option<&dyn KiteInstrumentStore>,
    filter: Option<&KiteInstrumentFilter<'_>>,
) -> Result<Vec<InstrumentAny>, KiteInstrumentLoadError> {
    let Some(store) = store else {
        return Ok(http_client.get_instruments(filter).await?.0);
    };
    let date = Timestamp::now().to_zoned(INDIA_TIME_ZONE.clone()).date();

    if let Some(csv) = store.load_snapshot(date).await? {
        return parse_instruments_csv(&csv, filter).map_err(Into::into);
    }

    let (instruments, csv) = http_client.get_instruments(filter).await?;
    store.store_snapshot(date, &csv).await?;
    Ok(instruments)
}

/// Returns the Kite instrument token stored in a Nautilus instrument's metadata.
pub fn instrument_token(instrument: &InstrumentAny) -> CorrectnessResult<u64> {
    let info = match instrument {
        InstrumentAny::Equity(instrument) => instrument.info.as_ref(),
        InstrumentAny::FuturesContract(instrument) => instrument.info.as_ref(),
        InstrumentAny::IndexInstrument(instrument) => instrument.info.as_ref(),
        InstrumentAny::OptionContract(instrument) => instrument.info.as_ref(),
        _ => {
            return Err(CorrectnessError::PredicateViolation {
                message: format!(
                    "unsupported Kite instrument class {:?}",
                    instrument.instrument_class()
                ),
            });
        }
    }
    .ok_or_else(|| CorrectnessError::PredicateViolation {
        message: format!("Kite instrument {} has no metadata", instrument.id()),
    })?;

    info.get_u64(INSTRUMENT_TOKEN_INFO_KEY)
        .ok_or_else(|| CorrectnessError::PredicateViolation {
            message: format!(
                "Kite instrument {} has no valid {INSTRUMENT_TOKEN_INFO_KEY} metadata",
                instrument.id()
            ),
        })
}

pub(crate) fn validate_loaded_instruments(
    requested: &HashSet<InstrumentId>,
    instruments: &[InstrumentAny],
) -> CorrectnessResult<()> {
    let loaded = instruments
        .iter()
        .map(Instrument::id)
        .collect::<HashSet<_>>();
    let mut missing = requested.difference(&loaded).collect::<Vec<_>>();
    missing.sort_unstable();

    if missing.is_empty() {
        return Ok(());
    }

    Err(CorrectnessError::PredicateViolation {
        message: format!(
            "Kite instruments not found: {}",
            missing
                .into_iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}
