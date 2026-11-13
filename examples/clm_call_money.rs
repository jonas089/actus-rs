//! CLM — Call Money.
//!
//! Run with: `cargo run --example clm_call_money`
//!
//! A loan with no fixed maturity: it runs until one side *calls* it, and is then repaid after a
//! notice period. Interest is typically capitalized or paid on a cycle and the rate floats with
//! the overnight market. Used for interbank call money, margin loans, and demand loans between
//! group companies.
//!
//! Scenario: a treasury lends CHF 5,000,000 to a subsidiary as call money on 2025-01-06. Interest
//! at the SARON overnight rate + 0.25% (reset monthly) is capitalized every month: it is added to
//! the loan instead of being paid, as is usual for ACTUS call money. On 2025-05-20 the treasury
//! calls the loan; with a 31-day notice period it is repaid on 2025-06-20.
//!
//! The call is not known in advance, so it is fed to the engine as an *observed event* (XD),
//! exactly like a market observation. Re-running the same contract with a different call date
//! (or none) gives a different cash-flow profile.

#[path = "common/mod.rs"]
mod common;

use actus_rs::state::StateVariable::*;
use actus_rs::EventType;
use common::*;
use serde_json::json;

fn main() -> actus_rs::Result<()> {
    title(
        "CLM: CHF 5M call money, called on 2025-05-20 with 31 days notice",
        "
        No maturity date is agreed. The lender calls the money on 2025-05-20; repayment follows
        31 days later. Interest is capitalized monthly until the call and paid out at the end.
        ",
    );

    let call_money = contract(json!({
        "contractType": "CLM",
        "contractID": "callmoney-sub-ch",
        "contractRole": "RPA", // lender's view
        "statusDate": "2025-01-05T00:00:00",
        "currency": "CHF",
        "initialExchangeDate": "2025-01-06T00:00:00",
        // No maturityDate: the contract ends only when it is called.
        "notionalPrincipal": 5000000,
        "nominalInterestRate": 0.0075,
        // Actual/365 is common for CHF money-market loans.
        "dayCountConvention": "A365",
        "cycleAnchorDateOfInterestPayment": "2025-02-06T00:00:00",
        "cycleOfInterestPayment": "P1ML1",
        // For CLM, this cycle *capitalizes* interest (IPCI events) rather than paying it.
        // Rate reset monthly to SARON + 0.25%.
        "cycleAnchorDateOfRateReset": "2025-02-06T00:00:00",
        "cycleOfRateReset": "P1ML1",
        "marketObjectCodeOfRateReset": "CHF-SARON",
        "rateSpread": 0.0025,
        // Notice period between the call and repayment.
        "xDayNotice": "P31D",
    }));

    let mut market = actus_rs::MarketData::new();
    series(
        &mut market,
        "CHF-SARON",
        &[("2025-02-06", "0.0045"), ("2025-03-06", "0.0045"), ("2025-04-06", "0.0020"), ("2025-05-06", "0.0020"), ("2025-06-06", "0.0000")],
    );
    // The call: an observed exercise (XD) event for this contract.
    observed_event(&mut market, "callmoney-sub-ch", EventType::XD, "2025-05-20", "0");

    let results = call_money.project(None, &market)?;
    print_table(&results, &[NotionalPrincipal, NominalInterestRate, AccruedInterest]);

    println!(
        "\nWhat happened:
  * Monthly IPCI events add the month's interest to the notional (no cash moves); RR events
    move the rate with SARON (0.75% initially, then 0.70%, 0.45%).
  * XD on 2025-05-20 is the call. Nothing is paid yet, but the repayment date is now fixed:
    2025-05-20 + 31 days = 2025-06-20. No more cycle events are generated after the call.
  * On 2025-06-20 the IP pays the interest accrued since the last IPCI and STD (settlement)
    repays the notional, which includes all capitalized interest. Without the XD event the
    contract would simply run on; its schedule would then need an analysis horizon.
Total received: {:.2} on a CHF 5,000,000 loan, i.e. {:.2} of interest",
        (sum_of(&results, EventType::IP) + sum_of(&results, EventType::STD)).to_f64(),
        net(&results).to_f64()
    );
    Ok(())
}
