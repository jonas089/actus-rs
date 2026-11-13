//! FUTUR — Future.
//!
//! Run with: `cargo run --example futur_commodity_future`
//!
//! An obligation to buy (long) or sell (short) an underlying at a fixed futures price on a
//! future date. Unlike an option there is no choice: the payout can be positive or negative.
//! With cash settlement the difference (price at maturity - futures price) is paid. The
//! underlying is referenced by market object code; its price is market data.
//!
//! As with OPTNS, the amounts are per unit of the underlying. (Exchange-traded futures are also
//! margined daily; that variation margin is not modelled here, only the final settlement.)
//!
//! Scenario: an airline locks in its fuel cost by buying a jet fuel future at USD 2.45 per gallon
//! for December delivery, cash-settled. A refiner sells the same future to lock in its revenue.
//! At expiry fuel trades at USD 2.71. We run both sides.

#[path = "common/mod.rs"]
mod common;

use actus_rs::state::StateVariable::*;
use common::*;
use serde_json::json;

fn future(id: &str, role: &str) -> serde_json::Value {
    json!({
        "contractType": "FUTUR",
        "contractID": id,
        // BUY = long (gains when the price rises); SEL = short.
        "contractRole": role,
        "statusDate": "2025-06-01T00:00:00",
        "currency": "USD",
        "contractStructure": [{
            "referenceType": "MOC",
            "referenceRole": "UDL",
            "object": { "marketObjectCode": "JETFUEL-USG" }
        }],
        // Entering a future costs nothing up front (apart from margin, not modelled).
        "purchaseDate": "2025-06-02T00:00:00",
        "priceAtPurchaseDate": 0,
        "maturityDate": "2025-12-15T00:00:00",
        // The agreed price per gallon.
        "futuresPrice": 2.45,
        "deliverySettlement": "S",
        "settlementPeriod": "P0D",
    })
}

fn main() -> actus_rs::Result<()> {
    title(
        "FUTUR: jet fuel future at USD 2.45/gal, both sides",
        "
        At maturity XD fixes price - futures price; STD pays it (long side) or the opposite
        (short side). Here the price rose to 2.71, so the long side receives 0.26 per gallon.
        ",
    );

    let mut market = actus_rs::MarketData::new();
    series(&mut market, "JETFUEL-USG", &[("2025-12-15", "2.71")]);

    println!("--- Airline: long (BUY) ---");
    let long = contract(future("fuel-long", "BUY")).project(None, &market)?;
    print_table(&long, &[ExerciseAmount]);
    println!(
        "  The airline receives {:.2} per gallon. It still buys its fuel in the market at 2.71,
  so its net fuel cost is 2.71 - 0.26 = 2.45, the price it locked in.\n",
        net(&long).to_f64()
    );

    println!("--- Refiner: short (SEL) ---");
    let short = contract(future("fuel-short", "SEL")).project(None, &market)?;
    print_table(&short, &[ExerciseAmount]);
    println!(
        "  The refiner pays {:.2} per gallon. It sells its fuel in the market at 2.71, so its net
  revenue is also 2.45. The two sides' flows cancel: a future is a zero-sum hedge.
  For a 42,000-gallon contract, multiply by 42,000: the airline receives USD {:.2}.",
        net(&short).abs().to_f64(),
        net(&long).to_f64() * 42000.0
    );
    Ok(())
}
