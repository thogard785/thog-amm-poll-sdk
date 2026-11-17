//! Everything read from the chain at one block.
use crate::{
    abi,
    layout::{self, Field},
    math::{mul_div, pow10, U128_MAX},
    Address, Error, Result, B256, U256,
};
use alloy_sol_types::SolCall;

/// Wrapped MON on Monad mainnet. The pool counts its own native MON balance
/// together with its WMON when it measures risk.
pub const WMON: Address = alloy_primitives::address!("3bd359c1119da7da1d913d1c4d2b7c461115433a");

/// The block a snapshot was read at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub number: u64,
    /// Unknown for a snapshot read by block number or tag: a contract cannot
    /// see the hash of the block it runs in.
    pub hash: Option<B256>,
    pub parent_hash: B256,
    pub base_fee: U256,
}

/// The pool's holding of one xStock.
///
/// An xStock contract keeps balances in shares and converts them to token
/// amounts with a multiplier scaled by 1e18. The pool prices with the
/// multiplier it has stored, which is not always the issuer's current one.
/// <https://docs.xstocks.fi/developers/multipliers>
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct XStock {
    /// `sharesOf(pool)` on the token contract.
    pub shares: U256,
    /// `storedMultiplier(token)` on the pool.
    pub multiplier: U256,
}

impl XStock {
    /// Token amount of `shares`: `floor(shares * multiplier / 1e18)`.
    pub fn amount(&self, shares: U256) -> Result<U256> {
        mul_div(shares, self.multiplier, pow10(18))
    }
    /// Shares a transfer of `amount` moves: `floor(amount * 1e18 / multiplier)`.
    pub fn shares_for(&self, amount: U256) -> Result<U256> {
        mul_div(amount, pow10(18), self.multiplier)
    }
}

/// The pool's complete state at one block, as read from the chain.
///
/// A snapshot is raw: storage words and balances. [`crate::Pool::new`] decodes
/// it into named values and quotes from it. [`crate::events::apply_block`]
/// advances it by one block of logs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// The pool contract.
    pub pool: Address,
    pub block: Block,
    /// Listed tokens, in the pool's listing order.
    pub tokens: Vec<Address>,
    /// Storage words 2..=127, indexed by `slot - 2`. Layout: [`crate::layout`].
    pub words: [U256; layout::WORD_COUNT],
    /// What each token contract reports the pool holds, in raw units.
    /// For an xStock this is `floor(shares * multiplier / 1e18)`.
    pub balances: Vec<U256>,
    /// The pool's native MON balance.
    pub native_balance: U256,
    /// Share state for each listed xStock; `None` for an ordinary token.
    pub xstocks: Vec<Option<XStock>>,
}

impl Snapshot {
    /// Build a snapshot from the return data of `getPoolData(0, 28)` and, for
    /// each listed xStock, its shares and stored multiplier at the same block.
    ///
    /// `xstock(token)` returns `None` for an ordinary token, that is, one whose
    /// `storedMultiplier` is zero.
    pub fn decode(
        pool: Address,
        pool_data: &[u8],
        xstock: impl Fn(Address) -> Option<XStock>,
    ) -> Result<Self> {
        let data = abi::getPoolDataCall::abi_decode_returns_validate(pool_data)
            .map_err(|e| Error::Abi(e.to_string()))?;
        Self::from_pool_data(pool, data, xstock)
    }

    /// As [`Snapshot::decode`], from already decoded pool data.
    pub fn from_pool_data(
        pool: Address,
        data: abi::PoolData,
        xstock: impl Fn(Address) -> Option<XStock>,
    ) -> Result<Self> {
        if data.schemaVersion != layout::SUPPORTED_SCHEMA {
            return Err(Error::UnsupportedFormat(u64::from(data.schemaVersion)));
        }
        let count = usize::from(data.tokenCount);
        if data.startTokenIndex != 0
            || data.tokens.len() != count
            || data.balances.len() != count
            || data.portfolioBalances != data.balances
            || data.categories.len() != usize::from(data.categoryCount)
            || data.words.len() != layout::WORD_COUNT
        {
            return Err(invalid("pool data does not cover the whole pool"));
        }
        let mut words = [U256::ZERO; layout::WORD_COUNT];
        for (index, word) in data.words.iter().enumerate() {
            if usize::from(word.slot) != index + usize::from(layout::FIRST_SLOT) {
                return Err(invalid("storage words are not slots 2..=127 in order"));
            }
            words[index] = word.value;
        }
        let tokens: Vec<Address> = data.tokens.iter().map(|t| t.token).collect();
        let snapshot = Self {
            pool,
            block: Block {
                number: data
                    .blockNumber
                    .try_into()
                    .map_err(|_| invalid("block number"))?,
                hash: None,
                parent_hash: data.parentBlockHash,
                base_fee: data.baseFeePerGas,
            },
            xstocks: tokens.iter().map(|token| xstock(*token)).collect(),
            tokens,
            words,
            balances: data.balances.clone(),
            native_balance: data.nativeBalance,
        };
        snapshot.validate()?;
        snapshot.check_described_layout(&data)?;
        Ok(snapshot)
    }

    pub fn word(&self, slot: u16) -> U256 {
        self.words[usize::from(slot - layout::FIRST_SLOT)]
    }
    pub fn set_word(&mut self, slot: u16, value: U256) {
        self.words[usize::from(slot - layout::FIRST_SLOT)] = value;
    }
    /// The value of one bit field.
    pub fn field(&self, field: Field) -> U256 {
        (self.word(field.slot) >> field.offset) & mask(field.width)
    }
    /// A field known to fit 64 bits.
    pub(crate) fn small(&self, field: Field) -> u64 {
        self.field(field).as_limbs()[0]
    }
    pub(crate) fn flag(&self, field: Field) -> bool {
        self.word(field.slot).bit(field.offset)
    }
    /// Replace one bit field, leaving the rest of its word unchanged.
    pub fn set_field(&mut self, field: Field, value: U256) {
        let mask = mask(field.width) << field.offset;
        let word = self.word(field.slot);
        self.set_word(
            field.slot,
            (word & !mask) | ((value << field.offset) & mask),
        );
    }
    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }
    pub fn category_count(&self) -> usize {
        self.small(layout::control::CATEGORY_COUNT) as usize
    }
    /// Listing position of `token`.
    pub fn index_of(&self, token: Address) -> Option<usize> {
        self.tokens.iter().position(|t| *t == token)
    }
    /// Block of the latest price update.
    pub fn price_block(&self) -> u64 {
        self.small(layout::control::PRICE_BLOCK)
    }
    /// Increases whenever a token is listed or retired.
    pub fn registry_generation(&self) -> u64 {
        self.small(layout::REGISTRY_GENERATION)
    }

    /// Checks that hold for any state the pool can be in. A failure means the
    /// data is incomplete, from mixed blocks, or in a format this crate does
    /// not decode.
    pub fn validate(&self) -> Result<()> {
        use layout::control;
        let format = self.small(layout::FORMAT);
        if self.small(control::LAYOUT_VERSION) != 8 || format != layout::SUPPORTED_FORMAT {
            return Err(Error::UnsupportedFormat(format));
        }
        let count = self.tokens.len();
        let categories = self.category_count();
        if !(1..=layout::MAX_TOKENS).contains(&count)
            || !(1..=count).contains(&categories)
            || self.small(control::TOKEN_COUNT) as usize != count
            || self.balances.len() != count
            || self.xstocks.len() != count
            || self.registry_generation() == 0
        {
            return Err(invalid("token and category counts disagree"));
        }
        for (index, token) in self.tokens.iter().enumerate() {
            let record = layout::TOKEN.at(index);
            let part = |(offset, width)| self.small(record.part(offset, width)) as usize;
            let catalog_id = part(layout::token::CATALOG_ID);
            if token.is_zero()
                || self.tokens[..index].contains(token)
                || part(layout::token::DECIMALS) > 18
                || part(layout::token::CATEGORY) >= categories
                || self.small(layout::LISTING_INDEX.at(catalog_id)) as usize != index + 1
            {
                return Err(invalid("token list disagrees with storage"));
            }
            if let Some(xstock) = &self.xstocks[index] {
                // Mixed-block reads show up here: the balance, the shares and
                // the multiplier must describe the same holding.
                if xstock.multiplier.is_zero()
                    || xstock.multiplier > U128_MAX
                    || xstock.amount(xstock.shares)? != self.balances[index]
                {
                    return Err(invalid("xStock shares, multiplier and balance disagree"));
                }
            }
        }
        for category in 0..categories {
            let record = layout::CATEGORY.at(category);
            let (offset, width) = layout::category::REFERENCE_TOKEN;
            let reference = self.small(record.part(offset, width)) as usize;
            let (offset, width) = layout::token::CATEGORY;
            if reference >= count
                || self.small(layout::TOKEN.at(reference).part(offset, width)) as usize != category
            {
                return Err(invalid("category reference token"));
            }
        }
        Ok(())
    }

    /// `getPoolData` describes where it keeps each token's fields. Refuse to
    /// decode if that description differs from [`crate::layout`].
    fn check_described_layout(&self, data: &abi::PoolData) -> Result<()> {
        use layout::control;
        for (index, described) in data.tokens.iter().enumerate() {
            let record = layout::TOKEN.at(index);
            let part = |(offset, width)| record.part(offset, width);
            let category = layout::CATEGORY.at(usize::from(described.category));
            let expected = [
                (described.price, layout::PRICE.at(index)),
                (described.tokenBidSpread, part(layout::token::SPREAD_IN)),
                (described.tokenAskSpread, part(layout::token::SPREAD_OUT)),
                (described.volumeRate, part(layout::token::SIZE_RATE)),
                (described.categorySpread, {
                    let (offset, width) = layout::category::SPREAD;
                    category.part(offset, width)
                }),
                (described.inventory, {
                    let (offset, width) = layout::category::OFFSET;
                    category.part(offset, width)
                }),
                (described.sides, control::SIDES.part(2 * index, 2)),
                (described.postedBlock, control::PRICE_BLOCK),
                (described.sequence, control::PRICE_SEQUENCE),
                (described.inventoryCap, layout::TOKEN_CAP.at(index)),
            ];
            let pair_start = if index == 0 {
                0
            } else {
                layout::pair_spread(0, index).reference()
            };
            let (decimals, category_index) = (layout::token::DECIMALS, layout::token::CATEGORY);
            if expected
                .iter()
                .any(|(actual, field)| *actual != field.reference())
                || described.pairStart != pair_start
                || described.priceTail != 0
                || u64::from(described.decimals) != self.small(part(decimals))
                || u64::from(described.category) != self.small(part(category_index))
            {
                return Err(Error::UnsupportedFormat(self.small(layout::FORMAT)));
            }
        }
        Ok(())
    }
}

pub(crate) fn mask(width: usize) -> U256 {
    if width >= 256 {
        U256::MAX
    } else {
        (U256::ONE << width) - U256::ONE
    }
}

fn invalid(what: &str) -> Error {
    Error::InvalidSnapshot(what.into())
}
