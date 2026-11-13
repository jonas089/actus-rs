//! SWAPS — Swap (generic, built from two legs).
//!
//! Run with: `cargo run --example swaps_cross_currency_swap`
//!
//! A swap made of two complete child contracts: the "first leg" is received and the "second
//! leg" is paid (for role RFL; PFL is the reverse). Each leg can be any lending contract (PAM,
//! LAM, ANN, ...), with its own currency, rates and schedules, so SWAPS covers what SWPPV cannot:
//! cross-currency swaps, amortizing swaps, basis swaps, swaps with notional exchange.
//!
//! Scenario: a European company issued a USD 10,000,000 bond paying 5% but its income is in
//! euros. It enters a two-year fixed-for-fixed cross-currency swap: it RECEIVES the USD leg
//! (matching its bond: USD 10M at 5%) and PAYS a EUR leg (EUR 9,200,000 at 3.2%). The notionals
//! are exchanged at the start and swapped back at maturity, so the bond is effectively in euros.
//!
//! Both legs are written in full as child contracts in `contractStructure`. Each event in the
//! result carries the ID of the leg it came from.

#[path = "common/mod.rs"]
mod common;

use common::*;
use serde_json::json;

fn main() -> actus_rs::Result<()> {
    title(
        "SWAPS: USD/EUR fixed-for-fixed cross-currency swap with notional exchange",
        "
        Leg 1 (received): USD 10,000,000 at 5.0%, semi-annual.
        Leg 2 (paid):     EUR  9,200,000 at 3.2%, semi-annual.
        ",
    );

    let swap = contract(json!({
        "contractType": "SWAPS",
        "contractID": "ccs-usdeur-01",
        // RFL = receive first leg, pay second leg.
        "contractRole": "RFL",
        "statusDate": "2025-03-01T00:00:00",
        "currency": "USD",
        // D = each leg's cash flows are paid as they are. S would net same-date flows into one
        // payment, which only makes sense when both legs are in the same currency.
        "deliverySettlement": "D",
        "contractStructure": [
            {
                // FIL = first leg. A complete PAM, written from the swap holder's side.
                "referenceType": "CNT",
                "referenceRole": "FIL",
                "object": {
                    "contractType": "PAM",
                    "contractID": "ccs-usd-leg",
                    "statusDate": "2025-03-01T00:00:00",
                    "currency": "USD",
                    "initialExchangeDate": "2025-03-03T00:00:00",
                    "maturityDate": "2027-03-03T00:00:00",
                    "notionalPrincipal": 10000000,
                    "nominalInterestRate": 0.05,
                    "dayCountConvention": "30E360",
                    "cycleAnchorDateOfInterestPayment": "2025-09-03T00:00:00",
                    "cycleOfInterestPayment": "P6ML0",
                }
            },
            {
                // SEL = second leg.
                "referenceType": "CNT",
                "referenceRole": "SEL",
                "object": {
                    "contractType": "PAM",
                    "contractID": "ccs-eur-leg",
                    "statusDate": "2025-03-01T00:00:00",
                    "currency": "EUR",
                    "initialExchangeDate": "2025-03-03T00:00:00",
                    "maturityDate": "2027-03-03T00:00:00",
                    "notionalPrincipal": 9200000,
                    "nominalInterestRate": 0.032,
                    "dayCountConvention": "30E360",
                    "cycleAnchorDateOfInterestPayment": "2025-09-03T00:00:00",
                    "cycleOfInterestPayment": "P6ML0",
                }
            }
        ],
    }));

    let results = swap.project(None, &actus_rs::MarketData::new())?;
    println!("{:<11} {:<5} {:<13} {:>8} {:>16}", "date", "event", "leg", "currency", "cash flow");
    println!("{}", "-".repeat(57));
    for r in &results {
        println!(
            "{:<11} {:<5} {:<13} {:>8} {:>16.2}",
            r.event.time.date(),
            r.event.event_type.to_string(),
            r.event.contract_id,
            r.event.currency,
            r.payoff.to_f64()
        );
    }
    println!(
        "\nWhat happened:
  * IED: the notionals are exchanged at the start. Receiving leg 1 means *lending* USD in the
    leg (we pay USD 10M out); paying leg 2 means *borrowing* EUR (we receive EUR 9.2M). In
    practice the company uses the EUR 9.2M and hands over the USD it raised with its bond.
  * IP: every six months we receive USD 250,000 (10M x 5% / 2) - exactly the bond coupon -
    and pay EUR 147,200 (9.2M x 3.2% / 2).
  * MD: the notionals are swapped back: USD 10M comes in to repay the bond, EUR 9.2M goes out.
  * Net effect: a USD bond turned into a EUR liability at 3.2%, with no currency risk left.
  * Cash flows are kept per currency; adding USD and EUR amounts would need FX rates."
    );
    Ok(())
}
