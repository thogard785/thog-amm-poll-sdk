//! Calculate ThogAMM quotes locally from pool state using exact integer arithmetic.
//! Quotes describe the snapshot's block and perform no network calls.
pub mod abi;
pub mod events;
pub mod math;
pub mod model;
pub mod rpc;
pub mod state;

pub use alloy_primitives::{Address, Bytes, B256, I256, U256};
pub use model::{Fraction, Limits, PoolModel, PreparedPair, Quote};
pub use state::{BlockContext, BlockHeader, PoolState, RpcLog};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid pool data: {0}")]
    InvalidData(String),
    #[error("missing storage word {0}")]
    MissingWord(u16),
    #[error("unsupported ThogAMM state format {0}; obtain the matching SDK release from ThogAMM")]
    UnsupportedSchema(u16),
    #[error("unknown token {0}")]
    UnknownToken(Address),
    #[error("quote unavailable: {0}")]
    Unavailable(&'static str),
    #[error("input or output exceeds limit {0}")]
    LimitExceeded(U256),
    #[error("Solidity arithmetic overflow or division by zero")]
    Arithmetic,
    #[error("RPC error: {0}")]
    Rpc(String),
    #[error("provider head {reported} is behind model block {current}")]
    ProviderBehind { reported: u64, current: u64 },
    #[error("transport error: {0}")]
    Transport(String),
    #[error("ABI error: {0}")]
    Abi(String),
    #[error("ThogAMM subscription lost continuity; reconnect to load current pool state")]
    Discontinuous,
    #[error(
        "ThogAMM was upgraded; obtain the supported SDK release from ThogAMM before reconnecting"
    )]
    ContractUpgraded,
}
pub type Result<T> = std::result::Result<T, Error>;
