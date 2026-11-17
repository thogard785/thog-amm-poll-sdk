//! Advance a snapshot by one block of logs, without reading the chain.
//!
//! The rules, event by event, are in
//! <https://github.com/thogard785/thog-amm-integration-guide/blob/main/REFERENCE.md#keeping-state-current-from-events>
use crate::{
    abi,
    layout::{self, control, Field, Packed},
    math::{add, sub, U128_MAX},
    pool::encode_cap,
    snapshot::{mask, Block, Snapshot, WMON},
    Address, Bytes, Error, Result, B256, U256,
};
use alloy_sol_types::{sol_data, SolEvent, SolType};
use serde::{Deserialize, Serialize};

/// A block header as `eth_getBlockByNumber` and `monadNewHeads` return it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockHeader {
    #[serde(with = "hex_u64")]
    pub number: u64,
    pub hash: B256,
    pub parent_hash: B256,
    pub base_fee_per_gas: U256,
}

/// A log as `eth_getLogs` and the `logs` subscription return it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Log {
    pub address: Address,
    pub topics: Vec<B256>,
    pub data: Bytes,
    pub block_hash: B256,
    #[serde(with = "hex_u64")]
    pub block_number: u64,
    #[serde(with = "hex_u64")]
    pub transaction_index: u64,
    #[serde(with = "hex_u64")]
    pub log_index: u64,
    #[serde(default)]
    pub removed: bool,
}

mod hex_u64 {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let text = String::deserialize(d)?;
        let digits = text
            .strip_prefix("0x")
            .ok_or_else(|| serde::de::Error::custom("expected a 0x-prefixed quantity"))?;
        u64::from_str_radix(digits, 16).map_err(serde::de::Error::custom)
    }
    pub fn serialize<S: Serializer>(value: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("0x{value:x}"))
    }
}

/// The log filters that together carry every change to the pool's state:
/// everything the pool emits, token transfers to and from it, and WMON
/// wrapping by it. Use them for `eth_getLogs` or `eth_subscribe("logs")`.
pub fn log_filters(pool: Address) -> Vec<serde_json::Value> {
    use serde_json::json;
    let holder = B256::from(pool.into_word());
    let transfers = [
        abi::Transfer::SIGNATURE_HASH,
        abi::TransferShares::SIGNATURE_HASH,
    ];
    let wrapping = [
        abi::Deposit::SIGNATURE_HASH,
        abi::Withdrawal::SIGNATURE_HASH,
    ];
    vec![
        json!({ "address": pool }),
        json!({ "topics": [transfers, holder] }),
        json!({ "topics": [transfers, null, holder] }),
        json!({ "address": WMON, "topics": [wrapping, holder] }),
    ]
}

/// Apply every log of the block after `snapshot`'s, in order.
///
/// `logs` must be all logs of that block matching [`log_filters`], sorted by
/// position. On any error the block could not be followed exactly: read a
/// fresh snapshot instead of continuing.
pub fn apply_block<'a>(
    mut snapshot: Snapshot,
    header: &BlockHeader,
    logs: impl IntoIterator<Item = &'a Log>,
) -> Result<Snapshot> {
    if header.number != snapshot.block.number + 1 || Some(header.parent_hash) != snapshot.block.hash
    {
        return Err(Error::Discontinuous);
    }
    let mut last_position = None;
    for log in logs {
        if log.removed || log.block_hash != header.hash || log.block_number != header.number {
            return Err(Error::Discontinuous);
        }
        let position = (log.transaction_index, log.log_index);
        if last_position.is_some_and(|last| last >= position) {
            return Err(invalid("logs are not unique and in order"));
        }
        last_position = Some(position);
        if log.address == snapshot.pool {
            apply_pool_log(&mut snapshot, log, header.number)?;
        } else if let Some(index) = snapshot.index_of(log.address) {
            apply_token_log(&mut snapshot, index, log)?;
        }
    }
    // An xStock's balance follows from its shares and the stored multiplier.
    for index in 0..snapshot.tokens.len() {
        if let Some(xstock) = &snapshot.xstocks[index] {
            snapshot.balances[index] = xstock.amount(xstock.shares)?;
        }
    }
    snapshot.block = Block {
        number: header.number,
        hash: Some(header.hash),
        parent_hash: header.parent_hash,
        base_fee: header.base_fee_per_gas,
    };
    snapshot.validate()?;
    Ok(snapshot)
}

fn apply_pool_log(snapshot: &mut Snapshot, log: &Log, block: u64) -> Result<()> {
    let Some(topic) = log.topics.first() else {
        return Ok(());
    };
    if *topic == abi::Upgraded::SIGNATURE_HASH {
        return Err(Error::ContractUpgraded);
    }
    if *topic == abi::MakerTokenAdded::SIGNATURE_HASH
        || *topic == abi::MakerTokenRetired::SIGNATURE_HASH
    {
        // A listing change also rewrites spreads and covariance that its
        // event does not carry.
        return Err(Error::RegistryChanged);
    }
    if *topic == abi::MakerPageUpdated::SIGNATURE_HASH {
        let event: abi::MakerPageUpdated = decode(log)?;
        match event.lane {
            1 => apply_price_update(snapshot, &event.payload, block)?,
            2 => apply_risk_update(snapshot, &event.payload)?,
            4 => apply_token_cost_update(snapshot, &event.payload)?,
            8 => apply_cap_update(snapshot, &event.payload)?,
            _ => return Err(invalid("unknown update kind")),
        }
        // The event carries the control word the update produced.
        if snapshot.word(control::SLOT) != event.control {
            return Err(invalid("update does not reproduce the pool's control word"));
        }
    } else if *topic == abi::MakerLendingExposureUpdated::SIGNATURE_HASH {
        let event: abi::MakerLendingExposureUpdated = decode(log)?;
        let category = usize::from(event.category);
        if category >= snapshot.category_count()
            || u64::from(event.sequence) != snapshot.small(layout::OFFSET_SEQUENCE) + 1
        {
            return Err(invalid("category offset update out of sequence"));
        }
        let (offset, width) = layout::category::OFFSET;
        snapshot.set_field(
            layout::CATEGORY.at(category).part(offset, width),
            U256::from(event.offset as u64),
        );
        snapshot.set_field(layout::OFFSET_SEQUENCE, U256::from(event.sequence));
    } else if *topic == abi::MakerMultiplierUpdated::SIGNATURE_HASH {
        let event: abi::MakerMultiplierUpdated = decode(log)?;
        if event.multiplier.is_zero() || event.multiplier > U128_MAX {
            return Err(invalid("multiplier out of range"));
        }
        // Anyone may sync a stock the pool does not list; that changes nothing here.
        if let Some(index) = snapshot.index_of(event.token) {
            snapshot.xstocks[index]
                .as_mut()
                .ok_or_else(|| invalid("multiplier update for an ordinary token"))?
                .multiplier = event.multiplier;
        }
    } else if *topic == abi::MakerInventoryUpdated::SIGNATURE_HASH {
        let event: abi::MakerInventoryUpdated = decode(log)?;
        let index = usize::from(event.index);
        if snapshot.tokens.get(index) != Some(&event.token) {
            return Err(invalid("recorded balance update for another token"));
        }
        set_recorded_units(snapshot, index, U256::from(event.balance));
    } else if *topic == abi::MakerWmonBalanceCheckpoint::SIGNATURE_HASH {
        let event: abi::MakerWmonBalanceCheckpoint = decode(log)?;
        let index = snapshot
            .index_of(WMON)
            .ok_or_else(|| invalid("WMON is not listed"))?;
        snapshot.balances[index] = event.balance;
    } else if *topic == abi::NativeBalanceCheckpoint::SIGNATURE_HASH {
        let event: abi::NativeBalanceCheckpoint = decode(log)?;
        snapshot.native_balance = event.balance;
    } else if *topic == abi::Paused::SIGNATURE_HASH {
        let event: abi::Paused = decode(log)?;
        snapshot.set_field(control::PAUSED, U256::from(event.paused as u8));
    }
    Ok(())
}

fn apply_token_log(snapshot: &mut Snapshot, index: usize, log: &Log) -> Result<()> {
    let Some(topic) = log.topics.first() else {
        return Ok(());
    };
    let pool = snapshot.pool;
    // Amount entering and leaving the pool; a transfer to itself moves nothing.
    let moved = |from: Address, to: Address, value: U256| {
        if from == to {
            (U256::ZERO, U256::ZERO)
        } else {
            (
                if to == pool { value } else { U256::ZERO },
                if from == pool { value } else { U256::ZERO },
            )
        }
    };
    if let Some(xstock) = &mut snapshot.xstocks[index] {
        // An xStock emits both events for one movement. Only the share event
        // is exact, so the ordinary Transfer is skipped.
        if *topic == abi::TransferShares::SIGNATURE_HASH {
            let event: abi::TransferShares = decode(log)?;
            let (received, sent) = moved(event.from, event.to, event.value);
            xstock.shares = sub(add(xstock.shares, received)?, sent)?;
        }
        return Ok(());
    }
    let (received, sent) = if *topic == abi::Transfer::SIGNATURE_HASH {
        let event: abi::Transfer = decode(log)?;
        moved(event.from, event.to, event.value)
    } else if log.address == WMON && *topic == abi::Deposit::SIGNATURE_HASH {
        let event: abi::Deposit = decode(log)?;
        moved(Address::ZERO, event.dst, event.wad)
    } else if log.address == WMON && *topic == abi::Withdrawal::SIGNATURE_HASH {
        let event: abi::Withdrawal = decode(log)?;
        moved(event.src, Address::ZERO, event.wad)
    } else {
        return Ok(());
    };
    snapshot.balances[index] = sub(add(snapshot.balances[index], received)?, sent)?;
    Ok(())
}

/// Store a recorded balance. Bit `i` of slot 121 says that word `i` of the
/// recorded balances is zero.
pub(crate) fn set_recorded_units(snapshot: &mut Snapshot, index: usize, units: U256) {
    let field = layout::RECORDED_UNITS.at(index);
    snapshot.set_field(field, units);
    let word_is_zero = snapshot.word(field.slot).is_zero();
    snapshot.set_field(
        Field::new(
            layout::RECORDED_ZERO_SLOT,
            usize::from(field.slot - layout::RECORDED_UNITS.first_slot),
            1,
        ),
        U256::from(word_is_zero as u8),
    );
}

// The payload of a MakerPageUpdated event is a header word followed by the
// new values, packed like the storage they replace. The header repeats the
// token and category counts and the registry generation it was built for.

/// The 32-byte words of an update payload.
fn payload_words(payload: &[u8]) -> Result<Vec<U256>> {
    if payload.is_empty() || payload.len() % 32 != 0 {
        return Err(invalid("update payload is not whole words"));
    }
    Ok(payload.chunks_exact(32).map(U256::from_be_slice).collect())
}

fn bits(word: U256, offset: usize, width: usize) -> U256 {
    (word >> offset) & mask(width)
}

/// Replace `count` fields of a packed array with whole payload words.
fn replace_words(snapshot: &mut Snapshot, array: Packed, count: usize, words: &[U256]) {
    for (index, word) in words.iter().take(array.words(count)).enumerate() {
        snapshot.set_word(array.first_slot + index as u16, *word);
    }
}

/// Kind 1: every token's price, the per-token switches, and the price block.
fn apply_price_update(snapshot: &mut Snapshot, payload: &[u8], block: u64) -> Result<()> {
    let words = payload_words(payload)?;
    let header = words[0];
    let tokens = snapshot.token_count();
    if words.len() != 1 + layout::PRICE.words(tokens)
        || bits(header, 81, 8) != U256::from(tokens)
        || bits(header, 89, 8) != U256::from(8)
        || bits(header, 97, 32) != snapshot.field(layout::REGISTRY_GENERATION)
        || !(header >> 129usize).is_zero()
        || block > u64::from(u32::MAX)
    {
        return Err(invalid("price update"));
    }
    replace_words(snapshot, layout::PRICE, tokens, &words[1..]);
    snapshot.set_field(control::PRICE_SEQUENCE, bits(header, 0, 16));
    snapshot.set_field(control::PRICES_ENABLED, bits(header, 16, 1));
    snapshot.set_field(control::SIDES, bits(header, 17, 64));
    snapshot.set_field(control::PRICE_BLOCK, U256::from(block));
    Ok(())
}

/// Kind 2: category spreads and offsets, covariance, token spreads and size
/// rates, pair spreads, and the pool-wide age and risk parameters.
fn apply_risk_update(snapshot: &mut Snapshot, payload: &[u8]) -> Result<()> {
    let words = payload_words(payload)?;
    let header = words[0];
    let tokens = snapshot.token_count();
    let categories = snapshot.category_count();
    let category_words = layout::CATEGORY.words(categories);
    let covariance_words = layout::COVARIANCE.words(categories * (categories + 1) / 2);
    let token_words = tokens.div_ceil(5);
    let pair_words = layout::PAIR_SPREAD.words(tokens * (tokens - 1) / 2);
    if bits(header, 0, 8) != U256::from(8)
        || bits(header, 8, 8) != U256::from(tokens)
        || bits(header, 16, 8) != U256::from(categories)
        || !bits(header, 80, 32).is_zero()
        || bits(header, 112, 32) != snapshot.field(layout::OFFSET_SEQUENCE)
        || bits(header, 144, 32) != snapshot.field(layout::REGISTRY_GENERATION)
        || !(header >> 176usize).is_zero()
        || words.len() != 1 + category_words + covariance_words + token_words + pair_words
    {
        return Err(invalid("risk update"));
    }
    let (categories_at, covariance_at) = (1, 1 + category_words);
    let tokens_at = covariance_at + covariance_words;
    let pairs_at = tokens_at + token_words;
    // Category records fill the low 255 bits of a word; the last storage word
    // of the array holds a single record.
    for index in 0..category_words {
        let slot = layout::CATEGORY.first_slot + index as u16;
        let width = if slot + 1 == layout::COVARIANCE.first_slot {
            layout::CATEGORY.width
        } else {
            255
        };
        snapshot.set_field(Field::new(slot, 0, width), words[categories_at + index]);
    }
    replace_words(
        snapshot,
        layout::COVARIANCE,
        categories * (categories + 1) / 2,
        &words[covariance_at..],
    );
    // Five 48-bit entries per word: spread in, spread out, size rate.
    for index in 0..tokens {
        let entry = words[tokens_at + index / 5] >> (index % 5 * 48);
        let record = layout::TOKEN.at(index);
        let (offset, width) = layout::token::SPREAD_IN;
        snapshot.set_field(record.part(offset, width), entry);
        // Spread out and size rate are adjacent in storage.
        let (offset, _) = layout::token::SPREAD_OUT;
        snapshot.set_field(record.part(offset, 32), entry >> 16usize);
    }
    replace_words(
        snapshot,
        layout::PAIR_SPREAD,
        tokens * (tokens - 1) / 2,
        &words[pairs_at..],
    );
    snapshot.set_field(control::RISK_AVERSION, bits(header, 24, 32));
    snapshot.set_field(control::MAX_PRICE_AGE, bits(header, 56, 8));
    snapshot.set_field(control::SPREAD_PER_BLOCK, bits(header, 64, 16));
    snapshot.set_field(control::RISK_READY, U256::ONE);
    Ok(())
}

/// Kind 4: every token's cost and every category's target.
fn apply_token_cost_update(snapshot: &mut Snapshot, payload: &[u8]) -> Result<()> {
    let words = payload_words(payload)?;
    let header = words[0];
    let tokens = snapshot.token_count();
    let categories = snapshot.category_count();
    let cost_words = tokens.div_ceil(3);
    if bits(header, 0, 8) != U256::from(8)
        || bits(header, 8, 8) != U256::from(tokens)
        || bits(header, 16, 8) != U256::from(categories)
        || !bits(header, 24, 8).is_zero()
        || bits(header, 32, 32) != snapshot.field(layout::REGISTRY_GENERATION)
        || !(header >> 64usize).is_zero()
        || words.len() != 1 + cost_words + layout::CATEGORY_TARGET.words(categories)
    {
        return Err(invalid("token cost update"));
    }
    // Three 76-bit entries per word: coefficient, kind, refund. Unlisted
    // positions of the last coefficient word are cleared.
    for word in 0..layout::COST_COEFFICIENT.words(tokens) {
        snapshot.set_word(
            layout::COST_COEFFICIENT.first_slot + word as u16,
            U256::ZERO,
        );
    }
    for index in 0..tokens {
        let entry = bits(words[1 + index / 3], index % 3 * 76, 76);
        snapshot.set_field(layout::COST_COEFFICIENT.at(index), entry);
        // Kind and refund are adjacent in storage.
        let (offset, _) = layout::token::COST_KIND;
        snapshot.set_field(layout::TOKEN.at(index).part(offset, 12), entry >> 64usize);
    }
    replace_words(
        snapshot,
        layout::CATEGORY_TARGET,
        categories,
        &words[1 + cost_words..],
    );
    snapshot.set_field(control::TOKEN_COSTS_READY, U256::ONE);
    Ok(())
}

/// Kind 8: caps for the named tokens and categories; others keep theirs.
fn apply_cap_update(snapshot: &mut Snapshot, payload: &[u8]) -> Result<()> {
    type Caps = (
        sol_data::Array<sol_data::Address>,
        sol_data::Array<sol_data::Uint<256>>,
        sol_data::Array<sol_data::Uint<8>>,
        sol_data::Array<sol_data::Uint<256>>,
    );
    let (tokens, token_caps, categories, category_caps) =
        Caps::abi_decode_params_validate(payload).map_err(|e| Error::Abi(e.to_string()))?;
    if tokens.len() != token_caps.len() || categories.len() != category_caps.len() {
        return Err(invalid("cap update"));
    }
    for (token, cap) in tokens.into_iter().zip(token_caps) {
        let index = snapshot
            .index_of(token)
            .ok_or_else(|| invalid("cap for an unlisted token"))?;
        snapshot.set_field(layout::TOKEN_CAP.at(index), U256::from(encode_cap(cap)));
    }
    for (category, cap) in categories.into_iter().zip(category_caps) {
        let category = usize::from(category);
        if category >= snapshot.category_count() {
            return Err(invalid("cap for an unknown category"));
        }
        snapshot.set_field(
            layout::CATEGORY_CAP.at(category),
            U256::from(encode_cap(cap)),
        );
    }
    Ok(())
}

fn decode<E: SolEvent>(log: &Log) -> Result<E> {
    E::decode_raw_log_validate(log.topics.iter().copied(), &log.data)
        .map_err(|e| Error::Abi(e.to_string()))
}

fn invalid(what: &str) -> Error {
    Error::InvalidSnapshot(what.into())
}
