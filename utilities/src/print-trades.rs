//! Prints all trades from the testnet exchange.
//!
//! Run with: cargo run --bin print_trades

use std::{pin::pin, time::Duration};

use alloy::{
    providers::{Provider, ProviderBuilder},
    rpc::client::RpcClient,
    transports::layers::RetryBackoffLayer,
};
use futures::StreamExt;
use perpl_sdk::{Chain, stream, types::StateInstant};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = RpcClient::builder()
        .layer(RetryBackoffLayer::new(10, 100, 200))
        .connect("https://testnet-rpc.monad.xyz")
        .await?;
    client.set_poll_interval(Duration::from_millis(500));
    let provider = ProviderBuilder::new().connect_client(client);

    let chain = Chain::testnet();

    // Start from the current block
    let block_num = provider.get_block_number().await?;
    println!("Starting from block {}", block_num);

    let raw_events_stream = stream::raw(
        &chain,
        provider.clone(),
        StateInstant::new(block_num, 0),
        tokio::time::sleep,
    );
    let mut trades_stream = pin!(stream::trade(&chain, provider, raw_events_stream).await?);

    println!("Listening for trades...\n");

    while let Some(Ok(block_events)) = trades_stream.next().await {
        if !block_events.events().is_empty() {
            let trades = block_events
                .events()
                .iter()
                .map(|e| e.event())
                .collect::<Vec<_>>();
            println!(
                "Block {} - {} trade(s):",
                block_events.instant().block_number(),
                trades.len()
            );
            for trade in &trades {
                println!(
                    "  Taker {} {:?} {} @ {} on perp={} (fee: {})",
                    trade.taker_account_id,
                    trade.taker_side,
                    trade.total_size(),
                    trade.avg_price().unwrap_or_default(),
                    trade.perpetual_id,
                    trade.taker_fee,
                );
                for fill in &trade.maker_fills {
                    println!(
                        "    <- Maker {} order {} filled {} @ {} (fee: {})",
                        fill.maker_account_id, fill.maker_order_id, fill.size, fill.price, fill.fee,
                    );
                }
            }
        }
    }

    Ok(())
}
