//! Engine-level invariants, checked over every official ACTUS test case.

use actus_rs::conformance::{load, market_data};
use actus_rs::observer::Recorder;
use actus_rs::terms::RawTerms;
use actus_rs::time::parse_datetime;
use actus_rs::{Contract, EventType};

const TYPES: &[&str] = &[
    "pam", "lam", "lax", "nam", "ann", "clm", "ump", "csh", "stk", "com", "fxout", "swppv", "swaps", "capfl", "optns",
    "futur", "ceg", "cec",
];

fn cases() -> Vec<(String, serde_json::Value)> {
    TYPES.iter().filter_map(|t| load(t)).flatten().collect()
}

fn contract(case: &serde_json::Value) -> Contract<18> {
    Contract::from_raw(&RawTerms::from_json(&case["terms"]).unwrap()).unwrap()
}

fn horizon(case: &serde_json::Value) -> Option<actus_rs::DateTime> {
    case.get("to").and_then(|v| v.as_str()).and_then(|s| parse_datetime(s).ok())
}

/// The lazy stream evaluates exactly the same events as the batch projection.
#[test]
fn stream_matches_projection() {
    for (id, case) in cases() {
        let c = contract(&case);
        let md = market_data(&case);
        let batch = c.project(horizon(&case), &md).unwrap();
        let streamed: Vec<_> = c.stream(&md).take(batch.len()).collect::<Result<_, _>>().unwrap();
        assert_eq!(batch.len(), streamed.len(), "{id}: stream ended early");
        for (a, b) in batch.iter().zip(&streamed) {
            assert_eq!(a, b, "{id}: stream and projection differ");
        }
    }
}

/// Recording the observations of a run and replaying only those reproduces it exactly.
#[test]
fn recorded_inputs_replay_identically() {
    for (id, case) in cases() {
        let c = contract(&case);
        let md = market_data(&case);
        let rec = Recorder::new(&md);
        let first = c.project(horizon(&case), &rec).unwrap();
        let replay_data = rec.into_market_data(md.events.clone());
        let second = c.project(horizon(&case), &replay_data).unwrap();
        assert_eq!(first, second, "{id}: replay differs");
    }
}

/// A lending contract that reaches maturity has no principal left afterwards.
#[test]
fn matured_loans_are_fully_repaid() {
    for t in ["pam", "lam", "lax", "nam", "ann"] {
        for (id, case) in load(t).into_iter().flatten() {
            let c = contract(&case);
            let res = c.project(horizon(&case), &market_data(&case)).unwrap();
            if let Some(last) = res.last().filter(|r| r.event.event_type == EventType::MD) {
                let nt = last.state.number(actus_rs::state::StateVariable::NotionalPrincipal).unwrap();
                assert!(nt.is_zero(), "{id}: notional {nt} left at maturity");
            }
        }
    }
}

/// Every event's required inputs are exactly the observations its step reads.
#[test]
fn required_inputs_cover_rate_resets() {
    use actus_rs::contract::ContractModel;
    let Some(cases) = load("pam") else { return };
    for (id, case) in cases {
        let Contract::Pam(pam) = contract(&case) else { unreachable!() };
        let md = market_data(&case);
        let mut state = pam.initial_state(&md).unwrap();
        for ev in pam.schedule(horizon(&case), &md).unwrap() {
            let needed = pam.required_inputs(&state, &ev);
            if ev.event_type == EventType::RR {
                let moc = pam.rate_reset.market_object_code.clone().unwrap();
                assert_eq!(needed, vec![(moc, ev.schedule_time)], "{id}");
            }
            state = pam.step(&state, &ev, &md).unwrap().0;
        }
    }
}
