# ThogAMM polling SDK

A Rust SDK for aggregators integrating ThogAMM on Monad. Read pool state with
**one `eth_call`**, then calculate quotes for any number of trade sizes locally.

Each complete refresh discovers the current schema-7 registry, including
listings and index changes. Token addresses, decimals, balances and pricing
data are included in the same atomic update.

- **Network:** Monad mainnet, chain ID `143`
- **ThogAMM pool:** `0x80c74517BCC2D67fFE02D3ED886796272F647210`
- **State endpoint:** `getPoolData(0, 32)` on that pool

## Install

```toml
[dependencies]
state-poll-sdk = { git = "https://github.com/thogard785/thog-amm-poll-sdk", tag = "v0.3.0" }
```

Requires Rust 1.88 or later and an async runtime such as Tokio for state reads.
Both Cargo resolvers 2 and 3 are supported. Commit your application's
`Cargo.lock`. Import the crate as `state_poll_sdk`.

## Connect once, then quote locally

Pass your Monad HTTP RPC URL, the ThogAMM pool address above, the two token
addresses, an input amount and your transaction's effective gas price in wei:

```rust
use state_poll_sdk::{Address, Config, Result, StatePollSdk, U256};

async fn follow_quotes(
    rpc: &str,
    pool: Address,
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    gas_price: U256,
) -> Result<()> {
    let mut sdk = StatePollSdk::connect(rpc, pool, Config::default()).await?;
    loop {
        let model = sdk.model()?;
        match model.quote_execution_exact_input(token_in, token_out, amount_in, gas_price) {
            Ok(quote) => println!("amount_out={}", quote.amount_out),
            Err(error) => eprintln!("quote unavailable: {error}"),
        }
        sdk.next_update().await?;
    }
}
```

`connect()` loads the initial model with one call. `refresh()` updates it with
exactly one call. `next_update()` refreshes on a 250 ms cadence by default,
without adding the RPC duration to each interval or bursting to catch up.
If you already follow chain heads, call `refresh()` for each new block. These updates include all listed tokens and
require no additional header, balance or token-discovery calls.

You control when to update. Quote methods are synchronous and make no network
calls. A failed refresh returns its cause and makes `sdk.model()` unavailable
until a complete refresh succeeds. Use `model.state().block.number` to identify
the source block. `model.state().tokens` provides
listed token addresses and decimals.

## Quote trade sizes

Amounts are `U256` values in raw token units: one token with six decimals is
`1_000_000`. `gas_price` is the intended transaction's effective price in wei;
for EIP-1559, use `min(maxFeePerGas, baseFee + maxPriorityFeePerGas)`.
Execution applies one 10 bp output reduction if that price exceeds twice the
quote block base fee or the AuctionHandler is already warm. The convenience
methods below assume a cold AuctionHandler. Use
`quote_execution_exact_input_with_context(..., gas_price, auction_handler_warm)`
and `quote_exact_output_with_context(...)` to specify account warmth explicitly.
`quote_exact_input(...)` matches `makerQuoteExactInput` with its strict 90%
receiving-inventory caps and no execution widening. Execution checks full caps.

| Method on `PoolModel` | Result |
| --- | --- |
| `quote_execution_exact_input(token_in, token_out, amount_in, gas_price)` | Output amount, including final full-cap checks; cold AuctionHandler |
| `quote_exact_output(token_in, token_out, amount_out, gas_price)` | Required input for an exact output amount |
| `limits(token_in, token_out)` | Directional input and output bounds |
| `marginal_price(token_in, token_out, amount_in)` | A fraction expressing raw output units per raw input unit |

For multiple amounts on the same pair, prepare it once per snapshot:

```rust
let pair = model.prepare(token_in, token_out)?;
for amount_in in amounts_in {
    let quote = pair.quote_execution_exact_input(amount_in, gas_price)?;
    println!("{} -> {}", quote.amount_in, quote.amount_out);
}
```

A prepared pair has the same quote methods, without the two token arguments.
Preparation and successful quotes allocate no memory. Clone a model to share
its immutable snapshot across workers, and prepare a new pair after an update.
Quotes use the contract's integer rounding and reject paused trading, stale
prices, disabled directions and insufficient capacity. `quote_exact_input`
without a gas price is also available to reproduce the contract's read-only
quote; use the execution methods above for routing.

Quotes apply to the snapshot's block. To evaluate price aging for a later landing
block, use `model.at_block(number, base_fee)` and quote against the returned model.
This assumes no intervening trades or price updates. Pairs share balances and
portfolio risk; quoting several trades independently does not simulate their
combined execution.

## Use your own RPC client

`pool_data_params(pool, block)` builds the JSON-RPC parameters for a single
`eth_call`. Send that request through your existing transport, then pass its raw
ABI result bytes to `decode_pool_data(pool, &bytes, block)` to build a model.
Decoding and all subsequent quotes work offline. Keep the generated parameters
unchanged: they ensure the state read includes the block's correct base fee.

`CallBlock` accepts latest, finalized, a block number or a block hash. Use the
same selector for fetching and decoding. `snapshot(reader, pool, block)` and
`StatePollSdk::with_reader` support a custom `ChainReader` implementation.
See [examples/offline.rs](examples/offline.rs) for quoting a saved response.

## Build a swap transaction

The SDK exposes `model::abi::makerSwapExactInputCall` with `tokenIn`, `tokenOut`,
`amountIn`, `minAmountOut`, `recipient` and `deadlineBlock`. Encode it with
`alloy_sol_types::SolCall` (`alloy-sol-types = "1"`) and send it to the
ThogAMM pool. The input holder approves the pool to spend the input token;
the pool pulls it from the caller using `transferFrom`. Use wrapped MON for
native-token trades and send zero native value.

Set `minAmountOut` to your acceptable minimum and `deadlineBlock` to an inclusive
block deadline. An exact-output quote executed through this exact-input swap
may return a small rounding surplus. Your router handles approvals, gas
estimation, signing and submission. The SDK calculates quotes and encodes calls.

## Run the examples

Set `THOGAMM_HTTP_RPC`, `THOGAMM_PROXY` (the pool address above), `TOKEN_IN`,
`TOKEN_OUT`, `AMOUNT_IN` and `EFFECTIVE_GAS_PRICE_WEI`, then run:

```sh
cargo run --locked --example poll
```

[examples/poll.rs](examples/poll.rs) refreshes state and prints local quotes.
For [examples/offline.rs](examples/offline.rs), set `POOL_DATA_FILE` to a file
containing the raw result hex, use comma-separated `AMOUNTS_IN`, and keep the
same pool, token and gas-price variables. Optionally set `BLOCK_NUMBER` to check
the response's block. Run `cargo run --locked --example offline`.

The [event SDK](https://github.com/thogard785/thog-amm-event-sdk) uses the same
quote model and updates it through subscriptions. Both SDKs are [MIT licensed](LICENSE).

## Schema 7 and verification

Version **0.3.0** supports schema 7 / wire 7 / model 1 and up to 32 active
tokens. It replaces the incompatible schema-6 model. State includes all 126
words, all registered ERC-20 balances, native MON, lending exposure offsets,
separate bid/ask widths, volume charges, wrapper policies and inventory caps.

Volume cost is `ceil(rate * notionalUSDWad² / 1e27)` and remains payable for
risk-reducing trades, same-category trades and zero lambda. Sliding output
refunds participate in the nonlinear output solve with exact integer rounding.

The [integration guide](https://github.com/thogard785/thog-amm-integration-guide)
contains the full ABI, state layout, math, settlement rules and reproducible
fixtures. Tests cover **9,240 contract comparisons**: 2,016 mainnet read-only
queries, 1,680 execution/exact-output calls on a local Monad fork, and 5,544
fork boundary cases. All 5,362 successful results match exactly; all 3,878
rejected cases also reject locally. Event replay matches 200 consecutive
mainnet snapshots, including every word, ERC-20 balance and native balance.
Run `cargo test --locked` (or `cargo test --workspace --locked` in the polling
repository). Fork tests do not broadcast transactions.

Live `sdk.model()` returns `Result<&PoolModel>`. A failed or cancelled refresh
invalidates live access until a full read succeeds. `received_at()` exposes the
last successful read time, and `Config::request_timeout` bounds each read
(default 2 seconds). Run updates continuously and compare the snapshot block
with your own head. A cloned model is a historical snapshot; it does not become
current as wall time passes. Use `at_block(landing_block, base_fee)` to evaluate
the contract's exact block-age rule for an intended landing block.
