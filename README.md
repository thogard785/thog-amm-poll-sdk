# ThogAMM polling SDK

Read the complete ThogAMM state with **one `eth_call` to `getPoolData(0,64)`**,
then calculate deterministic quotes locally for any number of amounts. Supports
up to 64 tokens and requires a compatible **schema-6** contract deployment.

## Install

```toml
[dependencies]
state-poll-sdk = { git = "https://github.com/thogard785/thog-amm-poll-sdk", tag = "v0.1.1" }
```

Requires Rust 1.88+. In your application, declare `rust-version = "1.88"` and
use Cargo resolver `"3"` (at the workspace root for a workspace). Commit the
application's lockfile: dependencies do not inherit this repository's lockfile.
The crate imports as `state_poll_sdk` and is distributed through GitHub.

## Read state and quote

```rust
use state_poll_sdk::{Address, Config, ExecutionContext, Result, StatePollSdk, U256};

async fn quote(
    rpc: &str,
    proxy: Address,
    sell: Address,
    buy: Address,
    amount: U256,
    context: &ExecutionContext,
) -> Result<U256> {
    let sdk = StatePollSdk::connect(rpc, proxy, Config::default()).await?;
    let pair = sdk.model().prepare(sell, buy)?;
    Ok(pair.quote_execution_exact_input(amount, context)?.amount_out)
}
```

Keep the client and prepared pair for repeated quoting. `connect()` makes one
initial call. `refresh()` makes exactly one call immediately; `next_update()`
waits the configured interval (500 ms by default), then makes exactly one call.
Both return even when state is unchanged. There are no extra discovery, header,
balance or pagination calls, redirects, automatic retries or subscriptions.

Each response includes metadata, balances, pricing/risk words and block context,
including newly listed tokens. A valid update replaces the model atomically.
Errors preserve the previous model and its original block stamp; an older response
returns `ProviderBehind`. Clone a model to share its immutable state across workers.
Prepare a new pair when selecting a new snapshot.

For your own RPC client, `pool_data_params(proxy, block)` builds the parameters
for one call and `decode_pool_data(proxy, &bytes, block)` builds the model from
its raw ABI return bytes without I/O. `snapshot(reader, proxy, block)` accepts
a custom `ChainReader`. `CallBlock` supports latest, finalized, number or hash.

Keep `gasPrice: "0x1"` in state-read requests: omitting fee fields on Monad can
zero the simulated BASEFEE. This preserves the block's base fee without another
read and is separate from a trade's gas price. Latest/number snapshots have no
current block hash; hash-pinned calls use the supplied EIP-1898 canonical hash.
The contract cannot return its current block hash.

## Quoting

All amounts are raw token units in `U256`; decimals come from the snapshot.
Math matches Solidity's integer rounding, overflow and availability rules.

| Method | Behavior |
| --- | --- |
| `quote_exact_input` | Read-only maker quote, including output balance checks |
| `quote_execution_exact_input` | Adds execution friction and settlement exposure checks |
| `quote_exact_output` | ERC-7815 Buy solver with contract rounding and execution context |
| `marginal_price` | Rational raw output/input price |
| `limits` | Directional sell/buy bounds |

`model.prepare(sell, buy)` reuses pair calculations across an amount ladder.
Its methods omit the two address arguments. Quotes are synchronous and perform
no I/O. Pauses, disabled sides, stale prices, insufficient inventory and arithmetic
limits produce explicit errors. Bounds are not a liquidity reservation.

For execution quotes, supply `ExecutionContext { gas_price, fast_lane_hot }`.
The gas price is effective `tx.gasprice`, not the fee cap. Friction applies once
when gas price exceeds twice the block base fee or the FastLane account is warm
at the contract's check. Account warmth is transaction-local, not pool state.

Quotes use the snapshot's block. `model.at_block(number, base_fee)?` projects
aging into a landing block while preserving the source identity; it does not
predict intervening trades or updates. Balances and portfolio risk are shared
across pairs. Repeated quotes are alternatives against one snapshot, not a
simulation of successive fills.

For direct settlement, `model::abi::makerSwapExactInputCall` encodes tokenIn,
tokenOut, amountIn, minAmountOut, recipient and deadlineBlock. Use
`alloy_sol_types::SolCall` (`alloy-sol-types = "=1.7.3"`) to encode it. The caller
holds the input and approves the proxy, which pulls funds using `transferFrom`.
Send zero native value and use wrapped native tokens. The deadline is an inclusive
block number. An exact-output solve used with this exact-input entry point can
produce integer surplus. Router fees, gas estimation, fee-on-transfer/rebasing
tokens, signing and transaction submission are outside the quote model.

## Examples and tests

Run `cargo run --locked --example poll` with `THOGAMM_HTTP_RPC`, `THOGAMM_PROXY`,
`TOKEN_IN`, `TOKEN_OUT`, `AMOUNT_IN`, `EFFECTIVE_GAS_PRICE_WEI` and `FAST_LANE_HOT`
(`true`/`false`). It uses the supplied execution context for each quote.

`cargo run --locked --example offline` quotes a saved response with no RPC client.
Use `POOL_DATA_FILE` containing just the result hex, comma-separated `AMOUNTS_IN`,
the same proxy/token/context variables, and optionally `BLOCK_NUMBER` to validate
the response's number. It does not need `THOGAMM_HTTP_RPC` or `AMOUNT_IN`.

```sh
cargo test --locked --release --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
```

The shared engine is in [model/](model/). The
[event SDK](https://github.com/thogard785/thog-amm-event-sdk) pins the same model
release. Tests retain the Solidity-generated quote fixtures and verify rounding,
state transitions and exact RPC counts. No live network is required by the tests.
Verify the deployed implementation before routing; ABI compatibility alone does
not prove unchanged pricing semantics after an upgrade.

[MIT license](LICENSE).
