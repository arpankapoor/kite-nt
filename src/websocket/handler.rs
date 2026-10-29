use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;
use std::time::Duration;

use nautilus_core::UnixNanos;
use nautilus_core::time::get_atomic_clock_realtime;
use nautilus_network::Message;
use nautilus_network::http::Url;
use nautilus_network::websocket::{MessageHandler, WebSocketClient, WebSocketConfig};
use strum::VariantArray;

use crate::common::consts::{KITE_VERSION, KITE_VERSION_HEADER};
use crate::credential::KiteSessionCredentials;
use crate::urls::websocket_url;
use crate::websocket::error::KiteWsError;
use crate::websocket::messages::{
    KiteWebSocketMode, KiteWsDataEvent, KiteWsExecutionEvent, KiteWsFrame, KiteWsFrameKind,
    KiteWsMessage, KiteWsRequest,
};
use crate::websocket::parse::parse_frame;

const API_KEY_QUERY: &str = "api_key";
const ACCESS_TOKEN_QUERY: &str = "access_token";

const CONNECT_TIMEOUT_MS: u64 = 5_000;
const IDLE_TIMEOUT_MS: u64 = 15_000;
const RECONNECT_DELAY_INITIAL_MS: u64 = 500;
const RECONNECT_DELAY_MAX_MS: u64 = 5_000;
const RECONNECT_BACKOFF_FACTOR: f64 = 1.5;
const RECONNECT_JITTER_MS: u64 = 250;

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) type DataReceiver = tokio::sync::mpsc::UnboundedReceiver<KiteWsDataEvent>;
pub(crate) type ExecutionReceiver = tokio::sync::mpsc::UnboundedReceiver<KiteWsExecutionEvent>;

#[derive(Debug)]
pub(crate) enum KiteWsCommand {
    ConnectData(tokio::sync::oneshot::Sender<Result<DataReceiver, KiteWsError>>),
    ConnectExecution(tokio::sync::oneshot::Sender<Result<ExecutionReceiver, KiteWsError>>),
    DisconnectData(tokio::sync::oneshot::Sender<()>),
    DisconnectExecution(tokio::sync::oneshot::Sender<()>),
    Subscribe(Vec<u32>),
    Unsubscribe(Vec<u32>),
    SetMode {
        mode: KiteWebSocketMode,
        tokens: Vec<u32>,
    },
}

pub(crate) struct KiteWsFeedHandler {
    session_credentials: Arc<KiteSessionCredentials>,
    raw_frame_tx: Option<flume::Sender<KiteWsFrame>>,
    command_rx: tokio::sync::mpsc::UnboundedReceiver<KiteWsCommand>,
    client: Option<WebSocketClient>,
    raw_rx: Option<tokio::sync::mpsc::UnboundedReceiver<KiteWsFrame>>,
    data_tx: Option<tokio::sync::mpsc::UnboundedSender<KiteWsDataEvent>>,
    execution_tx: Option<tokio::sync::mpsc::UnboundedSender<KiteWsExecutionEvent>>,
    subscriptions: HashMap<u32, Option<KiteWebSocketMode>>,
}

impl KiteWsFeedHandler {
    pub(crate) fn new(
        session_credentials: Arc<KiteSessionCredentials>,
        raw_frame_tx: Option<flume::Sender<KiteWsFrame>>,
        command_rx: tokio::sync::mpsc::UnboundedReceiver<KiteWsCommand>,
    ) -> Self {
        Self {
            session_credentials,
            raw_frame_tx,
            command_rx,
            client: None,
            raw_rx: None,
            data_tx: None,
            execution_tx: None,
            subscriptions: HashMap::new(),
        }
    }

    pub(crate) async fn run(mut self) {
        loop {
            tokio::select! {
                command = self.command_rx.recv() => {
                    let Some(command) = command else {
                        break;
                    };
                    self.handle_command(command).await;
                }
                frame = receive_frame(&mut self.raw_rx), if self.raw_rx.is_some() => {
                    let Some(frame) = frame else {
                        self.close_feed().await;
                        continue;
                    };
                    self.handle_frame(frame).await;
                }
            }
        }

        self.close_feed().await;
    }

    async fn handle_command(&mut self, command: KiteWsCommand) {
        match command {
            KiteWsCommand::ConnectData(result_tx) => {
                let result = self.connect_data().await;
                let _ = result_tx.send(result);
            }
            KiteWsCommand::ConnectExecution(result_tx) => {
                let result = self.connect_execution().await;
                let _ = result_tx.send(result);
            }
            KiteWsCommand::DisconnectData(result_tx) => {
                self.disconnect_data().await;
                let _ = result_tx.send(());
            }
            KiteWsCommand::DisconnectExecution(result_tx) => {
                self.disconnect_execution().await;
                let _ = result_tx.send(());
            }
            KiteWsCommand::Subscribe(tokens) => self.subscribe(tokens).await,
            KiteWsCommand::Unsubscribe(tokens) => self.unsubscribe(tokens).await,
            KiteWsCommand::SetMode { mode, tokens } => self.set_mode(mode, tokens).await,
        }
    }

    async fn connect_data(&mut self) -> Result<DataReceiver, KiteWsError> {
        if self.data_tx.is_some() {
            return Err(KiteWsError::ConsumerAlreadyConnected("data"));
        }
        if self.client.is_none() {
            self.connect_transport().await?;
        }
        let (data_tx, data_rx) = tokio::sync::mpsc::unbounded_channel();
        self.data_tx = Some(data_tx);
        Ok(data_rx)
    }

    async fn connect_execution(&mut self) -> Result<ExecutionReceiver, KiteWsError> {
        if self.execution_tx.is_some() {
            return Err(KiteWsError::ConsumerAlreadyConnected("execution"));
        }
        if self.client.is_none() {
            self.connect_transport().await?;
        }
        let (execution_tx, execution_rx) = tokio::sync::mpsc::unbounded_channel();
        self.execution_tx = Some(execution_tx);
        Ok(execution_rx)
    }

    async fn disconnect_data(&mut self) {
        if self.data_tx.take().is_some() && self.execution_tx.is_some() {
            let tokens = self.subscriptions.keys().copied().collect();
            self.unsubscribe(tokens).await;
        }
        self.close_if_unused().await;
    }

    async fn disconnect_execution(&mut self) {
        self.execution_tx = None;
        self.close_if_unused().await;
    }

    async fn close_if_unused(&mut self) {
        if self.data_tx.is_none() && self.execution_tx.is_none() {
            self.close_transport().await;
        }
    }

    async fn connect_transport(&mut self) -> Result<(), KiteWsError> {
        let (message_handler, raw_rx) = create_raw_message_handler();
        let config = WebSocketConfig::builder()
            .url(self.connection_url().to_string())
            .headers(vec![(
                KITE_VERSION_HEADER.to_owned(),
                KITE_VERSION.to_owned(),
            )])
            .connect_timeout_ms(CONNECT_TIMEOUT_MS)
            .reconnect_delay_initial_ms(RECONNECT_DELAY_INITIAL_MS)
            .reconnect_delay_max_ms(RECONNECT_DELAY_MAX_MS)
            .reconnect_backoff_factor(RECONNECT_BACKOFF_FACTOR)
            .reconnect_jitter_ms(RECONNECT_JITTER_MS)
            .idle_timeout_ms(IDLE_TIMEOUT_MS)
            .build()?;
        let client = WebSocketClient::builder()
            .config(config)
            .message_handler(message_handler)
            .close_handler(Arc::new(|frame| {
                match frame {
                    Some(frame) => tracing::warn!(
                        code = frame.code,
                        "Kite WebSocket peer closed the connection"
                    ),
                    None => tracing::warn!(
                        "Kite WebSocket peer closed the connection without a close status"
                    ),
                }
                true
            }))
            .connect()
            .await?;
        self.client = Some(client);
        self.raw_rx = Some(raw_rx);
        self.replay_subscriptions().await;
        Ok(())
    }

    async fn close_feed(&mut self) {
        self.data_tx = None;
        self.execution_tx = None;
        self.close_transport().await;
    }

    async fn close_transport(&mut self) {
        self.raw_rx = None;
        if let Some(client) = self.client.take()
            && tokio::time::timeout(SHUTDOWN_TIMEOUT, client.disconnect())
                .await
                .is_err()
        {
            tracing::warn!("Kite WebSocket did not disconnect within the shutdown timeout");
        }
        self.subscriptions.clear();
    }

    async fn handle_frame(&mut self, frame: KiteWsFrame) {
        // Persist the source frame before publishing any events derived from it.
        if let Some(frame_tx) = &self.raw_frame_tx
            && let Err(error) = frame_tx.send_async(frame.clone()).await
        {
            tracing::warn!(%error, "Kite raw frame receiver stopped");
            self.raw_frame_tx = None;
        }
        let message = match parse_frame(&frame) {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(%error, "Preserving malformed Kite WebSocket frame");
                return;
            }
        };
        self.route_message(message, frame.ts_init).await;
    }

    async fn route_message(&mut self, message: KiteWsMessage, ts_init: UnixNanos) {
        match message {
            KiteWsMessage::MarketData(data) => {
                self.send_data(KiteWsDataEvent::MarketData { data, ts_init })
                    .await;
            }
            KiteWsMessage::OrderUpdate(update) => {
                self.send_execution(KiteWsExecutionEvent::OrderUpdate {
                    update: *update,
                    ts_init,
                })
                .await;
            }
            KiteWsMessage::Error(message) => {
                tracing::warn!(%message, "Kite WebSocket error");
            }
            KiteWsMessage::Message(message) => {
                tracing::info!(kitemsg = %message, "Kite WebSocket message");
            }
            KiteWsMessage::InstrumentsMeta { etag, count } => {
                tracing::debug!(%etag, count, "Received Kite instruments metadata");
            }
            KiteWsMessage::RawText(payload) => {
                tracing::debug!(
                    length = payload.len(),
                    "Ignoring unrecognized Kite WebSocket text message"
                );
            }
            KiteWsMessage::Reconnected => {
                self.replay_subscriptions().await;
                self.send_data(KiteWsDataEvent::Reconnected).await;
                self.send_execution(KiteWsExecutionEvent::Reconnected).await;
            }
        }
    }

    async fn send_data(&mut self, event: KiteWsDataEvent) {
        let failed = self
            .data_tx
            .as_ref()
            .is_some_and(|data_tx| data_tx.send(event).is_err());
        if failed {
            self.data_tx = None;
            let tokens = self.subscriptions.keys().copied().collect();
            self.unsubscribe(tokens).await;
            self.close_if_unused().await;
        }
    }

    async fn send_execution(&mut self, event: KiteWsExecutionEvent) {
        let failed = self
            .execution_tx
            .as_ref()
            .is_some_and(|execution_tx| execution_tx.send(event).is_err());
        if failed {
            self.execution_tx = None;
            self.close_if_unused().await;
        }
    }

    async fn subscribe(&mut self, tokens: Vec<u32>) {
        let tokens = tokens
            .into_iter()
            .filter(|token| match self.subscriptions.entry(*token) {
                Entry::Vacant(entry) => {
                    entry.insert(None);
                    true
                }
                Entry::Occupied(_) => false,
            })
            .collect::<Vec<_>>();
        if !tokens.is_empty() {
            self.send_request(KiteWsRequest::Subscribe(tokens)).await;
        }
    }

    async fn unsubscribe(&mut self, tokens: Vec<u32>) {
        let tokens = tokens
            .into_iter()
            .filter(|token| self.subscriptions.remove(token).is_some())
            .collect::<Vec<_>>();
        if !tokens.is_empty() {
            self.send_request(KiteWsRequest::Unsubscribe(tokens)).await;
        }
    }

    async fn set_mode(&mut self, mode: KiteWebSocketMode, tokens: Vec<u32>) {
        let tokens = tokens
            .into_iter()
            .filter(|token| {
                let Some(configured_mode) = self.subscriptions.get_mut(token) else {
                    return false;
                };
                if *configured_mode == Some(mode) {
                    return false;
                }
                *configured_mode = Some(mode);
                true
            })
            .collect::<Vec<_>>();
        if !tokens.is_empty() {
            self.send_request(KiteWsRequest::Mode(mode, tokens)).await;
        }
    }

    async fn replay_subscriptions(&self) {
        if self.subscriptions.is_empty() || self.client.is_none() {
            return;
        }

        if !self
            .send_request(KiteWsRequest::Subscribe(
                self.subscriptions.keys().copied().collect(),
            ))
            .await
        {
            return;
        }

        for &mode in KiteWebSocketMode::VARIANTS {
            let tokens = self
                .subscriptions
                .iter()
                .filter_map(|(token, configured_mode)| {
                    (*configured_mode == Some(mode)).then_some(*token)
                })
                .collect::<Vec<_>>();
            if !tokens.is_empty() {
                self.send_request(KiteWsRequest::Mode(mode, tokens)).await;
            }
        }
    }

    async fn send_request(&self, request: KiteWsRequest) -> bool {
        let Some(client) = &self.client else {
            return false;
        };
        let payload = match serde_json::to_string(&request) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(%error, "Failed to serialize Kite WebSocket request");
                return false;
            }
        };
        if let Err(error) = client.send_text(payload, None).await {
            tracing::warn!(%error, "Failed to send Kite WebSocket request");
            return false;
        }
        true
    }

    fn connection_url(&self) -> Url {
        let mut url = websocket_url();
        url.query_pairs_mut()
            .append_pair(
                API_KEY_QUERY,
                self.session_credentials.api_key().expose_secret(),
            )
            .append_pair(
                ACCESS_TOKEN_QUERY,
                self.session_credentials.access_token().expose_secret(),
            );
        url
    }
}

fn create_raw_message_handler() -> (
    MessageHandler,
    tokio::sync::mpsc::UnboundedReceiver<KiteWsFrame>,
) {
    let (message_tx, message_rx) = tokio::sync::mpsc::unbounded_channel();
    let message_handler = Arc::new(move |message| {
        let ts_init = get_atomic_clock_realtime().get_time_ns();
        let (kind, payload) = match message {
            Message::Binary(payload) => (KiteWsFrameKind::Binary, payload),
            Message::Text(payload) => (KiteWsFrameKind::Text, payload),
            Message::Ping(_) | Message::Pong(_) | Message::Close(_) => return,
        };
        if message_tx
            .send(KiteWsFrame {
                ts_init,
                kind,
                payload,
            })
            .is_err()
        {
            tracing::debug!("Kite WebSocket message receiver has closed");
        }
    });
    (message_handler, message_rx)
}

async fn receive_frame(
    raw_rx: &mut Option<tokio::sync::mpsc::UnboundedReceiver<KiteWsFrame>>,
) -> Option<KiteWsFrame> {
    raw_rx.as_mut()?.recv().await
}
