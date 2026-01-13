use perpl_sdk::{error::DexError, types::PerpetualId};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Alloy contract error: {0}")]
    AlloyContract(#[from] alloy::contract::Error),
    #[error("Alloy local signer error: {0}")]
    AlloyLocalSigner(#[from] alloy::signers::local::LocalSignerError),
    #[error("Alloy pending transaction: {0}")]
    AlloyPendingTransaction(#[from] alloy::providers::PendingTransactionError),
    #[error("Dex error: {0}")]
    Dex(#[from] DexError),
    #[error("Invalid RPC URL: {0}")]
    InvalidRpcUrl(#[from] url::ParseError),
    #[error("No account found for strategy")]
    NoAccountFoundForStrategy,
    #[error("Too many accounts found for strategy")]
    TooManyAccountsForStrategy,
    #[error("Account ID already set")]
    AccountIdAlreadySet,
    #[error("Perpetual ID {0} not found in exchange state")]
    PerpetualNotFoundInExchangeState(PerpetualId),
    #[error("Strategy not initialized")]
    StrategyNotInitialized,
}
