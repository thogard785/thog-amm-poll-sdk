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
    // This effective gas price is below twice the projected block's base fee.
    assert_eq!(
        next.quote_execution_exact_input(a, b, amount, gas_price)
            .unwrap()
            .amount_out,
        next.quote_exact_input(a, b, amount).unwrap().amount_out
    );
    let high = model.at_block(n + 1, f.baseFee).unwrap();
    assert_eq!(
        high.quote_execution_exact_input(a, b, amount, f.baseFee * u(2))
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
    assert!(events::apply_block(&original, support::model_header(&after), &logs).is_err());
    logs.pop();
    logs[0].removed = true;
    assert!(events::apply_block(&original, support::model_header(&after), &logs).is_err());
    assert_eq!(before.state(), &original);
}

#[test]
fn deployed_publications_preserve_spare_bits_and_replay_caps_and_readiness() {
    use alloy_sol_types::{SolCall, SolValue};
    use std::io::Read;
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(&include_bytes!("../../tests/common/events-fork-v7.bin.gz")[..])
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
        )
        .unwrap();
        assert_eq!(&actual, after.state(), "stage {i}");
        assert!(actual.word(29).unwrap().bit(255));
        assert!(actual.word(9).unwrap().bit(250));
        if i == 0 {
            assert!(actual.word(2).unwrap().bit(134));
            assert!(!actual.word(2).unwrap().bit(135));
        }
    }
    assert_eq!(stages.len(), 3);
}

#[test]
fn wmon_checkpoint_checks_replayed_wmon_without_claiming_other_token_coverage() {
    use alloy_sol_types::SolEvent;
    use thogamm_model::{abi, state::WMON, Error, RpcLog};
    let f = support::fixture();
    let model = support::model(f.proxy, f.baseFee, &f.samples[0].snapshot);
    let wmon_index = model.index(WMON).unwrap();
    let h = support::header(model.state().block.number + 1, f.baseFee);
    let transfer = abi::Transfer {
        from: thogamm_model::Address::ZERO,
        to: f.proxy,
        value: u(7),
    }
    .encode_log_data();
    let checkpoint = abi::MakerWmonBalanceCheckpoint {
        balance: model.state().balances[wmon_index] + u(7),
    }
    .encode_log_data();
    let logs: Vec<_> = [(WMON, transfer), (f.proxy, checkpoint)]
        .into_iter()
        .enumerate()
        .map(|(i, (address, event))| RpcLog {
            address,
            topics: event.topics().to_vec(),
            data: event.data,
            block_hash: h.hash,
            block_number: h.number,
            transaction_index: 0,
            log_index: i as u64,
            removed: false,
        })
        .collect();
    let next = events::apply_block(model.state(), h.clone(), &logs).unwrap();
    assert_eq!(
        next.balances[wmon_index],
        model.state().balances[wmon_index] + u(7)
    );
    assert!(matches!(
        events::apply_block(model.state(), h, &logs[1..]),
        Err(Error::BalanceCheckpointMismatch { token: WMON, .. })
    ));
}
