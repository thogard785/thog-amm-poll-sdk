# ThogAMM polling SDK

A Rust SDK for aggregators integrating ThogAMM on Monad. Read pool state with
**one `eth_call`**, then calculate quotes for any number of trade sizes locally.

It works with the current ThogAMM deployment and automatically supports all
current and future tokens listed on ThogAMM. Token addresses, decimals, balances
and pricing data are included in every update.

- **Network:** Monad mainnet, chain ID `143`
- **ThogAMM pool:** `0x80c74517BCC2D67fFE02D3ED886796272F647210`
- **State endpoint:** `getPoolData(0, 64)` on that pool

## Install

```toml
[dependencies]
state-poll-sdk = { git = "https://github.com/thogard785/thog-amm-poll-sdk", tag = "v0.2.0" }
```

Requires Rust 1.88 or later and an async runtime such as Tokio for state reads.
Set your application's `rust-version` and use Cargo resolver `"3"` at the package
or workspace root so dependencies respect that Rust version. Commit your
application's `Cargo.lock`. Import the crate as `state_poll_sdk`.

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
        let model = sdk.model();
        match model.quote_execution_exact_input(token_in, token_out, amount_in, gas_price) {
            Ok(quote) => println!("amount_out={}", quote.amount_out),
            Err(error) => eprintln!("quote unavailable: {error}"),
        }
        sdk.next_update().await?;
    }
}
```

`connect()` loads the initial model with one call. `refresh()` updates it with
exactly one call. `next_update()` waits for the configured interval (500 ms by
default) and then refreshes once. These updates include all listed tokens and
require no additional header, balance or token-discovery calls.

You control when to update. Quote methods are synchronous and make no network
calls. A failed refresh preserves the last valid snapshot; use
`model.state().block.number` to track its age. `model.state().tokens` provides
listed token addresses and decimals.

## Quote trade sizes

Amounts are `U256` values in raw token units: one token with six decimals is
`1_000_000`. `gas_price` is the intended transaction's effective price in wei;
for EIP-1559, use `min(maxFeePerGas, baseFee + maxPriorityFeePerGas)`.
The model includes additional spread if that price exceeds twice the block base fee.

| Method on `PoolModel` | Result |
| --- | --- |
| `quote_execution_exact_input(token_in, token_out, amount_in, gas_price)` | Output amount, including balance and exposure checks |
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
`alloy_sol_types::SolCall` (`alloy-sol-types = "=1.7.3"`) and send it to the
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
