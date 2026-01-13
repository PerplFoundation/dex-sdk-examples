use alloy::{
    network::EthereumWallet,
    primitives::Address,
    providers::{DynProvider, ProviderBuilder},
    rpc::client::RpcClient,
};
use futures::StreamExt;
use perpl_sdk::{
    Chain, abi::dex::Exchange::ExchangeInstance, state::SnapshotBuilder, stream, types,
};
use std::{pin::pin, sync::Arc, time::Duration};
use tracing::{error, info, warn};
use url::Url;

use crate::strategies::{Strategy, StrategyType};

pub mod error;
pub mod strategies;

pub type Result<T> = std::result::Result<T, error::Error>;

#[derive(Debug)]
pub struct PerplMarketMakingBot {
    provider: DynProvider,
    accounts: Vec<types::AccountAddressOrID>,
    instance: ExchangeInstance<DynProvider>,
    chain: Chain,
    strategy: StrategyType,
    timeout: Duration,
}

impl PerplMarketMakingBot {
    pub async fn try_new(
        node_url: Url,
        wallet: EthereumWallet,
        chain: Chain,
        exchange_address: Address,
        strategy: StrategyType,
        timeout: Duration,
    ) -> Result<Self> {
        let wallet_address = wallet.default_signer().address();
        info!(
            strategy = strategy.name(),
            %wallet_address,
            %exchange_address,
            "Initializing Market Making Bot"
        );
        let rpc_client = RpcClient::new_http(node_url);
        let provider = DynProvider::new(
            ProviderBuilder::new()
                .wallet(wallet)
                .connect_client(rpc_client),
        );

        let instance = ExchangeInstance::new(exchange_address, provider.clone());

        Ok(Self {
            provider,
            accounts: vec![types::AccountAddressOrID::Address(wallet_address)],
            instance,
            chain,
            strategy,
            timeout,
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        loop {
            info!("Starting new exchange snapshot and event stream");
            let snapshot_builder = SnapshotBuilder::new(&self.chain, self.provider.clone())
                .with_accounts(self.accounts.clone())
                .with_perpetuals(vec![self.strategy.perpetual_id()]);

            let mut exchange = snapshot_builder.build().await?;
            info!("Exchange snapshot built successfully");

            let (error_tx, mut error_rx) = tokio::sync::mpsc::channel(100);

            self.strategy
                .initialize(&self.instance, &exchange)
                .await
                .inspect_err(|error| {
                    error!(%error, "Strategy initialization failed");
                })?;

            info!("Strategy initialized successfully, starting event processing loop");

            let instance = exchange.instant();
            let mut dex_stream = pin!(stream::raw(
                &self.chain,
                self.provider.clone(),
                instance,
                tokio::time::sleep,
            ));

            let mut interval = tokio::time::interval(self.timeout);
            interval.tick().await; // First tick completes immediately

            let order_semaphore = Arc::new(tokio::sync::Semaphore::new(1));
            let mut event_buffer = Vec::new();

            loop {
                let order_semaphore = order_semaphore.clone();

                tokio::select! {
                    event = dex_stream.next() => {
                        let Some(event) = event else {
                            error!("DEX stream closed unexpectedly, restarting...");
                            break;
                        };

                        let Ok(event) = event else {
                            error!("Error in DEX event stream, will auto-restart");
                            break;
                        };

                        event_buffer.push(event);

                        let Ok(permit) = order_semaphore.try_acquire_owned() else {
                            warn!("Previous strategy execution still in progress, skipping this event batch");
                            continue;
                        };

                        let mut block_events = Vec::new();

                        for ev in event_buffer.drain(..) {
                            let Some(result) = exchange.apply_events(&ev).unwrap() else {
                                continue;
                            };

                            block_events.push(result);
                        }

                        if block_events.is_empty() {
                            continue;
                        }

                        let state_events = block_events
                            .into_iter()
                            .flat_map(|b| b.events().iter().map(|ec| ec.event().clone()).collect::<Vec<_>>())
                            .flatten()
                            .collect::<Vec<_>>();

                        self.strategy
                            .execute(&self.instance, &exchange, &state_events, &error_tx, permit)
                            .await;
                    }
                    error = error_rx.recv() => {
                        let Some(err) = error else {
                            error!("Error channel closed unexpectedly, restarting...");
                            break;
                        };

                        let Ok(permit) = order_semaphore.try_acquire_owned() else {
                            warn!("Previous strategy execution still in progress, skipping this event batch");
                            continue;
                        };

                        warn!(%err, "Received error from strategy, will retry execution again if permitted");
                        self.strategy.execute(&self.instance, &exchange, &[], &error_tx, permit).await;
                    }
                    _ = interval.tick() => {
                        warn!("Timeout reached without receiving events, will run strategy execution just in case if permitted");
                        let Ok(permit) = order_semaphore.try_acquire_owned() else {
                            warn!("Previous strategy execution still in progress, skipping this event batch");
                            continue;
                        };
                        self.strategy.execute(&self.instance, &exchange, &[], &error_tx, permit).await;
                    }

                }
            }
        }
    }
}
