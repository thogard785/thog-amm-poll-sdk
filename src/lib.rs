//! Keep a ThogAMM pool current by polling an HTTP RPC endpoint.
//!
//! ```no_run
//! # async fn example() -> thogamm_poll::Result<()> {
//! use thogamm_poll::{Poller, TxContext, U256};
//!
//! let pool_address = "0x80c74517BCC2D67fFE02D3ED886796272F647210".parse().unwrap();
//! let usdc = "0x754704Bc059F8C67012fEd69BC8A327a5aafb603".parse().unwrap();
//! let weth = "0xEE8c0E9f1BFFb4Eb878d8f15f368A02a35481242".parse().unwrap();
//!
//! let poller = Poller::connect("https://rpc.monad.xyz", pool_address).await?;
//! let pair = poller.pool().pair(usdc, weth)?;
//! let quote = pair.maker_quote_exact_input(U256::from(100_000_000u64))?;
//! println!("100 USDC -> {} wei of WETH", quote.amount_out);
//! # Ok(()) }
//! ```
//!
//! Quoting is in the [`thogamm`] crate, re-exported here.
use std::time::Duration;
pub use thogamm::{
    self,
    rpc::{BlockId, EthCall, HttpRpc},
    Address, Error, Pair, Pool, Quote, Rejection, Result, Swap, TxContext, U256,
};
use thogamm::{rpc::read_snapshot, Snapshot};

/// Time between reads in [`Poller::next_update`]; a little over one Monad block.
pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(500);

/// A ThogAMM pool read from an RPC endpoint and re-read on demand.
pub struct Poller<R = HttpRpc> {
    rpc: R,
    pool: Pool,
    interval: Duration,
}

impl Poller<HttpRpc> {
    /// Read the pool at `pool_address` through a Monad HTTP RPC endpoint.
    pub async fn connect(http_url: impl Into<String>, pool_address: Address) -> Result<Self> {
        Self::with_rpc(HttpRpc::new(http_url)?, pool_address).await
    }
}

impl<R: EthCall> Poller<R> {
    /// As [`Poller::connect`], reading through your own RPC client.
    pub async fn with_rpc(rpc: R, pool_address: Address) -> Result<Self> {
        let snapshot = read_snapshot(&rpc, pool_address, BlockId::Latest, &[]).await?;
        Ok(Self {
            rpc,
            pool: Pool::new(snapshot)?,
            interval: DEFAULT_INTERVAL,
        })
    }

    /// Change the time [`Poller::next_update`] waits between reads.
    pub fn set_interval(&mut self, interval: Duration) {
        self.interval = interval;
    }

    /// The pool as of the last successful read. Its block is
    /// `pool().snapshot().block.number`.
    ///
    /// The pool's prices age even when nothing is read: before quoting, move
    /// the pool to the block you expect to trade in with [`Pool::at_block`].
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Read the pool's current state: one request. Returns whether anything
    /// changed. On an error the previous state stays in place, one block
    /// older than it was.
    pub async fn refresh(&mut self) -> Result<bool> {
        let current: &Snapshot = self.pool.snapshot();
        let next = read_snapshot(&self.rpc, current.pool, BlockId::Latest, &current.tokens).await?;
        if next.block.number < current.block.number {
            return Err(Error::NodeBehind {
                reported: next.block.number,
                current: current.block.number,
            });
        }
        if next == *current {
            return Ok(false);
        }
        self.pool = Pool::new(next)?;
        Ok(true)
    }

    /// Wait one interval, then [`refresh`](Poller::refresh).
    pub async fn next_update(&mut self) -> Result<&Pool> {
        tokio::time::sleep(self.interval).await;
        self.refresh().await?;
        Ok(&self.pool)
    }
}
