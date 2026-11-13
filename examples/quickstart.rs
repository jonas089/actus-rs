//! A 1-year bullet loan with monthly interest and a quarterly floating-rate reset.
//! `cargo run --example quickstart`

use actus_rs::time::parse_datetime;
use actus_rs::{Contract, Fixed, MarketData};
use serde_json::json;

fn main() -> actus_rs::Result<()> {
    let loan: Contract = Contract::from_json(&json!({
        "contractType": "PAM",
        "contractID": "loan-1",
        "contractRole": "RPA",
        "statusDate": "2024-01-01T00:00:00",
        "initialExchangeDate": "2024-01-02T00:00:00",
        "maturityDate": "2025-01-02T00:00:00",
        "currency": "USD",
        "notionalPrincipal": "1000000",
        "nominalInterestRate": "0.05",
        "dayCountConvention": "A360",
        "cycleOfInterestPayment": "P1ML1",
        "cycleAnchorDateOfInterestPayment": "2024-02-02T00:00:00",
        "cycleOfRateReset": "P3ML1",
        "cycleAnchorDateOfRateReset": "2024-04-02T00:00:00",
        "marketObjectCodeOfRateReset": "USD-SOFR",
        "rateSpread": "0.01"
    }))?;

    // Market data: SOFR fixings on the reset dates.
    let mut market = MarketData::new();
    for (date, rate) in [("2024-04-02", "0.053"), ("2024-07-02", "0.051"), ("2024-10-02", "0.047")] {
        market.insert("USD-SOFR", parse_datetime(date)?, Fixed::parse(rate).unwrap());
    }

    let mut total = Fixed::<18>::ZERO;
    for r in loan.stream(&market) {
        let r = r?;
        total += r.payoff;
        println!("{}  {:5}  {:>22}", r.event.time.date(), r.event.event_type.to_string(), r.payoff.to_string());
    }
    println!("net cash flow to the lender: {total}");
    Ok(())
}
