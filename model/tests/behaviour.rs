//! Properties of the public API that the recorded contract calls do not pin down.
#[path = "../../tests/common/mod.rs"]
mod common;
use common::*;
use thogamm::{layout, Address, Error, Pool, Rejection, Snapshot, TxContext, U256};

fn u(value: u64) -> U256 {
    U256::from(value)
}

/// (pool, USDC, WETH, an xStock with inventory): the deployed 17-token pool.
fn deployed_pool() -> (Pool, Address, Address, Address) {
    let (_, pool) = deployed();
    let tokens = pool.tokens();
    let stock = tokens
        .iter()
        .find(|t| t.xstock.as_ref().is_some_and(|x| !x.shares.is_zero()))
        .unwrap()
        .address;
    (pool.clone(), tokens[0].address, tokens[4].address, stock)
}

fn cold(gas_price: U256) -> TxContext {
    TxContext::with_gas_price(gas_price)
}

#[test]
fn a_swap_pays_the_quote_reduced_by_the_surcharge_tier() {
    let f = fixture("execution-model3.json.gz");
    // This snapshot was read in the block its prices were updated in.
    let same_block = pool(address(&f["proxy"]), &f["snapshots"][0]);
    assert_eq!(same_block.price_block(), same_block.block_number());
    let base = same_block.base_fee();
    let next_block = same_block
        .at_block(same_block.block_number() + 1, base)
        .unwrap();
    let (usdc, weth) = (
        same_block.tokens()[0].address,
        same_block.tokens()[4].address,
    );
    let warm = TxContext {
        gas_price: base,
        auction_handler_warm: true,
    };
    for (tx, in_price_block, later) in [
        (cold(base + base / u(2)), 0, 0),
        (cold(base + base / u(2) + u(1)), 50, 600),
        (cold(base * u(2) + u(1)), 100, 800),
        (cold(base * u(2) + base / u(2) + u(1)), 200, 1200),
        (warm, 200, 1200),
    ] {
        for (pool, surcharge) in [(&same_block, in_price_block), (&next_block, later)] {
            let pair = pool.pair(usdc, weth).unwrap();
            let quote = pair.maker_quote_exact_input(u(1_000_000)).unwrap();
            let swap = pair.maker_swap_exact_input(u(1_000_000), tx).unwrap();
            assert_eq!(swap.surcharge, surcharge);
            assert_eq!(
                swap.amount_out,
                quote.amount_out * u(1_000_000 - u64::from(surcharge)) / u(1_000_000)
            );
            assert_eq!(swap.fee, U256::ZERO);
        }
    }
}

#[test]
fn prices_age_with_the_evaluation_block_until_they_expire() {
    let (pool, usdc, weth, _) = deployed_pool();
    let amount = u(100_000_000);
    let now = pool
        .pair(usdc, weth)
        .unwrap()
        .maker_quote_exact_input(amount)
        .unwrap();
    assert_eq!(now.price_block, pool.price_block());
    let last = pool.prices_valid_through();
    assert!(last > pool.block_number());
    let older = pool.at_block(last, pool.base_fee()).unwrap();
    let aged = older
        .pair(usdc, weth)
        .unwrap()
        .maker_quote_exact_input(amount)
        .unwrap();
    assert!(aged.amount_out < now.amount_out, "an older price pays less");
    // Only the evaluation block moved; the state is the one that was read.
    assert_eq!(older.snapshot(), pool.snapshot());
    let expired = pool.at_block(last + 1, pool.base_fee()).unwrap();
    assert!(matches!(
        expired.pair(usdc, weth).unwrap_err().rejection(),
        Some(Rejection::StalePrices)
    ));
    assert!(pool
        .at_block(pool.block_number() - 1, pool.base_fee())
        .is_err());
}

#[test]
fn the_integrator_fee_is_parts_per_million_of_the_output() {
    let (pool, usdc, weth, stock) = deployed_pool();
    let tx = cold(pool.base_fee());
    for (token_out, amount) in [(weth, u(100_000_000)), (stock, u(50_000_000))] {
        let pair = pool.pair(usdc, token_out).unwrap();
        let plain = pair.maker_swap_exact_input(amount, tx).unwrap();
        // 100 ppm is one basis point; 10,000 ppm is one percent.
        for ppm in [0u32, 1, 100, 10_000, 999_999, 1_000_000] {
            let swap = pair.swap_with_extra_fees(amount, ppm, tx).unwrap();
            assert_eq!(
                swap.fee,
                plain.amount_out * u(u64::from(ppm)) / u(1_000_000)
            );
            assert_eq!(swap.amount_out + swap.fee, plain.amount_out);
        }
        let percent = pair.swap_with_extra_fees(amount, 10_000, tx).unwrap();
        assert_eq!(percent.fee, plain.amount_out / u(100));
        assert!(matches!(
            pair.swap_with_extra_fees(amount, 1_000_001, tx)
                .unwrap_err()
                .rejection(),
            Some(Rejection::FeeTooHigh)
        ));
    }
}

#[test]
fn buy_finds_the_smallest_input_that_sell_turns_into_the_target() {
    let (pool, usdc, weth, stock) = deployed_pool();
    let tx = cold(pool.base_fee() * u(3));
    for (token_in, token_out, target) in [
        (usdc, weth, u(10_000_000_000_000_000)),
        (weth, usdc, u(25_000_000)),
        (usdc, stock, u(100_000_000_000_000_000)),
        (stock, usdc, u(25_000_000)),
    ] {
        let pair = pool.pair(token_in, token_out).unwrap();
        let buy = pair.swap_buy(target, tx).unwrap();
        assert_eq!(buy.amount_out, target);
        assert!(pair.swap_sell(buy.amount_in, tx).unwrap().amount_out >= target);
        let one_less = pair.swap_sell(buy.amount_in - u(1), tx);
        assert!(one_less.map_or(true, |swap| swap.amount_out < target));
    }
    let pair = pool.pair(usdc, weth).unwrap();
    assert_eq!(pair.swap_buy(U256::ZERO, tx).unwrap().amount_in, U256::ZERO);
    assert_eq!(
        pair.swap_sell(U256::ZERO, tx).unwrap().amount_out,
        U256::ZERO
    );
    let limits = pair.get_limits().unwrap();
    assert!(matches!(
        pair.swap_buy(limits.buy + u(1), tx).unwrap_err().rejection(),
        Some(Rejection::AmountTooLarge { limit }) if *limit == limits.buy
    ));
}

#[test]
fn after_a_swap_the_pool_holds_what_the_swap_moved() {
    let (pool, usdc, weth, stock) = deployed_pool();
    let tx = cold(pool.base_fee());
    let balance = |pool: &Pool, token| pool.token(token).unwrap().balance;

    let swap = pool
        .pair(usdc, weth)
        .unwrap()
        .swap_with_extra_fees(u(100_000_000), 5_000, tx)
        .unwrap();
    let after = pool.after(&swap).unwrap();
    assert_eq!(balance(&after, usdc), balance(&pool, usdc) + swap.amount_in);
    assert_eq!(
        balance(&after, weth),
        balance(&pool, weth) - swap.amount_out - swap.fee
    );
    for token in [usdc, weth] {
        let token = after.token(token).unwrap();
        assert_eq!(
            token.recorded_balance, token.balance,
            "a swap records its own two tokens"
        );
    }
    assert_eq!(after.block_number(), pool.block_number());
    // A later swap is priced from the new holdings. Whichever direction adds
    // risk to the portfolio pays for it, so it cannot repeat at the same price.
    let repeats_differ = [
        (usdc, weth, u(100_000_000)),
        (weth, usdc, u(50_000_000_000_000_000)),
    ]
    .into_iter()
    .any(|(token_in, token_out, amount)| {
        let first = pool
            .pair(token_in, token_out)
            .unwrap()
            .maker_swap_exact_input(amount, tx)
            .unwrap();
        let after = pool.after(&first).unwrap();
        let second = after
            .pair(token_in, token_out)
            .unwrap()
            .maker_swap_exact_input(amount, tx)
            .unwrap();
        second.amount_out != first.amount_out
    });
    assert!(repeats_differ);

    // An xStock leaves in whole shares: at least the promised amount, at most
    // the rounding allowance more.
    let swap = pool
        .pair(usdc, stock)
        .unwrap()
        .maker_swap_exact_input(u(50_000_000), tx)
        .unwrap();
    let after = pool.after(&swap).unwrap();
    let left = balance(&pool, stock) - balance(&after, stock);
    assert!(
        left >= swap.amount_out && left <= swap.amount_out + u(3),
        "{left} vs {}",
        swap.amount_out
    );
}

#[test]
fn a_swap_of_zero_changes_nothing_and_any_other_swap_settles() {
    // One WETH sent straight to the pool: held, but not yet recorded.
    let (deployed, usdc, weth, stock) = deployed_pool();
    let index = |token| {
        deployed
            .tokens()
            .iter()
            .position(|t| t.address == token)
            .unwrap()
    };
    let mut snapshot = deployed.snapshot().clone();
    snapshot.balances[index(weth)] += u(1_000_000_000_000_000_000);
    let pool = Pool::new(snapshot).unwrap();
    let held = |pool: &Pool| pool.token(weth).unwrap().balance;
    let recorded = |pool: &Pool| pool.token(weth).unwrap().recorded_balance;
    assert_ne!(held(&pool), recorded(&pool));
    let tx = cold(pool.base_fee());
    let pair = pool.pair(usdc, weth).unwrap();

    // The contract returns from an ERC-7815 swap of zero before settlement,
    // so the pool still has not recorded the WETH it was sent.
    for swap in [
        pair.swap_sell(U256::ZERO, tx).unwrap(),
        pair.swap_buy(U256::ZERO, tx).unwrap(),
    ] {
        assert_eq!((swap.amount_in, swap.amount_out), (U256::ZERO, U256::ZERO));
        assert_eq!(pool.after(&swap).unwrap().snapshot(), pool.snapshot());
    }

    // A fee of 100% leaves the caller nothing, but that swap does settle: the
    // fee leaves the pool, and the pool records what it now holds.
    let swap = pair
        .swap_with_extra_fees(u(100_000_000), 1_000_000, tx)
        .unwrap();
    assert_eq!(swap.amount_out, U256::ZERO);
    let after = pool.after(&swap).unwrap();
    assert_eq!(held(&after), held(&pool) - swap.fee);
    assert_eq!(recorded(&after), held(&after));

    // With nothing sent to the caller, an xStock output loses the fee's
    // shares and no rounding allowance.
    let shares = |pool: &Pool| pool.token(stock).unwrap().xstock.clone().unwrap();
    let swap = pool
        .pair(usdc, stock)
        .unwrap()
        .swap_with_extra_fees(u(50_000_000), 1_000_000, tx)
        .unwrap();
    let before = shares(&pool);
    assert_eq!(
        shares(&pool.after(&swap).unwrap()).shares,
        before.shares - before.shares_for(swap.fee).unwrap()
    );
}

#[test]
fn rejections_say_why() {
    let (pool, usdc, weth, _) = deployed_pool();
    let reason = |result: thogamm::Result<_>| -> Rejection {
        let error: Error = match result {
            Ok(()) => panic!("expected a rejection"),
            Err(error) => error,
        };
        error
            .rejection()
            .cloned()
            .unwrap_or_else(|| panic!("{error}"))
    };
    assert_eq!(
        reason(pool.pair(usdc, usdc).map(drop)),
        Rejection::IdenticalTokens
    );
    let stranger = Address::repeat_byte(7);
    assert_eq!(
        reason(pool.pair(usdc, stranger).map(drop)),
        Rejection::UnknownToken(stranger)
    );
    let pair = pool.pair(usdc, weth).unwrap();
    assert_eq!(
        reason(pair.maker_quote_exact_input(U256::ZERO).map(drop)),
        Rejection::ZeroFill
    );
    let huge = U256::MAX;
    assert!(matches!(
        reason(pair.maker_quote_exact_input(huge).map(drop)),
        Rejection::AmountTooLarge { .. }
    ));
    // getLimits is an upper bound, not a promise: its sell limit is far more
    // than the pool can pay for.
    let limits = pair.get_limits().unwrap();
    assert!(pair.maker_quote_exact_input(limits.sell).is_err());

    let mut paused = pool.snapshot().clone();
    paused.set_field(layout::control::PAUSED, u(1));
    assert_eq!(
        reason(Pool::new(paused).unwrap().pair(usdc, weth).map(drop)),
        Rejection::Paused
    );
}

#[test]
fn state_in_another_format_or_from_mixed_blocks_is_refused() {
    let (pool, ..) = deployed_pool();
    let good: &Snapshot = pool.snapshot();

    let mut older_model = good.clone();
    older_model.set_field(layout::FORMAT, u(8 | 8 << 8 | 2 << 16));
    assert!(matches!(
        Pool::new(older_model),
        Err(Error::UnsupportedFormat(_))
    ));

    let mut mixed = good.clone();
    let stock = mixed
        .xstocks
        .iter()
        .position(|x| x.as_ref().is_some_and(|x| !x.shares.is_zero()))
        .unwrap();
    mixed.xstocks[stock].as_mut().unwrap().multiplier += u(1_000_000_000_000_000);
    assert!(matches!(Pool::new(mixed), Err(Error::InvalidSnapshot(_))));

    let mut short = good.clone();
    short.balances.pop();
    assert!(matches!(Pool::new(short), Err(Error::InvalidSnapshot(_))));
}

#[test]
fn a_prepared_pair_and_a_cloned_pool_can_cross_threads() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<Pool>();
    send_sync::<thogamm::Pair<'_>>();
    let (pool, usdc, weth, _) = deployed_pool();
    let clone = pool.clone();
    assert!(std::ptr::eq(pool.snapshot(), clone.snapshot()));
    let quote = std::thread::spawn(move || {
        clone
            .pair(usdc, weth)
            .unwrap()
            .maker_quote_exact_input(u(1_000_000))
            .unwrap()
    })
    .join()
    .unwrap();
    assert_eq!(
        quote,
        pool.pair(usdc, weth)
            .unwrap()
            .maker_quote_exact_input(u(1_000_000))
            .unwrap()
    );
}
