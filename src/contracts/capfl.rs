//! CAPFL — Cap / Floor on the variable rate of an embedded underlying loan (`UDL`).
//!
//! The underlying is carried twice as an asset (`RPA`): once as given, and once with the cap's
//! `lifeCap`/`lifeFloor` applied to its rate resets. Each interest payment of the cap pays the
//! absolute difference of the two interest payments (to the buyer, sign by role).
//!
//! Like SWAPS, the cap is a state machine of its own: its state holds both underlyings' states
//! and each cap event carries the underlying events it applies. Underlying events other than
//! interest payments (IED, RR, PR, MD, ...) are applied but not reported.

use super::swaps::{
    apply_components, clip_parent_events, combined, embedded_child, flat_state, in_report_window, order_parent_events,
    single, ChildSchedules, SwapsState,
};
use crate::any::AnyState;
use crate::contract::{settlement_fx, ContractModel};
use crate::error::{Error, Result};
use crate::events::{Event, EventType};
use crate::fixed::Fixed;
use crate::observer::RiskFactorObserver;
use crate::state::{StateVariable, StateView};
use crate::terms::{CommonTerms, ContractRole, RawTerms, TradeTerms};
use crate::time::DateTime;
use crate::{Contract, ReferenceRole};

/// State of a cap/floor: both underlyings' states and the reported state.
pub type CapflState<const S: u32> = SwapsState<S>;

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

    fn children(&self) -> [&Contract<S>; 2] {
        [&self.underlying, &self.underlying_capped]
    }

    /// Absolute difference of the two underlyings' accrued interest as of a parent event at
    /// `t` (state after their last event up to that day; an MD/TD later that day leaves 0).
    fn accrued_difference(
        &self,
        states: &[AnyState<S>],
        t: DateTime,
        parent_key: i64,
        obs: &dyn RiskFactorObserver<S>,
    ) -> Result<Fixed<S>> {
        let mut acc = [Fixed::ZERO; 2];
        for (i, child) in self.children().iter().enumerate() {
            let evs = child.schedule(None, obs).map_err(|e| Error::Child(e.to_string()))?;
            let day_end = crate::events::sort_key(t, EventType::AD);
            let closed_later = evs.iter().any(|e| {
                e.key() > parent_key
                    && e.key() <= day_end
                    && matches!(e.event_type, EventType::MD | EventType::TD)
                    && child.is_reported(e, &evs)
            });
            acc[i] = if closed_later { Fixed::ZERO } else { states[i].number(StateVariable::AccruedInterest) };
        }
        Ok((acc[0] - acc[1]).abs())
    }
}

impl<const S: u32> ContractModel<S> for CapflTerms<S> {
    type State = CapflState<S>;

    fn common(&self) -> &CommonTerms {
        &self.common
    }

    fn initial_state(&self, obs: &dyn RiskFactorObserver<S>) -> Result<CapflState<S>> {
        let children = self
            .children()
            .iter()
            .map(|c| c.initial_state(obs).map_err(|e| Error::Child(e.to_string())))
            .collect::<Result<Vec<_>>>()?;
        Ok(SwapsState {
            children,
            report: flat_state(self.common.status_date, Fixed::ZERO, Fixed::ZERO, Fixed::ZERO),
        })
    }

    fn schedule(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>> {
        let ch = ChildSchedules::new(&self.children(), obs)?;
        let (cur, cid) = (&self.common.currency, &self.common.contract_id);
        let mut used = [vec![false; ch.events[0].len()], vec![false; ch.events[1].len()]];
        let mut out = Vec::new();
        // Interest payments at the same time are combined into one cap payment.
        let capped_ips = ch.reported_of(1, EventType::IP);
        for a in ch.reported_of(0, EventType::IP) {
            let t = ch.events[0][a].time;
            if let Some(&b) = capped_ips.iter().find(|b| !used[1][**b] && ch.events[1][**b].time == t) {
                out.push(combined(&ch.events[0][a], &ch.events[1][b], cid));
                used[0][a] = true;
                used[1][b] = true;
            }
        }
        for (i, evs) in ch.events.iter().enumerate() {
            for (j, e) in evs.iter().enumerate() {
                if !used[i][j] {
                    out.push(single(i, e));
                }
            }
        }
        if let Some(prd) = self.trade.purchase_date {
            out.push(Event::at(EventType::PRD, prd, cur, cid));
        }
        if let Some(td) = self.trade.termination_date {
            out.push(Event::at(EventType::TD, td, cur, cid));
        }
        clip_parent_events(&mut out, self.trade.termination_date, to);
        order_parent_events(&mut out, &ch);
        Ok(out)
    }

    fn is_reported(&self, ev: &Event<S>, all: &[Event<S>]) -> bool {
        in_report_window(ev, all, &self.common, self.trade.purchase_date, &self.children())
            && (ev.components.len() != 1 || ev.event_type == EventType::IP)
    }

    fn step(&self, s: &CapflState<S>, ev: &Event<S>, obs: &dyn RiskFactorObserver<S>) -> Result<(CapflState<S>, Fixed<S>)> {
        let t = ev.time;
        let r = self.common.role_sign::<S>();
        if ev.components.is_empty() {
            let fx = settlement_fx(&self.common, t, obs)?;
            let ipac = r * self.accrued_difference(&s.children, t, ev.key(), obs)?;
            let (payoff, report) = match ev.event_type {
                EventType::PRD => {
                    (fx * (-self.trade.price_at_purchase_date + ipac), flat_state(t, Fixed::ZERO, Fixed::ZERO, ipac))
                }
                EventType::TD => (
                    fx * (self.trade.price_at_termination_date + ipac),
                    flat_state(t, Fixed::ZERO, Fixed::ZERO, Fixed::ZERO),
                ),
                other => return Err(Error::Unsupported(format!("event {other} for CAPFL"))),
            };
            return Ok((SwapsState { children: s.children.clone(), report }, payoff));
        }
        let (children, payoffs) = apply_components(&self.children(), &s.children, ev, obs)?;
        let (payoff, report) = if ev.components.len() == 2 {
            let fx = settlement_fx(&self.common, ev.schedule_time, obs)?;
            (fx * r * (payoffs[0] - payoffs[1]).abs(), flat_state(t, Fixed::ZERO, Fixed::ZERO, Fixed::ZERO))
        } else {
            (payoffs[0], children[ev.components[0].0 as usize].snapshot())
        };
        Ok((SwapsState { children, report }, payoff))
    }
}
