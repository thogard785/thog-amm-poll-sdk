//! Read the pool once and price 100 USDC -> WETH three ways.
//!
//!     MONAD_RPC=https://rpc.monad.xyz cargo run --example quote
use thogamm_poll::{Poller, TxContext, U256};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rpc = std::env::var("MONAD_RPC")?;
    let pool_address = "0x80c74517BCC2D67fFE02D3ED886796272F647210".parse()?;
    let usdc = "0x754704Bc059F8C67012fEd69BC8A327a5aafb603".parse()?;
    let weth = "0xEE8c0E9f1BFFb4Eb878d8f15f368A02a35481242".parse()?;
    let amount_in = U256::from(100_000_000u64); // 100 USDC, six decimals

    let poller = Poller::connect(rpc, pool_address).await?;
    let read = poller.pool();
    println!("read at block {}", read.snapshot().block.number);

    // Price the swap in the block it should land in, not the block that was read.
    let pool = read.at_block(read.block_number() + 1, read.base_fee())?;
    let pair = pool.pair(usdc, weth)?;

    // What makerQuoteExactInput returns.
    let quote = pair.maker_quote_exact_input(amount_in)?;
    println!("quote:          {} wei WETH", quote.amount_out);

    // What makerSwapExactInput pays in a transaction with a 10% priority fee.
    let tx = TxContext::with_gas_price(pool.base_fee() * U256::from(11) / U256::from(10));
    let swap = pair.maker_swap_exact_input(amount_in, tx)?;
    println!(
        "swap:           {} wei WETH (surcharge {} x 0.01 bp)",
        swap.amount_out, swap.surcharge
    );

    // What swapWithExtraFees pays with a 0.10% integrator fee: 1,000 parts per million.
    let with_fee = pair.swap_with_extra_fees(amount_in, 1_000, tx)?;
    println!(
        "swap with fee:  {} wei WETH to the caller, {} to the fee recipient",
        with_fee.amount_out, with_fee.fee
    );
    Ok(())
}
