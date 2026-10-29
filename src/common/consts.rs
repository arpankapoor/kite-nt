use std::time::Duration;

use jiff::tz::TimeZone;

pub(crate) static INDIA_TIME_ZONE: TimeZone = jiff::tz::get!("Asia/Kolkata");

pub(crate) const KITE: &str = "KITE";
pub(crate) const KITE_VERSION_HEADER: &str = "X-Kite-Version";
pub(crate) const KITE_VERSION: &str = "3";

pub(crate) const INSTRUMENT_TOKEN_INFO_KEY: &str = "instrument_token";

pub(crate) const CLIENT_TASK_GRACEFUL_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const CLIENT_TASK_ABORT_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) const VENUE_BFO: &str = "BFO";
pub(crate) const VENUE_BSE: &str = "BSE";
pub(crate) const VENUE_NFO: &str = "NFO";
pub(crate) const VENUE_NSE: &str = "NSE";
