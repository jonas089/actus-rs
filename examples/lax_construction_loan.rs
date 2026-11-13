//! LAX — Exotic Linear Amortizer.
//!
//! Run with: `cargo run --example lax_construction_loan`
//!
//! A LAM whose principal schedule is made of several *segments*, each with its own start date,
//! cycle, amount and direction: INC (the borrower draws more money, a PI event) or DEC (the
//! borrower repays, a PR event). Interest rates can also follow a piecewise schedule. This
//! covers construction loans, step-up/step-down repayment plans and grace periods.
//!
//! Scenario: a developer borrows EUR 1,000,000 to start a building project. Two more tranches of
//! EUR 500,000 are drawn as construction progresses (draw-down phase). After completion the loan
//! amortizes by EUR 250,000 per quarter. The rate is a fixed 5% during construction and steps up
//! to 5.5% once the building is finished.

#[path = "common/mod.rs"]
mod common;

use actus_rs::state::StateVariable::*;
use actus_rs::EventType;
use common::*;
use serde_json::json;

fn main() -> actus_rs::Result<()> {
    title(
        "LAX: EUR construction loan, two draw-downs then quarterly amortization",
        "
        Segment 1 (INC): draw EUR 500,000 every 3 months from 2025-04-01 until segment 2 starts.
        Segment 2 (DEC): repay EUR 250,000 every 3 months from 2025-10-01 until maturity.
        Rate: 5% fixed until 2025-10-01, then 5.5% fixed.
        ",
    );

    let loan = contract(json!({
        "contractType": "LAX",
        "contractID": "construction-12",
        "contractRole": "RPA", // the bank's view
        "statusDate": "2024-12-31T00:00:00",
        "currency": "EUR",
        "initialExchangeDate": "2025-01-01T00:00:00",
        "maturityDate": "2026-10-01T00:00:00",
        // First tranche paid out at IED.
        "notionalPrincipal": 1000000,
        "nominalInterestRate": 0.05,
        "dayCountConvention": "30E360",
        // Principal segments: the i-th entry of each array describes segment i. A segment runs
        // from its anchor date, on its cycle, until the next segment's anchor (or maturity).
        "arrayCycleAnchorDateOfPrincipalRedemption": ["2025-04-01T00:00:00", "2025-10-01T00:00:00"],
        "arrayCycleOfPrincipalRedemption": ["P3ML1", "P3ML1"],
        "arrayNextPrincipalRedemptionPayment": [500000, 250000],
        // INC = principal increase (more money drawn), DEC = principal decrease (repayment).
        "arrayIncreaseDecrease": ["INC", "DEC"],
        // Interest is paid quarterly over the whole life: one segment covering everything.
        "arrayCycleAnchorDateOfInterestPayment": ["2025-04-01T00:00:00"],
        "arrayCycleOfInterestPayment": ["P3ML1"],
        // Rate segments: on 2025-10-01 the rate is reset to a fixed value (F = fixed, RRF
        // event). "V" would instead treat arrayRate as a spread over marketObjectCodeOfRateReset.
        "arrayCycleAnchorDateOfRateReset": ["2025-10-01T00:00:00"],
        "arrayRate": [0.055],
        "arrayFixedVariable": ["F"],
    }));

    let results = loan.project(None, &actus_rs::MarketData::new())?;
    print_table(&results, &[NotionalPrincipal, NominalInterestRate, AccruedInterest]);

    println!(
        "\nWhat happened:
  * IED pays out the first EUR 1,000,000.
  * Two PI events (segment 1) pay out further tranches: the cash flow is negative for the bank
    and the notional grows to EUR 2,000,000.
  * From 2025-10-01, PR events (segment 2) bring EUR 250,000 back each quarter and the RRF
    switches the rate to 5.5%.
  * The IP on each date is computed before the RRF at the same timestamp, so the
    2025-10-01 interest still uses 5%; the 5.5% applies from then on.
  * Segment 2 would need 8 quarters to repay EUR 2,000,000 but maturity comes after 4:
    MD collects the remaining EUR 1,000,000 as a balloon.
Total drawn: {:.2}   Total repaid: {:.2}   Interest: {:.2}",
        (sum_of(&results, EventType::IED) + sum_of(&results, EventType::PI)).abs().to_f64(),
        (sum_of(&results, EventType::PR) + sum_of(&results, EventType::MD)).to_f64(),
        sum_of(&results, EventType::IP).to_f64()
    );
    Ok(())
}
