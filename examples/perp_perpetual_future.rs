//! PERP — Perpetual future (an extension; ACTUS does not define it).
//!
//! Run with: `cargo run --example perp_perpetual_future`
//!
//! A future with no maturity. Because it never settles against the underlying, a periodic funding
//! payment between the two sides keeps its price near the underlying's: when the perpetual
//! trades above, the funding rate is positive and the long side pays the short side; when it
//! trades below, the short side pays. The rate is published by the venue and is market data here.
//!
//! Gains and losses are paid in cash on the margining cycle (here daily), so the position is
//! always carried at the price it was last settled at. It ends when it is closed or liquidated.
//!
//! Scenario: a trader goes long 2 ETH at USD 3,000 against a market maker who is short. Funding
//! is paid every 8 hours. On the second day the price falls sharply and the venue liquidates the
//! long position at USD 2,700.

#[path = "common/mod.rs"]
mod common;

use actus_rs::state::StateVariable::*;
use actus_rs::EventType;
use common::*;
use serde_json::json;

fn perpetual(id: &str, role: &str) -> serde_json::Value {
    json!({
        "contractType": "PERP",
        "contractID": id,
        // LG = long (gains when the price rises); ST = short.
        "contractRole": role,
        "statusDate": "2026-03-01T00:00:00",
        "currency": "USD",
        // The price the position is marked at.
        "contractStructure": [{
            "referenceType": "MOC",
            "referenceRole": "UDL",
            "object": { "marketObjectCode": "ETH-USD" }
        }],
        "quantity": 2,
        // The entry price.
        "futuresPrice": 3000,
        // Funding every 8 hours; the rate is observed as "ETH-USD-FR" unless
        // marketObjectCodeOfFunding names another series.
        "cycleOfFunding": "PT8H",
        // Gains and losses are paid daily.
        "cycleOfMargining": "P1D",
    })
}

fn main() -> actus_rs::Result<()> {
    title(
        "PERP: 2 ETH perpetual entered at USD 3,000, both sides",
        "
        FND pays funding: quantity x price x rate, from the long to the short side when the
        rate is positive. MR pays the gain or loss since the last settlement. TD closes the
        position at the liquidation price.
        ",
    );

    let mut market = actus_rs::MarketData::new();
    let points = [
        ("2026-03-01T08:00:00", "3040", "0.0001"),
        ("2026-03-01T16:00:00", "3060", "0.0002"),
        ("2026-03-02T00:00:00", "3050", "0.0001"),
        ("2026-03-02T08:00:00", "2900", "-0.0003"),
    ];
    for (t, price, rate) in points {
        series(&mut market, "ETH-USD", &[(t, price)]);
        series(&mut market, "ETH-USD-FR", &[(t, rate)]);
    }
    // The venue closes both sides of the trade at 2,700.
    for id in ["eth-long", "eth-short"] {
        observed_event(&mut market, id, EventType::TD, "2026-03-02T11:30:00", "2700");
    }

    println!("--- Trader: long (LG) ---");
    let long = contract(perpetual("eth-long", "LG")).project(None, &market)?;
    print_table(&long, &[SettlementPrice, ContractPerformance]);
    println!(
        "  The first settlement pays the trader 2 x (3050 - 3000) = 100. The liquidation then
  costs 2 x (2700 - 3050) = -700. Funding nets to {:.2}: three payments while the perpetual
  traded above the underlying, one receipt after the fall. In total the trader loses {:.2},
  which is 2 x (2700 - 3000) = -600 plus funding.\n",
        sum_of(&long, EventType::FND).to_f64(),
        net(&long).abs().to_f64()
    );

    println!("--- Market maker: short (ST) ---");
    let short = contract(perpetual("eth-short", "ST")).project(None, &market)?;
    print_table(&short, &[SettlementPrice, ContractPerformance]);
    println!(
        "  Every flow is the mirror of the trader's, so the two sides sum to {:.2}.
  The margin the trader posted, and whether it covered the loss, is not part of this contract:
  it belongs to the account the position is held in.",
        (net(&long) + net(&short)).to_f64()
    );
    Ok(())
}
