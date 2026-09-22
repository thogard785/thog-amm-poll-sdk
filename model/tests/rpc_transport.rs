#[path = "../../tests/common/mod.rs"]
mod support;
use alloy_sol_types::SolCall;
use serde_json::{json, Value};
use thogamm_model::{
    abi,
    rpc::{pool_data_params, snapshot, CallBlock, HttpRpc},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test]
async fn each_snapshot_is_one_wire_request_for_the_complete_registry() {
    let f = support::fixture();
    let p = support::page(&f.samples[3].snapshot);
    let hash = support::hash(p.blockNumber.to::<u64>(), 0);
    let number = p.blockNumber.to::<u64>();
    let proxy = f.proxy;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let result = json!(thogamm_model::Bytes::from(
        abi::getPoolDataCall::abi_encode_returns(&p)
    ));
    let server = tokio::spawn(async move {
        for block in [
            CallBlock::Latest,
            CallBlock::Hash(hash),
            CallBlock::Number(number),
        ] {
            let (mut connection, _) = listener.accept().await.unwrap();
            let request = read_request(&mut connection).await;
            assert!(request.is_object(), "JSON-RPC batching is forbidden");
            assert_eq!(request["method"], "eth_call");
            assert_eq!(request["params"], pool_data_params(proxy, block));
            // Without this, Monad can expose BASEFEE=0 in the returned state.
            assert_eq!(request["params"][0]["gasPrice"], "0x1");
            assert_eq!(request["params"][1], block.rpc_value());
            assert_eq!(
                request["params"][0]["data"],
                json!(thogamm_model::Bytes::from(
                    abi::getPoolDataCall {
                        startTokenIndex: 0,
                        stopTokenIndex: 64
                    }
                    .abi_encode()
                ))
            );
            let response = json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string();
            connection.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).as_bytes()).await.unwrap();
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let rpc = HttpRpc::new(url).unwrap();
        let latest = snapshot(&rpc, f.proxy, CallBlock::Latest).await.unwrap();
        assert_eq!(
            latest.state(),
            support::polled_model(f.proxy, &f.samples[3].snapshot).state()
        );
        let pinned = snapshot(&rpc, f.proxy, CallBlock::Hash(hash))
            .await
            .unwrap();
        assert_eq!(
            pinned.state(),
            support::model(f.proxy, f.baseFee, &f.samples[3].snapshot).state()
        );
        let numbered = snapshot(&rpc, f.proxy, CallBlock::Number(number))
            .await
            .unwrap();
        assert_eq!(numbered.state(), latest.state());
        server.await.unwrap();
    })
    .await
    .unwrap();
}

async fn read_request(connection: &mut tokio::net::TcpStream) -> Value {
    let mut bytes = Vec::new();
    let body_start;
    loop {
        let mut chunk = [0; 2048];
        let n = connection.read(&mut chunk).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(pos) = bytes.windows(4).position(|x| x == b"\r\n\r\n") {
            body_start = pos + 4;
            break;
        }
    }
    let headers = String::from_utf8_lossy(&bytes[..body_start]);
    let length: usize = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|s| s.trim().parse().unwrap())
        })
        .unwrap();
    while bytes.len() < body_start + length {
        let mut chunk = [0; 4096];
        let n = connection.read(&mut chunk).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
    }
    serde_json::from_slice(&bytes[body_start..body_start + length]).unwrap()
}

#[tokio::test]
async fn http_redirect_is_reported_without_a_second_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let redirect = format!("{url}/redirected");
    let server = tokio::spawn(async move {
        let (mut connection, _) = listener.accept().await.unwrap();
        assert_eq!(read_request(&mut connection).await["method"], "eth_call");
        connection.write_all(format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: {redirect}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        drop(connection);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept())
                .await
                .is_err(),
            "HTTP redirect issued another request"
        );
    });
    let rpc = HttpRpc::new(url).unwrap();
    assert!(
        snapshot(&rpc, thogamm_model::Address::ZERO, CallBlock::Latest)
            .await
            .is_err()
    );
    server.await.unwrap();
}
