//! LAM — Linear Amortizer.
//!
//! Run with: `cargo run --example lam_linear_amortizer`
//!
//! The principal is repaid in equal instalments (the same amount of principal every period),
//! and interest is paid on the remaining balance on top. Payments therefore start high and shrink
//! over time. Typical for equipment loans, commercial real-estate loans and many European
//! business loans.
//!
//! Scenario: a leasing company finances a EUR 120,000 machine over 2 years with a fixed 6% rate.
//! The borrower repays EUR 15,000 of principal every quarter plus interest on what is still owed.

#[path = "common/mod.rs"]
mod common;

use actus_rs::state::StateVariable::*;
use actus_rs::EventType;
use common::*;
use serde_json::json;

fn main() -> actus_rs::Result<()> {
    title(
        "LAM: EUR 120k equipment loan, EUR 15k principal per quarter",
        "
        The lender pays out EUR 120,000. Each quarter the borrower repays a fixed EUR 15,000 of
        principal plus 6% interest on the outstanding balance, so the total payment falls over time.
        ",
    );

    let loan = contract(json!({
        "contractType": "LAM",
        "contractID": "equipment-loan-7",
        "contractRole": "RPA", // we are the lender
        "statusDate": "2024-02-28T00:00:00",
        "currency": "EUR",
        "initialExchangeDate": "2024-03-01T00:00:00",
        "maturityDate": "2026-03-01T00:00:00",
        "notionalPrincipal": 120000,
        "nominalInterestRate": 0.06,
        // 30E/360: every month counts as 30 days, so all quarterly interest periods are equal.
        "dayCountConvention": "30E360",
        "cycleAnchorDateOfInterestPayment": "2024-06-01T00:00:00",
        "cycleOfInterestPayment": "P3ML0",
        // Principal redemption (PR) schedule: every quarter, first one on 2024-06-01 ...
        "cycleAnchorDateOfPrincipalRedemption": "2024-06-01T00:00:00",
        "cycleOfPrincipalRedemption": "P3ML0",
        // ... of this fixed amount. 8 quarters x 15,000 = 120,000: fully repaid at maturity.
        "nextPrincipalRedemptionPayment": 15000,
        // Interest is calculated on the current notional (NT). "NTIED" would always use the
        // original amount, "NTL" the notional as of the last IPCB fixing.
        "interestCalculationBase": "NT",
    }));

    let results = loan.project(None, &actus_rs::MarketData::new())?;
    print_table(&results, &[NotionalPrincipal, NextPrincipalRedemptionPayment, AccruedInterest]);

    println!(
        "\nWhat happened:
  * Each quarter has a PR (EUR 15,000 of principal) and an IP. PR runs first: it books the
    interest accrued on the old balance into 'accrued int.', then lowers the notional. The IP
    then pays out exactly that accrued interest, so interest is always on the pre-repayment balance.
  * Interest falls from 1,800 (120,000 x 6% / 4) to 225 (15,000 x 6% / 4) as the balance shrinks.
  * MD collects whatever principal remains (here the last 15,000 instalment).
Principal repaid: {:.2}   Interest received: {:.2}",
        (sum_of(&results, EventType::PR) + sum_of(&results, EventType::MD)).to_f64(),
        sum_of(&results, EventType::IP).to_f64()
    );
    Ok(())
}
