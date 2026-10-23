#[path = "common/mod.rs"]
mod support;
use state_poll_sdk::{Config, StatePollSdk};
use thogamm_model::{rpc::CallBlock, Error, U256};

#[tokio::test]
async fn one_call_loads_all_registered_tokens_and_unchanged_refresh_is_also_one_call() {
    let f = support::fixture();
    let rpc = support::MockRpc::default();
    rpc.insert(support::page(&f.samples[0].snapshot), f.baseFee);
    let mut sdk = StatePollSdk::with_reader(rpc.clone(), f.proxy, Config::default())
        .await
        .unwrap();
    assert_eq!(rpc.call_count(), 1);
    assert_eq!(
        sdk.model().unwrap().state(),
        support::polled_model(f.proxy, &f.samples[0].snapshot).state()
    );
    assert_eq!(sdk.model().unwrap().max_index(), 7);
    assert!(!sdk.refresh().await.unwrap());
    assert_eq!(rpc.call_count(), 2);
    assert!(sdk.model().unwrap().state().block.hash.is_none());
    assert!(rpc
        .0
        .lock()
        .unwrap()
        .calls
        .iter()
        .all(|(block, _)| *block == CallBlock::Latest));
}
#[tokio::test]
async fn one_call_refreshes_complete_state_without_auxiliary_reads() {
    let f = support::fixture();
    let rpc = support::MockRpc::from_events(&f);
    let mut sdk = StatePollSdk::with_reader(rpc.clone(), f.proxy, Config::default())
        .await
        .unwrap();
    let last = support::page(&f.events[10].afterState);
    rpc.0.lock().unwrap().head = last.blockNumber.to::<u64>();
    assert!(sdk.refresh().await.unwrap());
    assert_eq!(rpc.call_count(), 2);
    assert_eq!(sdk.model().unwrap().max_index(), 7);
    assert_eq!(
        sdk.model().unwrap().state(),
        support::polled_model(f.proxy, &f.events[10].afterState).state()
    );
}
#[tokio::test]
async fn failed_refresh_invalidates_live_access_until_successful_recovery() {
    let f = support::fixture();
    let rpc = support::MockRpc::from_events(&f);
    rpc.0.lock().unwrap().head += 1;
    let mut sdk = StatePollSdk::with_reader(rpc.clone(), f.proxy, Config::default())
        .await
        .unwrap();
    let historical = sdk.model().unwrap().clone();
    let before = historical.state().clone();
    rpc.0.lock().unwrap().head -= 1;
    assert!(matches!(
        sdk.refresh().await,
        Err(Error::ProviderBehind { .. })
    ));
    assert_eq!(rpc.call_count(), 2);
    assert!(matches!(sdk.model(), Err(Error::SnapshotUnavailable)));
    rpc.0.lock().unwrap().fail_calls = true;
    assert!(sdk.refresh().await.is_err());
    assert_eq!(rpc.call_count(), 3);
    assert!(matches!(sdk.model(), Err(Error::SnapshotUnavailable)));
    rpc.0.lock().unwrap().fail_calls = false;
    rpc.0.lock().unwrap().head += 2;
    assert!(sdk.refresh().await.unwrap());
    assert_eq!(rpc.call_count(), 4);
    assert!(sdk.model().is_ok());
    assert_eq!(historical.state(), &before);
}
#[tokio::test]
async fn same_height_replacement_and_fee_changes_are_observed_in_the_single_call() {
    let f = support::fixture();
    let rpc = support::MockRpc::from_events(&f);
    let mut sdk = StatePollSdk::with_reader(rpc.clone(), f.proxy, Config::default())
        .await
        .unwrap();
    let number = sdk.model().unwrap().state().block.number;
    {
        let mut c = rpc.0.lock().unwrap();
        let hash = c.headers[&number].hash;
        let p = c.states.get_mut(&hash).unwrap();
        p.balances[0] += U256::ONE;
        p.portfolioBalances[0] += U256::ONE;
        p.baseFeePerGas *= U256::from(2);
        p.parentBlockHash = support::hash(number - 1, 1);
    }
    assert!(sdk.refresh().await.unwrap());
    assert_eq!(rpc.call_count(), 2);
    assert_eq!(
        sdk.model().unwrap().quote_base_fee_per_gas(),
        f.baseFee * U256::from(2)
    );
    assert_eq!(
        sdk.model().unwrap().state().block.parent_hash,
        support::hash(number - 1, 1)
    );
    assert_eq!(sdk.model().unwrap().state().block.number, number);
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
            ..Config::default()
        },
    )
    .await
    .unwrap();
    let before = sdk.model().unwrap().state().clone();
    for calls in 2..=3 {
        let model = tokio::time::timeout(std::time::Duration::from_secs(1), sdk.next_update())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(model.state(), &before);
        assert_eq!(rpc.call_count(), calls);
    }
}

#[derive(Clone)]
struct StallReader {
    rpc: support::MockRpc,
    stall: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
#[async_trait::async_trait]
impl state_poll_sdk::ChainReader for StallReader {
    async fn call(
        &self,
        to: thogamm_model::Address,
        data: thogamm_model::Bytes,
        block: CallBlock,
    ) -> thogamm_model::Result<thogamm_model::Bytes> {
        if self.stall.load(std::sync::atomic::Ordering::Relaxed) {
            std::future::pending::<()>().await;
        }
        self.rpc.call(to, data, block).await
    }
}
#[tokio::test]
async fn timeout_cancellation_and_unsupported_schema_invalidate_live_state() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::Duration;
    let f = support::fixture();
    let rpc = support::MockRpc::from_events(&f);
    let stall = Arc::new(AtomicBool::new(false));
    let reader = StallReader {
        rpc: rpc.clone(),
        stall: stall.clone(),
    };
    let mut sdk = StatePollSdk::with_reader(
        reader,
        f.proxy,
        Config {
            request_timeout: Duration::from_millis(20),
            ..Config::default()
        },
    )
    .await
    .unwrap();
    let received = sdk.received_at();
    stall.store(true, Ordering::Relaxed);
    assert!(
        tokio::time::timeout(Duration::from_millis(1), sdk.refresh())
            .await
            .is_err()
    );
    assert!(matches!(sdk.model(), Err(Error::SnapshotUnavailable)));
    assert!(matches!(sdk.refresh().await, Err(Error::Transport(_))));
    assert_eq!(sdk.received_at(), received);
    stall.store(false, Ordering::Relaxed);
    assert!(!sdk.refresh().await.unwrap());
    assert!(sdk.model().is_ok());
    {
        let mut inner = rpc.0.lock().unwrap();
        let hash = inner.headers[&inner.head].hash;
        inner.states.get_mut(&hash).unwrap().schemaVersion = 8;
    }
    assert!(matches!(
        sdk.refresh().await,
        Err(Error::UnsupportedSchema(8))
    ));
    assert!(matches!(sdk.model(), Err(Error::SnapshotUnavailable)));
}

#[derive(Clone)]
struct SlowReader(support::MockRpc);
#[async_trait::async_trait]
impl state_poll_sdk::ChainReader for SlowReader {
    async fn call(
        &self,
        to: thogamm_model::Address,
        data: thogamm_model::Bytes,
        block: CallBlock,
    ) -> thogamm_model::Result<thogamm_model::Bytes> {
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        self.0.call(to, data, block).await
    }
}
#[tokio::test(start_paused = true)]
async fn polling_cadence_does_not_accumulate_rpc_duration() {
    use std::time::Duration;
    let f = support::fixture();
    let rpc = support::MockRpc::from_events(&f);
    let mut sdk = StatePollSdk::with_reader(
        SlowReader(rpc.clone()),
        f.proxy,
        Config {
            poll_interval: Duration::from_millis(100),
            ..Config::default()
        },
    )
    .await
    .unwrap();
    sdk.next_update().await.unwrap();
    let first = tokio::time::Instant::now();
    sdk.next_update().await.unwrap();
    assert_eq!(first.elapsed(), Duration::from_millis(100));
    assert_eq!(rpc.call_count(), 3);
}
