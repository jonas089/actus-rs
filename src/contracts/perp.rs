//! PERP — Perpetual future: a linear, cash-settled future with no maturity.
//!
//! Not an ACTUS contract type; see `docs/notes/perp.md` for what it borrows from the standard
//! and what is new.
//!
//! A position of `quantity` units is carried at a settlement price: the entry price
//! (`futuresPrice`) until the first settlement, then the price it was last settled at. Three
//! things move cash:
//!
//! * `FND`, on the funding cycle: the long side pays `quantity × price × rate`, the short side
//!   receives it (the reverse when the rate is negative). The rate is observed, not computed.
//! * `MR`, on the margining cycle: the gain or loss since the last settlement is paid and the
//!   position is re-marked to the observed price.
//! * `TD`, when the position is closed or liquidated: the gain or loss since the last settlement
//!   is paid at the close price.
//!
//! Without a margining cycle the whole gain or loss is paid at `TD`. The contract never ends on
//! its own, so projections need a horizon unless a termination is known.

use super::loan::clip_events;
use super::optns::underlying_market_object;
use crate::contract::{settlement_fx, ContractModel};
use crate::error::{Error, Result};
use crate::events::{sort_events, Event, EventType};
use crate::fixed::Fixed;
use crate::impl_state_view;
use crate::observer::RiskFactorObserver;
use crate::state::{StateVariable, Value};
use crate::terms::{anchor, CommonTerms, ContractPerformance, RawTerms};
use crate::time::{schedule, Cycle, DateTime};

/// State of a perpetual position.
#[derive(Clone, Debug, PartialEq)]
pub struct PerpState<const S: u32> {
    pub status_date: DateTime,
    /// The price the position is carried at: gains and losses up to it have been paid.
    pub settlement_price: Fixed<S>,
    /// `TE` once the position is closed.
    pub contract_performance: ContractPerformance,
}

impl_state_view!(PerpState {
    status_date => StatusDate,
    settlement_price => SettlementPrice,
    contract_performance => ContractPerformance,
});

#[derive(Clone, Debug, PartialEq)]
pub struct PerpTerms<const S: u32> {
    pub common: CommonTerms,
    /// Market object code of the price the position is marked at (`CTST` reference with role
    /// UDL).
    pub underlying: String,
    /// Position size in units of the underlying; the direction is the contract role.
    pub quantity: Fixed<S>,
    /// Entry price, or the price last settled at when the terms describe a running position.
    pub futures_price: Fixed<S>,
    pub funding_anchor: Option<DateTime>,
    pub funding_cycle: Option<Cycle>,
    /// Market object observed for the funding rate of a period, at the period's end (default
    /// `"<underlying>-FR"`).
    pub funding_market_object_code: String,
    pub margining_anchor: Option<DateTime>,
    pub margining_cycle: Option<Cycle>,
    pub termination_date: Option<DateTime>,
    /// Close price at the termination date; the observed price when absent.
    pub price_at_termination_date: Option<Fixed<S>>,
}

impl<const S: u32> PerpTerms<S> {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        let common = CommonTerms::parse(raw)?;
        let underlying = underlying_market_object(raw)?;
        let quantity: Fixed<S> = raw.fixed_or("quantity", Fixed::ONE)?;
        if !quantity.is_positive() {
            return Err(Error::invalid("quantity", "must be positive; the contract role gives the direction"));
        }
        // Both cycles start counting at the status date, so their first event is one cycle later.
        let start = Some(common.status_date);
        Ok(PerpTerms {
            quantity,
            futures_price: raw.req_fixed("futuresPrice")?,
            funding_anchor: anchor(raw, "cycleAnchorDateOfFunding", "cycleOfFunding", start)?,
            funding_cycle: raw.cycle("cycleOfFunding")?,
            funding_market_object_code: raw
                .text("marketObjectCodeOfFunding")
                .unwrap_or_else(|| format!("{underlying}-FR")),
            margining_anchor: anchor(raw, "cycleAnchorDateOfMargining", "cycleOfMargining", start)?,
            margining_cycle: raw.cycle("cycleOfMargining")?,
            termination_date: raw.date("terminationDate")?,
            price_at_termination_date: raw.fixed("priceAtTerminationDate")?,
            underlying,
            common,
        })
    }

    /// Observed price of the underlying at `t`.
    pub fn price(&self, t: DateTime, obs: &dyn RiskFactorObserver<S>) -> Result<Fixed<S>> {
        obs.risk_factor(&self.underlying, t)
    }

    /// When the position ends and at what price, if known: the termination date of the terms or
    /// the first observed `TD` event (a close or a liquidation), whichever is earlier. A zero
    /// price means the price observed at that time.
    fn close(&self, obs: &dyn RiskFactorObserver<S>) -> Option<(DateTime, Fixed<S>)> {
        let agreed = self.termination_date.map(|t| (t, self.price_at_termination_date.unwrap_or(Fixed::ZERO)));
        let observed = obs
            .events(&self.common.contract_id)
            .into_iter()
            .filter(|e| e.event_type == EventType::TD && e.time >= self.common.status_date)
            .map(|e| (e.time, e.value))
            .min_by_key(|(t, _)| *t);
        match (agreed, observed) {
            (Some(a), Some(o)) => Some(if o.0 < a.0 { o } else { a }),
            (a, o) => a.or(o),
        }
    }

    /// Cyclic events of one kind, from the anchor up to and including `end`.
    fn cyclic(&self, ty: EventType, anchor: Option<DateTime>, cycle: Option<Cycle>, end: DateTime) -> Vec<Event<S>> {
        let (Some(a), Some(c)) = (anchor, cycle) else { return Vec::new() };
        let (cur, cid) = (&self.common.currency, &self.common.contract_id);
        schedule(Some(a), end + chrono::Duration::milliseconds(1), Some(c), self.common.end_of_month, false)
            .into_iter()
            .map(|t| Event::new(ty, t, self.common.business_day.shift_event_time(t), cur, cid))
            .collect()
    }
}

impl<const S: u32> ContractModel<S> for PerpTerms<S> {
    type State = PerpState<S>;

    fn common(&self) -> &CommonTerms {
        &self.common
    }

    fn initial_state(&self, _obs: &dyn RiskFactorObserver<S>) -> Result<PerpState<S>> {
        Ok(PerpState {
            status_date: self.common.status_date,
            settlement_price: self.futures_price,
            contract_performance: self.common.contract_performance,
        })
    }

    fn schedule(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>> {
        let (cur, cid) = (&self.common.currency, &self.common.contract_id);
        let close = self.close(obs);
        let end = match (close.map(|(t, _)| t), to) {
            (Some(td), Some(to)) => td.min(to),
            (Some(t), None) | (None, Some(t)) => t,
            (None, None) => {
                return Err(Error::Unsupported("PERP has no maturity; an analysis horizon is required".into()));
            }
        };
        let mut evs = self.cyclic(EventType::FND, self.funding_anchor, self.funding_cycle, end);
        evs.extend(self.cyclic(EventType::MR, self.margining_anchor, self.margining_cycle, end));
        for o in obs.events(cid) {
            if matches!(o.event_type, EventType::CE | EventType::AD) {
                let mut e = Event::at(o.event_type, o.time, cur, cid);
                if let Some(Value::Performance(p)) = o.states.get(StateVariable::ContractPerformance) {
                    e.event_type = EventType::CE;
                    e.performance = Some(p);
                }
                evs.push(e);
            }
        }
        if let Some((td, price)) = close {
            // By time, not by sort key: nothing happens to a position after it is closed.
            evs.retain(|e| e.time <= td);
            let mut e = Event::at(EventType::TD, td, cur, cid);
            e.value = price;
            evs.push(e);
        }
        clip_events(&mut evs, self.common.status_date, to);
        sort_events(&mut evs);
        Ok(evs)
    }

    fn step(&self, s0: &PerpState<S>, ev: &Event<S>, obs: &dyn RiskFactorObserver<S>) -> Result<(PerpState<S>, Fixed<S>)> {
        let t = ev.schedule_time;
        let position: Fixed<S> = self.common.role_sign::<S>() * self.quantity;
        let mut s = PerpState { status_date: t, ..s0.clone() };
        // Pays the gain or loss from the settlement price to `price` and re-marks the position.
        let mut settle = |price: Fixed<S>| -> Result<Fixed<S>> {
            let payoff = settlement_fx(&self.common, t, obs)? * position * (price - s0.settlement_price);
            s.settlement_price = price;
            Ok(payoff)
        };
        let payoff = match ev.event_type {
            EventType::FND => {
                let rate = obs.risk_factor(&self.funding_market_object_code, t)?;
                settlement_fx(&self.common, t, obs)? * -(position * self.price(t, obs)? * rate)
            }
            EventType::MR => settle(self.price(t, obs)?)?,
            EventType::TD => {
                let price = if ev.value.is_zero() { self.price(t, obs)? } else { ev.value };
                let payoff = settle(price)?;
                s.contract_performance = ContractPerformance::TE;
                payoff
            }
            EventType::CE => {
                s.contract_performance = ev.performance.unwrap_or(ContractPerformance::DF);
                Fixed::ZERO
            }
            EventType::AD => Fixed::ZERO,
            other => return Err(Error::Unsupported(format!("event {other} for PERP"))),
        };
        Ok((s, payoff))
    }
}
