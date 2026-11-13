//! State and helpers shared by the maturity-based lending contracts
//! (PAM, LAM, LAX, NAM, ANN, CLM, UMP).

use crate::contract::settlement_fx;
use crate::daycount::DayCounter;
use crate::error::Result;
use crate::events::{Event, EventType};
use crate::fixed::Fixed;
use crate::impl_state_view;
use crate::observer::RiskFactorObserver;
use crate::terms::{CommonTerms, ContractPerformance, FeeBasis, FeeTerms, RateResetTerms};
use crate::time::{schedule, DateTime};

/// State of a lending contract. Signed amounts (notional, accrued interest) are from the
/// creator's perspective: positive for an asset (`RPA`), negative for a liability (`RPL`).
#[derive(Clone, Debug, PartialEq)]
pub struct LoanState<const S: u32> {
    pub status_date: DateTime,
    pub maturity_date: Option<DateTime>,
    pub notional_principal: Fixed<S>,
    pub nominal_interest_rate: Fixed<S>,
    pub accrued_interest: Fixed<S>,
    pub fee_accrued: Fixed<S>,
    pub notional_scaling_multiplier: Fixed<S>,
    pub interest_scaling_multiplier: Fixed<S>,
    pub contract_performance: ContractPerformance,
    pub next_principal_redemption_payment: Option<Fixed<S>>,
    pub interest_calculation_base_amount: Option<Fixed<S>>,
    pub non_performing_date: Option<DateTime>,
}

impl_state_view!(LoanState {
    status_date => StatusDate,
    maturity_date => MaturityDate,
    notional_principal => NotionalPrincipal,
    nominal_interest_rate => NominalInterestRate,
    accrued_interest => AccruedInterest,
    fee_accrued => FeeAccrued,
    notional_scaling_multiplier => NotionalScalingMultiplier,
    interest_scaling_multiplier => InterestScalingMultiplier,
    contract_performance => ContractPerformance,
    next_principal_redemption_payment => NextPrincipalRedemptionPayment,
    interest_calculation_base_amount => InterestCalculationBaseAmount,
    non_performing_date => NonPerformingDate,
});

/// Calculation context shared by the lending contracts.
pub struct LoanCalc<'a, const S: u32> {
    pub common: &'a CommonTerms,
    pub day_counter: &'a DayCounter,
    pub fees: &'a FeeTerms<S>,
    pub fee_schedule_end: DateTime,
}

impl<'a, const S: u32> LoanCalc<'a, S> {
    /// Year fraction between two schedule times, each shifted per the calc/shift convention.
    pub fn yf(&self, from: DateTime, to: DateTime) -> Fixed<S> {
        let bd = &self.common.business_day;
        self.day_counter.year_fraction(bd.shift_calc_time(from), bd.shift_calc_time(to))
    }

    pub fn role_sign(&self) -> Fixed<S> {
        self.common.role_sign()
    }

    pub fn fx(&self, t: DateTime, obs: &dyn RiskFactorObserver<S>) -> Result<Fixed<S>> {
        settlement_fx(self.common, t, obs)
    }

    /// Interest accrued since the last status date on `base` (normally the notional).
    pub fn interest_since(&self, s: &LoanState<S>, t: DateTime, base: Fixed<S>) -> Fixed<S> {
        self.yf(s.status_date, t) * s.nominal_interest_rate * base
    }

    /// Fee accrual up to `t` (spec: `Feac` update common to most events).
    pub fn accrue_fee(&self, s: &mut LoanState<S>, t: DateTime) {
        let Some(rate) = self.fees.rate else { return };
        match self.fees.basis {
            Some(FeeBasis::A) => {
                let fp = self.fee_schedule();
                let prev = fp.iter().rev().find(|x| **x < t).copied();
                let next = fp.iter().find(|x| **x > t).copied();
                if let (Some(p), Some(n)) = (prev, next) {
                    s.fee_accrued = self.yf(p, t) / self.yf(p, n) * self.role_sign() * rate;
                }
            }
            _ => {
                s.fee_accrued += self.yf(s.status_date, t) * s.notional_principal * rate;
            }
        }
    }

    /// Fee payment schedule times.
    pub fn fee_schedule(&self) -> Vec<DateTime> {
        if self.fees.cycle.is_none() || self.fees.rate.is_none_or(|r| r.is_zero()) {
            return Vec::new();
        }
        schedule(self.fees.anchor, self.fee_schedule_end, self.fees.cycle, self.common.end_of_month, true)
    }

    /// FP payoff.
    pub fn fee_payoff(&self, s: &LoanState<S>, t: DateTime, obs: &dyn RiskFactorObserver<S>) -> Result<Fixed<S>> {
        let rate = self.fees.rate_or_zero();
        let fx = self.fx(t, obs)?;
        Ok(match self.fees.basis {
            Some(FeeBasis::A) => fx * self.role_sign() * rate,
            _ => fx * (self.yf(s.status_date, t) * rate * s.notional_principal + s.fee_accrued),
        })
    }

    /// New nominal rate after a rate reset observing `market_rate`.
    pub fn reset_rate(&self, rr: &RateResetTerms<S>, current: Fixed<S>, market_rate: Fixed<S>) -> Fixed<S> {
        let target = market_rate * rr.multiplier + rr.spread;
        let delta = (target - current).clamp_opt(rr.period_floor, rr.period_cap);
        (current + delta).clamp_opt(rr.life_floor, rr.life_cap)
    }

    /// Build events for a list of schedule times, shifting event times per the BDC.
    pub fn events(&self, times: &[DateTime], ty: EventType) -> Vec<Event<S>> {
        times
            .iter()
            .map(|t| {
                Event::new(
                    ty,
                    *t,
                    self.common.business_day.shift_event_time(*t),
                    &self.common.currency,
                    &self.common.contract_id,
                )
            })
            .collect()
    }

    pub fn event_at(&self, t: DateTime, ty: EventType) -> Event<S> {
        Event::at(ty, t, &self.common.currency, &self.common.contract_id)
    }
}

/// Keep only events in `[status date, to]` (by sort key, as for an AD event at those times).
pub fn clip_events<const S: u32>(events: &mut Vec<Event<S>>, status_date: DateTime, to: Option<DateTime>) {
    let lo = crate::events::sort_key(status_date, EventType::AD);
    events.retain(|e| e.key() >= lo);
    if let Some(to) = to {
        let hi = crate::events::sort_key(to, EventType::AD);
        events.retain(|e| e.key() <= hi);
    }
}

/// Apply a termination date: drop everything after it and add the TD event.
pub fn apply_termination<const S: u32>(events: &mut Vec<Event<S>>, td: Option<DateTime>, currency: &str, cid: &str) {
    if let Some(td) = td {
        let ev = Event::at(EventType::TD, td, currency, cid);
        let k = ev.key();
        events.retain(|e| e.key() <= k);
        events.push(ev);
    }
}

/// Report only events at or after the purchase date (if any).
pub fn after_purchase<const S: u32>(event: &Event<S>, purchase_date: Option<DateTime>) -> bool {
    match purchase_date {
        Some(prd) => event.event_type == EventType::AD || event.key() >= crate::events::sort_key(prd, EventType::PRD),
        None => true,
    }
}
