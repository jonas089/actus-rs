//! Helpers shared by the contract examples: building inputs and printing results as a table.
#![allow(dead_code)]

use actus_rs::observer::ObservedEvent;
use actus_rs::state::{StateSnapshot, StateVariable, Value};
use actus_rs::terms::ContractPerformance;
use actus_rs::time::parse_datetime;
use actus_rs::{Contract, DateTime, EventResult, EventType, Fixed, MarketData};
use std::collections::BTreeSet;

pub type F = Fixed<18>;
#[allow(unused_imports)]
pub use actus_rs::DateTime as Time;

pub fn date(s: &str) -> DateTime {
    parse_datetime(s).expect("valid date")
}

pub fn dec(s: &str) -> F {
    F::parse(s).expect("valid decimal")
}

/// Parse ACTUS JSON terms, panicking with a readable message on error.
pub fn contract(terms: serde_json::Value) -> Contract {
    Contract::from_json(&terms).unwrap_or_else(|e| panic!("invalid terms: {e}"))
}

/// Add a series of (date, value) observations for one market object.
pub fn series(md: &mut MarketData<18>, id: &str, points: &[(&str, &str)]) {
    for (d, v) in points {
        md.insert(id, date(d), dec(v));
    }
}

/// Record an unscheduled event (prepayment, call, credit event, ...) for a contract.
pub fn observed_event(md: &mut MarketData<18>, contract_id: &str, ty: EventType, time: &str, value: &str) {
    md.events.push(ObservedEvent {
        contract_id: contract_id.into(),
        event_type: ty,
        time: date(time),
        value: dec(value),
        states: StateSnapshot::default(),
    });
}

/// Record a credit event: the contract's performance changes (e.g. to default, `DF`).
pub fn credit_event(md: &mut MarketData<18>, contract_id: &str, time: &str, performance: ContractPerformance) {
    let mut states = StateSnapshot::default();
    states.0.insert(StateVariable::ContractPerformance, Value::Performance(performance));
    md.events.push(ObservedEvent {
        contract_id: contract_id.into(),
        event_type: EventType::CE,
        time: date(time),
        value: F::ZERO,
        states,
    });
}

pub fn title(name: &str, text: &str) {
    println!("\n=== {name} ===\n");
    for line in text.trim().lines() {
        println!("{}", line.trim());
    }
    println!();
}

fn fmt_num(x: F) -> String {
    let v = x.to_f64();
    if v == 0.0 { "0".into() } else { format!("{v:.2}") }
}

fn fmt_state(s: &StateSnapshot<18>, v: StateVariable) -> String {
    match s.get(v) {
        Some(Value::Number(n)) if v == StateVariable::NominalInterestRate => format!("{:.4}%", n.to_f64() * 100.0),
        Some(Value::Number(n)) => fmt_num(n),
        Some(Value::Date(d)) => d.date().to_string(),
        Some(Value::Performance(p)) => format!("{p:?}"),
        Some(Value::Flag(b)) => b.to_string(),
        None => "-".into(),
    }
}

fn short(v: StateVariable) -> &'static str {
    use StateVariable::*;
    match v {
        NotionalPrincipal => "notional",
        NominalInterestRate => "rate",
        AccruedInterest => "accrued int.",
        AccruedInterest2 => "accrued int. 2",
        FeeAccrued => "fee accrued",
        NextPrincipalRedemptionPayment => "next payment",
        InterestCalculationBaseAmount => "interest base",
        NotionalScalingMultiplier => "notional scale",
        InterestScalingMultiplier => "interest scale",
        ExerciseAmount => "exercise amt",
        ExerciseDate => "exercise date",
        ContractPerformance => "performance",
        _ => v.name(),
    }
}

/// Print the results as a table with the given state columns, then explain the event codes used.
pub fn print_table(results: &[EventResult<18>], columns: &[StateVariable]) {
    print!("{:<11} {:<5} {:>14}", "date", "event", "cash flow");
    for c in columns {
        print!(" {:>14}", short(*c));
    }
    println!();
    println!("{}", "-".repeat(33 + 15 * columns.len()));
    for r in results {
        print!("{:<11} {:<5} {:>14}", r.event.time.date(), r.event.event_type.to_string(), fmt_num(r.payoff));
        for c in columns {
            print!(" {:>14}", fmt_state(&r.state, *c));
        }
        println!();
    }
    let types: BTreeSet<EventType> = results.iter().map(|r| r.event.event_type).collect();
    println!("\nEvent codes:");
    for t in types {
        println!("  {:<5} {}", t.to_string(), describe(t));
    }
    println!("Cash flows are seen from the side named by contractRole: positive = received, negative = paid.");
}

/// Sum of all cash flows.
pub fn net(results: &[EventResult<18>]) -> F {
    results.iter().map(|r| r.payoff).sum()
}

/// Sum of cash flows of one event type.
pub fn sum_of(results: &[EventResult<18>], t: EventType) -> F {
    results.iter().filter(|r| r.event.event_type == t).map(|r| r.payoff).sum()
}

pub fn describe(t: EventType) -> &'static str {
    use EventType::*;
    match t {
        AD => "monitoring: states updated, no cash flow",
        IED => "initial exchange: the principal changes hands",
        FP => "fee payment",
        PR => "principal redemption (scheduled repayment)",
        PD => "principal drawing",
        PRF => "principal payment fixing: the instalment amount is recalculated",
        PY => "penalty payment",
        PP => "principal prepayment (unscheduled)",
        IP => "interest payment",
        IPCI => "interest capitalization: interest is added to principal instead of paid",
        CE => "credit event (e.g. default)",
        RRF => "rate reset to a rate already fixed in the terms",
        RR => "rate reset from an observed market rate",
        DV => "dividend payment",
        PRD => "purchase: the position is bought",
        MR => "margin call",
        TD => "termination: the position is sold or closed early",
        SC => "scaling index fixing (index-linked notional/interest)",
        IPCB => "interest calculation base fixing",
        MD => "maturity: final repayment",
        XD => "exercise: the payout amount is determined",
        STD => "settlement: the exercised amount is paid",
        PI => "principal increase",
        IPFX => "interest payment, fixed leg",
        IPFL => "interest payment, floating leg",
    }
}
