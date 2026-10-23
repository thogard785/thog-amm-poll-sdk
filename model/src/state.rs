use crate::{
    abi::{CategoryMetadata, PoolData, TokenMetadata},
    math::mask,
    Address, Bytes, Error, Result, B256, U256,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub mod quantity {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let text = String::deserialize(d)?;
        u64::from_str_radix(
            text.strip_prefix("0x")
                .ok_or_else(|| serde::de::Error::custom("expected hex quantity"))?,
            16,
        )
        .map_err(serde::de::Error::custom)
    }
    pub fn serialize<S: Serializer>(value: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("0x{value:x}"))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockHeader {
    #[serde(with = "quantity")]
    pub number: u64,
    pub hash: B256,
    pub parent_hash: B256,
    pub base_fee_per_gas: U256,
}
/// Context returned by the contract. A plain eth_call cannot discover its own
/// block hash; only a hash-pinned call or a subscription supplies `hash`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockContext {
    pub number: u64,
    pub hash: Option<B256>,
    pub parent_hash: B256,
    pub base_fee_per_gas: U256,
}
impl From<BlockHeader> for BlockContext {
    fn from(header: BlockHeader) -> Self {
        Self {
            number: header.number,
            hash: Some(header.hash),
            parent_hash: header.parent_hash,
            base_fee_per_gas: header.base_fee_per_gas,
        }
    }
}
impl TryFrom<BlockContext> for BlockHeader {
    type Error = Error;
    fn try_from(context: BlockContext) -> Result<Self> {
        Ok(Self {
            number: context.number,
            hash: context
                .hash
                .ok_or_else(|| Error::InvalidData("block hash is unknown".into()))?,
            parent_hash: context.parent_hash,
            base_fee_per_gas: context.base_fee_per_gas,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcLog {
    pub address: Address,
    pub topics: Vec<B256>,
    pub data: Bytes,
    pub block_hash: B256,
    #[serde(with = "quantity")]
    pub block_number: u64,
    #[serde(with = "quantity")]
    pub transaction_index: u64,
    #[serde(with = "quantity")]
    pub log_index: u64,
    #[serde(default)]
    pub removed: bool,
}

/// Atomic schema-7 page plus all registered ERC-20 balances and native MON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolState {
    pub proxy: Address,
    pub block: BlockContext,
    pub tokens: Vec<TokenMetadata>,
    pub balances: Vec<U256>,
    pub native_balance: U256,
    pub categories: Vec<CategoryMetadata>,
    pub words: BTreeMap<u16, U256>,
}

pub const WMON: Address = alloy_primitives::address!("3bd359c1119da7da1d913d1c4d2b7c461115433a");
pub fn reference(slot: u16, offset: usize, width: usize) -> u32 {
    (u32::from(slot) << 16) | ((offset as u32) << 8) | width as u32
}

impl PoolState {
    pub fn from_snapshot(proxy: Address, page: PoolData, hash: Option<B256>) -> Result<Self> {
        let block = BlockContext {
            number: page.blockNumber.try_into().map_err(|_| Error::Arithmetic)?,
            hash,
            parent_hash: page.parentBlockHash,
            base_fee_per_gas: page.baseFeePerGas,
        };
        Self::from_pages(proxy, block, vec![page])
    }

    pub fn from_pages(
        proxy: Address,
        block: impl Into<BlockContext>,
        pages: Vec<PoolData>,
    ) -> Result<Self> {
        let block = block.into();
        let first = pages
            .first()
            .ok_or_else(|| Error::InvalidData("no pages".into()))?;
        let count = usize::from(first.tokenCount);
        if !(1..=32).contains(&count) || !(1..=32).contains(&first.categories.len()) {
            return Err(Error::InvalidData("invalid registry dimensions".into()));
        }
        let categories = first.categories.clone();
        let balances = first.portfolioBalances.clone();
        let native_balance = first.nativeBalance;
        let mut tokens = vec![None; count];
        let mut words = BTreeMap::new();
        for page in pages {
            if page.schemaVersion != 7 {
                return Err(Error::UnsupportedSchema(page.schemaVersion));
            }
            if page.blockNumber != U256::from(block.number)
                || page.baseFeePerGas != block.base_fee_per_gas
                || page.parentBlockHash != block.parent_hash
                || usize::from(page.tokenCount) != count
                || usize::from(page.categoryCount) != categories.len()
                || page.categories != categories
                || page.portfolioBalances != balances
                || page.nativeBalance != native_balance
                || page.tokens.len() != page.balances.len()
                || balances.len() != count
                || usize::from(page.startTokenIndex) + page.tokens.len() > count
                || page.words.len() != 126
            {
                return Err(Error::InvalidData(
                    "inconsistent or incomplete snapshot pages".into(),
                ));
            }
            for (offset, (token, balance)) in page.tokens.into_iter().zip(page.balances).enumerate()
            {
                let index = usize::from(page.startTokenIndex) + offset;
                if balance != balances[index] || tokens[index].replace(token).is_some() {
                    return Err(Error::InvalidData(
                        "overlapping page or inconsistent balance".into(),
                    ));
                }
            }
            for (offset, word) in page.words.into_iter().enumerate() {
                if usize::from(word.slot) != offset + 2 {
                    return Err(Error::InvalidData(
                        "expected unique sorted slots 2..127".into(),
                    ));
                }
                if words
                    .insert(word.slot, word.value)
                    .is_some_and(|old| old != word.value)
                {
                    return Err(Error::InvalidData("pages contain different words".into()));
                }
            }
        }
        let tokens = tokens
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| Error::InvalidData("missing token metadata".into()))?;
        let state = Self {
            proxy,
            block,
            tokens,
            balances,
            native_balance,
            categories,
            words,
        };
        state.validate()?;
        Ok(state)
    }

    pub fn word(&self, slot: u16) -> Result<U256> {
        self.words
            .get(&slot)
            .copied()
            .ok_or(Error::MissingWord(slot))
    }
    pub fn field(&self, r: u32) -> Result<U256> {
        if r == 0 {
            return Ok(U256::ZERO);
        }
        let (offset, width) = (((r >> 8) & 255) as usize, (r & 255) as usize);
        if width == 0 || offset + width > 256 {
            return Err(Error::InvalidData("invalid field reference".into()));
        }
        Ok((self.word((r >> 16) as u16)? >> offset) & mask(width))
    }
    pub fn bits(&self, slot: u16, offset: usize, width: usize) -> Result<U256> {
        self.field(reference(slot, offset, width))
    }
    pub fn packed(&self, slot: u16, index: usize, width: usize, per_word: usize) -> Result<U256> {
        self.bits(
            slot + (index / per_word) as u16,
            index % per_word * width,
            width,
        )
    }
    pub fn token_bits(&self, index: usize, offset: usize, width: usize) -> Result<U256> {
        self.bits(9 + (index / 3) as u16, index % 3 * 75 + offset, width)
    }
    pub fn set_bits(&mut self, slot: u16, offset: usize, width: usize, value: U256) -> Result<()> {
        let old = self.word(slot)?;
        self.words.insert(
            slot,
            (old & !(mask(width) << offset)) | ((value & mask(width)) << offset),
        );
        Ok(())
    }
    pub fn price(&self, index: usize, upper: bool) -> Result<U256> {
        let packed = self.packed(21, index, 64, 4)?;
        let mantissa = packed & mask(56);
        let exponent = (packed >> 56usize).to::<u8>();
        // The deployed decoder returns zero for invalid fields. Context rejects
        // endpoint zero prices; inactive category prices can still value to zero.
        if mantissa < U256::from(2_000_000) || exponent > 18 {
            return Ok(U256::ZERO);
        }
        crate::math::mul(
            mantissa + U256::from(upper as u8),
            crate::math::pow10(18 - exponent),
        )
    }
    pub fn covariance(&self, a: usize, b: usize) -> Result<i64> {
        let (slot, offset) = covariance_location(a, b);
        Ok(self.bits(slot, offset, 16)?.to::<u16>() as i16 as i64)
    }
    pub fn refresh_categories(&mut self) -> Result<()> {
        for c in 0..self.categories.len() {
            let anchor = self
                .bits(29 + (c / 3) as u16, c % 3 * 85 + 80, 5)?
                .to::<u8>();
            let token = self
                .tokens
                .get(usize::from(anchor))
                .ok_or_else(|| Error::InvalidData("unknown anchor".into()))?;
            self.categories[c].anchorTokenIndex = anchor;
            self.categories[c].price = token.price;
            self.categories[c].priceTail = 0;
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        let count = self.tokens.len();
        let categories = self.categories.len();
        if !(1..=32).contains(&count)
            || !(1..=32).contains(&categories)
            || categories > count
            || self.balances.len() != count
            || self.words.len() != 126
        {
            return Err(Error::InvalidData("invalid image dimensions".into()));
        }
        for slot in 2..128 {
            self.word(slot)?;
        }
        if self.bits(2, 0, 8)? != U256::from(7)
            || self.bits(2, 8, 6)? != U256::from(count)
            || self.bits(2, 14, 6)? != U256::from(categories)
            || self.bits(8, 160, 24)? != U256::from(7 | 7 << 8 | 1 << 16)
            || self.bits(8, 184, 32)?.is_zero()
        {
            return Err(Error::InvalidData(
                "unsupported layout, wire, model or registry generation".into(),
            ));
        }
        let mut addresses = BTreeSet::new();
        for (i, t) in self.tokens.iter().enumerate() {
            let c = usize::from(t.category);
            let token_slot = 9 + (i / 3) as u16;
            let token_offset = i % 3 * 75;
            let catalog_id = self.token_bits(i, 0, 5)?.to::<usize>();
            if self.packed(20, catalog_id, 6, 32)? != U256::from(i + 1) {
                return Err(Error::InvalidData(
                    "registry index disagrees with token metadata".into(),
                ));
            }
            let category_slot = 29 + (c / 3) as u16;
            let category_offset = c % 3 * 85;
            if t.token.is_zero()
                || !addresses.insert(t.token)
                || t.decimals > 18
                || c >= categories
                || self.token_bits(i, 5, 5)? != U256::from(t.decimals)
                || self.token_bits(i, 10, 5)? != U256::from(t.category)
                || t.priceTail != 0
            {
                return Err(Error::InvalidData("invalid token metadata".into()));
            }
            for (actual, expected) in [
                (t.price, reference(21 + (i / 4) as u16, i % 4 * 64, 64)),
                (
                    t.tokenBidSpread,
                    reference(token_slot, token_offset + 15, 16),
                ),
                (
                    t.tokenAskSpread,
                    reference(token_slot, token_offset + 43, 16),
                ),
                (t.volumeRate, reference(token_slot, token_offset + 59, 16)),
                (
                    t.categorySpread,
                    reference(category_slot, category_offset + 64, 16),
                ),
                (t.inventory, reference(category_slot, category_offset, 64)),
                (t.sides, reference(2, 68 + i * 2, 2)),
                (t.postedBlock, reference(2, 36, 32)),
                (t.sequence, reference(2, 20, 16)),
                (
                    t.inventoryCap,
                    reference(120 + (i / 10) as u16, i % 10 * 24, 24),
                ),
                (
                    t.pairStart,
                    if i == 0 {
                        0
                    } else {
                        let (s, o) = pair_location(0, i);
                        reference(s, o, 16)
                    },
                ),
            ] {
                if actual != expected {
                    return Err(Error::InvalidData(
                        "metadata reference disagrees with schema 7".into(),
                    ));
                }
            }
        }
        for (c, m) in self.categories.iter().enumerate() {
            let anchor = self
                .bits(29 + (c / 3) as u16, c % 3 * 85 + 80, 5)?
                .to::<usize>();
            let t = self
                .tokens
                .get(anchor)
                .ok_or_else(|| Error::InvalidData("unknown category anchor".into()))?;
            if usize::from(m.anchorTokenIndex) != anchor
                || usize::from(t.category) != c
                || m.price != t.price
                || m.priceTail != 0
                || m.inventory != t.inventory
                || m.spread != t.categorySpread
                || m.postedBlock != t.postedBlock
                || m.inventoryCap != reference(124 + (c / 10) as u16, c % 10 * 24, 24)
            {
                return Err(Error::InvalidData(
                    "category metadata disagrees with words".into(),
                ));
            }
        }
        Ok(())
    }
}

pub fn pair_location(mut a: usize, mut b: usize) -> (u16, usize) {
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    let field = b * (b - 1) / 2 + a;
    (73 + (field / 16) as u16, field % 16 * 16)
}
pub fn covariance_location(mut a: usize, mut b: usize) -> (u16, usize) {
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    let field = b * (b + 1) / 2 + a;
    (40 + (field / 16) as u16, field % 16 * 16)
}
