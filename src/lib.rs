//! Read ThogAMM pool state once, then quote trade sizes locally in your aggregator.
//! Each refresh makes exactly one eth_call and includes newly listed tokens.
use std::time::{Duration, SystemTime};
pub use thogamm_model::rpc::{
    decode_pool_data, pool_data_calldata, pool_data_params, snapshot, CallBlock, ChainReader,
    HttpRpc,
};
pub use thogamm_model::{self as model, Address, Error, PoolModel, Result, U256};

#[derive(Clone, Debug)]
pub struct Config {
    /// Request cadence for `next_update()`. `refresh()` does not wait.
    pub poll_interval: Duration,
    /// Deadline for one state request, including custom ChainReader implementations.
    pub request_timeout: Duration,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(250),
            request_timeout: Duration::from_secs(2),
        }
    }
}
/// A ThogAMM client with a locally cached pool snapshot for synchronous quotes.
pub struct StatePollSdk<R = HttpRpc> {
    reader: R,
    model: PoolModel,
    config: Config,
    healthy: bool,
    received_at: SystemTime,
    timer: tokio::time::Interval,
}
impl StatePollSdk<HttpRpc> {
    /// Connect using a Monad HTTP RPC endpoint and the ThogAMM pool address.
    /// Loads the initial state with one call. Keep this client for later quotes.
    pub async fn connect(
        http_url: impl Into<String>,
        proxy: Address,
        config: Config,
    ) -> Result<Self> {
        Self::with_reader(HttpRpc::new(http_url)?, proxy, config).await
    }
}
impl<R: ChainReader> StatePollSdk<R> {
    /// Load initial state through your own RPC client with one call.
    pub async fn with_reader(reader: R, proxy: Address, config: Config) -> Result<Self> {
        if config.poll_interval.is_zero() || config.request_timeout.is_zero() {
            return Err(Error::InvalidData(
                "poll interval and request timeout must be positive".into(),
            ));
        }
        let model = tokio::time::timeout(
            config.request_timeout,
            snapshot(&reader, proxy, CallBlock::Latest),
        )
        .await
        .map_err(|_| Error::Transport("state request timed out".into()))??;
        let mut timer = tokio::time::interval_at(
            tokio::time::Instant::now() + config.poll_interval,
            config.poll_interval,
        );
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Ok(Self {
            timer,
            reader,
            model,
            config,
            healthy: true,
            received_at: SystemTime::now(),
        })
    }
    /// The most recent successful live read. After a failed or cancelled refresh,
    /// returns an explicit error until a complete refresh succeeds. A previously
    /// cloned PoolModel remains an immutable historical snapshot.
    pub fn model(&self) -> Result<&PoolModel> {
        if !self.healthy {
            return Err(Error::SnapshotUnavailable);
        }
        Ok(&self.model)
    }
    /// Local receipt time of the last successful read, not the block's timestamp.
    /// Compare the snapshot block with your chain head; receipt time alone cannot
    /// establish that an RPC provider is current.
    pub fn received_at(&self) -> SystemTime {
        self.received_at
    }
    /// Exactly one eth_call on every invocation, including unchanged blocks and
    /// errors. A failed refresh invalidates live access and reports its cause.
    /// `false` means the returned quote state and block context are unchanged.
    pub async fn refresh(&mut self) -> Result<bool> {
        self.healthy = false;
        let next = tokio::time::timeout(
            self.config.request_timeout,
            snapshot(&self.reader, self.model.state().proxy, CallBlock::Latest),
        )
        .await
        .map_err(|_| Error::Transport("state request timed out".into()))??;
        if next.state().block.number < self.model.state().block.number {
            return Err(Error::ProviderBehind {
                reported: next.state().block.number,
                current: self.model.state().block.number,
            });
        }
        self.healthy = true;
        self.received_at = SystemTime::now();
        if next.state() == self.model.state() {
            return Ok(false);
        }
        self.model = next;
        Ok(true)
    }
    /// Wait for the next scheduled tick, then make exactly one call. Return the model
    /// even if unchanged; waiting for a change must not hide additional calls.
    pub async fn next_update(&mut self) -> Result<&PoolModel> {
        self.timer.tick().await;
        self.refresh().await?;
        Ok(&self.model)
    }
}
