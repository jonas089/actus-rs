//! FXOUT — Foreign Exchange Outright.
//!
//! Run with: `cargo run --example fxout_fx_forward`
//!
//! An agreement today to exchange two currency amounts on a future date at a rate fixed today
//! (an FX forward; with a short date it is an FX spot trade). It can settle in two ways:
//!   * delivery (`deliverySettlement: D`): both amounts are actually paid, two MD events;
//!   * cash (`deliverySettlement: S`): only the difference in value is paid, one STD event,
//!     computed with the market exchange rate observed at maturity (a non-deliverable forward).
//!
//! Scenario: a US importer must pay a German supplier EUR 1,000,000 in six months. To remove the
//! currency risk it buys EUR forward at 1.0850 USD per EUR: it will pay USD 1,085,000 and receive
//! EUR 1,000,000. At maturity the market rate is 1.1200, so the hedge has gained value.

#[path = "common/mod.rs"]
mod common;

use common::*;
use serde_json::json;

fn terms(delivery: &str) -> serde_json::Value {
    json!({
        "contractType": "FXOUT",
        "contractID": "eur-forward",
        // RPL: from this side we PAY the first currency (USD) and RECEIVE the second (EUR).
        // RPA would be the opposite trade (receive USD, pay EUR).
        "contractRole": "RPL",
        "statusDate": "2025-03-01T00:00:00",
        "contractDealDate": "2025-03-01T00:00:00",
        "maturityDate": "2025-09-01T00:00:00",
        // First leg: USD 1,085,000 ...
        "currency": "USD",
        "notionalPrincipal": 1085000,
        // ... exchanged for EUR 1,000,000. The forward rate is implicit: 1,085,000 / 1,000,000.
        "currency2": "EUR",
        "notionalPrincipal2": 1000000,
        "deliverySettlement": delivery,
        // Cash settlement is paid two business days after maturity (spot convention).
        "settlementPeriod": "P2D",
    })
}

fn main() -> actus_rs::Result<()> {
    title(
        "FXOUT: buying EUR 1,000,000 six months forward at 1.0850",
        "
        The same forward run twice: once with physical delivery, once cash-settled.
        ",
    );

    // EUR/USD observed at maturity: needed only by the cash-settled version. The code is
    // "<currency2>/<currency>", i.e. the price of one EUR in USD.
    let mut market = actus_rs::MarketData::new();
    series(&mut market, "EUR/USD", &[("2025-09-01", "1.12")]);

    println!("--- 1) Physical delivery (deliverySettlement = D) ---");
    let delivered = contract(terms("D")).project(None, &market)?;
    print_table(&delivered, &[]);
    for r in &delivered {
        println!("  {} {} {:>14.2} {}", r.event.time.date(), r.event.event_type, r.payoff.to_f64(), r.event.currency);
    }
    println!(
        "  Two MD events in two currencies: the importer pays USD 1,085,000 and receives
  EUR 1,000,000, exactly what it needs for the supplier, whatever the market rate is.\n"
    );

    println!("--- 2) Cash settlement (deliverySettlement = S) ---");
    let settled = contract(terms("S")).project(None, &market)?;
    print_table(&settled, &[]);
    println!(
        "  One STD in USD = EUR 1,000,000 x 1.1200 - USD 1,085,000 = {:.2}.
  The importer still buys its euros in the market at 1.12 (USD 1,120,000), but the settlement
  gain brings the net cost back to USD 1,085,000: the same economic result as delivery.
  It is paid on 2025-09-03 because of the 2-day settlement period.",
        net(&settled).to_f64()
    );
    Ok(())
}
