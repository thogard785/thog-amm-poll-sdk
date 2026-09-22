#[path = "../../fixtures/support.rs"]
mod support;

use alloy_sol_types::SolCall;
use state_poll_sdk::{
    decode_pool_data, pool_data_params, snapshot, CallBlock, ExecutionContext, U256,
};
use thogamm_model::{abi, Error};

#[test]
fn externally_fetched_state_supports_all_quote_surfaces_without_a_reader() {
    let f = support::fixture();
    let set = &f.samples[3];
    let number = support::page(&set.snapshot).blockNumber.to::<u64>();
    // There is no RPC client or SDK background task in this path.
    let model = decode_pool_data(f.proxy, &set.snapshot, CallBlock::Number(number)).unwrap();
    let reference = support::model(f.proxy, f.baseFee, &set.snapshot);
    for sample in &set.quotes {
        let a = model.state().tokens[sample.inputIndex as usize].token;
        let b = model.state().tokens[sample.outputIndex as usize].token;
        let context = ExecutionContext {
            gas_price: sample.gasPrice,
            fast_lane_hot: sample.fastLaneHot,
        };
        let actual = model.prepare(a, b);
        match sample.kind {
            0 => assert_eq!(
                format!(
                    "{:?}",
                    actual.and_then(|p| p.quote_exact_input(sample.amount))
                ),
                format!("{:?}", reference.quote_exact_input(a, b, sample.amount))
            ),
            1 => assert_eq!(
                format!("{:?}", actual.and_then(|p| p.marginal_price(sample.amount))),
                format!("{:?}", reference.marginal_price(a, b, sample.amount))
            ),
            2 => assert_eq!(
                format!("{:?}", actual.and_then(|p| p.limits())),
                format!("{:?}", reference.limits(a, b))
            ),
            3 => assert_eq!(
                format!(
                    "{:?}",
                    actual.and_then(|p| p.quote_execution_exact_input(sample.amount, &context))
                ),
                format!(
                    "{:?}",
                    reference.quote_execution_exact_input(a, b, sample.amount, &context)
                )
            ),
            4 => assert_eq!(
                format!(
                    "{:?}",
                    actual.and_then(|p| p.quote_exact_output(sample.amount, &context))
                ),
                format!(
                    "{:?}",
                    reference.quote_exact_output(a, b, sample.amount, &context)
                )
            ),
            _ => unreachable!(),
        }
    }
    assert!(model.state().block.hash.is_none());
    let hash = support::hash(number, 0);
    let pinned = decode_pool_data(f.proxy, &set.snapshot, CallBlock::Hash(hash)).unwrap();
    assert_eq!(pinned.state(), reference.state());
    assert_eq!(
        pool_data_params(f.proxy, CallBlock::Number(number))[1],
        format!("0x{number:x}")
    );
}

#[test]
fn external_state_rejects_bad_abi_incomplete_registry_schema_and_wrong_block() {
    let f = support::fixture();
    let bytes = &f.samples[3].snapshot;
    let mut page = support::page(bytes);
    let number = page.blockNumber.to::<u64>();
    assert!(decode_pool_data(f.proxy, &bytes[..bytes.len() - 1], CallBlock::Latest).is_err());
    assert!(decode_pool_data(f.proxy, bytes, CallBlock::Number(number + 1)).is_err());
    page.schemaVersion = 5;
    assert!(matches!(
        decode_pool_data(
            f.proxy,
            &abi::getPoolDataCall::abi_encode_returns(&page),
            CallBlock::Latest
        ),
        Err(Error::UnsupportedSchema(5))
    ));
    page.schemaVersion = 6;
    page.tokens.pop();
    page.balances.pop();
    assert!(decode_pool_data(
        f.proxy,
        &abi::getPoolDataCall::abi_encode_returns(&page),
        CallBlock::Latest
    )
    .is_err());
}

#[tokio::test]
async fn caller_selected_historical_block_is_one_call_without_head_discovery() {
    let f = support::fixture();
    let rpc = support::MockRpc::from_events(&f);
    let number = rpc.0.lock().unwrap().head;
    rpc.0.lock().unwrap().head += 2;
    let model = snapshot(&rpc, f.proxy, CallBlock::Number(number))
        .await
        .unwrap();
    assert_eq!(rpc.call_count(), 1);
    assert_eq!(model.state().block.number, number);
    assert_eq!(model.state().block.base_fee_per_gas, f.baseFee);
}

#[test]
fn settlement_binding_preserves_recipient_minimum_and_block_deadline() {
    let f = support::fixture();
    let model = support::model(f.proxy, f.baseFee, &f.samples[0].snapshot);
    let args = abi::makerSwapExactInputCall {
        tokenIn: model.state().tokens[0].token,
        tokenOut: model.state().tokens[4].token,
        amountIn: U256::from(1_000_000),
        minAmountOut: U256::from(12345),
        recipient: model.state().tokens[1].token,
        deadlineBlock: U256::from(model.state().block.number + 2),
    };
    let bytes = args.abi_encode();
    // Selector from the independently compiled Solidity interface.
    assert_eq!(&bytes[..4], &[0x4d, 0x8b, 0xad, 0x92]);
    assert_eq!(bytes.len(), 4 + 6 * 32);
    assert_eq!(
        &bytes[4 + 2 * 32..4 + 3 * 32],
        &args.amountIn.to_be_bytes::<32>()
    );
    assert_eq!(
        &bytes[4 + 3 * 32..4 + 4 * 32],
        &args.minAmountOut.to_be_bytes::<32>()
    );
    assert_eq!(
        &bytes[4 + 4 * 32 + 12..4 + 5 * 32],
        args.recipient.as_slice()
    );
    assert_eq!(
        &bytes[4 + 5 * 32..],
        &args.deadlineBlock.to_be_bytes::<32>()
    );
}
