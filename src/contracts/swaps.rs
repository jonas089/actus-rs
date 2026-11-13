//! SWAPS — generic swap of two embedded leg contracts (first leg `FIL`, second leg `SEL`).
//!
//! Each leg is a full ACTUS contract (e.g. PAM, ANN) evaluated with the same observer. The leg
//! roles follow from the swap's role: receiving the first leg (`RFL`) makes the first leg an
//! asset (`RPA`) and the second a liability (`RPL`); paying it (`PFL`) is the reverse.
//! With delivery settlement (`DS = D`) all leg events are reported as they are; with cash
//! settlement (`DS = S`) congruent IED/MD/PR/IP events of the two legs are netted.

use crate::contract::{settlement_fx, ContractModel};
use crate::error::{Error, Result};
use crate::events::{sort_key, Event, EventResult, EventType};
use crate::fixed::Fixed;
use crate::observer::RiskFactorObserver;
use crate::state::{StateSnapshot, StateVariable, Value};
use crate::terms::{CommonTerms, ContractRole, DeliverySettlement, RawTerms, TradeTerms};
use crate::time::DateTime;
use crate::{parse_contract_structure, Contract, ReferenceObject, ReferenceRole};

#[derive(Clone, Debug, PartialEq)]
pub struct SwapsTerms<const S: u32> {
    pub common: CommonTerms,
    pub delivery_settlement: DeliverySettlement,
    pub trade: TradeTerms<S>,
    pub first_leg: Box<Contract<S>>,
    pub second_leg: Box<Contract<S>>,
}

/// Find the embedded child contract with `role` and parse it with the given contract role.
pub(crate) fn embedded_child<const S: u32>(
    raw: &RawTerms,
    role: ReferenceRole,
    child_role: Option<ContractRole>,
    adjust: impl FnOnce(&mut RawTerms),
) -> Result<Contract<S>> {
    let refs = parse_contract_structure(raw)?;
    let r = refs
        .into_iter()
        .find(|r| r.role == role)
        .ok_or_else(|| Error::MissingTerm(format!("contractStructure ({role:?})")))?;
    let ReferenceObject::Contract(mut child) = r.object else {
        return Err(Error::Unsupported(format!("{role:?} must be an embedded contract")));
    };
    if let Some(cr) = child_role {
        child.0.insert("contractRole".into(), serde_json::Value::String(format!("{cr:?}")));
    }
    adjust(&mut child);
    Contract::from_raw(&child).map_err(|e| Error::Child(e.to_string()))
}

/// A state snapshot holding only status date, notional, rate and accrued interest.
pub(crate) fn flat_state<const S: u32>(t: DateTime, nt: Fixed<S>, ipnr: Fixed<S>, ipac: Fixed<S>) -> StateSnapshot<S> {
    let mut s = StateSnapshot::default();
    s.0.insert(StateVariable::StatusDate, Value::Date(t));
    s.0.insert(StateVariable::NotionalPrincipal, Value::Number(nt));
    s.0.insert(StateVariable::NominalInterestRate, Value::Number(ipnr));
    s.0.insert(StateVariable::AccruedInterest, Value::Number(ipac));
    s
}

fn num<const S: u32>(s: &StateSnapshot<S>, v: StateVariable) -> Fixed<S> {
    s.number(v).unwrap_or(Fixed::ZERO)
}

impl<const S: u32> SwapsTerms<S> {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        let common = CommonTerms::parse(raw)?;
        let receive_first = matches!(common.contract_role, ContractRole::RFL | ContractRole::RF);
        let (r1, r2) = if receive_first {
            (ContractRole::RPA, ContractRole::RPL)
        } else {
            (ContractRole::RPL, ContractRole::RPA)
        };
        Ok(SwapsTerms {
            delivery_settlement: raw.enum_or("deliverySettlement", DeliverySettlement::D)?,
            trade: TradeTerms::parse(raw)?,
            first_leg: Box::new(embedded_child(raw, ReferenceRole::FIL, Some(r1), |_| {})?),
            second_leg: Box::new(embedded_child(raw, ReferenceRole::SEL, Some(r2), |_| {})?),
            common,
        })
    }

    fn net_event(&self, a: &EventResult<S>, b: &EventResult<S>) -> EventResult<S> {
        let ev = Event::new(
            a.event.event_type,
            a.event.schedule_time,
            a.event.time,
            &a.event.currency,
            &self.common.contract_id,
        );
        let state = flat_state(
            a.event.time,
            num(&a.state, StateVariable::NotionalPrincipal) + num(&b.state, StateVariable::NotionalPrincipal),
            Fixed::ZERO,
            num(&a.state, StateVariable::AccruedInterest) + num(&b.state, StateVariable::AccruedInterest),
        );
        EventResult { event: ev, payoff: a.payoff + b.payoff, state }
    }

    /// Net congruent events of the two legs (cash settlement).
    fn net_legs(&self, leg1: &[EventResult<S>], leg2: &[EventResult<S>]) -> Vec<EventResult<S>> {
        let of = |l: &[EventResult<S>], ty: EventType| -> Vec<EventResult<S>> {
            let mut v: Vec<EventResult<S>> = l.iter().filter(|e| e.event.event_type == ty).cloned().collect();
            v.sort_by_key(|e| e.event.key());
            v
        };
        let mut out = Vec::new();
        // Single events (IED, MD): netted if they coincide, otherwise not reported.
        for ty in [EventType::IED, EventType::MD] {
            if let (Some(a), Some(b)) = (of(leg1, ty).first(), of(leg2, ty).first())
                && a.event.time == b.event.time
            {
                out.push(self.net_event(a, b));
            }
        }
        // Cyclic events (PR, IP): net pairs at the same time, pass the rest through.
        for ty in [EventType::PR, EventType::IP] {
            let mut a = of(leg1, ty);
            let mut b = of(leg2, ty);
            let mut i = 0;
            while i < a.len() {
                if let Some(j) = b.iter().position(|x| x.event.time == a[i].event.time) {
                    out.push(self.net_event(&a[i], &b[j]));
                    a.remove(i);
                    b.remove(j);
                } else {
                    i += 1;
                }
            }
            out.extend(a);
            out.extend(b);
        }
        out
    }

    /// Sum of the legs' accrued interest at `t`, taken from leg events at exactly `t`
    /// (zero for a leg without an event at `t`).
    fn legs_accrued_at(&self, t: DateTime, legs: [&[EventResult<S>]; 2]) -> Fixed<S> {
        legs.iter()
            .map(|l| {
                let at: Vec<&EventResult<S>> = l.iter().filter(|e| e.event.time == t).collect();
                if at.iter().any(|e| e.event.event_type == EventType::IP) {
                    return Fixed::ZERO;
                }
                at.iter()
                    .find(|e| e.event.event_type == EventType::PR)
                    .or(at.last())
                    .map(|e| num(&e.state, StateVariable::AccruedInterest))
                    .unwrap_or(Fixed::ZERO)
            })
            .sum()
    }

    /// Evaluate the swap: run both legs and combine their events.
    pub fn evaluate(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<EventResult<S>>> {
        let leg1 = self.first_leg.project(None, obs).map_err(|e| Error::Child(e.to_string()))?;
        let leg2 = self.second_leg.project(None, obs).map_err(|e| Error::Child(e.to_string()))?;
        let (cur, cid) = (&self.common.currency, &self.common.contract_id);
        let mut out: Vec<EventResult<S>> = Vec::new();
        if let Some(prd) = self.trade.purchase_date {
            let fx = settlement_fx(&self.common, prd, obs)?;
            let ipac = self.legs_accrued_at(prd, [&leg1, &leg2]);
            out.push(EventResult {
                event: Event::at(EventType::PRD, prd, cur, cid),
                payoff: fx * self.trade.price_at_purchase_date,
                state: flat_state(prd, Fixed::ZERO, Fixed::ZERO, ipac),
            });
        }
        if let Some(td) = self.trade.termination_date {
            let fx = settlement_fx(&self.common, td, obs)?;
            let ipac = self.legs_accrued_at(td, [&leg1, &leg2]);
            out.push(EventResult {
                event: Event::at(EventType::TD, td, cur, cid),
                payoff: fx * self.trade.price_at_termination_date + ipac,
                state: flat_state(td, Fixed::ZERO, Fixed::ZERO, ipac),
            });
        }
        if self.delivery_settlement == DeliverySettlement::S {
            out.extend(self.net_legs(&leg1, &leg2));
        } else {
            out.extend(leg1.iter().cloned());
            out.extend(leg2.iter().cloned());
        }
        if let Some(td) = self.trade.termination_date {
            let k = sort_key(td, EventType::TD);
            out.retain(|e| e.event.key() <= k);
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

impl<const S: u32> ContractModel<S> for SwapsTerms<S> {
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

    /// A swap event's outcome is determined by its legs: it is looked up in the legs'
    /// evaluation up to the event time.
    fn step(&self, _s: &StateSnapshot<S>, ev: &Event<S>, obs: &dyn RiskFactorObserver<S>) -> Result<(StateSnapshot<S>, Fixed<S>)> {
        self.evaluate(Some(ev.time), obs)?
            .into_iter()
            .find(|r| r.event == *ev)
            .map(|r| (r.state, r.payoff))
            .ok_or_else(|| Error::Unsupported(format!("event {} at {} is not part of the swap", ev.event_type, ev.time)))
    }

    fn project(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<EventResult<S>>> {
        self.evaluate(to, obs)
    }
}
