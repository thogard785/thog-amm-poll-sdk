# ThogAMM polling SDK

Read a complete ThogAMM snapshot, then quote any number of trade sizes locally.
Version **0.4.1** supports **schema 8 / wire 8 / model 2** and the current 13-token
registry, including **SPYx, QQQx, EWYx, NVDAx and TSLAx**. These are unwrapped
xStocks with 18 decimals. Addresses are in the [integration guide](https://github.com/thogard785/thog-amm-integration-guide/blob/main/THOGAMM_AGGREGATOR_INTEGRATION.md#listed-tokens).

- Monad mainnet, chain ID **143**.
- Pool: `0x80c74517BCC2D67fFE02D3ED886796272F647210`.
- Rust **1.88+**, Cargo resolver **3**, Tokio. Commit your application's lockfile.

## Install and quote

```toml
[dependencies]
state-poll-sdk = { git = "https://github.com/thogard785/thog-amm-poll-sdk", tag = "v0.4.1" }
```

```rust
use state_poll_sdk::{Address, Config, Result, StatePollSdk, U256};

async fn quote(
    rpc: &str, pool: Address, token_in: Address, token_out: Address,
    amount_in: U256, effective_gas_price: U256,
) -> Result<U256> {
    let sdk = StatePollSdk::connect(rpc, pool, Config::default()).await?;
    Ok(sdk.model().quote_execution_exact_input(
        token_in, token_out, amount_in, effective_gas_price,
    )?.amount_out)
}
```

Keep the client for subsequent quotes. `refresh()` updates it immediately;
`next_update()` waits for the configured interval (500 ms by default), then
refreshes once. Each update reads a header, `getPoolData(0, 28)`, and one batch of
`sharesOf(pool)` plus `getCurrentMultiplier()` calls for listed xStocks. All calls
are pinned to the same block hash: **three HTTP requests** with the current list.

Quote methods are synchronous and perform no network calls. A failed refresh
returns an error and retains the previous snapshot. Check
`model.state().block.number` and `.hash` for provenance; `.tokens` supplies the
active registry. New listings and index changes are discovered on refresh.
Share accounting recognizes the contract's [nine compiled xStock addresses](model/src/shares.rs).
Listing another of those nine needs no model change; extending the contract's
xStock catalog requires a matching SDK release.

## Quote methods

| Method on `PoolModel` | Matches |
| --- | --- |
| `quote_exact_input(in, out, amount)` | Read-only `makerQuoteExactInput`, including 90% quote caps |
| `quote_execution_exact_input(in, out, amount, gas_price)` | `makerSwapExactInput`, including friction and full settlement caps |
| `quote_exact_output(in, out, target, gas_price)` | Required input for ERC-7815 Buy |
| `limits(in, out)` | Directional upper bounds; smaller sizes may still reject |
| `marginal_price(in, out, amount)` | Fraction of raw output units per raw input unit |

Amounts are `U256` raw units. Execution methods assume an ordinary external
caller/recipient and no extra fee. Their defaults use a cold AuctionHandler;
`quote_execution_exact_input_with_context` and `quote_exact_output_with_context`
accept `auction_handler_warm` as the final argument. Pass the transaction's
**effective** gas price, not its maximum fee cap. The model reproduces the
contract's share floors, dust and the distinct direct/Buy friction rounding.

`Quote.amount_in` is the amount passed to Thog. For execution quotes,
`amount_out` is guaranteed recipient credit at the quoted state/context:
compare `received >= amount_out`, not equality. For Buy it equals the target;
actual xStock credit may be higher. Read-only quotes omit execution friction.
If your router first pulls funds, quote/pass its measured newly received input,
not the requested transfer or its total balance. See the [funding example](https://github.com/thogard785/thog-amm-integration-guide/blob/main/THOGAMM_AGGREGATOR_INTEGRATION.md#funding-through-an-aggregator).

For an amount ladder, call `model.prepare(in, out)?` once and use the same methods
on the returned pair, omitting token arguments. Clone models to share immutable
state. Rebuild prepared pairs after updates. Independent quotes do not simulate
successive fills. `at_block(number, base_fee)` changes price age only; it holds
inventory, policy and issuer multipliers fixed.

## Custom transport or offline state

Implement `ChainReader::header` and `::calls`, then use `with_reader` or
`snapshot(reader, pool, CallBlock)`. The built-in transport accepts latest,
finalized, number or hash selectors and resolves all state reads to a hash.

For externally fetched data, use `pool_data_params(pool, block)` and
`decode_pool_data(pool, &bytes, block, share_balances)`. Supply an address-keyed
`model::ShareBalances` map containing each listed xStock's physical `shares` and
current `multiplier` from that same block. Decoding checks the share valuation
against the pool's physical balances. Keep the state call's `gasPrice: 0x1`:
it preserves Monad's block base fee. The multiplier return is three ABI words.

## Transactions and examples

Encode `model::abi::makerSwapExactInputCall` with `alloy_sol_types::SolCall`:
`tokenIn`, `tokenOut`, `amountIn`, `minAmountOut`, `recipient`, `deadlineBlock`.
Approve the pool for input and send zero native value. Your router handles
simulation, signing and submission. See the [settlement guide](https://github.com/thogard785/thog-amm-integration-guide/blob/main/THOGAMM_AGGREGATOR_INTEGRATION.md#quotes-limits-and-settlement).

[examples/poll.rs](examples/poll.rs) uses `THOGAMM_HTTP_RPC`, `THOGAMM_PROXY`,
`TOKEN_IN`, `TOKEN_OUT`, `AMOUNT_IN` and `EFFECTIVE_GAS_PRICE_WEI`:

```sh
cargo run --locked --example poll
```

[examples/offline.rs](examples/offline.rs) takes `POOL_DATA_FILE` (raw return hex),
`SHARE_STATE_FILE` (JSON `ShareBalances`), comma-separated `AMOUNTS_IN` and the same
pool/token/gas variables. Optional `BLOCK_NUMBER` checks the saved block.

## Verification and migration

Run `cargo test --workspace --locked`. The [shared contract corpus](https://github.com/thogard785/thog-amm-integration-guide#verification)
contains **39,936 comparisons**, plus 200 blocks of exact event replay. Expected
results come from deployed contract reads and local fork execution.

Upgrade from 0.3 by switching to schema 8, supplying issuer state to offline
constructors and implementing the new reader methods. Schema-7 snapshots are
rejected. The [event SDK](https://github.com/thogard785/thog-amm-event-sdk) shares
this model. Both SDKs are [MIT licensed](LICENSE).
