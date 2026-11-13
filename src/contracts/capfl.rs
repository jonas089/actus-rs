//! CAPFL — Cap / Floor on the variable rate of an embedded underlying loan (`UDL`).
//!
//! The underlying is evaluated twice as an asset (`RPA`): once as given, and once with the
//! cap's `lifeCap`/`lifeFloor` applied to its rate resets. Each interest payment of the cap
//! pays the absolute difference of the two interest payments (to the buyer, sign by role).

use super::swaps::{embedded_child, flat_state};
use crate::contract::{settlement_fx, ContractModel};
use crate::error::{Error, Result};
use crate::events::{sort_key, Event, EventResult, EventType};
use crate::fixed::Fixed;
use crate::observer::RiskFactorObserver;
use crate::state::{StateSnapshot, StateVariable};
use crate::terms::{CommonTerms, ContractRole, RawTerms, TradeTerms};
use crate::time::DateTime;
use crate::{Contract, ReferenceRole};

#[derive(Clone, Debug, PartialEq)]
pub struct CapflTerms<const S: u32> {
    pub common: CommonTerms,
    pub life_cap: Option<Fixed<S>>,
    pub life_floor: Option<Fixed<S>>,
    pub trade: TradeTerms<S>,
    /// The underlying as given.
    pub underlying: Box<Contract<S>>,
    /// The underlying with the cap/floor applied to its rate.
    pub underlying_capped: Box<Contract<S>>,
}

impl<const S: u32> CapflTerms<S> {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        let life_cap = raw.fixed("lifeCap")?;
        let life_floor = raw.fixed("lifeFloor")?;
        let set = |key: &str, v: Option<Fixed<S>>, t: &mut RawTerms| match v {
            Some(x) => {
                t.0.insert(key.into(), serde_json::Value::String(x.to_string()));
            }
            None => {
                t.0.remove(key);
            }
        };
        let underlying = embedded_child(raw, ReferenceRole::UDL, Some(ContractRole::RPA), |_| {})?;
        let underlying_capped = embedded_child(raw, ReferenceRole::UDL, Some(ContractRole::RPA), |t| {
            set("lifeCap", life_cap, t);
            set("lifeFloor", life_floor, t);
        })?;
        Ok(CapflTerms {
            common: CommonTerms::parse(raw)?,
            life_cap,
            life_floor,
            trade: TradeTerms::parse(raw)?,
            underlying: Box::new(underlying),
            underlying_capped: Box::new(underlying_capped),
        })
    }

    /// Absolute difference of the two underlyings' accrued interest at `t`.
    fn accrued_difference(&self, t: DateTime, plain: &[EventResult<S>], capped: &[EventResult<S>]) -> Fixed<S> {
        let k = sort_key(t, EventType::AD);
        let at = |l: &[EventResult<S>]| {
            l.iter()
                .rfind(|e| e.event.key() <= k)
                .and_then(|e| e.state.number(StateVariable::AccruedInterest))
                .unwrap_or(Fixed::ZERO)
        };
        (at(plain) - at(capped)).abs()
    }

    pub fn evaluate(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<EventResult<S>>> {
        let plain = self.underlying.project(None, obs).map_err(|e| Error::Child(e.to_string()))?;
        let capped = self.underlying_capped.project(None, obs).map_err(|e| Error::Child(e.to_string()))?;
        let r = self.common.role_sign::<S>();
        let (cur, cid) = (&self.common.currency, &self.common.contract_id);
        let ips = |l: &[EventResult<S>]| -> Vec<EventResult<S>> {
            l.iter().filter(|e| e.event.event_type == EventType::IP).cloned().collect()
        };
        let (p_ip, mut c_ip) = (ips(&plain), ips(&capped));
        let mut out = Vec::new();
        for a in p_ip {
            match c_ip.iter().position(|b| b.event.time == a.event.time) {
                Some(j) => {
                    let b = c_ip.remove(j);
                    let t = a.event.time;
                    let fx = settlement_fx(&self.common, a.event.schedule_time, obs)?;
                    out.push(EventResult {
                        event: Event::new(EventType::IP, a.event.schedule_time, t, &a.event.currency, cid),
                        payoff: fx * r * (a.payoff - b.payoff).abs(),
                        state: flat_state(t, Fixed::ZERO, Fixed::ZERO, Fixed::ZERO),
                    });
                }
                None => out.push(a),
            }
        }
        out.extend(c_ip);
        if let Some(prd) = self.trade.purchase_date {
            let fx = settlement_fx(&self.common, prd, obs)?;
            let ipac = r * self.accrued_difference(prd, &plain, &capped);
            out.push(EventResult {
                event: Event::at(EventType::PRD, prd, cur, cid),
                payoff: fx * (-self.trade.price_at_purchase_date + ipac),
                state: flat_state(prd, Fixed::ZERO, Fixed::ZERO, ipac),
            });
        }
        if let Some(td) = self.trade.termination_date {
            let fx = settlement_fx(&self.common, td, obs)?;
            let ipac = r * self.accrued_difference(td, &plain, &capped);
            let k = sort_key(td, EventType::TD);
            out.retain(|e| e.event.key() <= k);
            out.push(EventResult {
                event: Event::at(EventType::TD, td, cur, cid),
                payoff: fx * (self.trade.price_at_termination_date + ipac),
                state: flat_state(td, Fixed::ZERO, Fixed::ZERO, Fixed::ZERO),
            });
        }
        let lo = sort_key(self.common.status_date, EventType::AD);
        out.retain(|e| e.event.key() >= lo);
        if let Some(to) = to {
            let hi = sort_key(to, EventType::AD);
            out.retain(|e| e.event.key() <= hi);
        }
        if let Some(prd) = self.trade.purchase_date {
            let k = sort_key(prd, EventType::PRD);
            out.retain(|e| e.event.key() >= k);
        }
        out.sort_by_key(|e| e.event.key());
        Ok(out)
    }
}

impl<const S: u32> ContractModel<S> for CapflTerms<S> {
    type State = StateSnapshot<S>;

    fn common(&self) -> &CommonTerms {
        &self.common
    }

    fn initial_state(&self, _obs: &dyn RiskFactorObserver<S>) -> Result<StateSnapshot<S>> {
        Ok(flat_state(self.common.status_date, Fixed::ZERO, Fixed::ZERO, Fixed::ZERO))
    }

    fn schedule(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>> {
        Ok(self.evaluate(to, obs)?.into_iter().map(|r| r.event).collect())
    }

    fn step(&self, _s: &StateSnapshot<S>, ev: &Event<S>, obs: &dyn RiskFactorObserver<S>) -> Result<(StateSnapshot<S>, Fixed<S>)> {
        self.evaluate(Some(ev.time), obs)?
            .into_iter()
            .find(|r| r.event == *ev)
            .map(|r| (r.state, r.payoff))
            .ok_or_else(|| Error::Unsupported(format!("event {} at {} is not part of the cap/floor", ev.event_type, ev.time)))
    }

    fn project(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<EventResult<S>>> {
        self.evaluate(to, obs)
    }
}
