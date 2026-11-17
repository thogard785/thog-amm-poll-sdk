//! Reading a snapshot: one call, every value from the same block.
#[path = "../../tests/common/mod.rs"]
mod common;
use common::{node::Node, *};
use thogamm::{
    rpc::{eth_call_params, read_snapshot, snapshot_call, BlockId, EthCall, HttpRpc},
    Error, Snapshot,
};

fn recorded() -> (Node, Snapshot) {
    let f = fixture("deployed-stored-multipliers.json.gz");
    let pool = address(&f["proxy"]);
    (
        Node::new(pool, &f["snapshots"][0]),
        snapshot(pool, &f["snapshots"][0]),
    )
}

#[tokio::test]
async fn first_read_learns_the_token_list_and_later_reads_take_one_request() {
    let (node, expected) = recorded();
    let first = read_snapshot(&node, node.pool, BlockId::Latest, &[])
        .await
        .unwrap();
    assert_eq!(first, expected);
    assert_eq!(first.tokens.len(), 17);
    assert_eq!(first.xstocks.iter().flatten().count(), 9);
    assert_eq!(node.requests(), 2);

    let second = read_snapshot(&node, node.pool, BlockId::Latest, &first.tokens)
        .await
        .unwrap();
    assert_eq!(second, expected);
    assert_eq!(node.requests(), 3);
}

#[tokio::test]
async fn a_stale_token_list_is_replaced_not_trusted() {
    let (node, expected) = recorded();
    let mut stale = expected.tokens.clone();
    stale.swap(0, 1);
    let read = read_snapshot(&node, node.pool, BlockId::Latest, &stale)
        .await
        .unwrap();
    assert_eq!(read, expected);
    assert_eq!(node.requests(), 2);
}

#[tokio::test]
async fn a_read_pinned_to_a_number_must_come_from_that_block() {
    let (node, expected) = recorded();
    let pinned = BlockId::Number(expected.block.number);
    assert_eq!(
        read_snapshot(&node, node.pool, pinned, &expected.tokens)
            .await
            .unwrap(),
        expected
    );
    let mut moved = node.data();
    moved.blockNumber += thogamm::U256::from(1);
    let lying = Node {
        state: std::sync::Arc::new(std::sync::Mutex::new((
            moved,
            xstocks(&fixture("deployed-stored-multipliers.json.gz")["snapshots"][0]),
        ))),
        ..node.clone()
    };
    struct Relabel(Node);
    impl EthCall for Relabel {
        async fn eth_call(
            &self,
            to: thogamm::Address,
            data: thogamm::Bytes,
            _: BlockId,
        ) -> thogamm::Result<thogamm::Bytes> {
            self.0.eth_call(to, data, BlockId::Latest).await
        }
    }
    let result = read_snapshot(&Relabel(lying), node.pool, pinned, &expected.tokens).await;
    assert!(matches!(result, Err(Error::Rpc(_))), "{result:?}");
}

#[tokio::test]
async fn shares_and_multipliers_from_another_block_are_refused() {
    let (node, expected) = recorded();
    let (index, _) = expected
        .xstocks
        .iter()
        .enumerate()
        .find(|(_, x)| x.as_ref().is_some_and(|x| !x.shares.is_zero()))
        .unwrap();
    let token = expected.tokens[index];
    node.state.lock().unwrap().1.get_mut(&token).unwrap().shares +=
        thogamm::U256::from(10).pow(thogamm::U256::from(18));
    let result = read_snapshot(&node, node.pool, BlockId::Latest, &expected.tokens).await;
    assert!(
        matches!(result, Err(Error::InvalidSnapshot(_))),
        "{result:?}"
    );
}

/// The HTTP client sends a plain eth_call that keeps the block's base fee.
#[tokio::test]
async fn http_client_sends_the_documented_request() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (node, expected) = recorded();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (to, data) = snapshot_call(node.pool, &expected.tokens);
    let expected_params = eth_call_params(to, &data, BlockId::Latest);
    let output = node
        .eth_call(to, data.clone(), BlockId::Latest)
        .await
        .unwrap();
    let server = tokio::spawn(async move {
        let (mut connection, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let body = loop {
            let mut chunk = [0u8; 65536];
            let read = connection.read(&mut chunk).await.unwrap();
            assert!(read > 0);
            request.extend_from_slice(&chunk[..read]);
            let text = String::from_utf8_lossy(&request).to_string();
            if let Some((head, body)) = text.split_once("\r\n\r\n") {
                let length: usize = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap();
                if body.len() >= length {
                    break body.to_owned();
                }
            }
        };
        let request: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(request["method"], "eth_call");
        assert_eq!(request["params"], expected_params);
        assert_eq!(request["params"][0]["gasPrice"], "0x1");
        let reply = serde_json::json!({ "jsonrpc": "2.0", "id": request["id"], "result": output })
            .to_string();
        connection
            .write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}", reply.len()).as_bytes())
            .await
            .unwrap();
    });
    let read = read_snapshot(
        &HttpRpc::new(url).unwrap(),
        node.pool,
        BlockId::Latest,
        &expected.tokens,
    )
    .await
    .unwrap();
    assert_eq!(read, expected);
    server.await.unwrap();
}
