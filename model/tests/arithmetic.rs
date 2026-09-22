use alloy_primitives::U512;
use thogamm_model::{
    math::{mul_div, pow10},
    U256,
};

#[test]
fn powers_preserve_the_full_u8_wrapping_domain() {
    for exponent in 0..=255u8 {
        assert_eq!(pow10(exponent), U256::from(10).pow(U256::from(exponent)));
    }
}

#[test]
fn mul_div_preserves_wide_products_rounding_and_overflow() {
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
    let boundaries = [
        U256::ZERO,
        U256::ONE,
        U256::from(3),
        U256::ONE << 128,
        U256::MAX - U256::ONE,
        U256::MAX,
    ];
    let mut cases = Vec::new();
    for a in boundaries {
        for b in boundaries {
            for d in boundaries {
                cases.push((a, b, d));
            }
        }
    }
    for _ in 0..2048 {
        cases.push((word(), word(), word()));
    }
    for (a, b, d) in cases {
        for ceil in [false, true] {
            let actual = mul_div(a, b, d, ceil);
            if d.is_zero() {
                assert!(actual.is_err());
                continue;
            }
            let product = U512::from(a) * U512::from(b);
            let denominator = U512::from(d);
            let expected = product / denominator
                + U512::from(u8::from(ceil && product % denominator != U512::ZERO));
            if expected > U512::from(U256::MAX) {
                assert!(actual.is_err());
            } else {
                assert_eq!(U512::from(actual.unwrap()), expected);
            }
        }
    }
}
