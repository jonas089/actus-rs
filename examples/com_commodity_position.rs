//! COM — Commodity.
//!
//! Run with: `cargo run --example com_commodity_position`
//!
//! A position in a physical commodity (gold, oil, wheat, ...). Like STK it has a purchase and a
//! sale but, unlike shares, a commodity pays nothing while you hold it. Its main use in ACTUS is
//! as a building block: as the underlying of options and futures, or as collateral (see the
//! `cec_collateral` example).
//!
//! Scenario: a jeweller buys 100 troy ounces of gold at USD 2,050 per ounce as inventory hedge and
//! sells it a year later at USD 2,410. Nothing happens in between; to show how a portfolio system
//! would mark the holding to market, we value it at quarter ends from a gold price series.

#[path = "common/mod.rs"]
mod common;

use common::*;
use serde_json::json;

fn main() -> actus_rs::Result<()> {
    title(
        "COM: 100 oz of gold, bought at 2,050 and sold at 2,410",
        "
        The contract's only cash flows are the purchase (PRD) and the sale (TD). The holding's
        market value in between is a valuation question, answered from market data, not by ACTUS.
        ",
    );

    let gold = contract(json!({
        "contractType": "COM",
        "contractID": "gold-inventory",
        "contractRole": "RPA", // long the commodity
        "statusDate": "2025-01-01T00:00:00",
        "currency": "USD",
        // How many units, and what a unit is (informational).
        "quantity": 100,
        "unit": "TOZ",
        // The market object to use when valuing the commodity (here: a gold price series).
        "marketObjectCode": "XAU-USD",
        "purchaseDate": "2025-01-06T00:00:00",
        "priceAtPurchaseDate": 2050,
        "terminationDate": "2026-01-06T00:00:00",
        "priceAtTerminationDate": 2410,
    }));

    let mut market = actus_rs::MarketData::new();
    let quarter_ends = [("2025-03-31", "2140"), ("2025-06-30", "2290"), ("2025-09-30", "2205"), ("2025-12-31", "2380")];
    series(&mut market, "XAU-USD", &quarter_ends);

    let results = gold.project(None, &market)?;
    print_table(&results, &[]);

    println!("\nMark-to-market at the monitoring dates (quantity x observed gold price):");
    for (d, price) in quarter_ends {
        let value = dec("100") * dec(price);
        let pnl = value - dec("205000");
        println!("  {d}: value {:>12.2}   unrealized P&L {:>10.2}", value.to_f64(), pnl.to_f64());
    }
    println!(
        "\nWhat happened:
  * PRD: 100 x 2,050 = 205,000 paid.
  * No cash flows while holding gold: no coupons, no dividends (storage costs are not modelled).
  * TD:  100 x 2,410 = 241,000 received.
Realized gain: {:.2}",
        net(&results).to_f64()
    );
    Ok(())
}
