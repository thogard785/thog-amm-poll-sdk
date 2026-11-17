//! Checked 256-bit arithmetic with the contract's rounding.
//!
//! Solidity reverts on overflow and division by zero; these helpers return
//! [`Error::Arithmetic`] in the same cases, so a quote can never succeed here
//! where the contract would revert.
use crate::{Error, Result, I256, U256};
use alloy_primitives::U512;

pub(crate) const fn uint(value: u64) -> U256 {
    U256::from_limbs([value, 0, 0, 0])
}

/// 10^exponent for every power that fits 256 bits.
pub(crate) fn pow10(exponent: u8) -> U256 {
    POWERS_OF_TEN[exponent as usize]
}

const POWERS_OF_TEN: [U256; 78] = {
    let mut powers = [U256::ZERO; 78];
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

/// The largest value of a 128-bit field.
pub(crate) const U128_MAX: U256 = U256::from_limbs([u64::MAX, u64::MAX, 0, 0]);

pub(crate) fn add(a: U256, b: U256) -> Result<U256> {
    a.checked_add(b).ok_or(Error::Arithmetic)
}
pub(crate) fn sub(a: U256, b: U256) -> Result<U256> {
    a.checked_sub(b).ok_or(Error::Arithmetic)
}
pub(crate) fn mul(a: U256, b: U256) -> Result<U256> {
    a.checked_mul(b).ok_or(Error::Arithmetic)
}

/// `a * b / denominator` with a 512-bit product, rounded down.
pub(crate) fn mul_div(a: U256, b: U256, denominator: U256) -> Result<U256> {
    mul_div_rounding(a, b, denominator, false)
}
/// `a * b / denominator` with a 512-bit product, rounded up.
pub(crate) fn mul_div_up(a: U256, b: U256, denominator: U256) -> Result<U256> {
    mul_div_rounding(a, b, denominator, true)
}
fn mul_div_rounding(a: U256, b: U256, denominator: U256, up: bool) -> Result<U256> {
    if denominator.is_zero() {
        return Err(Error::Arithmetic);
    }
    let (mut quotient, remainder) =
        (U512::from(a) * U512::from(b)).div_rem(U512::from(denominator));
    if up && !remainder.is_zero() {
        quotient += U512::ONE;
    }
    narrow(quotient)
}

/// The 256-bit value of `wide`, or an error when it does not fit.
pub(crate) fn narrow(wide: U512) -> Result<U256> {
    let limbs = wide.as_limbs();
    if limbs[4..].iter().any(|limb| *limb != 0) {
        return Err(Error::Arithmetic);
    }
    Ok(U256::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3]]))
}

/// Smallest integer at or above the square root of `value`.
pub(crate) fn sqrt_up(value: U512) -> Result<U256> {
    if value.is_zero() {
        return Ok(U256::ZERO);
    }
    let mut root = U512::ONE << value.bit_len().div_ceil(2);
    loop {
        let next = (root + value / root) >> 1;
        if next >= root {
            break;
        }
        root = next;
    }
    if root * root != value {
        root += U512::ONE;
    }
    narrow(root)
}

// Signed values follow Solidity's int256: two's complement, division toward zero.

pub(crate) fn signed(value: i64) -> I256 {
    I256::try_from(value).expect("i64 fits int256")
}
/// Reinterpret an unsigned value as signed; fails at or above 2^255.
pub(crate) fn to_signed(value: U256) -> Result<I256> {
    if value.bit(255) {
        return Err(Error::Arithmetic);
    }
    Ok(I256::from_raw(value))
}
pub(crate) fn signed_add(a: I256, b: I256) -> Result<I256> {
    a.checked_add(b).ok_or(Error::Arithmetic)
}
pub(crate) fn signed_mul(a: I256, b: I256) -> Result<I256> {
    a.checked_mul(b).ok_or(Error::Arithmetic)
}
pub(crate) fn signed_div(a: I256, b: I256) -> Result<I256> {
    a.checked_div(b).ok_or(Error::Arithmetic)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powers_of_ten_are_exact() {
        for exponent in 0..78u8 {
            assert_eq!(pow10(exponent), U256::from(10).pow(U256::from(exponent)));
        }
    }

    #[test]
    fn mul_div_keeps_wide_products_rounding_and_overflow() {
        let mut random = 0xa102_fedc_8830_a781u64;
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
        let edges = [
            U256::ZERO,
            U256::ONE,
            uint(3),
            U256::ONE << 128,
            U256::MAX - U256::ONE,
            U256::MAX,
        ];
        let mut cases = Vec::new();
        for a in edges {
            for b in edges {
                for d in edges {
                    cases.push((a, b, d));
                }
            }
        }
        for _ in 0..2048 {
            cases.push((word(), word(), word()));
        }
        for (a, b, d) in cases {
            for up in [false, true] {
                let actual = mul_div_rounding(a, b, d, up);
                if d.is_zero() {
                    assert!(actual.is_err());
                    continue;
                }
                let product = U512::from(a) * U512::from(b);
                let expected = product / U512::from(d)
                    + U512::from(u8::from(up && !(product % U512::from(d)).is_zero()));
                match narrow(expected) {
                    Ok(expected) => assert_eq!(actual.unwrap(), expected),
                    Err(_) => assert!(actual.is_err()),
                }
            }
        }
    }

    #[test]
    fn sqrt_up_is_the_ceiling() {
        for value in [1u64, 2, 3, 4, 15, 16, 17, u64::MAX] {
            let root = sqrt_up(U512::from(value)).unwrap();
            assert!(U512::from(root) * U512::from(root) >= U512::from(value));
            let below = root - U256::ONE;
            assert!(U512::from(below) * U512::from(below) < U512::from(value));
        }
    }
}
