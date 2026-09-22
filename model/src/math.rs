//! Solidity-width operations. mulDiv alone uses a 512-bit intermediate.
use crate::{Error, Result, I256, U256};
use alloy_primitives::U512;

pub fn u(value: u64) -> U256 {
    U256::from(value)
}
pub fn signed(value: i128) -> I256 {
    I256::try_from(value).expect("i128 fits int256")
}
pub fn to_signed(value: U256) -> Result<I256> {
    if value.bit(255) {
        return Err(Error::Arithmetic);
    }
    Ok(I256::from_raw(value))
}
pub fn pow10(exponent: u8) -> U256 {
    POWERS_OF_TEN[exponent as usize]
}
// Match U256::pow's wrapping semantics for the entire u8 domain. Quote math
// uses exponents 0..36; computing these constants never belongs on the hot path.
const POWERS_OF_TEN: [U256; 256] = {
    let mut powers = [U256::ZERO; 256];
    let mut limbs = [1u64, 0, 0, 0];
    let mut exponent = 0;
    while exponent < powers.len() {
        powers[exponent] = U256::from_limbs(limbs);
        let mut carry = 0u128;
        let mut i = 0;
        while i < limbs.len() {
            let product = limbs[i] as u128 * 10 + carry;
            limbs[i] = product as u64;
            carry = product >> 64;
            i += 1;
        }
        exponent += 1;
    }
    powers
};
pub fn mask(width: usize) -> U256 {
    if width == 256 {
        U256::MAX
    } else {
        (U256::ONE << width) - U256::ONE
    }
}
pub fn add(a: U256, b: U256) -> Result<U256> {
    a.checked_add(b).ok_or(Error::Arithmetic)
}
pub fn sub(a: U256, b: U256) -> Result<U256> {
    a.checked_sub(b).ok_or(Error::Arithmetic)
}
pub fn mul(a: U256, b: U256) -> Result<U256> {
    a.checked_mul(b).ok_or(Error::Arithmetic)
}
pub fn ia(a: I256, b: I256) -> Result<I256> {
    a.checked_add(b).ok_or(Error::Arithmetic)
}
pub fn isub(a: I256, b: I256) -> Result<I256> {
    a.checked_sub(b).ok_or(Error::Arithmetic)
}
pub fn im(a: I256, b: I256) -> Result<I256> {
    a.checked_mul(b).ok_or(Error::Arithmetic)
}
pub fn idiv(a: I256, b: I256) -> Result<I256> {
    a.checked_div(b).ok_or(Error::Arithmetic)
}

pub fn mul_div(a: U256, b: U256, denominator: U256, ceil: bool) -> Result<U256> {
    if denominator.is_zero() {
        return Err(Error::Arithmetic);
    }
    let product = U512::from(a) * U512::from(b);
    let denominator = U512::from(denominator);
    let (mut quotient, remainder) = product.div_rem(denominator);
    if ceil && remainder != U512::ZERO {
        quotient += U512::ONE;
    }
    let limbs = quotient.as_limbs();
    if limbs[4..].iter().any(|x| *x != 0) {
        return Err(Error::Arithmetic);
    }
    Ok(U256::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3]]))
}

pub fn max_floored_input(maximum: U256, divisor: U256, multiplier: U256) -> Result<U256> {
    if maximum == U256::MAX {
        if multiplier <= divisor {
            return Ok(U256::MAX);
        }
        let quotient = mul_div(U256::MAX, divisor, multiplier, false)?;
        let remainder = (U512::from(U256::MAX) * U512::from(divisor)) % U512::from(multiplier);
        return if remainder >= U512::from(multiplier - divisor + U256::ONE) {
            add(quotient, U256::ONE)
        } else {
            Ok(quotient)
        };
    }
    if multiplier <= divisor && maximum >= mul_div(U256::MAX, multiplier, divisor, false)? {
        return Ok(U256::MAX);
    }
    sub(
        mul_div(maximum + U256::ONE, divisor, multiplier, true)?,
        U256::ONE,
    )
}
