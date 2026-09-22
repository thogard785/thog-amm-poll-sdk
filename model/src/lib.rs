//! Deterministic, integer-only ThogAMM schema-6 pricing and synchronization primitives.
//! Quote results describe the attached block. No RPC is performed by the quote engine.
pub mod abi;
pub mod events;
pub mod math;
pub mod model;
pub mod rpc;
pub mod state;

pub use alloy_primitives::{Address, Bytes, B256, I256, U256};
pub use model::{ExecutionContext, Fraction, Limits, PoolModel, PreparedPair, Quote};
pub use state::{BlockContext, BlockHeader, PoolState, RpcLog};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid pool data: {0}")]
    InvalidData(String),
    #[error("missing storage word {0}")]
    MissingWord(u16),
    #[error("unsupported pool schema {0}")]
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
    #[error("subscription stream is discontinuous or failed; explicitly reinitialize from complete state")]
    Discontinuous,
    #[error("contract upgraded; verify the new implementation and explicitly initialize a compatible SDK")]
    ContractUpgraded,
}
pub type Result<T> = std::result::Result<T, Error>;
