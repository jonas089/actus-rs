//! OPTNS — Option.
//!
//! Run with: `cargo run --example optns_equity_option`
//!
//! The right, but not the obligation, to buy (call) or sell (put) an underlying at a fixed
//! strike price. The buyer pays a premium up front. At exercise the option is worth its
//! *intrinsic value*: max(price - strike, 0) for a call, max(strike - price, 0) for a put. With
//! cash settlement that value is paid; nothing is paid if the option expires out of the money.
//! The underlying is referenced by its market object code: its price is market data.
//!
//! ACTUS options are per one unit of the underlying; a contract on 100 shares is 100 x these
//! amounts.
//!
//! Scenario: an investor expects ACME shares (trading at 180) to rise and buys a three-month
//! European call with strike 190 for a premium of 6.50. For comparison, a put with the same
//! strike is run against the same market. At expiry ACME trades at 204.30.

#[path = "common/mod.rs"]
mod common;

use actus_rs::state::StateVariable::*;
use common::*;
use serde_json::json;

fn option(id: &str, option_type: &str, premium: &str) -> serde_json::Value {
    json!({
        "contractType": "OPTNS",
        "contractID": id,
        // BUY = long the option: pay the premium, receive the payout. SEL = the writer.
        "contractRole": "BUY",
        "statusDate": "2025-06-01T00:00:00",
        "currency": "USD",
        // The underlying: here a market object (the ACME share price). It could also be a
        // complete child contract (referenceType CNT), e.g. an option on a bond.
        "contractStructure": [{
            "referenceType": "MOC",
            "referenceRole": "UDL",
            "object": { "marketObjectCode": "ACME" }
        }],
        // Premium paid on the purchase date.
        "purchaseDate": "2025-06-02T00:00:00",
        "priceAtPurchaseDate": premium,
        // Expiry.
        "maturityDate": "2025-09-02T00:00:00",
        // E = European (exercise only at maturity), A = American (any time, via an observed
        // XD event), B = Bermudan (on a schedule of exercise dates).
        "optionExerciseType": "E",
        // C = call, P = put, CP = collar (call strike optionStrike1, put strike optionStrike2).
        "optionType": option_type,
        "optionStrike1": 190,
        // S = cash settlement of the intrinsic value; D = physical delivery.
        "deliverySettlement": "S",
        // Cash is paid 2 days after exercise.
        "settlementPeriod": "P2D",
    })
}

fn main() -> actus_rs::Result<()> {
    title(
        "OPTNS: European call and put on ACME, strike 190",
        "
        PRD pays the premium. MD marks expiry (no cash). XD then fixes the payout from the share
        price observed at maturity ('exercise amt') and STD pays it after the settlement period.
        ",
    );

    // The underlying's price at expiry (the only observation a European option needs).
    let mut market = actus_rs::MarketData::new();
    series(&mut market, "ACME", &[("2025-09-02", "204.30")]);

    println!("--- Call (right to buy at 190) ---");
    let call = contract(option("acme-call-190", "C", "6.50")).project(None, &market)?;
    print_table(&call, &[ExerciseAmount]);
    println!(
        "  At expiry ACME = 204.30 > 190: intrinsic value = 204.30 - 190 = 14.30.
  Profit = 14.30 - 6.50 premium = {:.2} per share.\n",
        net(&call).to_f64()
    );

    println!("--- Put (right to sell at 190) ---");
    let put = contract(option("acme-put-190", "P", "9.80")).project(None, &market)?;
    print_table(&put, &[ExerciseAmount]);
    println!(
        "  At expiry ACME = 204.30 > 190: selling at 190 is worthless, the put expires with payout 0.
  Net result: {:.2} per share, the premium lost.",
        net(&put).to_f64()
    );
    Ok(())
}
