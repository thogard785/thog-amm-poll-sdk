# Snapshot transport


```rust
use state_poll_sdk::{Config, StatePollSdk};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut sdk = StatePollSdk::connect(
        std::env::var("THOGAMM_HTTP_RPC")?,
        std::env::var("THOGAMM_PROXY")?.parse()?,
        Config::default(),
    ).await?;
    loop {
        let model = sdk.next_update().await?;
        println!("block={} highest_token_index={}", model.state().block.number, model.max_index());
    }
}
```

Every `refresh()` sends exactly one `eth_call` at `latest`, including unchanged
blocks and errors. `getPoolData(0, 64)` resolves 64 to the current token count and
returns the complete registry, balances, words, block number, parent hash and base
fee. Newly listed tokens require no discovery call. `next_update()` waits 500 ms
by default, performs exactly one refresh and returns even when the model is
unchanged. Neither public update method hides a wait-for-change polling loop.

The state call sets `gasPrice: "0x1"` (one wei). On Monad, leaving the fee fields
unset can make the EVM expose `block.basefee = 0` during `eth_call`; a nonzero
simulation gas price preserves the selected block's base fee. This is a view
call and spends no gas. It does not set the effective gas price of a future
swap. Custom `ChainReader` implementations must preserve this behavior too;
no header or fee-estimation request is needed.

A validated response replaces the entire image atomically. A response from a lower
block returns `ProviderBehind` and retains the old image; a changed same-height
response replaces it. An unchanged model returns `false`, even if an otherwise
identical block was replaced: `eth_call` cannot expose the current block's hash.
`PoolState.block.hash` is consequently `None` for this client, never a fabricated
hash. Number, parent hash and base fee come from the same contract execution as
all returned state. No second request checks the header or canonicality. Applications
requiring hash provenance can use the shared `snapshot(reader, proxy,
CallBlock::Hash(hash))` helper with a hash they already have; that is also one call.

The underlying contract still supports pinned partial pages for other consumers.
The polling SDK deliberately always requests the complete image in one call.

Aggregators can own both scheduling and transport. `snapshot(reader, proxy,
CallBlock::Number(n))` or `CallBlock::Hash(hash)` performs one pinned state call.
Alternatively, `pool_data_params(proxy, block)` builds that call's JSON-RPC
parameters, and `decode_pool_data(proxy, &returned_bytes, block)` builds the model
offline from the response. The latter creates no client, timer, subscription or
background task. It rejects partial registries and checks a requested block number.
These functions are re-exported directly by `state-poll-sdk`.

## Aggregator-owned RPC infrastructure


Choose one state reader:

1. Let `StatePollSdk` own the HTTP client. `refresh()` immediately issues exactly
   one `eth_call`; `next_update()` waits the configured interval before that one
   call. Neither method loops until state changes or performs auxiliary reads.
2. Use your existing RPC infrastructure and scheduler. `pool_data_params()`
   returns the parameters for **one** `eth_call`. Pass the returned ABI bytes to
   `decode_pool_data()`; it validates and constructs the model with no I/O.
3. Implement `ChainReader` and call `snapshot(reader, proxy, block)`. This also
   issues exactly one state call, at your supplied number/hash/tag.

The second option is useful when an aggregator already manages RPC connections,
block admission and immutable state snapshots:

```rust
use state_poll_sdk::{
    decode_pool_data, pool_data_params, CallBlock, ExecutionContext,
};

// Select a block identity already known to the aggregator. Latest is also valid.
let block = CallBlock::Hash(admitted_block_hash);
let params = pool_data_params(proxy, block);
// Send exactly one JSON-RPC request using your own client:
// { "jsonrpc":"2.0", "id":1, "method":"eth_call", "params": params }
// Decode its result hex string into returned_bytes.
let model = decode_pool_data(proxy, &returned_bytes, block)?;

let execution = ExecutionContext {
    gas_price: effective_transaction_gas_price_wei,
    fast_lane_hot: auction_handler_already_warm,
};
let pair = model.prepare(sell_token, buy_token)?;
for amount in raw_sell_amounts {
    let result = pair.quote_execution_exact_input(amount, &execution);
    // Handle availability per amount; a rejected amount need not reject the pair.
}
```

The call always covers indices `[0,64)`, with 64 resolving to the current token
count. Do not precede it with `maxIndex()`, `getTokens()`, token `decimals()` or
`balanceOf()` calls: the response already includes the entire listed registry,
decimals, actual balances, packed quote state, all portfolio risk dependencies,
and quote block number/base fee/parent hash. Newly listed tokens arrive in the
next response without a discovery round trip or SDK change.

Keep the `gasPrice: "0x1"` field produced by `pool_data_params()`. Monad can set
the simulated BASEFEE to zero when `eth_call` omits fee fields. One wei preserves
the real block BASEFEE for this **state read**, without fetching a header or fee
suggestion. It is not the gas price of a trade. An `eth_call` spends no funds.
Custom readers must retain a nonzero simulation gas price as well.

The RPC block selector and response must refer to the same block. Number-pinned
responses are checked against their returned block number. A hash-pinned call
uses EIP-1898 `requireCanonical: true`; the hash attached to the local image is
the caller's requested hash. Plain latest/number calls record `hash: None`,
because a contract cannot return its own current block hash. The SDK never
invents a hash or fetches one with a second call.

## Offline example


Save the **result hex string** of a correctly configured `getPoolData(0,64)`
call in `POOL_DATA_FILE`. Set `THOGAMM_PROXY`, `TOKEN_IN`, `TOKEN_OUT`, comma-
separated `AMOUNTS_IN`, `EFFECTIVE_GAS_PRICE_WEI` and `FAST_LANE_HOT` (`true` or
`false`). Optionally set the decimal `BLOCK_NUMBER` used for the call to validate
the returned number. Then run:

```sh
cargo run --locked -p state-poll-sdk --example quote-data
```

This example has no network path. For an SDK-owned reader, use the `poll-quotes`
example in the repository README. Both share the same model and contract fixtures.

`POOL_DATA_FILE` contains just the `0x`-prefixed result hex, not JSON quotes or
the full JSON-RPC envelope. Set `AMOUNTS_IN` instead of `AMOUNT_IN` for that
example. It needs no HTTP endpoint. When optional `BLOCK_NUMBER` is provided,
the decoder verifies the response's returned number matches it.
