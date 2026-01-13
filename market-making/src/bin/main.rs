use alloy::{network::EthereumWallet, primitives::Address, signers::local::PrivateKeySigner};
use clap::Parser;
use fastnum::{UD64, decimal::Context};
use perpl_market_making_bot::{
    PerplMarketMakingBot,
    strategies::{StrategyType, bbo::BboStrategy, spread::SpreadStrategy, taker::TakerStrategy},
};
use perpl_sdk::Chain;
use std::{process::exit, time::Duration};
use tracing::error;
use url::Url;

#[derive(Debug, serde::Deserialize)]
struct PerplConfig {
    chain_id: u64,
    collateral_token_address: String,
    address: String,
    private_key: String,
    deployed_at_block: u64,
    perpetual_id: u32,
    node_rpc_url: String,
    timeout_seconds: Option<u64>,
}

#[derive(Debug, clap::Parser)]
enum StrategyConfig {
    Bbo(BboStrategyArgs),
    Spread(SpreadStrategyArgs),
    Taker(TakerStrategyArgs),
}

#[derive(Debug, clap::Args)]
struct BboStrategyArgs {
    /// Size of each order
    #[clap(long)]
    order_size: String,
}

#[derive(Debug, clap::Args)]
struct SpreadStrategyArgs {
    /// Number of orders to place on each side of the spread
    #[clap(long)]
    orders_per_side: usize,
    /// The size of each order
    #[clap(long)]
    order_size: String,
    /// Max matches per order
    #[clap(long)]
    max_matches: Option<u32>,
    /// Leverage for each order
    #[clap(long)]
    leverage: Option<String>,
}

#[derive(Debug, clap::Args)]
struct TakerStrategyArgs {
    /// The size of each order
    #[clap(long)]
    order_size: String,
    /// Leverage for each order
    #[clap(long)]
    leverage: Option<String>,
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().expect("Failed to load .env file");

    let perpl_config =
        envy::from_env::<PerplConfig>().expect("Failed to parse config from environment variables");

    let args = StrategyConfig::parse();

    let strategy = match args {
        StrategyConfig::Bbo(args) => {
            let order_size = args.order_size.parse().expect("Invalid order size");
            StrategyType::Bbo(BboStrategy::new(order_size, perpl_config.perpetual_id))
        }
        StrategyConfig::Spread(args) => {
            let order_size = args.order_size.parse().expect("Invalid order size");
            let leverage = args
                .leverage
                .as_ref()
                .map(|lev| UD64::from_str(lev, Context::default()).expect("Invalid leverage"));

            StrategyType::Spread(SpreadStrategy::new(
                args.orders_per_side,
                order_size,
                perpl_config.perpetual_id,
                args.max_matches,
                leverage.unwrap_or(UD64::ONE),
            ))
        }
        StrategyConfig::Taker(args) => {
            let order_size = args.order_size.parse().expect("Invalid order size");
            let leverage = args
                .leverage
                .as_ref()
                .map(|lev| UD64::from_str(lev, Context::default()).expect("Invalid leverage"));

            StrategyType::Taker(TakerStrategy::new(
                order_size,
                leverage.unwrap_or(UD64::ONE),
                perpl_config.perpetual_id,
            ))
        }
    };

    if std::env::var("RUST_LOG").is_err() {
        unsafe {
            std::env::set_var("RUST_LOG", "info");
        }
    }

    let collateral_token_address: Address = perpl_config
        .collateral_token_address
        .parse()
        .expect("Invalid collateral token address");
    let address: Address = perpl_config
        .address
        .parse()
        .expect("Invalid exchange address");
    let maker_private_key: PrivateKeySigner = perpl_config
        .private_key
        .parse()
        .expect("Invalid maker private key");

    let wallet = EthereumWallet::new(maker_private_key);

    let node_url = Url::parse(&perpl_config.node_rpc_url).expect("Invalid RPC URL");

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // Default is 30 seconds if not specified
    let timeout = Duration::from_secs(perpl_config.timeout_seconds.unwrap_or(30));

    let mut bot = PerplMarketMakingBot::try_new(
        node_url,
        wallet,
        Chain::custom(
            perpl_config.chain_id,
            collateral_token_address,
            perpl_config.deployed_at_block,
            address,
            vec![perpl_config.perpetual_id],
        ),
        address,
        strategy,
        timeout,
    )
    .await
    .expect("Failed to create market making bot");

    if let Err(error) = bot.run().await {
        error!(%error, "Market making bot encountered an error, shutting down");
        exit(1);
    }
}
