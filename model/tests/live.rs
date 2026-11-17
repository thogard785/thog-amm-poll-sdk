//! Compare this crate with the deployed pool, call for call, at one block.
//!
//! Needs a Monad RPC endpoint, so it only runs when asked:
//!
//!     MONAD_RPC=https://rpc.monad.xyz cargo test --release --test live -- --ignored --nocapture
use alloy_primitives::keccak256;
use serde_json::{json, Value};
use thogamm::{
    abi::{self, SolCall, SolValue},
    rpc::{read_snapshot, BlockId, HttpRpc, MULTICALL3},
    Address, Bytes, Pool, TxContext, U256, WMON,
};

/// `eth_call`, waiting out a public endpoint's rate limit.
async fn call(rpc: &HttpRpc, params: Value) -> thogamm::Result<Bytes> {
    for attempt in 0..8 {
        match rpc.request("eth_call", params.clone()).await {
            Ok(result) => return Ok(serde_json::from_value(result).unwrap()),
            Err(thogamm::Error::Transport(message)) if message.contains("429") => {
                tokio::time::sleep(std::time::Duration::from_secs(2 << attempt.min(3))).await
            }
            Err(error) => return Err(error),
        }
    }
    panic!("rate limited")
}

async fn read_pool(rpc: &HttpRpc) -> Pool {
    for _ in 0..8 {
        match read_snapshot(rpc, POOL.parse().unwrap(), BlockId::Latest, &[]).await {
            Ok(snapshot) => return Pool::new(snapshot).unwrap(),
            Err(thogamm::Error::Transport(message)) if message.contains("429") => {
                tokio::time::sleep(std::time::Duration::from_secs(4)).await
            }
            Err(error) => panic!("{error}"),
        }
    }
    panic!("rate limited")
}

const POOL: &str = "0x80c74517BCC2D67fFE02D3ED886796272F647210";
const POOL_ID: &str = "0xce389e78282dedac7b18ba7f775b7602d2ab3ab171bbd6711eb0239be6ef4dcc";

#[tokio::test]
#[ignore = "needs MONAD_RPC"]
async fn every_pair_matches_the_deployed_pool() {
    let rpc = HttpRpc::new(std::env::var("MONAD_RPC").expect("set MONAD_RPC")).unwrap();
    let pool_address: Address = POOL.parse().unwrap();
    let pool_id = POOL_ID.parse().unwrap();
    let pool = read_pool(&rpc).await;
    let block = BlockId::Number(pool.block_number());

    // For every pair: getLimits, then a quote and a marginal price at four sizes.
    let mut calls = Vec::new();
    let mut expected: Vec<(String, Option<Vec<u8>>)> = Vec::new();
    for input in pool.tokens() {
        for output in pool.tokens() {
            if input.address == output.address {
                continue;
            }
            let pair = pool.pair(input.address, output.address);
            let name = format!("{} -> {}", input.address, output.address);
            calls.push(
                abi::getLimitsCall {
                    poolId: pool_id,
                    sellToken: input.address,
                    buyToken: output.address,
                }
                .abi_encode(),
            );
            expected.push((
                format!("getLimits {name}"),
                pair.as_ref()
                    .ok()
                    .and_then(|p| p.get_limits().ok())
                    .map(|l| vec![l.sell, l.buy].abi_encode()),
            ));
            for usd in [1u64, 100, 10_000, 1_000_000] {
                if input.price_low.is_zero() {
                    continue;
                }
                let amount = U256::from(usd)
                    * U256::from(10).pow(U256::from(18 + u32::from(input.decimals)))
                    / input.price_low;
                calls.push(
                    abi::makerQuoteExactInputCall {
                        tokenIn: input.address,
                        tokenOut: output.address,
                        amountIn: amount,
                    }
                    .abi_encode(),
                );
                expected.push((
                    format!("makerQuoteExactInput ${usd} {name}"),
                    pair.as_ref()
                        .ok()
                        .and_then(|p| p.maker_quote_exact_input(amount).ok())
                        .map(|q| (q.amount_out, U256::from(q.price_block)).abi_encode()),
                ));
                calls.push(
                    abi::priceCall {
                        poolId: pool_id,
                        sellToken: input.address,
                        buyToken: output.address,
                        specifiedAmounts: vec![amount],
                    }
                    .abi_encode(),
                );
                expected.push((
                    format!("price ${usd} {name}"),
                    pair.as_ref()
                        .ok()
                        .and_then(|p| p.price(amount).ok())
                        .map(|f| {
                            vec![abi::Fraction {
                                numerator: f.numerator,
                                denominator: f.denominator,
                            }]
                            .abi_encode()
                        }),
                ));
            }
        }
    }

    let (mut accepted, mut rejected) = (0, 0);
    for (calls, expected) in calls.chunks(30).zip(expected.chunks(30)) {
        let batch = abi::aggregate3Call {
            calls: calls
                .iter()
                .map(|data| abi::Call3 {
                    target: pool_address,
                    allowFailure: true,
                    callData: Bytes::copy_from_slice(data),
                })
                .collect(),
        };
        let data: Bytes = batch.abi_encode().into();
        let output = call(
            &rpc,
            json!([{ "to": MULTICALL3, "data": data }, block.to_json()]),
        )
        .await
        .unwrap();
        let results = abi::aggregate3Call::abi_decode_returns_validate(&output).unwrap();
        for (result, (name, here)) in results.iter().zip(expected) {
            match here {
                Some(bytes) => {
                    assert!(
                        result.success,
                        "{name}: the pool reverted, this crate did not"
                    );
                    assert_eq!(result.returnData.as_ref(), bytes.as_slice(), "{name}");
                    accepted += 1;
                }
                None => {
                    assert!(
                        !result.success,
                        "{name}: the pool answered, this crate rejected"
                    );
                    rejected += 1;
                }
            }
        }
    }
    println!(
        "block {}: {} tokens, {accepted} equal results, {rejected} matching rejections",
        pool.block_number(),
        pool.tokens().len()
    );
    assert!(accepted > 0);
}

/// Simulate real swaps with `eth_call`: a caller given WMON and an allowance
/// by state override, and a stand-in fee recipient that accepts every call.
#[tokio::test]
#[ignore = "needs MONAD_RPC"]
async fn swaps_match_the_deployed_pool_at_every_fee_and_gas_price() {
    let rpc = HttpRpc::new(std::env::var("MONAD_RPC").expect("set MONAD_RPC")).unwrap();
    let pool_address: Address = POOL.parse().unwrap();
    let pool = read_pool(&rpc).await;
    let block = BlockId::Number(pool.block_number());
    let base_fee = pool.base_fee();
    assert!(!base_fee.is_zero());

    let caller = Address::repeat_byte(0xa1);
    let fee_recipient = Address::repeat_byte(0xfe);
    // WMON keeps balances in mapping slot 3 and allowances in slot 4.
    let balance_slot = keccak256((caller, U256::from(3)).abi_encode());
    let allowance_slot = keccak256(
        (
            pool_address,
            keccak256((caller, U256::from(4)).abi_encode()),
        )
            .abi_encode(),
    );
    let rich = U256::from(10).pow(U256::from(30));
    let overrides = json!({
        // Storage values are full 32-byte words.
        WMON.to_string(): { "stateDiff": {
            balance_slot.to_string(): thogamm::B256::from(rich),
            allowance_slot.to_string(): thogamm::B256::from(U256::MAX),
        } },
        fee_recipient.to_string(): { "code": "0x00" },
        // The call names a gas price, so the caller must be able to pay for gas.
        caller.to_string(): { "balance": rich },
    });

    let stock = pool
        .tokens()
        .iter()
        .find(|t| t.xstock.as_ref().is_some_and(|x| !x.shares.is_zero()))
        .unwrap()
        .address;
    let outputs = [pool.tokens()[0].address, pool.tokens()[4].address, stock];
    let amount_in = U256::from(2000) * U256::from(10).pow(U256::from(18));
    let gas_prices = [
        base_fee,
        base_fee + base_fee / U256::from(2) + U256::from(1),
        base_fee * U256::from(2) + U256::from(1),
        base_fee * U256::from(3),
    ];
    let (mut accepted, mut rejected) = (0, 0);
    for token_out in outputs {
        let pair = pool.pair(WMON, token_out);
        for gas_price in gas_prices {
            let tx = TxContext::with_gas_price(gas_price);
            let mut cases: Vec<(String, Vec<u8>, thogamm::Result<thogamm::Swap>)> = vec![(
                "makerSwapExactInput".into(),
                abi::makerSwapExactInputCall {
                    tokenIn: WMON,
                    tokenOut: token_out,
                    amountIn: amount_in,
                    minAmountOut: U256::ZERO,
                    recipient: caller,
                    deadlineBlock: U256::MAX,
                }
                .abi_encode(),
                pair.as_ref()
                    .map_err(|e| thogamm::Error::Rpc(e.to_string()))
                    .and_then(|p| p.maker_swap_exact_input(amount_in, tx)),
            )];
            for ppm in [0u32, 1, 100, 10_000, 250_000, 1_000_000] {
                cases.push((
                    format!("swapWithExtraFees {ppm} ppm"),
                    abi::swapWithExtraFeesCall {
                        tokenIn: WMON,
                        tokenOut: token_out,
                        amountIn: amount_in,
                        minAmountOut: U256::ZERO,
                        expiry: U256::MAX,
                        extraFeePpm: U256::from(ppm),
                        feeRecipient: fee_recipient,
                    }
                    .abi_encode(),
                    pair.as_ref()
                        .map_err(|e| thogamm::Error::Rpc(e.to_string()))
                        .and_then(|p| p.swap_with_extra_fees(amount_in, ppm, tx)),
                ));
            }
            for (name, data, here) in cases {
                let params = json!([
                    { "from": caller, "to": pool_address, "data": Bytes::from(data), "gasPrice": gas_price },
                    block.to_json(),
                    overrides,
                ]);
                let chain = call(&rpc, params).await;
                let label = format!("{name}, WMON -> {token_out}, gas price {gas_price}");
                match (chain, here) {
                    (Ok(output), Ok(swap)) => {
                        assert_eq!(U256::from_be_slice(&output), swap.amount_out, "{label}");
                        accepted += 1;
                    }
                    (Err(_), Err(_)) => rejected += 1,
                    (chain, here) => panic!("{label}: pool {chain:?}, this crate {here:?}"),
                }
            }
        }
    }
    println!(
        "block {}: {accepted} swaps equal to the pool, {rejected} matching rejections",
        pool.block_number()
    );
    assert!(accepted > 0);
}

/// How a transaction's fee fields and access list reach the surcharge.
#[tokio::test]
#[ignore = "needs MONAD_RPC"]
async fn fee_fields_and_the_access_list_set_the_surcharge() {
    let rpc = HttpRpc::new(std::env::var("MONAD_RPC").expect("set MONAD_RPC")).unwrap();
    let pool_address: Address = POOL.parse().unwrap();
    let pool = read_pool(&rpc).await;
    let block = BlockId::Number(pool.block_number());
    let base_fee = pool.base_fee();
    let caller = Address::repeat_byte(0xa1);
    let balance_slot = keccak256((caller, U256::from(3)).abi_encode());
    let allowance_slot = keccak256(
        (
            pool_address,
            keccak256((caller, U256::from(4)).abi_encode()),
        )
            .abi_encode(),
    );
    let rich = U256::from(10).pow(U256::from(30));
    let overrides = json!({
        WMON.to_string(): { "stateDiff": {
            balance_slot.to_string(): thogamm::B256::from(rich),
            allowance_slot.to_string(): thogamm::B256::from(U256::MAX),
        } },
        caller.to_string(): { "balance": rich },
    });
    let amount_in = U256::from(2000) * U256::from(10).pow(U256::from(18));
    let swap_to = |token_out| -> Bytes {
        abi::makerSwapExactInputCall {
            tokenIn: WMON,
            tokenOut: token_out,
            amountIn: amount_in,
            minAmountOut: U256::ZERO,
            recipient: caller,
            deadlineBlock: U256::MAX,
        }
        .abi_encode()
        .into()
    };
    let usdc = pool.tokens()[0].address;
    let pair = pool.pair(WMON, usdc).unwrap();
    let handler = "0xD32EdF6642D917DbBE7B8BF8e5d6F5df6a9FFF58";
    let triple = base_fee * U256::from(3);
    let tip = base_fee * U256::from(6) / U256::from(10);
    let access_list = json!([{ "address": handler, "storageKeys": [] }]);
    // (fields of the simulated transaction, the effective gas price they
    // give, whether they make the AuctionHandler warm)
    for (name, fields, gas_price, warm) in [
        ("no fee fields", json!({}), U256::ZERO, false),
        (
            "gasPrice at the base fee",
            json!({ "gasPrice": base_fee }),
            base_fee,
            false,
        ),
        (
            "gasPrice at three times the base fee",
            json!({ "gasPrice": triple }),
            triple,
            false,
        ),
        (
            "EIP-1559 without a tip",
            json!({ "maxFeePerGas": triple, "maxPriorityFeePerGas": U256::ZERO }),
            base_fee,
            false,
        ),
        (
            "EIP-1559 with a 60% tip",
            json!({ "maxFeePerGas": triple, "maxPriorityFeePerGas": tip }),
            base_fee + tip,
            false,
        ),
        (
            "EIP-1559 with a tip above the fee cap",
            json!({ "maxFeePerGas": triple, "maxPriorityFeePerGas": triple }),
            triple,
            false,
        ),
        // An access list that names the AuctionHandler makes it warm, and the
        // swap pays the highest surcharge at any gas price. This case sets the
        // EIP-1559 fields because a Monad node drops the access list of an
        // `eth_call` that is priced with `gasPrice`.
        (
            "EIP-1559 without a tip, AuctionHandler in the access list",
            json!({ "maxFeePerGas": triple, "maxPriorityFeePerGas": U256::ZERO, "accessList": access_list }),
            base_fee,
            true,
        ),
    ] {
        let mut tx = json!({ "from": caller, "to": pool_address, "data": swap_to(usdc) });
        for (key, value) in fields.as_object().unwrap() {
            tx[key] = value.clone();
        }
        let output = call(&rpc, json!([tx, block.to_json(), overrides]))
            .await
            .unwrap();
        let context = TxContext {
            gas_price,
            auction_handler_warm: warm,
        };
        let here = pair.maker_swap_exact_input(amount_in, context).unwrap();
        assert_eq!(U256::from_be_slice(&output), here.amount_out, "{name}");
        println!("{name}: surcharge {} x 0.01 bp", here.surcharge);
    }
    // Gas for a direct call from an account, for sizing a gas limit.
    let stock = pool
        .tokens()
        .iter()
        .find(|t| t.xstock.as_ref().is_some_and(|x| !x.shares.is_zero()))
        .unwrap()
        .address;
    for (name, token_out) in [
        ("USDC", usdc),
        ("WETH", pool.tokens()[4].address),
        ("an xStock", stock),
    ] {
        let tx = json!({ "from": caller, "to": pool_address, "data": swap_to(token_out), "gasPrice": base_fee });
        let gas = rpc
            .request("eth_estimateGas", json!([tx, block.to_json(), overrides]))
            .await
            .unwrap();
        println!(
            "eth_estimateGas, WMON -> {name}: {}",
            u64::from_str_radix(gas.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
        );
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}
