//! Issuer share state for the deployed contract's rebasing-token catalog.
use crate::{Address, U256};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareBalance {
    pub shares: U256,
    pub multiplier: U256,
}
pub type ShareBalances = BTreeMap<Address, ShareBalance>;
pub type Multipliers = BTreeMap<Address, U256>;

/// Recognizes the deployed contract's nine compiled unwrapped xStock addresses.
/// Active listings are discovered separately from pool state. Extending the
/// contract's xStock catalog requires a matching SDK release; registry discovery
/// alone does not recognize an arbitrary new issuer token.
pub fn is_xstock(token: Address) -> bool {
    const TOKENS: [Address; 9] = [
        alloy_primitives::address!("90a2a4c76b5d8c0bc892a69ea28aa775a8f2dd48"), // SPYx
        alloy_primitives::address!("a753a7395cae905cd615da0b82a53e0560f250af"), // QQQx
        alloy_primitives::address!("8d512a03600981b8e86556fd5c0fbdd726fe1821"), // SMHx
        alloy_primitives::address!("299c95eb52bfd5bc98d924bce17197b64864589b"), // EWYx
        alloy_primitives::address!("c845b2894dbddd03858fd2d643b4ef725fe0849d"), // NVDAx
        alloy_primitives::address!("8ad3c73f833d3f9a523ab01476625f269aeb7cf0"), // TSLAx
        alloy_primitives::address!("68fa48b1c2fe52b3d776e1953e0e782b5044ce28"), // SPCXx
        alloy_primitives::address!("f6a873bae4ba1b304e45df52a4b7d176e1c6a8c4"), // MUx
        alloy_primitives::address!("58100046a4afcd4ee4fadbd4244f3f895a341c56"), // SKHYx
    ];
    TOKENS.contains(&token)
}
