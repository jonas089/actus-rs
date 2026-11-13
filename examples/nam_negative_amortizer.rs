//! NAM — Negative Amortizer.
//!
//! Run with: `cargo run --example nam_negative_amortizer`
//!
//! The borrower pays a fixed total amount every period. That amount covers the interest first; only
//! what is left over repays principal. If the rate goes up the interest eats a larger share of the
//! payment and the loan pays down more slowly; if the interest ever exceeds the payment the balance
//! *grows* ("negative amortization"). Whatever is still owed at maturity is due as a balloon.
//! Typical for adjustable-rate mortgages with payment caps and some student loans.
//!
//! Scenario: a USD 100,000 two-year loan with a fixed USD 3,000 monthly payment. The rate starts
//! at 5% and resets every 6 months to the 1-year Treasury rate plus 1.5%. Rates rise sharply,
//! so less principal is repaid each month and a balloon remains at maturity.

#[path = "common/mod.rs"]
mod common;

use actus_rs::state::StateVariable::*;
use actus_rs::EventType;
use common::*;
use serde_json::json;

fn main() -> actus_rs::Result<()> {
    title(
        "NAM: USD 100k loan, fixed USD 3,000 monthly payment, floating rate",
        "
        Each month the borrower pays exactly USD 3,000 (interest + principal together). The rate
        floats, so the split between interest and principal moves, while the payment stays fixed.
        ",
    );

    let loan = contract(json!({
        "contractType": "NAM",
        "contractID": "arm-2291",
        "contractRole": "RPA", // lender's view
        "statusDate": "2024-12-31T00:00:00",
        "currency": "USD",
        "initialExchangeDate": "2025-01-01T00:00:00",
        "maturityDate": "2027-01-01T00:00:00",
        "notionalPrincipal": 100000,
        "nominalInterestRate": 0.05,
        "dayCountConvention": "30E360",
        // Interest and principal are both due monthly.
        "cycleAnchorDateOfInterestPayment": "2025-02-01T00:00:00",
        "cycleOfInterestPayment": "P1ML0",
        "cycleAnchorDateOfPrincipalRedemption": "2025-02-01T00:00:00",
        "cycleOfPrincipalRedemption": "P1ML0",
        // For NAM this is the TOTAL payment per period (principal + interest), not just principal.
        // Each PR repays 3,000 minus the interest accrued since the last payment.
        "nextPrincipalRedemptionPayment": 3000,
        "interestCalculationBase": "NT",
        // Rate resets every 6 months: new rate = UST-1Y + 1.5%.
        "cycleAnchorDateOfRateReset": "2025-07-01T00:00:00",
        "cycleOfRateReset": "P6ML1",
        "marketObjectCodeOfRateReset": "UST-1Y",
        "rateSpread": 0.015,
    }));

    let mut market = actus_rs::MarketData::new();
    series(
        &mut market,
        "UST-1Y",
        &[("2025-07-01", "0.045"), ("2026-01-01", "0.065"), ("2026-07-01", "0.085")],
    );

    let results = loan.project(None, &market)?;
    print_table(&results, &[NotionalPrincipal, NominalInterestRate, NextPrincipalRedemptionPayment]);

    let balloon = results.iter().find(|r| r.event.event_type == EventType::MD).map(|r| r.payoff);
    println!(
        "\nWhat happened:
  * Every month PR + IP together add up to exactly 3,000: the IP is the interest for the month
    and the PR is the remainder, which reduces the notional.
  * After each RR the rate rises (6% -> 8% -> 10%), the IP part grows and the PR part shrinks,
    so the balance falls more slowly.
  * At maturity the remaining balance is repaid in one balloon payment (MD): {:.2}.
Interest received: {:.2}",
        balloon.unwrap_or_default().to_f64(),
        sum_of(&results, EventType::IP).to_f64()
    );
    Ok(())
}
