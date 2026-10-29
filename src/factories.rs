use std::cell::RefCell;
use std::rc::Rc;

use nautilus_common::cache::CacheView;
use nautilus_common::clients::{DataClient, ExecutionClient};
use nautilus_common::clock::Clock;
use nautilus_common::factories::{ClientConfig, DataClientFactory, ExecutionClientFactory};
use nautilus_live::ExecutionClientCore;
use nautilus_model::enums::{AccountType, OmsType};
use nautilus_model::identifiers::{ClientId, TraderId, Venue};
use nautilus_model::types::Currency;

use crate::common::consts::KITE;
use crate::config::{KiteDataClientConfig, KiteExecutionClientConfig};
use crate::data::KiteDataClient;
use crate::execution::client::KiteExecutionClient;

/// Creates the Kite live data client for a Nautilus live node.
#[derive(Debug)]
pub struct KiteDataClientFactory;

/// Creates the Kite live execution client for a Nautilus live node.
#[derive(Debug)]
pub struct KiteExecutionClientFactory;

impl DataClientFactory for KiteDataClientFactory {
    fn create(
        &self,
        name: &str,
        config: &dyn ClientConfig,
        _cache: CacheView,
        _clock: Rc<RefCell<dyn Clock>>,
    ) -> anyhow::Result<Box<dyn DataClient>> {
        let config = config
            .as_any()
            .downcast_ref::<KiteDataClientConfig>()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Invalid config type for KiteDataClientFactory. Expected KiteDataClientConfig, was {config:?}"
                )
            })?;
        let client = KiteDataClient::new(ClientId::from(name), config.clone());

        Ok(Box::new(client))
    }

    fn name(&self) -> &str {
        KITE
    }

    fn config_type(&self) -> &str {
        stringify!(KiteDataClientConfig)
    }
}

impl ExecutionClientFactory for KiteExecutionClientFactory {
    fn create(
        &self,
        trader_id: TraderId,
        name: &str,
        config: &dyn ClientConfig,
        cache: CacheView,
        _clock: Rc<RefCell<dyn Clock>>,
    ) -> anyhow::Result<Box<dyn ExecutionClient>> {
        let config = config
            .as_any()
            .downcast_ref::<KiteExecutionClientConfig>()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Invalid config type for KiteExecutionClientFactory. Expected KiteExecutionClientConfig, was {config:?}"
                )
            })?;
        let core = ExecutionClientCore::new(
            trader_id,
            ClientId::from(name),
            Venue::from(KITE),
            OmsType::Netting,
            config.account_id,
            AccountType::Margin,
            Some(Currency::INR()),
            cache,
        );
        Ok(Box::new(KiteExecutionClient::new(core, config.clone())))
    }

    fn name(&self) -> &str {
        KITE
    }

    fn config_type(&self) -> &str {
        stringify!(KiteExecutionClientConfig)
    }
}
