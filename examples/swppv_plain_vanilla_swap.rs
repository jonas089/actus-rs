//! SWPPV — Plain Vanilla Interest Rate Swap.
//!
//! Run with: `cargo run --example swppv_plain_vanilla_swap`
//!
//! Two parties exchange interest payments on the same notional: one pays a fixed rate, the other
//! pays a floating market rate. The notional itself is never exchanged. Companies use it to turn
//! a floating-rate loan into a fixed-rate one (or vice versa); banks use it to manage the rate
//! risk of their balance sheet.
//!
//! Scenario: a company has a USD 10,000,000 floating-rate loan (see `pam_bullet_loan`) and wants
//! certainty. It enters a two-year swap where it *pays fixed* 4.00% and *receives floating*
//! SOFR, both semi-annually. The floating rate for each period is fixed at the start of the
//! period (RR event) and paid at the end (IPFL event).

#[path = "common/mod.rs"]
mod common;

use actus_rs::state::StateVariable::{AccruedInterest2, NominalInterestRate};
use actus_rs::EventType;
use common::*;
use serde_json::json;

fn main() -> actus_rs::Result<()> {
    title(
        "SWPPV: pay 4.00% fixed, receive SOFR, USD 10M, two years",
        "
        Each half-year the company pays the fixed leg (IPFX, negative) and receives the floating
        leg (IPFL, positive). The floating rate for a period is set by the RR at its start.
        ",
    );

    let swap = contract(json!({
        "contractType": "SWPPV",
        "contractID": "irs-4421",
        // PF = pay fixed (and receive floating). RF = receive fixed (and pay floating).
        "contractRole": "PF",
        "statusDate": "2025-01-01T00:00:00",
        "currency": "USD",
        "initialExchangeDate": "2025-01-02T00:00:00",
        "maturityDate": "2027-01-02T00:00:00",
        // The notional the interest is calculated on. It is never paid.
        "notionalPrincipal": 10000000,
        // Fixed-leg rate.
        "nominalInterestRate": 0.04,
        // Floating-leg rate for the first period, before the first reset is applied.
        "nominalInterestRate2": 0.043,
        "dayCountConvention": "A360",
        // Both legs pay every 6 months.
        "cycleAnchorDateOfInterestPayment": "2025-07-02T00:00:00",
        "cycleOfInterestPayment": "P6ML0",
        // The floating rate is re-fixed every 6 months from the start date: SOFR + 0%.
        "cycleAnchorDateOfRateReset": "2025-01-02T00:00:00",
        "cycleOfRateReset": "P6ML0",
        "marketObjectCodeOfRateReset": "USD-SOFR",
        "rateSpread": 0.0,
        // D = both legs are paid gross (two cash flows per date). S = only the net amount.
        "deliverySettlement": "D",
    }));

    let mut market = actus_rs::MarketData::new();
    series(
        &mut market,
        "USD-SOFR",
        &[("2025-01-02", "0.0430"), ("2025-07-02", "0.0405"), ("2026-01-02", "0.0365"), ("2026-07-02", "0.0350")],
    );

    let results = swap.project(None, &market)?;
    print_table(&results, &[NominalInterestRate, AccruedInterest2]);

    println!(
        "\nWhat happened:
  * IED and MD are zero: in an interest rate swap the notional never changes hands.
  * 'rate' is the state's floating rate, re-fixed by each RR; the fixed 4.00% is a term and
    never changes. 'accrued int. 2' is the floating-leg interest accrued just before IPFL.
  * IPFX: pay 10M x 4.00% x days/360. IPFL: receive 10M x SOFR fixed at the period start.
    While SOFR > 4% the company gains on the swap; once SOFR falls below 4% it pays net.
  * Combined with its floating-rate loan, the company's net interest cost is fixed at 4%.
Fixed paid: {:.2}   Floating received: {:.2}   Net: {:.2}",
        sum_of(&results, EventType::IPFX).to_f64(),
        sum_of(&results, EventType::IPFL).to_f64(),
        net(&results).to_f64()
    );
    Ok(())
}
