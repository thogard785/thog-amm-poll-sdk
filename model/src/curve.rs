//! The pricing curve of one directed pair, in the contract's integer arithmetic.
//!
//! Each step keeps the contract's operation order and rounding, so results are
//! equal to the last raw unit. The formulas, with the same names, are in
//! <https://github.com/thogard785/thog-amm-integration-guide/blob/main/REFERENCE.md#exact-input-quote>
//!
//! USD amounts are scaled by 1e18. Spreads and the surcharge are in units of
//! 0.01 bp, so 1,000,000 is 100%.
use crate::{math::*, Error, Rejection, Result, I256, U256};
use alloy_primitives::U512;

const SPREAD_SCALE: U256 = uint(1_000_000);

/// The cost of moving one token into or out of the pool, in curve form.
#[derive(Clone, Debug, Default)]
pub(crate) struct CostCurve {
    /// Fixed kind: rate scaled by 1e18. Zero otherwise.
    pub fixed_rate: U256,
    /// Inventory-based kind: cost per USD of holding, scaled by 1e36. Zero otherwise.
    pub slope: U256,
    /// Inventory-based kind: USD value of the pool's holding of the token.
    pub inventory_usd: U256,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Curve {
    /// Lower price of the input token and upper price of the output token.
    pub price_in: U256,
    pub price_out: U256,
    /// 10^decimals of each token.
    pub scale_in: U256,
    pub scale_out: U256,
    /// Rounding allowance of an xStock transfer; zero for ordinary tokens.
    pub dust_in: U256,
    pub dust_out: U256,
    /// Every spread that applies to the pair, including price age.
    pub spread: U256,
    pub risk_aversion: U256,
    pub size_rate: U256,
    /// Risk added per squared USD of trade, and per USD of trade.
    pub variance: I256,
    pub gradient: I256,
    pub cost_in: CostCurve,
    pub cost_out: CostCurve,
    /// Largest input the arithmetic accepts.
    pub max_amount_in: U256,
}

fn two_e54() -> U256 {
    uint(2) * pow10(54)
}

/// The execution surcharge in 0.01 bp for a transaction's gas price.
///
/// `prices_updated_this_block` is true when the pool's prices were updated in
/// the block the swap executes in.
pub(crate) fn surcharge(
    gas_price: U256,
    base_fee: U256,
    auction_handler_warm: bool,
    prices_updated_this_block: bool,
) -> u32 {
    let mut surcharge = 0;
    if auction_handler_warm {
        surcharge = 1200;
    } else if gas_price > base_fee {
        // Compare differences so thresholds of 1.5x, 2x and 2.5x the base fee
        // stay exact for odd base fees.
        let priority_fee = gas_price - base_fee;
        if priority_fee > base_fee {
            surcharge = if priority_fee - base_fee > base_fee / uint(2) {
                1200
            } else {
                800
            };
        } else if priority_fee > base_fee / uint(2) {
            surcharge = 600;
        }
    }
    if prices_updated_this_block && surcharge != 0 {
        surcharge = surcharge / 4 - 100;
    }
    surcharge
}

pub(crate) fn apply_surcharge(amount: U256, surcharge: u32) -> Result<U256> {
    mul_div(
        amount,
        SPREAD_SCALE - uint(u64::from(surcharge)),
        SPREAD_SCALE,
    )
}

impl Curve {
    /// Output for `amount_in`, with the surcharge applied before the output's
    /// rounding allowance is removed. Pass zero for no surcharge.
    pub fn evaluate(&self, amount_in: U256, surcharge: u32) -> Result<U256> {
        if amount_in > self.max_amount_in {
            return Err(Rejection::AmountTooLarge {
                limit: self.max_amount_in,
            }
            .into());
        }
        let notional = self.notional(amount_in)?;
        if notional.is_zero() {
            return Ok(U256::ZERO);
        }
        let mut budget = mul_div(notional, sub(SPREAD_SCALE, self.spread)?, SPREAD_SCALE)?;
        for charge in [
            self.risk_charge(notional)?,
            self.size_charge(notional)?,
            self.entry_cost(notional)?,
        ] {
            if charge >= budget {
                return Ok(U256::ZERO);
            }
            budget -= charge;
        }
        let output_usd = self.output_for_budget(budget)?;
        let mut amount_out = mul_div(output_usd, self.scale_out, self.price_out)?;
        if surcharge != 0 {
            amount_out = apply_surcharge(amount_out, surcharge)?;
        }
        Ok(amount_out.saturating_sub(self.dust_out))
    }

    /// USD value of the input the pool is certain to receive.
    fn notional(&self, amount_in: U256) -> Result<U256> {
        mul_div(
            amount_in.saturating_sub(self.dust_in),
            self.price_in,
            self.scale_in,
        )
    }

    fn linear_risk(&self, notional: U256) -> Result<I256> {
        let magnitude = to_signed(mul_div(
            notional,
            self.gradient.unsigned_abs(),
            uint(5) * pow10(17),
        )?)?;
        Ok(if self.gradient.is_negative() {
            -magnitude
        } else {
            magnitude
        })
    }

    /// Change in portfolio risk caused by the trade; negative when it reduces risk.
    fn risk_change(&self, notional: U256) -> Result<I256> {
        let n = to_signed(notional)?;
        signed_add(
            signed_div(
                signed_mul(signed_mul(n, n)?, self.variance)?,
                to_signed(pow10(27))?,
            )?,
            self.linear_risk(notional)?,
        )
    }

    /// Charged only when the trade adds risk.
    fn risk_charge(&self, notional: U256) -> Result<U256> {
        if self.risk_aversion.is_zero() {
            return Ok(U256::ZERO);
        }
        let change = self.risk_change(notional)?;
        if change <= I256::ZERO {
            return Ok(U256::ZERO);
        }
        Ok(mul(change.into_raw(), self.risk_aversion)? / SPREAD_SCALE)
    }

    fn risk_charge_slope(&self, notional: U256) -> Result<U256> {
        if self.risk_aversion.is_zero() {
            return Ok(U256::ZERO);
        }
        let change = self.risk_change(notional)?;
        let slope = signed_div(
            signed_mul(
                signed_add(
                    signed_div(
                        signed_mul(signed_mul(signed(2), to_signed(notional)?)?, self.variance)?,
                        signed(1_000_000_000),
                    )?,
                    signed_mul(signed(2), self.gradient)?,
                )?,
                to_signed(self.risk_aversion)?,
            )?,
            signed(1_000_000),
        )?;
        Ok(if change >= I256::ZERO && slope > I256::ZERO {
            slope.into_raw()
        } else {
            U256::ZERO
        })
    }

    /// Grows with the square of the trade's USD value; charged on every swap.
    fn size_charge(&self, notional: U256) -> Result<U256> {
        mul_div_up(mul(notional, notional)?, self.size_rate, pow10(27))
    }

    fn entry_cost(&self, notional: U256) -> Result<U256> {
        let cost = &self.cost_in;
        if !cost.fixed_rate.is_zero() {
            return mul_div_up(notional, cost.fixed_rate, pow10(18));
        }
        if cost.slope.is_zero() {
            return Ok(U256::ZERO);
        }
        mul_div_up(
            cost.slope,
            mul(notional, add(mul(uint(2), cost.inventory_usd)?, notional)?)?,
            two_e54(),
        )
    }

    /// Largest USD output whose own cost or credit leaves it within `budget`.
    fn output_for_budget(&self, budget: U256) -> Result<U256> {
        let cost = &self.cost_out;
        if !cost.fixed_rate.is_zero() {
            return mul_div(budget, pow10(18), add(pow10(18), cost.fixed_rate)?);
        }
        let slope = cost.slope;
        if slope.is_zero() {
            return Ok(budget);
        }
        let inventory = cost.inventory_usd;
        if inventory > (pow10(54) - U256::ONE) / slope {
            return Err(Rejection::TokenCost.into());
        }
        // Paying out the whole holding earns the full credit.
        let full_credit = mul_div(slope, mul(inventory, inventory)?, two_e54())?;
        if budget >= sub(inventory, full_credit)? {
            return add(budget, full_credit);
        }
        // Otherwise solve slope*y^2 + 2*b*y - 2e54*budget = 0 for y.
        let b = sub(pow10(54), mul(slope, inventory)?)?;
        let discriminant =
            U512::from(b) * U512::from(b) + U512::from(two_e54()) * U512::from(mul(slope, budget)?);
        let output = mul_div(two_e54(), budget, add(b, sqrt_up(discriminant)?)?)?;
        // The rounded root can be one short; take the next integer if it fits.
        let next = add(output, U256::ONE)?;
        let credit = mul_div(
            slope,
            mul(next, sub(mul(uint(2), inventory)?, next)?)?,
            two_e54(),
        )?;
        Ok(if sub(next, credit)? <= budget {
            next
        } else {
            output
        })
    }

    /// USD the next unit of input adds to the budget, scaled by 1e36.
    fn marginal_budget(&self, amount_in: U256) -> Result<U256> {
        let notional = self.notional(amount_in)?;
        let base = mul_div(sub(SPREAD_SCALE, self.spread)?, pow10(36), SPREAD_SCALE)?;
        let mut cost = add(
            mul(self.risk_charge_slope(notional)?, pow10(18))?,
            mul(self.cost_in.fixed_rate, pow10(18))?,
        )?;
        cost = add(
            cost,
            mul(mul(self.size_rate, notional)?, uint(2_000_000_000))?,
        )?;
        cost = add(
            cost,
            mul_div_up(
                self.cost_in.slope,
                add(self.cost_in.inventory_usd, notional)?,
                pow10(18),
            )?,
        )?;
        Ok(base.saturating_sub(cost))
    }

    /// Output per unit of additional input after trading `amount_in`, as a
    /// fraction of raw units: `(numerator, denominator)`.
    pub fn marginal_price(&self, amount_in: U256) -> Result<(U256, U256)> {
        if amount_in > self.max_amount_in {
            return Err(Rejection::AmountTooLarge {
                limit: self.max_amount_in,
            }
            .into());
        }
        let budget_slope = self.marginal_budget(amount_in)?;
        if budget_slope.is_zero() {
            return Ok((U256::ZERO, U256::ONE));
        }
        let amount_out = self.evaluate(amount_in, 0)?;
        let output_usd = mul_div(amount_out, self.price_out, self.scale_out)?;
        let mut output_cost = add(pow10(36), mul(self.cost_out.fixed_rate, pow10(18))?)?;
        if !self.cost_out.slope.is_zero() && output_usd < self.cost_out.inventory_usd {
            let credit = mul_div(
                self.cost_out.slope,
                self.cost_out.inventory_usd - output_usd,
                pow10(18),
            )?;
            if credit >= pow10(36) {
                return Err(Rejection::TokenCost.into());
            }
            output_cost -= credit;
        }
        let price_ratio = mul_div(self.price_in, pow10(36), self.price_out)?;
        let numerator = mul_div(price_ratio, budget_slope, output_cost)?;
        if numerator.is_zero() {
            return Ok((U256::ZERO, U256::ONE));
        }
        let denominator = if self.scale_in >= self.scale_out {
            mul(pow10(36), self.scale_in / self.scale_out)?
        } else {
            pow10(36) / (self.scale_out / self.scale_in)
        };
        Ok((numerator, denominator))
    }

    /// Smallest input whose output, after `surcharge`, reaches `amount_out`.
    pub fn amount_in_for(&self, amount_out: U256, surcharge: u32) -> Result<U256> {
        if amount_out.is_zero() {
            return Ok(U256::ZERO);
        }
        let unavailable = || Error::from(Rejection::OutputUnavailable);
        // Start from the input worth the output at the two posted prices.
        let target_usd = mul_div_up(
            add(amount_out, self.dust_out)?,
            self.price_out,
            self.scale_out,
        )?;
        let mut high = add(
            mul_div_up(target_usd, self.scale_in, self.price_in)?,
            self.dust_in,
        )?
        .max(U256::ONE)
        .min(self.max_amount_in);
        let mut low = U256::ZERO;
        // Double the input until it is enough.
        while self.evaluate(high, surcharge)? < amount_out {
            if self.marginal_budget(high)?.is_zero() {
                // More input no longer buys more output: the most the curve
                // pays lies where the marginal budget reaches zero.
                let (mut rising, mut flat) = (low, high);
                while flat - rising > U256::ONE {
                    let middle = rising + (flat - rising) / uint(2);
                    if self.marginal_budget(middle)?.is_zero() {
                        flat = middle;
                    } else {
                        rising = middle;
                    }
                }
                let at_rising = self.evaluate(rising, surcharge)?;
                let at_flat = self.evaluate(flat, surcharge)?;
                let (peak, peak_out) = if at_flat > at_rising {
                    (flat, at_flat)
                } else {
                    (rising, at_rising)
                };
                if peak_out < amount_out {
                    return Err(unavailable());
                }
                high = peak;
                break;
            }
            if high == self.max_amount_in {
                return Err(unavailable());
            }
            low = high;
            high = if high > self.max_amount_in / uint(2) {
                self.max_amount_in
            } else {
                high * uint(2)
            };
        }
        // Then bisect for the smallest input that is enough.
        while high - low > U256::ONE {
            let middle = low + (high - low) / uint(2);
            if self.evaluate(middle, surcharge)? >= amount_out {
                high = middle;
            } else {
                low = middle;
            }
        }
        Ok(high)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surcharge_tiers_are_strict_and_do_not_stack() {
        for (base, gas, expected) in [
            (100, 0, 0),
            (100, 150, 0),
            (100, 151, 600),
            (100, 200, 600),
            (100, 201, 800),
            (100, 250, 800),
            (100, 251, 1200),
            (3, 4, 0),
            (3, 5, 600),
            (3, 6, 600),
            (3, 7, 800),
            (3, 8, 1200),
            (0, 0, 0),
            (0, 1, 1200),
        ] {
            for warm in [false, true] {
                for same_block in [false, true] {
                    let tier = if warm { 1200 } else { expected };
                    let tier = if same_block && tier != 0 {
                        tier / 4 - 100
                    } else {
                        tier
                    };
                    assert_eq!(surcharge(uint(gas), uint(base), warm, same_block), tier);
                }
            }
        }
    }

    #[test]
    fn surcharge_thresholds_hold_for_any_fee() {
        let mut random = 0x1234_5678_9abc_def0u64;
        let mut word = || {
            let mut limbs = [0; 4];
            for limb in &mut limbs {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                *limb = random;
            }
            U256::from_limbs(limbs)
        };
        let mut cases = vec![
            (U256::MAX, U256::MAX),
            (U256::MAX, U256::MAX / uint(2)),
            (U256::MAX, U256::MAX / uint(3)),
        ];
        for _ in 0..1024 {
            cases.push((word(), word()));
        }
        for (gas, base) in cases {
            let (g, b) = (U512::from(gas), U512::from(base));
            let expected = if g * U512::from(2) > b * U512::from(5) {
                1200
            } else if g > b * U512::from(2) {
                800
            } else if g * U512::from(2) > b * U512::from(3) {
                600
            } else {
                0
            };
            assert_eq!(surcharge(gas, base, false, false), expected);
        }
    }
}
