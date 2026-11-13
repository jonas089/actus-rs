//! STK — Stock (an equity position with optional dividend payments).

use super::loan::{apply_termination, clip_events};
use crate::contract::{settlement_fx, ContractModel};
use crate::error::{Error, Result};
use crate::events::{sort_events, Event, EventType};
use crate::fixed::Fixed;
use crate::impl_state_view;
use crate::observer::RiskFactorObserver;
use crate::state::{StateVariable, Value};
use crate::terms::{anchor, CommonTerms, ContractPerformance, RawTerms, TradeTerms};
use crate::time::{schedule, Cycle, DateTime};

/// State of ownership contracts (STK, COM): only status date and performance.
#[derive(Clone, Debug, PartialEq)]
pub struct OwnershipState<const S: u32> {
    pub status_date: DateTime,
    pub contract_performance: ContractPerformance,
}

impl_state_view!(OwnershipState {
    status_date => StatusDate,
    contract_performance => ContractPerformance,
});

#[derive(Clone, Debug, PartialEq)]
pub struct StkTerms<const S: u32> {
    pub common: CommonTerms,
    pub quantity: Fixed<S>,
    pub notional_principal: Option<Fixed<S>>,
    pub dividend_anchor: Option<DateTime>,
    pub dividend_cycle: Option<Cycle>,
    /// Market object observed for dividend amounts per share (default `"<contractID>-DV"`).
    pub dividend_market_object_code: Option<String>,
    /// Known amount of the next dividend (paid at the dividend anchor).
    pub next_dividend_payment_amount: Option<Fixed<S>>,
    pub trade: TradeTerms<S>,
}

impl<const S: u32> StkTerms<S> {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        let trade = TradeTerms::parse(raw)?;
        // The dictionary names are `cycle(AnchorDate)OfDividend`; the official tests use
        // `...OfDividendPayment`. Both are accepted.
        let (anchor_key, cycle_key) = if raw.has("cycleOfDividend") || raw.has("cycleAnchorDateOfDividend") {
            ("cycleAnchorDateOfDividend", "cycleOfDividend")
        } else {
            ("cycleAnchorDateOfDividendPayment", "cycleOfDividendPayment")
        };
        Ok(StkTerms {
            common: CommonTerms::parse(raw)?,
            quantity: raw.fixed_or("quantity", Fixed::ONE)?,
            notional_principal: raw.fixed("notionalPrincipal")?,
            dividend_anchor: anchor(raw, anchor_key, cycle_key, trade.purchase_date)?,
            dividend_cycle: raw.cycle(cycle_key)?,
            dividend_market_object_code: raw.text("marketObjectCodeOfDividends"),
            next_dividend_payment_amount: raw.fixed("nextDividendPaymentAmount")?,
            trade,
        })
    }

    fn dividend_moc(&self) -> String {
        self.dividend_market_object_code.clone().unwrap_or_else(|| format!("{}-DV", self.common.contract_id))
    }
}

impl<const S: u32> ContractModel<S> for StkTerms<S> {
    type State = OwnershipState<S>;

    fn common(&self) -> &CommonTerms {
        &self.common
    }

    fn initial_state(&self, _obs: &dyn RiskFactorObserver<S>) -> Result<OwnershipState<S>> {
        Ok(OwnershipState { status_date: self.common.status_date, contract_performance: self.common.contract_performance })
    }

    fn schedule(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>> {
        let (cur, cid) = (&self.common.currency, &self.common.contract_id);
        let bd = &self.common.business_day;
        let mut evs = Vec::new();
        if let Some(prd) = self.trade.purchase_date {
            evs.push(Event::at(EventType::PRD, prd, cur, cid));
        }
        if let (Some(a), Some(c)) = (self.dividend_anchor, self.dividend_cycle) {
            // Dividends recur without end: generate up to the termination date, else one cycle
            // past the analysis horizon (later events are clipped below).
            let (end, include_end) = match (self.trade.termination_date, to) {
                (Some(td), _) => (td, true),
                (None, Some(to)) => (c.period.add_to(to), false),
                (None, None) => {
                    return Err(Error::Unsupported("STK dividends without termination need a horizon".into()));
                }
            };
            for (i, t) in schedule(Some(a), end, Some(c), self.common.end_of_month, include_end).into_iter().enumerate() {
                let mut e = Event::new(EventType::DV, t, bd.shift_event_time(t), cur, cid);
                // A known next dividend amount fixes the first dividend.
                if i == 0 {
                    if let Some(v) = self.next_dividend_payment_amount {
                        e.value = v;
                    }
                }
                evs.push(e);
            }
        }
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
        apply_termination(&mut evs, self.trade.termination_date, cur, cid);
        clip_events(&mut evs, self.common.status_date, to);
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
        let mut s = OwnershipState { status_date: t, ..s0.clone() };
        let payoff = match ev.event_type {
            EventType::PRD => fx * r * -(self.quantity * self.trade.price_at_purchase_date),
            EventType::TD => fx * r * self.quantity * self.trade.price_at_termination_date,
            EventType::DV => {
                let first_fixed = self.next_dividend_payment_amount.is_some() && Some(t) == self.dividend_anchor;
                let per_share = if first_fixed { ev.value } else { obs.risk_factor(&self.dividend_moc(), t)? };
                fx * r * self.quantity * per_share
            }
            EventType::CE => {
                s.contract_performance = ev.performance.unwrap_or(ContractPerformance::DF);
                Fixed::ZERO
            }
            EventType::AD => Fixed::ZERO,
            other => return Err(Error::Unsupported(format!("event {other} for STK"))),
        };
        Ok((s, payoff))
    }
}
