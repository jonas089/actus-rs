//! CLM — Call Money (loans without a fixed maturity that end when called with notice).
//!
//! Interest is capitalized on the IPCI cycle and paid together with the principal at maturity.
//! Maturity is either the `maturityDate` term or, when the contract is called, the call date
//! (an observed `XD` event) plus the notice period `xDayNotice`. A call produces an `XD` event
//! at the call date and `IP` + `STD` (settlement) at the end of the notice period.

use super::loan::{clip_events, LoanCalc, LoanState};
use super::pam::{credit_event_performance, initial_fee_accrued, rate_reset_events};
use crate::contract::ContractModel;
use crate::daycount::{DayCountConvention, DayCounter};
use crate::error::{Error, Result};
use crate::events::{Event, EventType};
use crate::fixed::Fixed;
use crate::observer::RiskFactorObserver;
use crate::terms::{anchor, parse_day_counter, CommonTerms, ContractPerformance, FeeTerms, RateResetTerms, RawTerms};
use crate::state::{StateVariable, StateView, Value};
use crate::time::{schedule, Cycle, DateTime, Period};

#[derive(Clone, Debug, PartialEq)]
pub struct ClmTerms<const S: u32> {
    pub common: CommonTerms,
    pub day_counter: DayCounter,
    pub initial_exchange_date: DateTime,
    pub maturity_date: Option<DateTime>,
    pub notional_principal: Fixed<S>,
    pub nominal_interest_rate: Option<Fixed<S>>,
    pub accrued_interest: Option<Fixed<S>>,
    pub interest_payment_anchor: Option<DateTime>,
    pub interest_payment_cycle: Option<Cycle>,
    /// Notice period between a call (XD) and repayment.
    pub x_day_notice: Option<Period>,
    pub rate_reset: RateResetTerms<S>,
    pub fees: FeeTerms<S>,
}

/// When a CLM ends and why.
#[derive(Clone, Copy, Debug, PartialEq)]
struct End {
    /// Repayment date (IP and MD/STD).
    maturity: DateTime,
    /// Call date, if the contract was called.
    call: Option<DateTime>,
}

impl<const S: u32> ClmTerms<S> {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        let common = CommonTerms::parse(raw)?;
        let ied = raw.req_date("initialExchangeDate")?;
        let md = raw.date("maturityDate")?;
        let def = Some(ied);
        Ok(ClmTerms {
            day_counter: parse_day_counter(raw, md, common.calendar())?
                .unwrap_or_else(|| DayCounter::new(DayCountConvention::A365)),
            initial_exchange_date: ied,
            maturity_date: md,
            notional_principal: raw.req_fixed("notionalPrincipal")?,
            nominal_interest_rate: raw.fixed("nominalInterestRate")?,
            accrued_interest: raw.fixed("accruedInterest")?,
            interest_payment_anchor: anchor(raw, "cycleAnchorDateOfInterestPayment", "cycleOfInterestPayment", def)?,
            interest_payment_cycle: raw.cycle("cycleOfInterestPayment")?,
            x_day_notice: raw.period("xDayNotice")?,
            rate_reset: RateResetTerms::parse(raw, def)?,
            fees: FeeTerms::parse(raw, def)?,
            common,
        })
    }

    fn calc(&self, end: DateTime) -> LoanCalc<'_, S> {
        LoanCalc { common: &self.common, day_counter: &self.day_counter, fees: &self.fees, fee_schedule_end: end }
    }

    fn rate(&self) -> Fixed<S> {
        self.nominal_interest_rate.unwrap_or(Fixed::ZERO)
    }

    /// The first observed call (XD) of this contract, if any.
    fn call_date(&self, obs: &dyn RiskFactorObserver<S>) -> Option<DateTime> {
        obs.events(&self.common.contract_id)
            .into_iter()
            .filter(|e| e.event_type == EventType::XD)
            .map(|e| e.time)
            .min()
    }

    fn end(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<End> {
        if let Some(md) = self.maturity_date {
            return Ok(End { maturity: md, call: None });
        }
        if let Some(xd) = self.call_date(obs) {
            let notice = self.x_day_notice.unwrap_or(Period::ZERO);
            return Ok(End { maturity: notice.add_to(xd), call: Some(xd) });
        }
        match to {
            Some(t) => Ok(End { maturity: t, call: None }),
            None => Err(Error::Unsupported(
                "CLM without maturity date or call needs an analysis horizon".into(),
            )),
        }
    }

    /// End of the cyclic (IPCI, RR, FP) schedules: the call date if called, else maturity.
    fn cycle_end(end: End) -> DateTime {
        end.call.unwrap_or(end.maturity)
    }

    fn ipci_schedule(&self, end: DateTime) -> Vec<DateTime> {
        if self.interest_payment_cycle.is_none() {
            return Vec::new();
        }
        schedule(self.interest_payment_anchor, end, self.interest_payment_cycle, self.common.end_of_month, false)
    }
}

/// CLM state: the lending state plus the call (exercise) date and the interest fixed at the call.
#[derive(Clone, Debug, PartialEq)]
pub struct ClmState<const S: u32> {
    pub loan: LoanState<S>,
    /// Date the contract was called (observed XD).
    pub exercise_date: Option<DateTime>,
    /// Interest accrued up to the call date, payable at repayment (set by a call with notice).
    pub exercise_amount: Option<Fixed<S>>,
}

const CLM_VARIABLES: &[StateVariable] = &[
    StateVariable::StatusDate,
    StateVariable::MaturityDate,
    StateVariable::NotionalPrincipal,
    StateVariable::NominalInterestRate,
    StateVariable::AccruedInterest,
    StateVariable::FeeAccrued,
    StateVariable::NotionalScalingMultiplier,
    StateVariable::InterestScalingMultiplier,
    StateVariable::ContractPerformance,
    StateVariable::NonPerformingDate,
    StateVariable::ExerciseDate,
    StateVariable::ExerciseAmount,
];

impl<const S: u32> StateView<S> for ClmState<S> {
    fn get(&self, var: StateVariable) -> Option<Value<S>> {
        match var {
            StateVariable::ExerciseDate => self.exercise_date.map(Value::Date),
            StateVariable::ExerciseAmount => self.exercise_amount.map(Value::Number),
            v => self.loan.get(v),
        }
    }
    fn variables(&self) -> &'static [StateVariable] {
        CLM_VARIABLES
    }
}

impl<const S: u32> ContractModel<S> for ClmTerms<S> {
    type State = ClmState<S>;

    fn common(&self) -> &CommonTerms {
        &self.common
    }

    fn initial_state(&self, obs: &dyn RiskFactorObserver<S>) -> Result<ClmState<S>> {
        let sd = self.common.status_date;
        let end = if self.maturity_date.is_some() || self.call_date(obs).is_some() {
            self.end(None, obs).ok()
        } else {
            None
        };
        let cycle_end = end.map(Self::cycle_end).unwrap_or(sd);
        let calc = self.calc(cycle_end);
        let started = self.initial_exchange_date <= sd;
        let nt = if started { self.common.role_sign() * self.notional_principal } else { Fixed::ZERO };
        let ipnr = if started { self.rate() } else { Fixed::ZERO };
        // Accrued interest at the status date is taken from the terms (zero if absent); it is
        // not estimated from the last capitalization date (the official tests require this).
        let ipac = if started { self.accrued_interest.unwrap_or(Fixed::ZERO) } else { Fixed::ZERO };
        let loan = LoanState {
            status_date: sd,
            maturity_date: end.map(|e| e.maturity),
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
        };
        Ok(ClmState { loan, exercise_date: None, exercise_amount: None })
    }

    fn schedule(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>> {
        let end = self.end(to, obs)?;
        let cycle_end = Self::cycle_end(end);
        let calc = self.calc(cycle_end);
        let (cur, cid) = (&self.common.currency, &self.common.contract_id);
        let mut evs = vec![
            Event::at(EventType::IED, self.initial_exchange_date, cur, cid),
            Event::at(EventType::IP, end.maturity, cur, cid),
            Event::at(if end.call.is_some() { EventType::STD } else { EventType::MD }, end.maturity, cur, cid),
        ];
        if let Some(xd) = end.call {
            evs.push(Event::at(EventType::XD, xd, cur, cid));
        }
        evs.extend(calc.events(&self.ipci_schedule(cycle_end), EventType::IPCI));
        evs.extend(rate_reset_events(&calc, &self.rate_reset, cycle_end));
        if self.fees.cycle.is_some() && self.fees.rate.is_some_and(|r| !r.is_zero()) {
            let fp = schedule(self.fees.anchor, cycle_end, self.fees.cycle, self.common.end_of_month, false);
            evs.extend(calc.events(&fp, EventType::FP));
        }
        for o in obs.events(cid) {
            if matches!(o.event_type, EventType::CE | EventType::AD) {
                let mut e = calc.event_at(o.time, o.event_type);
                if let Some(crate::state::Value::Performance(p)) =
                    o.states.get(crate::state::StateVariable::ContractPerformance)
                {
                    e.event_type = EventType::CE;
                    e.performance = Some(p);
                }
                evs.push(e);
            }
        }
        // Events after the end of the analysis horizon are only dropped when the contract has
        // no maturity of its own; a called or matured CLM always runs to repayment.
        let horizon = if self.maturity_date.is_none() && end.call.is_none() { to } else { None };
        clip_events(&mut evs, self.common.status_date, horizon);
        // A call (XD) is ordered before any other event at the same instant.
        evs.sort_by_key(|e| {
            let off = if e.event_type == EventType::XD { 0 } else { e.event_type.sequence() };
            e.time.and_utc().timestamp() + off
        });
        Ok(evs)
    }

    fn step(&self, c0: &ClmState<S>, ev: &Event<S>, obs: &dyn RiskFactorObserver<S>) -> Result<(ClmState<S>, Fixed<S>)> {
        let mut c = c0.clone();
        let s0 = &c0.loan;
        let end = s0.maturity_date.unwrap_or(ev.schedule_time);
        let calc = self.calc(end);
        let t = ev.schedule_time;
        let r = self.common.role_sign();
        let fx = calc.fx(t, obs)?;
        let mut s = s0.clone();
        // Once called, interest stops accruing at the call date.
        let accrued = if c0.exercise_date.is_some() {
            Fixed::ZERO
        } else {
            calc.interest_since(s0, t, s0.notional_principal)
        };
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
                s.accrued_interest = self.accrued_interest.unwrap_or(Fixed::ZERO);
                fx * r * -self.notional_principal
            }
            EventType::XD => {
                // The call fixes the interest up to the call date. With immediate effect it stays
                // in accrued interest (repaid at the same instant); with a notice period it is
                // moved to the exercise amount, payable at repayment.
                calc.accrue_fee(&mut s, t);
                c.exercise_date = Some(t);
                if self.x_day_notice.unwrap_or(Period::ZERO).is_zero() {
                    s.accrued_interest += accrued;
                } else {
                    c.exercise_amount = Some(s0.accrued_interest + accrued);
                    s.accrued_interest = Fixed::ZERO;
                }
                Fixed::ZERO
            }
            EventType::IP => {
                let fixed = c0.exercise_amount.unwrap_or(Fixed::ZERO);
                c.exercise_amount = c0.exercise_amount.map(|_| Fixed::ZERO);
                s.accrued_interest = Fixed::ZERO;
                fx * (s0.accrued_interest + accrued + fixed)
            }
            EventType::MD | EventType::STD => {
                let p = fx * (s0.notional_principal + s0.accrued_interest + accrued + s0.fee_accrued);
                s.notional_principal = Fixed::ZERO;
                s.accrued_interest = Fixed::ZERO;
                s.fee_accrued = Fixed::ZERO;
                p
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
            other => return Err(Error::Unsupported(format!("event {other} for CLM"))),
        };
        s.status_date = t;
        c.loan = s;
        Ok((c, payoff))
    }
}
