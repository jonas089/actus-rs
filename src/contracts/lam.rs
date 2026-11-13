//! LAM — Linear Amortizer, and the amortizer machinery shared with NAM and ANN.
//!
//! LAM, NAM and ANN have the same terms and nearly the same event logic; they differ in how a
//! principal redemption (PR) is computed and in how a missing maturity date / redemption
//! amount is derived. [`AmortizerTerms`] implements all three, selected by [`AmortizerKind`];
//! the kind-specific pieces live in this file (LAM), `nam.rs` and `ann.rs`.

use super::loan::{after_purchase, apply_termination, clip_events, LoanCalc, LoanState};
use super::pam::{
    credit_event_performance, initial_accrued_interest, initial_fee_accrued, interest_events, observed_loan_events,
    rate_reset_events, scaling_multiplier,
};
use super::{ann, nam};
use crate::contract::ContractModel;
use crate::daycount::{DayCountConvention, DayCounter};
use crate::error::{Error, Result};
use crate::events::{sort_events, Event, EventType};
use crate::fixed::Fixed;
use crate::observer::RiskFactorObserver;
use crate::terms::{
    anchor, parse_day_counter, CommonTerms, ContractPerformance, FeeTerms, InterestCalculationBase, PenaltyType,
    PrepaymentTerms, RateResetTerms, RawTerms, ScalingTerms, TradeTerms,
};
use crate::time::{last_day_of_month, schedule, Cycle, DateTime, EndOfMonthConvention};

/// Which amortizing contract type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AmortizerKind {
    /// Linear amortizer: fixed principal redemption per period.
    Lam,
    /// Negative amortizer: fixed total payment (principal + interest) per period; the
    /// principal part is what is left after interest.
    Nam,
    /// Annuity: like NAM, but the total payment is recomputed so the loan is exactly repaid at
    /// maturity (and re-fixed after every rate reset).
    Ann,
}

/// Terms of LAM, NAM and ANN contracts.
#[derive(Clone, Debug, PartialEq)]
pub struct AmortizerTerms<const S: u32> {
    pub kind: AmortizerKind,
    pub common: CommonTerms,
    pub day_counter: DayCounter,
    pub initial_exchange_date: DateTime,
    /// Explicit maturity date; if absent it is derived (see [`AmortizerTerms::maturity`]).
    pub maturity_date_term: Option<DateTime>,
    /// ANN only: date by which the annuity amount is computed to fully amortize.
    pub amortization_date: Option<DateTime>,
    pub notional_principal: Fixed<S>,
    pub premium_discount_at_ied: Fixed<S>,
    pub nominal_interest_rate: Option<Fixed<S>>,
    pub accrued_interest: Option<Fixed<S>>,
    pub interest_payment_anchor: Option<DateTime>,
    pub interest_payment_cycle: Option<Cycle>,
    pub capitalization_end_date: Option<DateTime>,
    pub principal_redemption_anchor: DateTime,
    pub principal_redemption_cycle: Option<Cycle>,
    pub next_principal_redemption_payment: Option<Fixed<S>>,
    pub interest_calculation_base: InterestCalculationBase,
    pub interest_calculation_base_amount: Fixed<S>,
    pub interest_calculation_base_anchor: Option<DateTime>,
    pub interest_calculation_base_cycle: Option<Cycle>,
    pub rate_reset: RateResetTerms<S>,
    pub fees: FeeTerms<S>,
    pub scaling: ScalingTerms<S>,
    pub prepayment: PrepaymentTerms<S>,
    pub trade: TradeTerms<S>,
    /// Resolved maturity (explicit, derived, or amortization date for ANN).
    pub maturity_date: DateTime,
}

/// LAM terms.
pub type LamTerms<const S: u32> = AmortizerTerms<S>;

impl<const S: u32> AmortizerTerms<S> {
    pub fn parse(raw: &RawTerms, kind: AmortizerKind) -> Result<Self> {
        Self::parse_with(raw, kind, None)
    }

    /// Parse; `maturity` overrides the maturity derivation (used by LAX, which derives its
    /// maturity from its array terms).
    pub(crate) fn parse_with(raw: &RawTerms, kind: AmortizerKind, maturity: Option<DateTime>) -> Result<Self> {
        let common = CommonTerms::parse(raw)?;
        let ied = raw.req_date("initialExchangeDate")?;
        let md = raw.date("maturityDate")?;
        let def = Some(ied);
        let mut t = AmortizerTerms {
            kind,
            day_counter: parse_day_counter(raw, md, common.calendar())?
                .unwrap_or_else(|| DayCounter::new(DayCountConvention::A365)),
            initial_exchange_date: ied,
            maturity_date_term: md,
            amortization_date: if kind == AmortizerKind::Ann { raw.date("amortizationDate")? } else { None },
            notional_principal: raw.req_fixed("notionalPrincipal")?,
            premium_discount_at_ied: raw.fixed_or("premiumDiscountAtIED", Fixed::ZERO)?,
            nominal_interest_rate: raw.fixed("nominalInterestRate")?,
            accrued_interest: raw.fixed("accruedInterest")?,
            interest_payment_anchor: anchor(raw, "cycleAnchorDateOfInterestPayment", "cycleOfInterestPayment", def)?,
            interest_payment_cycle: raw.cycle("cycleOfInterestPayment")?,
            capitalization_end_date: raw.date("capitalizationEndDate")?,
            principal_redemption_anchor: raw.date("cycleAnchorDateOfPrincipalRedemption")?.unwrap_or(ied),
            principal_redemption_cycle: raw.cycle("cycleOfPrincipalRedemption")?,
            next_principal_redemption_payment: raw.fixed("nextPrincipalRedemptionPayment")?,
            interest_calculation_base: raw.enum_or("interestCalculationBase", InterestCalculationBase::NT)?,
            interest_calculation_base_amount: raw.fixed_or("interestCalculationBaseAmount", Fixed::ZERO)?,
            interest_calculation_base_anchor: anchor(
                raw,
                "cycleAnchorDateOfInterestCalculationBase",
                "cycleOfInterestCalculationBase",
                def,
            )?,
            interest_calculation_base_cycle: raw.cycle("cycleOfInterestCalculationBase")?,
            rate_reset: RateResetTerms::parse(raw, def)?,
            fees: FeeTerms::parse(raw, def)?,
            scaling: ScalingTerms::parse(raw, def)?,
            prepayment: PrepaymentTerms::parse(raw, def)?,
            trade: TradeTerms::parse(raw)?,
            common,
            maturity_date: ied,
        };
        t.maturity_date = match maturity {
            Some(m) => m,
            None => t.derive_maturity()?,
        };
        t.day_counter.maturity = Some(t.maturity_date);
        Ok(t)
    }

    fn derive_maturity(&self) -> Result<DateTime> {
        if let Some(md) = self.maturity_date_term {
            return Ok(md);
        }
        match self.kind {
            AmortizerKind::Lam => self.lam_maturity(),
            AmortizerKind::Nam => nam::derived_maturity(self),
            AmortizerKind::Ann => match self.amortization_date {
                Some(amd) => Ok(amd),
                None => nam::derived_maturity(self),
            },
        }
    }

    pub(crate) fn pr_cycle(&self) -> Result<Cycle> {
        self.principal_redemption_cycle.ok_or_else(|| Error::MissingTerm("cycleOfPrincipalRedemption".into()))
    }

    pub(crate) fn prnxt_term(&self) -> Result<Fixed<S>> {
        self.next_principal_redemption_payment
            .ok_or_else(|| Error::MissingTerm("nextPrincipalRedemptionPayment (or maturityDate)".into()))
    }

    /// Number of whole periods needed to redeem the notional at `per_period`, rounded up.
    pub(crate) fn periods_needed(&self, per_period: Fixed<S>) -> Result<i64> {
        if !per_period.is_positive() {
            return Err(Error::invalid("nextPrincipalRedemptionPayment", "does not amortize the notional"));
        }
        let q = self.notional_principal / per_period;
        let whole = q.raw() / Fixed::<S>::SCALE;
        Ok(if q.raw() % Fixed::<S>::SCALE != 0 { whole + 1 } else { whole } as i64)
    }

    /// LAM: last PR date before the status date (or the PR anchor) plus enough cycles to
    /// redeem the notional with the fixed redemption amount.
    fn lam_maturity(&self) -> Result<DateTime> {
        let cycle = self.pr_cycle()?;
        let n = self.periods_needed(self.prnxt_term()?)?;
        let sd = self.common.status_date;
        let anchor = self.principal_redemption_anchor;
        let (last, remaining) = if anchor >= sd {
            (anchor, n - 1)
        } else {
            let prev = schedule(Some(anchor), sd, Some(cycle), self.common.end_of_month, true)
                .into_iter()
                .filter(|t| *t < sd)
                .next_back()
                .unwrap_or(anchor);
            (prev, n)
        };
        let t = cycle.period.times(remaining).add_to(last);
        let eom = self.common.end_of_month == EndOfMonthConvention::EOM
            && crate::time::is_last_day_of_month(last)
            && cycle.period.months > 0;
        Ok(if eom { last_day_of_month(t) } else { t })
    }

    pub(crate) fn calc(&self) -> LoanCalc<'_, S> {
        LoanCalc {
            common: &self.common,
            day_counter: &self.day_counter,
            fees: &self.fees,
            fee_schedule_end: self.maturity_date,
        }
    }

    pub(crate) fn rate(&self) -> Fixed<S> {
        self.nominal_interest_rate.unwrap_or(Fixed::ZERO)
    }

    pub(crate) fn ip_schedule(&self) -> Vec<DateTime> {
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

    /// Principal redemption dates (maturity excluded; the MD event redeems the rest).
    pub(crate) fn pr_schedule(&self) -> Vec<DateTime> {
        schedule(
            Some(self.principal_redemption_anchor),
            self.maturity_date,
            self.principal_redemption_cycle,
            self.common.end_of_month,
            false,
        )
    }

    /// Initial interest calculation base for a running contract.
    fn initial_ipcb(&self, nt: Fixed<S>) -> Fixed<S> {
        match self.interest_calculation_base {
            InterestCalculationBase::NT => nt,
            _ => self.common.role_sign() * self.interest_calculation_base_amount,
        }
    }

    /// Accrued interest given as a term, signed by the contract role (terms are role-neutral).
    fn signed_accrued_term(&self) -> Option<Fixed<S>> {
        self.accrued_interest.map(|a| self.common.role_sign() * a)
    }

    /// Whether the interest calculation base follows the notional after PR events.
    fn ipcb_follows_notional_on_pr(&self) -> bool {
        match self.kind {
            AmortizerKind::Lam => self.interest_calculation_base != InterestCalculationBase::NTL,
            AmortizerKind::Nam | AmortizerKind::Ann => self.interest_calculation_base == InterestCalculationBase::NT,
        }
    }

    fn initial_prnxt(&self, s: &LoanState<S>) -> Result<Fixed<S>> {
        if let Some(p) = self.next_principal_redemption_payment {
            return Ok(p);
        }
        match self.kind {
            AmortizerKind::Lam => {
                // Notional spread evenly over the remaining PR dates (maturity included).
                let sd = self.common.status_date;
                let n = schedule(
                    Some(self.principal_redemption_anchor),
                    self.maturity_date,
                    self.principal_redemption_cycle,
                    self.common.end_of_month,
                    true,
                )
                .into_iter()
                .filter(|t| *t > sd)
                .count();
                if n == 0 {
                    return Err(Error::invalid("cycleOfPrincipalRedemption", "no redemption dates"));
                }
                Ok(self.notional_principal / Fixed::from_int(n as i64))
            }
            AmortizerKind::Nam => self.prnxt_term(),
            AmortizerKind::Ann => {
                if self.initial_exchange_date > self.common.status_date {
                    Ok(Fixed::ZERO)
                } else {
                    Ok(ann::annuity_amount(self, s))
                }
            }
        }
    }

    /// Principal redeemed by a PR event (unsigned) and its payoff.
    fn principal_redemption(&self, s: &LoanState<S>, accrued_since: Fixed<S>) -> Fixed<S> {
        let prnxt = s.next_principal_redemption_payment.unwrap_or(Fixed::ZERO);
        match self.kind {
            AmortizerKind::Lam => {
                let r = self.common.role_sign();
                prnxt - r * (prnxt.abs() - s.notional_principal.abs()).max(Fixed::ZERO)
            }
            AmortizerKind::Nam | AmortizerKind::Ann => nam::redemption(self, s, accrued_since),
        }
    }
}

impl<const S: u32> ContractModel<S> for AmortizerTerms<S> {
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
        let ipcb = if started { self.initial_ipcb(nt) } else { Fixed::ZERO };
        let ipac = initial_accrued_interest(
            &calc,
            self.nominal_interest_rate,
            // The official tests expect no estimate of interest accrued before the status date
            // when `accruedInterest` is not given (see docs/notes/loans.md).
            Some(self.signed_accrued_term().unwrap_or(Fixed::ZERO)),
            &self.ip_schedule(),
            self.initial_exchange_date,
            ipcb,
            ipnr,
        );
        let mut s = LoanState {
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
            interest_calculation_base_amount: Some(ipcb),
            non_performing_date: None,
        };
        s.next_principal_redemption_payment = Some(self.initial_prnxt(&s)?);
        Ok(s)
    }

    fn schedule(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>> {
        let calc = self.calc();
        let (cur, cid) = (&self.common.currency, &self.common.contract_id);
        let md = self.maturity_date;
        let mut evs = vec![Event::at(EventType::IED, self.initial_exchange_date, cur, cid)];
        evs.extend(calc.events(&self.pr_schedule(), EventType::PR));
        evs.extend(calc.events(&[md], EventType::MD));
        if let Some(prd) = self.trade.purchase_date {
            evs.push(Event::at(EventType::PRD, prd, cur, cid));
        }
        let ip = self.ip_schedule();
        if !ip.is_empty() {
            evs.extend(interest_events(&calc, &ip, self.capitalization_end_date));
        } else if let Some(ced) = self.capitalization_end_date {
            evs.extend(calc.events(&[ced], EventType::IPCI));
        }
        let rr = rate_reset_events(&calc, &self.rate_reset, md);
        if self.kind == AmortizerKind::Ann {
            evs.extend(ann::prf_events(self, &calc, &rr));
        }
        evs.extend(rr);
        evs.extend(calc.events(&calc.fee_schedule(), EventType::FP));
        if self.scaling.effect.any() {
            let times = schedule(self.scaling.anchor, md, self.scaling.cycle, self.common.end_of_month, false);
            evs.extend(calc.events(&times, EventType::SC));
        }
        if self.interest_calculation_base == InterestCalculationBase::NTL {
            let times = schedule(
                self.interest_calculation_base_anchor,
                md,
                self.interest_calculation_base_cycle,
                self.common.end_of_month,
                false,
            );
            evs.extend(calc.events(&times, EventType::IPCB));
        }
        evs.extend(observed_loan_events(&calc, &self.prepayment, obs));
        apply_termination(&mut evs, self.trade.termination_date, cur, cid);
        clip_events(&mut evs, self.common.status_date, to.or(Some(md)));
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
        let ipcb0 = s0.interest_calculation_base_amount.unwrap_or(s0.notional_principal);
        let accrued = calc.interest_since(s0, t, ipcb0);
        let follow_nt = |s: &mut LoanState<S>, cond: bool| {
            if cond {
                s.interest_calculation_base_amount = Some(s.notional_principal);
            }
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
                s.interest_calculation_base_amount = Some(self.initial_ipcb(s.notional_principal));
                s.accrued_interest = match (self.signed_accrued_term(), self.interest_payment_anchor) {
                    (Some(a), _) => a,
                    (None, Some(ipanx)) if ipanx < t => {
                        calc.yf(ipanx, t) * s.notional_principal * s.nominal_interest_rate
                    }
                    _ => Fixed::ZERO,
                };
                if self.kind == AmortizerKind::Ann && ann::fixes_amount_at_ied(self) {
                    s.status_date = t;
                    s.next_principal_redemption_payment = Some(r * ann::annuity_amount_from(self, &s, true));
                }
                fx * r * -(self.notional_principal + self.premium_discount_at_ied)
            }
            EventType::PR => {
                let redemption = self.principal_redemption(s0, accrued);
                s.accrued_interest += accrued;
                calc.accrue_fee(&mut s, t);
                s.notional_principal -= r * redemption;
                follow_nt(&mut s, self.ipcb_follows_notional_on_pr());
                fx * r * s0.notional_scaling_multiplier * redemption
            }
            EventType::MD => {
                let p = fx
                    * (s0.notional_scaling_multiplier * s0.notional_principal
                        + s0.interest_scaling_multiplier * (s0.accrued_interest + accrued)
                        + s0.fee_accrued);
                s.notional_principal = Fixed::ZERO;
                s.accrued_interest = Fixed::ZERO;
                s.fee_accrued = Fixed::ZERO;
                s.interest_calculation_base_amount = Some(Fixed::ZERO);
                p
            }
            EventType::PP => {
                s.accrued_interest += accrued;
                calc.accrue_fee(&mut s, t);
                let amount = r * ev.value;
                s.notional_principal -= amount;
                follow_nt(&mut s, self.interest_calculation_base == InterestCalculationBase::NT);
                fx * amount
            }
            EventType::PY => {
                s.accrued_interest += accrued;
                calc.accrue_fee(&mut s, t);
                let c = fx * r * calc.yf(s0.status_date, t) * s0.notional_principal;
                match self.prepayment.penalty_type {
                    PenaltyType::A => fx * r * self.prepayment.penalty_rate,
                    PenaltyType::N => c * self.prepayment.penalty_rate,
                    PenaltyType::I => {
                        let moc = self.rate_reset.market_object_code.as_deref().ok_or_else(|| {
                            Error::MissingTerm("marketObjectCodeOfRateReset".into())
                        })?;
                        let m = obs.risk_factor(moc, t)?;
                        c * (s0.nominal_interest_rate - m).max(Fixed::ZERO)
                    }
                    PenaltyType::O => Fixed::ZERO,
                }
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
                fx * r * -(self.trade.price_at_purchase_date + s0.accrued_interest + accrued)
            }
            EventType::TD => {
                let p = fx * r * (self.trade.price_at_termination_date + s0.accrued_interest + accrued);
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
                follow_nt(&mut s, self.interest_calculation_base != InterestCalculationBase::NTL);
                Fixed::ZERO
            }
            EventType::IPCB => {
                s.accrued_interest += accrued;
                calc.accrue_fee(&mut s, t);
                s.interest_calculation_base_amount = Some(s0.notional_principal);
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
            EventType::PRF => {
                s.accrued_interest += accrued;
                calc.accrue_fee(&mut s, t);
                s.status_date = t;
                s.next_principal_redemption_payment = Some(r * ann::annuity_amount(self, &s));
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
            other => return Err(Error::Unsupported(format!("event {other} for {:?}", self.kind))),
        };
        s.status_date = t;
        Ok((s, payoff))
    }
}
