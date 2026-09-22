//! Offline amount-ladder quoting from an aggregator's own eth_call response.
//! No RPC client, subscriptions, polling task, or network access is constructed.
use state_poll_sdk::{decode_pool_data, model::Bytes, CallBlock, ExecutionContext, U256};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proxy = std::env::var("THOGAMM_PROXY")?.parse()?;
    let input = std::env::var("TOKEN_IN")?.parse()?;
    let output = std::env::var("TOKEN_OUT")?.parse()?;
    let response = std::fs::read_to_string(std::env::var("POOL_DATA_FILE")?)?;
    let bytes: Bytes = response.trim().parse()?;
    let block = match std::env::var("BLOCK_NUMBER") {
        Ok(number) => CallBlock::Number(number.parse()?),
        Err(std::env::VarError::NotPresent) => CallBlock::Latest,
        Err(error) => return Err(error.into()),
    };
    let model = decode_pool_data(proxy, &bytes, block)?;
    let execution = ExecutionContext {
        gas_price: std::env::var("EFFECTIVE_GAS_PRICE_WEI")?.parse()?,
        fast_lane_hot: std::env::var("FAST_LANE_HOT")?.parse()?,
    };
    let pair = model.prepare(input, output)?;
    for amount in std::env::var("AMOUNTS_IN")?.split(',') {
        let amount: U256 = amount.trim().parse()?;
        match pair.quote_execution_exact_input(amount, &execution) {
            Ok(quote) => println!(
                "block={} input={amount} output={}",
                model.state().block.number,
                quote.amount_out
            ),
            Err(error) => println!(
                "block={} input={amount} unavailable={error}",
                model.state().block.number
            ),
        }
    }
    Ok(())
}
