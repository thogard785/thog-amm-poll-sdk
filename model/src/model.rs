use crate::{
    math::*,
    state::{covariance_location, pair_location},
    Address, Error, PoolState, Result, I256, U256,
};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quote {
    pub amount_in: U256,
    pub amount_out: U256,
    pub notional_usd_wad: U256,
    pub penalty_usd_wad: U256,
    pub last_posted_block: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fraction {
    pub numerator: U256,
    pub denominator: U256,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub sell: U256,
    pub buy: U256,
}
#[derive(Clone, Debug)]
pub struct PoolModel {
    state: Arc<PoolState>,
    quote_block_number: u64,
    quote_base_fee_per_gas: U256,
}
/// One directed pair prepared against one immutable model and quote block.
/// Reuse it for amount ladders, limits and execution quotes. Preparation and
/// successful quotes allocate nothing. Prepare again from each new snapshot or
/// projected model; the borrow prevents silently mixing state or block contexts.
#[derive(Clone, Debug)]
pub struct PreparedPair<'a> {
    model: &'a PoolModel,
    curve: Curve,
}
#[derive(Clone, Debug)]
struct Curve {
    input: usize,
    output: usize,
    price_in: U256,
    price_out: U256,
    penalty_price: U256,
    input_scale: U256,
    output_scale: U256,
    spread: U256,
    lambda: U256,
    input_limit: U256,
    variance: I256,
    gradient: I256,
    posted: u64,
}
impl PoolModel {
    pub fn new(state: PoolState) -> Result<Self> {
        state.validate()?;
        Ok(Self::from_validated(state))
    }
    /// Build from a decoded contract image, validating the state exactly once.
    pub fn from_snapshot(
        proxy: Address,
        page: crate::abi::PoolData,
        hash: Option<crate::B256>,
    ) -> Result<Self> {
        Ok(Self::from_validated(PoolState::from_snapshot(
            proxy, page, hash,
        )?))
    }
    fn from_validated(state: PoolState) -> Self {
        Self {
            quote_block_number: state.block.number,
            quote_base_fee_per_gas: state.block.base_fee_per_gas,
            state: Arc::new(state),
        }
    }
    pub fn state(&self) -> &PoolState {
        &self.state
    }
    /// Evaluate the observed inventory/prices at a hypothetical execution block.
    /// Preserves the source snapshot and hash; assumes no intervening state updates.
    /// The caller supplies that block's base fee (it is not predicted by this model).
    pub fn at_block(&self, number: u64, base_fee_per_gas: U256) -> Result<Self> {
        if number < self.state.block.number {
            return Err(Error::InvalidData(
                "quote block predates source snapshot".into(),
            ));
        }
        let mut projected = self.clone();
        projected.quote_block_number = number;
        projected.quote_base_fee_per_gas = base_fee_per_gas;
        Ok(projected)
    }
    pub fn quote_block_number(&self) -> u64 {
        self.quote_block_number
    }
    pub fn quote_base_fee_per_gas(&self) -> U256 {
        self.quote_base_fee_per_gas
    }
    pub fn into_state(self) -> PoolState {
        Arc::unwrap_or_clone(self.state)
    }
    pub fn index(&self, token: Address) -> Result<usize> {
        self.state
            .tokens
            .iter()
            .position(|t| t.token == token)
            .ok_or(Error::UnknownToken(token))
    }
    pub fn max_index(&self) -> u8 {
        (self.state.tokens.len() - 1) as u8
    }
    pub fn prepare(&self, input: Address, output: Address) -> Result<PreparedPair<'_>> {
        Ok(PreparedPair {
            model: self,
            curve: self.curve(input, output)?,
        })
    }
    /// Matches makerQuoteExactInput: includes output balance, excludes execution friction.
    pub fn quote_exact_input(
        &self,
        input: Address,
        output: Address,
        amount: U256,
    ) -> Result<Quote> {
        if amount.is_zero() {
            return Err(Error::Unavailable("zero fill"));
        }
        self.prepare(input, output)?.quote_exact_input(amount)
    }
    /// Quote the output for an input amount, including balance and exposure limits.
    /// `gas_price` is the transaction's effective gas price in wei, not its fee cap.
    /// Prices include the pool's additional spread when it exceeds twice the block base fee.
    pub fn quote_execution_exact_input(
        &self,
        input: Address,
        output: Address,
        amount: U256,
        gas_price: U256,
    ) -> Result<Quote> {
        if amount.is_zero() {
            return Err(Error::Unavailable("zero fill"));
        }
        self.prepare(input, output)?
            .quote_execution_exact_input(amount, gas_price)
    }
    /// Quote the input needed for an exact output amount using contract rounding.
    /// `gas_price` is the transaction's effective gas price in wei, not its fee cap.
    pub fn quote_exact_output(
        &self,
        input: Address,
        output: Address,
        amount: U256,
        gas_price: U256,
    ) -> Result<Quote> {
        self.prepare(input, output)?
            .quote_exact_output(amount, gas_price)
    }
    /// ERC-7815 price(): raw output units per raw input unit, at the specified input.
    pub fn marginal_price(
        &self,
        input: Address,
        output: Address,
        amount: U256,
    ) -> Result<Fraction> {
        self.prepare(input, output)?.marginal_price(amount)
    }
    pub fn limits(&self, input: Address, output: Address) -> Result<Limits> {
        self.prepare(input, output)?.limits()
    }
    fn exposure(&self, category: usize) -> Result<I256> {
        let raw = self
            .state
            .field(self.state.categories[category].inventory)?
            .to::<u64>();
        Ok(signed(if raw & (1 << 55usize) != 0 {
            raw as i128 - (1i128 << 56usize)
        } else {
            raw as i128
        }))
    }
    fn covariance(&self, left: usize, right: usize) -> Result<I256> {
        if left == 0 || right == 0 {
            return Ok(I256::ZERO);
        }
        let (slot, offset) = covariance_location(left, right);
        Ok(signed(
            ((self.state.word(slot)? >> offset) & mask(16)).to::<u16>() as i16 as i128,
        ))
    }
    fn category_usd(&self, category: usize, max_age: u64) -> Result<I256> {
        let exposure = self.exposure(category)?;
        if exposure.is_zero() {
            return Ok(I256::ZERO);
        }
        let c = &self.state.categories[category];
        if c.anchorTokenIndex >= 8 && self.age(c.postedBlock)? > max_age {
            return Err(Error::Unavailable("stale risk price"));
        }
        let price = decode_price(self.state.packed_price(c.price, c.priceTail)?, false);
        idiv(im(exposure, to_signed(price)?)?, signed(1_000_000))
    }
    fn age(&self, reference: u32) -> Result<u64> {
        let posted = self.state.field(reference)?.to::<u64>();
        if posted == 0 || posted > self.quote_block_number {
            return Err(Error::Unavailable("stale prices"));
        }
        Ok(self.quote_block_number - posted)
    }
    fn curve(&self, input: Address, output: Address) -> Result<Curve> {
        if input == output {
            return Err(Error::Unavailable("identical tokens"));
        }
        let a = self.index(input)?;
        let b = self.index(output)?;
        let ta = &self.state.tokens[a];
        let tb = &self.state.tokens[b];
        let v1 = self.state.word(75)?;
        let r1 = self.state.word(77)?;
        if !v1.bit(255) || v1.bit(0) {
            return Err(Error::Unavailable("maker paused"));
        }
        if (a == 7 || b == 7) && (!self.state.word(76)?.bit(255) || !self.state.word(80)?.bit(255))
        {
            return Err(Error::Unavailable("side disabled"));
        }
        if !self.state.field(ta.sides)?.bit(0) {
            return Err(Error::Unavailable("sell side disabled"));
        }
        if !self.state.field(tb.sides)?.bit(1) {
            return Err(Error::Unavailable("buy side disabled"));
        }
        let global_posted = ((v1 >> 31usize) & mask(32)).to::<u64>();
        if global_posted == 0 || global_posted > self.quote_block_number {
            return Err(Error::Unavailable("stale prices"));
        }
        let age = (self.quote_block_number - global_posted)
            .max(self.age(ta.postedBlock)?)
            .max(self.age(tb.postedBlock)?);
        let max_age = ((r1 >> 198usize) & mask(8)).to::<u64>();
        if max_age == 0 || age > max_age {
            return Err(Error::Unavailable("stale prices"));
        }
        let price_in = decode_price(self.state.packed_price(ta.price, ta.priceTail)?, false);
        let price_out = decode_price(self.state.packed_price(tb.price, tb.priceTail)?, true);
        let penalty_price = decode_price(self.state.packed_price(tb.price, tb.priceTail)?, false);
        if price_in.is_zero() || price_out.is_zero() {
            return Err(Error::Unavailable("stale prices"));
        }
        let (slot, offset) = pair_location(a, b);
        let pair_word = self.state.word(slot)?;
        if a.max(b) < 8 && !pair_word.bit(255) {
            return Err(Error::Unavailable("pair risk uninitialized"));
        }
        let spread = u(4)
            * (self.state.field(ta.tokenSpread)?
                + self.state.field(tb.tokenSpread)?
                + self.state.field(ta.categorySpread)?
                + self.state.field(tb.categorySpread)?
                + ((pair_word >> offset) & mask(10)))
            + u(age) * ((r1 >> 190usize) & mask(8));
        if spread >= u(400_000) {
            return Err(Error::Unavailable("invalid spread"));
        }
        let ca = ta.category as usize;
        let cb = tb.category as usize;
        let lambda = if ca == cb {
            U256::ZERO
        } else {
            (r1 >> 110usize) & mask(16)
        };
        let mut variance = I256::ZERO;
        let mut gradient = I256::ZERO;
        if !lambda.is_zero() {
            if !self.state.word(76)?.bit(255) {
                return Err(Error::Unavailable("layout migration required"));
            }
            variance = isub(
                ia(self.covariance(ca, ca)?, self.covariance(cb, cb)?)?,
                im(signed(2), self.covariance(ca, cb)?)?,
            )?;
            let mut ga = I256::ZERO;
            let mut gb = I256::ZERO;
            for other in 1..self.state.categories.len() {
                // Match contract order, including freshness even for zero covariance.
                let usd = self.category_usd(other, max_age)?;
                ga = ia(
                    ga,
                    idiv(im(usd, self.covariance(ca, other)?)?, signed(1_000_000_000))?,
                )?;
                gb = ia(
                    gb,
                    idiv(im(usd, self.covariance(cb, other)?)?, signed(1_000_000_000))?,
                )?;
            }
            gradient = isub(ga, gb)?;
        }
        let input_scale = pow10(ta.decimals);
        let output_scale = pow10(tb.decimals);
        let mut max_notional = max_floored_input(U256::MAX, price_out, output_scale)?;
        if !lambda.is_zero() {
            max_notional = max_notional.min(U256::from(
                664_619_068_552_888_026_590_626_425_199_190_042u128,
            ));
        }
        let input_limit = max_floored_input(max_notional, input_scale, price_in)?;
        Ok(Curve {
            input: a,
            output: b,
            price_in,
            price_out,
            penalty_price,
            input_scale,
            output_scale,
            spread,
            lambda,
            input_limit,
            variance,
            gradient,
            posted: self.quote_block_number - age,
        })
    }
    fn exposure_capacity(&self, index: usize, increasing: bool) -> Result<U256> {
        let t = &self.state.tokens[index];
        let exposure = self.exposure(t.category as usize)?;
        let headroom = if increasing {
            isub(signed((1i128 << 55usize) - 1), exposure)?
        } else {
            isub(exposure, signed(-(1i128 << 55usize)))?
        };
        if headroom <= I256::ZERO {
            return Ok(U256::ZERO);
        }
        mul_div(headroom.into_raw(), pow10(t.decimals), u(1_000_000), false)
    }
    fn check_balance(&self, c: &Curve, q: &Quote) -> Result<()> {
        if q.amount_out > self.state.balances[c.output] {
            return Err(Error::Unavailable("balance"));
        }
        Ok(())
    }
    fn check_exposure(&self, c: &Curve, q: &Quote) -> Result<()> {
        let input = &self.state.tokens[c.input];
        let output = &self.state.tokens[c.output];
        if input.category == output.category {
            return Ok(());
        }
        for (token, amount, increasing) in
            [(input, q.amount_in, true), (output, q.amount_out, false)]
        {
            // The contract bounds each rounded delta to int56::MAX as well as
            // checking the resulting inventory. Negative starting inventory
            // must not accidentally permit a delta larger than int56::MAX.
            let scale = pow10(token.decimals);
            let delta = mul_div(amount, u(1_000_000), scale, true)?;
            if delta > (U256::ONE << 55usize) - U256::ONE {
                return Err(Error::Unavailable("exposure overflow"));
            }
            let old = self.exposure(token.category as usize)?;
            let next = if increasing {
                ia(old, to_signed(delta)?)?
            } else {
                isub(old, to_signed(delta)?)?
            };
            if next < signed(-(1i128 << 55usize)) || next > signed((1i128 << 55usize) - 1) {
                return Err(Error::Unavailable("exposure overflow"));
            }
        }
        Ok(())
    }
    fn curve_limits(&self, c: &Curve) -> Result<Limits> {
        let mut sell = c.input_limit;
        let mut buy = self.state.balances[c.output];
        let cross = self.state.tokens[c.input].category != self.state.tokens[c.output].category;
        if cross {
            sell = sell.min(self.exposure_capacity(c.input, true)?);
            buy = buy.min(self.exposure_capacity(c.output, false)?);
        }
        if buy.is_zero() {
            sell = U256::ZERO;
        } else if (!cross || c.lambda.is_zero()) && buy != U256::MAX {
            // Saturation is the contract's specified inverse-overflow branch.
            let inverse = (|| -> Result<U256> {
                let fair = mul_div(buy + U256::ONE, u(400_000), u(400_000) - c.spread, true)?;
                let notional = mul_div(fair, c.price_out, c.output_scale, true)?;
                mul_div(notional, c.input_scale, c.price_in, true)
            })();
            if let Ok(input) = inverse {
                if !input.is_zero() {
                    sell = sell.min(input - U256::ONE);
                }
            }
        }
        Ok(Limits { sell, buy })
    }
}
impl PreparedPair<'_> {
    pub fn quote_exact_input(&self, amount: U256) -> Result<Quote> {
        if amount.is_zero() {
            return Err(Error::Unavailable("zero fill"));
        }
        let quote = self.curve.quote(amount, false)?;
        if quote.amount_out.is_zero() {
            return Err(Error::Unavailable("zero fill"));
        }
        self.model.check_balance(&self.curve, &quote)?;
        Ok(quote)
    }
    /// Quote an input amount with balance and exposure checks, reusing this pair's calculations.
    /// `gas_price` is the transaction's effective gas price in wei.
    pub fn quote_execution_exact_input(&self, amount: U256, gas_price: U256) -> Result<Quote> {
        if amount.is_zero() {
            return Err(Error::Unavailable("zero fill"));
        }
        let quote = self.curve.quote(
            amount,
            gas_price > mul(u(2), self.model.quote_base_fee_per_gas)?,
        )?;
        if quote.amount_out.is_zero() {
            return Err(Error::Unavailable("zero fill"));
        }
        self.model.check_balance(&self.curve, &quote)?;
        self.model.check_exposure(&self.curve, &quote)?;
        Ok(quote)
    }
    /// Quote the input for an exact output amount. `gas_price` is the effective price in wei.
    pub fn quote_exact_output(&self, amount: U256, gas_price: U256) -> Result<Quote> {
        let limits = self.limits()?;
        if amount > limits.buy {
            return Err(Error::LimitExceeded(limits.buy));
        }
        if amount.is_zero() {
            return self.curve.quote(U256::ZERO, false);
        }
        let friction = gas_price > mul(u(2), self.model.quote_base_fee_per_gas)?;
        let mut quote = self.curve.solve_output(amount, limits.sell, friction)?;
        quote.amount_out = amount;
        self.model.check_exposure(&self.curve, &quote)?;
        Ok(quote)
    }
    pub fn marginal_price(&self, amount: U256) -> Result<Fraction> {
        self.curve.marginal(amount)
    }
    pub fn limits(&self) -> Result<Limits> {
        self.model.curve_limits(&self.curve)
    }
}
fn decode_price(packed: U256, upper: bool) -> U256 {
    let mantissa = packed & mask(40);
    let exponent = packed >> 40usize;
    if exponent > u(18) || mantissa < u(if upper { 200_000 } else { 1 }) {
        return U256::ZERO;
    }
    (mantissa + u(u64::from(upper))) * pow10(18 - exponent.to::<u8>())
}
impl Curve {
    fn delta_variance(&self, v: U256) -> Result<I256> {
        let v = to_signed(v)?;
        ia(
            idiv(im(im(v, v)?, self.variance)?, to_signed(pow10(27))?)?,
            idiv(
                im(im(signed(2), v)?, self.gradient)?,
                signed(1_000_000_000_000_000_000),
            )?,
        )
    }
    fn quote(&self, amount: U256, friction: bool) -> Result<Quote> {
        if amount > self.input_limit {
            return Err(Error::LimitExceeded(self.input_limit));
        }
        let mut q = Quote {
            amount_in: amount,
            amount_out: U256::ZERO,
            notional_usd_wad: U256::ZERO,
            penalty_usd_wad: U256::ZERO,
            last_posted_block: self.posted,
        };
        if amount.is_zero() {
            return Ok(q);
        }
        q.notional_usd_wad = mul_div(amount, self.price_in, self.input_scale, false)?;
        if q.notional_usd_wad.is_zero() {
            return Ok(q);
        }
        let fair = mul_div(q.notional_usd_wad, self.output_scale, self.price_out, false)?;
        q.amount_out = mul_div(fair, u(400_000) - self.spread, u(400_000), false)?;
        if !self.lambda.is_zero() {
            let delta = self.delta_variance(q.notional_usd_wad)?;
            if delta > I256::ZERO {
                q.penalty_usd_wad = mul(delta.into_raw(), self.lambda)? / u(1_000_000);
            }
        }
        if !q.penalty_usd_wad.is_zero() {
            let penalty = mul_div(
                q.penalty_usd_wad,
                self.output_scale,
                self.penalty_price,
                true,
            )?;
            q.amount_out = q.amount_out.saturating_sub(penalty);
        }
        if friction {
            q.amount_out = mul_div(q.amount_out, u(9_990), u(10_000), false)?;
        }
        Ok(q)
    }
    fn slope(&self, amount: U256) -> Result<U256> {
        if self.lambda.is_zero() {
            return Ok(U256::ZERO);
        }
        let v = mul_div(amount, self.price_in, self.input_scale, false)?;
        let delta = self.delta_variance(v)?;
        let slope = idiv(
            im(
                ia(
                    idiv(
                        im(im(signed(2), to_signed(v)?)?, self.variance)?,
                        signed(1_000_000_000),
                    )?,
                    im(signed(2), self.gradient)?,
                )?,
                to_signed(self.lambda)?,
            )?,
            signed(1_000_000),
        )?;
        Ok(if delta >= I256::ZERO && slope > I256::ZERO {
            slope.into_raw()
        } else {
            U256::ZERO
        })
    }
    fn marginal(&self, amount: U256) -> Result<Fraction> {
        if amount > self.input_limit {
            return Err(Error::LimitExceeded(self.input_limit));
        }
        let fair = mul_div(self.price_in, pow10(36), self.price_out, false)?;
        let mut rate = mul_div(fair, u(400_000) - self.spread, u(400_000), false)?;
        let slope = self.slope(amount)?;
        if slope >= pow10(18) {
            rate = U256::ZERO;
        } else if !slope.is_zero() {
            let fair_penalty = mul_div(self.price_in, pow10(36), self.penalty_price, false)?;
            rate = rate.saturating_sub(mul_div(fair_penalty, slope, pow10(18), false)?);
        }
        if rate.is_zero() {
            return Ok(Fraction {
                numerator: U256::ZERO,
                denominator: U256::ONE,
            });
        }
        let denominator = if self.input_scale >= self.output_scale {
            mul(pow10(36), self.input_scale / self.output_scale)?
        } else {
            pow10(36) / (self.output_scale / self.input_scale)
        };
        Ok(Fraction {
            numerator: rate,
            denominator,
        })
    }
    fn marginal_positive(&self, amount: U256) -> Result<bool> {
        if amount > self.input_limit {
            return Err(Error::LimitExceeded(self.input_limit));
        }
        let slope = self.slope(amount)?;
        if slope.is_zero() {
            return Ok(true);
        }
        if slope >= pow10(18) {
            return Ok(false);
        }
        Ok(mul(
            mul(u(400_000) - self.spread, pow10(18))?,
            self.penalty_price,
        )? > mul(mul(u(400_000), slope)?, self.price_out)?)
    }
    fn linear_input(&self, output: U256, friction: bool) -> Result<U256> {
        let spread_out = if friction {
            mul_div(output, u(10_000), u(9_990), true)?
        } else {
            output
        };
        let fair = mul_div(spread_out, u(400_000), u(400_000) - self.spread, true)?;
        let notional = mul_div(fair, self.price_out, self.output_scale, true)?;
        mul_div(notional, self.input_scale, self.price_in, true)
    }
    fn solve_output(&self, amount: U256, max_input: U256, friction: bool) -> Result<Quote> {
        let unavailable = || Error::Unavailable("requested output unavailable");
        let mut low = self
            .linear_input(amount, friction)
            .map_err(|_| unavailable())?;
        if low > max_input {
            return Err(unavailable());
        }
        let mut q = self.quote(low, friction)?;
        if q.amount_out >= amount {
            return Ok(q);
        }
        if !self.marginal_positive(low)? {
            return Err(unavailable());
        }
        let mut high = low;
        loop {
            if high == max_input {
                return Err(unavailable());
            }
            let next = if high.is_zero() {
                U256::ONE
            } else if high > max_input / u(2) {
                max_input
            } else {
                high * u(2)
            };
            let next_q = self.quote(next, friction)?;
            if next_q.amount_out >= amount {
                high = next;
                q = next_q;
                break;
            }
            if !self.marginal_positive(next)? {
                let (peak, peak_q) = self.peak(high, next, friction)?;
                high = peak;
                q = peak_q;
                if q.amount_out < amount {
                    return Err(unavailable());
                }
                break;
            }
            low = next;
            high = next;
        }
        while high - low > U256::ONE {
            let mid = low + (high - low) / u(2);
            let mid_q = self.quote(mid, friction)?;
            if mid_q.amount_out >= amount {
                high = mid;
                q = mid_q;
            } else {
                low = mid;
            }
        }
        Ok(q)
    }
    fn peak(&self, mut low: U256, mut high: U256, friction: bool) -> Result<(U256, Quote)> {
        while high - low > U256::ONE {
            let mid = low + (high - low) / u(2);
            if !self.marginal_positive(mid)? {
                high = mid;
            } else {
                low = mid;
            }
        }
        let l = self.quote(low, friction)?;
        let h = self.quote(high, friction)?;
        Ok(if h.amount_out > l.amount_out {
            (high, h)
        } else {
            (low, l)
        })
    }
}
