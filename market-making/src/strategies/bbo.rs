use crate::{Result, error::Error, strategies::Strategy};
use alloy::providers::DynProvider;
use fastnum::{UD64, udec64};
use perpl_sdk::{
    abi::dex::Exchange::{ExchangeInstance, OrderDesc},
    error::DexError,
    state::{Exchange, Order, OrderEventType, StateEvents},
    types::{AccountId, OrderRequest, PerpetualId, RequestType},
};
use std::sync::OnceLock;
use tokio::sync::{OwnedSemaphorePermit, mpsc};
use tracing::{debug, error, info};

/// A simple market making strategy that places fixed size orders at the best bid and offer
#[derive(Debug)]
pub struct BboStrategy {
    /// The size of each order
    pub order_size: UD64,
    /// The perpetual ID to trade on
    pub perpetual_id: PerpetualId,
    /// Account ID
    pub account_id: OnceLock<AccountId>,
}

impl Strategy for BboStrategy {
    fn name(&self) -> &'static str {
        "BBO"
    }

    fn perpetual_id(&self) -> PerpetualId {
        self.perpetual_id
    }

    /// On initialization this strategy cancels all existing orders
    async fn initialize(
        &mut self,
        instance: &ExchangeInstance<DynProvider>,
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

        // Cancel all existing orders
        let order_descs = self.cancel_all_orders(exchange);
        if order_descs.is_empty() {
            return Ok(());
        }

        let builder = instance.execOpsAndOrders(vec![], order_descs, true);
        debug!(?builder, "Submitting cancel all orders transaction");
        let res = builder.send().await?;
        let tx = res.get_receipt().await?;
        debug!(?tx, "Cancel all orders transaction complete");
        info!(%account_id, "BBO Strategy initialized");

        Ok(())
    }

    async fn execute(
        &mut self,
        instance: &ExchangeInstance<DynProvider>,
        exchange: &Exchange,
        events: &[StateEvents],
        error_tx: &mpsc::Sender<DexError>,
        permit: OwnedSemaphorePermit,
    ) {
        if self.account_id.get().is_none() {
            panic!("Strategy not initialized");
        }

        if events.is_empty() {
            // This strategy only acts when there are block events
            return;
        }

        let mut fill_event_found = false;
        for event in events {
            let StateEvents::Order(order_event) = event else {
                continue;
            };

            if matches!(order_event.r#type, OrderEventType::Filled { .. }) {
                fill_event_found = true;
                break;
            }
        }

        if !fill_event_found {
            // No fills, no action
            return;
        }

        let (best_bid, best_ask) = self.get_bbo(exchange);
        let Some(best_bid) = best_bid else {
            info!("No best bid available, skipping order placement");
            return;
        };

        let Some(best_ask) = best_ask else {
            info!("No best ask available, skipping order placement");
            return;
        };

        let open_orders = self.fetch_open_orders(exchange);

        // This strategy only places one bid and one ask order at a time
        let bid = open_orders
            .iter()
            .find(|o| o.r#type().side() == perpl_sdk::types::OrderSide::Bid);

        let mut order_descs = Vec::new();

        if let Some(bid) = bid {
            if bid.price() < best_bid {
                let update_bid = self.update_order(exchange, bid, best_bid);
                order_descs.push(update_bid);
            }
        } else {
            let place_bid = self.place_order(exchange, RequestType::OpenLong, best_bid);
            order_descs.push(place_bid);
        }

        let ask = open_orders
            .iter()
            .find(|o| o.r#type().side() == perpl_sdk::types::OrderSide::Ask);

        if let Some(ask) = ask {
            if ask.price() > best_ask {
                let update_ask = self.update_order(exchange, ask, best_ask);
                order_descs.push(update_ask);
            }
        } else {
            let place_ask = self.place_order(exchange, RequestType::OpenShort, best_ask);
            order_descs.push(place_ask);
        }

        let builder = instance.execOpsAndOrders(vec![], order_descs, true);

        debug!(?builder, "Submitting initial bbo orders transaction");

        match builder.send().await.map_err(DexError::from) {
            Ok(res) => {
                let error_tx = error_tx.clone();
                tokio::spawn(async move {
                    match res.get_receipt().await.map_err(DexError::from) {
                        Ok(tx) => {
                            debug!(?tx, "Bbo orders transaction complete");
                        }
                        Err(error) => {
                            error!(%error, "Error executing bbo orders transaction");
                            error_tx
                                .send(error)
                                .await
                                .expect("Failed to send error to channel");
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

impl BboStrategy {
    pub fn new(order_size: UD64, perpetual_id: PerpetualId) -> Self {
        Self {
            order_size,
            perpetual_id,
            account_id: OnceLock::new(),
        }
    }

    fn cancel_all_orders(&self, exchange: &Exchange) -> Vec<OrderDesc> {
        let open_orders = self.fetch_open_orders(exchange);
        let mut order_descs = vec![];

        for order in open_orders {
            info!(order_id = %order.order_id(), "Cancelling order");
            let request = OrderRequest::new(
                0,
                self.perpetual_id,
                RequestType::Cancel,
                Some(order.order_id()),
                udec64!(0),
                udec64!(0),
                None,
                false,
                false,
                false,
                None,
                udec64!(0),
                None,
                None,
            );

            let request = request.prepare(exchange);
            order_descs.push(request);
        }

        order_descs
    }

    fn fetch_open_orders<'a>(&self, exchange: &'a Exchange) -> Vec<&'a Order> {
        let Some(account_id) = self.account_id.get() else {
            panic!("Strategy not initialized");
        };

        exchange
            .perpetuals()
            .get(&self.perpetual_id)
            .unwrap()
            .l3_book()
            .all_orders()
            .values()
            .filter(|o| o.account_id() == *account_id)
            .map(|o| &*(*o))
            .collect()
    }

    fn get_bbo(&self, exchange: &Exchange) -> (Option<UD64>, Option<UD64>) {
        let perpetual = exchange
            .perpetuals()
            .get(&self.perpetual_id)
            .expect("perpetual must exist");

        let best_bid_price = perpetual.l3_book().best_bid().map(|(price, _)| price);
        let best_ask_price = perpetual.l3_book().best_ask().map(|(price, _)| price);
        (best_bid_price, best_ask_price)
    }

    fn place_order(&self, exchange: &Exchange, order_type: RequestType, price: UD64) -> OrderDesc {
        let request = OrderRequest::new(
            0,
            self.perpetual_id,
            order_type,
            None,
            price,
            self.order_size,
            None,
            // post_only since we want to provide liquidity not take it
            true,
            false,
            false,
            None,
            // No leverage
            UD64::ONE,
            None,
            None,
        );

        request.prepare(exchange)
    }

    fn update_order(&self, exchange: &Exchange, order: &Order, price: UD64) -> OrderDesc {
        let request = OrderRequest::new(
            0,
            self.perpetual_id,
            RequestType::Change,
            Some(order.order_id()),
            price,
            self.order_size,
            None,
            // post_only since we want to provide liquidity not take it
            true,
            false,
            false,
            None,
            // No leverage
            UD64::ONE,
            None,
            None,
        );

        request.prepare(exchange)
    }
}
