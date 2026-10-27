//! Integer curve of the deployed schema-8 contract. No floating-point arithmetic.
use crate::{
    math::*,
    model::{Fraction, Quote},
    Error, Result, I256, U256,
};
use alloy_primitives::U512;

#[derive(Clone, Debug, Default)]
pub(crate) struct Curve {
    pub balances: [U256; 28],
    pub input_dust: U256,
    pub output_dust: U256,
    pub input: usize,
    pub output: usize,
    pub price_in: U256,
    pub price_out: U256,
    pub input_scale: U256,
    pub output_scale: U256,
    pub spread: U256,
    pub lambda: U256,
    pub volume_rate: U256,
    pub input_limit: U256,
    pub variance: I256,
    pub gradient: I256,
    pub posted: u64,
    pub entry_fixed: U256,
    pub entry_slope: U256,
    pub entry_inventory: U256,
    pub exit_fixed: U256,
    pub exit_slope: U256,
    pub exit_inventory: U256,
}

fn quadratic_denominator() -> U256 {
    u(2) * pow10(54)
}

impl Curve {
    pub fn quote(&self, amount: U256, friction: bool) -> Result<Quote> {
        if amount > self.input_limit {
            return Err(Error::LimitExceeded(self.input_limit));
        }
        let n = mul_div(
            amount.saturating_sub(self.input_dust),
            self.price_in,
            self.input_scale,
            false,
        )?;
        let mut q = Quote {
            amount_in: amount,
            amount_out: U256::ZERO,
            notional_usd_wad: n,
            penalty_usd_wad: U256::ZERO,
            volume_cost_usd_wad: U256::ZERO,
            wrapper_cost_usd_wad: I256::ZERO,
            last_posted_block: self.posted,
        };
        if n.is_zero() {
            return Ok(q);
        }
        let mut available = mul_div(n, sub(u(1_000_000), self.spread)?, u(1_000_000), false)?;
        q.penalty_usd_wad = self.penalty(n)?;
        q.volume_cost_usd_wad = mul_div(mul(n, n)?, self.volume_rate, pow10(27), true)?;
        let entry = self.entry_cost(n)?;
        for charge in [q.penalty_usd_wad, q.volume_cost_usd_wad, entry] {
            if charge >= available {
                return Ok(q);
            }
            available -= charge;
        }
        let output_usd = self.output_for_budget(available)?;
        q.wrapper_cost_usd_wad = ia(to_signed(entry)?, self.exit_cost(output_usd)?)?;
        q.amount_out = mul_div(output_usd, self.output_scale, self.price_out, false)?;
        if friction {
            q.amount_out = mul_div(q.amount_out, u(9990), u(10000), false)?;
        }
        q.amount_out = q.amount_out.saturating_sub(self.output_dust);
        Ok(q)
    }

    fn linear_risk(&self, n: U256) -> Result<I256> {
        let value = to_signed(mul_div(
            n,
            self.gradient.unsigned_abs(),
            u(500_000_000_000_000_000),
            false,
        )?)?;
        Ok(if self.gradient.is_negative() {
            -value
        } else {
            value
        })
    }
    fn penalty_delta(&self, n: U256) -> Result<I256> {
        let v = to_signed(n)?;
        ia(
            idiv(im(im(v, v)?, self.variance)?, to_signed(pow10(27))?)?,
            self.linear_risk(n)?,
        )
    }
    fn penalty(&self, n: U256) -> Result<U256> {
        if self.lambda.is_zero() {
            return Ok(U256::ZERO);
        }
        let delta = self.penalty_delta(n)?;
        if delta <= I256::ZERO {
            return Ok(U256::ZERO);
        }
        Ok(mul(delta.into_raw(), self.lambda)? / u(1_000_000))
    }
    fn penalty_slope(&self, n: U256) -> Result<U256> {
        if self.lambda.is_zero() {
            return Ok(U256::ZERO);
        }
        let delta = self.penalty_delta(n)?;
        let slope = idiv(
            im(
                ia(
                    idiv(
                        im(im(signed(2), to_signed(n)?)?, self.variance)?,
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
    fn entry_cost(&self, n: U256) -> Result<U256> {
        if !self.entry_fixed.is_zero() {
            return mul_div(n, self.entry_fixed, pow10(18), true);
        }
        if self.entry_slope.is_zero() {
            return Ok(U256::ZERO);
        }
        mul_div(
            self.entry_slope,
            mul(n, add(mul(u(2), self.entry_inventory)?, n)?)?,
            quadratic_denominator(),
            true,
        )
    }
    fn exit_cost(&self, output: U256) -> Result<I256> {
        if !self.exit_fixed.is_zero() {
            return to_signed(mul_div(output, self.exit_fixed, pow10(18), true)?);
        }
        if self.exit_slope.is_zero() {
            return Ok(I256::ZERO);
        }
        let z = output.min(self.exit_inventory);
        let value = mul_div(
            self.exit_slope,
            mul(z, sub(mul(u(2), self.exit_inventory)?, z)?)?,
            quadratic_denominator(),
            false,
        )?;
        Ok(-to_signed(value)?)
    }
    fn output_for_budget(&self, budget: U256) -> Result<U256> {
        if !self.exit_fixed.is_zero() {
            return mul_div(budget, pow10(18), add(pow10(18), self.exit_fixed)?, false);
        }
        let k = self.exit_slope;
        if k.is_zero() {
            return Ok(budget);
        }
        let inventory = self.exit_inventory;
        if inventory > (pow10(54) - U256::ONE) / k {
            return Err(Error::Unavailable("nonpositive wrapper value"));
        }
        let credit = mul_div(
            k,
            mul(inventory, inventory)?,
            quadratic_denominator(),
            false,
        )?;
        if budget >= sub(inventory, credit)? {
            return add(budget, credit);
        }
        let b = sub(pow10(54), mul(k, inventory)?)?;
        let k_budget = mul(k, budget)?;
        let discriminant = U512::from(b) * U512::from(b)
            + U512::from(quadratic_denominator()) * U512::from(k_budget);
        let root = sqrt_ceil(discriminant)?;
        let output = mul_div(quadratic_denominator(), budget, add(b, root)?, false)?;
        let next = add(output, U256::ONE)?;
        let next_credit = mul_div(
            k,
            mul(next, sub(mul(u(2), inventory)?, next)?)?,
            quadratic_denominator(),
            false,
        )?;
        Ok(if sub(next, next_credit)? <= budget {
            next
        } else {
            output
        })
    }
    pub fn remaining_slope(&self, amount: U256) -> Result<U256> {
        let n = mul_div(
            amount.saturating_sub(self.input_dust),
            self.price_in,
            self.input_scale,
            false,
        )?;
        let base = mul_div(
            sub(u(1_000_000), self.spread)?,
            pow10(36),
            u(1_000_000),
            false,
        )?;
        let mut cost = add(
            mul(self.penalty_slope(n)?, pow10(18))?,
            mul(self.entry_fixed, pow10(18))?,
        )?;
        cost = add(
            cost,
            mul(mul(mul(self.volume_rate, n)?, u(2))?, u(1_000_000_000))?,
        )?;
        cost = add(
            cost,
            mul_div(
                self.entry_slope,
                add(self.entry_inventory, n)?,
                pow10(18),
                true,
            )?,
        )?;
        Ok(if cost < base { base - cost } else { U256::ZERO })
    }
    pub fn marginal(&self, amount: U256) -> Result<Fraction> {
        if amount > self.input_limit {
            return Err(Error::LimitExceeded(self.input_limit));
        }
        let slope = self.remaining_slope(amount)?;
        if slope.is_zero() {
            return Ok(Fraction {
                numerator: U256::ZERO,
                denominator: U256::ONE,
            });
        }
        let q = self.quote(amount, false)?;
        let output_usd = mul_div(q.amount_out, self.price_out, self.output_scale, false)?;
        let mut denominator = add(pow10(36), mul(self.exit_fixed, pow10(18))?)?;
        if !self.exit_slope.is_zero() && output_usd < self.exit_inventory {
            let refund = mul_div(
                self.exit_slope,
                self.exit_inventory - output_usd,
                pow10(18),
                false,
            )?;
            if refund >= pow10(36) {
                return Err(Error::Unavailable("nonpositive wrapper value"));
            }
            denominator = sub(denominator, refund)?;
        }
        let fair = mul_div(self.price_in, pow10(36), self.price_out, false)?;
        let numerator = mul_div(fair, slope, denominator, false)?;
        denominator = if numerator.is_zero() {
            U256::ONE
        } else if self.input_scale >= self.output_scale {
            mul(pow10(36), self.input_scale / self.output_scale)?
        } else {
            pow10(36) / (self.output_scale / self.input_scale)
        };
        Ok(Fraction {
            numerator,
            denominator,
        })
    }
    pub fn solve_output(&self, target: U256, friction: bool) -> Result<Quote> {
        if target.is_zero() {
            return self.quote(U256::ZERO, friction);
        }
        let target_usd = mul_div(
            add(target, self.output_dust)?,
            self.price_out,
            self.output_scale,
            true,
        )?;
        let mut high = add(
            mul_div(target_usd, self.input_scale, self.price_in, true)?,
            self.input_dust,
        )?
        .max(U256::ONE)
        .min(self.input_limit);
        let mut low = U256::ZERO;
        let mut q = self.quote(high, friction)?;
        while q.amount_out < target {
            if self.remaining_slope(high)?.is_zero() {
                let (mut a, mut b) = (low, high);
                while b - a > U256::ONE {
                    let mid = a + (b - a) / u(2);
                    if self.remaining_slope(mid)?.is_zero() {
                        b = mid;
                    } else {
                        a = mid;
                    }
                }
                let (qa, qb) = (self.quote(a, friction)?, self.quote(b, friction)?);
                (high, q) = if qb.amount_out > qa.amount_out {
                    (b, qb)
                } else {
                    (a, qa)
                };
                if q.amount_out < target {
                    return Err(Error::Unavailable("requested output unavailable"));
                }
                break;
            }
            if high == self.input_limit {
                return Err(Error::Unavailable("requested output unavailable"));
            }
            low = high;
            high = if high > self.input_limit / u(2) {
                self.input_limit
            } else {
                high * u(2)
            };
            q = self.quote(high, friction)?;
        }
        while high - low > U256::ONE {
            let mid = low + (high - low) / u(2);
            let candidate = self.quote(mid, friction)?;
            if candidate.amount_out >= target {
                high = mid;
                q = candidate;
            } else {
                low = mid;
            }
        }
        Ok(q)
    }
}

fn sqrt_ceil(value: U512) -> Result<U256> {
    if value.is_zero() {
        return Ok(U256::ZERO);
    }
    let mut x = U512::ONE << value.bit_len().div_ceil(2);
    loop {
        let next = (x + value / x) >> 1;
        if next >= x {
            break;
        }
        x = next;
    }
    if x * x != value {
        x += U512::ONE;
    }
    let limbs = x.as_limbs();
    if limbs[4..].iter().any(|v| *v != 0) {
        return Err(Error::Arithmetic);
    }
    Ok(U256::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3]]))
}
