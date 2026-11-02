use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nautilus_common::cache::Cache;
use nautilus_common::clients::ExecutionClient;
use nautilus_common::live::sender::EventSender;
use nautilus_common::live::{get_data_event_sender, get_exec_event_sender};
use nautilus_common::messages::DataEvent;
use nautilus_common::messages::execution::{
    BatchCancelOrders, CancelAllOrders, CancelOrder, GenerateFillReports,
    GenerateOrderStatusReport, GenerateOrderStatusReports, GeneratePositionStatusReports,
    ModifyOrder, QueryAccount, QueryOrder, SubmitOrder, SubmitOrderList,
};
use nautilus_core::time::get_atomic_clock_realtime;
use nautilus_core::{Params, UnixNanos};
use nautilus_live::execution::failure::CommandFailure;
use nautilus_live::task::{TaskGroup, TaskRef};
use nautilus_live::{ExecutionClientCore, ExecutionEventEmitter};
use nautilus_model::accounts::AccountAny;
use nautilus_model::enums::{OmsType, PositionSide};
use nautilus_model::identifiers::{AccountId, ClientId, InstrumentId, Venue, VenueOrderId};
use nautilus_model::instruments::{Instrument, InstrumentAny};
use nautilus_model::orders::{Order, OrderAny};
use nautilus_model::reports::{FillReport, OrderStatusReport, PositionStatusReport};
use nautilus_model::types::{AccountBalance, MarginBalance, Quantity};
use nautilus_network::retry::RetryError;
use rust_decimal::Decimal;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::common::consts::{
    CLIENT_TASK_ABORT_TIMEOUT, CLIENT_TASK_GRACEFUL_TIMEOUT, VENUE_BFO, VENUE_BSE, VENUE_NFO,
    VENUE_NSE,
};
use crate::common::parse::parse_instrument_id;
use crate::config::KiteExecutionClientConfig;
use crate::execution::charges::fill_commission;
use crate::execution::models::{KiteOrderRecord, KiteOrderUpdate, KiteOrderVariety};
use crate::execution::parse::{
    parse_account_state, parse_fill_report, parse_holding_status_report, parse_kite_product,
    parse_modify_order_request, parse_order_status_report, parse_order_update_report,
    parse_place_order_request, parse_position_status_report,
};
use crate::http::client::KiteHttpClient;
use crate::http::error::{KiteException, KiteHttpError};
use crate::http::models::{KiteHolding, KitePositions, KiteUserMargins};
use crate::instruments::{
    KiteInstrumentStore, exact_instrument_filter, load_instruments, validate_loaded_instruments,
};
use crate::websocket::client::KiteWsClient;
use crate::websocket::messages::KiteWsExecutionEvent;

const ACCOUNT_REGISTRATION_TIMEOUT: Duration = Duration::from_secs(30);
const ACCOUNT_REGISTRATION_POLL_INTERVAL: Duration = Duration::from_millis(10);
const ORDER_TRADES_RETRY_DELAY: Duration = Duration::from_millis(250);

pub(crate) struct KiteExecutionClient {
    core: ExecutionClientCore,
    emitter: ExecutionEventEmitter,
    data_sender: EventSender<DataEvent>,
    http_client: KiteHttpClient,
    ws_client: KiteWsClient,
    tasks: TaskGroup,
    ws_stream_task: Option<TaskRef>,
    instruments: Arc<HashMap<InstrumentId, InstrumentAny>>,
    instrument_store: Option<Arc<dyn KiteInstrumentStore>>,
    instrument_universe: Vec<InstrumentId>,
}

impl KiteExecutionClient {
    pub(crate) fn new(core: ExecutionClientCore, config: KiteExecutionClientConfig) -> Self {
        let emitter = ExecutionEventEmitter::new(
            get_atomic_clock_realtime(),
            core.trader_id,
            core.account_id,
            core.account_type,
            core.base_currency,
        );
        Self {
            core,
            emitter,
            data_sender: get_data_event_sender(),
            http_client: config.http_client,
            ws_client: config.ws_client,
            tasks: TaskGroup::new(),
            ws_stream_task: None,
            instruments: Arc::new(HashMap::new()),
            instrument_store: config.instrument_store,
            instrument_universe: config.instrument_universe,
        }
    }

    async fn load_execution_instruments(
        &mut self,
        positions: &KitePositions,
        holdings: &[KiteHolding],
        orders: &[KiteOrderRecord],
    ) -> anyhow::Result<()> {
        let configured = self.instrument_universe.len();
        let open_positions = positions
            .net
            .iter()
            .filter(|position| position.quantity != 0)
            .count();
        let mut instrument_universe = self
            .instrument_universe
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        for position in positions
            .net
            .iter()
            .filter(|position| position.quantity != 0)
        {
            instrument_universe.insert(parse_instrument_id(
                &position.exchange,
                &position.trading_symbol,
            )?);
        }
        for holding in holdings {
            instrument_universe.insert(parse_instrument_id(
                &holding.exchange,
                &holding.trading_symbol,
            )?);
        }
        for order in orders {
            instrument_universe
                .insert(parse_instrument_id(&order.exchange, &order.trading_symbol)?);
        }

        let requested = instrument_universe.len();
        let filter = exact_instrument_filter(instrument_universe.iter().copied());
        let instruments = load_instruments(
            &self.http_client,
            self.instrument_store.as_deref(),
            Some(&filter),
        )
        .await?;
        validate_loaded_instruments(&instrument_universe, &instruments)?;
        let loaded = instruments.len();
        let instruments: Arc<HashMap<InstrumentId, InstrumentAny>> = Arc::new(
            instruments
                .into_iter()
                .map(|instrument| (instrument.id(), instrument))
                .collect(),
        );
        for instrument in instruments.values() {
            self.data_sender
                .send(DataEvent::Data(instrument.clone().into()))?;
        }
        self.instruments = instruments;
        self.core.set_instruments_initialized();
        tracing::info!(
            configured,
            holdings = holdings.len(),
            orders = orders.len(),
            open_positions,
            requested,
            loaded,
            "Loaded Kite execution instruments",
        );
        Ok(())
    }

    fn spawn_ws_stream(
        &mut self,
        stream: UnboundedReceiverStream<KiteWsExecutionEvent>,
    ) -> anyhow::Result<()> {
        let future = run_ws_stream(
            stream,
            self.http_client.clone(),
            Arc::clone(&self.instruments),
            self.core.account_id,
            self.emitter.clone(),
        );
        self.ws_stream_task = Some(self.tasks.spawn_named(
            "Kite execution WebSocket",
            async move {
                if let Err(error) = future.await {
                    tracing::warn!(%error, "Kite execution WebSocket stream failed");
                }
            },
        )?);
        Ok(())
    }

    fn ws_stream_is_running(&self) -> bool {
        self.ws_stream_task.as_ref().is_some_and(TaskRef::is_active)
    }

    fn spawn_command<F>(&self, description: &'static str, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if let Err(error) = self.tasks.spawn(future) {
            tracing::warn!(%error, "Skipping Kite {description} after shutdown began");
        }
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

    async fn await_account_registered(&self) -> anyhow::Result<()> {
        let account_id = self.core.account_id;
        tokio::time::timeout(ACCOUNT_REGISTRATION_TIMEOUT, async {
            while self.core.cache().account(&account_id).is_none() {
                tokio::time::sleep(ACCOUNT_REGISTRATION_POLL_INTERVAL).await;
            }
        })
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "timed out waiting for Kite account {account_id} to be registered in the cache"
            )
        })
    }

    fn dispatch_order_submit(&self, order: OrderAny, params: Option<&Params>, ts_init: UnixNanos) {
        if order.is_closed() {
            tracing::warn!(client_order_id = %order.client_order_id(), "Cannot submit closed Kite order");
            return;
        }

        let instrument_id = order.instrument_id();
        let Some(instrument) = self.instruments.get(&instrument_id) else {
            self.emitter.emit_order_denied(
                &order,
                &format!("Kite execution instrument {instrument_id} is not loaded"),
            );
            return;
        };
        let request = match parse_kite_product(params).and_then(|product| {
            parse_place_order_request(&order, instrument, product, None, ts_init)
        }) {
            Ok(request) => request,
            Err(error) => {
                self.emitter.emit_order_denied(&order, &error.to_string());
                return;
            }
        };

        self.emitter.emit_order_submitted(&order);
        let http_client = self.http_client.clone();
        let emitter = self.emitter.clone();
        self.spawn_command("order submission", async move {
            match http_client
                .place_order(KiteOrderVariety::Regular, &request)
                .await
            {
                Ok(data) if data.children.is_empty() => {
                    match VenueOrderId::new_checked(&data.order_id) {
                        Ok(venue_order_id) => emitter.emit_order_accepted(
                            &order,
                            venue_order_id,
                            get_atomic_clock_realtime().get_time_ns(),
                        ),
                        Err(error) => tracing::error!(
                            %error,
                            client_order_id = %order.client_order_id(),
                            "Kite accepted an order with an invalid order ID; outcome remains unresolved",
                        ),
                    }
                }
                Ok(data) => tracing::error!(
                    client_order_id = %order.client_order_id(),
                    children = data.children.len(),
                    "Kite unexpectedly returned autoslice results for a regular order; outcome remains unresolved",
                ),
                Err(error) => match classify_command_failure(&error) {
                    CommandFailure::NotSent(reason)
                    | CommandFailure::VenueRejected(reason) => emitter.emit_order_rejected(
                        &order,
                        &reason,
                        get_atomic_clock_realtime().get_time_ns(),
                        false,
                    ),
                    CommandFailure::Ambiguous(_) => tracing::warn!(
                        %error,
                        client_order_id = %order.client_order_id(),
                        "Kite order submission outcome is unknown; leaving order in flight",
                    ),
                },
            }
        });
    }

    fn dispatch_order_cancel(&self, order: OrderAny, venue_order_id: VenueOrderId) {
        let http_client = self.http_client.clone();
        let emitter = self.emitter.clone();
        self.spawn_command("order cancellation", async move {
            match http_client
                .cancel_order(KiteOrderVariety::Regular, venue_order_id.as_str(), None)
                .await
            {
                Ok(returned_order_id) if returned_order_id == venue_order_id.as_str() => {
                    tracing::debug!(
                        %venue_order_id,
                        client_order_id = %order.client_order_id(),
                        "Kite accepted order cancellation",
                    );
                }
                Ok(returned_order_id) => tracing::warn!(
                    %venue_order_id,
                    %returned_order_id,
                    client_order_id = %order.client_order_id(),
                    "Kite cancellation returned an unexpected order ID; outcome remains unresolved",
                ),
                Err(error) => match classify_command_failure(&error) {
                    CommandFailure::NotSent(reason) | CommandFailure::VenueRejected(reason) => {
                        emitter.emit_order_cancel_rejected(
                            &order,
                            Some(venue_order_id),
                            &reason,
                            get_atomic_clock_realtime().get_time_ns(),
                        );
                    }
                    CommandFailure::Ambiguous(_) => tracing::warn!(
                        %error,
                        %venue_order_id,
                        client_order_id = %order.client_order_id(),
                        "Kite order cancellation outcome is unknown; leaving cancellation pending",
                    ),
                },
            }
        });
    }
}

#[async_trait(?Send)]
impl ExecutionClient for KiteExecutionClient {
    fn is_connected(&self) -> bool {
        self.core.is_connected()
    }

    fn client_id(&self) -> ClientId {
        self.core.client_id
    }

    fn account_id(&self) -> AccountId {
        self.core.account_id
    }

    fn venue(&self) -> Venue {
        self.core.venue
    }

    fn oms_type(&self) -> OmsType {
        self.core.oms_type
    }

    fn get_account(&self) -> Option<AccountAny> {
        self.core.cache().account_owned(&self.core.account_id)
    }

    fn retain_unresolved_submissions(&self) -> bool {
        true
    }

    fn handles_order_venue(&self, venue: Venue) -> bool {
        matches!(
            venue.as_str(),
            VENUE_NSE | VENUE_BSE | VENUE_NFO | VENUE_BFO
        )
    }

    fn generate_account_state(
        &self,
        balances: Vec<AccountBalance>,
        margins: Vec<MarginBalance>,
        reported: bool,
        ts_event: UnixNanos,
        info: Option<Params>,
    ) -> anyhow::Result<()> {
        self.emitter
            .emit_account_state(balances, margins, reported, ts_event, info);
        Ok(())
    }

    fn start(&mut self) -> anyhow::Result<()> {
        if self.core.is_started() {
            return Ok(());
        }

        let sender = get_exec_event_sender();
        self.emitter.set_sender(sender);
        self.core.set_started();

        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        if self.core.is_stopped() {
            return Ok(());
        }

        self.tasks.begin_shutdown();
        self.core.set_stopped();
        self.core.set_disconnected();

        Ok(())
    }

    async fn connect(&mut self) -> anyhow::Result<()> {
        if self.core.is_connected() && self.ws_stream_is_running() {
            return Ok(());
        }
        if self.core.is_connected() {
            tracing::warn!("Cleaning up terminated Kite execution stream before reconnecting");
            self.disconnect().await?;
        }
        self.prepare_tasks().await?;

        let (margins, positions, holdings, orders) = tokio::try_join!(
            self.http_client.get_user_margins(),
            self.http_client.get_positions(),
            self.http_client.get_holdings(),
            self.http_client.get_orders(),
        )?;
        self.load_execution_instruments(&positions, &holdings, &orders)
            .await?;
        emit_account_state(&margins, &self.emitter)?;
        self.await_account_registered().await?;
        let stream = self.ws_client.connect_execution().await?;
        if let Err(error) = self.spawn_ws_stream(stream) {
            self.ws_client.disconnect_execution().await?;
            return Err(error);
        }
        self.core.set_connected();
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        self.tasks.begin_shutdown();
        let close_result = self.ws_client.disconnect_execution().await;
        let tasks_result = self.finish_tasks().await;
        self.ws_stream_task = None;
        self.core.set_disconnected();

        match (close_result, tasks_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error.into()),
            (Ok(()), Err(error)) => Err(error),
            (Err(close_error), Err(tasks_error)) => {
                Err(tasks_error.context(format!("Kite WebSocket close also failed: {close_error}")))
            }
        }
    }

    fn submit_order(&self, cmd: SubmitOrder) -> anyhow::Result<()> {
        let order = self.core.cache().try_order_owned(&cmd.client_order_id)?;
        self.dispatch_order_submit(order, cmd.params.as_ref(), cmd.ts_init);
        Ok(())
    }

    fn submit_order_list(&self, cmd: SubmitOrderList) -> anyhow::Result<()> {
        for order in self.core.get_orders_for_list(&cmd.order_list)? {
            self.dispatch_order_submit(order, cmd.params.as_ref(), cmd.ts_init);
        }
        Ok(())
    }

    fn cancel_order(&self, cmd: CancelOrder) -> anyhow::Result<()> {
        let order = self.core.cache().try_order_owned(&cmd.client_order_id)?;
        if order.is_closed() {
            tracing::warn!(client_order_id = %cmd.client_order_id, "Cannot cancel closed Kite order");
            return Ok(());
        }

        let Some(venue_order_id) = cmd.venue_order_id.or_else(|| order.venue_order_id()) else {
            self.emitter.emit_order_cancel_rejected(
                &order,
                None,
                "Kite cancellation requires a venue order ID",
                get_atomic_clock_realtime().get_time_ns(),
            );
            return Ok(());
        };

        self.dispatch_order_cancel(order, venue_order_id);
        Ok(())
    }

    fn cancel_all_orders(&self, cmd: CancelAllOrders) -> anyhow::Result<()> {
        let cache = self.core.cache();
        let orders = cache.orders_open(
            Some(&self.core.venue),
            Some(&cmd.instrument_id),
            Some(&cmd.strategy_id),
            Some(&self.core.account_id),
            cmd.order_side,
        );

        for order in orders {
            let Some(venue_order_id) = order.venue_order_id() else {
                tracing::warn!(
                    client_order_id = %order.client_order_id(),
                    "Skipping Kite cancellation because the order has no venue order ID",
                );
                continue;
            };
            self.dispatch_order_cancel(order.cloned(), venue_order_id);
        }
        Ok(())
    }

    fn batch_cancel_orders(&self, cmd: BatchCancelOrders) -> anyhow::Result<()> {
        let cache = self.core.cache();
        for cancel in cmd.cancels {
            let Some(order) = cache.order_owned(&cancel.client_order_id) else {
                tracing::warn!(
                    client_order_id = %cancel.client_order_id,
                    "Skipping Kite cancellation because the order is absent from the cache",
                );
                continue;
            };
            if order.is_closed() {
                tracing::warn!(
                    client_order_id = %cancel.client_order_id,
                    "Cannot cancel closed Kite order",
                );
                continue;
            }

            let Some(venue_order_id) = cancel.venue_order_id.or_else(|| order.venue_order_id())
            else {
                self.emitter.emit_order_cancel_rejected(
                    &order,
                    None,
                    "Kite cancellation requires a venue order ID",
                    get_atomic_clock_realtime().get_time_ns(),
                );
                continue;
            };
            self.dispatch_order_cancel(order, venue_order_id);
        }
        Ok(())
    }

    fn query_account(&self, _cmd: QueryAccount) -> anyhow::Result<()> {
        let http_client = self.http_client.clone();
        let emitter = self.emitter.clone();
        self.spawn_command("account query", async move {
            if let Err(error) = refresh_account_state(&http_client, &emitter).await {
                tracing::error!(%error, "Failed to refresh Kite account state");
            }
        });
        Ok(())
    }

    fn query_order(&self, cmd: QueryOrder) -> anyhow::Result<()> {
        let venue_order_id = cmd.venue_order_id.or_else(|| {
            self.core
                .cache()
                .venue_order_id(&cmd.client_order_id)
                .copied()
        });
        let Some(venue_order_id) = venue_order_id else {
            tracing::warn!(
                client_order_id = %cmd.client_order_id,
                "Cannot query Kite order without a venue order ID",
            );
            return Ok(());
        };
        let Some(instrument) = self.instruments.get(&cmd.instrument_id).cloned() else {
            tracing::warn!(
                instrument_id = %cmd.instrument_id,
                "Cannot query Kite order because its instrument is not loaded",
            );
            return Ok(());
        };

        let http_client = self.http_client.clone();
        let account_id = self.core.account_id;
        let emitter = self.emitter.clone();
        self.spawn_command("order query", async move {
            let history = match http_client.get_order_history(venue_order_id.as_str()).await {
                Ok(history) => history,
                Err(error) => {
                    tracing::error!(%error, %venue_order_id, "Failed to query Kite order");
                    return;
                }
            };
            let report = match history
                .iter()
                .map(|order| {
                    parse_order_status_report(
                        order,
                        &instrument,
                        account_id,
                        Some(cmd.client_order_id),
                        cmd.ts_init,
                    )
                })
                .collect::<Result<Vec<_>, _>>()
                .map(|reports| reports.into_iter().max_by_key(|report| report.ts_last))
            {
                Ok(report) => report,
                Err(error) => {
                    tracing::error!(%error, %venue_order_id, "Failed to parse Kite order history");
                    return;
                }
            };
            let Some(report) = report.filter(|report| {
                report.instrument_id == cmd.instrument_id
                    && report.client_order_id == Some(cmd.client_order_id)
                    && report.venue_order_id == venue_order_id
            }) else {
                tracing::warn!(
                    %venue_order_id,
                    client_order_id = %cmd.client_order_id,
                    "Kite order history did not match the query",
                );
                return;
            };
            emitter.send_order_status_report(report);
        });
        Ok(())
    }

    fn modify_order(&self, cmd: ModifyOrder) -> anyhow::Result<()> {
        let order = self.core.cache().try_order_owned(&cmd.client_order_id)?;
        if order.is_closed() {
            tracing::warn!(client_order_id = %cmd.client_order_id, "Cannot modify closed Kite order");
            return Ok(());
        }

        let Some(venue_order_id) = cmd.venue_order_id.or_else(|| order.venue_order_id()) else {
            self.emitter.emit_order_modify_rejected(
                &order,
                None,
                "Kite modification requires a venue order ID",
                get_atomic_clock_realtime().get_time_ns(),
            );
            return Ok(());
        };
        let Some(instrument) = self.instruments.get(&cmd.instrument_id) else {
            self.emitter.emit_order_modify_rejected(
                &order,
                Some(venue_order_id),
                &format!(
                    "Kite execution instrument {} is not loaded",
                    cmd.instrument_id
                ),
                get_atomic_clock_realtime().get_time_ns(),
            );
            return Ok(());
        };
        let request = match parse_modify_order_request(&cmd, instrument) {
            Ok(request) => request,
            Err(error) => {
                self.emitter.emit_order_modify_rejected(
                    &order,
                    Some(venue_order_id),
                    &error.to_string(),
                    get_atomic_clock_realtime().get_time_ns(),
                );
                return Ok(());
            }
        };

        let http_client = self.http_client.clone();
        let emitter = self.emitter.clone();
        self.spawn_command("order modification", async move {
            match http_client
                .modify_order(KiteOrderVariety::Regular, venue_order_id.as_str(), &request)
                .await
            {
                Ok(returned_order_id) if returned_order_id == venue_order_id.as_str() => {
                    tracing::debug!(
                        %venue_order_id,
                        client_order_id = %order.client_order_id(),
                        "Kite accepted order modification",
                    );
                }
                Ok(returned_order_id) => tracing::warn!(
                    %venue_order_id,
                    %returned_order_id,
                    client_order_id = %order.client_order_id(),
                    "Kite modification returned an unexpected order ID; outcome remains unresolved",
                ),
                Err(error) => match classify_command_failure(&error) {
                    CommandFailure::NotSent(reason) | CommandFailure::VenueRejected(reason) => {
                        emitter.emit_order_modify_rejected(
                            &order,
                            Some(venue_order_id),
                            &reason,
                            get_atomic_clock_realtime().get_time_ns(),
                        );
                    }
                    CommandFailure::Ambiguous(_) => tracing::warn!(
                        %error,
                        %venue_order_id,
                        client_order_id = %order.client_order_id(),
                        "Kite order modification outcome is unknown; leaving modification pending",
                    ),
                },
            }
        });
        Ok(())
    }

    async fn generate_order_status_report(
        &self,
        cmd: &GenerateOrderStatusReport,
    ) -> anyhow::Result<Option<OrderStatusReport>> {
        let venue_order_id = match (cmd.venue_order_id, cmd.client_order_id) {
            (Some(venue_order_id), _) => venue_order_id,
            (None, Some(client_order_id)) => {
                let Some(venue_order_id) =
                    self.core.cache().venue_order_id(&client_order_id).copied()
                else {
                    return Ok(None);
                };
                venue_order_id
            }
            (None, None) => anyhow::bail!(
                "Kite order status report requires a client or venue order identifier"
            ),
        };
        let history = self
            .http_client
            .get_order_history(venue_order_id.as_str())
            .await?;
        let cache = self.core.cache();
        let report = history
            .into_iter()
            .map(|order| {
                order_status_report(
                    &self.instruments,
                    &cache,
                    &order,
                    self.core.account_id,
                    cmd.ts_init,
                )
            })
            .collect::<anyhow::Result<Vec<_>>>()?
            .into_iter()
            .max_by_key(|report| report.ts_last)
            .filter(|report| {
                cmd.instrument_id
                    .is_none_or(|instrument_id| report.instrument_id == instrument_id)
                    && cmd.client_order_id.is_none_or(|client_order_id| {
                        report.client_order_id == Some(client_order_id)
                    })
                    && cmd
                        .venue_order_id
                        .is_none_or(|venue_order_id| report.venue_order_id == venue_order_id)
            });

        Ok(report)
    }

    async fn generate_order_status_reports(
        &self,
        cmd: &GenerateOrderStatusReports,
    ) -> anyhow::Result<Vec<OrderStatusReport>> {
        let orders = self.http_client.get_orders().await?;
        let cache = self.core.cache();
        orders
            .into_iter()
            .map(|order| {
                order_status_report(
                    &self.instruments,
                    &cache,
                    &order,
                    self.core.account_id,
                    cmd.ts_init,
                )
            })
            .collect::<anyhow::Result<Vec<_>>>()
            .map(|mut reports| {
                reports.retain(|report| {
                    let matches_instrument = cmd
                        .instrument_id
                        .is_none_or(|instrument_id| report.instrument_id == instrument_id);
                    let is_active =
                        report.order_status.is_open() || report.order_status.is_inflight();
                    let matches_open_filter = if cmd.open_only { is_active } else { true };
                    let matches_time_range = if report.order_status.is_closed() {
                        cmd.start.is_none_or(|start| report.ts_last >= start)
                            && cmd.end.is_none_or(|end| report.ts_last <= end)
                    } else {
                        true
                    };

                    matches_instrument && matches_open_filter && matches_time_range
                });
                reports
            })
    }

    async fn generate_fill_reports(
        &self,
        cmd: GenerateFillReports,
    ) -> anyhow::Result<Vec<FillReport>> {
        let mut trades = self.http_client.get_trades().await?;
        trades.sort_unstable_by_key(|trade| trade.exchange_timestamp);
        let cache = self.core.cache();
        let mut turnover_by_order = HashMap::<String, Decimal>::new();
        let mut reports = Vec::with_capacity(trades.len());

        for trade in trades {
            let instrument =
                instruments_get(&self.instruments, &trade.exchange, &trade.trading_symbol)?;
            let venue_order_id = VenueOrderId::new_checked(&trade.order_id)?;
            let turnover = turnover_by_order.entry(trade.order_id.clone()).or_default();
            let fill_turnover = trade.average_price * Decimal::from(trade.quantity);
            let commission = fill_commission(
                instrument,
                trade.product,
                trade.transaction_type,
                *turnover,
                fill_turnover,
            )?;
            *turnover += fill_turnover;
            reports.push(parse_fill_report(
                &trade,
                instrument,
                self.core.account_id,
                cache.client_order_id(&venue_order_id).copied(),
                commission,
                cmd.ts_init,
            )?);
        }

        reports.retain(|report| {
            cmd.instrument_id
                .is_none_or(|instrument_id| report.instrument_id == instrument_id)
                && cmd
                    .venue_order_id
                    .is_none_or(|venue_order_id| report.venue_order_id == venue_order_id)
                && cmd.start.is_none_or(|start| report.ts_event >= start)
                && cmd.end.is_none_or(|end| report.ts_event <= end)
        });
        Ok(reports)
    }

    async fn generate_position_status_reports(
        &self,
        cmd: &GeneratePositionStatusReports,
    ) -> anyhow::Result<Vec<PositionStatusReport>> {
        let (positions, holdings) = tokio::try_join!(
            self.http_client.get_positions(),
            self.http_client.get_holdings(),
        )?;
        let mut reports = HashMap::<InstrumentId, PositionStatusReport>::new();

        for holding in holdings {
            let instrument = instruments_get(
                &self.instruments,
                &holding.exchange,
                &holding.trading_symbol,
            )?;
            let report = parse_holding_status_report(
                &holding,
                instrument,
                self.core.account_id,
                cmd.ts_init,
            )?;
            merge_position_report(&mut reports, report)?;
        }
        for position in positions.net {
            if position.quantity == 0 {
                continue;
            }
            let instrument = instruments_get(
                &self.instruments,
                &position.exchange,
                &position.trading_symbol,
            )?;
            let report = parse_position_status_report(
                &position,
                instrument,
                self.core.account_id,
                cmd.ts_init,
            )?;
            merge_position_report(&mut reports, report)?;
        }

        let cache = self.core.cache();
        for position in cache.positions_open_refs(
            None,
            cmd.instrument_id.as_ref(),
            None,
            Some(&self.core.account_id),
            None,
        ) {
            if reports.contains_key(&position.instrument_id) {
                continue;
            }

            // Kite omits closed positions, so absence from its complete snapshot means flat.
            reports.insert(
                position.instrument_id,
                PositionStatusReport::new(
                    self.core.account_id,
                    position.instrument_id,
                    PositionSide::Flat,
                    Quantity::zero(position.size_precision),
                    cmd.ts_init,
                    cmd.ts_init,
                    None,
                    None,
                    None,
                ),
            );
        }

        Ok(reports
            .into_values()
            .filter(|report| {
                cmd.instrument_id
                    .is_none_or(|instrument_id| report.instrument_id == instrument_id)
            })
            .collect())
    }
}

fn merge_position_report(
    reports: &mut HashMap<InstrumentId, PositionStatusReport>,
    report: PositionStatusReport,
) -> anyhow::Result<()> {
    let Some(existing) = reports.get_mut(&report.instrument_id) else {
        reports.insert(report.instrument_id, report);
        return Ok(());
    };
    let signed_quantity = existing.signed_decimal_qty + report.signed_decimal_qty;
    let position_side = if signed_quantity.is_sign_positive() {
        PositionSide::Long
    } else if signed_quantity.is_sign_negative() {
        PositionSide::Short
    } else {
        PositionSide::Flat
    };
    let quantity = Quantity::from_decimal_dp(
        signed_quantity.abs(),
        existing.quantity.precision.max(report.quantity.precision),
    )?;

    // Kite separates holdings and product-specific positions, while Nautilus netting expects one
    // exposure per instrument. A combined average is not reliable across products or sides.
    *existing = PositionStatusReport::new(
        existing.account_id,
        existing.instrument_id,
        position_side,
        quantity,
        existing.ts_last.max(report.ts_last),
        existing.ts_init.max(report.ts_init),
        None,
        None,
        None,
    );
    Ok(())
}

fn emit_account_state(
    margins: &KiteUserMargins,
    emitter: &ExecutionEventEmitter,
) -> anyhow::Result<()> {
    let (balance, margin) = parse_account_state(margins)?;
    emitter.try_emit_account_state(
        vec![balance],
        vec![margin],
        true,
        get_atomic_clock_realtime().get_time_ns(),
        None,
    )
}

fn classify_command_failure(error: &KiteHttpError) -> CommandFailure {
    match error {
        KiteHttpError::Form(_) | KiteHttpError::Correctness(_) => {
            CommandFailure::not_sent(error.to_string())
        }
        KiteHttpError::Retry(RetryError::InvalidConfiguration { .. }) => {
            CommandFailure::not_sent(error.to_string())
        }
        KiteHttpError::Api {
            status,
            error_type,
            message,
        } if *status < 500 && *status != 429 && *error_type != KiteException::Network => {
            CommandFailure::venue_rejected(message.clone())
        }
        _ => CommandFailure::ambiguous(error.to_string()),
    }
}

async fn refresh_account_state(
    http_client: &KiteHttpClient,
    emitter: &ExecutionEventEmitter,
) -> anyhow::Result<()> {
    let margins = http_client.get_user_margins().await?;
    emit_account_state(&margins, emitter)
}

async fn process_order_update(
    update: &KiteOrderUpdate,
    ts_init: UnixNanos,
    http_client: &KiteHttpClient,
    instruments: &HashMap<InstrumentId, InstrumentAny>,
    account_id: AccountId,
    emitter: &ExecutionEventEmitter,
) {
    let instrument_id = match parse_instrument_id(&update.exchange, &update.trading_symbol) {
        Ok(instrument_id) => instrument_id,
        Err(error) => {
            tracing::warn!(%error, order_id = %update.order_id, "Invalid Kite order update instrument");
            return;
        }
    };
    let Some(instrument) = instruments.get(&instrument_id) else {
        tracing::warn!(%instrument_id, order_id = %update.order_id, "Kite order update instrument is not loaded");
        return;
    };

    let report = match parse_order_update_report(update, instrument, account_id, None, ts_init) {
        Ok(report) => report,
        Err(error) => {
            tracing::warn!(%error, ?update, "Failed to parse Kite order update");
            return;
        }
    };
    if update.filled_quantity == 0 {
        emitter.send_order_status_report(report);
        return;
    }

    let trades = http_client.get_order_trades(&update.order_id).await;
    let trades = match trades {
        Ok(trades) if trades.is_empty() => {
            // Kite can deliver the WebSocket update before the order book reflects it:
            // https://kite.trade/forum/discussion/14512/doubts-about-order-update-postbacks-over-websocket
            tokio::time::sleep(ORDER_TRADES_RETRY_DELAY).await;
            http_client.get_order_trades(&update.order_id).await
        }
        result => result,
    };
    let mut trades = match trades {
        Ok(trades) => trades,
        Err(error) => {
            tracing::warn!(%error, order_id = %update.order_id, "Failed to fetch Kite order trades; deferring filled order update for reconciliation");
            return;
        }
    };
    trades.sort_unstable_by_key(|trade| trade.exchange_timestamp);

    let mut turnover = Decimal::ZERO;
    let mut fills = Vec::with_capacity(trades.len());
    for trade in trades {
        let fill_turnover = trade.average_price * Decimal::from(trade.quantity);
        let commission = match fill_commission(
            instrument,
            trade.product,
            trade.transaction_type,
            turnover,
            fill_turnover,
        ) {
            Ok(commission) => commission,
            Err(error) => {
                tracing::warn!(%error, order_id = %update.order_id, "Failed to calculate Kite fill commission; deferring filled order update for reconciliation");
                return;
            }
        };
        turnover += fill_turnover;
        match parse_fill_report(&trade, instrument, account_id, None, commission, ts_init) {
            Ok(fill) => fills.push(fill),
            Err(error) => {
                tracing::warn!(%error, order_id = %update.order_id, trade_id = %trade.trade_id, "Failed to parse Kite fill; deferring filled order update for reconciliation");
                return;
            }
        }
    }

    if fills.is_empty() {
        tracing::warn!(
            order_id = %update.order_id,
            filled_quantity = update.filled_quantity,
            "Kite order update reported fills but the order trades endpoint returned none; deferring filled order update for reconciliation",
        );
    } else {
        emitter.send_order_with_fills(report, fills);
    }
}

async fn run_ws_stream(
    mut stream: UnboundedReceiverStream<KiteWsExecutionEvent>,
    http_client: KiteHttpClient,
    instruments: Arc<HashMap<InstrumentId, InstrumentAny>>,
    account_id: AccountId,
    emitter: ExecutionEventEmitter,
) -> anyhow::Result<()> {
    while let Some(event) = stream.next().await {
        match event {
            KiteWsExecutionEvent::OrderUpdate { update, ts_init } => {
                process_order_update(
                    &update,
                    ts_init,
                    &http_client,
                    &instruments,
                    account_id,
                    &emitter,
                )
                .await;
            }
            KiteWsExecutionEvent::Message(_) => {}
            KiteWsExecutionEvent::Reconnected => {
                tracing::info!("Kite execution WebSocket reconnected");
            }
        }
        if let Err(error) = refresh_account_state(&http_client, &emitter).await {
            tracing::warn!(%error, "Failed to refresh Kite account state after WebSocket event");
        }
    }
    Ok(())
}

fn order_status_report(
    instruments: &HashMap<InstrumentId, InstrumentAny>,
    cache: &Cache,
    order: &KiteOrderRecord,
    account_id: AccountId,
    ts_init: UnixNanos,
) -> anyhow::Result<OrderStatusReport> {
    let instrument = instruments_get(instruments, &order.exchange, &order.trading_symbol)?;
    let venue_order_id = VenueOrderId::new_checked(&order.order_id)?;
    Ok(parse_order_status_report(
        order,
        instrument,
        account_id,
        cache.client_order_id(&venue_order_id).copied(),
        ts_init,
    )?)
}

fn instruments_get<'a>(
    instruments: &'a HashMap<InstrumentId, InstrumentAny>,
    exchange: &str,
    trading_symbol: &str,
) -> anyhow::Result<&'a InstrumentAny> {
    let instrument_id = parse_instrument_id(exchange, trading_symbol)?;
    instruments
        .get(&instrument_id)
        .ok_or_else(|| anyhow::anyhow!("instrument not loaded: {instrument_id}"))
}
