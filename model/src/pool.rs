//! The pool decoded into named values, and the quotes it gives.
use crate::{
    curve::{apply_surcharge, surcharge, CostCurve, Curve},
    layout::{self, control},
    math::*,
    snapshot::{Snapshot, XStock, WMON},
    Address, Error, Rejection, Result, I256, U256,
};
use std::sync::Arc;

/// A token's own cost of entering or leaving the pool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenCost {
    None,
    /// A flat rate, scaled by 1e18, charged on the way in and on the way out.
    Fixed {
        rate: U256,
    },
    /// A charge on the way in that grows with the pool's holding of the token.
    /// `refund_permille` thousandths of it are credited back on the way out.
    InventoryBased {
        coefficient: U256,
        refund_permille: u32,
    },
}

/// One listed token and its pricing inputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub address: Address,
    pub decimals: u8,
    /// Index into [`Pool::categories`]. Tokens of one category track the same asset.
    pub category: usize,
    /// Whether the pool currently accepts the token as swap input, and pays it as output.
    pub can_be_input: bool,
    pub can_be_output: bool,
    /// USD price of one whole token, scaled by 1e18. The pool values what it
    /// receives and holds at `price_low` and what it pays at `price_high`.
    /// Both are zero while the token has no valid price.
    pub price_low: U256,
    pub price_high: U256,
    /// Spread when the token is swap input or output, in 0.01 bp.
    pub spread_in: u32,
    pub spread_out: u32,
    /// Size charge when the token is swap input, in 0.01 bp per $1,000.
    pub size_rate: u32,
    pub cost: TokenCost,
    /// The most the pool will hold, in raw units.
    pub cap: U256,
    /// What the token contract reports the pool holds, in raw units.
    pub balance: U256,
    /// The holding the pool last recorded, in raw units. A swap prices its own
    /// two tokens from `balance` and every other token from `recorded_balance`.
    pub recorded_balance: U256,
    /// Share state when the token is an xStock.
    pub xstock: Option<XStock>,
    /// `recorded_balance` in USD at `price_low`, with the pool's native MON
    /// added for WMON. `None` if it does not fit 256 bits.
    recorded_usd: Option<U256>,
}

impl Token {
    fn scale(&self) -> U256 {
        pow10(self.decimals)
    }
    /// What the pool tracks: shares for an xStock, raw units otherwise.
    fn units(&self) -> U256 {
        self.xstock.as_ref().map_or(self.balance, |x| x.shares)
    }
    fn amount_of_units(&self, units: U256) -> Result<U256> {
        self.xstock.as_ref().map_or(Ok(units), |x| x.amount(units))
    }
    fn units_for(&self, amount: U256) -> Result<U256> {
        self.xstock
            .as_ref()
            .map_or(Ok(amount), |x| x.shares_for(amount))
    }
    /// An xStock transfer of `amount` can deliver up to this much less,
    /// because the token moves whole shares: `max(2, ceil(multiplier / 1e18))`.
    fn dust(&self) -> Result<U256> {
        match &self.xstock {
            None => Ok(U256::ZERO),
            Some(x) => Ok(mul_div_up(x.multiplier, U256::ONE, pow10(18))?.max(uint(2))),
        }
    }
    fn usd(&self, amount: U256) -> Result<U256> {
        mul_div(amount, self.price_low, self.scale())
    }
}

/// A group of tokens that track the same asset, such as USDC and USDT0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Category {
    /// Spread for any swap touching the category, in 0.01 bp.
    pub spread: u32,
    /// Position held outside the pool, in millionths of the reference token.
    /// Added to the category's holdings when the pool measures risk.
    pub offset: i64,
    /// Index of the token whose price values `offset`.
    pub reference_token: usize,
    /// Target holding in USD, scaled by 1e18. Scales inventory-based token costs.
    pub target_usd: U256,
    /// The most the pool will hold across the category, in USD scaled by 1e18.
    pub cap_usd: U256,
}

/// The transaction a swap executes in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxContext {
    /// Effective gas price in wei. For an EIP-1559 transaction this is
    /// `min(maxFeePerGas, baseFee + maxPriorityFeePerGas)`.
    pub gas_price: U256,
    /// Whether the FastLane AuctionHandler
    /// (`0xD32EdF6642D917DbBE7B8BF8e5d6F5df6a9FFF58`) is already warm when the
    /// pool is called: the transaction called it earlier, or names it in its
    /// [access list](https://eips.ethereum.org/EIPS/eip-2930). A warm
    /// AuctionHandler puts the swap in the highest surcharge tier, so leave it
    /// out of your access list; `eth_createAccessList` includes it.
    pub auction_handler_warm: bool,
}

impl TxContext {
    pub fn with_gas_price(gas_price: U256) -> Self {
        Self {
            gas_price,
            auction_handler_warm: false,
        }
    }
}

/// The result of `makerQuoteExactInput`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quote {
    pub amount_out: U256,
    /// Block of the price update the quote used (`lastPostedBlock`).
    pub price_block: u64,
}

/// A simulated swap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Swap {
    /// Input the caller provides, in raw units.
    pub amount_in: U256,
    /// What the call returns: the least the recipient receives, in raw units,
    /// after any integrator fee. For an xStock the recipient's balance can
    /// rise by a few units more.
    pub amount_out: U256,
    /// Integrator fee sent to the fee recipient; zero unless the swap is a
    /// `swapWithExtraFees` with a nonzero `extraFeePpm`.
    pub fee: U256,
    /// Execution surcharge that reduced the output, in 0.01 bp.
    pub surcharge: u32,
    /// Block of the price update the swap used.
    pub price_block: u64,
    /// `None` for an ERC-7815 swap of zero, which returns before settlement.
    settled: Option<Settled>,
}

/// What the pool holds of a swap's two tokens once the swap has settled, in
/// the units the pool tracks: shares for an xStock, raw units otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Settled {
    input: usize,
    output: usize,
    units_in: U256,
    units_out: U256,
}

/// The result of ERC-7815 `getLimits`. Each is an upper bound on its own:
/// an amount below a limit can still be rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Largest input, in raw units of the input token.
    pub sell: U256,
    /// Largest output, in raw units of the output token.
    pub buy: U256,
}

/// A price as raw output units per raw input unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fraction {
    pub numerator: U256,
    pub denominator: U256,
}

/// The pool at one block, ready to quote. Quoting makes no network calls.
///
/// Cloning is cheap; a clone shares the underlying snapshot.
#[derive(Clone, Debug)]
pub struct Pool {
    snapshot: Arc<Snapshot>,
    block_number: u64,
    base_fee: U256,
    paused: bool,
    ready: bool,
    price_block: u64,
    max_price_age: u64,
    spread_per_block: u32,
    risk_aversion: U256,
    tokens: Vec<Token>,
    categories: Vec<Category>,
}

/// One trading direction of a [`Pool`], prepared once and quoted many times.
///
/// Its methods carry the names of the contract calls they reproduce.
#[derive(Clone, Debug)]
pub struct Pair<'a> {
    pool: &'a Pool,
    input: usize,
    output: usize,
    curve: Curve,
}

impl Pool {
    /// Decode a snapshot. Quotes are evaluated at the snapshot's block until
    /// [`Pool::at_block`] moves them.
    pub fn new(snapshot: Snapshot) -> Result<Self> {
        snapshot.validate()?;
        let s = &snapshot;
        let categories: Vec<Category> = (0..s.category_count())
            .map(|index| {
                let record = layout::CATEGORY.at(index);
                let part = |(offset, width)| s.small(record.part(offset, width));
                Category {
                    spread: part(layout::category::SPREAD) as u32,
                    offset: part(layout::category::OFFSET) as i64,
                    reference_token: part(layout::category::REFERENCE_TOKEN) as usize,
                    target_usd: s.field(layout::CATEGORY_TARGET.at(index)) * pow10(12),
                    cap_usd: decode_cap(s.small(layout::CATEGORY_CAP.at(index))),
                }
            })
            .collect();
        let mut tokens = Vec::with_capacity(s.token_count());
        for (index, address) in s.tokens.iter().enumerate() {
            let record = layout::TOKEN.at(index);
            let part = |(offset, width)| s.small(record.part(offset, width));
            let coefficient = s.field(layout::COST_COEFFICIENT.at(index));
            let (price_low, price_high) = decode_price(s.small(layout::PRICE.at(index)));
            let xstock = s.xstocks[index].clone();
            let recorded_units = s.field(layout::RECORDED_UNITS.at(index));
            let mut token = Token {
                address: *address,
                decimals: part(layout::token::DECIMALS) as u8,
                category: part(layout::token::CATEGORY) as usize,
                can_be_input: s.flag(control::can_be_input(index)),
                can_be_output: s.flag(control::can_be_output(index)),
                price_low,
                price_high,
                spread_in: part(layout::token::SPREAD_IN) as u32,
                spread_out: part(layout::token::SPREAD_OUT) as u32,
                size_rate: part(layout::token::SIZE_RATE) as u32,
                cost: match part(layout::token::COST_KIND) {
                    0 => TokenCost::None,
                    1 => TokenCost::Fixed { rate: coefficient },
                    _ => TokenCost::InventoryBased {
                        coefficient,
                        refund_permille: part(layout::token::COST_REFUND) as u32,
                    },
                },
                cap: decode_cap(s.small(layout::TOKEN_CAP.at(index))),
                balance: s.balances[index],
                recorded_balance: U256::ZERO,
                xstock,
                recorded_usd: None,
            };
            token.recorded_balance = token.amount_of_units(recorded_units)?;
            token.recorded_usd = risk_usd(&token, token.recorded_balance, s.native_balance).ok();
            tokens.push(token);
        }
        Ok(Self {
            block_number: s.block.number,
            base_fee: s.block.base_fee,
            paused: s.flag(control::PAUSED) || !s.flag(control::PRICES_ENABLED),
            ready: s.flag(control::RISK_READY) && s.flag(control::TOKEN_COSTS_READY),
            price_block: s.price_block(),
            max_price_age: s.small(control::MAX_PRICE_AGE),
            spread_per_block: s.small(control::SPREAD_PER_BLOCK) as u32,
            risk_aversion: s.field(control::RISK_AVERSION),
            tokens,
            categories,
            snapshot: Arc::new(snapshot),
        })
    }

    /// The same state evaluated at a later block: prices are older there, and
    /// the execution surcharge compares the gas price with that block's base
    /// fee. Balances, prices and parameters stay as they were read.
    ///
    /// Quote at the block you expect your transaction to land in.
    pub fn at_block(&self, number: u64, base_fee: U256) -> Result<Self> {
        if number < self.snapshot.block.number {
            return Err(Error::InvalidSnapshot(
                "cannot evaluate before the snapshot's block".into(),
            ));
        }
        let mut pool = self.clone();
        pool.block_number = number;
        pool.base_fee = base_fee;
        Ok(pool)
    }

    /// The chain data this pool was decoded from, including the block it was read at.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    /// The block quotes are evaluated at.
    pub fn block_number(&self) -> u64 {
        self.block_number
    }
    pub fn base_fee(&self) -> U256 {
        self.base_fee
    }
    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }
    pub fn categories(&self) -> &[Category] {
        &self.categories
    }
    pub fn token(&self, address: Address) -> Option<&Token> {
        self.tokens.iter().find(|t| t.address == address)
    }
    /// Block of the latest price update.
    pub fn price_block(&self) -> u64 {
        self.price_block
    }
    /// The last block at which the current prices can be traded on. Every swap
    /// is rejected after it until the prices are updated.
    pub fn prices_valid_through(&self) -> u64 {
        self.price_block + self.max_price_age
    }
    /// Spread added per block of price age, in 0.01 bp.
    pub fn spread_per_block(&self) -> u32 {
        self.spread_per_block
    }

    /// Prepare one trading direction. Fails when the pool would reject every
    /// amount for it, for example while prices are too old.
    pub fn pair(&self, token_in: Address, token_out: Address) -> Result<Pair<'_>> {
        if token_in == token_out {
            return Err(Rejection::IdenticalTokens.into());
        }
        let index = |token| {
            self.snapshot
                .index_of(token)
                .ok_or(Rejection::UnknownToken(token))
        };
        let (input, output) = (index(token_in)?, index(token_out)?);
        let (tin, tout) = (&self.tokens[input], &self.tokens[output]);
        if tin.units() > U128_MAX || tout.units() > U128_MAX {
            return Err(Rejection::InventoryLimit.into());
        }
        if self.paused {
            return Err(Rejection::Paused.into());
        }
        if !self.ready {
            return Err(Rejection::NotReady.into());
        }
        if !tin.can_be_input {
            return Err(Rejection::InputDisabled.into());
        }
        if !tout.can_be_output {
            return Err(Rejection::OutputDisabled.into());
        }
        if self.price_block == 0
            || self.price_block > self.block_number
            || self.max_price_age == 0
            || self.block_number - self.price_block > self.max_price_age
            || tin.price_low.is_zero()
            || tout.price_high.is_zero()
        {
            return Err(Rejection::StalePrices.into());
        }
        let (a, b) = (tin.category, tout.category);
        let age = self.block_number - self.price_block;
        let spread = u64::from(tin.spread_in)
            + u64::from(tout.spread_out)
            + u64::from(self.categories[a].spread)
            + u64::from(self.categories[b].spread)
            + age * u64::from(self.spread_per_block)
            + self.snapshot.small(layout::pair_spread(input, output));
        if spread >= 1_000_000 {
            return Err(Rejection::SpreadTooWide.into());
        }
        let (variance, gradient) = if self.risk_aversion.is_zero() || a == b {
            (I256::ZERO, I256::ZERO)
        } else {
            self.risk_terms(input, output)?
        };
        // floor(sqrt(int256.max / 131070)): the largest USD value whose square,
        // times any variance, fits the signed arithmetic.
        const MAX_NOTIONAL: U256 = U256::from_limbs([9851759478235162, 36029071900016680, 0, 0]);
        Ok(Pair {
            curve: Curve {
                price_in: tin.price_low,
                price_out: tout.price_high,
                scale_in: tin.scale(),
                scale_out: tout.scale(),
                dust_in: tin.dust()?,
                dust_out: tout.dust()?,
                spread: uint(spread),
                risk_aversion: self.risk_aversion,
                size_rate: uint(u64::from(tin.size_rate)),
                variance,
                gradient,
                cost_in: self.cost_curve(tin, true)?,
                cost_out: self.cost_curve(tout, false)?,
                max_amount_in: sub(
                    mul_div_up(MAX_NOTIONAL + U256::ONE, tin.scale(), tin.price_low)?,
                    U256::ONE,
                )?,
            },
            pool: self,
            input,
            output,
        })
    }

    /// The pool as it will be once `swap` has settled, for quoting a later
    /// swap of the same route. `swap` must come from this pool.
    ///
    /// Settlement records what the pool holds of the swap's two tokens, which
    /// also takes in anything sent to the pool since they were last recorded.
    pub fn after(&self, swap: &Swap) -> Result<Self> {
        let Some(settled) = &swap.settled else {
            return Ok(self.clone());
        };
        let mut snapshot = (*self.snapshot).clone();
        for (index, units) in [
            (settled.input, settled.units_in),
            (settled.output, settled.units_out),
        ] {
            let token = self.tokens.get(index).ok_or(Error::Arithmetic)?;
            snapshot.balances[index] = token.amount_of_units(units)?;
            if let Some(xstock) = &mut snapshot.xstocks[index] {
                xstock.shares = units;
            }
            crate::events::set_recorded_units(&mut snapshot, index, units);
        }
        let mut pool = Self::new(snapshot)?;
        pool.block_number = self.block_number;
        pool.base_fee = self.base_fee;
        Ok(pool)
    }

    fn covariance(&self, a: usize, b: usize) -> i64 {
        i64::from(self.snapshot.small(layout::covariance(a, b)) as u16 as i16)
    }

    /// How a USD of this trade changes portfolio risk: `variance` per squared
    /// USD and `gradient` per USD, both from the category covariance matrix.
    fn risk_terms(&self, input: usize, output: usize) -> Result<(I256, I256)> {
        let (a, b) = (self.tokens[input].category, self.tokens[output].category);
        let variance = self.covariance(a, a) + self.covariance(b, b) - 2 * self.covariance(a, b);
        let mut difference = [0i64; layout::MAX_TOKENS];
        let mut exposure = [I256::ZERO; layout::MAX_TOKENS];
        for (index, category) in self.categories.iter().enumerate() {
            difference[index] = self.covariance(a, index) - self.covariance(b, index);
            if difference[index] != 0 {
                let reference_price = to_signed(self.tokens[category.reference_token].price_low)?;
                exposure[index] = signed_div(
                    signed_mul(signed(category.offset), reference_price)?,
                    signed(1_000_000),
                )?;
            }
        }
        for (index, token) in self.tokens.iter().enumerate() {
            if difference[token.category] == 0 {
                continue;
            }
            let value = if index == input || index == output {
                risk_usd(token, token.balance, self.snapshot.native_balance)?
            } else {
                token.recorded_usd.ok_or(Error::Arithmetic)?
            };
            if value > U128_MAX {
                return Err(Rejection::InventoryLimit.into());
            }
            exposure[token.category] = signed_add(exposure[token.category], to_signed(value)?)?;
        }
        let mut gradient = I256::ZERO;
        for index in 0..self.categories.len() {
            if difference[index] != 0 {
                gradient = signed_add(
                    gradient,
                    signed_div(
                        signed_mul(exposure[index], signed(difference[index]))?,
                        signed(1_000_000_000),
                    )?,
                )?;
            }
        }
        Ok((signed(variance), gradient))
    }

    fn cost_curve(&self, token: &Token, entering: bool) -> Result<CostCurve> {
        let (coefficient, refund_permille) = match &token.cost {
            TokenCost::None => return Ok(CostCurve::default()),
            TokenCost::Fixed { rate } => {
                return Ok(CostCurve {
                    fixed_rate: *rate,
                    ..CostCurve::default()
                })
            }
            TokenCost::InventoryBased {
                coefficient,
                refund_permille,
            } => (*coefficient, *refund_permille),
        };
        let target = self.categories[token.category].target_usd;
        if target.is_zero() {
            return Err(Rejection::TokenCost.into());
        }
        let inventory_usd = token.usd(token.balance)?;
        if inventory_usd > U128_MAX {
            return Err(Rejection::InventoryLimit.into());
        }
        let slope = if entering {
            mul_div_up(coefficient, pow10(36), target)?
        } else {
            let slope = mul_div(
                mul(coefficient, uint(u64::from(refund_permille)))?,
                pow10(36),
                mul(target, uint(1000))?,
            )?;
            if !slope.is_zero() && inventory_usd > (pow10(54) - U256::ONE) / slope {
                return Err(Rejection::TokenCost.into());
            }
            slope
        };
        Ok(CostCurve {
            fixed_rate: U256::ZERO,
            slope,
            inventory_usd,
        })
    }
}

impl Pair<'_> {
    fn tokens(&self) -> (&Token, &Token) {
        (
            &self.pool.tokens[self.input],
            &self.pool.tokens[self.output],
        )
    }

    fn surcharge(&self, tx: TxContext) -> u32 {
        surcharge(
            tx.gas_price,
            self.pool.base_fee,
            tx.auction_handler_warm,
            self.pool.price_block == self.pool.block_number,
        )
    }

    /// `makerQuoteExactInput(tokenIn, tokenOut, amountIn)`.
    ///
    /// Leaves out the execution surcharge and checks inventory against 90% of
    /// each cap, exactly as the contract's read-only quote does.
    pub fn maker_quote_exact_input(&self, amount_in: U256) -> Result<Quote> {
        let amount_out = self.curve.evaluate(amount_in, 0)?;
        if amount_out.is_zero() {
            return Err(Rejection::ZeroFill.into());
        }
        self.check_caps(amount_in, add(amount_out, self.curve.dust_out)?, true)?;
        Ok(Quote {
            amount_out,
            price_block: self.pool.price_block,
        })
    }

    /// `makerSwapExactInput(tokenIn, tokenOut, amountIn, minAmountOut, recipient, deadlineBlock)`.
    ///
    /// `Swap::amount_out` is the call's return value; compare it with your
    /// `minAmountOut`.
    pub fn maker_swap_exact_input(&self, amount_in: U256, tx: TxContext) -> Result<Swap> {
        self.swap_with_extra_fees(amount_in, 0, tx)
    }

    /// `swapWithExtraFees(tokenIn, tokenOut, amountIn, minAmountOut, expiry, extraFeePpm, feeRecipient)`.
    ///
    /// `extra_fee_ppm` is in parts per million of the output: 100 is one basis
    /// point, 10,000 is 1%. `Swap::amount_out` is net of `Swap::fee`.
    pub fn swap_with_extra_fees(
        &self,
        amount_in: U256,
        extra_fee_ppm: u32,
        tx: TxContext,
    ) -> Result<Swap> {
        if extra_fee_ppm > 1_000_000 {
            return Err(Rejection::FeeTooHigh.into());
        }
        let surcharge = self.surcharge(tx);
        // The surcharge is taken from the quoted amount, after the output's
        // rounding allowance.
        let quoted = self.curve.evaluate(amount_in, 0)?;
        let gross = apply_surcharge(quoted, surcharge)?;
        if gross.is_zero() {
            return Err(Rejection::ZeroFill.into());
        }
        let fee = mul_div(gross, uint(u64::from(extra_fee_ppm)), uint(1_000_000))?;
        let amount_out = gross - fee;
        let to_recipient = if amount_out.is_zero() {
            U256::ZERO
        } else {
            add(amount_out, self.curve.dust_out)?
        };
        self.settle(amount_in, amount_out, fee, [to_recipient, fee], surcharge)
    }

    /// ERC-7815 `swap(poolId, sellToken, buyToken, OrderSide.Sell, amountIn)`.
    ///
    /// Rounds differently from `makerSwapExactInput` for an xStock output: the
    /// surcharge is taken before the rounding allowance, not after.
    pub fn swap_sell(&self, amount_in: U256, tx: TxContext) -> Result<Swap> {
        let surcharge = self.surcharge(tx);
        if amount_in.is_zero() {
            return Ok(self.nothing(surcharge));
        }
        let amount_out = self.curve.evaluate(amount_in, surcharge)?;
        if amount_out.is_zero() {
            return Err(Rejection::ZeroFill.into());
        }
        let to_recipient = add(amount_out, self.curve.dust_out)?;
        self.settle(
            amount_in,
            amount_out,
            U256::ZERO,
            [to_recipient, U256::ZERO],
            surcharge,
        )
    }

    /// ERC-7815 `swap(poolId, sellToken, buyToken, OrderSide.Buy, amountOut)`.
    ///
    /// `Swap::amount_in` is the smallest input that pays `amount_out`.
    pub fn swap_buy(&self, amount_out: U256, tx: TxContext) -> Result<Swap> {
        let limit = self.tokens().1.balance.saturating_sub(self.curve.dust_out);
        if amount_out > limit {
            return Err(Rejection::AmountTooLarge { limit }.into());
        }
        let surcharge = self.surcharge(tx);
        if amount_out.is_zero() {
            return Ok(self.nothing(surcharge));
        }
        let amount_in = self.curve.amount_in_for(amount_out, surcharge)?;
        let to_recipient = add(amount_out, self.curve.dust_out)?;
        self.settle(
            amount_in,
            amount_out,
            U256::ZERO,
            [to_recipient, U256::ZERO],
            surcharge,
        )
    }

    /// The contract accepts an ERC-7815 swap of zero and returns before
    /// settlement: nothing moves, and the pool records nothing.
    fn nothing(&self, surcharge: u32) -> Swap {
        Swap {
            amount_in: U256::ZERO,
            amount_out: U256::ZERO,
            fee: U256::ZERO,
            surcharge,
            price_block: self.pool.price_block,
            settled: None,
        }
    }

    /// ERC-7815 `price(poolId, sellToken, buyToken, [amountIn])`: the price of
    /// the next unit after trading `amount_in`, not the average price of the trade.
    pub fn price(&self, amount_in: U256) -> Result<Fraction> {
        let amount_out = self.curve.evaluate(amount_in, 0)?;
        let paid = if amount_out.is_zero() {
            U256::ZERO
        } else {
            add(amount_out, self.curve.dust_out)?
        };
        self.check_caps(amount_in, paid, true)?;
        let (numerator, denominator) = self.curve.marginal_price(amount_in)?;
        Ok(Fraction {
            numerator,
            denominator,
        })
    }

    /// ERC-7815 `getLimits(poolId, sellToken, buyToken)`.
    pub fn get_limits(&self) -> Result<Limits> {
        let (tin, tout) = self.tokens();
        let cap = quote_threshold(tin.cap)?.min(tin.amount_of_units(U128_MAX)?);
        let sell = if !tout.balance.is_zero() && tin.balance < cap {
            self.curve.max_amount_in.min(cap - tin.balance)
        } else {
            U256::ZERO
        };
        Ok(Limits {
            sell,
            buy: tout.balance.saturating_sub(self.curve.dust_out),
        })
    }

    /// Move the tokens the way settlement does, each transfer rounded down to
    /// whole shares on its own, then check the full inventory caps.
    fn settle(
        &self,
        amount_in: U256,
        amount_out: U256,
        fee: U256,
        transfers_out: [U256; 2],
        surcharge: u32,
    ) -> Result<Swap> {
        let (tin, tout) = self.tokens();
        let units_in = add(tin.units(), tin.units_for(amount_in)?)?;
        let mut units_out = tout.units();
        for transfer in transfers_out {
            units_out = sub(units_out, tout.units_for(transfer)?)
                .map_err(|_| Error::from(Rejection::InventoryLimit))?;
        }
        if units_in > U128_MAX {
            return Err(Rejection::InventoryLimit.into());
        }
        let received = sub(tin.amount_of_units(units_in)?, tin.balance)?;
        let paid = sub(tout.balance, tout.amount_of_units(units_out)?)?;
        self.check_caps(received, paid, false)?;
        Ok(Swap {
            amount_in,
            amount_out,
            fee,
            surcharge,
            price_block: self.pool.price_block,
            settled: Some(Settled {
                input: self.input,
                output: self.output,
                units_in,
                units_out,
            }),
        })
    }

    /// The pool refuses a swap that would leave it holding more of the input
    /// token, or of the input token's category, than its cap. A holding that
    /// is already above a cap may shrink but not grow. Quotes use 90% of each
    /// cap so that a quoted swap still fits when it settles.
    fn check_caps(&self, received: U256, paid: U256, for_quote: bool) -> Result<()> {
        let (tin, tout) = self.tokens();
        let threshold = |cap| {
            if for_quote {
                quote_threshold(cap)
            } else {
                Ok(cap)
            }
        };
        let token_limit = threshold(tin.cap)?
            .max(tin.balance)
            .min(tin.amount_of_units(U128_MAX)?);
        if paid > tout.balance || tin.balance > token_limit || received > token_limit - tin.balance
        {
            return Err(Rejection::InventoryLimit.into());
        }
        let native = self.pool.snapshot.native_balance;
        let (mut before, mut after) = (U256::ZERO, U256::ZERO);
        for (index, token) in self.pool.tokens.iter().enumerate() {
            if token.category != tin.category {
                continue;
            }
            if index == self.input {
                before = add(before, risk_usd(token, token.balance, native)?)?;
                after = add(
                    after,
                    risk_usd(token, add(token.balance, received)?, native)?,
                )?;
            } else if index == self.output {
                before = add(before, risk_usd(token, token.balance, native)?)?;
                after = add(after, risk_usd(token, token.balance - paid, native)?)?;
            } else {
                let value = token.recorded_usd.ok_or(Error::Arithmetic)?;
                before = add(before, value)?;
                after = add(after, value)?;
            }
        }
        if after > threshold(self.pool.categories[tin.category].cap_usd)?.max(before) {
            return Err(Rejection::InventoryLimit.into());
        }
        Ok(())
    }
}

/// USD value of a holding as the pool measures it for risk and caps: at the
/// lower price, with the pool's native MON counted as WMON.
fn risk_usd(token: &Token, amount: U256, native_balance: U256) -> Result<U256> {
    let amount = if token.address == WMON {
        add(amount, native_balance)?
    } else {
        amount
    };
    token.usd(amount)
}

/// Quotes stay strictly below 90% of a cap: `ceil(cap * 90 / 100) - 1`.
fn quote_threshold(cap: U256) -> Result<U256> {
    if cap.is_zero() {
        return Ok(cap);
    }
    sub(mul_div_up(cap, uint(90), uint(100))?, U256::ONE)
}

/// A price field holds a 56-bit mantissa `m` and an 8-bit exponent `e`. The
/// price lies between `m * 10^(18 - e)` and `(m + 1) * 10^(18 - e)`.
fn decode_price(packed: u64) -> (U256, U256) {
    let mantissa = packed & ((1 << 56) - 1);
    let exponent = (packed >> 56) as u8;
    if mantissa < 2_000_000 || exponent > 18 {
        return (U256::ZERO, U256::ZERO);
    }
    let unit = pow10(18 - exponent);
    (uint(mantissa) * unit, uint(mantissa + 1) * unit)
}

/// A cap field holds a 16-bit mantissa and an 8-bit shift: `mantissa << shift`.
/// All ones means no cap.
pub(crate) fn decode_cap(packed: u64) -> U256 {
    if packed == 0xff_ffff {
        return U256::MAX;
    }
    uint(packed & 0xffff) << ((packed >> 16) as usize)
}

/// The cap field the contract stores for `amount`; it rounds down.
pub(crate) fn encode_cap(amount: U256) -> u64 {
    if amount == U256::MAX {
        return 0xff_ffff;
    }
    let shift = amount.bit_len().saturating_sub(16);
    (amount >> shift).as_limbs()[0] | (shift as u64) << 16
}
