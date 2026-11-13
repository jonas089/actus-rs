//! ANN — Annuity.
//!
//! Run with: `cargo run --example ann_annuity_mortgage`
//!
//! The classic mortgage or consumer loan: a constant total payment (interest + principal) that is
//! calculated so the loan is exactly paid off at maturity. Early payments are mostly interest,
//! later ones mostly principal. Unlike NAM, the payment is *recalculated* whenever the rate
//! changes, so the loan still ends at zero. ACTUS models this recalculation with a PRF event
//! ("principal redemption amount fixing").
//!
//! Scenario: a homeowner borrows GBP 250,000 over 5 years with quarterly payments. The rate is
//! fixed at 4.5% for the first year, then resets yearly to the Bank of England base rate + 1.25%.

#[path = "common/mod.rs"]
mod common;

use actus_rs::state::StateVariable::*;
use actus_rs::EventType;
use common::*;
use serde_json::json;

fn main() -> actus_rs::Result<()> {
    title(
        "ANN: GBP 250k five-year mortgage, quarterly annuity payments",
        "
        The quarterly payment is computed so that, at the current rate, the loan is paid off exactly
        at maturity. After every rate reset the payment is recomputed (PRF event).
        ",
    );

    let mortgage = contract(json!({
        "contractType": "ANN",
        "contractID": "mortgage-55",
        "contractRole": "RPA", // the bank's view
        "statusDate": "2025-01-01T00:00:00",
        "currency": "GBP",
        "initialExchangeDate": "2025-01-15T00:00:00",
        "maturityDate": "2030-01-15T00:00:00",
        "notionalPrincipal": 250000,
        "nominalInterestRate": 0.045,
        "dayCountConvention": "30E360",
        "cycleAnchorDateOfInterestPayment": "2025-04-15T00:00:00",
        "cycleOfInterestPayment": "P3ML0",
        "cycleAnchorDateOfPrincipalRedemption": "2025-04-15T00:00:00",
        "cycleOfPrincipalRedemption": "P3ML0",
        // No "nextPrincipalRedemptionPayment": for ANN it is optional and, when missing, the
        // engine computes the annuity amount A = N x r / (1 - (1 + r)^-n) over the remaining
        // periods. Supplying it would fix the instalment instead.
        "interestCalculationBase": "NT",
        "cycleAnchorDateOfRateReset": "2026-01-15T00:00:00",
        "cycleOfRateReset": "P1YL1",
        "marketObjectCodeOfRateReset": "BOE-BASE",
        "rateSpread": 0.0125,
    }));

    let mut market = actus_rs::MarketData::new();
    series(
        &mut market,
        "BOE-BASE",
        &[("2026-01-15", "0.0400"), ("2027-01-15", "0.0350"), ("2028-01-15", "0.0300"), ("2029-01-15", "0.0300")],
    );

    let results = mortgage.project(None, &market)?;
    print_table(&results, &[NotionalPrincipal, NominalInterestRate, NextPrincipalRedemptionPayment]);

    println!(
        "\nWhat happened:
  * 'next payment' is the total quarterly instalment (PR + IP). PR + IP on each date add up to it.
  * The first PRF (the day before the first repayment) fixes the initial instalment. After each RR, PRF recomputes the
    instalment for the new rate over the remaining quarters, so the loan still ends at exactly 0.
  * Early on the IP share is large; with every payment the notional falls, so the IP share
    shrinks and the PR share grows.
  * On the last date the final IP and the MD (the remaining notional) again add up to about one
    instalment: the loan ends at exactly 0.
Total interest paid by the borrower: {:.2}",
        sum_of(&results, EventType::IP).to_f64()
    );
    Ok(())
}
