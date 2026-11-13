//! CAPFL — Cap and Floor.
//!
//! Run with: `cargo run --example capfl_interest_rate_cap`
//!
//! An option on the interest rate of an underlying floating-rate loan. A *cap* pays the holder
//! the interest above a maximum rate (it protects a borrower against rising rates); a *floor*
//! pays the interest below a minimum rate (it protects a lender against falling rates). A
//! *collar* has both. The underlying loan is a complete child contract; the cap pays the
//! difference between the loan's interest and the interest it would pay at the capped rate.
//!
//! Scenario: a property company has a EUR 20,000,000 one-year loan at 3-month EURIBOR + 1.5%,
//! reset quarterly. It buys a 4.5% cap on the loan's all-in rate. In 2025 rates spike, the loan
//! rate goes above 4.5% for some quarters and the cap pays out the excess.

#[path = "common/mod.rs"]
mod common;

use actus_rs::EventType;
use common::*;
use serde_json::json;

fn main() -> actus_rs::Result<()> {
    title(
        "CAPFL: a 4.5% cap on a EUR 20M EURIBOR + 1.5% loan",
        "
        For each interest period: cap payout = notional x max(loan rate - 4.5%, 0) x year fraction.
        ",
    );

    let cap = contract(json!({
        "contractType": "CAPFL",
        "contractID": "cap-prop-co",
        // BUY: we hold the cap and receive its payouts. SEL would be the bank that wrote it.
        "contractRole": "BUY",
        "statusDate": "2024-12-31T00:00:00",
        "currency": "EUR",
        // The maximum rate. Use "lifeFloor" for a floor, or both for a collar.
        "lifeCap": 0.045,
        "contractStructure": [{
            // UDL = the underlying contract whose interest the cap refers to.
            "referenceType": "CNT",
            "referenceRole": "UDL",
            "object": {
                "contractType": "PAM",
                "contractID": "prop-co-loan",
                "statusDate": "2024-12-31T00:00:00",
                "currency": "EUR",
                "initialExchangeDate": "2025-01-02T00:00:00",
                "maturityDate": "2026-01-02T00:00:00",
                "notionalPrincipal": 20000000,
                "nominalInterestRate": 0.0425,
                "dayCountConvention": "A360",
                "cycleAnchorDateOfInterestPayment": "2025-04-02T00:00:00",
                "cycleOfInterestPayment": "P3ML0",
                "cycleAnchorDateOfRateReset": "2025-04-02T00:00:00",
                "cycleOfRateReset": "P3ML0",
                "marketObjectCodeOfRateReset": "EURIBOR-3M",
                "rateSpread": 0.015,
            }
        }],
    }));

    // EURIBOR fixings: the loan rate is EURIBOR + 1.5%.
    let mut market = actus_rs::MarketData::new();
    series(&mut market, "EURIBOR-3M", &[("2025-04-02", "0.0340"), ("2025-07-02", "0.0285"), ("2025-10-02", "0.0225")]);

    let results = cap.project(None, &market)?;
    print_table(&results, &[]);

    println!(
        "\nThe loan's rate per quarter (EURIBOR + 1.5%):
  2025-01-02 .. 04-02: 4.25% (initial rate)       -> below the cap, payout 0
  2025-04-02 .. 07-02: 3.40% + 1.5% = 4.90%       -> cap pays 20M x 0.40% x 91/360
  2025-07-02 .. 10-02: 2.85% + 1.5% = 4.35%       -> below the cap, payout 0
  2025-10-02 .. 26-01-02: 2.25% + 1.5% = 3.75%    -> below the cap, payout 0

What happened:
  * The cap's events follow the underlying loan's interest dates (IP). The payout is the
    difference between the loan's interest and the interest at the capped rate.
  * Only the quarter at 4.90% pays out. Combined with the loan, the company never pays more
    than 4.5% on its debt. (The premium paid for the cap would be a PRD event if
    purchaseDate / priceAtPurchaseDate were given.)
Total cap payouts: {:.2}",
        sum_of(&results, EventType::IP).to_f64()
    );
    Ok(())
}
