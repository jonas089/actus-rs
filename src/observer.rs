//! Inputs from outside the contract: market data and unscheduled events.
//!
//! The engine never reads anything except through [`RiskFactorObserver`], so every input a
//! computation used can be recorded ([`Recorder`]), replayed ([`MarketData`]) or declared up
//! front ([`required_inputs`](crate::contract::ContractModel::required_inputs)).

use crate::error::{Error, Result};
use crate::events::EventType;
use crate::fixed::Fixed;
use crate::state::StateSnapshot;
use crate::time::DateTime;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

/// Source of risk-factor observations (rates, prices, indices, FX) and unscheduled events.
pub trait RiskFactorObserver<const S: u32> {
    /// Value of risk factor `id` at `time`.
    fn risk_factor(&self, id: &str, time: DateTime) -> Result<Fixed<S>>;

    /// Unscheduled events (prepayments, credit events, exercises, ...) for a contract.
    fn events(&self, _contract_id: &str) -> Vec<ObservedEvent<S>> {
        Vec::new()
    }
}

/// An unscheduled event observed for a contract.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ObservedEvent<const S: u32> {
    pub contract_id: String,
    pub event_type: EventType,
    pub time: DateTime,
    pub value: Fixed<S>,
    /// State overrides carried by the event (e.g. `contractPerformance: DF` for a credit event).
    pub states: StateSnapshot<S>,
}

/// In-memory market data: exact-time lookups of risk factor series plus observed events.
#[derive(Clone, Debug, Default)]
pub struct MarketData<const S: u32> {
    pub series: BTreeMap<String, BTreeMap<DateTime, Fixed<S>>>,
    pub events: Vec<ObservedEvent<S>>,
}

impl<const S: u32> MarketData<S> {
    pub fn new() -> Self {
        MarketData { series: BTreeMap::new(), events: Vec::new() }
    }

    pub fn insert(&mut self, id: &str, time: DateTime, value: Fixed<S>) {
        self.series.entry(id.to_string()).or_default().insert(time, value);
    }
}

impl<const S: u32> RiskFactorObserver<S> for MarketData<S> {
    fn risk_factor(&self, id: &str, time: DateTime) -> Result<Fixed<S>> {
        self.series
            .get(id)
            .and_then(|s| s.get(&time))
            .copied()
            .ok_or_else(|| Error::MissingObservation { id: id.to_string(), time })
    }

    fn events(&self, contract_id: &str) -> Vec<ObservedEvent<S>> {
        self.events.iter().filter(|e| e.contract_id == contract_id).cloned().collect()
    }
}

/// Wraps an observer and records every risk factor observation that was read.
pub struct Recorder<'a, const S: u32> {
    inner: &'a dyn RiskFactorObserver<S>,
    log: RefCell<BTreeMap<(String, DateTime), Fixed<S>>>,
}

impl<'a, const S: u32> Recorder<'a, S> {
    pub fn new(inner: &'a dyn RiskFactorObserver<S>) -> Self {
        Recorder { inner, log: RefCell::new(BTreeMap::new()) }
    }

    /// The recorded observations as replayable market data.
    pub fn into_market_data(self, events: Vec<ObservedEvent<S>>) -> MarketData<S> {
        let mut md = MarketData::new();
        for ((id, t), v) in self.log.into_inner() {
            md.insert(&id, t, v);
        }
        md.events = events;
        md
    }
}

impl<const S: u32> RiskFactorObserver<S> for Recorder<'_, S> {
    fn risk_factor(&self, id: &str, time: DateTime) -> Result<Fixed<S>> {
        let v = self.inner.risk_factor(id, time)?;
        self.log.borrow_mut().insert((id.to_string(), time), v);
        Ok(v)
    }
    fn events(&self, contract_id: &str) -> Vec<ObservedEvent<S>> {
        self.inner.events(contract_id)
    }
}

/// Observer that answers every query with a placeholder value and records which observations
/// were requested. Used to compute the inputs an event needs before they are available.
#[derive(Default)]
pub struct Probe {
    pub requested: RefCell<BTreeSet<(String, DateTime)>>,
}

impl<const S: u32> RiskFactorObserver<S> for Probe {
    fn risk_factor(&self, id: &str, time: DateTime) -> Result<Fixed<S>> {
        self.requested.borrow_mut().insert((id.to_string(), time));
        Ok(Fixed::ONE)
    }
}

/// An observer with no data at all.
pub struct NoMarketData;

impl<const S: u32> RiskFactorObserver<S> for NoMarketData {
    fn risk_factor(&self, id: &str, time: DateTime) -> Result<Fixed<S>> {
        Err(Error::MissingObservation { id: id.to_string(), time })
    }
}
