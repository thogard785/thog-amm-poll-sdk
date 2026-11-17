//! Where each pricing input lives in the pool's storage.
//!
//! `getPoolData` returns storage words 2 to 127 of the pool contract. Every
//! value a quote needs is a bit field inside one of those words. This module
//! names each field once; the rest of the crate never spells a slot number.
//!
//! Bit offsets count from the least significant bit. The same table, with
//! units and meanings, is in the integration guide's reference:
//! <https://github.com/thogard785/thog-amm-integration-guide/blob/main/REFERENCE.md#state-layout>

/// First and last storage word returned by `getPoolData`.
pub const FIRST_SLOT: u16 = 2;
pub const LAST_SLOT: u16 = 127;
/// Number of words returned by `getPoolData`.
pub const WORD_COUNT: usize = (LAST_SLOT - FIRST_SLOT + 1) as usize;
/// The pool lists at most this many tokens, and at most this many categories.
pub const MAX_TOKENS: usize = 28;

/// The state format this crate decodes, as the pool reports it in [`FORMAT`]:
/// storage layout 8, update payload format 8, pricing model 3.
pub const SUPPORTED_FORMAT: u64 = 8 | 8 << 8 | 3 << 16;
/// `PoolData.schemaVersion` for that format.
pub const SUPPORTED_SCHEMA: u16 = 8;

/// One bit field: `width` bits starting at bit `offset` of storage word `slot`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    pub slot: u16,
    pub offset: usize,
    pub width: usize,
}

impl Field {
    pub const fn new(slot: u16, offset: usize, width: usize) -> Self {
        Self {
            slot,
            offset,
            width,
        }
    }

    /// A narrower field inside this one, `offset` bits above its lowest bit.
    pub const fn part(self, offset: usize, width: usize) -> Self {
        Self::new(self.slot, self.offset + offset, width)
    }

    /// How `getPoolData` itself describes a field in its token and category
    /// metadata: `slot << 16 | offset << 8 | width`.
    pub const fn reference(self) -> u32 {
        (self.slot as u32) << 16 | (self.offset as u32) << 8 | self.width as u32
    }
}

/// Equal-width fields packed from the low bits upward, `per_word` to a word,
/// across consecutive words starting at `first_slot`.
#[derive(Clone, Copy, Debug)]
pub struct Packed {
    pub first_slot: u16,
    pub width: usize,
    pub per_word: usize,
}

impl Packed {
    pub const fn at(self, index: usize) -> Field {
        Field::new(
            self.first_slot + (index / self.per_word) as u16,
            index % self.per_word * self.width,
            self.width,
        )
    }

    /// Words needed to hold `count` fields.
    pub const fn words(self, count: usize) -> usize {
        count.div_ceil(self.per_word)
    }
}

/// Slot 2: counts, switches and the pool-wide pricing parameters.
pub mod control {
    use super::Field;
    pub const SLOT: u16 = 2;
    /// Storage layout version; must be 8.
    pub const LAYOUT_VERSION: Field = Field::new(SLOT, 0, 8);
    pub const TOKEN_COUNT: Field = Field::new(SLOT, 8, 6);
    pub const CATEGORY_COUNT: Field = Field::new(SLOT, 14, 6);
    /// Counter of price updates.
    pub const PRICE_SEQUENCE: Field = Field::new(SLOT, 20, 16);
    /// Block of the latest price update.
    pub const PRICE_BLOCK: Field = Field::new(SLOT, 36, 32);
    /// Two switches per token: bit `2i` lets token `i` be swap input,
    /// bit `2i + 1` lets it be swap output.
    pub const SIDES: Field = Field::new(SLOT, 68, 64);
    pub const PAUSED: Field = Field::new(SLOT, 132, 1);
    pub const PRICES_ENABLED: Field = Field::new(SLOT, 133, 1);
    /// Set once spreads, covariance and limits have been published.
    pub const RISK_READY: Field = Field::new(SLOT, 134, 1);
    /// Set once token costs have been published.
    pub const TOKEN_COSTS_READY: Field = Field::new(SLOT, 135, 1);
    /// Prices older than this many blocks cannot be traded on.
    pub const MAX_PRICE_AGE: Field = Field::new(SLOT, 138, 8);
    /// Spread added per block of price age, in 0.01 bp.
    pub const SPREAD_PER_BLOCK: Field = Field::new(SLOT, 146, 16);
    /// Multiplier on the portfolio risk charge, scaled by 1e6.
    pub const RISK_AVERSION: Field = Field::new(SLOT, 162, 32);

    pub const fn can_be_input(token: usize) -> Field {
        Field::new(SLOT, SIDES.offset + 2 * token, 1)
    }
    pub const fn can_be_output(token: usize) -> Field {
        Field::new(SLOT, SIDES.offset + 2 * token + 1, 1)
    }
}

/// Counter of changes to category offsets, in the high bits of slot 7.
pub const OFFSET_SEQUENCE: Field = Field::new(7, 160, 32);
/// Layout, update payload format and pricing model identifiers; see [`SUPPORTED_FORMAT`].
pub const FORMAT: Field = Field::new(8, 160, 24);
/// Increases whenever a token is listed or retired. Never zero.
pub const REGISTRY_GENERATION: Field = Field::new(8, 184, 32);

/// One 75-bit record per listed token, in listing order.
pub const TOKEN: Packed = Packed {
    first_slot: 9,
    width: 75,
    per_word: 3,
};

/// Fields inside a token record, as (offset, width).
pub mod token {
    /// Index of the token in the contract's compiled address table.
    pub const CATALOG_ID: (usize, usize) = (0, 5);
    pub const DECIMALS: (usize, usize) = (5, 5);
    pub const CATEGORY: (usize, usize) = (10, 5);
    /// Spread when the token is swap input, in 0.01 bp.
    pub const SPREAD_IN: (usize, usize) = (15, 16);
    /// Token cost kind: 0 none, 1 fixed, 2 inventory-based.
    pub const COST_KIND: (usize, usize) = (31, 2);
    /// Share of an inventory-based cost credited back on the way out, in 1/1000.
    pub const COST_REFUND: (usize, usize) = (33, 10);
    /// Spread when the token is swap output, in 0.01 bp.
    pub const SPREAD_OUT: (usize, usize) = (43, 16);
    /// Size charge when the token is swap input, in 0.01 bp per $1,000.
    pub const SIZE_RATE: (usize, usize) = (59, 16);
}

/// Listing position plus one for each compiled token; zero when not listed.
pub const LISTING_INDEX: Packed = Packed {
    first_slot: 19,
    width: 6,
    per_word: 32,
};

/// One encoded USD price per token; see [`crate::Token::price_low`].
pub const PRICE: Packed = Packed {
    first_slot: 20,
    width: 64,
    per_word: 4,
};

/// One 85-bit record per category.
pub const CATEGORY: Packed = Packed {
    first_slot: 27,
    width: 85,
    per_word: 3,
};

/// Fields inside a category record, as (offset, width).
pub mod category {
    /// Signed position held outside the pool, in millionths of the reference token.
    pub const OFFSET: (usize, usize) = (0, 64);
    /// Spread for any swap touching the category, in 0.01 bp.
    pub const SPREAD: (usize, usize) = (64, 16);
    /// Listing index of the token whose price values the offset.
    pub const REFERENCE_TOKEN: (usize, usize) = (80, 5);
}

/// Lower triangle of the category covariance matrix: signed 16-bit values
/// scaled by 1e9. Entry `(a, b)` with `a <= b` is field `b (b + 1) / 2 + a`.
pub const COVARIANCE: Packed = Packed {
    first_slot: 37,
    width: 16,
    per_word: 16,
};

/// Extra spread per unordered token pair, in 0.01 bp. Pair `(a, b)` with
/// `a < b` is field `b (b - 1) / 2 + a`.
pub const PAIR_SPREAD: Packed = Packed {
    first_slot: 63,
    width: 16,
    per_word: 16,
};

/// Token cost coefficient per token, scaled by 1e18.
pub const COST_COEFFICIENT: Packed = Packed {
    first_slot: 87,
    width: 64,
    per_word: 4,
};

/// Target holding per category in USD, scaled by 1e6.
pub const CATEGORY_TARGET: Packed = Packed {
    first_slot: 94,
    width: 64,
    per_word: 4,
};

/// Largest holding per token in raw units; see [`crate::Token::cap`] for the encoding.
pub const TOKEN_CAP: Packed = Packed {
    first_slot: 101,
    width: 24,
    per_word: 10,
};

/// Largest holding per category in USD scaled by 1e18, same encoding.
pub const CATEGORY_CAP: Packed = Packed {
    first_slot: 104,
    width: 24,
    per_word: 10,
};

/// The balance the pool last recorded per token: raw units, or shares for an xStock.
pub const RECORDED_UNITS: Packed = Packed {
    first_slot: 107,
    width: 128,
    per_word: 2,
};

/// One bit per word of [`RECORDED_UNITS`], set while that whole word is zero.
pub const RECORDED_ZERO_SLOT: u16 = 121;

pub const fn covariance(a: usize, b: usize) -> Field {
    let (low, high) = if a <= b { (a, b) } else { (b, a) };
    COVARIANCE.at(high * (high + 1) / 2 + low)
}

/// `a` and `b` must differ.
pub const fn pair_spread(a: usize, b: usize) -> Field {
    let (low, high) = if a <= b { (a, b) } else { (b, a) };
    PAIR_SPREAD.at(high * (high - 1) / 2 + low)
}
