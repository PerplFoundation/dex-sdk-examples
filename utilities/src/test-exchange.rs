use dex_sdk::testing::TestExchange;
use fastnum::UD64;
use tokio::signal::unix::{SignalKind, signal};
use tracing::{info, warn};

#[tokio::main]
async fn main() {
    if std::env::var("RUST_LOG").is_err() {
        unsafe {
            std::env::set_var("RUST_LOG", "info");
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    tokio::spawn(async move {
        let exchange = TestExchange::new().await;
        // Create maker account (ID 0) with 1,000,000 units of collateral
        let maker_one = exchange.account(0, 1_000_000).await;
        // Create maker account (ID 1) with 1,000,000 units of collateral
        let maker_two = exchange.account(1, 1_000_000).await;
        // Create taker account (ID 2) with 1,000,000 units of collateral
        let taker = exchange.account(2, 1_000_000).await;
        // Createa BTC perpetual market
        let btc_perp = exchange.btc_perp().await;

        info!(
            chain = ?exchange.chain(),
            address = %exchange.exchange.address(),
            rpc_url = %exchange.rpc_url,
            maker_one_address = %maker_one.address,
            maker_two_address = %maker_two.address,
            taker_address = %taker.address,
            btc_perp_id = btc_perp.id,
            "Test Exchange Started"
        );

        let mut iteration = 0;
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
            let price: u32 = 100_000 + ((iteration % 5) * 10_000);
            let price = UD64::from_u32(price);
            info!(%price, "Updated BTC perpetual index price");
            btc_perp.set_index_price(price).await;
            iteration += 1;
        }
    });

    let mut sigterm = signal(SignalKind::terminate()).unwrap();
    let mut sigint = signal(SignalKind::interrupt()).unwrap();
    tokio::select! {
        _ = sigterm.recv() => warn!("Received SIGTERM"),
        _ = sigint.recv() => warn!("Received SIGINT"),

    };
}
