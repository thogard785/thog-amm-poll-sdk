//! One request per snapshot: eth_call(getPoolData(0, 64)).
use crate::{abi, Address, Bytes, Error, PoolModel, Result, B256};
use alloy_sol_types::SolCall;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CallBlock {
    #[default]
    Latest,
    Finalized,
    Number(u64),
    Hash(B256),
}
impl CallBlock {
    pub fn rpc_value(self) -> Value {
        match self {
            Self::Latest => json!("latest"),
            Self::Finalized => json!("finalized"),
            Self::Number(number) => json!(format!("0x{number:x}")),
            Self::Hash(hash) => json!({"blockHash":hash,"requireCanonical":true}),
        }
    }
}
/// Deliberately exposes only eth_call. Snapshot construction needs no other RPC.
/// Implementations must preserve the selected block's BASEFEE: on Monad, set a
/// nonzero simulation gasPrice (the built-in reader uses 1 wei). Omitting fee
/// fields can zero BASEFEE in eth_call, corrupting execution-friction context.
#[async_trait]
pub trait ChainReader: Clone + Send + Sync {
    async fn call(&self, to: Address, data: Bytes, block: CallBlock) -> Result<Bytes>;
}
#[derive(Clone)]
pub struct HttpRpc {
    client: reqwest::Client,
    url: String,
    next_id: Arc<AtomicU64>,
}
impl HttpRpc {
    pub fn new(url: impl Into<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(Self {
            client,
            url: url.into(),
            next_id: Arc::new(AtomicU64::new(1)),
        })
    }
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let response = self
            .client
            .post(&self.url)
            .json(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .send()
            .await
            .map_err(|e| Error::Transport(e.without_url().to_string()))?
            .error_for_status()
            .map_err(|e| Error::Transport(e.without_url().to_string()))?;
        let result: Value = response
            .json()
            .await
            .map_err(|e| Error::Rpc(e.without_url().to_string()))?;
        if result.get("id") != Some(&json!(id)) || result.get("jsonrpc") != Some(&json!("2.0")) {
            return Err(Error::Rpc("invalid JSON-RPC envelope".into()));
        }
        if let Some(error) = result.get("error") {
            return Err(Error::Rpc(error.to_string()));
        }
        result
            .get("result")
            .filter(|x| !x.is_null())
            .cloned()
            .ok_or_else(|| Error::Rpc("missing result".into()))
    }
}
#[async_trait]
impl ChainReader for HttpRpc {
    async fn call(&self, to: Address, data: Bytes, block: CallBlock) -> Result<Bytes> {
        let value = self
            // This is a state read, not a trade simulation. Nonzero gasPrice
            // preserves BASEFEE without fetching a header or fee suggestion.
            .request("eth_call", state_call_params(to, data, block))
            .await?;
        serde_json::from_value(value).map_err(|e| Error::Rpc(e.to_string()))
    }
}
fn state_call_params(to: Address, data: Bytes, block: CallBlock) -> Value {
    json!([{"to":to,"data":data,"gasPrice":"0x1"},block.rpc_value()])
}
/// Calldata for a complete image, including any newly listed tokens. No I/O.
pub fn pool_data_calldata() -> Bytes {
    abi::getPoolDataCall {
        startTokenIndex: 0,
        stopTokenIndex: 64,
    }
    .abi_encode()
    .into()
}
/// The `params` of one eth_call, for aggregators that own the RPC transport.
/// Retain the 1-wei simulation gasPrice: it preserves the actual block BASEFEE.
/// This fee is unrelated to the eventual swap's effective transaction gas price.
pub fn pool_data_params(proxy: Address, block: CallBlock) -> Value {
    state_call_params(proxy, pool_data_calldata(), block)
}
/// Build an offline model from the raw ABI return bytes of getPoolData(0,64).
/// `block` must be the selector used for that request. Hash provenance is supplied
/// by the caller; the EVM cannot verify or return its own current block hash.
/// This validates the image and performs no RPC, header lookup or subscription.
pub fn decode_pool_data(proxy: Address, bytes: &[u8], block: CallBlock) -> Result<PoolModel> {
    let page = abi::getPoolDataCall::abi_decode_returns_validate(bytes)
        .map_err(|e| Error::Abi(e.to_string()))?;
    if page.startTokenIndex != 0 || page.tokens.len() != usize::from(page.tokenCount) {
        return Err(Error::InvalidData(
            "RPC returned an incomplete registry".into(),
        ));
    }
    if let CallBlock::Number(number) = block {
        if page.blockNumber != crate::U256::from(number) {
            return Err(Error::InvalidData(
                "snapshot block differs from requested block".into(),
            ));
        }
    }
    let hash = match block {
        CallBlock::Hash(hash) => Some(hash),
        _ => None,
    };
    PoolModel::from_snapshot(proxy, page, hash)
}
/// Fetch every listed token, balance, dependency and quote context in exactly
/// one call. No maxIndex query, header query, pagination loop, retry or batch.
pub async fn snapshot<R: ChainReader>(
    rpc: &R,
    proxy: Address,
    block: CallBlock,
) -> Result<PoolModel> {
    let bytes = rpc.call(proxy, pool_data_calldata(), block).await?;
    decode_pool_data(proxy, &bytes, block)
}
