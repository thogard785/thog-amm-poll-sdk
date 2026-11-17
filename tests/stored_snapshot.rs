use alloy_sol_types::SolCall;
use std::{
    io::Read,
    sync::{Arc, Mutex},
};
use thogamm_model::{
    abi,
    rpc::{CallBlock, ChainReader},
    Address, BlockHeader, Bytes, Result, ShareBalances, U256,
};

#[derive(Clone)]
struct CapturedRpc {
    proxy: Address,
    header: BlockHeader,
    page: Bytes,
    shares: ShareBalances,
    calls: Arc<Mutex<Vec<(Address, Bytes)>>>,
}

#[async_trait::async_trait]
impl ChainReader for CapturedRpc {
    async fn header(&self, block: CallBlock) -> Result<BlockHeader> {
        assert_eq!(block, CallBlock::Latest);
        Ok(self.header.clone())
    }
    async fn calls(&self, calls: Vec<(Address, Bytes)>, block: CallBlock) -> Result<Vec<Bytes>> {
        assert_eq!(block, CallBlock::Hash(self.header.hash));
        calls
            .into_iter()
            .map(|(to, data)| {
                self.calls.lock().unwrap().push((to, data.clone()));
                let result = if data.starts_with(&abi::getPoolDataCall::SELECTOR) {
                    assert_eq!(to, self.proxy);
                    let call = abi::getPoolDataCall::abi_decode_validate(&data).unwrap();
                    assert_eq!((call.startTokenIndex, call.stopTokenIndex), (0, 28));
                    self.page.to_vec()
                } else if data.starts_with(&abi::sharesOfCall::SELECTOR) {
                    let call = abi::sharesOfCall::abi_decode_validate(&data).unwrap();
                    assert_eq!(call.holder, self.proxy);
                    abi::sharesOfCall::abi_encode_returns(&self.shares[&to].shares)
                } else {
                    assert_eq!(to, self.proxy, "multipliers must be read from the pool");
                    let call = abi::storedMultiplierCall::abi_decode_validate(&data).unwrap();
                    // A one-word response catches regressions to the issuer's three-word ABI.
                    abi::storedMultiplierCall::abi_encode_returns(
                        &self.shares[&call.token].multiplier,
                    )
                };
                Ok(result.into())
            })
            .collect()
    }
}

#[tokio::test]
async fn polling_loads_all_nine_stored_multipliers_from_the_pool_at_one_hash() {
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(&include_bytes!("common/deployed-stored-multipliers.json.gz")[..])
        .read_to_end(&mut bytes)
        .unwrap();
    let f: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let s = &f["snapshots"][0];
    let rpc = CapturedRpc {
        proxy: f["proxy"].as_str().unwrap().parse().unwrap(),
        header: BlockHeader {
            number: s["blockNumber"].as_u64().unwrap(),
            hash: s["blockHash"].as_str().unwrap().parse().unwrap(),
            parent_hash: s["parentHash"].as_str().unwrap().parse().unwrap(),
            base_fee_per_gas: s["baseFeePerGas"].as_str().unwrap().parse().unwrap(),
        },
        page: s["snapshot"].as_str().unwrap().parse().unwrap(),
        shares: serde_json::from_value(s["shareBalances"].clone()).unwrap(),
        calls: Arc::default(),
    };
    let mut sdk = state_poll_sdk::StatePollSdk::with_reader(
        rpc.clone(),
        rpc.proxy,
        state_poll_sdk::Config::default(),
    )
    .await
    .unwrap();
    assert_eq!(sdk.model().state().tokens.len(), 17);
    assert_eq!(sdk.model().state().share_balances, rpc.shares);
    assert_eq!(rpc.shares.len(), 9);
    assert_eq!(rpc.calls.lock().unwrap().len(), 19);
    // Quotes must not trigger reads, even when the quote is unavailable.
    let tokens = &sdk.model().state().tokens;
    let _ = sdk
        .model()
        .quote_exact_input(tokens[0].token, tokens[13].token, U256::from(1_000_000));
    assert_eq!(rpc.calls.lock().unwrap().len(), 19);
    assert!(!sdk.refresh().await.unwrap());
    assert_eq!(rpc.calls.lock().unwrap().len(), 38);
}
