//! SWAPS — generic swap of two embedded leg contracts (first leg `FIL`, second leg `SEL`).
//!
//! Each leg is a full ACTUS contract (e.g. PAM, ANN) evaluated with the same observer. The leg
//! roles follow from the swap's role: receiving the first leg (`RFL`) makes the first leg an
//! asset (`RPA`) and the second a liability (`RPL`); paying it (`PFL`) is the reverse.
//! With delivery settlement (`DS = D`) all leg events are reported as they are; with cash
//! settlement (`DS = S`) congruent IED/MD/PR/IP events of the two legs are netted.
//!
//! The swap is a state machine of its own: its state holds both legs' states, and every swap
//! event carries the leg events it applies (`Event::components`). Leg events that the swap does
//! not report (e.g. rate resets under cash settlement) are still applied, as hidden events, so
//! the legs' states stay exact.

use crate::any::AnyState;
use crate::contract::{settlement_fx, ContractModel};
use crate::error::{Error, Result};
use crate::events::{sort_key, Event, EventType};
use crate::fixed::Fixed;
use crate::observer::RiskFactorObserver;
use crate::state::{StateSnapshot, StateVariable, StateView, Value};
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

/// State of a combined contract: its children's states plus what it reports.
#[derive(Clone, Debug, PartialEq)]
pub struct SwapsState<const S: u32> {
    pub children: Vec<AnyState<S>>,
    /// The state reported after the last event (a leg's snapshot, or netted values).
    pub report: StateSnapshot<S>,
}

impl<const S: u32> StateView<S> for SwapsState<S> {
    fn get(&self, var: StateVariable) -> Option<Value<S>> {
        self.report.get(var)
    }
    fn variables(&self) -> &'static [StateVariable] {
        self.report.variables()
    }
    fn check(&self, context: &str) -> Result<()> {
        self.report.check(context)?;
        self.children.iter().try_for_each(|c| c.check(context))
    }
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

// ---------------------------------------------------------------------------------------------
// Machinery shared by combined contracts whose children are embedded (SWAPS, CAPFL)
// ---------------------------------------------------------------------------------------------

/// The children's own schedules, and for each child event whether the child reports it.
pub(crate) struct ChildSchedules<const S: u32> {
    pub events: Vec<Vec<Event<S>>>,
    pub reported: Vec<Vec<bool>>,
}

impl<const S: u32> ChildSchedules<S> {
    pub fn new(children: &[&Contract<S>], obs: &dyn RiskFactorObserver<S>) -> Result<Self> {
        let mut events = Vec::new();
        let mut reported = Vec::new();
        for c in children {
            let evs = c.schedule(None, obs).map_err(|e| Error::Child(e.to_string()))?;
            reported.push(evs.iter().map(|e| c.is_reported(e, &evs)).collect());
            events.push(evs);
        }
        Ok(ChildSchedules { events, reported })
    }

    /// Indices of reported events of `ty` in child `i`.
    pub fn reported_of(&self, i: usize, ty: EventType) -> Vec<usize> {
        (0..self.events[i].len()).filter(|j| self.reported[i][*j] && self.events[i][*j].event_type == ty).collect()
    }
}

/// A parent event that applies exactly one child event and looks like it.
pub(crate) fn single<const S: u32>(i: usize, e: &Event<S>) -> Event<S> {
    let mut p = e.clone();
    p.components = vec![(i as u8, e.clone())];
    p
}

/// A parent event combining one event of child 0 and one of child 1.
pub(crate) fn combined<const S: u32>(a: &Event<S>, b: &Event<S>, contract_id: &str) -> Event<S> {
    let mut p = Event::new(a.event_type, a.schedule_time, a.time, &a.currency, contract_id);
    p.components = vec![(0, a.clone()), (1, b.clone())];
    p
}

/// Sort parent events by time/sequence; ties follow the children's own event order so that
/// every child sees its events in its own schedule order. Events without components (the
/// parent's own PRD/TD) go first among ties.
pub(crate) fn order_parent_events<const S: u32>(out: &mut [Event<S>], children: &ChildSchedules<S>) {
    let mut merged: Vec<(i64, usize, usize)> = Vec::new();
    for (i, evs) in children.events.iter().enumerate() {
        for (j, e) in evs.iter().enumerate() {
            merged.push((e.key(), i, j));
        }
    }
    merged.sort_by_key(|m| m.0);
    let rank = |i: usize, e: &Event<S>| {
        merged
            .iter()
            .position(|m| m.1 == i && children.events[i][m.2] == *e)
            .map(|p| p as i64)
            .unwrap_or(i64::MAX)
    };
    let mut keyed: Vec<(i64, i64, Event<S>)> = out
        .iter()
        .map(|p| {
            let r = p.components.iter().map(|(i, e)| rank(*i as usize, e)).min().unwrap_or(-1);
            (p.key(), r, p.clone())
        })
        .collect();
    keyed.sort_by_key(|k| (k.0, k.1));
    for (slot, k) in out.iter_mut().zip(keyed) {
        *slot = k.2;
    }
}

/// Drop events after the termination date and after the horizon `to`.
pub(crate) fn clip_parent_events<const S: u32>(out: &mut Vec<Event<S>>, td: Option<DateTime>, to: Option<DateTime>) {
    if let Some(td) = td {
        let k = sort_key(td, EventType::TD);
        out.retain(|e| e.key() <= k);
    }
    if let Some(to) = to {
        let k = sort_key(to, EventType::AD);
        out.retain(|e| e.key() <= k);
    }
}

/// Reporting window of a combined contract: from the status date (as an AD event) and the
/// purchase date on; and every child event must be one its child reports.
pub(crate) fn in_report_window<const S: u32>(
    ev: &Event<S>,
    all: &[Event<S>],
    common: &CommonTerms,
    purchase: Option<DateTime>,
    children: &[&Contract<S>],
) -> bool {
    let k = ev.key();
    if k < sort_key(common.status_date, EventType::AD) {
        return false;
    }
    if purchase.is_some_and(|prd| k < sort_key(prd, EventType::PRD)) {
        return false;
    }
    ev.components.iter().all(|(i, ce)| {
        let i = *i as usize;
        let child_all: Vec<Event<S>> =
            all.iter().flat_map(|p| p.components.iter()).filter(|(j, _)| *j as usize == i).map(|(_, e)| e.clone()).collect();
        children[i].is_reported(ce, &child_all)
    })
}

/// Apply a parent event's components to the children. Returns the new child states and each
/// component's payoff.
pub(crate) fn apply_components<const S: u32>(
    children: &[&Contract<S>],
    states: &[AnyState<S>],
    ev: &Event<S>,
    obs: &dyn RiskFactorObserver<S>,
) -> Result<(Vec<AnyState<S>>, Vec<Fixed<S>>)> {
    let mut next = states.to_vec();
    let mut payoffs = Vec::with_capacity(ev.components.len());
    for (i, ce) in &ev.components {
        let i = *i as usize;
        let (s, p) = children[i].step(&next[i], ce, obs).map_err(|e| Error::Child(e.to_string()))?;
        next[i] = s;
        payoffs.push(p);
    }
    Ok((next, payoffs))
}

/// A child's accrued interest as seen by a parent PRD/TD event at `t` (key `parent_key`),
/// following the reference behaviour: 0 if the child has an IP at `t` or no event at `t`;
/// otherwise the accrued interest after the child's PR at `t`, else after its last event at
/// `t` (an MD/TD after the parent event leaves 0).
pub(crate) fn child_accrued_at<const S: u32>(
    child: &Contract<S>,
    state: &AnyState<S>,
    t: DateTime,
    parent_key: i64,
    obs: &dyn RiskFactorObserver<S>,
) -> Result<Fixed<S>> {
    let evs = child.schedule(None, obs).map_err(|e| Error::Child(e.to_string()))?;
    let at: Vec<&Event<S>> = evs.iter().filter(|e| e.time == t && child.is_reported(e, &evs)).collect();
    if at.is_empty() || at.iter().any(|e| e.event_type == EventType::IP) {
        return Ok(Fixed::ZERO);
    }
    let current = state.number(StateVariable::AccruedInterest);
    if at.iter().any(|e| e.event_type == EventType::PR) {
        return Ok(current);
    }
    let last = at.last().expect("non-empty");
    if last.key() > parent_key && matches!(last.event_type, EventType::MD | EventType::TD) {
        return Ok(Fixed::ZERO);
    }
    Ok(current)
}

// ---------------------------------------------------------------------------------------------
// SWAPS
// ---------------------------------------------------------------------------------------------

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

    fn legs(&self) -> [&Contract<S>; 2] {
        [&self.first_leg, &self.second_leg]
    }

    /// Cash settlement: net congruent IED/MD (only if at the same time, otherwise not reported)
    /// and PR/IP pairs at the same time; everything else is applied but not reported.
    fn net_schedule(&self, ch: &ChildSchedules<S>) -> Vec<Event<S>> {
        let cid = &self.common.contract_id;
        let mut used = [vec![false; ch.events[0].len()], vec![false; ch.events[1].len()]];
        let mut out = Vec::new();
        for ty in [EventType::IED, EventType::MD] {
            if let (Some(&a), Some(&b)) = (ch.reported_of(0, ty).first(), ch.reported_of(1, ty).first())
                && ch.events[0][a].time == ch.events[1][b].time
            {
                out.push(combined(&ch.events[0][a], &ch.events[1][b], cid));
                used[0][a] = true;
                used[1][b] = true;
            }
        }
        for ty in [EventType::PR, EventType::IP] {
            let bs = ch.reported_of(1, ty);
            for a in ch.reported_of(0, ty) {
                let t = ch.events[0][a].time;
                if let Some(&b) = bs.iter().find(|b| !used[1][**b] && ch.events[1][**b].time == t) {
                    out.push(combined(&ch.events[0][a], &ch.events[1][b], cid));
                    used[0][a] = true;
                    used[1][b] = true;
                }
            }
        }
        for (i, evs) in ch.events.iter().enumerate() {
            for (j, e) in evs.iter().enumerate() {
                if !used[i][j] {
                    out.push(single(i, e));
                }
            }
        }
        out
    }
}

impl<const S: u32> ContractModel<S> for SwapsTerms<S> {
    type State = SwapsState<S>;

    fn common(&self) -> &CommonTerms {
        &self.common
    }

    fn initial_state(&self, obs: &dyn RiskFactorObserver<S>) -> Result<SwapsState<S>> {
        let children = self
            .legs()
            .iter()
            .map(|c| c.initial_state(obs).map_err(|e| Error::Child(e.to_string())))
            .collect::<Result<Vec<_>>>()?;
        Ok(SwapsState {
            children,
            report: flat_state(self.common.status_date, Fixed::ZERO, Fixed::ZERO, Fixed::ZERO),
        })
    }

    fn schedule(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>> {
        let ch = ChildSchedules::new(&self.legs(), obs)?;
        let (cur, cid) = (&self.common.currency, &self.common.contract_id);
        let mut out = Vec::new();
        if let Some(prd) = self.trade.purchase_date {
            out.push(Event::at(EventType::PRD, prd, cur, cid));
        }
        if let Some(td) = self.trade.termination_date {
            out.push(Event::at(EventType::TD, td, cur, cid));
        }
        match self.delivery_settlement {
            DeliverySettlement::S => out.extend(self.net_schedule(&ch)),
            DeliverySettlement::D => {
                for (i, evs) in ch.events.iter().enumerate() {
                    out.extend(evs.iter().map(|e| single(i, e)));
                }
            }
        }
        clip_parent_events(&mut out, self.trade.termination_date, to);
        order_parent_events(&mut out, &ch);
        Ok(out)
    }

    fn is_reported(&self, ev: &Event<S>, all: &[Event<S>]) -> bool {
        if !in_report_window(ev, all, &self.common, self.trade.purchase_date, &self.legs()) {
            return false;
        }
        match (self.delivery_settlement, ev.components.len()) {
            (_, 0) | (_, 2) | (DeliverySettlement::D, _) => true,
            (DeliverySettlement::S, _) => matches!(ev.event_type, EventType::PR | EventType::IP),
        }
    }

    fn step(&self, s: &SwapsState<S>, ev: &Event<S>, obs: &dyn RiskFactorObserver<S>) -> Result<(SwapsState<S>, Fixed<S>)> {
        let t = ev.time;
        if ev.components.is_empty() {
            let fx = settlement_fx(&self.common, t, obs)?;
            let mut ipac = Fixed::ZERO;
            for (leg, st) in self.legs().iter().zip(&s.children) {
                ipac += child_accrued_at(leg, st, t, ev.key(), obs)?;
            }
            let payoff = match ev.event_type {
                EventType::PRD => fx * self.trade.price_at_purchase_date,
                EventType::TD => fx * self.trade.price_at_termination_date + ipac,
                other => return Err(Error::Unsupported(format!("event {other} for SWAPS"))),
            };
            let report = flat_state(t, Fixed::ZERO, Fixed::ZERO, ipac);
            return Ok((SwapsState { children: s.children.clone(), report }, payoff));
        }
        let (children, payoffs) = apply_components(&self.legs(), &s.children, ev, obs)?;
        let payoff = payoffs.iter().copied().sum();
        let report = if ev.components.len() == 2 {
            let n = |v| children[0].number(v) + children[1].number(v);
            flat_state(t, n(StateVariable::NotionalPrincipal), Fixed::ZERO, n(StateVariable::AccruedInterest))
        } else {
            children[ev.components[0].0 as usize].snapshot()
        };
        Ok((SwapsState { children, report }, payoff))
    }
}
