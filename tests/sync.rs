#[path = "common/mod.rs"]
mod support;
use state_poll_sdk::{Config, StatePollSdk};
use thogamm_model::{rpc::CallBlock, Error, U256};

#[tokio::test]
async fn one_call_loads_all_64_tokens_and_unchanged_refresh_is_also_one_call() {
    let f = support::fixture();
    let rpc = support::MockRpc::default();
    rpc.insert(support::page(&f.samples[3].snapshot), f.baseFee);
    let mut sdk = StatePollSdk::with_reader(rpc.clone(), f.proxy, Config::default())
        .await
        .unwrap();
    assert_eq!(rpc.call_count(), 1);
    assert_eq!(
        sdk.model().state(),
        support::polled_model(f.proxy, &f.samples[3].snapshot).state()
    );
    assert_eq!(sdk.model().max_index(), 63);
    assert!(!sdk.refresh().await.unwrap());
    assert_eq!(rpc.call_count(), 2);
    assert!(sdk.model().state().block.hash.is_none());
    assert!(rpc
        .0
        .lock()
        .unwrap()
        .calls
        .iter()
        .all(|(block, _)| *block == CallBlock::Latest));
}
#[tokio::test]
async fn one_call_discovers_new_listings_without_max_index_or_pagination() {
    let f = support::fixture();
    let rpc = support::MockRpc::from_events(&f);
    let mut sdk = StatePollSdk::with_reader(rpc.clone(), f.proxy, Config::default())
        .await
        .unwrap();
    let last = support::page(&f.events[10].afterState);
    rpc.0.lock().unwrap().head = last.blockNumber.to::<u64>();
    assert!(sdk.refresh().await.unwrap());
    assert_eq!(rpc.call_count(), 2);
    assert_eq!(sdk.model().max_index(), 17);
    assert_eq!(
        sdk.model().state(),
        support::polled_model(f.proxy, &f.events[10].afterState).state()
    );
}
#[tokio::test]
async fn failures_and_provider_lag_do_not_retry_or_replace_the_model() {
    let f = support::fixture();
    let rpc = support::MockRpc::from_events(&f);
    rpc.0.lock().unwrap().head += 1;
    let mut sdk = StatePollSdk::with_reader(rpc.clone(), f.proxy, Config::default())
        .await
        .unwrap();
    let before = sdk.model().state().clone();
    rpc.0.lock().unwrap().head -= 1;
    assert!(matches!(
        sdk.refresh().await,
        Err(Error::ProviderBehind { .. })
    ));
    assert_eq!(rpc.call_count(), 2);
    assert_eq!(sdk.model().state(), &before);
    rpc.0.lock().unwrap().fail_calls = true;
    assert!(sdk.refresh().await.is_err());
    assert_eq!(rpc.call_count(), 3);
    assert_eq!(sdk.model().state(), &before);
    rpc.0.lock().unwrap().fail_calls = false;
    rpc.0.lock().unwrap().head += 2;
    assert!(sdk.refresh().await.unwrap());
    assert_eq!(rpc.call_count(), 4);
}
#[tokio::test]
async fn same_height_replacement_and_fee_changes_are_observed_in_the_single_call() {
    let f = support::fixture();
    let rpc = support::MockRpc::from_events(&f);
    let mut sdk = StatePollSdk::with_reader(rpc.clone(), f.proxy, Config::default())
        .await
        .unwrap();
    let number = sdk.model().state().block.number;
    {
        let mut c = rpc.0.lock().unwrap();
        let hash = c.headers[&number].hash;
        let p = c.states.get_mut(&hash).unwrap();
        p.balances[0] += U256::ONE;
        p.baseFeePerGas *= U256::from(2);
        p.parentBlockHash = support::hash(number - 1, 1);
    }
    assert!(sdk.refresh().await.unwrap());
    assert_eq!(rpc.call_count(), 2);
    assert_eq!(
        sdk.model().quote_base_fee_per_gas(),
        f.baseFee * U256::from(2)
    );
    assert_eq!(
        sdk.model().state().block.parent_hash,
        support::hash(number - 1, 1)
    );
    assert_eq!(sdk.model().state().block.number, number);
}

#[tokio::test]
async fn next_update_returns_after_one_call_even_when_state_is_unchanged() {
    let f = support::fixture();
    let rpc = support::MockRpc::from_events(&f);
    let mut sdk = StatePollSdk::with_reader(
        rpc.clone(),
        f.proxy,
        Config {
            poll_interval: std::time::Duration::from_millis(1),
        },
    )
    .await
    .unwrap();
    let before = sdk.model().state().clone();
    for calls in 2..=3 {
        let model = tokio::time::timeout(std::time::Duration::from_secs(1), sdk.next_update())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(model.state(), &before);
        assert_eq!(rpc.call_count(), calls);
    }
}
