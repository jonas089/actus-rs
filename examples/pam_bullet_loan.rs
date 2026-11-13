//! PAM — Principal At Maturity (a "bullet" loan or bond).
//!
//! Run with: `cargo run --example pam_bullet_loan`
//!
//! The simplest lending contract: the full principal is lent at the start, only interest is paid
//! during the life of the loan, and the whole principal comes back in one payment at maturity.
//! Most corporate and government bonds, term deposits and bullet loans are PAMs.
//!
//! Scenario: a bank lends a company USD 1,000,000 for 3 years. Interest is paid quarterly. The
//! rate floats: it starts at 7.30% and is reset once a year to the then-current SOFR rate plus a
//! 2.00% spread. We supply the SOFR observations the reset events need as market data.

#[path = "common/mod.rs"]
mod common;

use actus_rs::state::StateVariable::*;
use actus_rs::EventType;
use common::*;
use serde_json::json;

fn main() -> actus_rs::Result<()> {
    title(
        "PAM: USD 1M three-year floating-rate bullet loan",
        "
        A bank lends USD 1,000,000 to a company. The company pays interest every quarter and
        repays the full amount after three years. The rate is SOFR + 2%, reset every year.
        ",
    );

    let loan = contract(json!({
        "contractType": "PAM",
        "contractID": "loan-001",
        // Whose point of view the cash flows are from. RPA = "real position asset": we are the
        // lender, so we pay out the principal (negative) and receive interest (positive).
        // RPL would be the borrower's view with all signs flipped.
        "contractRole": "RPA",
        // The date the contract is evaluated from. Everything before this is history.
        "statusDate": "2024-01-01T00:00:00",
        "currency": "USD",
        // Initial exchange: the principal is paid out to the borrower on this date.
        "initialExchangeDate": "2024-01-02T00:00:00",
        "maturityDate": "2027-01-02T00:00:00",
        "notionalPrincipal": 1000000,
        // Rate for the first period (as a decimal: 0.073 = 7.30% per year).
        "nominalInterestRate": 0.073,
        // How a period's year fraction is counted. A360 = actual days / 360 (money markets).
        "dayCountConvention": "A360",
        // Interest is paid every 3 months, starting 3 months after the loan is paid out.
        "cycleAnchorDateOfInterestPayment": "2024-04-02T00:00:00",
        "cycleOfInterestPayment": "P3ML0",
        // The rate is reset every 12 months, first one year after the start.
        "cycleAnchorDateOfRateReset": "2025-01-02T00:00:00",
        "cycleOfRateReset": "P1YL0",
        // At each reset: new rate = rateMultiplier × SOFR + rateSpread.
        "marketObjectCodeOfRateReset": "USD-SOFR",
        "rateSpread": 0.02,
    }));

    // The SOFR fixings observed on the reset dates. In production these come from a data feed.
    let mut market = actus_rs::MarketData::new();
    series(&mut market, "USD-SOFR", &[("2025-01-02", "0.0430"), ("2026-01-02", "0.0365")]);

    // Run the contract from the status date to its natural end (maturity).
    let results = loan.project(None, &market)?;
    print_table(&results, &[NotionalPrincipal, NominalInterestRate, AccruedInterest]);

    println!(
        "\nWhat happened:
  * IED: we pay out the principal (-1,000,000).
  * IP:  each quarter we receive interest = notional x rate x (days / 360). The amounts differ
         slightly from quarter to quarter because quarters have different numbers of days.
  * RR:  on 2025-01-02 the rate becomes SOFR 4.30% + 2% = 6.30%; a year later 3.65% + 2% = 5.65%.
         Interest paid after a reset uses the new rate.
  * MD:  the principal comes back in one piece; the notional drops to 0.
Total interest received: {:.2}
Net cash flow over the life of the loan: {:.2}",
        sum_of(&results, EventType::IP).to_f64(),
        net(&results).to_f64()
    );
    Ok(())
}
