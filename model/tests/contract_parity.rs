#[path = "../../tests/common/mod.rs"]
mod support;
use alloy_sol_types::SolValue;
use thogamm_model::{
    events,
    math::{mask, u},
    Error, ExecutionContext, PoolModel, PoolState, U256,
};

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
    let context = ExecutionContext {
        gas_price: f.baseFee * u(2) + U256::ONE,
        fast_lane_hot: false,
    };
    // This effective gas price is below twice the projected block's base fee.
    assert_eq!(
        next.quote_execution_exact_input(a, b, amount, &context)
            .unwrap()
            .amount_out,
        next.quote_exact_input(a, b, amount).unwrap().amount_out
    );
    let high = model.at_block(n + 1, f.baseFee).unwrap();
    assert!(
        high.quote_execution_exact_input(a, b, amount, &context)
            .unwrap()
            .amount_out
            < high.quote_exact_input(a, b, amount).unwrap().amount_out
    );
}

#[test]
fn quotes_match_solidity_across_legacy_and_64_token_images() {
    let f = support::fixture();
    let mut tested = [0usize; 5];
    let mut successes = [0usize; 5];
    for (world, set) in f.samples.iter().enumerate() {
        let model = if world == 7 {
            let source = support::model(f.proxy, f.baseFee, &f.samples[0].snapshot);
            let projected = source
                .at_block(source.state().block.number + 1, f.baseFee)
                .unwrap();
            assert_eq!(projected.state(), source.state());
            projected
        } else {
            support::model(f.proxy, f.baseFee, &set.snapshot)
        };
        for (case, s) in set.quotes.iter().enumerate() {
            let a = model.state().tokens[s.inputIndex as usize].token;
            let b = model.state().tokens[s.outputIndex as usize].token;
            let context = ExecutionContext {
                gas_price: s.gasPrice,
                fast_lane_hot: s.fastLaneHot,
            };
            let got: thogamm_model::Result<Vec<U256>> = match s.kind {
                0 => model
                    .quote_exact_input(a, b, s.amount)
                    .map(|q| vec![q.amount_out, U256::from(q.last_posted_block)]),
                1 => model
                    .marginal_price(a, b, s.amount)
                    .map(|q| vec![q.numerator, q.denominator]),
                2 => model.limits(a, b).map(|q| vec![q.sell, q.buy]),
                3 => model
                    .quote_execution_exact_input(a, b, s.amount, &context)
                    .map(|q| vec![q.amount_out]),
                4 => model
                    .quote_exact_output(a, b, s.amount, &context)
                    .map(|q| vec![q.amount_in]),
                _ => panic!("unknown sample"),
            };
            let prepared = model.prepare(a, b).and_then(|pair| match s.kind {
                0 => pair
                    .quote_exact_input(s.amount)
                    .map(|q| vec![q.amount_out, U256::from(q.last_posted_block)]),
                1 => pair
                    .marginal_price(s.amount)
                    .map(|q| vec![q.numerator, q.denominator]),
                2 => pair.limits().map(|q| vec![q.sell, q.buy]),
                3 => pair
                    .quote_execution_exact_input(s.amount, &context)
                    .map(|q| vec![q.amount_out]),
                4 => pair
                    .quote_exact_output(s.amount, &context)
                    .map(|q| vec![q.amount_in]),
                _ => unreachable!(),
            });
            tested[s.kind as usize] += 1;
            assert_eq!(
                prepared.is_ok(),
                s.success,
                "prepared world {world} case {case}: {prepared:?}"
            );
            assert_eq!(got.is_ok(),s.success,"world {world} case {case}, kind {}, pair {}->{}, amount {}, rust {got:?}, solidity {}",s.kind,s.inputIndex,s.outputIndex,s.amount,s.result);
            if !s.success {
                continue;
            }
            successes[s.kind as usize] += 1;
            let expected = match s.kind {
                0 => {
                    let (amount, block) = <(U256, U256)>::abi_decode_validate(&s.result).unwrap();
                    vec![amount, block]
                }
                1 => {
                    let prices =
                        Vec::<support::ContractFraction>::abi_decode_validate(&s.result).unwrap();
                    vec![prices[0].numerator, prices[0].denominator]
                }
                2 => Vec::<U256>::abi_decode_validate(&s.result).unwrap(),
                3 => vec![U256::abi_decode_validate(&s.result).unwrap()],
                4 => vec![
                    support::Trade::abi_decode_validate(&s.result)
                        .unwrap()
                        .calculatedAmount,
                ],
                _ => unreachable!(),
            };
            assert_eq!(got.unwrap(), expected, "world {world} case {case}");
            assert_eq!(
                prepared.unwrap(),
                expected,
                "prepared world {world} case {case}"
            );
        }
    }
    assert!(tested.iter().all(|n| *n == 1024));
    assert!(successes.iter().all(|n| *n > 100));
    println!("Solidity cases by surface: {tested:?}; exact-value successes: {successes:?}");
}
#[test]
fn packed_word_validation_checks_every_pair_and_covariance_dependency() {
    use std::collections::BTreeSet;
    use thogamm_model::state::{covariance_location, pair_location};
    let f = support::fixture();
    for set in &f.samples[..4] {
        let model = support::model(f.proxy, f.baseFee, &set.snapshot);
        let mut required = BTreeSet::new();
        for b in 1..model.state().tokens.len() {
            for a in 0..b {
                required.insert(pair_location(a, b).0);
            }
        }
        for b in 1..model.state().categories.len() {
            for a in 1..=b {
                required.insert(covariance_location(a, b).0);
            }
        }
        for slot in required {
            let mut state = model.state().clone();
            state.words.remove(&slot);
            assert!(
                matches!(state.validate(), Err(Error::MissingWord(s)) if s == slot),
                "missing {slot}"
            );
        }
    }
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
fn event_images_equal_contract_exports_word_for_word_and_balance_for_balance() {
    let f = support::fixture();
    for (index, event) in f.events.iter().enumerate() {
        let before = support::model(f.proxy, f.baseFee, &event.beforeState);
        let after = support::model(f.proxy, f.baseFee, &event.afterState);
        let logs = support::logs(event, f.baseFee);
        let actual = events::apply_block(before.state(), support::model_header(&after), &logs);
        assert_eq!(&actual.unwrap(), after.state(), "event stage {index}");
    }
}
#[test]
fn pages_must_cover_one_coherent_block_and_every_token() {
    let f = support::fixture();
    let page = support::page(&f.samples[3].snapshot);
    let block = support::header(page.blockNumber.to::<u64>(), f.baseFee);
    let mut pages: Vec<_> = f.singleTokenPages.iter().map(support::page).collect();
    assert!(pages.iter().all(|p| p.words.len() < page.words.len()));
    let complete = PoolState::from_pages(f.proxy, block.clone(), pages.clone()).unwrap();
    assert_eq!(complete.tokens.len(), 64);
    assert_eq!(
        &complete,
        support::model(f.proxy, f.baseFee, &f.samples[3].snapshot).state()
    );
    pages[8].words[0].value ^= U256::ONE;
    assert!(PoolState::from_pages(f.proxy, block.clone(), pages.clone()).is_err());
    pages[8].words[0].value ^= U256::ONE;
    pages[9].blockNumber += U256::ONE;
    assert!(PoolState::from_pages(f.proxy, block.clone(), pages.clone()).is_err());
    pages[9].blockNumber -= U256::ONE;
    pages.pop();
    assert!(PoolState::from_pages(f.proxy, block, pages).is_err());
}
#[test]
fn quote_guards_and_inventory_boundaries_match_contract_rules() {
    let f = support::fixture();
    let base = support::model(f.proxy, f.baseFee, &f.samples[0].snapshot);
    let a = base.state().tokens[0].token;
    let b = base.state().tokens[4].token;
    assert!(base.quote_exact_input(a, b, U256::ZERO).is_err());
    assert!(base.quote_exact_input(a, a, u(100)).is_err());
    let mut s = base.state().clone();
    s.balances[4] = U256::ZERO;
    assert!(PoolModel::new(s)
        .unwrap()
        .quote_exact_input(a, b, u(1_000_000))
        .is_err());
    let mut s = base.state().clone();
    let i1 = s.word(79).unwrap();
    s.words
        .insert(79, (i1 & !mask(56)) | ((U256::ONE << 55usize) - U256::ONE));
    let m = PoolModel::new(s).unwrap();
    assert_eq!(m.limits(a, b).unwrap().sell, U256::ZERO);
    // Read-only maker quoting does not run settlement's signed inventory checks.
    assert!(m.quote_exact_input(a, b, u(1_000_000)).is_ok());
    assert!(m
        .quote_execution_exact_input(
            a,
            b,
            u(1_000_000),
            &ExecutionContext {
                gas_price: U256::ZERO,
                fast_lane_hot: false
            }
        )
        .is_err());
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
fn signed_delta_bound_is_independent_of_resulting_inventory_headroom() {
    let f = support::fixture();
    let mut state = support::model(f.proxy, f.baseFee, &f.samples[0].snapshot).into_state();
    state.words.insert(79, U256::ONE << 55usize); // USD starts at int56::MIN.
    state
        .words
        .insert(77, state.word(77).unwrap() & !(mask(16) << 110usize));
    state.balances[4] = U256::MAX;
    let model = PoolModel::new(state).unwrap();
    let a = model.state().tokens[0].token;
    let b = model.state().tokens[4].token;
    let amount = U256::ONE << 55usize;
    assert!(model.limits(a, b).unwrap().sell >= amount);
    assert!(model.quote_exact_input(a, b, amount).is_ok());
    assert!(matches!(
        model.quote_execution_exact_input(
            a,
            b,
            amount,
            &ExecutionContext {
                gas_price: U256::ZERO,
                fast_lane_hot: false
            }
        ),
        Err(Error::Unavailable("exposure overflow"))
    ));
}
