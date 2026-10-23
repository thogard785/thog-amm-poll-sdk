//! Absolute maker checkpoints plus ERC-20 balance deltas, applied atomically by block.
use crate::{
    abi,
    math::{add, mask, sub},
    Address, BlockHeader, Error, PoolState, Result, RpcLog, U256,
};
use alloy_sol_types::{sol_data, SolEvent, SolType};

fn decode<E: SolEvent>(log: &RpcLog) -> Result<E> {
    E::decode_raw_log_validate(log.topics.iter().copied(), &log.data)
        .map_err(|e| Error::Abi(e.to_string()))
}
fn invalid() -> Error {
    Error::InvalidData("invalid schema-7 publication".into())
}
fn page(state: &mut PoolState, e: abi::MakerPageUpdated, block: u64) -> Result<()> {
    let tokens = state.tokens.len();
    let categories = state.categories.len();
    let generation = state.bits(8, 184, 32)?;
    if e.lane == 8 {
        let (addresses, limits, cats, cat_limits) =
            <(
                sol_data::Array<sol_data::Address>,
                sol_data::Array<sol_data::Uint<256>>,
                sol_data::Array<sol_data::Uint<8>>,
                sol_data::Array<sol_data::Uint<256>>,
            )>::abi_decode_params_validate(&e.payload)
            .map_err(|e| Error::Abi(e.to_string()))?;
        if addresses.len() != limits.len() || cats.len() != cat_limits.len() {
            return Err(invalid());
        }
        for (address, limit) in addresses.into_iter().zip(limits) {
            let i = state
                .tokens
                .iter()
                .position(|t| t.token == address)
                .ok_or_else(invalid)?;
            state.set_bits(
                120 + (i / 10) as u16,
                i % 10 * 24,
                24,
                U256::from(crate::model::encode_cap(limit)),
            )?;
        }
        for (c, limit) in cats.into_iter().zip(cat_limits) {
            let c = usize::from(c);
            if c >= categories {
                return Err(invalid());
            }
            state.set_bits(
                124 + (c / 10) as u16,
                c % 10 * 24,
                24,
                U256::from(crate::model::encode_cap(limit)),
            )?;
        }
    } else {
        if e.payload.len() % 32 != 0 || e.payload.is_empty() {
            return Err(invalid());
        }
        let words: Vec<U256> = e
            .payload
            .chunks_exact(32)
            .map(U256::from_be_slice)
            .collect();
        let h = words[0];
        let f = |offset: usize, width: usize| (h >> offset) & mask(width);
        match e.lane {
            1 => {
                if words.len() != 1 + tokens.div_ceil(4)
                    || f(81, 8) != U256::from(tokens)
                    || f(89, 8) != U256::from(7)
                    || f(97, 32) != generation
                    || !(h >> 129usize).is_zero()
                    || block > u32::MAX as u64
                {
                    return Err(invalid());
                }
                for (i, word) in words[1..].iter().enumerate() {
                    state.words.insert(21 + i as u16, *word);
                }
                state.set_bits(2, 20, 16, f(0, 16))?;
                state.set_bits(2, 36, 32, U256::from(block))?;
                state.set_bits(2, 68, 64, f(17, 64))?;
                state.set_bits(2, 133, 1, f(16, 1))?;
            }
            2 => {
                if f(0, 8) != U256::from(7)
                    || f(8, 8) != U256::from(tokens)
                    || f(16, 8) != U256::from(categories)
                    || !f(80, 32).is_zero()
                    || f(112, 32) != state.bits(7, 160, 32)?
                    || f(144, 32) != generation
                    || !(h >> 176usize).is_zero()
                {
                    return Err(invalid());
                }
                let cat_words = categories.div_ceil(3);
                let covariance_words = (categories * (categories + 1) / 2).div_ceil(16);
                let policy_words = tokens.div_ceil(5);
                let pair_words = (tokens * (tokens - 1) / 2).div_ceil(16);
                if words.len() != 1 + cat_words + covariance_words + policy_words + pair_words {
                    return Err(invalid());
                }
                let mut cursor = 1;
                for i in 0..cat_words {
                    state.set_bits(
                        29 + i as u16,
                        0,
                        if i == 10 { 170 } else { 255 },
                        words[cursor],
                    )?;
                    cursor += 1;
                }
                for i in 0..covariance_words {
                    state.words.insert(40 + i as u16, words[cursor]);
                    cursor += 1;
                }
                for i in 0..tokens {
                    let policy = words[cursor + i / 5] >> (i % 5 * 48);
                    state.set_bits(9 + (i / 3) as u16, i % 3 * 75 + 15, 16, policy)?;
                    state.set_bits(9 + (i / 3) as u16, i % 3 * 75 + 43, 32, policy >> 16usize)?;
                }
                cursor += policy_words;
                for i in 0..pair_words {
                    state.words.insert(73 + i as u16, words[cursor]);
                    cursor += 1;
                }
                state.set_bits(2, 134, 1, U256::ONE)?;
                state.set_bits(2, 138, 8, f(56, 8))?;
                state.set_bits(2, 146, 16, f(64, 16))?;
                state.set_bits(2, 162, 32, f(24, 32))?;
                state.refresh_categories()?;
            }
            4 => {
                let wrapper_words = tokens.div_ceil(3);
                if words.len() != 1 + wrapper_words + categories.div_ceil(4)
                    || f(0, 8) != U256::from(7)
                    || f(8, 8) != U256::from(tokens)
                    || f(16, 8) != U256::from(categories)
                    || !f(24, 8).is_zero()
                    || f(32, 32) != generation
                    || !(h >> 64usize).is_zero()
                {
                    return Err(invalid());
                }
                let mut coefficients = [U256::ZERO; 8];
                for i in 0..tokens {
                    let policy = (words[1 + i / 3] >> (i % 3 * 76)) & mask(76);
                    coefficients[i / 4] |= (policy & mask(64)) << (i % 4 * 64);
                    state.set_bits(9 + (i / 3) as u16, i % 3 * 75 + 31, 12, policy >> 64usize)?;
                }
                for (i, coefficient) in coefficients.iter().enumerate().take(tokens.div_ceil(4)) {
                    state.words.insert(104 + i as u16, *coefficient);
                }
                for i in 0..categories.div_ceil(4) {
                    state
                        .words
                        .insert(112 + i as u16, words[1 + wrapper_words + i]);
                }
                state.set_bits(2, 135, 1, U256::ONE)?;
            }
            _ => return Err(invalid()),
        }
    }
    if state.word(2)? != e.control {
        return Err(Error::InvalidData(
            "publication control disagrees with replay".into(),
        ));
    }
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
            if *topic == abi::MakerTokenAdded::SIGNATURE_HASH
                || *topic == abi::MakerTokenRetired::SIGNATURE_HASH
            {
                // Registry mutations also rewrite policy/matrix rows not included
                // in these events. A complete new snapshot is required.
                return Err(Error::RegistryChanged);
            } else if *topic == abi::MakerPageUpdated::SIGNATURE_HASH {
                page(
                    &mut next,
                    decode::<abi::MakerPageUpdated>(log)?,
                    block.number,
                )?;
            } else if *topic == abi::MakerLendingExposureUpdated::SIGNATURE_HASH {
                let e = decode::<abi::MakerLendingExposureUpdated>(log)?;
                let c = usize::from(e.category);
                if c >= next.categories.len()
                    || U256::from(e.sequence) != next.bits(7, 160, 32)? + U256::ONE
                {
                    return Err(invalid());
                }
                next.set_bits(
                    29 + (c / 3) as u16,
                    c % 3 * 85,
                    64,
                    U256::from(e.offset as u64),
                )?;
                next.set_bits(7, 160, 32, U256::from(e.sequence))?;
            } else if *topic == abi::MakerWmonBalanceCheckpoint::SIGNATURE_HASH {
                let i = next
                    .tokens
                    .iter()
                    .position(|t| t.token == crate::state::WMON)
                    .ok_or_else(invalid)?;
                let observed = decode::<abi::MakerWmonBalanceCheckpoint>(log)?.balance;
                if next.balances[i] != observed {
                    return Err(Error::BalanceCheckpointMismatch {
                        token: crate::state::WMON,
                        expected: next.balances[i],
                        observed,
                    });
                }
            } else if *topic == abi::NativeBalanceCheckpoint::SIGNATURE_HASH {
                next.native_balance = decode::<abi::NativeBalanceCheckpoint>(log)?.balance;
            } else if *topic == abi::Paused::SIGNATURE_HASH {
                next.set_bits(
                    2,
                    132,
                    1,
                    U256::from(decode::<abi::Paused>(log)?.paused as u8),
                )?;
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
            } else if log.address == crate::state::WMON && *topic == abi::Deposit::SIGNATURE_HASH {
                let e = decode::<abi::Deposit>(log)?;
                if e.dst == next.proxy {
                    next.balances[index] = add(next.balances[index], e.wad)?;
                }
            } else if log.address == crate::state::WMON && *topic == abi::Withdrawal::SIGNATURE_HASH
            {
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
