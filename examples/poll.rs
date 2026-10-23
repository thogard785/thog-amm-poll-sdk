use state_poll_sdk::{Address, Config, Error, Result, StatePollSdk, U256};
use std::time::Duration;

async fn follow_quotes(
    http_rpc: &str,
    pool: Address,
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    gas_price: U256,
) -> Result<()> {
    let mut delay = Duration::from_millis(250);
    let mut sdk = loop {
        match StatePollSdk::connect(http_rpc, pool, Config::default()).await {
            Ok(sdk) => break sdk,
            Err(error @ (Error::Transport(_) | Error::Rpc(_))) => {
                eprintln!("connect failed: {error}; retrying in {delay:?}");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(8));
            }
            Err(error) => return Err(error),
        }
    };
    loop {
        match sdk.next_update().await {
            Ok(model) => {
                delay = Duration::from_millis(250);
                match model.quote_execution_exact_input(token_in, token_out, amount_in, gas_price) {
                    Ok(q) => println!(
                        "block={} amount_out={}",
                        model.state().block.number,
                        q.amount_out
                    ),
                    Err(error) => eprintln!("quote unavailable: {error}"),
                }
            }
            Err(error @ (Error::Transport(_) | Error::Rpc(_) | Error::ProviderBehind { .. })) => {
                eprintln!("refresh failed: {error}; retrying in {delay:?}");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(8));
            }
            Err(error) => return Err(error),
        }
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    follow_quotes(
        &std::env::var("THOGAMM_HTTP_RPC")?,
        std::env::var("THOGAMM_PROXY")?.parse()?,
        std::env::var("TOKEN_IN")?.parse()?,
        std::env::var("TOKEN_OUT")?.parse()?,
        std::env::var("AMOUNT_IN")?.parse()?,
        std::env::var("EFFECTIVE_GAS_PRICE_WEI")?.parse()?,
    )
    .await?;
    Ok(())
}
