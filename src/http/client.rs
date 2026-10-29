use std::collections::HashMap;
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Arc;

use jiff::Span;
use jiff::civil::DateTime;
use nautilus_core::correctness::CorrectnessError;
use nautilus_core::time::get_atomic_clock_realtime;
use nautilus_model::data::BarType;
use nautilus_model::identifiers::InstrumentId;
use nautilus_model::instruments::{Instrument, InstrumentAny};
use nautilus_network::http::{HttpClient, HttpMethod, HttpResponse};
use nautilus_network::ratelimiter::quota::Quota;
use nautilus_network::retry::{RetryManager, create_http_retry_manager};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::common::consts::{KITE_VERSION, KITE_VERSION_HEADER};
use crate::credential::KiteSessionCredentials;
use crate::data_types::{KiteHistoricalData, supports_open_interest};
use crate::execution::models::{
    KiteModifyOrderRequest, KiteOrderIdData, KiteOrderRecord, KiteOrderVariety, KitePlaceOrderData,
    KitePlaceOrderRequest, KiteTradeRecord,
};
use crate::http::consts::{CONTENT_TYPE_HEADER, FORM_CONTENT_TYPE, TIMEOUT_SECONDS};
use crate::http::error::{KiteException, KiteHttpError};
use crate::http::models::{
    KiteCandleInterval, KiteHistoricalCandle, KiteHistoricalCandleData, KiteHolding,
    KiteInstrumentCsv, KiteLtpQuote, KiteMarketQuote, KiteOhlcQuote, KiteOrderCharges,
    KiteOrderChargesRequest, KiteOrderMargin, KiteOrderMarginRequest, KitePositions,
    KiteUserMargins, KiteUserProfile,
};
use crate::http::parse::{candle_interval, parse_historical_candles, parse_instruments_csv};
use crate::http::response::{decode, ensure_success};
use crate::instruments::{KiteInstrumentFilter, instrument_token};
use crate::urls::api_url;

const INSTRUMENTS_PATH: &str = "/instruments";
const HISTORICAL_CANDLES_PATH: &str = "/instruments/historical";
const QUOTES_PATH: &str = "/quote";
const OHLC_QUOTES_PATH: &str = "/quote/ohlc";
const LTP_QUOTES_PATH: &str = "/quote/ltp";
const ORDER_CHARGES_PATH: &str = "/charges/orders";
const ORDER_MARGINS_PATH: &str = "/margins/orders";
const ORDERS_PATH: &str = "/orders";
const TRADES_PATH: &str = "/trades";
const HOLDINGS_PATH: &str = "/portfolio/holdings";
const POSITIONS_PATH: &str = "/portfolio/positions";
const USER_MARGINS_PATH: &str = "/user/margins";
const USER_PROFILE_PATH: &str = "/user/profile";

const AUTHORIZATION_HEADER: &str = "Authorization";
const JSON_CONTENT_TYPE: &str = "application/json";

const DEFAULT_RATE_LIMIT_KEY: &str = "default";
const HISTORICAL_RATE_LIMIT_KEY: &str = "historical";
const DEFAULT_QUOTA: Option<Quota> = Quota::per_second(NonZeroU32::new(10).unwrap());
const HISTORICAL_QUOTA: Quota = Quota::per_second(NonZeroU32::new(3).unwrap()).unwrap();

const HISTORICAL_QUERY_DATETIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S";
const READ_OPERATION_NAME: &str = "Kite read request";

/// Authenticated Kite Connect client with Nautilus domain conversions.
#[derive(Clone, Debug)]
pub struct KiteHttpClient {
    inner: Arc<KiteHttpClientInner>,
}

#[derive(Debug)]
struct KiteHttpClientInner {
    client: HttpClient,
    session_credentials: Arc<KiteSessionCredentials>,
    retry_manager: RetryManager<KiteHttpError>,
}

#[derive(Clone, Copy)]
enum KiteHttpRequestBody<'a> {
    Form(&'a [u8]),
    Json(&'a [u8]),
}

struct KiteHttpRequest<'a> {
    method: HttpMethod,
    path: &'a str,
    params: Option<&'a HashMap<String, Vec<String>>>,
    body: Option<KiteHttpRequestBody<'a>>,
    rate_limit_key: &'static str,
}

impl KiteHttpClient {
    /// Creates a client for an authenticated Kite session.
    pub fn new(session_credentials: Arc<KiteSessionCredentials>) -> Result<Self, KiteHttpError> {
        let headers = HashMap::from([(KITE_VERSION_HEADER.to_owned(), KITE_VERSION.to_owned())]);
        let keyed_quotas = vec![(HISTORICAL_RATE_LIMIT_KEY.to_owned(), HISTORICAL_QUOTA)];
        let client = HttpClient::builder()
            .headers(headers)
            .keyed_quotas(keyed_quotas)
            .maybe_default_quota(DEFAULT_QUOTA)
            .redact_url(true)
            .timeout_secs(TIMEOUT_SECONDS)
            .build()?;

        Ok(Self {
            inner: Arc::new(KiteHttpClientInner {
                client,
                session_credentials,
                retry_manager: create_http_retry_manager(),
            }),
        })
    }

    /// Loads Kite's [instrument list] and converts matching records to Nautilus instruments.
    ///
    /// [instrument list]: https://kite.trade/docs/connect/v3/market-data-and-instruments/#retrieving-full-instrument-list
    pub async fn get_instruments(
        &self,
        filter: Option<&KiteInstrumentFilter<'_>>,
    ) -> Result<(Vec<InstrumentAny>, KiteInstrumentCsv), KiteHttpError> {
        let csv = self.get_instruments_csv().await?;
        let instruments = parse_instruments_csv(&csv, filter)?;
        Ok((instruments, csv))
    }

    /// Loads Kite [historical candle data] and converts it to Nautilus bars and open interest.
    ///
    /// [historical candle data]: https://kite.trade/docs/connect/v3/historical/
    pub async fn get_historical_candles(
        &self,
        instrument: &InstrumentAny,
        bar_type: BarType,
        from: DateTime,
        to: DateTime,
        continuous: bool,
        limit: Option<NonZeroUsize>,
    ) -> Result<KiteHistoricalData, KiteHttpError> {
        if bar_type.instrument_id() != instrument.id() {
            return Err(CorrectnessError::PredicateViolation {
                message: format!(
                    "bar type instrument {} does not match requested instrument {}",
                    bar_type.instrument_id(),
                    instrument.id()
                ),
            }
            .into());
        }

        let interval = candle_interval(bar_type)?;
        let instrument_token = instrument_token(instrument)?;
        let candles = self
            .get_historical_candle_batches(
                instrument_token,
                interval,
                from,
                to,
                continuous,
                supports_open_interest(instrument),
                limit,
            )
            .await?;
        let ts_init = get_atomic_clock_realtime().get_time_ns();

        let mut data = parse_historical_candles(candles, instrument, bar_type, ts_init);
        data.bars.sort_unstable_by_key(|bar| bar.ts_event);
        data.open_interest
            .sort_unstable_by_key(|open_interest| open_interest.ts_event);
        Ok(data)
    }

    /// Downloads the current Kite [instrument list] CSV.
    ///
    /// [instrument list]: https://kite.trade/docs/connect/v3/market-data-and-instruments/#retrieving-full-instrument-list
    pub async fn get_instruments_csv(&self) -> Result<KiteInstrumentCsv, KiteHttpError> {
        let response = self
            .get_raw(INSTRUMENTS_PATH, None, DEFAULT_RATE_LIMIT_KEY)
            .await?;
        Ok(KiteInstrumentCsv::new(response.body))
    }

    /// Returns Kite [full market quotes] keyed by `EXCHANGE:tradingsymbol`.
    ///
    /// Kite accepts at most 500 instruments in one request and omits instruments for which no
    /// quote is available.
    ///
    /// [full market quotes]: https://kite.trade/docs/connect/v3/market-quotes/#retrieving-full-market-quotes
    pub async fn get_quotes(
        &self,
        instrument_ids: &[InstrumentId],
    ) -> Result<HashMap<String, KiteMarketQuote>, KiteHttpError> {
        let params = quote_params(instrument_ids);

        self.get_json(QUOTES_PATH, Some(&params), DEFAULT_RATE_LIMIT_KEY)
            .await
    }

    /// Returns Kite [OHLC quotes] keyed by `EXCHANGE:tradingsymbol`.
    ///
    /// Kite accepts at most 1000 instruments in one request and omits instruments for which no
    /// quote is available.
    ///
    /// [OHLC quotes]: https://kite.trade/docs/connect/v3/market-quotes/#retrieving-ohlc-quotes
    pub async fn get_ohlc_quotes(
        &self,
        instrument_ids: &[InstrumentId],
    ) -> Result<HashMap<String, KiteOhlcQuote>, KiteHttpError> {
        let params = quote_params(instrument_ids);

        self.get_json(OHLC_QUOTES_PATH, Some(&params), DEFAULT_RATE_LIMIT_KEY)
            .await
    }

    /// Returns Kite [last-traded-price quotes] keyed by `EXCHANGE:tradingsymbol`.
    ///
    /// Kite accepts at most 1000 instruments in one request and omits instruments for which no
    /// quote is available.
    ///
    /// [last-traded-price quotes]: https://kite.trade/docs/connect/v3/market-quotes/#retrieving-ltp-quotes
    pub async fn get_ltp_quotes(
        &self,
        instrument_ids: &[InstrumentId],
    ) -> Result<HashMap<String, KiteLtpQuote>, KiteHttpError> {
        let params = quote_params(instrument_ids);

        self.get_json(LTP_QUOTES_PATH, Some(&params), DEFAULT_RATE_LIMIT_KEY)
            .await
    }

    /// Returns all orders for the current trading day from Kite's [orders API].
    ///
    /// [orders API]: https://kite.trade/docs/connect/v3/orders/
    pub async fn get_orders(&self) -> Result<Vec<KiteOrderRecord>, KiteHttpError> {
        self.get_json(ORDERS_PATH, None, DEFAULT_RATE_LIMIT_KEY)
            .await
    }

    /// Returns all trades for the current trading day from Kite's [orders API].
    ///
    /// [orders API]: https://kite.trade/docs/connect/v3/orders/
    pub async fn get_trades(&self) -> Result<Vec<KiteTradeRecord>, KiteHttpError> {
        self.get_json(TRADES_PATH, None, DEFAULT_RATE_LIMIT_KEY)
            .await
    }

    /// Returns trades belonging to one Kite order from Kite's [orders API].
    ///
    /// [orders API]: https://kite.trade/docs/connect/v3/orders/
    pub async fn get_order_trades(
        &self,
        order_id: &str,
    ) -> Result<Vec<KiteTradeRecord>, KiteHttpError> {
        let path = format!("{ORDERS_PATH}/{order_id}/trades");
        self.get_json(&path, None, DEFAULT_RATE_LIMIT_KEY).await
    }

    /// Returns the status history of one Kite order from Kite's [orders API].
    ///
    /// [orders API]: https://kite.trade/docs/connect/v3/orders/
    pub async fn get_order_history(
        &self,
        order_id: &str,
    ) -> Result<Vec<KiteOrderRecord>, KiteHttpError> {
        let path = format!("{ORDERS_PATH}/{order_id}");
        self.get_json(&path, None, DEFAULT_RATE_LIMIT_KEY).await
    }

    /// Returns account margin balances from Kite's [user API].
    ///
    /// [user API]: https://kite.trade/docs/connect/v3/user/#funds-and-margins
    pub async fn get_user_margins(&self) -> Result<KiteUserMargins, KiteHttpError> {
        self.get_json(USER_MARGINS_PATH, None, DEFAULT_RATE_LIMIT_KEY)
            .await
    }

    /// Returns the authenticated user's profile from Kite's [user API].
    ///
    /// [user API]: https://kite.trade/docs/connect/v3/user/#user-profile
    pub async fn get_user_profile(&self) -> Result<KiteUserProfile, KiteHttpError> {
        self.get_json(USER_PROFILE_PATH, None, DEFAULT_RATE_LIMIT_KEY)
            .await
    }

    /// Returns day and net positions from Kite's [portfolio API].
    ///
    /// [portfolio API]: https://kite.trade/docs/connect/v3/portfolio/#positions
    pub async fn get_positions(&self) -> Result<KitePositions, KiteHttpError> {
        self.get_json(POSITIONS_PATH, None, DEFAULT_RATE_LIMIT_KEY)
            .await
    }

    /// Returns delivery holdings from Kite's [portfolio API].
    ///
    /// [portfolio API]: https://kite.trade/docs/connect/v3/portfolio/#holdings
    pub async fn get_holdings(&self) -> Result<Vec<KiteHolding>, KiteHttpError> {
        self.get_json(HOLDINGS_PATH, None, DEFAULT_RATE_LIMIT_KEY)
            .await
    }

    /// Calculates an itemized charge breakdown using Kite's [charges API].
    ///
    /// [charges API]: https://kite.trade/docs/connect/v3/margins/#virtual-contract-note
    pub async fn get_order_charges(
        &self,
        orders: &[KiteOrderChargesRequest],
    ) -> Result<Vec<KiteOrderCharges>, KiteHttpError> {
        self.send_json_request(HttpMethod::POST, ORDER_CHARGES_PATH, orders)
            .await
    }

    /// Calculates margin requirements using Kite's [margin API].
    ///
    /// [margin API]: https://kite.trade/docs/connect/v3/margins/#order-margins
    pub async fn get_order_margins(
        &self,
        orders: &[KiteOrderMarginRequest],
    ) -> Result<Vec<KiteOrderMargin>, KiteHttpError> {
        self.send_json_request(HttpMethod::POST, ORDER_MARGINS_PATH, orders)
            .await
    }

    /// Places an order through Kite's [orders API].
    ///
    /// [orders API]: https://kite.trade/docs/connect/v3/orders/#placing-orders
    pub async fn place_order(
        &self,
        variety: KiteOrderVariety,
        request: &KitePlaceOrderRequest,
    ) -> Result<KitePlaceOrderData, KiteHttpError> {
        let path = format!("{ORDERS_PATH}/{}", variety.as_str());
        self.send_form_request(HttpMethod::POST, &path, request)
            .await
    }

    /// Modifies an existing order through Kite's [orders API].
    ///
    /// [orders API]: https://kite.trade/docs/connect/v3/orders/#modifying-orders
    pub async fn modify_order(
        &self,
        variety: KiteOrderVariety,
        order_id: &str,
        request: &KiteModifyOrderRequest,
    ) -> Result<String, KiteHttpError> {
        let path = format!("{ORDERS_PATH}/{}/{order_id}", variety.as_str());
        let data: KiteOrderIdData = self
            .send_form_request(HttpMethod::PUT, &path, request)
            .await?;

        Ok(data.order_id)
    }

    /// Cancels an order through Kite's [orders API].
    ///
    /// `parent_order_id` identifies the parent of a cover order when applicable.
    ///
    /// [orders API]: https://kite.trade/docs/connect/v3/orders/#cancelling-orders
    pub async fn cancel_order(
        &self,
        variety: KiteOrderVariety,
        order_id: &str,
        parent_order_id: Option<&str>,
    ) -> Result<String, KiteHttpError> {
        let path = format!("{ORDERS_PATH}/{}/{order_id}", variety.as_str());
        let data: KiteOrderIdData = match parent_order_id {
            Some(parent_order_id) => {
                self.send_form_request(
                    HttpMethod::DELETE,
                    &path,
                    &[("parent_order_id", parent_order_id)],
                )
                .await?
            }
            None => {
                self.send_command_request(HttpMethod::DELETE, &path, None)
                    .await?
            }
        };

        Ok(data.order_id)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors Kite's historical candle request parameters"
    )]
    async fn get_historical_candle_batches(
        &self,
        instrument_token: u64,
        interval: KiteCandleInterval,
        from: DateTime,
        to: DateTime,
        continuous: bool,
        include_open_interest: bool,
        limit: Option<NonZeroUsize>,
    ) -> Result<Vec<KiteHistoricalCandle>, KiteHttpError> {
        let mut batches: Vec<Vec<KiteHistoricalCandle>> = Vec::new();
        let mut candle_count = 0;

        for (batch_from, batch_to) in historical_batches(from, to, interval.maximum_request_days())
        {
            let mut batch = self
                .get_historical_candles_once(
                    instrument_token,
                    interval,
                    batch_from,
                    batch_to,
                    continuous,
                    include_open_interest,
                )
                .await?;
            batch.sort_unstable_by_key(|candle| candle.timestamp);
            if batches
                .last()
                .and_then(|newer| newer.first())
                .is_some_and(|newer| {
                    batch
                        .last()
                        .is_some_and(|older| newer.timestamp == older.timestamp)
                })
            {
                batch.pop();
            }

            if let Some(limit) = limit {
                let remaining = limit.get().saturating_sub(candle_count);
                if batch.len() > remaining {
                    batch.drain(..batch.len() - remaining);
                }
            }

            candle_count += batch.len();
            batches.push(batch);
            if limit.is_some_and(|limit| candle_count >= limit.get()) {
                break;
            }
        }

        Ok(batches.into_iter().rev().flatten().collect())
    }

    async fn get_raw(
        &self,
        path: &str,
        params: Option<&HashMap<String, Vec<String>>>,
        rate_limit_key: &'static str,
    ) -> Result<HttpResponse, KiteHttpError> {
        let request = KiteHttpRequest {
            method: HttpMethod::GET,
            path,
            params,
            body: None,
            rate_limit_key,
        };
        self.inner
            .retry_manager
            .invocation(
                READ_OPERATION_NAME,
                || self.send_authenticated_request(&request),
                should_retry_read,
                KiteHttpError::Retry,
            )
            .execute()
            .await
    }

    async fn get_json<T>(
        &self,
        path: &str,
        params: Option<&HashMap<String, Vec<String>>>,
        rate_limit_key: &'static str,
    ) -> Result<T, KiteHttpError>
    where
        T: DeserializeOwned,
    {
        let response = self.get_raw(path, params, rate_limit_key).await?;
        decode(response)
    }

    async fn send_form_request<T, F>(
        &self,
        method: HttpMethod,
        path: &str,
        form: &F,
    ) -> Result<T, KiteHttpError>
    where
        T: DeserializeOwned,
        F: Serialize,
    {
        let body = serde_urlencoded::to_string(form)?.into_bytes();
        self.send_command_request(method, path, Some(KiteHttpRequestBody::Form(&body)))
            .await
    }

    async fn send_json_request<T, B>(
        &self,
        method: HttpMethod,
        path: &str,
        body: &B,
    ) -> Result<T, KiteHttpError>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let body = serde_json::to_vec(body)?;
        self.send_retryable_request(method, path, Some(KiteHttpRequestBody::Json(&body)))
            .await
    }

    async fn send_retryable_request<T>(
        &self,
        method: HttpMethod,
        path: &str,
        body: Option<KiteHttpRequestBody<'_>>,
    ) -> Result<T, KiteHttpError>
    where
        T: DeserializeOwned,
    {
        let request = KiteHttpRequest {
            method,
            path,
            params: None,
            body,
            rate_limit_key: DEFAULT_RATE_LIMIT_KEY,
        };
        let response = self
            .inner
            .retry_manager
            .invocation(
                READ_OPERATION_NAME,
                || self.send_authenticated_request(&request),
                should_retry_read,
                KiteHttpError::Retry,
            )
            .execute()
            .await?;

        decode(response)
    }

    async fn send_command_request<T>(
        &self,
        method: HttpMethod,
        path: &str,
        body: Option<KiteHttpRequestBody<'_>>,
    ) -> Result<T, KiteHttpError>
    where
        T: DeserializeOwned,
    {
        let request = KiteHttpRequest {
            method,
            path,
            params: None,
            body,
            rate_limit_key: DEFAULT_RATE_LIMIT_KEY,
        };
        let response = self.send_authenticated_request(&request).await?;

        decode(response)
    }

    async fn get_historical_candles_once(
        &self,
        instrument_token: u64,
        interval: KiteCandleInterval,
        from: DateTime,
        to: DateTime,
        continuous: bool,
        include_open_interest: bool,
    ) -> Result<Vec<KiteHistoricalCandle>, KiteHttpError> {
        let path = format!(
            "{HISTORICAL_CANDLES_PATH}/{instrument_token}/{}",
            interval.as_str()
        );
        let params = HashMap::from([
            (
                "from".to_owned(),
                vec![from.strftime(HISTORICAL_QUERY_DATETIME_FORMAT).to_string()],
            ),
            (
                "to".to_owned(),
                vec![to.strftime(HISTORICAL_QUERY_DATETIME_FORMAT).to_string()],
            ),
            (
                "continuous".to_owned(),
                vec![u8::from(continuous).to_string()],
            ),
            (
                "oi".to_owned(),
                vec![u8::from(include_open_interest).to_string()],
            ),
        ]);
        let data: KiteHistoricalCandleData = self
            .get_json(&path, Some(&params), HISTORICAL_RATE_LIMIT_KEY)
            .await?;

        Ok(data.candles)
    }

    fn authorization_header(&self) -> String {
        format!(
            "token {}:{}",
            self.inner.session_credentials.api_key().expose_secret(),
            self.inner
                .session_credentials
                .access_token()
                .expose_secret()
        )
    }

    async fn send_authenticated_request(
        &self,
        request: &KiteHttpRequest<'_>,
    ) -> Result<HttpResponse, KiteHttpError> {
        let url = api_url(request.path).to_string();
        let mut headers =
            HashMap::from([(AUTHORIZATION_HEADER.to_owned(), self.authorization_header())]);
        let (body, content_type) = match request.body {
            Some(KiteHttpRequestBody::Form(body)) => (Some(body), Some(FORM_CONTENT_TYPE)),
            Some(KiteHttpRequestBody::Json(body)) => (Some(body), Some(JSON_CONTENT_TYPE)),
            None => (None, None),
        };
        if let Some(content_type) = content_type {
            headers.insert(CONTENT_TYPE_HEADER.to_owned(), content_type.to_owned());
        }

        let response = self
            .inner
            .client
            .request(
                request.method.into(),
                url,
                request.params,
                Some(headers),
                body.map(<[u8]>::to_vec),
                None,
                Some(vec![request.rate_limit_key.to_owned()]),
            )
            .await?;

        ensure_success(response)
    }
}

fn quote_params(instrument_ids: &[InstrumentId]) -> HashMap<String, Vec<String>> {
    let instruments = instrument_ids
        .iter()
        .map(|instrument_id| {
            format!(
                "{}:{}",
                instrument_id.venue.as_str(),
                instrument_id.symbol.as_str()
            )
        })
        .collect();

    HashMap::from([("i".to_owned(), instruments)])
}

fn historical_batches(
    from: DateTime,
    to: DateTime,
    maximum_request_days: i64,
) -> impl Iterator<Item = (DateTime, DateTime)> {
    assert!(maximum_request_days > 0);

    let mut next_batch_to = Some(to);

    std::iter::from_fn(move || {
        let batch_to = next_batch_to?;
        let batch_from = if batch_to > from {
            batch_to
                .checked_sub(Span::new().days(maximum_request_days))
                .map_or(from, |candidate| candidate.max(from))
        } else {
            from
        };

        next_batch_to = (batch_from > from).then_some(batch_from);
        Some((batch_from, batch_to))
    })
}

fn should_retry_read(error: &KiteHttpError) -> bool {
    match error {
        KiteHttpError::Api {
            status, error_type, ..
        } => *error_type == KiteException::Network || is_retryable_http_status(*status),
        KiteHttpError::HttpStatus(status) => is_retryable_http_status(*status),
        _ => false,
    }
}

const fn is_retryable_http_status(status: u16) -> bool {
    matches!(status, 429 | 502 | 503 | 504)
}

#[cfg(test)]
mod tests {
    use jiff::civil::DateTime;

    use crate::http::client::historical_batches;

    #[test]
    fn splits_historical_requests_at_the_interval_limit() {
        let from = DateTime::new(2026, 1, 1, 0, 0, 0, 0).unwrap();
        let boundary = DateTime::new(2026, 1, 16, 0, 0, 0, 0).unwrap();
        let to = DateTime::new(2026, 2, 15, 0, 0, 0, 0).unwrap();

        assert_eq!(
            historical_batches(from, to, 30).collect::<Vec<_>>(),
            vec![(boundary, to), (from, boundary)]
        );
    }
}
