//! CSH — Cash.
//!
//! Run with: `cargo run --example csh_cash_position`
//!
//! The simplest ACTUS contract: an amount of money in one currency. It has no schedule and no
//! cash flows of its own; its state is just the balance. It exists so that a portfolio can hold
//! cash next to loans, bonds and derivatives and be valued and projected uniformly. The only
//! events are monitoring events (AD) that you request, e.g. at reporting dates.
//!
//! Scenario: a corporate treasury holds EUR 2,000,000 at its bank. It has also placed a
//! three-month term deposit (a PAM where the treasury is the lender) and issued a six-month
//! commercial paper (a PAM where the treasury is the borrower). We project all three and build a
//! simple liquidity ladder: the cash balance over time once the other contracts' cash flows land.

#[path = "common/mod.rs"]
mod common;

use actus_rs::state::StateVariable::*;
use actus_rs::EventType;
use common::*;
use serde_json::json;

fn main() -> actus_rs::Result<()> {
    title(
        "CSH: a treasury's cash position as the base of a liquidity ladder",
        "
        CSH holds the opening balance. Month-end monitoring events (AD) show that its state never
        changes by itself; the cash flows of the other contracts are what move money.
        ",
    );

    let cash = contract(json!({
        "contractType": "CSH",
        "contractID": "cash-eur-main",
        // RPA: the cash is an asset. An overdraft would be RPL with a negative balance.
        "contractRole": "RPA",
        "statusDate": "2025-01-01T00:00:00",
        "currency": "EUR",
        "notionalPrincipal": 2000000,
    }));

    // AD (monitoring) events are requested by the caller, like observations.
    let mut market = actus_rs::MarketData::new();
    for d in ["2025-01-31", "2025-02-28", "2025-03-31", "2025-04-30", "2025-05-31", "2025-06-30"] {
        observed_event(&mut market, "cash-eur-main", EventType::AD, d, "0");
    }
    let cash_results = cash.project(Some(date("2025-06-30")), &market)?;
    print_table(&cash_results, &[NotionalPrincipal]);

    // The other two positions.
    let deposit = contract(json!({
        "contractType": "PAM", "contractID": "term-deposit", "contractRole": "RPA",
        "statusDate": "2025-01-01T00:00:00", "currency": "EUR",
        "initialExchangeDate": "2025-01-15T00:00:00", "maturityDate": "2025-04-15T00:00:00",
        "notionalPrincipal": 1500000, "nominalInterestRate": 0.028, "dayCountConvention": "A360",
        "cycleAnchorDateOfInterestPayment": "2025-04-15T00:00:00", "cycleOfInterestPayment": "P3ML0",
    }));
    let paper = contract(json!({
        "contractType": "PAM", "contractID": "commercial-paper", "contractRole": "RPL",
        "statusDate": "2025-01-01T00:00:00", "currency": "EUR",
        "initialExchangeDate": "2025-02-01T00:00:00", "maturityDate": "2025-08-01T00:00:00",
        "notionalPrincipal": 1000000, "nominalInterestRate": 0.031, "dayCountConvention": "A360",
        "cycleAnchorDateOfInterestPayment": "2025-08-01T00:00:00", "cycleOfInterestPayment": "P6ML0",
    }));

    // Merge all cash flows in time order and run the balance forward from the CSH state.
    let mut flows: Vec<(Time, &str, EventType, F)> = Vec::new();
    for (name, c) in [("term deposit", &deposit), ("commercial paper", &paper)] {
        for r in c.project(None, &market)? {
            if !r.payoff.is_zero() {
                flows.push((r.event.time, name, r.event.event_type, r.payoff));
            }
        }
    }
    flows.sort_by_key(|f| f.0);

    let mut balance = cash_results[0].state.number(NotionalPrincipal).unwrap_or_default();
    println!("\nLiquidity ladder (opening balance {:.2}):", balance.to_f64());
    println!("{:<11} {:<17} {:<5} {:>14} {:>14}", "date", "contract", "event", "cash flow", "balance");
    for (t, name, ty, amount) in flows {
        balance += amount;
        println!("{:<11} {:<17} {:<5} {:>14.2} {:>14.2}", t.date(), name, ty.to_string(), amount.to_f64(), balance.to_f64());
    }
    println!(
        "\nWhat happened:
  * The CSH contract itself only reports its balance at each AD; cash never earns or pays
    anything inside ACTUS (an interest-bearing account would be a UMP).
  * Placing the deposit takes 1,500,000 out, issuing the paper brings 1,000,000 in; both come
    back with interest at maturity. The ladder shows the lowest balance and when it occurs,
    which is what a treasurer checks against the overdraft limit."
    );
    Ok(())
}
