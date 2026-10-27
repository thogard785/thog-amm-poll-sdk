//! Hash-pinned pool and issuer state. Quotes perform no network calls.
use crate::{
    abi, shares::is_xstock, Address, BlockHeader, Bytes, Error, Multipliers, PoolModel, Result,
    ShareBalance, ShareBalances, B256,
};
use alloy_sol_types::SolCall;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
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
/// Reads must preserve BASEFEE. The built-in transport sets gasPrice to 1 wei.
#[async_trait]
pub trait ChainReader: Clone + Send + Sync {
    async fn header(&self, block: CallBlock) -> Result<BlockHeader>;
    async fn calls(&self, calls: Vec<(Address, Bytes)>, block: CallBlock) -> Result<Vec<Bytes>>;
    async fn call(&self, to: Address, data: Bytes, block: CallBlock) -> Result<Bytes> {
        let mut results = self.calls(vec![(to, data)], block).await?;
        if results.len() != 1 {
            return Err(Error::Rpc("expected one call result".into()));
        }
        Ok(results.remove(0))
    }
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
    async fn header(&self, block: CallBlock) -> Result<BlockHeader> {
        let (method, selector) = match block {
            CallBlock::Hash(hash) => ("eth_getBlockByHash", json!(hash)),
            _ => ("eth_getBlockByNumber", block.rpc_value()),
        };
        serde_json::from_value(self.request(method, json!([selector, false])).await?)
            .map_err(|e| Error::Rpc(e.to_string()))
    }
    async fn calls(&self, calls: Vec<(Address, Bytes)>, block: CallBlock) -> Result<Vec<Bytes>> {
        if calls.is_empty() {
            return Ok(Vec::new());
        }
        let first = self
            .next_id
            .fetch_add(calls.len() as u64, Ordering::Relaxed);
        let requests: Vec<_> = calls.into_iter().enumerate().map(|(i, (to, data))|
            json!({"jsonrpc":"2.0", "id":first+i as u64, "method":"eth_call", "params":state_call_params(to,data,block)})
        ).collect();
        let results: Vec<Value> = self
            .client
            .post(&self.url)
            .json(&requests)
            .send()
            .await
            .map_err(|e| Error::Transport(e.without_url().to_string()))?
            .error_for_status()
            .map_err(|e| Error::Transport(e.without_url().to_string()))?
            .json()
            .await
            .map_err(|e| Error::Rpc(e.without_url().to_string()))?;
        let mut by_id = BTreeMap::new();
        for result in results {
            let id = result["id"]
                .as_u64()
                .ok_or_else(|| Error::Rpc("missing batch ID".into()))?;
            if result["jsonrpc"] != "2.0"
                || id < first
                || id >= first + requests.len() as u64
                || by_id.insert(id, result).is_some()
            {
                return Err(Error::Rpc("invalid JSON-RPC batch".into()));
            }
        }
        (0..requests.len())
            .map(|i| {
                let response = by_id
                    .remove(&(first + i as u64))
                    .ok_or_else(|| Error::Rpc("incomplete JSON-RPC batch".into()))?;
                if let Some(error) = response.get("error") {
                    return Err(Error::Rpc(error.to_string()));
                }
                serde_json::from_value(response["result"].clone())
                    .map_err(|e| Error::Rpc(e.to_string()))
            })
            .collect()
    }
}
fn state_call_params(to: Address, data: Bytes, block: CallBlock) -> Value {
    json!([{"to":to,"data":data,"gasPrice":"0x1"},block.rpc_value()])
}
/// Calldata for a complete image, including any newly listed tokens. No I/O.
pub fn pool_data_calldata() -> Bytes {
    abi::getPoolDataCall {
        startTokenIndex: 0,
        stopTokenIndex: 28,
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
/// Decode a complete pool image and issuer share state read at the same block.
/// The caller owns provenance for externally supplied reads.
pub fn decode_pool_data(
    proxy: Address,
    bytes: &[u8],
    block: CallBlock,
    shares: ShareBalances,
) -> Result<PoolModel> {
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
    PoolModel::from_snapshot(proxy, page, hash, shares)
}
/// Read current issuer multipliers at a known block, including activations without logs.
pub async fn read_multipliers<R: ChainReader>(
    rpc: &R,
    tokens: &[Address],
    hash: B256,
) -> Result<Multipliers> {
    let tokens: Vec<_> = tokens.iter().copied().filter(|t| is_xstock(*t)).collect();
    let calls = tokens
        .iter()
        .map(|t| (*t, abi::getCurrentMultiplierCall {}.abi_encode().into()))
        .collect();
    let results = rpc.calls(calls, CallBlock::Hash(hash)).await?;
    if results.len() != tokens.len() {
        return Err(Error::Rpc("incomplete issuer batch".into()));
    }
    tokens
        .into_iter()
        .zip(results)
        .map(|(token, data)| {
            let m = abi::getCurrentMultiplierCall::abi_decode_returns_validate(&data)
                .map_err(|e| Error::Abi(e.to_string()))?
                .multiplier;
            if m.is_zero() {
                return Err(Error::InvalidData("zero xStock multiplier".into()));
            }
            Ok((token, m))
        })
        .collect()
}
/// Header, pool image, then one issuer batch, all eth_calls pinned to the header hash.
pub async fn snapshot<R: ChainReader>(
    rpc: &R,
    proxy: Address,
    block: CallBlock,
) -> Result<PoolModel> {
    let header = rpc.header(block).await?;
    if matches!(block, CallBlock::Hash(hash) if hash != header.hash)
        || matches!(block, CallBlock::Number(number) if number != header.number)
    {
        return Err(Error::InvalidData(
            "RPC header differs from requested block".into(),
        ));
    }
    let pinned = CallBlock::Hash(header.hash);
    let bytes = rpc.call(proxy, pool_data_calldata(), pinned).await?;
    let page = abi::getPoolDataCall::abi_decode_returns_validate(&bytes)
        .map_err(|e| Error::Abi(e.to_string()))?;
    let tokens: Vec<_> = page
        .tokens
        .iter()
        .filter(|t| is_xstock(t.token))
        .map(|t| t.token)
        .collect();
    let mut calls = Vec::new();
    for token in &tokens {
        calls.push((
            *token,
            abi::sharesOfCall { holder: proxy }.abi_encode().into(),
        ));
        calls.push((*token, abi::getCurrentMultiplierCall {}.abi_encode().into()));
    }
    let results = rpc.calls(calls, pinned).await?;
    if results.len() != 2 * tokens.len() {
        return Err(Error::Rpc("incomplete issuer batch".into()));
    }
    let mut shares = ShareBalances::new();
    for (token, pair) in tokens.into_iter().zip(results.chunks_exact(2)) {
        shares.insert(
            token,
            ShareBalance {
                shares: abi::sharesOfCall::abi_decode_returns_validate(&pair[0])
                    .map_err(|e| Error::Abi(e.to_string()))?,
                multiplier: abi::getCurrentMultiplierCall::abi_decode_returns_validate(&pair[1])
                    .map_err(|e| Error::Abi(e.to_string()))?
                    .multiplier,
            },
        );
    }
    let model = decode_pool_data(proxy, &bytes, pinned, shares)?;
    if model.state().block != header.into() {
        return Err(Error::InvalidData(
            "pool and header context disagree".into(),
        ));
    }
    Ok(model)
}
