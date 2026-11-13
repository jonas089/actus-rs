//! UMP — Undefined Maturity Profile.
//!
//! Run with: `cargo run --example ump_savings_account`
//!
//! A balance with no maturity and no repayment schedule: the principal only changes when the
//! customer deposits or withdraws, and interest is credited (capitalized) on a cycle. Savings
//! accounts, current accounts and overdraft lines are UMPs.
//!
//! Scenario: a customer opens a savings account with USD 10,000 at a bank. Interest at 3% is
//! credited to the balance every quarter. During the year the customer deposits USD 2,000 and
//! later withdraws USD 5,000. We model it from the *bank's* view: a deposit is money the bank
//! owes, so the role is RPL (liability) and the signs are flipped compared to a loan.

#[path = "common/mod.rs"]
mod common;

use actus_rs::state::StateVariable::*;
use actus_rs::EventType;
use common::*;
use serde_json::json;

fn main() -> actus_rs::Result<()> {
    title(
        "UMP: USD savings account (bank's view), quarterly interest credited",
        "
        The account has no maturity. Deposits (PI) and withdrawals (PR) are not scheduled; they
        are supplied as observed events. Interest is added to the balance each quarter (IPCI).
        ",
    );

    let account = contract(json!({
        "contractType": "UMP",
        "contractID": "savings-8812",
        // RPL = real position liability: the bank owes the balance to the customer.
        // The bank receives money at opening (positive) and pays it out on withdrawals (negative).
        "contractRole": "RPL",
        "statusDate": "2025-01-01T00:00:00",
        "currency": "USD",
        "initialExchangeDate": "2025-01-02T00:00:00",
        "notionalPrincipal": 10000,
        "nominalInterestRate": 0.03,
        "dayCountConvention": "A365",
        // For UMP the interest cycle capitalizes: interest is added to the balance (IPCI).
        "cycleAnchorDateOfInterestPayment": "2025-04-01T00:00:00",
        "cycleOfInterestPayment": "P3ML1",
        // How long a withdrawal notice is (informational here; withdrawals are observed events).
        "xDayNotice": "P0D",
    }));

    let mut market = actus_rs::MarketData::new();
    // A deposit: principal increase. The value is the amount.
    observed_event(&mut market, "savings-8812", EventType::PI, "2025-05-15", "2000");
    // A withdrawal: principal repayment from the bank to the customer.
    observed_event(&mut market, "savings-8812", EventType::PR, "2025-09-10", "5000");

    // UMP has no natural end, so we pick an analysis horizon. (Alternatively use
    // `account.stream(&market)` and stop whenever you like: the schedule is generated lazily.)
    let results = account.project(Some(date("2025-12-31")), &market)?;
    print_table(&results, &[NotionalPrincipal, AccruedInterest]);

    let last = results.last().and_then(|r| r.state.number(NotionalPrincipal)).unwrap_or_default();
    println!(
        "\nWhat happened:
  * IED: the customer's initial deposit arrives (+10,000 for the bank). The notional is shown
    negative because it is a liability of the bank.
  * IPCI: each quarter the interest is added to the balance; no cash moves.
  * PI on 2025-05-15: the customer deposits 2,000 (+ for the bank, balance grows).
  * PR on 2025-09-10: the customer withdraws 5,000 (- for the bank, balance falls).
Balance owed to the customer at the horizon: {:.2} (plus accrued interest since the last IPCI)",
        last.abs().to_f64()
    );
    Ok(())
}
