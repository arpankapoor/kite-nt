use std::sync::Arc;

use nautilus_live::task::SharedTaskSlot;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::credential::KiteSessionCredentials;
use crate::websocket::error::KiteWsError;
use crate::websocket::handler::{KiteWsCommand, KiteWsFeedHandler};
use crate::websocket::messages::{
    KiteWebSocketMode, KiteWsDataEvent, KiteWsExecutionEvent, KiteWsFrame,
};

/// Shared controller for Kite's market-data and order-update WebSocket.
///
/// Clones send commands to one background handler. The handler remains owned until the final
/// client clone is dropped.
#[derive(Clone, Debug)]
pub struct KiteWsClient {
    command_tx: tokio::sync::mpsc::UnboundedSender<KiteWsCommand>,
    _handler_task: Arc<SharedTaskSlot<()>>,
}

impl KiteWsClient {
    /// Starts a shared WebSocket client.
    ///
    /// When `frame_tx` is present, every raw frame is forwarded before decoding. Its receiver must
    /// be drained continuously because frame delivery completes before decoding and event routing.
    pub fn new(
        session_credentials: Arc<KiteSessionCredentials>,
        frame_tx: Option<flume::Sender<KiteWsFrame>>,
    ) -> Result<Self, KiteWsError> {
        let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
        let handler = KiteWsFeedHandler::new(session_credentials, frame_tx, command_rx);
        let handler_task = Arc::new(SharedTaskSlot::new());
        handler_task.spawn(handler.run())?;

        Ok(Self {
            command_tx,
            _handler_task: handler_task,
        })
    }

    /// Connects the market-data consumer and returns its event stream.
    pub async fn connect_data(
        &self,
    ) -> Result<UnboundedReceiverStream<KiteWsDataEvent>, KiteWsError> {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.send(KiteWsCommand::ConnectData(result_tx))?;
        let data_rx = result_rx.await??;
        Ok(UnboundedReceiverStream::new(data_rx))
    }

    /// Connects the execution consumer and returns its order-update stream.
    pub async fn connect_execution(
        &self,
    ) -> Result<UnboundedReceiverStream<KiteWsExecutionEvent>, KiteWsError> {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.send(KiteWsCommand::ConnectExecution(result_tx))?;
        let execution_rx = result_rx.await??;
        Ok(UnboundedReceiverStream::new(execution_rx))
    }

    /// Disconnects the market-data consumer.
    pub async fn disconnect_data(&self) -> Result<(), KiteWsError> {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.send(KiteWsCommand::DisconnectData(result_tx))?;
        result_rx.await?;
        Ok(())
    }

    /// Disconnects the execution consumer.
    pub async fn disconnect_execution(&self) -> Result<(), KiteWsError> {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.send(KiteWsCommand::DisconnectExecution(result_tx))?;
        result_rx.await?;
        Ok(())
    }

    /// Subscribes to the supplied Kite instrument tokens.
    pub fn subscribe(&self, tokens: Vec<u32>) -> Result<(), KiteWsError> {
        self.send(KiteWsCommand::Subscribe(tokens))
    }

    /// Unsubscribes from the supplied Kite instrument tokens.
    pub fn unsubscribe(&self, tokens: Vec<u32>) -> Result<(), KiteWsError> {
        self.send(KiteWsCommand::Unsubscribe(tokens))
    }

    /// Changes the WebSocket packet mode for the supplied tokens.
    pub fn set_mode(&self, mode: KiteWebSocketMode, tokens: Vec<u32>) -> Result<(), KiteWsError> {
        self.send(KiteWsCommand::SetMode { mode, tokens })
    }

    fn send(&self, command: KiteWsCommand) -> Result<(), KiteWsError> {
        self.command_tx
            .send(command)
            .map_err(|_| KiteWsError::HandlerStopped)
    }
}
