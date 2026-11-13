//! UMP — Undefined Maturity Profile (e.g. current accounts, savings without fixed repayment).
//!
//! Principal changes only through observed (unscheduled) `PR` / `PI` events; interest is
//! capitalized on the IPCI cycle. The contract has no natural end, so projections need a horizon.

use super::loan::{apply_termination, clip_events, LoanCalc, LoanState};
use super::pam::{credit_event_performance, initial_accrued_interest, initial_fee_accrued, rate_reset_events};
use crate::contract::ContractModel;
use crate::daycount::{DayCountConvention, DayCounter};
use crate::error::{Error, Result};
use crate::events::{sort_events, Event, EventType};
use crate::fixed::Fixed;
use crate::observer::RiskFactorObserver;
use crate::state::{StateVariable, Value};
use crate::terms::{
    anchor, parse_day_counter, CommonTerms, ContractPerformance, FeeTerms, RateResetTerms, RawTerms, TradeTerms,
};
use crate::time::{schedule, Cycle, DateTime, Period};

#[derive(Clone, Debug, PartialEq)]
pub struct UmpTerms<const S: u32> {
    pub common: CommonTerms,
    pub day_counter: DayCounter,
    pub initial_exchange_date: DateTime,
    pub notional_principal: Fixed<S>,
    pub nominal_interest_rate: Option<Fixed<S>>,
    pub accrued_interest: Option<Fixed<S>>,
    pub interest_payment_anchor: Option<DateTime>,
    pub interest_payment_cycle: Option<Cycle>,
    pub x_day_notice: Option<Period>,
    pub maximum_penalty_free_disbursement: Option<Fixed<S>>,
    pub rate_reset: RateResetTerms<S>,
    pub fees: FeeTerms<S>,
    pub trade: TradeTerms<S>,
}

impl<const S: u32> UmpTerms<S> {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        let common = CommonTerms::parse(raw)?;
        let ied = raw.req_date("initialExchangeDate")?;
        let def = Some(ied);
        Ok(UmpTerms {
            day_counter: parse_day_counter(raw, None, common.calendar())?
                .unwrap_or_else(|| DayCounter::new(DayCountConvention::A365)),
            initial_exchange_date: ied,
            notional_principal: raw.req_fixed("notionalPrincipal")?,
            nominal_interest_rate: raw.fixed("nominalInterestRate")?,
            accrued_interest: raw.fixed("accruedInterest")?,
            interest_payment_anchor: anchor(raw, "cycleAnchorDateOfInterestPayment", "cycleOfInterestPayment", def)?,
            interest_payment_cycle: raw.cycle("cycleOfInterestPayment")?,
            x_day_notice: raw.period("xDayNotice")?,
            maximum_penalty_free_disbursement: raw.fixed("maximumPenaltyFreeDisbursement")?,
            rate_reset: RateResetTerms::parse(raw, def)?,
            fees: FeeTerms::parse(raw, def)?,
            trade: TradeTerms::parse(raw)?,
            common,
        })
    }

    fn calc(&self, end: DateTime) -> LoanCalc<'_, S> {
        LoanCalc { common: &self.common, day_counter: &self.day_counter, fees: &self.fees, fee_schedule_end: end }
    }

    fn rate(&self) -> Fixed<S> {
        self.nominal_interest_rate.unwrap_or(Fixed::ZERO)
    }

    fn ipci_schedule(&self, end: DateTime) -> Vec<DateTime> {
        if self.interest_payment_cycle.is_none() {
            return Vec::new();
        }
        schedule(self.interest_payment_anchor, end, self.interest_payment_cycle, self.common.end_of_month, false)
    }

    /// End of the cyclic schedules: termination date, else the analysis horizon.
    fn horizon(&self, to: Option<DateTime>) -> Result<DateTime> {
        to.or(self.trade.termination_date)
            .ok_or_else(|| Error::Unsupported("UMP has no maturity; an analysis horizon is required".into()))
    }
}

impl<const S: u32> ContractModel<S> for UmpTerms<S> {
    type State = LoanState<S>;

    fn common(&self) -> &CommonTerms {
        &self.common
    }

    fn initial_state(&self, _obs: &dyn RiskFactorObserver<S>) -> Result<LoanState<S>> {
        let sd = self.common.status_date;
        let calc = self.calc(sd);
        let started = self.initial_exchange_date <= sd;
        let nt = if started { self.common.role_sign() * self.notional_principal } else { Fixed::ZERO };
        let ipnr = if started { self.rate() } else { Fixed::ZERO };
        let ipac = initial_accrued_interest(
            &calc,
            self.nominal_interest_rate,
            self.accrued_interest.map(|a| self.common.role_sign() * a),
            &self.ipci_schedule(sd),
            self.initial_exchange_date,
            nt,
            ipnr,
        );
        Ok(LoanState {
            status_date: sd,
            maturity_date: None,
            notional_principal: nt,
            nominal_interest_rate: ipnr,
            accrued_interest: ipac,
            fee_accrued: initial_fee_accrued(&calc, nt),
            notional_scaling_multiplier: Fixed::ONE,
            interest_scaling_multiplier: Fixed::ONE,
            contract_performance: self.common.contract_performance,
            next_principal_redemption_payment: None,
            interest_calculation_base_amount: None,
            non_performing_date: None,
        })
    }

    fn schedule(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>> {
        let end = self.horizon(to)?;
        let calc = self.calc(end);
        let (cur, cid) = (&self.common.currency, &self.common.contract_id);
        let mut evs = vec![Event::at(EventType::IED, self.initial_exchange_date, cur, cid)];
        evs.extend(calc.events(&self.ipci_schedule(end), EventType::IPCI));
        evs.extend(rate_reset_events(&calc, &self.rate_reset, end));
        if self.fees.cycle.is_some() && self.fees.rate.is_some_and(|r| !r.is_zero()) {
            let fp = schedule(self.fees.anchor, end, self.fees.cycle, self.common.end_of_month, false);
            evs.extend(calc.events(&fp, EventType::FP));
        }
        for o in obs.events(cid) {
            match o.event_type {
                EventType::PR | EventType::PI | EventType::AD | EventType::CE => {
                    let mut e = calc.event_at(o.time, o.event_type);
                    e.value = o.value;
                    if let Some(Value::Performance(p)) = o.states.get(StateVariable::ContractPerformance) {
                        e.event_type = EventType::CE;
                        e.performance = Some(p);
                    }
                    evs.push(e);
                }
                _ => {}
            }
        }
        apply_termination(&mut evs, self.trade.termination_date, cur, cid);
        clip_events(&mut evs, self.common.status_date, to);
        sort_events(&mut evs);
        Ok(evs)
    }

    fn step(&self, s0: &LoanState<S>, ev: &Event<S>, obs: &dyn RiskFactorObserver<S>) -> Result<(LoanState<S>, Fixed<S>)> {
        let t = ev.schedule_time;
        let calc = self.calc(self.trade.termination_date.unwrap_or(t));
        let r = self.common.role_sign();
        let fx = calc.fx(t, obs)?;
        let mut s = s0.clone();
        let accrued = calc.interest_since(s0, t, s0.notional_principal);
        let payoff = match ev.event_type {
            EventType::AD => {
                s.accrued_interest += accrued;
                calc.accrue_fee(&mut s, t);
                Fixed::ZERO
            }
            EventType::CE => {
                s.accrued_interest += accrued;
                calc.accrue_fee(&mut s, t);
                if let Some(p) = credit_event_performance(ev) {
                    s.contract_performance = p;
                    if s.non_performing_date.is_none() && p != ContractPerformance::PF {
                        s.non_performing_date = Some(t);
                    }
                }
                Fixed::ZERO
            }
            EventType::IED => {
                s.notional_principal = r * self.notional_principal;
                s.nominal_interest_rate = self.rate();
                s.accrued_interest = self.accrued_interest.map(|a| r * a).unwrap_or(Fixed::ZERO);
                fx * r * -self.notional_principal
            }
            // Observed principal repayment (PR, value = amount repaid to the creator) or
            // increase (PI, value = amount drawn by the counterparty).
            EventType::PR | EventType::PI => {
                s.accrued_interest += accrued;
                calc.accrue_fee(&mut s, t);
                let amount = if ev.event_type == EventType::PR { r * ev.value } else { -(r * ev.value) };
                s.notional_principal -= amount;
                fx * amount
            }
            EventType::FP => {
                let p = calc.fee_payoff(s0, t, obs)?;
                s.accrued_interest += accrued;
                s.fee_accrued = Fixed::ZERO;
                p
            }
            EventType::IPCI => {
                s.notional_principal = s0.notional_principal + s0.accrued_interest + accrued;
                s.accrued_interest = Fixed::ZERO;
                calc.accrue_fee(&mut s, t);
                Fixed::ZERO
            }
            EventType::RR => {
                let moc = self
                    .rate_reset
                    .market_object_code
                    .as_deref()
                    .ok_or_else(|| Error::MissingTerm("marketObjectCodeOfRateReset".into()))?;
                let market = obs.risk_factor(moc, t)?;
                s.accrued_interest += accrued;
                calc.accrue_fee(&mut s, t);
                s.nominal_interest_rate = calc.reset_rate(&self.rate_reset, s0.nominal_interest_rate, market);
                Fixed::ZERO
            }
            EventType::RRF => {
                s.accrued_interest += accrued;
                calc.accrue_fee(&mut s, t);
                s.nominal_interest_rate = self.rate_reset.next_reset_rate.unwrap_or(s0.nominal_interest_rate);
                Fixed::ZERO
            }
            EventType::TD => {
                let p = fx * (r * self.trade.price_at_termination_date + s0.accrued_interest + accrued);
                s.notional_principal = Fixed::ZERO;
                s.accrued_interest = Fixed::ZERO;
                s.fee_accrued = Fixed::ZERO;
                p
            }
            other => return Err(Error::Unsupported(format!("event {other} for UMP"))),
        };
        s.status_date = t;
        Ok((s, payoff))
    }
}
