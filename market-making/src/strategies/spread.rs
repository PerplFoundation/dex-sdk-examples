use crate::{Result, error::Error, strategies::Strategy};
use alloy::providers::DynProvider;
use fastnum::{UD64, udec64};
use perpl_sdk::{
    abi::dex::Exchange::{ExchangeInstance, OrderDesc},
    error::DexError,
    state::{Exchange, Order, StateEvents},
    types::{AccountId, OrderRequest, OrderType, PerpetualId, RequestType},
};
use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
};
use tokio::sync::{OwnedSemaphorePermit, mpsc};
use tracing::{debug, error, info, trace};

/// A market making strategy that places orders of fixed sizes above and below the mark price
#[derive(Debug)]
pub struct SpreadStrategy {
    /// Number of orders to place on each side of the spread
    pub orders_per_side: usize,
    /// The size of each order
    pub order_size: UD64,
    /// The perpetual ID to trade on
    pub perpetual_id: PerpetualId,
    /// Max matches per order
    pub max_matches_per_order: Option<u32>,
    /// Leverage for orders
    pub leverage: UD64,
    /// Account ID
    pub account_id: OnceLock<AccountId>,
    /// Current mark price
    pub current_mark_price: UD64,
}

impl Strategy for SpreadStrategy {
    fn name(&self) -> &'static str {
        "Spread"
    }

    fn perpetual_id(&self) -> PerpetualId {
        self.perpetual_id
    }

    /// On initialization this strategy cancels all existing orders
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

        info!(%account_id, "Spread Strategy initialized");

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
        let book = exchange
            .perpetuals()
            .get(&self.perpetual_id)
            .unwrap()
            .l3_book();

        info!("{:#?}", book);

        let Some(_account_id) = self.account_id.get().copied() else {
            panic!("Strategy not initialized");
        };

        let order_descs = self.process_orders(exchange);
        if order_descs.is_empty() {
            return;
        }

        let builder = instance.execOpsAndOrders(vec![], order_descs, false);

        trace!(?builder, "Submitting initial spread orders transaction");

        match builder.send().await.map_err(DexError::from) {
            Ok(res) => {
                let error_tx = error_tx.clone();
                tokio::spawn(async move {
                    match res.get_receipt().await.map_err(DexError::from) {
                        Ok(tx) => {
                            debug!(?tx, "Spread orders transaction complete");
                        }
                        Err(error) => {
                            error!(%error, "Error executing spread orders transaction");
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

impl SpreadStrategy {
    pub fn new(
        orders_per_side: usize,
        order_size: UD64,
        perpetual_id: PerpetualId,
        max_matches_per_order: Option<u32>,
        leverage: UD64,
    ) -> Self {
        Self {
            orders_per_side,
            order_size,
            perpetual_id,
            max_matches_per_order,
            leverage,
            account_id: OnceLock::new(),
            current_mark_price: UD64::ZERO,
        }
    }

    /// Process existing orders and determine necessary actions to maintain the spread
    fn process_orders(&mut self, exchange: &Exchange) -> Vec<OrderDesc> {
        let new_mark_price = self.get_mark_price(exchange);
        let bids_first = new_mark_price <= self.current_mark_price;

        self.current_mark_price = new_mark_price;

        info!(mark_price = %self.current_mark_price, "Mark price");

        let open_orders = self.fetch_open_orders(exchange);

        let mut current_bids: HashMap<UD64, &Order> = open_orders
            .iter()
            .filter(|o| o.r#type() == OrderType::OpenLong)
            .map(|o| (o.price(), *o))
            .collect();

        let mut current_asks: HashMap<UD64, &Order> = open_orders
            .iter()
            .filter(|o| o.r#type() == OrderType::OpenShort)
            .map(|o| (o.price(), *o))
            .collect();

        let mut target_bid_prices = HashSet::new();
        let mut target_ask_prices = HashSet::new();

        for i in 1..=self.orders_per_side {
            let spread_offset = UD64::from(i) / udec64!(500); // e.g., 0.2% per order away from mark price

            let bid_price = (UD64::ONE - spread_offset) * self.current_mark_price;
            let ask_price = (UD64::ONE + spread_offset) * self.current_mark_price;

            target_bid_prices.insert(bid_price);
            target_ask_prices.insert(ask_price);
        }

        let mut bid_descs = self.create_target_order_changes(
            exchange,
            &mut current_bids,
            target_bid_prices,
            RequestType::OpenLong,
        );

        let mut ask_descs = self.create_target_order_changes(
            exchange,
            &mut current_asks,
            target_ask_prices,
            RequestType::OpenShort,
        );

        if bids_first {
            bid_descs.append(&mut ask_descs);
            bid_descs
        } else {
            ask_descs.append(&mut bid_descs);
            ask_descs
        }
    }

    fn create_target_order_changes(
        &self,
        exchange: &Exchange,
        current: &mut HashMap<UD64, &Order>,
        target_prices: HashSet<UD64>,
        request_type: RequestType,
    ) -> Vec<OrderDesc> {
        let mut order_descs = Vec::new();
        let mut remaining = Vec::new();

        for price in target_prices.into_iter() {
            if let Some(existing_order) = current.remove(&price) {
                if existing_order.size() != self.order_size {
                    // Update order size
                    let update_desc =
                        self.update_order(exchange, existing_order, price, self.order_size);
                    order_descs.push(update_desc);
                } else {
                    // Order exists with correct size, do nothing
                    continue;
                }
            } else {
                remaining.push(price);
            }
        }

        // If there are any remaining orders we can update existing ones
        if !remaining.is_empty() {
            for existing in current.values() {
                let Some(price) = remaining.pop() else {
                    break;
                };

                let update_desc = self.update_order(exchange, existing, price, self.order_size);
                order_descs.push(update_desc);
            }
        }

        // Place new orders for any remaining
        if !remaining.is_empty() {
            // Place new orders for any remaining bids
            for price in remaining {
                let order_desc = self.place_order(exchange, request_type, price, self.order_size);
                order_descs.push(order_desc);
            }
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

    fn get_mark_price(&self, exchange: &Exchange) -> UD64 {
        let perpetual = exchange
            .perpetuals()
            .get(&self.perpetual_id)
            .expect("perpetual must exist");

        perpetual.mark_price()
    }

    fn place_order(
        &self,
        exchange: &Exchange,
        order_type: RequestType,
        price: UD64,
        size: UD64,
    ) -> OrderDesc {
        info!(?order_type, %price, %size, "Placing order");
        let request = OrderRequest::new(
            0,
            self.perpetual_id,
            order_type,
            None,
            price,
            size,
            None,
            // post_only since we want to provide liquidity not take it
            true,
            false,
            false,
            self.max_matches_per_order,
            self.leverage,
            None,
            None,
        );

        request.prepare(exchange)
    }

    fn update_order(
        &self,
        exchange: &Exchange,
        order: &Order,
        price: UD64,
        size: UD64,
    ) -> OrderDesc {
        info!(order_id = order.order_id(), %price, %size, "Updating order");
        let request = OrderRequest::new(
            0,
            self.perpetual_id,
            RequestType::Change,
            Some(order.order_id()),
            price,
            size,
            None,
            // post_only since we want to provide liquidity not take it
            true,
            false,
            false,
            self.max_matches_per_order,
            self.leverage,
            None,
            None,
        );

        request.prepare(exchange)
    }
}
