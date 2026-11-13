//! Combined contracts are genuine state machines: driving them event by event through
//! `Contract::{initial_state, schedule, step}` gives exactly the projected results.

use actus_rs::conformance::{load, market_data};
use actus_rs::state::StateView;
use actus_rs::terms::RawTerms;
use actus_rs::Contract;

fn check(ct: &str) {
    let Some(cases) = load(ct) else {
        eprintln!("actus-tests not found; skipping {ct}");
        return;
    };
    for (id, case) in cases {
        let Ok(c) = Contract::<18>::from_raw(&RawTerms::from_json(&case["terms"]).unwrap()) else { continue };
        let md = market_data(&case);
        let projected = c.project(None, &md).unwrap_or_else(|e| panic!("{id}: {e}"));
        let events = c.schedule(None, &md).unwrap();
        let mut state = c.initial_state(&md).unwrap();
        let mut stepped = Vec::new();
        for ev in &events {
            let (next, payoff) = c.step(&state, ev, &md).unwrap_or_else(|e| panic!("{id}: {e}"));
            if c.is_reported(ev, &events) {
                stepped.push((ev.clone(), payoff, next.snapshot()));
            }
            state = next;
        }
        assert_eq!(stepped.len(), projected.len(), "{id}");
        for (s, p) in stepped.iter().zip(&projected) {
            assert_eq!(s.0, p.event, "{id}");
            assert_eq!(s.1, p.payoff, "{id}");
            assert_eq!(s.2, p.state, "{id}");
        }
    }
}

#[test]
fn swaps_stepwise() {
    check("swaps");
}

#[test]
fn capfl_stepwise() {
    check("capfl");
}
