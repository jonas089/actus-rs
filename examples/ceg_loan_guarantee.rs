//! CEG — Credit Enhancement Guarantee.
//!
//! Run with: `cargo run --example ceg_loan_guarantee`
//!
//! A guarantee that pays out if a covered contract defaults. The covered contracts are complete
//! child contracts (or IDs of contracts held elsewhere). When one of them suffers a credit event
//! of a covered type, the guarantee is exercised (XD): the payout is the covered *exposure* at
//! that moment times the coverage ratio, and it is paid after the settlement period (STD). The
//! guarantor can charge a premium up front (PRD) and/or a running fee (FP). Used for
//! government-backed SME loans, export credit insurance, parent-company guarantees and
//! letters of credit.
//!
//! Scenario: a bank lends EUR 2,000,000 to a small company for three years. A public development
//! agency guarantees 80% of the outstanding principal plus accrued interest, for a fee of 0.5%
//! per year paid quarterly. In month 20 the company defaults. The default is a *credit event*
//! observed on the covered loan, supplied like any other observation.

#[path = "common/mod.rs"]
mod common;

use actus_rs::state::StateVariable::*;
use actus_rs::terms::ContractPerformance;
use actus_rs::EventType;
use common::*;
use serde_json::json;

fn main() -> actus_rs::Result<()> {
    title(
        "CEG: 80% state guarantee on a EUR 2M SME loan, default after 20 months",
        "
        Exposure = outstanding notional + accrued interest of the loan (guaranteedExposure NI).
        Payout   = 80% of the exposure at the default date, plus any fee accrued at that point.
        ",
    );

    let covered_loan = json!({
        "contractType": "LAM",
        "contractID": "sme-loan-311",
        "contractRole": "RPA",
        "statusDate": "2025-01-01T00:00:00",
        "currency": "EUR",
        "initialExchangeDate": "2025-01-02T00:00:00",
        "maturityDate": "2028-01-02T00:00:00",
        "notionalPrincipal": 2000000,
        "nominalInterestRate": 0.065,
        "dayCountConvention": "30E360",
        "cycleAnchorDateOfInterestPayment": "2025-04-02T00:00:00",
        "cycleOfInterestPayment": "P3ML0",
        "cycleAnchorDateOfPrincipalRedemption": "2025-04-02T00:00:00",
        "cycleOfPrincipalRedemption": "P3ML0",
        "nextPrincipalRedemptionPayment": 166666.67,
    });

    let guarantee = contract(json!({
        "contractType": "CEG",
        "contractID": "agency-guarantee-311",
        // We are the bank: we BUY the protection (the agency is the SEL side).
        "contractRole": "BUY",
        "statusDate": "2025-01-01T00:00:00",
        "currency": "EUR",
        "maturityDate": "2028-01-02T00:00:00",
        // An upfront arrangement fee, paid when the guarantee is put in place (PRD). PRD also
        // fixes the guaranteed notional from the loan, which is paid out the day before.
        "purchaseDate": "2025-01-03T00:00:00",
        "priceAtPurchaseDate": 2000,
        // COVE = covered contract. Here a full child contract, so the engine can compute its
        // exposure at any date. A "CID" reference to a contract ID would need the exposure
        // supplied as notionalPrincipal instead.
        "contractStructure": [{ "referenceType": "CNT", "referenceRole": "COVE", "object": covered_loan }],
        // What is guaranteed: NO = notional only, NI = notional + accrued interest,
        // MV = market value (observed under the covered contract's marketObjectCode).
        "guaranteedExposure": "NI",
        // Share of the exposure that is covered.
        "coverageOfCreditEnhancement": 0.8,
        // Which credit events trigger the guarantee. DF = default; DL/DQ = delayed/delinquent.
        "creditEventTypeCovered": ["DF"],
        // Guarantee fee: 0.5% per year of the guaranteed notional, paid quarterly.
        "feeRate": 0.005,
        "feeBasis": "N",
        "cycleAnchorDateOfFee": "2025-04-02T00:00:00",
        "cycleOfFee": "P3ML0",
        // The agency pays 30 days after the default is declared.
        "settlementPeriod": "P30D",
    }));

    // The default: an observed credit event on the covered loan (not on the guarantee).
    let mut market = actus_rs::MarketData::new();
    credit_event(&mut market, "sme-loan-311", "2026-09-15", ContractPerformance::DF);

    let results = guarantee.project(None, &market)?;
    print_table(&results, &[NotionalPrincipal, FeeAccrued, ExerciseAmount]);

    let payout = sum_of(&results, EventType::STD);
    println!(
        "\nWhat happened:
  * PRD: the arrangement fee. 'notional' is the guaranteed amount: 80% of the loan's
    exposure, refreshed at every event, so it falls as the loan amortizes.
  * FP: the running fee, 0.5% p.a. on the guaranteed notional over each quarter.
    Note the sign: the official ACTUS test cases report fees as received by the BUY role, the
    same side that receives the payout. This engine follows the tests (see
    docs/notes/derivatives.md); economically the protection buyer pays the fee.
  * XD on 2026-09-15: the loan defaulted. On that date the loan had 1,166,666.66 outstanding
    (5 of 12 instalments repaid) plus interest accrued since 2026-07-02; the guarantee fixes
    80% of that as the exercise amount. No fees are charged after the exercise.
  * STD 30 days later: {:.2} is settled = the exercise amount plus the fee accrued between
    the last FP and the default (STD settles the fee account too). The bank's loss on the loan
    is limited to the uncovered 20%.",
        payout.to_f64()
    );
    Ok(())
}
