use crate::{
    curve::Curve,
    math::*,
    state::{pair_location, WMON},
    Address, Error, PoolState, Result, I256, U256,
};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quote {
    pub amount_in: U256,
    pub amount_out: U256,
    pub notional_usd_wad: U256,
    pub penalty_usd_wad: U256,
    pub volume_cost_usd_wad: U256,
    /// Signed entry charge plus output charge/refund, in USD-wad.
    pub wrapper_cost_usd_wad: I256,
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
/// A directed pair prepared against one immutable state and quote block.
#[derive(Clone, Debug)]
pub struct PreparedPair<'a> {
    model: &'a PoolModel,
    curve: Curve,
}

impl PoolModel {
    pub fn new(state: PoolState) -> Result<Self> {
        state.validate()?;
        Ok(Self::from_validated(state))
    }
    pub fn from_snapshot(
        proxy: Address,
        page: crate::abi::PoolData,
        hash: Option<crate::B256>,
        share_balances: crate::shares::ShareBalances,
    ) -> Result<Self> {
        Ok(Self::from_validated(PoolState::from_snapshot(
            proxy,
            page,
            hash,
            share_balances,
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
    /// Advance price age only; this does not predict intervening state changes.
    pub fn at_block(&self, number: u64, base_fee_per_gas: U256) -> Result<Self> {
        if number < self.state.block.number {
            return Err(Error::InvalidData(
                "quote block predates source snapshot".into(),
            ));
        }
        let mut next = self.clone();
        next.quote_block_number = number;
        next.quote_base_fee_per_gas = base_fee_per_gas;
        Ok(next)
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
    /// Matches makerQuoteExactInput, including its 90% quote caps, without execution widening.
    pub fn quote_exact_input(
        &self,
        input: Address,
        output: Address,
        amount: U256,
    ) -> Result<Quote> {
        self.prepare(input, output)?.quote_exact_input(amount)
    }
    /// Simulates ordinary direct execution with a cold AuctionHandler. Use the
    /// explicit-context variant if an earlier route hop can warm that account.
    pub fn quote_execution_exact_input(
        &self,
        input: Address,
        output: Address,
        amount: U256,
        gas_price: U256,
    ) -> Result<Quote> {
        self.quote_execution_exact_input_with_context(input, output, amount, gas_price, false)
    }
    pub fn quote_execution_exact_input_with_context(
        &self,
        input: Address,
        output: Address,
        amount: U256,
        gas_price: U256,
        auction_handler_warm: bool,
    ) -> Result<Quote> {
        self.prepare(input, output)?
            .quote_execution_exact_input_with_context(amount, gas_price, auction_handler_warm)
    }
    pub fn quote_exact_output(
        &self,
        input: Address,
        output: Address,
        amount: U256,
        gas_price: U256,
    ) -> Result<Quote> {
        self.quote_exact_output_with_context(input, output, amount, gas_price, false)
    }
    pub fn quote_exact_output_with_context(
        &self,
        input: Address,
        output: Address,
        amount: U256,
        gas_price: U256,
        auction_handler_warm: bool,
    ) -> Result<Quote> {
        self.prepare(input, output)?
            .quote_exact_output_with_context(amount, gas_price, auction_handler_warm)
    }
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

    fn inventory_usd(
        &self,
        balances: &[U256; 28],
        index: usize,
        native: bool,
        bounded: bool,
    ) -> Result<U256> {
        let token = &self.state.tokens[index];
        let mut balance = balances[index];
        if native && token.token == WMON {
            balance = add(balance, self.state.native_balance)?;
        }
        let value = mul_div(
            balance,
            self.state.price(index, false)?,
            pow10(token.decimals),
            false,
        )?;
        if bounded && value > mask(128) {
            return Err(Error::Unavailable("inventory arithmetic range"));
        }
        Ok(value)
    }
    fn curve(&self, input: Address, output: Address) -> Result<Curve> {
        if input == output {
            return Err(Error::Unavailable("identical tokens"));
        }
        let i = self.index(input)?;
        let o = self.index(output)?;
        let s = &self.state;
        let control = s.word(2)?;
        if control.bit(132) || !control.bit(133) {
            return Err(Error::Unavailable("paused"));
        }
        if !control.bit(134) || !control.bit(135) {
            return Err(Error::Unavailable("risk uninitialized"));
        }
        if !control.bit(68 + 2 * i) || !control.bit(69 + 2 * o) {
            return Err(Error::Unavailable("side disabled"));
        }
        let posted = s.bits(2, 36, 32)?.to::<u64>();
        let max_age = s.bits(2, 138, 8)?.to::<u64>();
        if posted == 0
            || posted > self.quote_block_number
            || max_age == 0
            || self.quote_block_number - posted > max_age
        {
            return Err(Error::Unavailable("stale price"));
        }
        let (a, b) = (
            usize::from(s.tokens[i].category),
            usize::from(s.tokens[o].category),
        );
        let mut c = Curve {
            balances: s.pair_balances(i, o)?,
            input_dust: s.transfer_dust(i)?,
            output_dust: s.transfer_dust(o)?,
            input: i,
            output: o,
            price_in: s.price(i, false)?,
            price_out: s.price(o, true)?,
            input_scale: pow10(s.tokens[i].decimals),
            output_scale: pow10(s.tokens[o].decimals),
            posted,
            lambda: s.bits(2, 162, 32)?,
            volume_rate: s.token_bits(i, 59, 16)?,
            ..Curve::default()
        };
        if c.price_in.is_zero() || c.price_out.is_zero() {
            return Err(Error::Unavailable("invalid endpoint price"));
        }
        let (pair_slot, pair_offset) = pair_location(i, o);
        c.spread = s.token_bits(i, 15, 16)?
            + s.token_bits(o, 43, 16)?
            + s.bits(27 + (a / 3) as u16, a % 3 * 85 + 64, 16)?
            + s.bits(27 + (b / 3) as u16, b % 3 * 85 + 64, 16)?
            + u(self.quote_block_number - posted) * s.bits(2, 146, 16)?
            + s.bits(pair_slot, pair_offset, 16)?;
        if c.spread >= u(1_000_000) {
            return Err(Error::Unavailable("spread"));
        }
        if !c.lambda.is_zero() && a != b {
            c.variance = signed(i128::from(
                s.covariance(a, a)? + s.covariance(b, b)? - 2 * s.covariance(a, b)?,
            ));
            let mut differences = [0i64; 28];
            let mut exposures = [I256::ZERO; 28];
            for category in 0..s.categories.len() {
                differences[category] = s.covariance(a, category)? - s.covariance(b, category)?;
                if differences[category] == 0 {
                    continue;
                }
                let offset = s
                    .bits(27 + (category / 3) as u16, category % 3 * 85, 64)?
                    .to::<u64>() as i64;
                let anchor = usize::from(s.categories[category].anchorTokenIndex);
                exposures[category] = idiv(
                    im(
                        signed(i128::from(offset)),
                        to_signed(s.price(anchor, false)?)?,
                    )?,
                    signed(1_000_000),
                )?;
            }
            for index in 0..s.tokens.len() {
                let category = usize::from(s.tokens[index].category);
                if differences[category] != 0 {
                    exposures[category] = ia(
                        exposures[category],
                        to_signed(self.inventory_usd(&c.balances, index, true, true)?)?,
                    )?;
                }
            }
            for category in 0..s.categories.len() {
                c.gradient = ia(
                    c.gradient,
                    idiv(
                        im(
                            exposures[category],
                            signed(i128::from(differences[category])),
                        )?,
                        signed(1_000_000_000),
                    )?,
                )?;
            }
        }
        (c.entry_fixed, c.entry_slope, c.entry_inventory) = self.wrapper(&c.balances, i, true)?;
        (c.exit_fixed, c.exit_slope, c.exit_inventory) = self.wrapper(&c.balances, o, false)?;
        // floor(sqrt(int256.max / 131070)), matching the deployed quadratic bound.
        const MAX_NOTIONAL: U256 = U256::from_limbs([9851759478235162, 36029071900016680, 0, 0]);
        c.input_limit = max_floored_input(MAX_NOTIONAL, c.input_scale, c.price_in)?;
        Ok(c)
    }
    fn wrapper(
        &self,
        balances: &[U256; 28],
        index: usize,
        receiving: bool,
    ) -> Result<(U256, U256, U256)> {
        let s = &self.state;
        let kind = s.token_bits(index, 31, 2)?.to::<u8>();
        if kind == 0 {
            return Ok((U256::ZERO, U256::ZERO, U256::ZERO));
        }
        let coefficient = s.packed(87, index, 64, 4)?;
        if kind == 1 {
            return Ok((coefficient, U256::ZERO, U256::ZERO));
        }
        let target = mul(
            s.packed(94, usize::from(s.tokens[index].category), 64, 4)?,
            pow10(12),
        )?;
        if target.is_zero() {
            return Err(Error::Unavailable("wrapper target"));
        }
        let inventory = self.inventory_usd(balances, index, false, true)?;
        let slope = if receiving {
            mul_div(coefficient, pow10(36), target, true)?
        } else {
            mul_div(
                mul(coefficient, s.token_bits(index, 33, 10)?)?,
                pow10(36),
                mul(target, u(1000))?,
                false,
            )?
        };
        if !receiving && !slope.is_zero() && inventory > (pow10(54) - U256::ONE) / slope {
            return Err(Error::Unavailable("nonpositive wrapper value"));
        }
        Ok((U256::ZERO, slope, inventory))
    }
    fn check_inventory(
        &self,
        c: &Curve,
        mut input: U256,
        mut output: U256,
        quote: bool,
    ) -> Result<()> {
        let s = &self.state;
        if !output.is_zero() {
            output = add(output, c.output_dust)?;
        }
        if !quote {
            // Settlement floors each transfer to shares, then values the resulting holdings.
            let received_units = add(s.physical_units(c.input), s.transfer_units(c.input, input)?)?;
            let remaining_units = sub(
                s.physical_units(c.output),
                s.transfer_units(c.output, output)?,
            )?;
            if received_units > mask(128) || remaining_units > mask(128) {
                return Err(Error::Unavailable("inventory uint128"));
            }
            input = sub(
                s.amount_for_units(c.input, received_units)?,
                c.balances[c.input],
            )?;
            output = sub(
                c.balances[c.output],
                s.amount_for_units(c.output, remaining_units)?,
            )?;
        }
        let token_cap = cap_limit(s.packed(101, c.input, 24, 10)?, quote)?
            .max(c.balances[c.input])
            .min(s.amount_for_units(c.input, mask(128))?);
        if output > c.balances[c.output]
            || c.balances[c.input] > token_cap
            || input > token_cap - c.balances[c.input]
        {
            return Err(Error::Unavailable("balance"));
        }
        let category = s.tokens[c.input].category;
        let (mut value, mut before_value) = (U256::ZERO, U256::ZERO);
        for (index, token) in s.tokens.iter().enumerate() {
            if token.category != category {
                continue;
            }
            let mut balance = c.balances[index];
            if token.token == WMON {
                balance = add(balance, s.native_balance)?;
            }
            before_value = add(
                before_value,
                mul_div(
                    balance,
                    s.price(index, false)?,
                    pow10(token.decimals),
                    false,
                )?,
            )?;
            if index == c.input {
                balance = add(balance, input)?;
            }
            if index == c.output {
                balance = sub(balance, output)?;
            }
            value = add(
                value,
                mul_div(
                    balance,
                    s.price(index, false)?,
                    pow10(token.decimals),
                    false,
                )?,
            )?;
        }
        if value
            > cap_limit(s.packed(104, usize::from(category), 24, 10)?, quote)?.max(before_value)
        {
            return Err(Error::Unavailable("balance"));
        }
        Ok(())
    }
}

impl PreparedPair<'_> {
    pub fn quote_exact_input(&self, amount: U256) -> Result<Quote> {
        let q = self.curve.quote(amount, false)?;
        if amount.is_zero() || q.amount_out.is_zero() {
            return Err(Error::Unavailable("zero fill"));
        }
        self.model
            .check_inventory(&self.curve, amount, q.amount_out, true)?;
        Ok(q)
    }
    pub fn quote_execution_exact_input(&self, amount: U256, gas_price: U256) -> Result<Quote> {
        self.quote_execution_exact_input_with_context(amount, gas_price, false)
    }
    pub fn quote_execution_exact_input_with_context(
        &self,
        amount: U256,
        gas_price: U256,
        auction_handler_warm: bool,
    ) -> Result<Quote> {
        let friction =
            auction_handler_warm || gas_price > mul(u(2), self.model.quote_base_fee_per_gas)?;
        // makerSwapExactInput applies friction after the quoted xStock dust.
        // ERC-7815 Buy instead passes friction into the curve before dust.
        let mut q = self.curve.quote(amount, false)?;
        if friction {
            q.amount_out = mul_div(q.amount_out, u(9990), u(10000), false)?;
        }
        if amount.is_zero() || q.amount_out.is_zero() {
            return Err(Error::Unavailable("zero fill"));
        }
        self.model
            .check_inventory(&self.curve, amount, q.amount_out, false)?;
        Ok(q)
    }
    /// Minimum raw input for ERC-7815 Buy. The payout is exactly the target;
    /// min-input arithmetic may otherwise produce an extra raw output unit.
    pub fn quote_exact_output(&self, target: U256, gas_price: U256) -> Result<Quote> {
        self.quote_exact_output_with_context(target, gas_price, false)
    }
    pub fn quote_exact_output_with_context(
        &self,
        target: U256,
        gas_price: U256,
        auction_handler_warm: bool,
    ) -> Result<Quote> {
        let buy_limit =
            self.model.state.balances[self.curve.output].saturating_sub(self.curve.output_dust);
        if target > buy_limit {
            return Err(Error::LimitExceeded(buy_limit));
        }
        let friction =
            auction_handler_warm || gas_price > mul(u(2), self.model.quote_base_fee_per_gas)?;
        let mut q = self.curve.solve_output(target, friction)?;
        if !target.is_zero() {
            self.model
                .check_inventory(&self.curve, q.amount_in, target, false)?;
        }
        q.amount_out = target;
        Ok(q)
    }
    pub fn marginal_price(&self, amount: U256) -> Result<Fraction> {
        let q = self.curve.quote(amount, false)?;
        self.model
            .check_inventory(&self.curve, amount, q.amount_out, true)?;
        self.curve.marginal(amount)
    }
    pub fn limits(&self) -> Result<Limits> {
        let balance = self.model.state.balances[self.curve.input];
        let buy =
            self.model.state.balances[self.curve.output].saturating_sub(self.curve.output_dust);
        let cap = cap_limit(
            self.model.state.packed(101, self.curve.input, 24, 10)?,
            true,
        )?
        .min(
            self.model
                .state
                .amount_for_units(self.curve.input, mask(128))?,
        );
        let sell = if !self.model.state.balances[self.curve.output].is_zero() && balance < cap {
            self.curve.input_limit.min(cap - balance)
        } else {
            U256::ZERO
        };
        Ok(Limits { sell, buy })
    }
}

pub fn encode_cap(amount: U256) -> u32 {
    if amount == U256::MAX {
        return 0xffffff;
    }
    let shift = amount.bit_len().saturating_sub(16);
    (amount >> shift).to::<u32>() | ((shift as u32) << 16)
}
pub fn decode_cap(packed: U256) -> Result<U256> {
    if packed == u(0xffffff) {
        return Ok(U256::MAX);
    }
    let shift = (packed >> 16usize).to::<usize>();
    let mantissa = packed & mask(16);
    if packed > mask(24) || shift > 240 || (shift != 0 && mantissa < u(32768)) {
        return Err(Error::InvalidData("invalid inventory cap encoding".into()));
    }
    Ok(mantissa << shift)
}
fn cap_limit(packed: U256, quote: bool) -> Result<U256> {
    let value = decode_cap(packed)?;
    if quote && !value.is_zero() {
        return sub(mul_div(value, u(90), u(100), true)?, U256::ONE);
    }
    Ok(value)
}
