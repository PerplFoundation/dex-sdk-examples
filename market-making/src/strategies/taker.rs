use std::sync::OnceLock;

use crate::{Result, error::Error, strategies::Strategy};
use alloy::providers::DynProvider;
use dex_sdk::{
    abi::dex::Exchange::{ExchangeInstance, OrderDesc},
    error::DexError,
    state::{Exchange, Position, PositionType, StateEvents},
    types::{AccountId, OrderRequest, PerpetualId, RequestType},
};
use fastnum::UD64;
use rand::{
    Rng,
    distr::{Bernoulli, Distribution, OpenClosed01},
};
use tokio::sync::{OwnedSemaphorePermit, mpsc};
use tracing::{debug, error, info};

/// A simple taker strategy that buys and sells over and over.
#[derive(Debug)]
pub struct TakerStrategy {
    /// Leverage for orders
    pub leverage: UD64,
    /// Account ID
    pub account_id: OnceLock<AccountId>,
    /// The perpetual ID to trade on
    pub perpetual_id: PerpetualId,
    /// Max order size
    pub max_order_size: UD64,
    /// Operation distribution
    op_distribution: rand::distr::Bernoulli,
}

impl Strategy for TakerStrategy {
    fn name(&self) -> &'static str {
        "Taker"
    }

    fn perpetual_id(&self) -> PerpetualId {
        self.perpetual_id
    }

    async fn initialize(
        &mut self,
        _instance: &ExchangeInstance<DynProvider>,
        exchange: &Exchange,
    ) -> Result<()> {
        let accounts = exchange.accounts();
        if accounts.is_empty() {
            return Err(Error::NoAccountFoundForStrategy);
        }

        if accounts.len() > 1 {
            return Err(Error::TooManyAccountsForStrategy);
        }

        let account_id = *accounts.keys().next().unwrap();

        self.account_id
            .set(account_id)
            .map_err(|_| Error::AccountIdAlreadySet)?;

        if !exchange.perpetuals().contains_key(&self.perpetual_id) {
            return Err(Error::PerpetualNotFoundInExchangeState(self.perpetual_id));
        }

        info!(%account_id, "Taker Strategy initialized");

        Ok(())
    }

    async fn execute(
        &mut self,
        instance: &ExchangeInstance<DynProvider>,
        exchange: &Exchange,
        _events: &[StateEvents],
        error_tx: &mpsc::Sender<DexError>,
        permit: OwnedSemaphorePermit,
    ) {
        let position = self.get_position(exchange);
        let size_multiplier = rand::rng().sample::<f64, _>(OpenClosed01);
        let size =
            UD64::from_f64(size_multiplier).expect("failed to parse UD64") * self.max_order_size;

        let long = self.op_distribution.sample(&mut rand::rng());
        let mut order_descs = Vec::new();

        if long {
            if let Some(pos) = position
                && pos.r#type() == PositionType::Short
            {
                // Close short position
                order_descs.push(self.place_order(exchange, RequestType::CloseLong, pos.size()));
            }

            order_descs.push(self.place_order(exchange, RequestType::OpenLong, size));
        } else {
            if let Some(pos) = position
                && pos.r#type() == PositionType::Long
            {
                // Close long position
                order_descs.push(self.place_order(exchange, RequestType::CloseShort, pos.size()));
            }

            order_descs.push(self.place_order(exchange, RequestType::OpenShort, size));
        }

        let builder = instance.execOpsAndOrders(vec![], order_descs, false);

        match builder.send().await.map_err(DexError::from) {
            Ok(res) => {
                let error_tx = error_tx.clone();
                tokio::spawn(async move {
                    match res.get_receipt().await.map_err(DexError::from) {
                        Ok(tx) => {
                            debug!(?tx, "Taker orders transaction complete");
                        }
                        Err(error) => {
                            error!(%error, "Error executing taker orders transaction");
                            error_tx.send(error).await.expect("Failed to send error");
                        }
                    }

                    drop(permit);
                });
            }
            Err(error) => {
                error!(%error, "Error sending transaction");
                error_tx.send(error).await.expect("Failed to send error");
            }
        }
    }
}

impl TakerStrategy {
    /// Create a new TakerStrategy
    pub fn new(max_order_size: UD64, leverage: UD64, perpetual_id: PerpetualId) -> Self {
        Self {
            leverage,
            perpetual_id,
            account_id: OnceLock::new(),
            max_order_size,
            op_distribution: Bernoulli::new(0.5).unwrap(),
        }
    }

    fn get_position<'a>(&self, exchange: &'a Exchange) -> Option<&'a Position> {
        let Some(account_id) = self.account_id.get() else {
            panic!("Strategy not initialized");
        };

        let account = exchange
            .accounts()
            .get(account_id)
            .expect("Account should exist in exchange state");

        account.positions().get(&self.perpetual_id)
    }

    fn place_order(&self, exchange: &Exchange, order_type: RequestType, size: UD64) -> OrderDesc {
        let price = match order_type {
            RequestType::OpenLong => UD64::MAX,
            _ => UD64::ZERO,
        };

        let request = OrderRequest::new(
            0,
            self.perpetual_id,
            order_type,
            None,
            price,
            size,
            None,
            false,
            false,
            // immediate or cancel
            true,
            None,
            self.leverage,
            None,
            None,
        );

        request.prepare(exchange)
    }
}
