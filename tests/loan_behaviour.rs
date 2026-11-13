//! Behaviour not covered by the official test cases: liability-side trades, prepayment
//! scheduling and effects, observed UMP flows and fixed first dividends.

use actus_rs::state::{StateSnapshot, StateVariable, Value};
use actus_rs::time::parse_datetime as d;
use actus_rs::{Contract, EventResult, EventType, Fixed, MarketData, ObservedEvent};
use serde_json::{Value as Json, json};

type F = Fixed<18>;

fn contract(terms: Json) -> Contract<18> {
    Contract::from_json(&terms).unwrap()
}

fn f(x: &str) -> F {
    F::parse(x).unwrap()
}

fn num(r: &EventResult<18>, v: StateVariable) -> f64 {
    match r.state.get(v) {
        Some(Value::Number(n)) => n.to_f64(),
        other => panic!("{v:?} is {other:?}"),
    }
}

fn observed(cid: &str, ty: EventType, t: &str, value: &str) -> ObservedEvent<18> {
    ObservedEvent {
        contract_id: cid.into(),
        event_type: ty,
        time: d(t).unwrap(),
        value: f(value),
        states: StateSnapshot::default(),
    }
}

fn of_type(rs: &[EventResult<18>], ty: EventType) -> Vec<&EventResult<18>> {
    rs.iter().filter(|r| r.event.event_type == ty).collect()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn liability_purchase_follows_reference_formula() {
    // 1000 at 12% (30E/360) since 2020-01-01; bought on 2020-03-01 at 990. Two months of
    // interest (20) have accrued. Economically the party taking over the liability should be
    // paid price + accrued (1010), but the spec formula R·(−1)·(PPRD + Ipac) — which the
    // official cases lam04, nam04 and lam21 pin — gives price − accrued (970). We follow the
    // official tests; see docs/notes/core.md ("Spec issues").
    let rs = contract(json!({
        "contractType": "PAM", "contractID": "rpl", "contractRole": "RPL", "currency": "USD",
        "statusDate": "2020-02-01T00:00:00", "initialExchangeDate": "2020-01-01T00:00:00",
        "maturityDate": "2021-01-01T00:00:00", "dayCountConvention": "30E360",
        "notionalPrincipal": "1000", "nominalInterestRate": "0.12",
        "purchaseDate": "2020-03-01T00:00:00", "priceAtPurchaseDate": "990",
    }))
    .project(None, &MarketData::new())
    .unwrap();
    let prd = of_type(&rs, EventType::PRD)[0];
    assert!(
        close(prd.payoff.to_f64(), 970.0),
        "PRD payoff {}",
        prd.payoff
    );
    // At maturity the liability holder repays the notional plus a year of interest.
    let paid: f64 = rs
        .iter()
        .filter(|r| r.event.time == d("2021-01-01").unwrap())
        .map(|r| r.payoff.to_f64())
        .sum();
    assert!(close(paid, -1120.0), "paid at maturity {paid}");
}

#[test]
fn pam_scheduled_prepayment_uses_rate_model() {
    let mut md = MarketData::new();
    md.insert("PPRATE", d("2021-01-01").unwrap(), f("0.1"));
    md.insert("PPRATE", d("2022-01-01").unwrap(), f("0"));
    let rs = contract(json!({
        "contractType": "PAM", "contractID": "pp", "contractRole": "RPA", "currency": "USD",
        "statusDate": "2020-01-01T00:00:00", "initialExchangeDate": "2020-01-01T00:00:00",
        "maturityDate": "2023-01-01T00:00:00", "dayCountConvention": "30E360",
        "notionalPrincipal": "1000", "nominalInterestRate": "0.05",
        "cycleAnchorDateOfInterestPayment": "2021-01-01T00:00:00", "cycleOfInterestPayment": "P1YL1",
        "prepaymentEffect": "M", "cycleAnchorDateOfOptionality": "2021-01-01T00:00:00",
        "cycleOfOptionality": "P1YL1", "objectCodeOfPrepaymentModel": "PPRATE",
    }))
    .project(None, &md)
    .unwrap();
    let pp = of_type(&rs, EventType::PP);
    // PP on 2021-01-01 and 2022-01-01 (maturity excluded); the second has a zero rate but is
    // still emitted.
    assert_eq!(pp.len(), 2);
    assert!(close(pp[0].payoff.to_f64(), 100.0));
    assert!(close(num(pp[0], StateVariable::NotionalPrincipal), 900.0));
    assert!(close(pp[1].payoff.to_f64(), 0.0));
    let md_ev = of_type(&rs, EventType::MD)[0];
    assert!(close(md_ev.payoff.to_f64(), 900.0));
}

fn ann(effect: &str) -> Vec<EventResult<18>> {
    let mut md = MarketData::new();
    md.events
        .push(observed("ann", EventType::PP, "2020-06-01T00:00:00", "300"));
    contract(json!({
        "contractType": "ANN", "contractID": "ann", "contractRole": "RPA", "currency": "USD",
        "statusDate": "2020-01-01T00:00:00", "initialExchangeDate": "2020-01-01T00:00:00",
        "maturityDate": "2021-01-01T00:00:00", "dayCountConvention": "30E360",
        "notionalPrincipal": "1200", "nominalInterestRate": "0.06",
        "cycleAnchorDateOfPrincipalRedemption": "2020-02-01T00:00:00", "cycleOfPrincipalRedemption": "P1ML1",
        "cycleAnchorDateOfInterestPayment": "2020-02-01T00:00:00", "cycleOfInterestPayment": "P1ML1",
        "prepaymentEffect": effect,
    }))
    .project(None, &md)
    .unwrap()
}

/// Notional reaches zero at the end, principal flows add up to the notional and the total
/// cash flow equals principal plus the interest accrued on the outstanding balances.
fn check_consistent(rs: &[EventResult<18>]) {
    let last = rs.last().unwrap();
    assert!(close(num(last, StateVariable::NotionalPrincipal), 0.0));
    let principal: f64 = rs
        .iter()
        .filter(|r| {
            matches!(
                r.event.event_type,
                EventType::PR | EventType::PP | EventType::MD
            )
        })
        .map(|r| r.payoff.to_f64())
        .sum();
    assert!(close(principal, 1200.0), "principal {principal}");
    // Interest: 6%/12 on the balance outstanding during each month.
    let mut interest = 0.0;
    let mut prev_nt = 1200.0;
    for r in rs.iter().filter(|r| r.event.event_type == EventType::IP) {
        interest += prev_nt * 0.005;
        assert!(
            close(r.payoff.to_f64(), prev_nt * 0.005),
            "IP {} on {prev_nt}",
            r.payoff
        );
        prev_nt = rs
            .iter()
            .rfind(|x| x.event.time <= r.event.time)
            .map(|x| num(x, StateVariable::NotionalPrincipal))
            .unwrap();
        // The PP on 2020-06-01 shares its date with PR/IP; the balance after it is used next.
    }
    let paid: f64 = rs
        .iter()
        .filter(|r| r.event.event_type != EventType::IED)
        .map(|r| r.payoff.to_f64())
        .sum();
    assert!(
        close(paid, 1200.0 + interest),
        "paid {paid} vs {}",
        1200.0 + interest
    );
}

#[test]
fn ann_observed_prepayment_reduces_annuity() {
    let rs = ann("A");
    let pp = of_type(&rs, EventType::PP);
    assert_eq!(pp.len(), 1);
    assert!(close(pp[0].payoff.to_f64(), 300.0));
    let pr = of_type(&rs, EventType::PR);
    // Twelve redemptions (the last at maturity): payment drops after the prepayment and the
    // loan still runs to maturity.
    let pay = |i: usize| num(pr[i], StateVariable::NextPrincipalRedemptionPayment);
    assert!(pay(5) < pay(3), "{} !< {}", pay(5), pay(3));
    assert!(
        pr.last().unwrap().payoff.to_f64() > 0.0
            || of_type(&rs, EventType::MD)[0].payoff.to_f64() > 0.0
    );
    check_consistent(&rs);
}

#[test]
fn ann_observed_prepayment_shortens_maturity() {
    let rs = ann("M");
    let pr = of_type(&rs, EventType::PR);
    let pay = |i: usize| num(pr[i], StateVariable::NextPrincipalRedemptionPayment);
    assert!(close(pay(8), pay(3)), "payment kept");
    // Repaid before the contractual maturity: trailing redemptions are zero.
    let zero_after = rs.iter().position(|r| {
        r.event.event_type == EventType::PR && close(num(r, StateVariable::NotionalPrincipal), 0.0)
    });
    let paid_off = rs[zero_after.expect("repaid early")].event.time;
    assert!(paid_off < d("2021-01-01").unwrap());
    assert!(
        rs.iter()
            .filter(|r| r.event.time > paid_off)
            .all(|r| close(r.payoff.to_f64(), 0.0))
    );
    check_consistent(&rs);
}

#[test]
fn ump_observed_repayment_and_increase() {
    let mut md = MarketData::new();
    md.events
        .push(observed("ump", EventType::PR, "2020-03-01T00:00:00", "200"));
    md.events
        .push(observed("ump", EventType::PI, "2020-06-01T00:00:00", "100"));
    let rs = contract(json!({
        "contractType": "UMP", "contractID": "ump", "contractRole": "RPA", "currency": "USD",
        "statusDate": "2020-01-01T00:00:00", "initialExchangeDate": "2020-01-01T00:00:00",
        "dayCountConvention": "30E360", "notionalPrincipal": "1000", "nominalInterestRate": "0",
    }))
    .project(Some(d("2021-01-01").unwrap()), &md)
    .unwrap();
    let pr = of_type(&rs, EventType::PR)[0];
    assert!(close(pr.payoff.to_f64(), 200.0));
    assert!(close(num(pr, StateVariable::NotionalPrincipal), 800.0));
    let pi = of_type(&rs, EventType::PI)[0];
    assert!(close(pi.payoff.to_f64(), -100.0));
    assert!(close(num(pi, StateVariable::NotionalPrincipal), 900.0));
}

#[test]
fn stk_first_dividend_is_fixed_by_next_dividend_amount() {
    let mut md = MarketData::new();
    for t in ["2020-03-01", "2020-06-01", "2020-09-01"] {
        md.insert("DIV", d(t).unwrap(), f("3"));
    }
    let rs = contract(json!({
        "contractType": "STK", "contractID": "stk", "contractRole": "RPA", "currency": "USD",
        "statusDate": "2020-01-01T00:00:00", "quantity": "10",
        "cycleAnchorDateOfDividend": "2020-03-01T00:00:00", "cycleOfDividend": "P3ML1",
        "marketObjectCodeOfDividends": "DIV", "nextDividendPaymentAmount": "2",
    }))
    .project(Some(d("2020-10-01").unwrap()), &md)
    .unwrap();
    let dv: Vec<f64> = of_type(&rs, EventType::DV)
        .iter()
        .map(|r| r.payoff.to_f64())
        .collect();
    assert_eq!(dv, vec![20.0, 30.0, 30.0]);
}
