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
async fn snapshot_pins_pool_and_issuer_batches_to_one_hash() {
    let f = support::fixture();
    let p = support::page(&f.samples[0].snapshot);
    let number = p.blockNumber.to::<u64>();
    let header = support::header(number, f.baseFee);
    let hash = header.hash;
    let proxy = f.proxy;
    let shares = support::shares_for(&p);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for block in [
            CallBlock::Latest,
            CallBlock::Hash(hash),
            CallBlock::Number(number),
        ] {
            for phase in 0..3 {
                let (mut connection, _) = listener.accept().await.unwrap();
                let request = read_request(&mut connection).await;
                let response = if phase == 0 {
                    assert_eq!(request["method"], if matches!(block, CallBlock::Hash(_)) { "eth_getBlockByHash" } else { "eth_getBlockByNumber" });
                    json!({"jsonrpc":"2.0","id":request["id"],"result":header})
                } else {
                    let calls = request.as_array().unwrap();
                    assert_eq!(calls.len(), if phase == 1 {1} else {10});
                    let mut responses = Vec::new();
                    for call in calls {
                        assert_eq!(call["method"], "eth_call");
                        assert_eq!(call["params"][0]["gasPrice"], "0x1");
                        assert_eq!(call["params"][1], CallBlock::Hash(hash).rpc_value());
                        let to: thogamm_model::Address = serde_json::from_value(call["params"][0]["to"].clone()).unwrap();
                        let data: thogamm_model::Bytes = serde_json::from_value(call["params"][0]["data"].clone()).unwrap();
                        let result = if phase == 1 {
                            assert_eq!(call["params"], pool_data_params(proxy, CallBlock::Hash(hash)));
                            abi::getPoolDataCall::abi_encode_returns(&p)
                        } else if data.starts_with(&abi::sharesOfCall::SELECTOR) {
                            assert_eq!(abi::sharesOfCall::abi_decode_validate(&data).unwrap().holder, proxy);
                            abi::sharesOfCall::abi_encode_returns(&shares[&to].shares)
                        } else {
                            assert_eq!(data.as_ref(), abi::getCurrentMultiplierCall {}.abi_encode());
                            abi::getCurrentMultiplierCall::abi_encode_returns(&abi::getCurrentMultiplierReturn {
                                multiplier:shares[&to].multiplier, activation:thogamm_model::U256::ZERO, period:thogamm_model::U256::ZERO
                            })
                        };
                        responses.push(json!({"jsonrpc":"2.0","id":call["id"],"result":thogamm_model::Bytes::from(result)}));
                    }
                    responses.reverse(); // Batch response order is not request order.
                    json!(responses)
                }.to_string();
                connection.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).as_bytes()).await.unwrap();
            }
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let rpc = HttpRpc::new(url).unwrap();
        let expected = support::model(f.proxy, f.baseFee, &f.samples[0].snapshot);
        for block in [
            CallBlock::Latest,
            CallBlock::Hash(hash),
            CallBlock::Number(number),
        ] {
            assert_eq!(
                snapshot(&rpc, f.proxy, block).await.unwrap().state(),
                expected.state()
            );
        }
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
        assert_eq!(
            read_request(&mut connection).await["method"],
            "eth_getBlockByNumber"
        );
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
