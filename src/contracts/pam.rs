//! PAM — Principal At Maturity (bullet loans, bonds, deposits).

use super::loan::{after_purchase, apply_termination, clip_events, LoanCalc, LoanState};
use crate::contract::ContractModel;
use crate::daycount::{DayCountConvention, DayCounter};
use crate::error::{Error, Result};
use crate::events::{sort_events, Event, EventType};
use crate::fixed::Fixed;
use crate::observer::RiskFactorObserver;
use crate::terms::{
    anchor, parse_day_counter, CommonTerms, ContractPerformance, FeeTerms, PenaltyType, PrepaymentEffect,
    PrepaymentTerms, RateResetTerms, RawTerms, ScalingTerms, TradeTerms,
};
use crate::time::{schedule, Cycle, DateTime};

#[derive(Clone, Debug, PartialEq)]
pub struct PamTerms<const S: u32> {
    pub common: CommonTerms,
    pub day_counter: DayCounter,
    pub initial_exchange_date: DateTime,
    pub maturity_date: DateTime,
    pub notional_principal: Fixed<S>,
    pub premium_discount_at_ied: Fixed<S>,
    pub nominal_interest_rate: Option<Fixed<S>>,
    pub accrued_interest: Option<Fixed<S>>,
    pub interest_payment_anchor: Option<DateTime>,
    pub interest_payment_cycle: Option<Cycle>,
    pub capitalization_end_date: Option<DateTime>,
    pub rate_reset: RateResetTerms<S>,
    pub fees: FeeTerms<S>,
    pub scaling: ScalingTerms<S>,
    pub prepayment: PrepaymentTerms<S>,
    pub trade: TradeTerms<S>,
}

impl<const S: u32> PamTerms<S> {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        let common = CommonTerms::parse(raw)?;
        let ied = raw.req_date("initialExchangeDate")?;
        let md = raw.req_date("maturityDate")?;
        // Cycle anchors default to the initial exchange date when only a cycle is given.
        let def = Some(ied);
        Ok(PamTerms {
            day_counter: parse_day_counter(raw, Some(md), common.calendar())?
                .unwrap_or_else(|| DayCounter::new(DayCountConvention::A365)),
            initial_exchange_date: ied,
            maturity_date: md,
            notional_principal: raw.req_fixed("notionalPrincipal")?,
            premium_discount_at_ied: raw.fixed_or("premiumDiscountAtIED", Fixed::ZERO)?,
            nominal_interest_rate: raw.fixed("nominalInterestRate")?,
            accrued_interest: raw.fixed("accruedInterest")?,
            interest_payment_anchor: anchor(raw, "cycleAnchorDateOfInterestPayment", "cycleOfInterestPayment", def)?,
            interest_payment_cycle: raw.cycle("cycleOfInterestPayment")?,
            capitalization_end_date: raw.date("capitalizationEndDate")?,
            rate_reset: RateResetTerms::parse(raw, def)?,
            fees: FeeTerms::parse(raw, def)?,
            scaling: ScalingTerms::parse(raw, def)?,
            prepayment: PrepaymentTerms::parse(raw, def)?,
            trade: TradeTerms::parse(raw)?,
            common,
        })
    }

    fn calc(&self) -> LoanCalc<'_, S> {
        LoanCalc {
            common: &self.common,
            day_counter: &self.day_counter,
            fees: &self.fees,
            fee_schedule_end: self.maturity_date,
        }
    }

    fn ip_schedule(&self) -> Vec<DateTime> {
        if self.interest_payment_cycle.is_none() && self.interest_payment_anchor.is_none() {
            return Vec::new();
        }
        schedule(
            self.interest_payment_anchor,
            self.maturity_date,
            self.interest_payment_cycle,
            self.common.end_of_month,
            true,
        )
    }

    fn rate(&self) -> Fixed<S> {
        self.nominal_interest_rate.unwrap_or(Fixed::ZERO)
    }
}

/// Interest schedule with capitalization: IP events up to the capitalization end date become
/// IPCI (interest is added to principal instead of being paid).
pub(crate) fn interest_events<const S: u32>(
    calc: &LoanCalc<'_, S>,
    ip_times: &[DateTime],
    capitalization_end: Option<DateTime>,
) -> Vec<Event<S>> {
    let mut evs = calc.events(ip_times, EventType::IP);
    if let Some(ced) = capitalization_end {
        let shifted = calc.common.business_day.shift_event_time(ced);
        let ipci = Event::new(EventType::IPCI, ced, shifted, &calc.common.currency, &calc.common.contract_id);
        evs.retain(|e| e.key() != ipci.key());
        for e in evs.iter_mut() {
            if e.key() <= ipci.key() {
                e.event_type = EventType::IPCI;
            }
        }
        evs.push(ipci);
    }
    evs
}

/// Rate reset events; with a known next reset rate the first reset after the status date is
/// a fixing (RRF) instead of a market observation (RR).
pub(crate) fn rate_reset_events<const S: u32>(
    calc: &LoanCalc<'_, S>,
    rr: &RateResetTerms<S>,
    end: DateTime,
) -> Vec<Event<S>> {
    if !rr.is_defined() {
        return Vec::new();
    }
    let times = schedule(rr.anchor, end, rr.cycle, calc.common.end_of_month, false);
    let mut evs = calc.events(&times, EventType::RR);
    if rr.next_reset_rate.is_some() {
        let sd_key = crate::events::sort_key(calc.common.status_date, EventType::AD);
        let mut sorted: Vec<usize> = (0..evs.len()).collect();
        sorted.sort_by_key(|i| evs[*i].key());
        if let Some(i) = sorted.into_iter().find(|i| evs[*i].key() > sd_key) {
            evs[i].event_type = EventType::RRF;
        }
    }
    evs
}

/// Prepayment (PP), penalty (PY), credit (CE) and monitoring (AD) events of a lending contract.
///
/// Prepayments follow the spec: none at all unless `prepaymentEffect != N`; otherwise a PP
/// event on every optionality date `S(OPANX, OPCL, end)` (anchor defaulting to IED; `end`
/// itself excluded, a prepayment on the maturity date is just the redemption) plus every
/// observed PP at other times. A PY event precedes each PP when a penalty type is set.
/// The amount of a PP is the observed amount at that instant, else (see
/// [`prepayment_amount`]) the prepayment-model rate, else zero.
pub(crate) fn observed_loan_events<const S: u32>(
    calc: &LoanCalc<'_, S>,
    prepayment: &PrepaymentTerms<S>,
    end: DateTime,
    obs: &dyn RiskFactorObserver<S>,
) -> Vec<Event<S>> {
    let mut out = Vec::new();
    let mut prepayments: std::collections::BTreeMap<DateTime, Fixed<S>> =
        std::collections::BTreeMap::new();
    if prepayment.effect != PrepaymentEffect::N
        && (prepayment.anchor.is_some() || prepayment.cycle.is_some())
    {
        for t in schedule(
            prepayment.anchor,
            end,
            prepayment.cycle,
            calc.common.end_of_month,
            false,
        ) {
            if t < end {
                prepayments.insert(t, Fixed::ZERO);
            }
        }
    }
    for o in obs.events(&calc.common.contract_id) {
        match o.event_type {
            EventType::PP => {
                if prepayment.effect != PrepaymentEffect::N {
                    prepayments.insert(o.time, o.value);
                }
            }
            EventType::CE | EventType::AD => {
                let mut e = calc.event_at(o.time, o.event_type);
                e.value = o.value;
                if let Some(crate::state::Value::Performance(p)) =
                    o.states.get(crate::state::StateVariable::ContractPerformance)
                {
                    e.event_type = EventType::CE;
                    e.performance = Some(p);
                }
                out.push(e);
            }
            _ => {}
        }
    }
    for (t, value) in prepayments {
        // PY first: the penalty is computed on the pre-prepayment notional.
        for ty in [EventType::PY, EventType::PP] {
            if ty == EventType::PY && prepayment.penalty_type == PenaltyType::O {
                continue;
            }
            let mut e = calc.event_at(t, ty);
            e.value = value;
            out.push(e);
        }
    }
    out
}

/// Signed principal prepaid by a PP event (or the PP that a PY event belongs to), given the
/// pre-prepayment notional `nt`: the event's observed amount (unsigned, signed here by the
/// contract role), else `rate * Nt` with the rate observed from the prepayment model, else 0.
/// Never more than is outstanding.
pub(crate) fn prepayment_amount<const S: u32>(
    common: &CommonTerms,
    prepayment: &PrepaymentTerms<S>,
    ev: &Event<S>,
    nt: Fixed<S>,
    obs: &dyn RiskFactorObserver<S>,
) -> Result<Fixed<S>> {
    let amount = if !ev.value.is_zero() {
        common.role_sign() * ev.value
    } else if let Some(model) = prepayment.model_object_code.as_deref() {
        obs.risk_factor(model, ev.schedule_time)? * nt
    } else {
        Fixed::ZERO
    };
    Ok(if amount.abs() > nt.abs() { nt } else { amount })
}

/// PY payoff (spec POF_PY_PAM), charged only when the accompanying prepayment is non-zero.
pub(crate) fn penalty_payoff<const S: u32>(
    calc: &LoanCalc<'_, S>,
    prepayment: &PrepaymentTerms<S>,
    rate_reset: &RateResetTerms<S>,
    s0: &LoanState<S>,
    ev: &Event<S>,
    obs: &dyn RiskFactorObserver<S>,
) -> Result<Fixed<S>> {
    let t = ev.schedule_time;
    if prepayment_amount(calc.common, prepayment, ev, s0.notional_principal, obs)?.is_zero() {
        return Ok(Fixed::ZERO);
    }
    let fx = calc.fx(t, obs)?;
    let r = calc.role_sign();
    let c = fx * r * calc.yf(s0.status_date, t) * s0.notional_principal;
    Ok(match prepayment.penalty_type {
        PenaltyType::A => fx * r * prepayment.penalty_rate,
        PenaltyType::N => c * prepayment.penalty_rate,
        PenaltyType::I => {
            let moc = rate_reset
                .market_object_code
                .as_deref()
                .ok_or_else(|| Error::MissingTerm("marketObjectCodeOfRateReset".into()))?;
            let m = obs.risk_factor(moc, t)?;
            c * (s0.nominal_interest_rate - m).max(Fixed::ZERO)
        }
        PenaltyType::O => Fixed::ZERO,
    })
}

/// Initial accrued interest: explicit term, else estimated over the current interest period.
pub(crate) fn initial_accrued_interest<const S: u32>(
    calc: &LoanCalc<'_, S>,
    rate: Option<Fixed<S>>,
    accrued: Option<Fixed<S>>,
    ip_times: &[DateTime],
    ied: DateTime,
    nt: Fixed<S>,
    ipnr: Fixed<S>,
) -> Fixed<S> {
    let sd = calc.common.status_date;
    if rate.is_none() {
        return Fixed::ZERO;
    }
    if let Some(a) = accrued {
        return a;
    }
    if ied >= sd {
        return Fixed::ZERO;
    }
    let prev = ip_times.iter().rev().find(|t| **t < sd).copied().unwrap_or(ied).max(ied);
    calc.yf(prev, sd) * nt * ipnr
}

/// Initial fee accrued: explicit term, else estimated over the current fee period.
pub(crate) fn initial_fee_accrued<const S: u32>(calc: &LoanCalc<'_, S>, nt: Fixed<S>) -> Fixed<S> {
    let Some(rate) = calc.fees.rate else { return Fixed::ZERO };
    if let Some(a) = calc.fees.accrued {
        return a;
    }
    let sd = calc.common.status_date;
    let fp = calc.fee_schedule();
    let prev = fp.iter().rev().find(|t| **t < sd).copied();
    let next = fp.iter().find(|t| **t > sd).copied();
    match (prev, next, calc.fees.basis) {
        (Some(p), Some(n), Some(crate::terms::FeeBasis::A)) => calc.yf(p, sd) / calc.yf(p, n) * rate,
        (Some(p), _, _) => calc.yf(p, sd) * nt * rate,
        _ => Fixed::ZERO,
    }
}

impl<const S: u32> ContractModel<S> for PamTerms<S> {
    type State = LoanState<S>;

    fn common(&self) -> &CommonTerms {
        &self.common
    }

    fn initial_state(&self, _obs: &dyn RiskFactorObserver<S>) -> Result<LoanState<S>> {
        let calc = self.calc();
        let sd = self.common.status_date;
        let started = self.initial_exchange_date <= sd;
        let nt = if started { self.common.role_sign() * self.notional_principal } else { Fixed::ZERO };
        let ipnr = if started { self.rate() } else { Fixed::ZERO };
        let ipac = initial_accrued_interest(
            &calc,
            self.nominal_interest_rate,
            // Terms are role-neutral; the state is signed by the contract role.
            self.accrued_interest.map(|a| self.common.role_sign() * a),
            &self.ip_schedule(),
            self.initial_exchange_date,
            nt,
            ipnr,
        );
        Ok(LoanState {
            status_date: sd,
            maturity_date: Some(self.maturity_date),
            notional_principal: nt,
            nominal_interest_rate: ipnr,
            accrued_interest: ipac,
            fee_accrued: initial_fee_accrued(&calc, nt),
            notional_scaling_multiplier: self.scaling.notional_multiplier,
            interest_scaling_multiplier: self.scaling.interest_multiplier,
            contract_performance: self.common.contract_performance,
            next_principal_redemption_payment: None,
            interest_calculation_base_amount: None,
            non_performing_date: None,
        })
    }

    fn schedule(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>> {
        let calc = self.calc();
        let (cur, cid) = (&self.common.currency, &self.common.contract_id);
        let mut evs = vec![
            Event::at(EventType::IED, self.initial_exchange_date, cur, cid),
            Event::at(EventType::MD, self.maturity_date, cur, cid),
        ];
        if let Some(prd) = self.trade.purchase_date {
            evs.push(Event::at(EventType::PRD, prd, cur, cid));
        }
        let ip = self.ip_schedule();
        if !ip.is_empty() {
            evs.extend(interest_events(&calc, &ip, self.capitalization_end_date));
        } else if let Some(ced) = self.capitalization_end_date {
            evs.extend(calc.events(&[ced], EventType::IPCI));
        }
        evs.extend(rate_reset_events(&calc, &self.rate_reset, self.maturity_date));
        evs.extend(calc.events(&calc.fee_schedule(), EventType::FP));
        if self.scaling.effect.any() {
            let times = schedule(
                self.scaling.anchor,
                self.maturity_date,
                self.scaling.cycle,
                self.common.end_of_month,
                false,
            );
            evs.extend(calc.events(&times, EventType::SC));
        }
        evs.extend(observed_loan_events(
            &calc,
            &self.prepayment,
            self.maturity_date,
            obs,
        ));
        apply_termination(&mut evs, self.trade.termination_date, cur, cid);
        clip_events(&mut evs, self.common.status_date, to);
        sort_events(&mut evs);
        Ok(evs)
    }

    fn is_reported(&self, event: &Event<S>, _all: &[Event<S>]) -> bool {
        after_purchase(event, self.trade.purchase_date)
    }

    fn step(&self, s0: &LoanState<S>, ev: &Event<S>, obs: &dyn RiskFactorObserver<S>) -> Result<(LoanState<S>, Fixed<S>)> {
        let calc = self.calc();
        let t = ev.schedule_time;
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
                s.accrued_interest = match (self.accrued_interest, self.interest_payment_anchor) {
                    (Some(a), _) => r * a,
                    (None, Some(ipanx)) if ipanx < t => {
                        calc.yf(ipanx, t) * s.notional_principal * s.nominal_interest_rate
                    }
                    _ => Fixed::ZERO,
                };
                fx * r * -(self.notional_principal + self.premium_discount_at_ied)
            }
            EventType::MD => {
                let p = fx
                    * (s0.notional_scaling_multiplier * s0.notional_principal
                        + s0.interest_scaling_multiplier * (s0.accrued_interest + accrued)
                        + s0.fee_accrued);
                s.notional_principal = Fixed::ZERO;
                s.accrued_interest = Fixed::ZERO;
                s.fee_accrued = Fixed::ZERO;
                p
            }
            EventType::PP => {
                let amount = prepayment_amount(&self.common, &self.prepayment, ev, s0.notional_principal, obs)?;
                s.accrued_interest += accrued;
                calc.accrue_fee(&mut s, t);
                s.notional_principal -= amount;
                fx * amount
            }
            EventType::PY => {
                let p = penalty_payoff(&calc, &self.prepayment, &self.rate_reset, s0, ev, obs)?;
                s.accrued_interest += accrued;
                calc.accrue_fee(&mut s, t);
                p
            }
            EventType::FP => {
                let p = calc.fee_payoff(s0, t, obs)?;
                s.accrued_interest += accrued;
                s.fee_accrued = Fixed::ZERO;
                p
            }
            EventType::PRD => {
                s.accrued_interest += accrued;
                calc.accrue_fee(&mut s, t);
                // Accrued interest is already signed by the role; only the price needs R.
                fx * -(r * self.trade.price_at_purchase_date + s0.accrued_interest + accrued)
            }
            EventType::TD => {
                let p = fx * (r * self.trade.price_at_termination_date + s0.accrued_interest + accrued);
                s.notional_principal = Fixed::ZERO;
                s.accrued_interest = Fixed::ZERO;
                s.fee_accrued = Fixed::ZERO;
                p
            }
            EventType::IP => {
                let p = fx * s0.interest_scaling_multiplier * (s0.accrued_interest + accrued);
                s.accrued_interest = Fixed::ZERO;
                calc.accrue_fee(&mut s, t);
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
            EventType::SC => {
                s.accrued_interest += accrued;
                calc.accrue_fee(&mut s, t);
                let multiplier = scaling_multiplier(&self.scaling, t, obs)?;
                if self.scaling.effect.interest {
                    s.interest_scaling_multiplier = multiplier;
                }
                if self.scaling.effect.notional {
                    s.notional_scaling_multiplier = multiplier;
                }
                Fixed::ZERO
            }
            other => return Err(Error::Unsupported(format!("event {other} for PAM"))),
        };
        s.status_date = t;
        Ok((s, payoff))
    }
}

/// Contract performance carried by a credit event (defaults to DF).
pub(crate) fn credit_event_performance<const S: u32>(ev: &Event<S>) -> Option<ContractPerformance> {
    Some(ev.performance.unwrap_or(ContractPerformance::DF))
}

/// Index scaling multiplier: current index value relative to the index at contract deal date.
pub(crate) fn scaling_multiplier<const S: u32>(
    sc: &ScalingTerms<S>,
    t: DateTime,
    obs: &dyn RiskFactorObserver<S>,
) -> Result<Fixed<S>> {
    let moc = sc
        .market_object_code
        .as_deref()
        .ok_or_else(|| Error::MissingTerm("marketObjectCodeOfScalingIndex".into()))?;
    let base = sc
        .index_at_contract_deal_date
        .ok_or_else(|| Error::MissingTerm("scalingIndexAtContractDealDate".into()))?;
    Ok(obs.risk_factor(moc, t)? / base)
}
