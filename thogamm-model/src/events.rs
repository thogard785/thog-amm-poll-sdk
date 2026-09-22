//! Absolute maker checkpoints plus ERC-20 balance deltas, applied atomically by block.
use crate::{
    abi,
    math::{add, mask, sub},
    Address, BlockHeader, Error, PoolState, Result, RpcLog, U256,
};
use alloy_sol_types::SolEvent;

fn decode<E: SolEvent>(log: &RpcLog) -> Result<E> {
    E::decode_raw_log_validate(log.topics.iter().copied(), &log.data)
        .map_err(|e| Error::Abi(e.to_string()))
}
fn set_v3(state: &mut PoolState, v3: U256) -> Result<()> {
    let stored = (v3 & mask(45)) | (((v3 >> 48usize) & mask(2)) << 45usize);
    let clear = (mask(32) << 144usize) | (mask(15) << 240usize);
    state.words.insert(
        76,
        (state.word(76)? & !clear)
            | ((stored & mask(32)) << 144usize)
            | ((stored >> 32usize) << 240usize),
    );
    Ok(())
}
/// Returns a new image only after every log was checked. Never double-count swap events
/// alongside Transfer: maker/taker events are inventory checkpoints, not balance deltas.
pub fn apply_block(previous: &PoolState, block: BlockHeader, logs: &[RpcLog]) -> Result<PoolState> {
    apply_owned_block(previous.clone(), block, logs)
}
/// Apply a complete block to an owned, unpublished candidate. Ownership avoids
/// copying the image at each block of a finalized ancestry; failure discards the
/// candidate, so the caller's published model remains unchanged.
pub fn apply_owned_block<'a>(
    mut next: PoolState,
    block: BlockHeader,
    logs: impl IntoIterator<Item = &'a RpcLog>,
) -> Result<PoolState> {
    if block.number != next.block.number.checked_add(1).ok_or(Error::Arithmetic)?
        || Some(block.parent_hash) != next.block.hash
    {
        return Err(Error::Discontinuous);
    }
    let mut previous_position = None;
    for log in logs {
        if log.removed || log.block_hash != block.hash || log.block_number != block.number {
            return Err(Error::Discontinuous);
        }
        let position = (log.transaction_index, log.log_index);
        if previous_position.is_some_and(|p| p >= position) {
            return Err(Error::InvalidData("logs must be unique and ordered".into()));
        }
        previous_position = Some(position);
        let Some(topic) = log.topics.first() else {
            continue;
        };
        if log.address == next.proxy {
            if *topic == abi::Upgraded::SIGNATURE_HASH {
                return Err(Error::ContractUpgraded);
            }
            if *topic == abi::MakerStorageUpdated::SIGNATURE_HASH {
                let e = decode::<abi::MakerStorageUpdated>(log)?;
                if e.slot != 83
                    && !(264..320).contains(&e.slot)
                    && !(325..384).contains(&e.slot)
                    && !(384..510).contains(&e.slot)
                    && !(512..592).contains(&e.slot)
                {
                    return Err(Error::InvalidData(
                        "unexpected registry storage slot".into(),
                    ));
                }
                next.words.insert(e.slot, e.value);
            } else if *topic == abi::MakerTokenAdded::SIGNATURE_HASH {
                let e = decode::<abi::MakerTokenAdded>(log)?;
                if usize::from(e.tokenIndex) != next.tokens.len() || e.token != e.metadata.token {
                    return Err(Error::InvalidData("non-append listing".into()));
                }
                let category = usize::from(e.metadata.category);
                if category == next.categories.len() {
                    next.categories.push(e.categoryMetadata);
                } else if next.categories.get(category) != Some(&e.categoryMetadata) {
                    return Err(Error::InvalidData(
                        "listing changed category metadata".into(),
                    ));
                }
                next.tokens.push(e.metadata);
                // Transfers before listing are already included in this checkpoint.
                next.balances.push(e.balance);
            } else if *topic == abi::MakerPriceStateUpdated::SIGNATURE_HASH {
                let e = decode::<abi::MakerPriceStateUpdated>(log)?;
                next.words.insert(75, e.v1);
                if !e.auxiliary.is_zero() {
                    next.words.insert(
                        76,
                        (next.word(76)? & !mask(144)) | (e.auxiliary & mask(144)),
                    );
                    set_v3(&mut next, e.auxiliary >> 144usize)?;
                }
            } else if *topic == abi::MakerRiskStateUpdatedV4::SIGNATURE_HASH {
                let e = decode::<abi::MakerRiskStateUpdatedV4>(log)?;
                next.words.insert(75, e.v1);
                next.words.insert(77, e.r1);
                next.words.insert(79, e.i1);
                next.words.insert(81, e.p1);
                next.words.insert(82, e.p2);
                next.words.insert(
                    76,
                    (next.word(76)? & !(mask(64) << 176usize)) | ((e.r2 >> 32usize) << 176usize),
                );
                set_v3(&mut next, e.v3)?;
                let retained = next.word(80)? & !mask(152);
                next.words.insert(
                    80,
                    e.r3 | ((e.r2 & mask(32)) << 64usize)
                        | (e.i2 << 96usize)
                        | retained
                        | if e.fields & 6 == 6 {
                            U256::ONE << 255usize
                        } else {
                            U256::ZERO
                        },
                );
            } else if *topic == abi::MakerInventoryUpdated::SIGNATURE_HASH {
                next.words
                    .insert(79, decode::<abi::MakerInventoryUpdated>(log)?.i1);
            } else if *topic == abi::MakerInventoryUpdatedV3::SIGNATURE_HASH {
                let e = decode::<abi::MakerInventoryUpdatedV3>(log)?;
                next.words.insert(79, e.i1);
                next.words.insert(
                    80,
                    (next.word(80)? & !(mask(56) << 96usize)) | (e.i2 << 96usize),
                );
            } else if *topic == abi::MakerWmonBalanceCheckpoint::SIGNATURE_HASH {
                next.balances[3] = decode::<abi::MakerWmonBalanceCheckpoint>(log)?.balance;
            } else if *topic == abi::Paused::SIGNATURE_HASH {
                let enabled = U256::ONE << 255usize;
                let old = next.word(75)?;
                next.words.insert(
                    75,
                    if decode::<abi::Paused>(log)?.paused {
                        old & !enabled
                    } else {
                        old | enabled
                    },
                );
            }
        } else if let Some(index) = next.tokens.iter().position(|t| t.token == log.address) {
            if *topic == abi::Transfer::SIGNATURE_HASH {
                let e = decode::<abi::Transfer>(log)?;
                // Self-transfers are a no-op, even at a uint256 balance boundary.
                if e.from == e.to {
                    continue;
                }
                if e.from == next.proxy {
                    next.balances[index] = sub(next.balances[index], e.value)?;
                }
                if e.to == next.proxy {
                    next.balances[index] = add(next.balances[index], e.value)?;
                }
            } else if index == 3 && *topic == abi::Deposit::SIGNATURE_HASH {
                let e = decode::<abi::Deposit>(log)?;
                if e.dst == next.proxy {
                    next.balances[index] = add(next.balances[index], e.wad)?;
                }
            } else if index == 3 && *topic == abi::Withdrawal::SIGNATURE_HASH {
                let e = decode::<abi::Withdrawal>(log)?;
                if e.src == next.proxy {
                    next.balances[index] = sub(next.balances[index], e.wad)?;
                }
            }
        }
    }
    next.block = block.into();
    next.validate()?;
    Ok(next)
}
/// Filters are also used for subscriptions. Transfers cover the proxy in either
/// indexed position; no hard-coded token list can miss a later listing.
pub fn log_filters(proxy: Address, wmon: Address) -> Vec<serde_json::Value> {
    use serde_json::json;
    let owner = alloy_primitives::B256::from(proxy.into_word());
    vec![
        json!({"address":proxy}),
        json!({"topics":[abi::Transfer::SIGNATURE_HASH,owner]}),
        json!({"topics":[abi::Transfer::SIGNATURE_HASH,null,owner]}),
        json!({"address":wmon,"topics":[[abi::Deposit::SIGNATURE_HASH,abi::Withdrawal::SIGNATURE_HASH],owner]}),
    ]
}
