//! STK — Stock.
//!
//! Run with: `cargo run --example stk_equity_position`
//!
//! An equity position: bought at a price, pays dividends whose amounts are not known in advance,
//! and is eventually sold. ACTUS does not model the share price itself (that is market data); it
//! models the cash flows of holding the position.
//!
//! Scenario: a fund buys 500 shares at USD 180.00 per share on 2025-01-10. The company pays
//! quarterly dividends; the per-share amounts are read from market data under the code given in
//! `marketObjectCodeOfDividends`. The fund sells the position on 2026-03-20 at USD 212.40.

#[path = "common/mod.rs"]
mod common;

use actus_rs::EventType;
use common::*;
use serde_json::json;

fn main() -> actus_rs::Result<()> {
    title(
        "STK: 500 shares bought at 180.00, quarterly dividends, sold at 212.40",
        "
        PRD pays quantity x purchase price. Each DV receives quantity x the dividend per share
        observed on that date. TD receives quantity x the sale price.
        ",
    );

    let position = contract(json!({
        "contractType": "STK",
        "contractID": "acme-equity",
        "contractRole": "RPA", // we hold the shares (a short position would be RPL)
        "statusDate": "2025-01-01T00:00:00",
        "currency": "USD",
        // Number of shares. Prices and dividends below are per share.
        "quantity": 500,
        "purchaseDate": "2025-01-10T00:00:00",
        "priceAtPurchaseDate": 180.00,
        // Dividends: on this cycle, read the per-share amount from market data series "ACME-DIV".
        "cycleAnchorDateOfDividendPayment": "2025-03-15T00:00:00",
        "cycleOfDividendPayment": "P3ML1",
        "marketObjectCodeOfDividends": "ACME-DIV",
        // Sale of the position. Without it the stock never ends; you would then pass a horizon
        // to `project` or use the lazy `stream`.
        "terminationDate": "2026-03-20T00:00:00",
        "priceAtTerminationDate": 212.40,
    }));

    // Declared dividends per share. The company raised its dividend in 2026.
    let mut market = actus_rs::MarketData::new();
    series(
        &mut market,
        "ACME-DIV",
        &[
            ("2025-03-15", "0.85"),
            ("2025-06-15", "0.85"),
            ("2025-09-15", "0.85"),
            ("2025-12-15", "0.85"),
            ("2026-03-15", "0.95"),
        ],
    );

    let results = position.project(None, &market)?;
    print_table(&results, &[]);

    let bought = sum_of(&results, EventType::PRD);
    let sold = sum_of(&results, EventType::TD);
    let dividends = sum_of(&results, EventType::DV);
    println!(
        "\nWhat happened:
  * PRD: 500 x 180.00 = 90,000 paid for the shares.
  * DV:  500 x 0.85 = 425 per quarter, then 500 x 0.95 = 475 after the raise.
  * TD:  500 x 212.40 = 106,200 received when the position is sold. No dividends after that.
Price gain: {:.2}   Dividends: {:.2}   Total return: {:.2} ({:.2}% on the purchase price)",
        (sold + bought).to_f64(),
        dividends.to_f64(),
        net(&results).to_f64(),
        net(&results).to_f64() / bought.abs().to_f64() * 100.0
    );
    Ok(())
}
