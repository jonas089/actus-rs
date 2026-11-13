//! FXOUT — Foreign Exchange Outright (FX forward / swap leg).
//!
//! The creator receives `NT` in `CUR` and pays `NT2` in `CUR2` at maturity (delivery), or the
//! net difference in `CUR` valued at the `CUR2/CUR` rate observed at maturity (cash settlement).

use super::loan::clip_events;
use crate::contract::{settlement_fx, ContractModel};
use crate::error::{Error, Result};
use crate::events::{sort_events, Event, EventType};
use crate::fixed::Fixed;
use crate::impl_state_view;
use crate::observer::RiskFactorObserver;
use crate::terms::{CommonTerms, ContractPerformance, DeliverySettlement, RawTerms, TradeTerms};
use crate::time::{DateTime, Period};

#[derive(Clone, Debug, PartialEq)]
pub struct FxoutTerms<const S: u32> {
    pub common: CommonTerms,
    pub currency2: String,
    pub maturity_date: DateTime,
    pub notional_principal: Fixed<S>,
    pub notional_principal2: Fixed<S>,
    pub delivery_settlement: DeliverySettlement,
    pub settlement_period: Period,
    pub trade: TradeTerms<S>,
}

/// Parse an ACTUS period that may carry a stub suffix (`P1DL0` is accepted as `P1D`).
pub(crate) fn period_lenient(raw: &RawTerms, key: &str) -> Result<Option<Period>> {
    match raw.text(key) {
        None => Ok(None),
        Some(s) => {
            let p = match s.split_once('L') {
                Some((p, _)) => p,
                None => s.as_str(),
            };
            Period::parse(p).map(Some).map_err(|_| Error::invalid(key, s.clone()))
        }
    }
}

impl<const S: u32> FxoutTerms<S> {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        Ok(FxoutTerms {
            common: CommonTerms::parse(raw)?,
            currency2: raw.req_text("currency2")?,
            maturity_date: raw.req_date("maturityDate")?,
            notional_principal: raw.req_fixed("notionalPrincipal")?,
            notional_principal2: raw.req_fixed("notionalPrincipal2")?,
            delivery_settlement: raw.enum_or("deliverySettlement", DeliverySettlement::D)?,
            settlement_period: period_lenient(raw, "settlementPeriod")?.unwrap_or(Period::ZERO),
            trade: TradeTerms::parse(raw)?,
        })
    }
}

/// State of an FXOUT contract (it carries no balances, only status and performance).
#[derive(Clone, Debug, PartialEq)]
pub struct FxoutState<const S: u32> {
    pub status_date: DateTime,
    pub maturity_date: DateTime,
    pub contract_performance: ContractPerformance,
    _scale: std::marker::PhantomData<Fixed<S>>,
}

impl_state_view!(FxoutState {
    status_date => StatusDate,
    maturity_date => MaturityDate,
    contract_performance => ContractPerformance,
});

impl<const S: u32> ContractModel<S> for FxoutTerms<S> {
    type State = FxoutState<S>;

    fn common(&self) -> &CommonTerms {
        &self.common
    }

    fn initial_state(&self, _obs: &dyn RiskFactorObserver<S>) -> Result<FxoutState<S>> {
        Ok(FxoutState {
            status_date: self.common.status_date,
            maturity_date: self.maturity_date,
            contract_performance: self.common.contract_performance,
            _scale: std::marker::PhantomData,
        })
    }

    fn schedule(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>> {
        let (cur, cid) = (&self.common.currency, &self.common.contract_id);
        let bd = &self.common.business_day;
        let mut evs = Vec::new();
        if let Some(prd) = self.trade.purchase_date {
            evs.push(Event::at(EventType::PRD, prd, cur, cid));
        }
        if let Some(td) = self.trade.termination_date {
            evs.push(Event::at(EventType::TD, td, cur, cid));
        } else if self.delivery_settlement == DeliverySettlement::D {
            let md = self.maturity_date;
            evs.push(Event::new(EventType::MD, md, bd.shift_event_time(md), cur, cid));
            evs.push(Event::new(EventType::MD, md, bd.shift_event_time(md), &self.currency2, cid));
        } else {
            let t = bd.shift_event_time(self.settlement_period.add_to(self.maturity_date));
            evs.push(Event::at(EventType::STD, t, cur, cid));
        }
        for o in obs.events(cid) {
            if o.event_type == EventType::CE {
                let mut e = Event::at(EventType::CE, o.time, cur, cid);
                if let Some(crate::state::Value::Performance(p)) =
                    o.states.get(crate::state::StateVariable::ContractPerformance)
                {
                    e.performance = Some(p);
                }
                evs.push(e);
            }
        }
        clip_events(&mut evs, self.common.status_date, to);
        sort_events(&mut evs);
        Ok(evs)
    }

    fn step(&self, s0: &FxoutState<S>, ev: &Event<S>, obs: &dyn RiskFactorObserver<S>) -> Result<(FxoutState<S>, Fixed<S>)> {
        let t = ev.schedule_time;
        let r = self.common.role_sign::<S>();
        let fx = settlement_fx(&self.common, t, obs)?;
        let mut s = s0.clone();
        let payoff = match ev.event_type {
            EventType::AD => Fixed::ZERO,
            EventType::CE => {
                s.contract_performance = ev.performance.unwrap_or(ContractPerformance::DF);
                Fixed::ZERO
            }
            EventType::PRD => fx * r * -self.trade.price_at_purchase_date,
            // The reference implementation pays the termination price without the role sign.
            EventType::TD => fx * self.trade.price_at_termination_date,
            EventType::MD if ev.currency == self.currency2 && self.currency2 != self.common.currency => {
                fx * r * -self.notional_principal2
            }
            EventType::MD => fx * r * self.notional_principal,
            EventType::STD => {
                let id = format!("{}/{}", self.currency2, self.common.currency);
                let rate = obs.risk_factor(&id, s0.maturity_date)?;
                fx * r * (self.notional_principal - rate * self.notional_principal2)
            }
            other => return Err(Error::Unsupported(format!("event {other} for FXOUT"))),
        };
        s.status_date = t;
        Ok((s, payoff))
    }
}
