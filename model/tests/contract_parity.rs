#[path = "../../tests/common/mod.rs"]
mod support;
use thogamm_model::{events, math::u, PoolModel, U256};

#[test]
fn projected_quotes_preserve_provenance_and_use_the_requested_block_context() {
    let f = support::fixture();
    let model = support::model(f.proxy, f.baseFee, &f.samples[0].snapshot);
    let n = model.state().block.number;
    let a = model.state().tokens[0].token;
    let b = model.state().tokens[4].token;
    let amount = u(1_000_000);
    let next = model.at_block(n + 1, f.baseFee * u(2)).unwrap();
    assert_eq!(next.state(), model.state());
    assert_eq!(next.quote_block_number(), n + 1);
    assert_eq!(next.quote_base_fee_per_gas(), f.baseFee * u(2));
    assert!(model.at_block(n - 1, f.baseFee).is_err());
    assert!(model
        .at_block(n + 256, f.baseFee)
        .unwrap()
        .quote_exact_input(a, b, amount)
        .is_err());
    let gas_price = f.baseFee * u(2) + U256::ONE;
    // This effective gas price is below 1.5 times the projected block's base fee.
    assert_eq!(
        next.quote_execution_exact_input(a, b, amount, gas_price)
            .unwrap()
            .amount_out,
        next.quote_exact_input(a, b, amount).unwrap().amount_out
    );
    let high = model.at_block(n + 1, f.baseFee).unwrap();
    assert_eq!(
        high.quote_execution_exact_input(a, b, amount, f.baseFee + f.baseFee / u(2))
            .unwrap()
            .amount_out,
        high.quote_exact_input(a, b, amount).unwrap().amount_out
    );
    assert!(
        high.quote_execution_exact_input(a, b, amount, gas_price)
            .unwrap()
            .amount_out
            < high.quote_exact_input(a, b, amount).unwrap().amount_out
    );
}

#[test]
fn shared_snapshots_keep_prepared_quotes_isolated_from_new_state_and_context() {
    let f = support::fixture();
    let model = support::model(f.proxy, f.baseFee, &f.samples[0].snapshot);
    let shared = model.clone();
    assert!(std::ptr::eq(model.state(), shared.state()));
    let a = model.state().tokens[0].token;
    let b = model.state().tokens[4].token;
    let pair = model.prepare(a, b).unwrap();
    let amount = u(1_000_000);
    let original = pair.quote_exact_input(amount).unwrap();
    let mut state = shared.into_state();
    state.balances[4] = U256::ZERO;
    let empty = PoolModel::new(state).unwrap();
    assert!(empty
        .prepare(a, b)
        .unwrap()
        .quote_exact_input(amount)
        .is_err());
    assert_eq!(pair.quote_exact_input(amount).unwrap(), original);
    let future = model
        .at_block(model.state().block.number + 256, f.baseFee)
        .unwrap();
    assert!(future.prepare(a, b).is_err());
    assert_eq!(pair.quote_exact_input(amount).unwrap(), original);
    fn send_sync<T: Send + Sync>() {}
    send_sync::<PoolModel>();
    send_sync::<thogamm_model::PreparedPair<'_>>();
}
#[test]
fn duplicate_removed_or_out_of_order_logs_never_partially_mutate_state() {
    let f = support::fixture();
    let e = &f.events[0];
    let before = support::model(f.proxy, f.baseFee, &e.beforeState);
    let after = support::model(f.proxy, f.baseFee, &e.afterState);
    let mut logs = support::logs(e, f.baseFee);
    let original = before.state().clone();
    logs.push(logs[0].clone());
    assert!(events::apply_block(
        &original,
        support::model_header(&after),
        &logs,
        &support::multipliers_for(&after)
    )
    .is_err());
    logs.pop();
    logs[0].removed = true;
    assert!(events::apply_block(
        &original,
        support::model_header(&after),
        &logs,
        &support::multipliers_for(&after)
    )
    .is_err());
    assert_eq!(before.state(), &original);
}

#[test]
fn deployed_publications_preserve_spare_bits_and_replay_caps_and_readiness() {
    use alloy_sol_types::{SolCall, SolValue};
    use std::io::Read;
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(&include_bytes!("../../tests/common/events-fork-v8.bin.gz")[..])
        .read_to_end(&mut bytes)
        .unwrap();
    let stages = Vec::<support::EventSet>::abi_decode_validate(&bytes).unwrap();
    let f = support::fixture();
    let normalize = |data: &thogamm_model::Bytes| {
        let mut p = support::page(data);
        p.parentBlockHash = support::hash(p.blockNumber.to::<u64>() - 1, 0);
        thogamm_model::Bytes::from(thogamm_model::abi::getPoolDataCall::abi_encode_returns(&p))
    };
    for (i, e) in stages.iter().enumerate() {
        let before = support::model(f.proxy, f.baseFee, &normalize(&e.beforeState));
        let after = support::model(f.proxy, f.baseFee, &normalize(&e.afterState));
        let actual = events::apply_block(
            before.state(),
            support::model_header(&after),
            &support::logs(e, f.baseFee),
            &support::multipliers_for(&after),
        )
        .unwrap();
        assert_eq!(&actual, after.state(), "stage {i}");
        assert!(actual.word(27).unwrap().bit(255));
        assert!(actual.word(9).unwrap().bit(250));
        if i == 0 {
            assert!(actual.word(2).unwrap().bit(134));
            assert!(!actual.word(2).unwrap().bit(135));
        }
    }
    assert_eq!(stages.len(), 3);
}

#[test]
fn legacy_execution_model_is_rejected() {
    let f = support::fixture();
    let model = support::model(f.proxy, f.baseFee, &f.samples[0].snapshot);
    let mut state = model.into_state();
    let metadata = state.words.get_mut(&8).unwrap();
    *metadata = (*metadata & !(u(255) << 176usize)) | (u(2) << 176usize);
    assert!(PoolModel::new(state).is_err());
}

#[test]
fn projecting_to_next_block_removes_the_same_block_discount() {
    use alloy_sol_types::SolCall;
    use std::io::Read;
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(
        &include_bytes!("../../tests/common/execution-model3.json.gz")[..],
    )
    .read_to_end(&mut bytes)
    .unwrap();
    let f: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let s = &f["snapshots"][0];
    let encoded: thogamm_model::Bytes = s["snapshot"].as_str().unwrap().parse().unwrap();
    let fresh = PoolModel::from_snapshot(
        f["proxy"].as_str().unwrap().parse().unwrap(),
        thogamm_model::abi::getPoolDataCall::abi_decode_returns_validate(&encoded).unwrap(),
        None,
        serde_json::from_value(s["shareBalances"].clone()).unwrap(),
    )
    .unwrap();
    let base = fresh.quote_base_fee_per_gas();
    let aged = fresh
        .at_block(fresh.quote_block_number() + 1, base)
        .unwrap();
    let a = fresh.state().tokens[0].token;
    let b = fresh.state().tokens[4].token;
    let amount = u(1_000_000);
    for (gas, warm, discounted, raw) in [
        (base + base / u(2), false, 0, 0),
        (base + base / u(2) + U256::ONE, false, 50, 600),
        (base * u(2) + U256::ONE, false, 100, 800),
        (base * u(2) + base / u(2) + U256::ONE, false, 200, 1200),
        (base, true, 200, 1200),
    ] {
        for (model, units) in [(&fresh, discounted), (&aged, raw)] {
            let read = model.quote_exact_input(a, b, amount).unwrap().amount_out;
            let expected =
                thogamm_model::math::mul_div(read, u(1_000_000 - units), u(1_000_000), false)
                    .unwrap();
            assert_eq!(
                model
                    .quote_execution_exact_input_with_context(a, b, amount, gas, warm)
                    .unwrap()
                    .amount_out,
                expected
            );
        }
    }
}
