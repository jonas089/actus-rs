//! PERP — perpetual future. There are no official ACTUS cases for it, so every number here is
//! worked out by hand in the comments.

use actus_rs::contract::ContractModel;
use actus_rs::observer::{ObservedEvent, Recorder};
use actus_rs::state::{StateSnapshot, StateVariable, Value};
use actus_rs::terms::ContractPerformance;
use actus_rs::time::{parse_datetime, TickClock};
use actus_rs::{Contract, DateTime, Error, EventResult, EventType, Fixed, MarketData};
use serde_json::json;

type F = Fixed<18>;

fn d(s: &str) -> DateTime {
    parse_datetime(s).unwrap()
}

fn f(s: &str) -> F {
    F::parse(s).unwrap()
}

/// Two units at an entry price of 100, funding every 8 hours, settled daily.
fn terms(id: &str, role: &str) -> serde_json::Value {
    json!({
        "contractType": "PERP",
        "contractID": id,
        "contractRole": role,
        "statusDate": "2026-01-01T00:00:00",
        "currency": "USD",
        "contractStructure": [{
            "referenceType": "MOC", "referenceRole": "UDL", "object": { "marketObjectCode": "ASSET" }
        }],
        "quantity": 2,
        "futuresPrice": 100,
        "cycleOfFunding": "PT8H",
        "cycleOfMargining": "P1D",
    })
}

fn with(mut terms: serde_json::Value, extra: serde_json::Value) -> serde_json::Value {
    for (k, v) in extra.as_object().unwrap() {
        if v.is_null() {
            terms.as_object_mut().unwrap().remove(k);
        } else {
            terms[k] = v.clone();
        }
    }
    terms
}

fn contract(terms: serde_json::Value) -> Contract {
    Contract::from_json(&terms).unwrap()
}

/// Price and funding rate at each funding time of the first two days.
const MARKET: &[(&str, &str, &str)] = &[
    ("2026-01-01T08:00:00", "101", "0.0001"),
    ("2026-01-01T16:00:00", "99", "0.0001"),
    ("2026-01-02T00:00:00", "104", "0.0001"),
    ("2026-01-02T08:00:00", "103", "-0.0002"),
    ("2026-01-02T16:00:00", "106", "0.0001"),
    ("2026-01-03T00:00:00", "110", "0.0001"),
];

fn market() -> MarketData<18> {
    let mut md = MarketData::new();
    for (t, price, rate) in MARKET {
        md.insert("ASSET", d(t), f(price));
        md.insert("ASSET-FR", d(t), f(rate));
    }
    md
}

fn observed(md: &mut MarketData<18>, id: &str, ty: EventType, time: &str, value: &str) {
    md.events.push(ObservedEvent {
        contract_id: id.into(),
        event_type: ty,
        time: d(time),
        value: f(value),
        states: StateSnapshot::default(),
    });
}

fn flows(results: &[EventResult<18>]) -> Vec<(DateTime, EventType, F)> {
    results.iter().map(|r| (r.event.time, r.event.event_type, r.payoff)).collect()
}

fn net(results: &[EventResult<18>]) -> F {
    results.iter().map(|r| r.payoff).sum()
}

const TWO_DAYS: &str = "2026-01-03T00:00:00";

#[test]
fn long_position_cash_flows() {
    let res = contract(terms("p", "LG")).project(Some(d(TWO_DAYS)), &market()).unwrap();
    // Funding: the long pays 2 × price × rate, and receives it when the rate is negative.
    // Margining: 2 × (price - price last settled at); the first is against the entry price.
    let expected = [
        ("2026-01-01T08:00:00", EventType::FND, "-0.0202"),
        ("2026-01-01T16:00:00", EventType::FND, "-0.0198"),
        ("2026-01-02T00:00:00", EventType::FND, "-0.0208"),
        ("2026-01-02T00:00:00", EventType::MR, "8"),
        ("2026-01-02T08:00:00", EventType::FND, "0.0412"),
        ("2026-01-02T16:00:00", EventType::FND, "-0.0212"),
        ("2026-01-03T00:00:00", EventType::FND, "-0.022"),
        ("2026-01-03T00:00:00", EventType::MR, "12"),
    ];
    let expected: Vec<_> = expected.iter().map(|(t, ty, p)| (d(t), *ty, f(p))).collect();
    assert_eq!(flows(&res), expected);
    assert_eq!(net(&res), f("19.9372"));

    // The position is carried at the price it was last settled at.
    let carried: Vec<F> = res.iter().map(|r| r.state.number(StateVariable::SettlementPrice).unwrap()).collect();
    let expected = ["100", "100", "100", "104", "104", "104", "104", "110"].map(f);
    assert_eq!(carried, expected);
}

#[test]
fn long_and_short_cancel() {
    let long = contract(terms("l", "LG")).project(Some(d(TWO_DAYS)), &market()).unwrap();
    let short = contract(terms("s", "ST")).project(Some(d(TWO_DAYS)), &market()).unwrap();
    assert_eq!(long.len(), short.len());
    for (l, s) in long.iter().zip(&short) {
        assert_eq!(l.payoff, -s.payoff, "{} at {}", l.event.event_type, l.event.time);
    }
}

#[test]
fn margining_moves_cash_earlier_but_not_the_total() {
    let closed = json!({ "terminationDate": TWO_DAYS });
    let margined = contract(with(terms("m", "LG"), closed.clone())).project(None, &market()).unwrap();
    let unmargined = with(with(terms("u", "LG"), closed), json!({ "cycleOfMargining": null }));
    let unmargined = contract(unmargined).project(None, &market()).unwrap();

    // Margined: 8 and 12 at the two settlements, nothing left at the close.
    let last = margined.last().unwrap();
    assert_eq!((last.event.event_type, last.payoff), (EventType::TD, F::ZERO));
    // Unmargined: the whole 2 × (110 - 100) at the close.
    let last = unmargined.last().unwrap();
    assert_eq!((last.event.event_type, last.payoff), (EventType::TD, f("20")));
    assert!(unmargined.iter().all(|r| r.event.event_type != EventType::MR));

    assert_eq!(net(&margined), f("19.9372"));
    assert_eq!(net(&margined), net(&unmargined));
    for res in [&margined, &unmargined] {
        let state = &res.last().unwrap().state;
        assert_eq!(state.get(StateVariable::ContractPerformance), Some(Value::Performance(ContractPerformance::TE)));
        assert_eq!(state.number(StateVariable::SettlementPrice), Some(f("110")));
    }
}

#[test]
fn close_at_an_agreed_price() {
    // Sold at 108 while the market is at 110. The daily settlement falls on the same instant and
    // comes first, marking the position to 110, so the close pays 2 × (108 - 110).
    let t = with(terms("p", "LG"), json!({ "terminationDate": TWO_DAYS, "priceAtTerminationDate": 108 }));
    let res = contract(t).project(None, &market()).unwrap();
    let last = res.last().unwrap();
    assert_eq!((last.event.event_type, last.payoff), (EventType::TD, f("-4")));
    assert_eq!(net(&res), f("15.9372"));
}

#[test]
fn liquidation_ends_the_position() {
    // Liquidated at 90, four hours after the first daily settlement at 104.
    let mut md = market();
    observed(&mut md, "p", EventType::TD, "2026-01-02T04:00:00", "90");
    let c = contract(terms("p", "LG"));
    // The end is known, so no horizon is needed; a later one changes nothing.
    let res = c.project(None, &md).unwrap();
    assert_eq!(res, c.project(Some(d("2027-01-01T00:00:00")), &md).unwrap());

    let expected = [
        ("2026-01-01T08:00:00", EventType::FND, "-0.0202"),
        ("2026-01-01T16:00:00", EventType::FND, "-0.0198"),
        ("2026-01-02T00:00:00", EventType::FND, "-0.0208"),
        ("2026-01-02T00:00:00", EventType::MR, "8"),
        ("2026-01-02T04:00:00", EventType::TD, "-28"),
    ];
    let expected: Vec<_> = expected.iter().map(|(t, ty, p)| (d(t), *ty, f(p))).collect();
    assert_eq!(flows(&res), expected);
    // 2 × (90 - 100) less the funding paid.
    assert_eq!(net(&res), f("-20.0608"));

    // An earlier observed close wins over a termination date in the terms, and the reverse.
    let later = with(terms("p", "LG"), json!({ "terminationDate": TWO_DAYS }));
    assert_eq!(contract(later).project(None, &md).unwrap(), res);
    let earlier = with(terms("p", "LG"), json!({ "terminationDate": "2026-01-01T16:00:00" }));
    let res = contract(earlier).project(None, &md).unwrap();
    assert_eq!(res.last().unwrap().event.time, d("2026-01-01T16:00:00"));
    // Closed at the observed 99: 2 × (99 - 100).
    assert_eq!(res.last().unwrap().payoff, f("-2"));
}

#[test]
fn observed_close_without_a_price_uses_the_market() {
    let mut md = market();
    observed(&mut md, "p", EventType::TD, "2026-01-01T16:00:00", "0");
    let res = contract(terms("p", "LG")).project(None, &md).unwrap();
    assert_eq!(res.last().unwrap().payoff, f("-2"));
}

#[test]
fn settlement_currency_converts_every_cash_flow() {
    let mut md = market();
    for (t, _, _) in MARKET {
        md.insert("USD/EUR", d(t), f("0.5"));
    }
    let eur = contract(with(terms("p", "LG"), json!({ "settlementCurrency": "EUR" })));
    let eur = eur.project(Some(d(TWO_DAYS)), &md).unwrap();
    let usd = contract(terms("p", "LG")).project(Some(d(TWO_DAYS)), &md).unwrap();
    for (e, u) in eur.iter().zip(&usd) {
        assert_eq!(e.payoff, u.payoff * f("0.5"));
    }
}

#[test]
fn no_end_needs_a_horizon_and_streams_without_one() {
    let c = contract(terms("p", "LG"));
    let md = market();
    assert!(matches!(c.project(None, &md), Err(Error::Unsupported(_))));

    let batch = c.project(Some(d(TWO_DAYS)), &md).unwrap();
    let mut stream = c.stream(&md);
    let streamed: Vec<_> = stream.by_ref().take(batch.len()).collect::<Result<_, _>>().unwrap();
    assert_eq!(batch, streamed);
    // The stream goes on past the data: the next funding has no observation.
    assert!(matches!(stream.next(), Some(Err(Error::MissingObservation { .. }))));
}

#[test]
fn stream_matches_projection_when_closed() {
    let mut md = market();
    observed(&mut md, "p", EventType::TD, "2026-01-02T04:00:00", "90");
    let c = contract(terms("p", "LG"));
    let batch = c.project(None, &md).unwrap();
    let streamed: Vec<_> = c.stream(&md).collect::<Result<_, _>>().unwrap();
    assert_eq!(batch, streamed);
}

#[test]
fn required_inputs_name_the_price_and_the_funding_rate() {
    let Contract::Perp(perp) = contract(terms("p", "LG")) else { unreachable!() };
    let md = market();
    let state = perp.initial_state(&md).unwrap();
    for ev in perp.schedule(Some(d(TWO_DAYS)), &md).unwrap() {
        let t = ev.schedule_time;
        let needed = perp.required_inputs(&state, &ev);
        match ev.event_type {
            EventType::FND => assert_eq!(needed, vec![("ASSET".to_string(), t), ("ASSET-FR".to_string(), t)]),
            EventType::MR => assert_eq!(needed, vec![("ASSET".to_string(), t)]),
            other => panic!("unexpected {other}"),
        }
    }
}

#[test]
fn recorded_inputs_replay_identically() {
    let c = contract(terms("p", "LG"));
    let md = market();
    let rec = Recorder::new(&md);
    let first = c.project(Some(d(TWO_DAYS)), &rec).unwrap();
    let replay = rec.into_market_data(Vec::new());
    assert_eq!(first, c.project(Some(d(TWO_DAYS)), &replay).unwrap());
}

#[test]
fn stepwise_matches_projection() {
    let c = contract(terms("p", "ST"));
    let md = market();
    let projected = c.project(Some(d(TWO_DAYS)), &md).unwrap();
    let mut state = c.initial_state(&md).unwrap();
    for (ev, expected) in c.schedule(Some(d(TWO_DAYS)), &md).unwrap().iter().zip(&projected) {
        let (next, payoff) = c.step(&state, ev, &md).unwrap();
        assert_eq!(payoff, expected.payoff);
        state = next;
    }
}

#[test]
fn a_credit_event_marks_the_position_without_cash() {
    let mut md = market();
    md.events.push(ObservedEvent {
        contract_id: "p".into(),
        event_type: EventType::CE,
        time: d("2026-01-01T12:00:00"),
        value: F::ZERO,
        states: StateSnapshot([(StateVariable::ContractPerformance, Value::Performance(ContractPerformance::DF))].into()),
    });
    let res = contract(terms("p", "LG")).project(Some(d("2026-01-01T16:00:00")), &md).unwrap();
    let ce = &res[1];
    assert_eq!((ce.event.event_type, ce.payoff), (EventType::CE, F::ZERO));
    assert_eq!(ce.state.get(StateVariable::ContractPerformance), Some(Value::Performance(ContractPerformance::DF)));
}

/// A position on a ledger with 12-second blocks: settled every block, funded every 5 blocks.
fn tick_terms(clock: &TickClock) -> serde_json::Value {
    with(
        terms("p", "LG"),
        json!({
            "cycleOfFunding": clock.cycle(5).unwrap().to_string(),
            "cycleOfMargining": clock.cycle(1).unwrap().to_string(),
        }),
    )
}

#[test]
fn events_at_block_resolution_keep_their_order() {
    let clock = TickClock::new(d("2026-01-01T00:00:00"), 12).unwrap();
    let mut md = MarketData::new();
    // The price rises by 1 per block from 100 at genesis; funding is 0.001 per period.
    for tick in 1..=12u64 {
        md.insert("ASSET", clock.time(tick).unwrap(), F::from_int(100 + tick as i64));
        md.insert("ASSET-FR", clock.time(tick).unwrap(), f("0.001"));
    }
    // Liquidated at block 12 at 90, two blocks after the second funding.
    md.events.push(ObservedEvent {
        contract_id: "p".into(),
        event_type: EventType::TD,
        time: clock.time(12).unwrap(),
        value: f("90"),
        states: StateSnapshot::default(),
    });

    let res = contract(tick_terms(&clock)).project(None, &md).unwrap();
    let got: Vec<_> = res.iter().map(|r| (clock.tick(r.event.time).unwrap(), r.event.event_type, r.payoff)).collect();
    // Each block's settlement pays 2 × 1. Funding at blocks 5 and 10 is 2 × price × 0.001 and
    // comes before that block's settlement. The close pays 2 × (90 - 112).
    let mut expected = Vec::new();
    for tick in 1..=12u64 {
        if tick % 5 == 0 {
            expected.push((tick, EventType::FND, -(f("0.002") * F::from_int(100 + tick as i64))));
        }
        expected.push((tick, EventType::MR, f("2")));
    }
    expected.push((12, EventType::TD, f("-44")));
    assert_eq!(got, expected);
    // 2 × (90 - 100), less funding of 0.21 + 0.22.
    assert_eq!(net(&res), f("-20.43"));
}

#[test]
fn a_block_resolution_stream_starts_without_scheduling_a_year() {
    let clock = TickClock::new(d("2026-01-01T00:00:00"), 12).unwrap();
    let mut md = MarketData::new();
    for tick in 1..=4u64 {
        md.insert("ASSET", clock.time(tick).unwrap(), F::from_int(100 + tick as i64));
    }
    let c = contract(tick_terms(&clock));
    let first: Vec<_> = c.stream(&md).take(4).collect::<Result<_, _>>().unwrap();
    let ticks: Vec<_> = first.iter().map(|r| clock.tick(r.event.time).unwrap()).collect();
    assert_eq!(ticks, vec![1, 2, 3, 4]);
    assert!(first.iter().all(|r| r.event.event_type == EventType::MR && r.payoff == f("2")));
}

#[test]
fn terms_are_validated() {
    let parse = |extra| Contract::<18>::from_json(&with(terms("p", "LG"), extra));
    assert!(matches!(parse(json!({ "quantity": 0 })), Err(Error::InvalidTerm { .. })));
    assert!(matches!(parse(json!({ "quantity": -1 })), Err(Error::InvalidTerm { .. })));
    assert!(matches!(parse(json!({ "futuresPrice": null })), Err(Error::MissingTerm(_))));
    assert!(matches!(parse(json!({ "contractStructure": null })), Err(Error::MissingTerm(_))));
    // Neither cycle is required: without them the position only pays when it is closed.
    let bare = parse(json!({ "cycleOfFunding": null, "cycleOfMargining": null, "terminationDate": TWO_DAYS })).unwrap();
    let res = bare.project(None, &market()).unwrap();
    assert_eq!(flows(&res), vec![(d(TWO_DAYS), EventType::TD, f("20"))]);
}
