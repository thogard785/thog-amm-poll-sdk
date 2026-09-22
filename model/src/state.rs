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

/// Complete raw image. Construct through `from_pages`; pages must all describe one block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolState {
    pub proxy: Address,
    pub block: BlockContext,
    pub tokens: Vec<TokenMetadata>,
    pub balances: Vec<U256>,
    pub categories: Vec<CategoryMetadata>,
    pub words: BTreeMap<u16, U256>,
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
        let count = first.tokenCount as usize;
        let categories = first.categories.clone();
        if !(8..=64).contains(&count)
            || !(5..=64).contains(&categories.len())
            || categories.len() > count
        {
            return Err(Error::InvalidData("invalid token/category count".into()));
        }
        let mut tokens = vec![None; count];
        let mut balances = vec![U256::ZERO; count];
        let mut words = BTreeMap::new();
        for page in pages {
            if page.schemaVersion != 6 {
                return Err(Error::UnsupportedSchema(page.schemaVersion));
            }
            if page.blockNumber != U256::from(block.number)
                || page.baseFeePerGas != block.base_fee_per_gas
                || page.parentBlockHash != block.parent_hash
                || page.tokenCount as usize != count
                || page.categoryCount as usize != categories.len()
                || page.categories != categories
                || page.tokens.len() != page.balances.len()
                || page.startTokenIndex as usize + page.tokens.len() > count
            {
                return Err(Error::InvalidData("inconsistent pages".into()));
            }
            for (offset, (token, balance)) in page.tokens.into_iter().zip(page.balances).enumerate()
            {
                let index = page.startTokenIndex as usize + offset;
                if tokens[index].replace(token).is_some() {
                    return Err(Error::InvalidData("overlapping token pages".into()));
                }
                balances[index] = balance;
            }
            let mut previous = None;
            for word in page.words {
                if previous.is_some_and(|slot| slot >= word.slot) {
                    return Err(Error::InvalidData("unsorted or duplicate words".into()));
                }
                previous = Some(word.slot);
                if let Some(old) = words.insert(word.slot, word.value) {
                    if old != word.value {
                        return Err(Error::InvalidData(
                            "shared word differs between pages".into(),
                        ));
                    }
                }
            }
        }
        let tokens = tokens
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| Error::InvalidData("missing token page".into()))?;
        let state = Self {
            proxy,
            block,
            tokens,
            balances,
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
    pub fn field(&self, reference: u32) -> Result<U256> {
        if reference == 0 {
            return Ok(U256::ZERO);
        }
        let offset = ((reference >> 8usize) & 255) as usize;
        let width = (reference & 255) as usize;
        if width == 0 || offset + width > 256 {
            return Err(Error::InvalidData("invalid field reference".into()));
        }
        Ok((self.word((reference >> 16usize) as u16)? >> offset) & mask(width))
    }
    pub fn packed_price(&self, price: u32, tail: u32) -> Result<U256> {
        Ok(self.field(price)? | (self.field(tail)? << ((price & 255) as usize)))
    }
    pub fn validate(&self) -> Result<()> {
        if self.tokens.len() != self.balances.len()
            || !(8..=64).contains(&self.tokens.len())
            || !(5..=64).contains(&self.categories.len())
        {
            return Err(Error::InvalidData("invalid image dimensions".into()));
        }
        for slot in [75, 76, 77, 79, 80, 81, 82, 83] {
            self.word(slot)?;
        }
        let counts = self.word(83)?;
        let (token_count, category_count) = if counts.is_zero() {
            (8, 5)
        } else {
            (
                (counts & U256::from(255)).to::<usize>(),
                ((counts >> 8usize) & U256::from(255)).to::<usize>(),
            )
        };
        if token_count != self.tokens.len() || category_count != self.categories.len() {
            return Err(Error::InvalidData(
                "counts word disagrees with metadata".into(),
            ));
        }
        let mut addresses = BTreeSet::new();
        for (index, t) in self.tokens.iter().enumerate() {
            if t.token.is_zero()
                || !addresses.insert(t.token)
                || t.decimals > 18
                || t.category as usize >= self.categories.len()
            {
                return Err(Error::InvalidData("invalid token metadata".into()));
            }
            for (reference, width) in [
                (t.price, if index == 7 { 32 } else { 48 }),
                (t.tokenSpread, 10),
                (t.categorySpread, 10),
                (t.inventory, 56),
                (t.sides, 2),
                (t.postedBlock, 32),
                (t.sequence, 16),
            ] {
                if reference == 0 || reference & 255 != width {
                    return Err(Error::InvalidData("invalid metadata field width".into()));
                }
            }
            if (index == 7 && t.priceTail & 255 != 13) || (index != 7 && t.priceTail != 0) {
                return Err(Error::InvalidData("invalid split price reference".into()));
            }
            for reference in [
                t.price,
                t.priceTail,
                t.tokenSpread,
                t.categorySpread,
                t.inventory,
                t.sides,
                t.postedBlock,
                t.sequence,
                t.pairStart,
            ] {
                self.field(reference)?;
            }
            let c = &self.categories[t.category as usize];
            if t.inventory != c.inventory || t.categorySpread != c.spread {
                return Err(Error::InvalidData("category references disagree".into()));
            }
        }
        for (index, c) in self.categories.iter().enumerate() {
            let anchor = self
                .tokens
                .get(c.anchorTokenIndex as usize)
                .ok_or_else(|| Error::InvalidData("unknown category anchor".into()))?;
            if anchor.category as usize != index
                || c.price != anchor.price
                || c.priceTail != anchor.priceTail
                || c.postedBlock != anchor.postedBlock
            {
                return Err(Error::InvalidData(
                    "category anchor disagrees with registry".into(),
                ));
            }
            for reference in [c.price, c.priceTail, c.inventory, c.spread, c.postedBlock] {
                self.field(reference)?;
            }
        }
        // Legacy pair/covariance words were checked above. Extension fields are
        // contiguous: 25 pair spreads or 16 covariance entries per packed word.
        // Validate each word once instead of repeating a tree lookup per field.
        let tokens = self.tokens.len();
        let pair_fields = tokens * (tokens - 1) / 2 - 28;
        for word in 0..pair_fields.div_ceil(25) {
            self.word(512 + word as u16)?;
        }
        let categories = self.categories.len();
        if categories > 5 {
            let covariance_fields = categories * (categories - 1) / 2;
            for word in 0..covariance_fields.div_ceil(16) {
                self.word(384 + word as u16)?;
            }
        }
        Ok(())
    }
}

pub fn pair_location(mut left: usize, mut right: usize) -> (u16, usize) {
    if left > right {
        std::mem::swap(&mut left, &mut right);
    }
    let field = if right < 8 {
        left * (15 - left) / 2 + right - left - 1
    } else {
        right * (right - 1) / 2 + left - 28
    };
    (
        (if right < 8 { 81 } else { 512 }) + (field / 25) as u16,
        field % 25 * 10,
    )
}
pub fn covariance_location(mut left: usize, mut right: usize) -> (u16, usize) {
    if left > right {
        std::mem::swap(&mut left, &mut right);
    }
    if left == 0 {
        return (0, 0);
    }
    if right < 4 {
        let field = if left == 1 {
            right - 1
        } else if left == 2 {
            right + 1
        } else {
            5
        };
        if field < 2 {
            (80, 64 + field * 16)
        } else {
            (76, 176 + (field - 2) * 16)
        }
    } else if right == 4 {
        (80, (left - 1) * 16)
    } else {
        let field = (right - 1) * right / 2 + left - 1;
        (384 + (field / 16) as u16, field % 16 * 16)
    }
}
