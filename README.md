# ThogAMM polling SDK

Quote [ThogAMM](https://github.com/thogard785/thog-amm-integration-guide) in
your own process. The SDK reads the pool's state over HTTP and reproduces the
contract's arithmetic in memory, to the last raw unit, so a route search can
try thousands of amounts without a network call.

- Network: Monad mainnet, chain ID 143
- Pool: `0x80c74517BCC2D67fFE02D3ED886796272F647210`
- Supports implementation `0xb108a1daf3c9ba52778b97a15ebdfa3f6f59b506`
- Rust 1.88 or later. Before Rust 1.90, your project also needs Cargo's
  `resolver = "3"`, which the 2024 edition uses by default

New to ThogAMM? Read the
[integration guide](https://github.com/thogard785/thog-amm-integration-guide/blob/main/THOGAMM_AGGREGATOR_INTEGRATION.md)
first. To follow the pool from a WebSocket subscription instead of polling, use
the [event SDK](https://github.com/thogard785/thog-amm-event-sdk).

## Use it

```toml
[dependencies]
thogamm-poll = { git = "https://github.com/thogard785/thog-amm-poll-sdk", tag = "v0.7.0" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust
use thogamm_poll::{Poller, TxContext, U256};

let mut poller = Poller::connect(rpc_url, pool_address).await?;

// Price the swap in the block it should land in.
let read = poller.pool();
let pool = read.at_block(read.block_number() + 1, read.base_fee())?;

let pair = pool.pair(usdc, weth)?;
let quote = pair.maker_quote_exact_input(amount_in)?;          // makerQuoteExactInput
let swap = pair.maker_swap_exact_input(amount_in, tx)?;        // makerSwapExactInput
let with_fee = pair.swap_with_extra_fees(amount_in, 1_000, tx)?; // swapWithExtraFees, 0.10%

poller.refresh().await?; // read the pool again: one request
```

[examples/quote.rs](examples/quote.rs) is this as a complete program, and
[examples/poll.rs](examples/poll.rs) follows the pool in a loop:

```sh
MONAD_RPC=https://rpc.monad.xyz cargo run --example quote
```

## What you get

A `Pool` is the pool at one block. `pool.pair(token_in, token_out)` prepares
one direction; its methods have the names of the contract calls they reproduce
and return the same numbers.

| Contract call | Method on the pair | Returns |
| --- | --- | --- |
| `makerQuoteExactInput` | `maker_quote_exact_input(amount_in)` | `Quote { amount_out, price_block }` |
| `makerSwapExactInput` | `maker_swap_exact_input(amount_in, tx)` | `Swap` |
| `swapWithExtraFees` | `swap_with_extra_fees(amount_in, extra_fee_ppm, tx)` | `Swap` |
| ERC-7815 `swap`, sell side | `swap_sell(amount_in, tx)` | `Swap` |
| ERC-7815 `swap`, buy side | `swap_buy(amount_out, tx)` | `Swap` |
| ERC-7815 `price` | `price(amount_in)` | `Fraction` |
| ERC-7815 `getLimits` | `get_limits()` | `Limits { sell, buy }` |

A `Swap` holds `amount_in`, `amount_out` (what the call returns), `fee` (the
integrator fee) and `surcharge` (the execution surcharge that was applied).
`tx` is a `TxContext`: the transaction's effective gas price, and whether the
FastLane AuctionHandler is warm when the pool is called. Together they decide
the surcharge; the guide explains
[both](https://github.com/thogard785/thog-amm-integration-guide/blob/main/THOGAMM_AGGREGATOR_INTEGRATION.md#execution-surcharge).

A call the contract would revert returns `Error::Rejected` with the reason,
for example `Rejection::StalePrices` or `Rejection::InventoryLimit`.

Three things to get right:

- **Quote at the block you will trade in.** Prices age every block.
  `pool.at_block(number, base_fee)` evaluates the state you read at a later
  block; quotes fail there once the prices have expired
  (`pool.prices_valid_through()`).
- **`extra_fee_ppm` is parts per million.** 100 is one basis point; 10,000 is
  1%. Using a fee needs a fee recipient set up with us first; see the
  [guide](https://github.com/thogard785/thog-amm-integration-guide/blob/main/THOGAMM_AGGREGATOR_INTEGRATION.md#swap-with-an-integrator-fee).
- **Quote a route's swaps in sequence.** `pool.after(&swap)` is the pool as
  that swap leaves it. Quoting two swaps against the same pool ignores the
  first one's effect on the second.

## Keeping it current

`poller.refresh()` reads the pool again with one `eth_call` and returns whether
anything changed. `poller.next_update()` waits the polling interval (500 ms
unless changed with `set_interval`) and then refreshes.

A failed refresh returns the error and keeps the state from the last
successful read. That state keeps aging: `pool.snapshot().block.number` says
which block it is from.

## Your own RPC client

Implement `EthCall`, one method, to read through the client you already have:

```rust
impl thogamm_poll::EthCall for MyClient {
    async fn eth_call(&self, to: Address, data: Bytes, block: BlockId) -> thogamm::Result<Bytes> {
        // Send with gasPrice set, or a Monad node reports a zero base fee.
        self.call(to, data, block).await
    }
}
let poller = Poller::with_rpc(my_client, pool_address).await?;
```

Or leave the network out entirely: `thogamm::rpc::snapshot_call(pool, tokens)`
gives the call to make and `decode_snapshot` turns its output into a
`Snapshot` for `Pool::new`.

## Layout

| Path | Contents |
| --- | --- |
| `src/lib.rs` | `Poller` |
| `model/` | The `thogamm` crate: state, arithmetic and events, with no polling. The event SDK uses it too. |
| `model/src/layout.rs` | Where each value sits in the pool's storage |
| `model/src/snapshot.rs` | The state read at one block |
| `model/src/pool.rs`, `curve.rs` | Decoding and pricing |
| `model/src/events.rs` | Advancing a snapshot by one block of logs |
| `model/src/rpc.rs` | Reading a snapshot in one call |

The [reference](https://github.com/thogard785/thog-amm-integration-guide/blob/main/REFERENCE.md)
specifies what this code implements, section for file.

## Tests

```sh
cargo test --workspace --release
```

The tests replay 61,920 calls recorded from the contract (quotes, marginal
prices, limits, swaps at every surcharge tier, exact-output swaps) and require
the same integers, and a rejection wherever the contract reverted. They also
replay 250 recorded blocks of logs and compare the result with the state read
from the chain at each block.

Three more tests compare against the deployed pool and need an endpoint:

```sh
MONAD_RPC=https://rpc.monad.xyz cargo test --release -p thogamm --test live -- --ignored --nocapture
```

They check every pair's quote, price and limits at one block, simulate swaps
and fee swaps at each surcharge tier, and check how a transaction's fee fields
and access list set the surcharge.

[MIT license](LICENSE).
