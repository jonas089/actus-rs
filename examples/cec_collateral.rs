//! CEC — Credit Enhancement Collateral.
//!
//! Run with: `cargo run --example cec_collateral`
//!
//! Collateral pledged against a covered contract. If the covered contract defaults, the
//! collateral (the *covering* contracts) is liquidated at its market value, up to the covered
//! exposure: the lender cannot take more than it is owed. Used for secured loans, Lombard
//! loans, repo-like financing and gold- or securities-backed lending.
//!
//! Scenario: a private bank lends USD 1,500,000 against 800 troy ounces of gold. The covered
//! contract (COVE) is the loan, a complete PAM; the covering contract (COVI) is the gold, a
//! complete COM whose value is quantity x the gold price observed under its marketObjectCode.
//! We run the same arrangement in two worlds:
//!   1) the gold price is high when the borrower defaults: the bank recovers its full exposure;
//!   2) the gold price has crashed: the bank recovers only what the gold is worth.

#[path = "common/mod.rs"]
mod common;

use actus_rs::state::StateVariable::*;
use actus_rs::terms::ContractPerformance;
use common::*;
use serde_json::json;

fn arrangement() -> serde_json::Value {
    json!({
        "contractType": "CEC",
        "contractID": "lombard-77",
        // BUY: we (the bank) hold the collateral rights.
        "contractRole": "BUY",
        "statusDate": "2025-01-01T00:00:00",
        "currency": "USD",
        // The exposure that is secured: here the loan's notional (NO). NI would add accrued
        // interest. coverageOfCreditEnhancement (default 1) scales it.
        "guaranteedExposure": "NO",
        "creditEventTypeCovered": ["DF"],
        // The collateral is sold and the proceeds paid 5 days after the default.
        "settlementPeriod": "P5D",
        "contractStructure": [
            {
                // COVE: the covered loan.
                "referenceType": "CNT",
                "referenceRole": "COVE",
                "object": {
                    "contractType": "PAM",
                    "contractID": "lombard-loan-77",
                    "contractRole": "RPA",
                    "statusDate": "2025-01-01T00:00:00",
                    "currency": "USD",
                    "initialExchangeDate": "2025-01-02T00:00:00",
                    "maturityDate": "2027-01-02T00:00:00",
                    "notionalPrincipal": 1500000,
                    "nominalInterestRate": 0.055,
                    "dayCountConvention": "A360",
                    "cycleAnchorDateOfInterestPayment": "2025-04-02T00:00:00",
                    "cycleOfInterestPayment": "P3ML0",
                }
            },
            {
                // COVI: the collateral, 800 oz of gold valued at the "XAU-USD" price.
                "referenceType": "CNT",
                "referenceRole": "COVI",
                "object": {
                    "contractType": "COM",
                    "contractID": "pledged-gold-77",
                    "contractRole": "RPA",
                    "statusDate": "2025-01-01T00:00:00",
                    "currency": "USD",
                    "quantity": 800,
                    "unit": "TOZ",
                    "marketObjectCode": "XAU-USD",
                }
            }
        ],
    })
}

fn run(label: &str, gold_price: &str) -> actus_rs::Result<()> {
    let mut market = actus_rs::MarketData::new();
    // Gold price on the default date (used to value the collateral at exercise).
    series(&mut market, "XAU-USD", &[("2025-11-10", gold_price)]);
    // The borrower defaults: a credit event observed on the covered loan.
    credit_event(&mut market, "lombard-loan-77", "2025-11-10", ContractPerformance::DF);

    println!("--- {label}: gold at USD {gold_price}/oz, collateral worth USD {:.2} ---", 800.0 * dec(gold_price).to_f64());
    let results = contract(arrangement()).project(None, &market)?;
    print_table(&results, &[NotionalPrincipal, ExerciseAmount]);
    println!();
    Ok(())
}

fn main() -> actus_rs::Result<()> {
    title(
        "CEC: USD 1.5M Lombard loan secured by 800 oz of gold",
        "
        On default (XD): exercise amount = min(collateral value, covered exposure).
        STD pays it out 5 days later. Without a default the arrangement just ends at maturity (MD).
        ",
    );
    run("1) Default while gold is high", "2400")?;
    run("2) Default after a gold crash", "1650")?;
    println!(
        "What happened:
  * 'notional' at XD is the covered exposure: the loan's USD 1,500,000.
  * World 1: the gold is worth 800 x 2,400 = 1,920,000 > 1,500,000, so the bank takes exactly
    1,500,000 from the sale; the remaining 420,000 belongs to the borrower.
  * World 2: the gold is worth only 800 x 1,650 = 1,320,000 < 1,500,000, so the bank recovers
    1,320,000 and is left with an unsecured claim of 180,000.
  * This is why lenders set a loan-to-value limit and margin calls: the same contract terms give
    a full or a partial recovery depending only on market data."
    );
    Ok(())
}
