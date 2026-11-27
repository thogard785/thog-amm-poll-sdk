//! Every recorded contract call must give the same result here: the same
//! integers on success, and a rejection wherever the contract reverted.
#[path = "../../tests/common/mod.rs"]
mod common;
use common::*;
use thogamm::{
    abi::{self, SolValue},
    TxContext, U256,
};

/// Runs one fixture file and returns (calls, calls the contract accepted).
fn run(name: &str) -> (usize, usize) {
    let f = fixture(name);
    let (mut calls, mut accepted) = (0, 0);
    for entry in f["snapshots"].as_array().unwrap() {
        let pool = pool(address(&f["proxy"]), entry);
        for case in entry["cases"].as_array().unwrap() {
            let token = |key: &str| pool.tokens()[case[key].as_u64().unwrap() as usize].address;
            let amount = uint(&case["amount"]);
            let tx = || TxContext {
                gas_price: uint(&case["gasPrice"]),
                auction_handler_warm: case["auctionHandlerWarm"].as_bool().unwrap(),
            };
            let result = pool
                .pair(token("inputIndex"), token("outputIndex"))
                .and_then(|pair| {
                    Ok(match case["method"].as_str().unwrap() {
                        "quote" => {
                            let quote = pair.maker_quote_exact_input(amount)?;
                            (quote.amount_out, U256::from(quote.price_block)).abi_encode()
                        }
                        "price" => {
                            let price = pair.price(amount)?;
                            vec![abi::Fraction {
                                numerator: price.numerator,
                                denominator: price.denominator,
                            }]
                            .abi_encode()
                        }
                        "limits" => {
                            let limits = pair.get_limits()?;
                            vec![limits.sell, limits.buy].abi_encode()
                        }
                        "execution" => pair
                            .maker_swap_exact_input(amount, tx())?
                            .amount_out
                            .abi_encode(),
                        "exactOutput" => pair.swap_buy(amount, tx())?.amount_in.abi_encode(),
                        other => panic!("unknown method {other}"),
                    })
                });
            let expected = case["success"].as_bool().unwrap();
            assert_eq!(
                result.is_ok(),
                expected,
                "{name}: {case}\n  here: {result:?}"
            );
            if let Ok(result) = result {
                let recorded = bytes(&case["result"]);
                // A swap returns more than the amount; the amount comes first.
                assert_eq!(result, recorded[..result.len()], "{name}: {case}");
                accepted += 1;
            }
            calls += 1;
        }
    }
    (calls, accepted)
}

#[test]
fn deployed_pool_with_eighteen_tokens() {
    assert_eq!(run("deployed-18.json.gz"), (18 * 17 * 12, 1816));
}

#[test]
fn deployed_pool_with_seventeen_tokens() {
    assert_eq!(
        run("deployed-stored-multipliers.json.gz"),
        (17 * 16 * 12, 2033)
    );
}

#[test]
fn deployed_pool_with_thirteen_tokens() {
    assert_eq!(run("deployed-v8.json.gz"), (1872, 954));
}

#[test]
fn pricing_and_inventory_boundaries() {
    assert_eq!(run("boundaries-v8.json.gz"), (24024, 9982));
}

#[test]
fn swaps_and_exact_output_under_every_surcharge_tier() {
    assert_eq!(run("execution-model3.json.gz"), (32760, 16428));
}
