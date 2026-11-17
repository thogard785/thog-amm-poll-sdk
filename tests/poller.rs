mod common;
use common::{node::Node, *};
use thogamm_poll::{thogamm::Error, Poller, U256};

fn node() -> Node {
    let f = fixture("deployed-stored-multipliers.json.gz");
    Node::new(address(&f["proxy"]), &f["snapshots"][0])
}

#[tokio::test]
async fn connect_reads_the_whole_pool_and_each_refresh_is_one_request() {
    let node = node();
    let mut poller = Poller::with_rpc(node.clone(), node.pool).await.unwrap();
    assert_eq!(
        node.requests(),
        2,
        "the first read also learns the token list"
    );
    assert_eq!(poller.pool().tokens().len(), 17);
    assert_eq!(
        poller
            .pool()
            .tokens()
            .iter()
            .filter(|t| t.xstock.is_some())
            .count(),
        9
    );

    assert!(!poller.refresh().await.unwrap(), "nothing changed");
    assert_eq!(node.requests(), 3);

    let mut next = node.data();
    next.blockNumber += U256::from(1);
    node.set(next);
    assert!(poller.refresh().await.unwrap());
    assert_eq!(node.requests(), 4);
}

#[tokio::test]
async fn a_failed_or_older_read_keeps_the_previous_state() {
    let node = node();
    let mut poller = Poller::with_rpc(node.clone(), node.pool).await.unwrap();
    let before = poller.pool().snapshot().clone();

    *node.fail.lock().unwrap() = true;
    assert!(matches!(poller.refresh().await, Err(Error::Rpc(_))));
    assert_eq!(poller.pool().snapshot(), &before);
    *node.fail.lock().unwrap() = false;

    let mut older = node.data();
    older.blockNumber -= U256::from(1);
    node.set(older);
    assert!(matches!(
        poller.refresh().await,
        Err(Error::NodeBehind { .. })
    ));
    assert_eq!(poller.pool().snapshot(), &before);
}
