//! Replaying a block's logs must reproduce the state read from the chain at
//! that block.
#[path = "../../tests/common/mod.rs"]
mod common;
use alloy_sol_types::sol;
use common::*;
use serde_json::Value;
use thogamm::{
    abi::SolValue,
    events::{apply_block, BlockHeader, Log},
    layout, Error, Pool, Snapshot, U256,
};

fn header(block: &Value) -> BlockHeader {
    serde_json::from_value(block["header"].clone()).unwrap()
}

fn logs(block: &Value) -> Vec<Log> {
    serde_json::from_value(block["logs"].clone()).unwrap()
}

fn read_at(f: &Value, block: &Value) -> Snapshot {
    let mut snapshot = snapshot(address(&f["proxy"]), block);
    snapshot.block.hash = Some(header(block).hash);
    snapshot
}

/// Storage words 122..=127 configure the pool's own hedging account. Quotes
/// never read them and no event carries them, so replay leaves them as read.
fn quote_words(snapshot: &Snapshot) -> &[U256] {
    &snapshot.words[..122 - usize::from(layout::FIRST_SLOT)]
}

fn assert_same_state(replayed: &Snapshot, read: &Snapshot, exact_words: bool) {
    let block = read.block.number;
    assert_eq!(replayed.block, read.block);
    assert_eq!(replayed.tokens, read.tokens, "tokens at {block}");
    assert_eq!(replayed.balances, read.balances, "balances at {block}");
    assert_eq!(
        replayed.native_balance, read.native_balance,
        "native balance at {block}"
    );
    assert_eq!(replayed.xstocks, read.xstocks, "xStocks at {block}");
    if exact_words {
        assert_eq!(replayed.words, read.words, "storage at {block}");
    } else {
        assert_eq!(
            quote_words(replayed),
            quote_words(read),
            "storage at {block}"
        );
    }
}

fn replay(name: &str, exact_words: bool) -> (usize, usize) {
    let f = fixture(name);
    let blocks = f["blocks"].as_array().unwrap();
    let mut snapshot = read_at(&f, &blocks[0]);
    let mut log_count = 0;
    for block in &blocks[1..] {
        let logs = logs(block);
        log_count += logs.len();
        snapshot = apply_block(snapshot, &header(block), &logs).unwrap();
        assert_same_state(&snapshot, &read_at(&f, block), exact_words);
    }
    (blocks.len(), log_count)
}

#[test]
fn two_hundred_blocks_of_the_thirteen_token_pool() {
    assert_eq!(replay("events-v8.json.gz", true), (201, 220));
}

#[test]
fn fifty_blocks_of_the_seventeen_token_pool() {
    assert_eq!(
        replay("events-stored-multipliers.json.gz", false),
        (51, 505)
    );
}

#[test]
fn replayed_state_quotes_like_the_state_read_from_the_chain() {
    let f = fixture("events-stored-multipliers.json.gz");
    let blocks = f["blocks"].as_array().unwrap();
    let mut snapshot = read_at(&f, &blocks[0]);
    for block in &blocks[1..] {
        snapshot = apply_block(snapshot, &header(block), &logs(block)).unwrap();
    }
    let replayed = Pool::new(snapshot).unwrap();
    let read = Pool::new(read_at(&f, blocks.last().unwrap())).unwrap();
    for input in read.tokens() {
        for output in read.tokens() {
            for amount in [
                U256::from(1),
                U256::from(10).pow(U256::from(input.decimals)),
            ] {
                let quote = |pool: &Pool| {
                    pool.pair(input.address, output.address)
                        .and_then(|pair| pair.maker_quote_exact_input(amount))
                        .ok()
                };
                assert_eq!(
                    quote(&replayed),
                    quote(&read),
                    "{} -> {}",
                    input.address,
                    output.address
                );
            }
        }
    }
}

#[test]
fn a_block_is_applied_whole_or_not_at_all() {
    let f = fixture("events-v8.json.gz");
    let blocks = f["blocks"].as_array().unwrap();
    let (index, block) = blocks
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, b)| logs(b).len() > 1)
        .unwrap();
    let before = read_at(&f, &blocks[index - 1]);
    let good = logs(block);

    let mut repeated = good.clone();
    repeated.push(good[0].clone());
    assert!(apply_block(before.clone(), &header(block), &repeated).is_err());

    let mut removed = good.clone();
    removed[0].removed = true;
    assert!(matches!(
        apply_block(before.clone(), &header(block), &removed),
        Err(Error::Discontinuous)
    ));

    // A block that does not follow the snapshot's block.
    let later = &blocks[index + 1];
    assert!(matches!(
        apply_block(before.clone(), &header(later), &logs(later)),
        Err(Error::Discontinuous)
    ));

    assert!(apply_block(before, &header(block), &good).is_ok());
}

sol! {
    struct CapturedLog { bytes32[] topics; bytes data; address emitter; }
    struct Stage { bytes beforeState; bytes afterState; CapturedLog[] logs; }
}

/// Parameter updates recorded on a fork: risk, token cost and cap updates,
/// applied one per block.
#[test]
fn parameter_updates_reproduce_storage_bit_for_bit() {
    let stages = Vec::<Stage>::abi_decode_validate(&gunzip("events-fork-v8.bin.gz")).unwrap();
    assert_eq!(stages.len(), 3);
    let reference = fixture("deployed-v8.json.gz");
    let pool = address(&reference["proxy"]);
    let xstocks = xstocks(&reference["snapshots"][0]);
    let decode = |state: &[u8]| {
        let entry = serde_json::json!({ "snapshot": thogamm::Bytes::copy_from_slice(state) });
        snapshot_from(pool, pool_data(&entry), &xstocks)
    };
    for (index, stage) in stages.iter().enumerate() {
        let mut before = decode(&stage.beforeState);
        let read = decode(&stage.afterState);
        before.block.hash = Some(hash(before.block.number));
        let header = BlockHeader {
            number: read.block.number,
            hash: hash(read.block.number),
            parent_hash: hash(before.block.number),
            base_fee_per_gas: read.block.base_fee,
        };
        let logs: Vec<Log> = stage
            .logs
            .iter()
            .enumerate()
            .map(|(position, log)| Log {
                address: log.emitter,
                topics: log.topics.clone(),
                data: log.data.clone(),
                block_hash: header.hash,
                block_number: header.number,
                transaction_index: 0,
                log_index: position as u64,
                removed: false,
            })
            .collect();
        let replayed = apply_block(before, &header, &logs).unwrap();
        assert_eq!(replayed.words, read.words, "stage {index}");
        assert_eq!(replayed.balances, read.balances, "stage {index}");
    }
}
