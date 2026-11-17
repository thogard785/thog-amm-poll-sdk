//! Read a complete snapshot with one `eth_call`.
//!
//! The pool data, each token's stored multiplier and each xStock's shares must
//! come from the same block. Sending them as one [Multicall3] call makes that
//! true by construction: every read runs in a single EVM execution.
//!
//! [Multicall3]: https://www.multicall3.com
use crate::{abi, Address, Bytes, Error, Result, Snapshot, XStock, U256};
use alloy_sol_types::SolCall;
use serde_json::{json, Value};
use std::{
    future::Future,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

/// Multicall3, deployed at the same address on every chain including Monad.
pub const MULTICALL3: Address =
    alloy_primitives::address!("cA11bde05977b3631167028862bE2a173976CA11");

/// The block to read at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BlockId {
    #[default]
    Latest,
    Finalized,
    Number(u64),
}

impl BlockId {
    /// The JSON-RPC block parameter.
    pub fn to_json(self) -> Value {
        match self {
            Self::Latest => json!("latest"),
            Self::Finalized => json!("finalized"),
            Self::Number(number) => json!(format!("0x{number:x}")),
        }
    }
}

/// The one RPC method a snapshot needs. Implement it to read through your own
/// RPC client; [`HttpRpc`] is a ready-made implementation.
pub trait EthCall: Send + Sync {
    /// `eth_call` of `data` to `to` at `block`, returning the call's output.
    ///
    /// Send the call with `gasPrice` set to `0x1`. A Monad node reports a zero
    /// base fee to an `eth_call` that names no gas price, and the snapshot
    /// records the base fee it sees.
    fn eth_call(
        &self,
        to: Address,
        data: Bytes,
        block: BlockId,
    ) -> impl Future<Output = Result<Bytes>> + Send;
}

/// The `params` of that `eth_call`, for callers that build their own requests.
pub fn eth_call_params(to: Address, data: &Bytes, block: BlockId) -> Value {
    json!([{ "to": to, "data": data, "gasPrice": "0x1" }, block.to_json()])
}

/// The single call that reads a whole snapshot: `to` and `data` for
/// [`EthCall::eth_call`]. `tokens` is the pool's token list.
///
/// It asks the pool for its data and each token's stored multiplier, and each
/// token for the pool's shares. Only an xStock answers the last question;
/// ordinary tokens are allowed to fail it.
pub fn snapshot_call(pool: Address, tokens: &[Address]) -> (Address, Bytes) {
    let mut calls = vec![abi::Call3 {
        target: pool,
        allowFailure: false,
        callData: abi::getPoolDataCall {
            startTokenIndex: 0,
            stopTokenIndex: 28,
        }
        .abi_encode()
        .into(),
    }];
    for token in tokens {
        calls.push(abi::Call3 {
            target: pool,
            allowFailure: false,
            callData: abi::storedMultiplierCall { token: *token }
                .abi_encode()
                .into(),
        });
        calls.push(abi::Call3 {
            target: *token,
            allowFailure: true,
            callData: abi::sharesOfCall { holder: pool }.abi_encode().into(),
        });
    }
    (
        MULTICALL3,
        abi::aggregate3Call { calls }.abi_encode().into(),
    )
}

/// What [`decode_snapshot`] found.
pub enum Decoded {
    Snapshot(Box<Snapshot>),
    /// The pool's token list differs from the one the call was built with.
    /// Read again with this list.
    TokensChanged(Vec<Address>),
}

/// Decode the output of [`snapshot_call`] made with the same `tokens`.
pub fn decode_snapshot(pool: Address, tokens: &[Address], output: &[u8]) -> Result<Decoded> {
    let results = abi::aggregate3Call::abi_decode_returns_validate(output)
        .map_err(|e| Error::Abi(e.to_string()))?;
    if results.len() != 1 + 2 * tokens.len() || !results[0].success {
        return Err(Error::Rpc("incomplete snapshot call result".into()));
    }
    let data = abi::getPoolDataCall::abi_decode_returns_validate(&results[0].returnData)
        .map_err(|e| Error::Abi(e.to_string()))?;
    let listed: Vec<Address> = data.tokens.iter().map(|t| t.token).collect();
    if listed != tokens {
        return Ok(Decoded::TokensChanged(listed));
    }
    let mut xstocks = Vec::with_capacity(tokens.len());
    for reads in results[1..].chunks_exact(2) {
        let (multiplier, shares) = (&reads[0], &reads[1]);
        let multiplier =
            abi::storedMultiplierCall::abi_decode_returns_validate(&multiplier.returnData)
                .map_err(|e| Error::Abi(e.to_string()))?;
        // A zero stored multiplier is how the pool marks an ordinary token.
        // Such a token's answer about shares means nothing and is not read.
        xstocks.push(if multiplier.is_zero() {
            None
        } else {
            if !shares.success || shares.returnData.len() != 32 {
                return Err(Error::Rpc("xStock did not report its shares".into()));
            }
            Some(XStock {
                shares: U256::from_be_slice(&shares.returnData),
                multiplier,
            })
        });
    }
    let snapshot = Snapshot::from_pool_data(pool, data, |token| {
        tokens
            .iter()
            .position(|t| *t == token)
            .and_then(|index| xstocks[index].clone())
    })?;
    Ok(Decoded::Snapshot(Box::new(snapshot)))
}

/// Read the pool's complete state at `block`.
///
/// `known_tokens` is the token list from an earlier snapshot, or empty. With
/// a current list this is one request; otherwise it is two, the first of
/// which learns the list.
pub async fn read_snapshot(
    rpc: &impl EthCall,
    pool: Address,
    block: BlockId,
    known_tokens: &[Address],
) -> Result<Snapshot> {
    let mut tokens = known_tokens.to_vec();
    // The list changes only when the pool lists or retires a token, so a
    // second mismatch in a row means reads are landing on different blocks.
    for _ in 0..3 {
        let (to, data) = snapshot_call(pool, &tokens);
        let output = rpc.eth_call(to, data, block).await?;
        match decode_snapshot(pool, &tokens, &output)? {
            Decoded::Snapshot(snapshot) => {
                if matches!(block, BlockId::Number(number) if number != snapshot.block.number) {
                    return Err(Error::Rpc("node answered for a different block".into()));
                }
                return Ok(*snapshot);
            }
            Decoded::TokensChanged(listed) => tokens = listed,
        }
    }
    Err(Error::Rpc("the pool's token list kept changing".into()))
}

/// A minimal JSON-RPC client over HTTP.
#[derive(Clone)]
pub struct HttpRpc {
    client: reqwest::Client,
    url: String,
    next_id: Arc<AtomicU64>,
}

impl HttpRpc {
    pub fn new(url: impl Into<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(Self {
            client,
            url: url.into(),
            next_id: Arc::new(AtomicU64::new(1)),
        })
    }

    /// Send one JSON-RPC request and return its `result`.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        // Errors are reported without the URL, which often embeds an API key.
        let transport = |e: reqwest::Error| Error::Transport(e.without_url().to_string());
        let mut reply: Value = self
            .client
            .post(&self.url)
            .json(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .send()
            .await
            .map_err(transport)?
            .error_for_status()
            .map_err(transport)?
            .json()
            .await
            .map_err(transport)?;
        if let Some(error) = reply.get("error") {
            return Err(Error::Rpc(error.to_string()));
        }
        match reply.get_mut("result").map(Value::take) {
            Some(result) if reply["id"] == json!(id) && !result.is_null() => Ok(result),
            _ => Err(Error::Rpc("malformed JSON-RPC reply".into())),
        }
    }
}

impl EthCall for HttpRpc {
    async fn eth_call(&self, to: Address, data: Bytes, block: BlockId) -> Result<Bytes> {
        let result = self
            .request("eth_call", eth_call_params(to, &data, block))
            .await?;
        serde_json::from_value(result).map_err(|e| Error::Rpc(e.to_string()))
    }
}
