//! ThogAMM's pricing, reproduced in memory with exact integer arithmetic.
//!
//! ```text
//! chain ──read_snapshot──▶ Snapshot ──Pool::new──▶ Pool ──pair──▶ Pair ──▶ quotes and swaps
//!                             ▲
//!                             └── events::apply_block (one block of logs)
//! ```
//!
//! A [`Snapshot`] is the pool's raw state at one block. A [`Pool`] is that
//! state decoded into named values. A [`Pair`] is one trading direction; its
//! methods have the names of the contract calls they reproduce and return the
//! same numbers.
//!
//! Integration guide: <https://github.com/thogard785/thog-amm-integration-guide>
pub mod abi;
mod curve;
pub mod events;
pub mod layout;
mod math;
mod pool;
pub mod rpc;
mod snapshot;

pub use alloy_primitives::{Address, Bytes, B256, I256, U256};
pub use pool::{Category, Fraction, Limits, Pair, Pool, Quote, Swap, Token, TokenCost, TxContext};
pub use snapshot::{Block, Snapshot, XStock, WMON};

/// Why the pool would refuse a quote or swap. Each names the contract's revert.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Rejection {
    /// `maker: unknown token`
    #[error("token {0} is not listed")]
    UnknownToken(Address),
    /// `maker: unknown token`
    #[error("input and output are the same token")]
    IdenticalTokens,
    /// `maker: paused`
    #[error("the pool is paused")]
    Paused,
    /// `maker: risk uninitialized`
    #[error("the pool's parameters have not been published yet")]
    NotReady,
    /// `maker: side off`
    #[error("the pool is not accepting the input token")]
    InputDisabled,
    /// `maker: side off`
    #[error("the pool is not paying out the output token")]
    OutputDisabled,
    /// `maker: stale`
    #[error("the pool's prices are older than its maximum age")]
    StalePrices,
    /// `maker: spread`
    #[error("the spread for this pair is 100% or more")]
    SpreadTooWide,
    /// `maker: zero fill`
    #[error("the amount is too small to produce any output")]
    ZeroFill,
    /// `MakerAmountTooLarge(limit)` or ERC-7815 `LimitExceeded(limit)`
    #[error("the amount exceeds the limit of {limit}")]
    AmountTooLarge { limit: U256 },
    /// `maker: balance` or `maker: inventory uint128`
    #[error("the swap would exceed the pool's inventory or an inventory cap")]
    InventoryLimit,
    /// ERC-7815 `Unavailable("requested output unavailable")`
    #[error("no input amount produces the requested output")]
    OutputUnavailable,
    /// `maker: wrapper target` or `maker: nonpositive wrapper value`
    #[error("the token's cost parameters do not allow this swap")]
    TokenCost,
    /// `maker: extra fee`
    #[error("extraFeePpm is above 1,000,000")]
    FeeTooHigh,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The pool would revert the call.
    #[error("the pool would reject this: {0}")]
    Rejected(#[from] Rejection),
    /// The contract's checked arithmetic would overflow, so it would revert.
    #[error("the pool's arithmetic would overflow")]
    Arithmetic,
    /// The state is in a format this version does not decode. Use the SDK
    /// release that matches the deployed contract.
    #[error("unsupported pool state format {0:#x}")]
    UnsupportedFormat(u64),
    /// The data read from the chain is incomplete or inconsistent.
    #[error("invalid pool state: {0}")]
    InvalidSnapshot(String),
    #[error("ABI decoding failed: {0}")]
    Abi(String),
    #[error("RPC error: {0}")]
    Rpc(String),
    #[error("transport error: {0}")]
    Transport(String),
    /// The node answered for an older block than one already seen.
    #[error("the node is at block {reported}, behind block {current} already read")]
    NodeBehind { reported: u64, current: u64 },
    /// A block or log is missing or out of order. Read a fresh snapshot.
    #[error("the event stream skipped or repeated something")]
    Discontinuous,
    /// A token was listed or retired. Read a fresh snapshot.
    #[error("the pool's token list changed")]
    RegistryChanged,
    /// The pool's implementation changed. Check that this SDK release supports
    /// it before reading a fresh snapshot.
    #[error("the pool contract was upgraded")]
    ContractUpgraded,
}

impl Error {
    /// The reason the pool would refuse, when that is what this error is.
    pub fn rejection(&self) -> Option<&Rejection> {
        match self {
            Self::Rejected(rejection) => Some(rejection),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
