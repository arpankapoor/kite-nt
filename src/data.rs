use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use jiff::civil::DateTime;
use nautilus_common::clients::{DataClient, execute_instrument_subscriptions};
use nautilus_common::live::get_data_event_sender;
use nautilus_common::live::sender::EventSender;
use nautilus_common::messages::DataEvent;
use nautilus_common::messages::data::{
    BarsResponse, DataRequestFailed, DataResponse, DataResponseEnvelope, InstrumentResponse,
    InstrumentsResponse, RequestBars, RequestInstrument, RequestInstruments, SubscribeBookDepth,
    SubscribeCustomData, SubscribeIndexPrices, SubscribeQuotes, UnsubscribeBookDepth,
    UnsubscribeCustomData, UnsubscribeIndexPrices, UnsubscribeQuotes,
};
use nautilus_core::correctness::CorrectnessError;
use nautilus_core::datetime::datetime_to_unix_nanos;
use nautilus_core::time::get_atomic_clock_realtime;
use nautilus_core::{AtomicMap, UnixNanos};
use nautilus_live::task::{TaskGroup, TaskRef};
use nautilus_model::data::DataType;
use nautilus_model::identifiers::{ClientId, InstrumentId, Venue};
use nautilus_model::instruments::{Instrument, InstrumentAny};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::common::consts::{
    CLIENT_TASK_ABORT_TIMEOUT, CLIENT_TASK_GRACEFUL_TIMEOUT, INDIA_TIME_ZONE, KITE,
};
use crate::config::KiteDataClientConfig;
use crate::data_types::{KITE_OPEN_INTEREST_TYPE, register_kite_custom_data};
use crate::http::client::KiteHttpClient;
use crate::http::parse::candle_interval;
use crate::instruments::{
    KiteInstrumentStore, exact_instrument_filter, instrument_token, load_instruments,
    validate_loaded_instruments,
};
use crate::websocket::client::KiteWsClient;
use crate::websocket::error::KiteWsError;
use crate::websocket::messages::{KiteMarketData, KiteWebSocketMode, KiteWsDataEvent};
use crate::websocket::parse::{KiteMarketDataSelection, normalize_selected_market_data};

pub(crate) struct KiteDataClient {
    client_id: ClientId,
    http_client: KiteHttpClient,
    ws_client: KiteWsClient,
    tasks: TaskGroup,
    ws_stream_task: Option<TaskRef>,
    is_connected: bool,
    data_sender: EventSender<DataEvent>,
    // Full Nautilus instruments keyed by their stable domain identifiers.
    instruments: Arc<HashMap<InstrumentId, InstrumentAny>>,
    // Resolves Kite's numeric WebSocket tokens to Nautilus instrument identifiers.
    instrument_ids_by_token: Arc<HashMap<u32, InstrumentId>>,
    // Requested Nautilus data types for each Kite WebSocket token.
    subscriptions: Arc<AtomicMap<u32, KiteMarketDataSelection>>,
    instrument_store: Option<Arc<dyn KiteInstrumentStore>>,
    instrument_universe: Vec<InstrumentId>,
}

impl KiteDataClient {
    pub(crate) fn new(client_id: ClientId, config: KiteDataClientConfig) -> Self {
        Self {
            client_id,
            http_client: config.http_client,
            ws_client: config.ws_client,
            tasks: TaskGroup::new(),
            ws_stream_task: None,
            is_connected: false,
            data_sender: get_data_event_sender(),
            instruments: Arc::new(HashMap::new()),
            instrument_ids_by_token: Arc::new(HashMap::new()),
            subscriptions: Arc::new(AtomicMap::new()),
            instrument_store: config.instrument_store,
            instrument_universe: config.instrument_universe,
        }
    }

    async fn bootstrap_instruments(&mut self) -> anyhow::Result<()> {
        let requested = self
            .instrument_universe
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        let filter = exact_instrument_filter(requested.iter().copied());
        let instruments = load_instruments(
            &self.http_client,
            self.instrument_store.as_deref(),
            Some(&filter),
        )
        .await?;
        validate_loaded_instruments(&requested, &instruments)?;
        let mut instruments_by_id = HashMap::with_capacity(instruments.len());
        let mut instrument_ids_by_token = HashMap::with_capacity(instruments.len());

        for instrument in instruments {
            let instrument_id = instrument.id();
            let token = u32::try_from(instrument_token(&instrument)?)?;
            instrument_ids_by_token.insert(token, instrument_id);
            instruments_by_id.insert(instrument_id, instrument);
        }

        for instrument in instruments_by_id.values() {
            if let Err(error) = self
                .data_sender
                .send(DataEvent::Data(instrument.clone().into()))
            {
                tracing::warn!(%error, instrument_id = %instrument.id(), "Failed to publish Kite instrument");
            }
        }

        self.instruments = Arc::new(instruments_by_id);
        self.instrument_ids_by_token = Arc::new(instrument_ids_by_token);

        Ok(())
    }

    fn spawn_ws_stream(
        &mut self,
        stream: UnboundedReceiverStream<KiteWsDataEvent>,
    ) -> anyhow::Result<()> {
        let instruments = Arc::clone(&self.instruments);
        let instrument_ids_by_token = Arc::clone(&self.instrument_ids_by_token);
        let subscriptions = Arc::clone(&self.subscriptions);
        let data_sender = self.data_sender.clone();

        self.ws_stream_task = Some(self.tasks.spawn_named("Kite data WebSocket", async move {
            if let Err(error) = run_ws_stream(
                stream,
                instruments,
                instrument_ids_by_token,
                subscriptions,
                data_sender,
            )
            .await
            {
                tracing::warn!(%error, "Kite data WebSocket stream failed");
            }
        })?);
        Ok(())
    }

    fn kite_index_token(&self, instrument_id: InstrumentId) -> anyhow::Result<u32> {
        let (instrument, token) = self.kite_instrument_token(instrument_id)?;
        if !matches!(instrument, InstrumentAny::IndexInstrument(_)) {
            return Err(CorrectnessError::PredicateViolation {
                message: format!("Kite instrument {instrument_id} is not an index"),
            }
            .into());
        }

        Ok(token)
    }

    fn kite_non_index_token(&self, instrument_id: InstrumentId) -> anyhow::Result<u32> {
        let (instrument, token) = self.kite_instrument_token(instrument_id)?;
        if matches!(instrument, InstrumentAny::IndexInstrument(_)) {
            return Err(CorrectnessError::PredicateViolation {
                message: format!("Kite instrument {instrument_id} is an index"),
            }
            .into());
        }

        Ok(token)
    }

    fn kite_instrument_token(
        &self,
        instrument_id: InstrumentId,
    ) -> anyhow::Result<(&InstrumentAny, u32)> {
        let instrument = self.instruments.get(&instrument_id).ok_or_else(|| {
            CorrectnessError::PredicateViolation {
                message: format!("unknown Kite instrument {instrument_id}"),
            }
        })?;

        Ok((instrument, u32::try_from(instrument_token(instrument)?)?))
    }

    fn update_market_data_subscription(
        &mut self,
        instrument_id: InstrumentId,
        update: impl FnOnce(&mut KiteMarketDataSelection),
    ) -> anyhow::Result<()> {
        let token = self.kite_non_index_token(instrument_id)?;
        let previous = self.subscriptions.get_cloned(&token).unwrap_or_default();
        let mut updated = previous;
        update(&mut updated);
        if updated == previous {
            return Ok(());
        }

        if updated.is_empty() {
            self.subscriptions.remove(&token);
        } else {
            self.subscriptions.insert(token, updated);
        }

        // Quotes, depth and open interest share one Kite Full subscription, so only the first
        // request and final cancellation need a venue command.
        let update_result = match (previous.is_empty(), updated.is_empty()) {
            (true, false) => self.subscribe_full(token),
            (false, true) => self.ws_client.unsubscribe(vec![token]),
            _ => Ok(()),
        };
        if let Err(error) = update_result {
            // Keep local filtering aligned when the venue command cannot be queued.
            if previous.is_empty() {
                self.subscriptions.remove(&token);
            } else {
                self.subscriptions.insert(token, previous);
            }
            return Err(error.into());
        }
        Ok(())
    }

    fn subscribe_full(&self, token: u32) -> Result<(), KiteWsError> {
        self.ws_client.subscribe(vec![token])?;
        self.ws_client
            .set_mode(KiteWebSocketMode::Full, vec![token])
    }

    fn ws_stream_is_running(&self) -> bool {
        self.ws_stream_task.as_ref().is_some_and(TaskRef::is_active)
    }

    async fn finish_tasks(&self) -> anyhow::Result<()> {
        self.tasks
            .finish_shutdown(CLIENT_TASK_GRACEFUL_TIMEOUT, CLIENT_TASK_ABORT_TIMEOUT)
            .await?;
        Ok(())
    }

    async fn prepare_tasks(&self) -> anyhow::Result<()> {
        if !self.tasks.is_open() {
            self.tasks.begin_shutdown();
            self.finish_tasks().await?;
            self.tasks.start_generation()?;
        }
        Ok(())
    }
}

fn unix_nanos_to_india_datetime(timestamp: UnixNanos) -> DateTime {
    timestamp
        .to_datetime_utc()
        .to_zoned(INDIA_TIME_ZONE.clone())
        .datetime()
}

fn open_interest_instrument_id(data_type: &DataType) -> anyhow::Result<InstrumentId> {
    let instrument_id = data_type.identifier().ok_or_else(|| {
        anyhow::anyhow!("{KITE_OPEN_INTEREST_TYPE} subscriptions require an identifier")
    })?;
    instrument_id.parse().map_err(|error| {
        anyhow::anyhow!("invalid Kite open interest instrument ID {instrument_id}: {error}")
    })
}

async fn run_ws_stream(
    mut stream: UnboundedReceiverStream<KiteWsDataEvent>,
    instruments: Arc<HashMap<InstrumentId, InstrumentAny>>,
    instrument_ids_by_token: Arc<HashMap<u32, InstrumentId>>,
    subscriptions: Arc<AtomicMap<u32, KiteMarketDataSelection>>,
    data_sender: EventSender<DataEvent>,
) -> anyhow::Result<()> {
    while let Some(event) = stream.next().await {
        match event {
            KiteWsDataEvent::MarketData { data, ts_init } => {
                for data in data {
                    process_market_data(
                        &data,
                        ts_init,
                        &instruments,
                        &instrument_ids_by_token,
                        &subscriptions,
                        &data_sender,
                    );
                }
            }
            KiteWsDataEvent::Reconnected => tracing::info!("Kite WebSocket reconnected"),
        }
    }
    Ok(())
}

fn process_market_data(
    data: &KiteMarketData,
    ts_init: UnixNanos,
    instruments: &HashMap<InstrumentId, InstrumentAny>,
    instrument_ids_by_token: &HashMap<u32, InstrumentId>,
    subscriptions: &AtomicMap<u32, KiteMarketDataSelection>,
    data_sender: &EventSender<DataEvent>,
) {
    let token = data.instrument_token();
    let Some(instrument_id) = instrument_ids_by_token.get(&token) else {
        tracing::warn!(
            instrument_token = token,
            "Ignoring market data for unknown Kite instrument"
        );
        return;
    };
    let Some(instrument) = instruments.get(instrument_id) else {
        tracing::warn!(%instrument_id, instrument_token = token, "Kite instrument lookup is inconsistent");
        return;
    };

    let selection = match instrument {
        InstrumentAny::IndexInstrument(_) => KiteMarketDataSelection::ALL,
        _ => match subscriptions.get_cloned(&token) {
            Some(selection) => selection,
            None => return,
        },
    };
    let result = normalize_selected_market_data(data, instrument, ts_init, selection);

    match result {
        Ok(updates) => {
            for update in updates {
                if let Err(error) = data_sender.send(DataEvent::Data(update)) {
                    tracing::warn!(%error, %instrument_id, "Failed to publish Kite market data");
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, %instrument_id, instrument_token = token, "Ignoring invalid Kite market data");
        }
    }
}

#[async_trait::async_trait(?Send)]
impl DataClient for KiteDataClient {
    fn client_id(&self) -> ClientId {
        self.client_id
    }

    fn venue(&self) -> Option<Venue> {
        None
    }

    fn start(&mut self) -> anyhow::Result<()> {
        tracing::info!(client_id = %self.client_id, "Starting Kite data client");
        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        tracing::info!(client_id = %self.client_id, "Stopping Kite data client");
        self.tasks.begin_shutdown();
        Ok(())
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        tracing::debug!(client_id = %self.client_id, "Resetting Kite data client");
        self.instruments = Arc::new(HashMap::new());
        self.instrument_ids_by_token = Arc::new(HashMap::new());
        self.subscriptions.store(Default::default());
        Ok(())
    }

    fn dispose(&mut self) -> anyhow::Result<()> {
        self.stop()
    }

    fn is_connected(&self) -> bool {
        self.is_connected && self.ws_stream_is_running()
    }

    fn is_disconnected(&self) -> bool {
        !self.is_connected()
    }

    fn subscribe(&mut self, cmd: SubscribeCustomData) -> anyhow::Result<()> {
        if cmd.data_type.type_name() != KITE_OPEN_INTEREST_TYPE {
            tracing::warn!(
                data_type = cmd.data_type.type_name(),
                "Unsupported Kite custom data subscription"
            );
            return Ok(());
        }

        let instrument_id = open_interest_instrument_id(&cmd.data_type)?;
        self.update_market_data_subscription(instrument_id, |subscriptions| {
            subscriptions.open_interest = true;
        })
    }

    fn subscribe_book_depth(&mut self, cmd: SubscribeBookDepth) -> anyhow::Result<()> {
        execute_instrument_subscriptions(&cmd.instrument_ids, |instrument_id| {
            self.update_market_data_subscription(instrument_id, |subscriptions| {
                subscriptions.depth = true;
            })
        })
    }

    fn subscribe_quotes(&mut self, cmd: SubscribeQuotes) -> anyhow::Result<()> {
        execute_instrument_subscriptions(&cmd.instrument_ids, |instrument_id| {
            self.update_market_data_subscription(instrument_id, |subscriptions| {
                subscriptions.quotes = true;
            })
        })
    }

    fn subscribe_index_prices(&mut self, cmd: SubscribeIndexPrices) -> anyhow::Result<()> {
        execute_instrument_subscriptions(&cmd.instrument_ids, |instrument_id| {
            let token = self.kite_index_token(instrument_id)?;
            Ok(self.subscribe_full(token)?)
        })
    }

    fn unsubscribe_book_depth(&mut self, cmd: &UnsubscribeBookDepth) -> anyhow::Result<()> {
        execute_instrument_subscriptions(&cmd.instrument_ids, |instrument_id| {
            self.update_market_data_subscription(instrument_id, |subscriptions| {
                subscriptions.depth = false;
            })
        })
    }

    fn unsubscribe(&mut self, cmd: &UnsubscribeCustomData) -> anyhow::Result<()> {
        if cmd.data_type.type_name() != KITE_OPEN_INTEREST_TYPE {
            tracing::warn!(
                data_type = cmd.data_type.type_name(),
                "Unsupported Kite custom data unsubscription"
            );
            return Ok(());
        }

        let instrument_id = open_interest_instrument_id(&cmd.data_type)?;
        self.update_market_data_subscription(instrument_id, |subscriptions| {
            subscriptions.open_interest = false;
        })
    }

    fn unsubscribe_quotes(&mut self, cmd: &UnsubscribeQuotes) -> anyhow::Result<()> {
        execute_instrument_subscriptions(&cmd.instrument_ids, |instrument_id| {
            self.update_market_data_subscription(instrument_id, |subscriptions| {
                subscriptions.quotes = false;
            })
        })
    }

    fn unsubscribe_index_prices(&mut self, cmd: &UnsubscribeIndexPrices) -> anyhow::Result<()> {
        execute_instrument_subscriptions(&cmd.instrument_ids, |instrument_id| {
            let token = self.kite_index_token(instrument_id)?;
            Ok(self.ws_client.unsubscribe(vec![token])?)
        })
    }

    fn request_instrument(&self, request: RequestInstrument) -> anyhow::Result<()> {
        let mut instruments = Vec::with_capacity(request.instrument_ids.len());
        let mut failures = Vec::new();
        for instrument_id in &request.instrument_ids {
            match self.instruments.get(instrument_id) {
                Some(instrument) => instruments.push(instrument.clone()),
                None => failures.push(format!("unknown Kite instrument {instrument_id}")),
            }
        }

        let client_id = request.client_id.unwrap_or(self.client_id);
        let start = datetime_to_unix_nanos(request.start);
        let end = datetime_to_unix_nanos(request.end);
        let ts_init = get_atomic_clock_realtime().get_time_ns();
        let response = if request.instrument_ids.len() == 1 && instruments.len() == 1 {
            instruments.into_iter().next().map(|instrument| {
                DataResponse::Instrument(Box::new(InstrumentResponse::new(
                    request.request_id,
                    client_id,
                    instrument.id(),
                    instrument,
                    start,
                    end,
                    ts_init,
                    request.params,
                )))
            })
        } else if request.instrument_ids.len() > 1 {
            Some(DataResponse::Instruments(InstrumentsResponse::new(
                request.request_id,
                client_id,
                Venue::from(KITE),
                instruments,
                start,
                end,
                ts_init,
                request.params,
            )))
        } else {
            None
        };
        let event = if failures.is_empty() {
            let response = response.ok_or_else(|| CorrectnessError::PredicateViolation {
                message: "Kite instrument request did not contain any instruments".to_owned(),
            })?;
            DataEvent::ResponseEnvelope(DataResponseEnvelope::new(response, true, None))
        } else {
            DataEvent::RequestFailed(DataRequestFailed {
                request_id: request.request_id,
                reason: failures.join("; "),
                response,
            })
        };
        self.data_sender.send(event)?;
        Ok(())
    }

    fn request_instruments(&self, request: RequestInstruments) -> anyhow::Result<()> {
        let requested_venue = request.venue;
        let instruments = self
            .instruments
            .values()
            .filter(|instrument| requested_venue.is_none_or(|venue| instrument.venue() == venue))
            .cloned()
            .collect();
        let response = DataResponse::Instruments(InstrumentsResponse::new(
            request.request_id,
            request.client_id.unwrap_or(self.client_id),
            requested_venue.unwrap_or_else(|| Venue::from(KITE)),
            instruments,
            datetime_to_unix_nanos(request.start),
            datetime_to_unix_nanos(request.end),
            get_atomic_clock_realtime().get_time_ns(),
            request.params,
        ));
        self.data_sender.send(DataEvent::Response(response))?;
        Ok(())
    }

    fn request_bars(&self, request: RequestBars) -> anyhow::Result<()> {
        let requests = request
            .bar_types
            .iter()
            .map(|bar_type| {
                candle_interval(*bar_type)?;
                let instrument_id = bar_type.instrument_id();
                let instrument =
                    self.instruments
                        .get(&instrument_id)
                        .cloned()
                        .ok_or_else(|| CorrectnessError::PredicateViolation {
                            message: format!("unknown Kite instrument {instrument_id}"),
                        })?;
                Ok((*bar_type, instrument))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let start = request
            .start
            .ok_or_else(|| CorrectnessError::PredicateViolation {
                message: "Kite historical bar requests require a start time".to_owned(),
            })?;
        let start_ns = datetime_to_unix_nanos(Some(start)).ok_or_else(|| {
            CorrectnessError::PredicateViolation {
                message: format!("Kite historical bar start time {start} is unsupported"),
            }
        })?;
        let end_ns = match request.end {
            Some(end) => datetime_to_unix_nanos(Some(end)).ok_or_else(|| {
                CorrectnessError::PredicateViolation {
                    message: format!("Kite historical bar end time {end} is unsupported"),
                }
            })?,
            None => get_atomic_clock_realtime().get_time_ns(),
        };
        let start = unix_nanos_to_india_datetime(start_ns);
        let end = unix_nanos_to_india_datetime(end_ns);
        let http_client = self.http_client.clone();
        let sender = self.data_sender.clone();
        let client_id = request.client_id.unwrap_or(self.client_id);
        let request_id = request.request_id;
        let params = request.params;
        let limit = request.limit;

        self.tasks.spawn(async move {
            let mut failures = Vec::new();
            for (bar_type, instrument) in requests {
                match http_client
                    .get_historical_candles(&instrument, bar_type, start, end, false, limit)
                    .await
                {
                    Ok(data) => {
                        let response = DataResponse::Bars(BarsResponse::new(
                            request_id,
                            client_id,
                            bar_type,
                            data.bars,
                            Some(start_ns),
                            Some(end_ns),
                            get_atomic_clock_realtime().get_time_ns(),
                            params.clone(),
                        ));
                        let envelope = DataResponseEnvelope::new(response, false, None);
                        if let Err(error) = sender.send(DataEvent::ResponseEnvelope(envelope)) {
                            tracing::warn!(%error, %bar_type, "Failed to publish Kite historical bars");
                            return;
                        }
                    }
                    Err(error) => {
                        tracing::error!(%error, %bar_type, "Kite historical bar request failed");
                        failures.push(format!("{bar_type}: {error}"));
                    }
                }
            }

            let event = if failures.is_empty() {
                DataEvent::ResponseEnvelope(DataResponseEnvelope::terminal(request_id, None))
            } else {
                DataEvent::RequestFailed(DataRequestFailed {
                    request_id,
                    reason: failures.join("; "),
                    response: None,
                })
            };
            if let Err(error) = sender.send(event) {
                tracing::warn!(%error, "Failed to complete Kite historical bar request");
            }
        })?;

        Ok(())
    }

    async fn connect(&mut self) -> anyhow::Result<()> {
        if self.is_connected && self.ws_stream_is_running() {
            return Ok(());
        }
        if self.is_connected {
            tracing::warn!("Cleaning up terminated Kite WebSocket stream before reconnecting");
            self.disconnect().await?;
        }
        self.prepare_tasks().await?;

        register_kite_custom_data();
        self.bootstrap_instruments().await?;
        let stream = self.ws_client.connect_data().await?;
        if let Err(error) = self.spawn_ws_stream(stream) {
            if let Err(close_error) = self.ws_client.disconnect_data().await {
                return Err(
                    error.context(format!("Kite WebSocket cleanup also failed: {close_error}"))
                );
            }
            return Err(error);
        }
        self.is_connected = true;
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        self.tasks.begin_shutdown();
        let close_result = self.ws_client.disconnect_data().await;
        let tasks_result = self.finish_tasks().await;
        self.ws_stream_task = None;
        self.instruments = Arc::new(HashMap::new());
        self.instrument_ids_by_token = Arc::new(HashMap::new());
        self.subscriptions.store(Default::default());
        self.is_connected = false;

        match (close_result, tasks_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error.into()),
            (Ok(()), Err(error)) => Err(error),
            (Err(close_error), Err(tasks_error)) => {
                Err(tasks_error.context(format!("Kite WebSocket close also failed: {close_error}")))
            }
        }
    }
}
