//! COM — Commodity (a position in a physical commodity, bought and sold at given prices).

use super::stk::OwnershipState;
use crate::contract::{settlement_fx, ContractModel};
use crate::error::{Error, Result};
use crate::events::{sort_events, Event, EventType};
use crate::fixed::Fixed;
use crate::observer::RiskFactorObserver;
use crate::terms::{CommonTerms, RawTerms, TradeTerms};
use crate::time::DateTime;

#[derive(Clone, Debug, PartialEq)]
pub struct ComTerms<const S: u32> {
    pub common: CommonTerms,
    pub quantity: Fixed<S>,
    pub unit: Option<String>,
    pub market_value_observed: Option<Fixed<S>>,
    pub trade: TradeTerms<S>,
}

impl<const S: u32> ComTerms<S> {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        Ok(ComTerms {
            common: CommonTerms::parse(raw)?,
            quantity: raw.fixed_or("quantity", Fixed::ONE)?,
            unit: raw.text("unit"),
            market_value_observed: raw.fixed("marketValueObserved")?,
            trade: TradeTerms::parse(raw)?,
        })
    }
}

impl<const S: u32> ContractModel<S> for ComTerms<S> {
    type State = OwnershipState<S>;

    fn common(&self) -> &CommonTerms {
        &self.common
    }

    fn initial_state(&self, _obs: &dyn RiskFactorObserver<S>) -> Result<OwnershipState<S>> {
        Ok(OwnershipState { status_date: self.common.status_date, contract_performance: self.common.contract_performance })
    }

    /// PRD and TD, each only if strictly after the status date and strictly before the horizon.
    fn schedule(&self, to: Option<DateTime>, _obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>> {
        let (cur, cid) = (&self.common.currency, &self.common.contract_id);
        let sd = self.common.status_date;
        let in_window = |t: DateTime| t > sd && to.is_none_or(|to| to > t);
        let mut evs = Vec::new();
        if let Some(prd) = self.trade.purchase_date.filter(|t| in_window(*t)) {
            evs.push(Event::at(EventType::PRD, prd, cur, cid));
        }
        if let Some(td) = self.trade.termination_date.filter(|t| in_window(*t)) {
            evs.push(Event::at(EventType::TD, td, cur, cid));
        }
        sort_events(&mut evs);
        Ok(evs)
    }

    fn step(
        &self,
        s0: &OwnershipState<S>,
        ev: &Event<S>,
        obs: &dyn RiskFactorObserver<S>,
    ) -> Result<(OwnershipState<S>, Fixed<S>)> {
        let t = ev.schedule_time;
        let fx = settlement_fx(&self.common, t, obs)?;
        let r = self.common.role_sign();
        let payoff = match ev.event_type {
            EventType::PRD => fx * r * -(self.quantity * self.trade.price_at_purchase_date),
            EventType::TD => fx * r * self.quantity * self.trade.price_at_termination_date,
            EventType::AD => Fixed::ZERO,
            other => return Err(Error::Unsupported(format!("event {other} for COM"))),
        };
        Ok((OwnershipState { status_date: t, ..s0.clone() }, payoff))
    }
}
