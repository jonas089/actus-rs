//! SWPPV — Plain Vanilla Interest Rate Swap (fixed vs. floating on one notional).
//!
//! The creator receives the fixed leg (`RF`/`RFL`, role sign +1) or pays it (`PF`/`PFL`).
//! With delivery settlement (`DS = D`, default) both legs are paid separately as `IPFX`
//! (fixed) and `IPFL` (floating) events; with cash settlement (`DS = S`) a single net `IP`.

use super::loan::{after_purchase, apply_termination, clip_events};
use crate::contract::{settlement_fx, ContractModel};
use crate::daycount::{DayCountConvention, DayCounter};
use crate::error::{Error, Result};
use crate::events::{sort_events, Event, EventType};
use crate::fixed::Fixed;
use crate::impl_state_view;
use crate::observer::RiskFactorObserver;
use crate::terms::{
    anchor, parse_day_counter, CommonTerms, ContractPerformance, DeliverySettlement, RateResetTerms, RawTerms,
    TradeTerms,
};
use crate::time::{schedule, Cycle, DateTime};

#[derive(Clone, Debug, PartialEq)]
pub struct SwppvTerms<const S: u32> {
    pub common: CommonTerms,
    pub day_counter: DayCounter,
    pub initial_exchange_date: DateTime,
    pub maturity_date: DateTime,
    pub notional_principal: Fixed<S>,
    /// Fixed leg rate.
    pub nominal_interest_rate: Fixed<S>,
    /// Initial floating leg rate.
    pub nominal_interest_rate2: Fixed<S>,
    pub accrued_interest: Option<Fixed<S>>,
    pub interest_payment_anchor: Option<DateTime>,
    pub interest_payment_cycle: Option<Cycle>,
    pub rate_reset: RateResetTerms<S>,
    pub delivery_settlement: DeliverySettlement,
    pub trade: TradeTerms<S>,
}

impl<const S: u32> SwppvTerms<S> {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        let common = CommonTerms::parse(raw)?;
        let ied = raw.req_date("initialExchangeDate")?;
        let md = raw.req_date("maturityDate")?;
        let def = Some(ied);
        Ok(SwppvTerms {
            day_counter: parse_day_counter(raw, Some(md), common.calendar())?
                .unwrap_or_else(|| DayCounter::new(DayCountConvention::A365)),
            initial_exchange_date: ied,
            maturity_date: md,
            notional_principal: raw.req_fixed("notionalPrincipal")?,
            nominal_interest_rate: raw.req_fixed("nominalInterestRate")?,
            nominal_interest_rate2: raw.fixed_or("nominalInterestRate2", Fixed::ZERO)?,
            accrued_interest: raw.fixed("accruedInterest")?,
            interest_payment_anchor: anchor(raw, "cycleAnchorDateOfInterestPayment", "cycleOfInterestPayment", def)?,
            interest_payment_cycle: raw.cycle("cycleOfInterestPayment")?,
            rate_reset: RateResetTerms::parse(raw, def)?,
            delivery_settlement: raw.enum_or("deliverySettlement", DeliverySettlement::D)?,
            trade: TradeTerms::parse(raw)?,
            common,
        })
    }

    fn yf(&self, from: DateTime, to: DateTime) -> Fixed<S> {
        let bd = &self.common.business_day;
        self.day_counter.year_fraction(bd.shift_calc_time(from), bd.shift_calc_time(to))
    }

    fn net(&self) -> bool {
        self.delivery_settlement == DeliverySettlement::S
    }
}

/// State of a plain vanilla swap.
///
/// `accrued_interest` holds the fixed leg accrual (gross mode) or the net fixed-minus-floating
/// accrual (net mode); `accrued_interest2` holds the floating leg accrual (gross mode).
/// `nominal_interest_rate` is the current floating rate.
#[derive(Clone, Debug, PartialEq)]
pub struct SwppvState<const S: u32> {
    pub status_date: DateTime,
    pub notional_principal: Fixed<S>,
    pub nominal_interest_rate: Fixed<S>,
    pub accrued_interest: Fixed<S>,
    pub accrued_interest2: Fixed<S>,
    pub contract_performance: ContractPerformance,
}

impl_state_view!(SwppvState {
    status_date => StatusDate,
    notional_principal => NotionalPrincipal,
    nominal_interest_rate => NominalInterestRate,
    accrued_interest => AccruedInterest,
    accrued_interest2 => AccruedInterest2,
    contract_performance => ContractPerformance,
});

impl<const S: u32> ContractModel<S> for SwppvTerms<S> {
    type State = SwppvState<S>;

    fn common(&self) -> &CommonTerms {
        &self.common
    }

    fn initial_state(&self, _obs: &dyn RiskFactorObserver<S>) -> Result<SwppvState<S>> {
        let sd = self.common.status_date;
        let r = self.common.role_sign::<S>();
        let started = self.initial_exchange_date <= sd;
        Ok(SwppvState {
            status_date: sd,
            notional_principal: if started { r * self.notional_principal } else { Fixed::ZERO },
            nominal_interest_rate: if started { self.nominal_interest_rate2 } else { Fixed::ZERO },
            accrued_interest: match (started, self.accrued_interest) {
                (true, Some(a)) => r * a,
                _ => Fixed::ZERO,
            },
            accrued_interest2: Fixed::ZERO,
            contract_performance: self.common.contract_performance,
        })
    }

    fn schedule(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>> {
        let (cur, cid) = (&self.common.currency, &self.common.contract_id);
        let bd = &self.common.business_day;
        let eom = self.common.end_of_month;
        let shifted = |times: &[DateTime], ty: EventType| -> Vec<Event<S>> {
            times.iter().map(|t| Event::new(ty, *t, bd.shift_event_time(*t), cur, cid)).collect()
        };
        let mut evs = Vec::new();
        if let Some(prd) = self.trade.purchase_date {
            evs.push(Event::at(EventType::PRD, prd, cur, cid));
        }
        evs.push(Event::at(EventType::IED, self.initial_exchange_date, cur, cid));
        evs.push(Event::at(EventType::MD, self.maturity_date, cur, cid));
        let ip = schedule(self.interest_payment_anchor, self.maturity_date, self.interest_payment_cycle, eom, true);
        if self.net() {
            evs.extend(shifted(&ip, EventType::IP));
        } else {
            evs.extend(shifted(&ip, EventType::IPFX));
            evs.extend(shifted(&ip, EventType::IPFL));
        }
        let rr = schedule(self.rate_reset.anchor, self.maturity_date, self.rate_reset.cycle, eom, false);
        evs.extend(shifted(&rr, EventType::RR));
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
        apply_termination(&mut evs, self.trade.termination_date, cur, cid);
        clip_events(&mut evs, self.common.status_date, to);
        sort_events(&mut evs);
        Ok(evs)
    }

    fn is_reported(&self, event: &Event<S>, _all: &[Event<S>]) -> bool {
        after_purchase(event, self.trade.purchase_date)
    }

    fn step(&self, s0: &SwppvState<S>, ev: &Event<S>, obs: &dyn RiskFactorObserver<S>) -> Result<(SwppvState<S>, Fixed<S>)> {
        let t = ev.schedule_time;
        let r = self.common.role_sign::<S>();
        let fx = settlement_fx(&self.common, t, obs)?;
        let y = self.yf(s0.status_date, t);
        let nt = s0.notional_principal;
        let fixed_rate = self.nominal_interest_rate;
        let float_rate = s0.nominal_interest_rate;
        // Fixed leg (gross) or fixed-minus-floating (net) accrual since the last status date.
        let fixed_accrual = if self.net() { y * (fixed_rate - float_rate) * nt } else { y * fixed_rate * nt };
        let float_accrual = -(y * float_rate * nt);
        let mut s = s0.clone();
        let accrue = |s: &mut SwppvState<S>| {
            s.accrued_interest += fixed_accrual;
            s.accrued_interest2 += float_accrual;
        };
        let payoff = match ev.event_type {
            EventType::AD => {
                accrue(&mut s);
                Fixed::ZERO
            }
            EventType::CE => {
                accrue(&mut s);
                s.contract_performance = ev.performance.unwrap_or(ContractPerformance::DF);
                Fixed::ZERO
            }
            EventType::PRD => {
                accrue(&mut s);
                fx * r * -self.trade.price_at_purchase_date
            }
            EventType::IED => {
                s.notional_principal = r * self.notional_principal;
                s.nominal_interest_rate = self.nominal_interest_rate2;
                s.accrued_interest = Fixed::ZERO;
                s.accrued_interest2 = Fixed::ZERO;
                Fixed::ZERO
            }
            EventType::RR => {
                let moc = self
                    .rate_reset
                    .market_object_code
                    .as_deref()
                    .ok_or_else(|| Error::MissingTerm("marketObjectCodeOfRateReset".into()))?;
                accrue(&mut s);
                s.nominal_interest_rate = obs.risk_factor(moc, t)? * self.rate_reset.multiplier + self.rate_reset.spread;
                Fixed::ZERO
            }
            EventType::IPFX => {
                let p = fx * (s0.accrued_interest + y * fixed_rate * nt);
                s.accrued_interest = Fixed::ZERO;
                s.accrued_interest2 += float_accrual;
                p
            }
            EventType::IPFL => {
                let p = fx * (s0.accrued_interest2 + float_accrual);
                s.accrued_interest2 = Fixed::ZERO;
                p
            }
            EventType::IP => {
                let p = fx * (s0.accrued_interest + y * (fixed_rate - float_rate) * nt);
                s.accrued_interest = Fixed::ZERO;
                s.accrued_interest2 = Fixed::ZERO;
                p
            }
            EventType::MD => {
                s.notional_principal = Fixed::ZERO;
                s.accrued_interest = Fixed::ZERO;
                s.accrued_interest2 = Fixed::ZERO;
                Fixed::ZERO
            }
            EventType::TD => {
                s.notional_principal = Fixed::ZERO;
                s.accrued_interest = Fixed::ZERO;
                s.accrued_interest2 = Fixed::ZERO;
                // Paid without the role sign, as in the reference implementation.
                fx * self.trade.price_at_termination_date
            }
            other => return Err(Error::Unsupported(format!("event {other} for SWPPV"))),
        };
        s.status_date = t;
        Ok((s, payoff))
    }
}
