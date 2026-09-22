# thog-amm-poll-sdk

[![Rust SDK](https://github.com/thogard785/thog-amm-poll-sdk/actions/workflows/ci.yml/badge.svg)](https://github.com/thogard785/thog-amm-poll-sdk/actions/workflows/ci.yml)

Rust SDK for deterministic ThogAMM quotes from **one state read**. Each update
makes **exactly one `eth_call` to `getPoolData(0,64)`**, then any number of
amounts can be quoted locally without more calls or subscriptions.


Both SDKs share the exact integer `thogamm-model` engine. It models all 64 token
slots, actual balances, portfolio risk, pair spreads, inventory limits, pauses,
staleness and execution friction. Quotes return raw `U256` token amounts.
For the other update mechanism, see the [event SDK](https://github.com/thogard785/thog-amm-event-sdk).

**Requires a compatible schema-6 ThogAMM deployment.** A published SDK does not
upgrade the live pool. Configure a verified proxy; see [deployment requirements](docs/DEPLOYMENT.md).
This library does not sign or submit transactions.

## Install

Use Rust **1.88 or newer**. For a new application, pin the release and enable
Rust-version-aware dependency selection in `Cargo.toml`:

```toml
[package]
name = "thogamm-example"
version = "0.1.0"
edition = "2021"
rust-version = "1.88"
resolver = "3"

[dependencies]
state-poll-sdk = { git = "https://github.com/thogard785/thog-amm-poll-sdk", tag = "v0.1.0" }
tokio = { version = "1.48", features = ["macros", "rt-multi-thread"] }
```

Cargo uses the **consuming application's** resolver and lockfile; it does not
inherit a Git dependency's lockfile or `.cargo/config.toml`. In an existing
workspace, set `resolver = "3"` in its root `[workspace]` and declare the intended
`rust-version` on the consuming package. This prevents fresh resolution from
choosing newer transitive dependencies that require a newer compiler than the
stated minimum. See [Cargo's Rust-version-aware resolver](https://doc.rust-lang.org/edition-guide/rust-2024/cargo-resolver.html).
The SDK repository itself already includes the matching configuration and lockfile.

The GitHub repository is named `thog-amm-poll-sdk`; the Rust crate remains `state-poll-sdk` and
imports as `state_poll_sdk`. Git installation resolves the workspace package.
Packages are distributed through GitHub, not crates.io. Commit your application's
lockfile for reproducible dependencies. No private repository access is needed.

The canonical model is in [thog-amm-poll-sdk](https://github.com/thogard785/thog-amm-poll-sdk/tree/v0.1.0/thogamm-model).
The event SDK pins that public release instead of duplicating pricing code.
Using both SDKs at matching tags resolves one shared model crate and compatible
`PoolModel`, amount and error types.

## Quickstart

Clone this repository and set the following environment variables. Substitute
your verified pool/token addresses and transaction context; the table does not
advertise any default proxy as currently deployed and compatible.

| Variable | Required value |
| --- | --- |
| `THOGAMM_HTTP_RPC` | Monad HTTP endpoint, used once per refresh |
| `THOGAMM_PROXY` | Verified schema-6 pool proxy address |
| `TOKEN_IN`, `TOKEN_OUT` | Listed ERC-20 token addresses |
| `AMOUNT_IN` | Integer sell amount in raw token units; 1,000,000 is one token only when decimals = 6 |
| `EFFECTIVE_GAS_PRICE_WEI` | Intended execution's effective transaction gas price, not its fee cap |
| `FAST_LANE_HOT` | `true` or `false`, based on the actual execution context |

```sh
git clone https://github.com/thogard785/thog-amm-poll-sdk.git
cd thog-amm-poll-sdk
cargo run --locked -p state-poll-sdk --example execution-quotes
```

The example quotes the initial image, then each published update. It holds the
supplied execution context fixed for demonstration; a router must supply the
correct gas price and warmth for each intended transaction. Each update replaces
the model atomically; unavailable quotes are reported with their source block.
The program does not send a swap. The complete executable example is
[`execution-quotes.rs`](state-poll-sdk/examples/execution-quotes.rs).

```rust
use state_poll_sdk::{Config, ExecutionContext, StatePollSdk, U256};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sell = std::env::var("TOKEN_IN")?.parse()?;
    let buy = std::env::var("TOKEN_OUT")?.parse()?;
    let amount: U256 = std::env::var("AMOUNT_IN")?.parse()?;
    let context = ExecutionContext {
        gas_price: std::env::var("EFFECTIVE_GAS_PRICE_WEI")?.parse()?,
        fast_lane_hot: std::env::var("FAST_LANE_HOT")?.parse()?,
    };
    let mut sdk = StatePollSdk::connect(
        std::env::var("THOGAMM_HTTP_RPC")?,
        std::env::var("THOGAMM_PROXY")?.parse()?,
        Config::default(),
    )
    .await?;

    loop {
        let model = sdk.model();
        match model.quote_execution_exact_input(sell, buy, amount, &context) {
            Ok(quote) => println!(
                "block={} amount_out={} last_posted_block={}",
                model.state().block.number,
                quote.amount_out,
                quote.last_posted_block
            ),
            Err(error) => eprintln!("block={} unavailable={error}", model.state().block.number),
        }
        sdk.next_update().await?;
    }
}
```

## Local quote surfaces

| API | Purpose |
| --- | --- |
| `quote_exact_input` | Match the read-only maker quote |
| `quote_execution_exact_input` | Include execution friction and settlement exposure checks |
| `quote_exact_output` | Solve the ERC-7815 Buy input with exact contract rounding |
| `marginal_price` | Rational raw output/input price |
| `limits` | Directional sell/buy bounds |
| `prepare` | Reuse decoded pair math across an amount ladder |
| `at_block` | Project aging/base-fee context while retaining source-state provenance |

These methods are synchronous and perform no network I/O. Clone the immutable
model to share a consistent snapshot across workers. Prepare each direction once
per snapshot, then reuse it for many amounts. Quoting does not mutate balances or
simulate successive fills; books share liquidity and portfolio risk.

## Choose your state reader

| Entry point | Behavior |
| --- | --- |
| `StatePollSdk::connect` / `with_reader` | One initial complete state call |
| `refresh()` | Exactly one call immediately; returns whether the model changed |
| `next_update()` | Waits the configured interval, then exactly one call; returns even if unchanged |
| `snapshot(reader, proxy, block)` | One call at the supplied number, hash or tag |
| `pool_data_params(proxy, block)` | Builds request parameters locally; no I/O |
| `decode_pool_data(proxy, bytes, block)` | Validates raw ABI bytes and builds the model locally; no I/O |

The default polling interval is 500 ms. There are no hidden retries, redirects,
pagination loops, `maxIndex`, header, storage or balance queries. Every actual
refresh invocation includes exactly one call even if state is unchanged or the
request fails. The caller decides when to invoke another refresh.

The request includes `gasPrice: "0x1"` to preserve Monad's real block BASEFEE in
the view execution. Keep this field when using your own transport. It is distinct
from the effective gas price supplied for eventual trade execution.

For an aggregator-owned RPC client, feed its single call's raw return bytes into
`decode_pool_data`. The [transport guide](docs/TRANSPORT.md) gives the request and
block-provenance rules. The fully offline
[`quote-data` example](state-poll-sdk/examples/quote-data.rs) consumes a saved
response and quotes a comma-separated amount ladder without constructing a client.

## Documentation

- [Transport lifecycle and RPC contract](docs/TRANSPORT.md)
- [Quote API, units, execution context and shared liquidity](docs/QUOTING.md)
- [Settlement encoding and router responsibilities](docs/SETTLEMENT.md)
- [Deployment and upgrade compatibility](docs/DEPLOYMENT.md)
- [Troubleshooting](docs/TROUBLESHOOTING.md)
- [CPU performance and benchmark methodology](docs/PERFORMANCE.md)
- [Source and fixture provenance](docs/PROVENANCE.md)
- [Release/version policy](docs/RELEASES.md), [changelog](CHANGELOG.md), and [contributing](CONTRIBUTING.md)

Generate API documentation locally with `cargo doc --locked --workspace --no-deps --open`.
Run all tests with `cargo test --locked --workspace --all-targets`; no live RPC
access or funded wallet is needed by the tests. CI also runs release tests,
formatting, Clippy, documentation builds, and fixture/link checks.

## License

[MIT](LICENSE). Commercial use, modification and redistribution are permitted
subject to retaining the license and copyright notice. Third-party dependencies
retain their respective licenses. Repository issues are for SDK source support;
coordinate production operation and upgrades through your deployment's contact.
