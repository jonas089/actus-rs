//! Runner for the official ACTUS test suite (<https://github.com/actusfrf/actus-tests>).
//!
//! The test files are not redistributed with this crate (they are CC BY-SA); point
//! `ACTUS_TESTS_DIR` at a checkout's `tests/` folder, or place it at `../ref/actus-tests/tests`.

use crate::events::EventResult;
use crate::fixed::Fixed;
use crate::observer::{MarketData, ObservedEvent};
use crate::state::{StateSnapshot, StateVariable, Value};
use crate::terms::{ContractPerformance, RawTerms};
use crate::time::parse_datetime;
use crate::{Contract, EventType};
use serde_json::Value as Json;
use std::path::PathBuf;
use std::str::FromStr;

pub type F = Fixed<18>;

/// Directory holding `actus-tests-*.json`, if available.
pub fn tests_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("ACTUS_TESTS_DIR") {
        return Some(PathBuf::from(d));
    }
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ref/actus-tests/tests");
    p.exists().then_some(p)
}

/// Load all cases of one file, e.g. `pam`.
pub fn load(contract_type: &str) -> Option<Vec<(String, Json)>> {
    let path = tests_dir()?.join(format!("actus-tests-{contract_type}.json"));
    let text = std::fs::read_to_string(path).ok()?;
    let json: Json = serde_json::from_str(&text).ok()?;
    let mut cases: Vec<(String, Json)> = json.as_object()?.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    cases.sort_by(|a, b| natural_key(&a.0).cmp(&natural_key(&b.0)));
    Some(cases)
}

fn natural_key(s: &str) -> (String, u64) {
    let digits: String = s.chars().rev().take_while(|c| c.is_ascii_digit()).collect::<String>().chars().rev().collect();
    (s[..s.len() - digits.len()].to_string(), digits.parse().unwrap_or(0))
}

fn num(v: &Json) -> Option<F> {
    match v {
        Json::Number(n) => F::parse(&n.to_string()).ok(),
        Json::String(s) => F::parse(s).ok(),
        _ => None,
    }
}

/// Build market data (risk factor series and observed events) for a test case.
pub fn market_data(case: &Json) -> MarketData<18> {
    let mut md = MarketData::new();
    if let Some(obs) = case.get("dataObserved").and_then(|v| v.as_object()) {
        for (key, series) in obs {
            let id = series.get("identifier").and_then(|v| v.as_str()).unwrap_or(key);
            for p in series.get("data").and_then(|v| v.as_array()).into_iter().flatten() {
                let (Some(t), Some(v)) = (p.get("timestamp").and_then(|v| v.as_str()), p.get("value").and_then(num))
                else {
                    continue;
                };
                if let Ok(t) = parse_datetime(t) {
                    md.insert(id, t, v);
                }
            }
        }
    }
    for e in case.get("eventsObserved").and_then(|v| v.as_array()).into_iter().flatten() {
        let Some(t) = e.get("time").and_then(|v| v.as_str()).and_then(|t| parse_datetime(t).ok()) else { continue };
        let Some(ty) = e.get("type").and_then(|v| v.as_str()).and_then(|t| EventType::from_str(t).ok()) else {
            continue;
        };
        let mut states = StateSnapshot::default();
        if let Some(st) = e.get("states").and_then(|v| v.as_object()) {
            for (k, v) in st {
                if let Some(var) = StateVariable::from_name(k) {
                    let val = match (var, v) {
                        (StateVariable::ContractPerformance, Json::String(s)) => {
                            ContractPerformance::from_str(s).ok().map(Value::Performance)
                        }
                        _ => num(v).map(Value::Number),
                    };
                    if let Some(val) = val {
                        states.0.insert(var, val);
                    }
                }
            }
        }
        md.events.push(ObservedEvent {
            contract_id: e.get("contractId").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            event_type: ty,
            time: t,
            value: e.get("value").and_then(num).unwrap_or(F::ZERO),
            states,
        });
    }
    md
}

/// Run one case; returns a description of the first mismatch, if any.
pub fn run_case(case: &Json) -> Result<Vec<EventResult<18>>, String> {
    let terms = case.get("terms").ok_or("no terms")?;
    let contract = Contract::<18>::from_raw(&RawTerms::from_json(terms).map_err(|e| e.to_string())?)
        .map_err(|e| format!("parse error: {e}"))?;
    let to = case.get("to").and_then(|v| v.as_str()).and_then(|s| parse_datetime(s).ok());
    let md = market_data(case);
    contract.project(to, &md).map_err(|e| format!("evaluation error: {e}"))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 + 1e-10 * b.abs()
}

/// Compare computed results with the expected ones; `Err` describes the mismatches.
pub fn compare(case: &Json, computed: &[EventResult<18>]) -> Result<(), String> {
    let expected = case.get("results").and_then(|v| v.as_array()).ok_or("no results")?;
    let mut problems = Vec::new();
    if expected.len() != computed.len() {
        problems.push(format!("expected {} events, computed {}", expected.len(), computed.len()));
    }
    for (i, exp) in expected.iter().enumerate() {
        let Some(got) = computed.get(i) else { break };
        let exp_obj = exp.as_object().ok_or("bad result")?;
        let mut diffs = Vec::new();
        for (k, v) in exp_obj {
            match k.as_str() {
                "eventDate" => {
                    let want = parse_datetime(v.as_str().unwrap_or("")).ok();
                    if want != Some(got.event.time) {
                        diffs.push(format!("date {} != {}", got.event.time, v));
                    }
                }
                "eventType" => {
                    if v.as_str() != Some(&got.event.event_type.to_string()) {
                        diffs.push(format!("type {} != {}", got.event.event_type, v));
                    }
                }
                "currency" => {}
                "payoff" => {
                    let want = num(v).map(|x| x.to_f64()).unwrap_or(f64::NAN);
                    if !close(got.payoff.to_f64(), want) {
                        diffs.push(format!("payoff {} != {}", got.payoff, v));
                    }
                }
                name => {
                    let Some(var) = StateVariable::from_name(name) else { continue };
                    let got_v = got.state.get(var);
                    match (got_v, v) {
                        (Some(Value::Number(n)), _) => {
                            let want = num(v).map(|x| x.to_f64());
                            match want {
                                Some(w) if close(n.to_f64(), w) => {}
                                Some(_) => diffs.push(format!("{name} {n} != {v}")),
                                None if v.is_null() => {}
                                None => diffs.push(format!("{name} {n} != {v}")),
                            }
                        }
                        (Some(Value::Date(d)), Json::String(s)) => {
                            if parse_datetime(s).ok() != Some(d) {
                                diffs.push(format!("{name} {d} != {s}"));
                            }
                        }
                        (None, Json::Null) => {}
                        (None, Json::Number(n)) if n.as_f64() == Some(0.0) => {}
                        (None, _) => diffs.push(format!("{name} missing (expected {v})")),
                        _ => {}
                    }
                }
            }
        }
        if !diffs.is_empty() {
            problems.push(format!(
                "#{i} {} {}: {}",
                exp_obj.get("eventDate").and_then(|v| v.as_str()).unwrap_or("?"),
                exp_obj.get("eventType").and_then(|v| v.as_str()).unwrap_or("?"),
                diffs.join("; ")
            ));
        }
        if problems.len() > 6 {
            break;
        }
    }
    if problems.is_empty() { Ok(()) } else { Err(problems.join("\n    ")) }
}

/// Run all cases of a contract type. Returns (passed, total, failure descriptions).
pub fn run_suite(contract_type: &str) -> Option<(usize, usize, Vec<String>)> {
    let cases = load(contract_type)?;
    let mut failures = Vec::new();
    let total = cases.len();
    for (id, case) in &cases {
        let r = run_case(case).and_then(|res| compare(case, &res));
        if let Err(e) = r {
            failures.push(format!("{id}:\n    {e}"));
        }
    }
    Some((total - failures.len(), total, failures))
}
