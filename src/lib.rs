//! Read ThogAMM pool state once, then quote trade sizes locally in your aggregator.
//! Each refresh makes exactly one eth_call and includes newly listed tokens.
use std::time::Duration;
pub use thogamm_model::rpc::{
    decode_pool_data, pool_data_calldata, pool_data_params, snapshot, CallBlock, ChainReader,
    HttpRpc,
};
pub use thogamm_model::{self as model, Address, Error, PoolModel, Result, U256};

#[derive(Clone, Debug)]
pub struct Config {
    /// Time between calls when using `next_update()`. `refresh()` does not wait.
    pub poll_interval: Duration,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(500),
        }
    }
}
/// A ThogAMM client with a locally cached pool snapshot for synchronous quotes.
pub struct StatePollSdk<R = HttpRpc> {
    reader: R,
    model: PoolModel,
    config: Config,
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
        if config.poll_interval.is_zero() {
            return Err(Error::InvalidData("poll interval must be positive".into()));
        }
        let model = snapshot(&reader, proxy, CallBlock::Latest).await?;
        Ok(Self {
            reader,
            model,
            config,
        })
    }
    /// The most recent pool snapshot. Quote methods perform no network calls.
    pub fn model(&self) -> &PoolModel {
        &self.model
    }
    /// Exactly one eth_call on every invocation, including unchanged blocks and
    /// errors. Publishes the complete returned image or preserves the last image.
    /// `false` means the returned quote state and block context are unchanged.
    pub async fn refresh(&mut self) -> Result<bool> {
        let next = snapshot(&self.reader, self.model.state().proxy, CallBlock::Latest).await?;
        if next.state().block.number < self.model.state().block.number {
            return Err(Error::ProviderBehind {
                reported: next.state().block.number,
                current: self.model.state().block.number,
            });
        }
        if next.state() == self.model.state() {
            return Ok(false);
        }
        self.model = next;
        Ok(true)
    }
    /// Wait one polling interval, then make exactly one call. Return the model
    /// even if unchanged; waiting for a change must not hide additional calls.
    pub async fn next_update(&mut self) -> Result<&PoolModel> {
        tokio::time::sleep(self.config.poll_interval).await;
        self.refresh().await?;
        Ok(&self.model)
    }
}
