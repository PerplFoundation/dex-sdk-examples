use crate::{
    Result,
    strategies::{bbo::BboStrategy, spread::SpreadStrategy, taker::TakerStrategy},
};
use alloy::providers::DynProvider;
use dex_sdk::{
    abi::dex::Exchange::ExchangeInstance,
    error::DexError,
    state::{Exchange, StateEvents},
    types::PerpetualId,
};
use tokio::sync::{OwnedSemaphorePermit, mpsc};

pub mod bbo;
pub mod spread;
pub mod taker;

pub trait Strategy {
    fn name(&self) -> &'static str;

    fn perpetual_id(&self) -> PerpetualId;

    /// Run the initial setup for the strategy
    fn initialize(
        &mut self,
        instance: &ExchangeInstance<DynProvider>,
        exchange: &Exchange,
    ) -> impl Future<Output = Result<()>>;

    /// Execute the strategy logic based on the current exchange state and block events
    fn execute(
        &mut self,
        instance: &ExchangeInstance<DynProvider>,
        exchange: &Exchange,
        events: &[StateEvents],
        error_tx: &mpsc::Sender<DexError>,
        permit: OwnedSemaphorePermit,
    ) -> impl Future<Output = ()>;
}

#[derive(Debug)]
pub enum StrategyType {
    Bbo(BboStrategy),
    Spread(SpreadStrategy),
    Taker(TakerStrategy),
}

impl Strategy for StrategyType {
    fn name(&self) -> &'static str {
        match self {
            StrategyType::Bbo(strategy) => strategy.name(),
            StrategyType::Spread(strategy) => strategy.name(),
            StrategyType::Taker(strategy) => strategy.name(),
        }
    }

    fn perpetual_id(&self) -> PerpetualId {
        match self {
            StrategyType::Bbo(strategy) => strategy.perpetual_id(),
            StrategyType::Spread(strategy) => strategy.perpetual_id(),
            StrategyType::Taker(strategy) => strategy.perpetual_id(),
        }
    }

    async fn initialize(
        &mut self,
        instance: &ExchangeInstance<DynProvider>,
        exchange: &Exchange,
    ) -> Result<()> {
        match self {
            StrategyType::Bbo(strategy) => strategy.initialize(instance, exchange).await,
            StrategyType::Spread(strategy) => strategy.initialize(instance, exchange).await,
            StrategyType::Taker(strategy) => strategy.initialize(instance, exchange).await,
        }
    }

    async fn execute(
        &mut self,
        instance: &ExchangeInstance<DynProvider>,
        exchange: &Exchange,
        events: &[StateEvents],
        error_tx: &mpsc::Sender<DexError>,
        permit: OwnedSemaphorePermit,
    ) {
        match self {
            StrategyType::Bbo(strategy) => {
                strategy
                    .execute(instance, exchange, events, error_tx, permit)
                    .await
            }
            StrategyType::Spread(strategy) => {
                strategy
                    .execute(instance, exchange, events, error_tx, permit)
                    .await
            }
            StrategyType::Taker(strategy) => {
                strategy
                    .execute(instance, exchange, events, error_tx, permit)
                    .await
            }
        }
    }
}
