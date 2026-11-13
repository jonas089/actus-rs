//! LAX — Exotic Linear Amortizer.
//!
//! A linear amortizer whose principal schedule, interest schedule and rate resets are given as
//! arrays of segments. Each principal segment has its own anchor, cycle, fixed amount and
//! direction: `DEC` segments redeem principal (PR events), `INC` segments draw more principal
//! (PI events). Each rate-reset segment is either fixed (`RRF`: the segment's rate becomes the
//! nominal rate) or variable (`RR`: the market rate plus the segment's rate as extra spread).
//!
//! Everything that is not array-driven (IED, MD, IP, fees, scaling, prepayments, ...) behaves
//! exactly as for LAM, so this type wraps an [`AmortizerTerms`] and delegates those events.

use super::lam::{AmortizerKind, AmortizerTerms};
use super::loan::{apply_termination, clip_events, LoanState};
use super::pam::{interest_events, observed_loan_events};
use crate::contract::ContractModel;
use crate::error::{Error, Result};
use crate::events::{sort_events, Event, EventType};
use crate::fixed::Fixed;
use crate::observer::RiskFactorObserver;
use crate::terms::{CommonTerms, InterestCalculationBase, RawTerms};
use crate::time::{array_schedule, parse_datetime, schedule, Cycle, DateTime};

/// One segment of the principal schedule (`ARPRANX[i]`, `ARPRCL[i]`, `ARPRNXT[i]`, `ARINCDEC[i]`).
#[derive(Clone, Debug, PartialEq)]
pub struct PrincipalSegment<const S: u32> {
    pub anchor: DateTime,
    pub cycle: Option<Cycle>,
    /// Amount per event (unsigned, role-neutral).
    pub payment: Fixed<S>,
    /// `true` for `INC` (principal increase), `false` for `DEC` (redemption).
    pub increase: bool,
}

/// One segment of the rate-reset schedule (`ARRRANX[i]`, `ARRRCL[i]`, `ARRATE[i]`, `ARFIXVAR[i]`).
#[derive(Clone, Debug, PartialEq)]
pub struct RateResetSegment<const S: u32> {
    pub anchor: DateTime,
    pub cycle: Option<Cycle>,
    pub rate: Fixed<S>,
    /// `true` for `F`/`FIX` (the segment rate is the new rate), `false` for `V`/`VAR`.
    pub fixed: bool,
}

/// LAX terms.
#[derive(Clone, Debug, PartialEq)]
pub struct LaxTerms<const S: u32> {
    /// LAM-equivalent terms (maturity resolved from the arrays when not given).
    pub base: AmortizerTerms<S>,
    pub principal: Vec<PrincipalSegment<S>>,
    pub interest_anchors: Vec<DateTime>,
    pub interest_cycles: Vec<Option<Cycle>>,
    pub rate_resets: Vec<RateResetSegment<S>>,
}

fn parse_list<T>(raw: &RawTerms, key: &str, f: impl Fn(&str) -> Result<T>) -> Result<Vec<T>> {
    raw.list(key).iter().map(|s| f(s).map_err(|_| Error::invalid(key, s))).collect()
}

fn parse_fixed<const S: u32>(s: &str) -> Result<Fixed<S>> {
    Fixed::parse(s).map_err(|_| Error::invalid("fixed", s))
}

/// Element `i` of an optional per-segment list (an empty list means "not given").
fn nth<T: Clone>(v: &[T], i: usize, key: &str) -> Result<Option<T>> {
    if v.is_empty() {
        return Ok(None);
    }
    v.get(i).cloned().map(Some).ok_or_else(|| Error::invalid(key, "array shorter than its anchor array"))
}

impl<const S: u32> LaxTerms<S> {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        let pr_anchors = parse_list(raw, "arrayCycleAnchorDateOfPrincipalRedemption", parse_datetime)?;
        let pr_cycles = parse_list(raw, "arrayCycleOfPrincipalRedemption", Cycle::parse)?;
        let pr_payments = parse_list(raw, "arrayNextPrincipalRedemptionPayment", parse_fixed::<S>)?;
        let pr_dirs = raw.list("arrayIncreaseDecrease");
        let mut principal = Vec::with_capacity(pr_anchors.len());
        for (i, anchor) in pr_anchors.iter().enumerate() {
            let payment = nth(&pr_payments, i, "arrayNextPrincipalRedemptionPayment")?
                .ok_or_else(|| Error::MissingTerm("arrayNextPrincipalRedemptionPayment".into()))?;
            let increase = match nth(&pr_dirs, i, "arrayIncreaseDecrease")?.as_deref() {
                Some("INC") => true,
                Some("DEC") | None => false,
                Some(other) => return Err(Error::invalid("arrayIncreaseDecrease", other)),
            };
            principal.push(PrincipalSegment {
                anchor: *anchor,
                cycle: nth(&pr_cycles, i, "arrayCycleOfPrincipalRedemption")?,
                payment,
                increase,
            });
        }

        let interest_anchors = parse_list(raw, "arrayCycleAnchorDateOfInterestPayment", parse_datetime)?;
        let ip_cycles = parse_list(raw, "arrayCycleOfInterestPayment", Cycle::parse)?;
        let interest_cycles = (0..interest_anchors.len())
            .map(|i| nth(&ip_cycles, i, "arrayCycleOfInterestPayment"))
            .collect::<Result<Vec<_>>>()?;

        let rr_anchors = parse_list(raw, "arrayCycleAnchorDateOfRateReset", parse_datetime)?;
        let rr_cycles = parse_list(raw, "arrayCycleOfRateReset", Cycle::parse)?;
        let rr_rates = parse_list(raw, "arrayRate", parse_fixed::<S>)?;
        let rr_fixvar = raw.list("arrayFixedVariable");
        let mut rate_resets = Vec::with_capacity(rr_anchors.len());
        for (i, anchor) in rr_anchors.iter().enumerate() {
            let fixed = match nth(&rr_fixvar, i, "arrayFixedVariable")?.as_deref() {
                Some("F") | Some("FIX") => true,
                Some("V") | Some("VAR") | None => false,
                Some(other) => return Err(Error::invalid("arrayFixedVariable", other)),
            };
            rate_resets.push(RateResetSegment {
                anchor: *anchor,
                cycle: nth(&rr_cycles, i, "arrayCycleOfRateReset")?,
                rate: nth(&rr_rates, i, "arrayRate")?.unwrap_or(Fixed::ZERO),
                fixed,
            });
        }

        let maturity = match raw.date("maturityDate")? {
            Some(md) => md,
            None => {
                let common = CommonTerms::parse(raw)?;
                let t = derived_maturity(&principal, raw.req_fixed("notionalPrincipal")?, &common)?;
                common.business_day.shift_event_time(t)
            }
        };
        let mut base = AmortizerTerms::parse_with(raw, AmortizerKind::Lam, Some(maturity))?;
        // The scalar PR/IP terms are not used by LAX; point them at the first segments so the
        // shared LAM logic (initial state, IED accrual) sees consistent values.
        base.principal_redemption_anchor = principal.first().map(|p| p.anchor).unwrap_or(base.initial_exchange_date);
        base.principal_redemption_cycle = principal.first().and_then(|p| p.cycle);
        base.next_principal_redemption_payment = Some(principal.first().map(|p| p.payment).unwrap_or(Fixed::ZERO));
        base.interest_payment_anchor = interest_anchors.first().copied();
        base.interest_payment_cycle = interest_cycles.first().copied().flatten();
        Ok(LaxTerms { base, principal, interest_anchors, interest_cycles, rate_resets })
    }

    /// End of segment `i`: the next segment's anchor, or the maturity date.
    fn segment_end(&self, anchors: impl Fn(usize) -> Option<DateTime>, i: usize) -> DateTime {
        anchors(i + 1).unwrap_or(self.base.maturity_date)
    }

    /// Principal segment governing a PR/PI event scheduled at `t`.
    fn principal_segment(&self, t: DateTime) -> Option<&PrincipalSegment<S>> {
        self.principal.iter().rev().find(|p| p.anchor <= t)
    }

    fn rate_reset_segment(&self, t: DateTime) -> Option<&RateResetSegment<S>> {
        self.rate_resets.iter().rev().find(|r| r.anchor <= t)
    }
}

/// Maturity implied by the principal segments when no maturity date is given: the date of the
/// event that brings the outstanding notional to zero (unshifted). Without cycles, the last
/// anchor.
fn derived_maturity<const S: u32>(segs: &[PrincipalSegment<S>], notional: Fixed<S>, common: &CommonTerms) -> Result<DateTime> {
    let last = segs.last().ok_or_else(|| Error::MissingTerm("maturityDate".into()))?;
    if segs.iter().all(|p| p.cycle.is_none()) {
        return Ok(last.anchor);
    }
    let mut outstanding = notional;
    for (i, seg) in segs.iter().enumerate() {
        let apply = |outstanding: &mut Fixed<S>| {
            if seg.increase {
                *outstanding += seg.payment;
            } else {
                *outstanding -= seg.payment;
            }
        };
        if let Some(next) = segs.get(i + 1) {
            for t in schedule(Some(seg.anchor), next.anchor, seg.cycle, common.end_of_month, false) {
                apply(&mut outstanding);
                if !outstanding.is_positive() {
                    return Ok(t);
                }
            }
        } else {
            let cycle = seg.cycle.ok_or_else(|| Error::MissingTerm("maturityDate".into()))?;
            if seg.increase || !seg.payment.is_positive() {
                return Err(Error::invalid("arrayIncreaseDecrease", "last segment does not amortize; give maturityDate"));
            }
            // Count events with a bounded loop (the notional decreases strictly each event).
            let mut k = 0i64;
            loop {
                apply(&mut outstanding);
                if !outstanding.is_positive() {
                    return Ok(cycle.period.times(k).add_to(seg.anchor));
                }
                k += 1;
                if k > 1_000_000 {
                    return Err(Error::invalid("arrayNextPrincipalRedemptionPayment", "too many redemption periods"));
                }
            }
        }
    }
    unreachable!("the last segment always returns")
}

impl<const S: u32> ContractModel<S> for LaxTerms<S> {
    type State = LoanState<S>;

    fn common(&self) -> &CommonTerms {
        &self.base.common
    }

    fn initial_state(&self, obs: &dyn RiskFactorObserver<S>) -> Result<LoanState<S>> {
        let mut s = self.base.initial_state(obs)?;
        let sd = self.base.common.status_date;
        s.next_principal_redemption_payment =
            Some(self.principal_segment(sd).or(self.principal.first()).map(|p| p.payment).unwrap_or(Fixed::ZERO));
        Ok(s)
    }

    fn schedule(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>> {
        let b = &self.base;
        let calc = b.calc();
        let (cur, cid) = (&b.common.currency, &b.common.contract_id);
        let md = b.maturity_date;
        let eomc = b.common.end_of_month;
        let mut evs = vec![Event::at(EventType::IED, b.initial_exchange_date, cur, cid)];
        if let Some(prd) = b.trade.purchase_date {
            evs.push(Event::at(EventType::PRD, prd, cur, cid));
        }
        for (i, seg) in self.principal.iter().enumerate() {
            let end = self.segment_end(|j| self.principal.get(j).map(|p| p.anchor), i);
            let ty = if seg.increase { EventType::PI } else { EventType::PR };
            evs.extend(calc.events(&schedule(Some(seg.anchor), end, seg.cycle, eomc, false), ty));
        }
        evs.extend(calc.events(&[md], EventType::MD));
        if !self.interest_anchors.is_empty() {
            let ip = array_schedule(&self.interest_anchors, &self.interest_cycles, md, eomc);
            evs.extend(interest_events(&calc, &ip, b.capitalization_end_date));
        } else if let Some(ced) = b.capitalization_end_date {
            evs.extend(calc.events(&[ced], EventType::IPCI));
        }
        let mut rr = Vec::new();
        for (i, seg) in self.rate_resets.iter().enumerate() {
            let end = self.segment_end(|j| self.rate_resets.get(j).map(|r| r.anchor), i);
            let ty = if seg.fixed { EventType::RRF } else { EventType::RR };
            rr.extend(calc.events(&schedule(Some(seg.anchor), end, seg.cycle, eomc, false), ty));
        }
        evs.extend(rr);
        evs.extend(calc.events(&calc.fee_schedule(), EventType::FP));
        if b.scaling.effect.any() {
            let times = schedule(b.scaling.anchor, md, b.scaling.cycle, eomc, false);
            evs.extend(calc.events(&times, EventType::SC));
        }
        if b.interest_calculation_base == InterestCalculationBase::NTL {
            let times =
                schedule(b.interest_calculation_base_anchor, md, b.interest_calculation_base_cycle, eomc, false);
            evs.extend(calc.events(&times, EventType::IPCB));
        }
        evs.extend(observed_loan_events(&calc, &b.prepayment, obs));
        apply_termination(&mut evs, b.trade.termination_date, cur, cid);
        clip_events(&mut evs, b.common.status_date, to.or(Some(md)));
        sort_events(&mut evs);
        Ok(evs)
    }

    fn is_reported(&self, event: &Event<S>, all: &[Event<S>]) -> bool {
        self.base.is_reported(event, all)
    }

    fn step(&self, s0: &LoanState<S>, ev: &Event<S>, obs: &dyn RiskFactorObserver<S>) -> Result<(LoanState<S>, Fixed<S>)> {
        let b = &self.base;
        let calc = b.calc();
        let t = ev.schedule_time;
        let r = b.common.role_sign();
        let ipcb0 = s0.interest_calculation_base_amount.unwrap_or(s0.notional_principal);
        let accrue = |s: &mut LoanState<S>| {
            s.accrued_interest += calc.interest_since(s0, t, ipcb0);
            calc.accrue_fee(s, t);
        };
        let follow_nt = b.interest_calculation_base != InterestCalculationBase::NTL;
        match ev.event_type {
            EventType::PR | EventType::PI => {
                let seg = self
                    .principal_segment(t)
                    .ok_or_else(|| Error::invalid("arrayCycleAnchorDateOfPrincipalRedemption", "no segment for event"))?;
                let fx = calc.fx(t, obs)?;
                let mut s = s0.clone();
                accrue(&mut s);
                s.next_principal_redemption_payment = Some(seg.payment);
                let payoff = if ev.event_type == EventType::PR {
                    // Redeem the segment amount, but never more than is outstanding.
                    let red = seg.payment - (seg.payment - s0.notional_principal.abs()).max(Fixed::ZERO);
                    s.notional_principal -= r * red;
                    fx * r * s0.notional_scaling_multiplier * red
                } else {
                    s.notional_principal += r * seg.payment;
                    fx * -r * s0.notional_scaling_multiplier * seg.payment
                };
                if follow_nt {
                    s.interest_calculation_base_amount = Some(s.notional_principal);
                }
                s.status_date = t;
                Ok((s, payoff))
            }
            EventType::RR | EventType::RRF => {
                let mut s = s0.clone();
                accrue(&mut s);
                let rr = &b.rate_reset;
                let seg = self.rate_reset_segment(t);
                let first_after_sd = self.first_reset_after_status_date() == Some(ev.schedule_time);
                s.nominal_interest_rate = match (rr.next_reset_rate, first_after_sd) {
                    // An already-fixed next rate replaces the first reset after the status date.
                    (Some(nrr), true) => nrr,
                    _ => match ev.event_type {
                        EventType::RRF => {
                            let rate = seg.map(|g| g.rate).or(rr.next_reset_rate).unwrap_or(s0.nominal_interest_rate);
                            calc.reset_rate(rr, s0.nominal_interest_rate, rate)
                        }
                        _ => {
                            let moc = rr
                                .market_object_code
                                .as_deref()
                                .ok_or_else(|| Error::MissingTerm("marketObjectCodeOfRateReset".into()))?;
                            let market = obs.risk_factor(moc, t)?;
                            let mut terms = rr.clone();
                            terms.spread += seg.map(|g| g.rate).unwrap_or(Fixed::ZERO);
                            calc.reset_rate(&terms, s0.nominal_interest_rate, market)
                        }
                    },
                };
                s.status_date = t;
                Ok((s, Fixed::ZERO))
            }
            _ => b.step(s0, ev, obs),
        }
    }
}

impl<const S: u32> LaxTerms<S> {
    /// Schedule time of the first rate reset strictly after the status date.
    fn first_reset_after_status_date(&self) -> Option<DateTime> {
        let sd = self.base.common.status_date;
        let eomc = self.base.common.end_of_month;
        self.rate_resets
            .iter()
            .enumerate()
            .flat_map(|(i, seg)| {
                let end = self.segment_end(|j| self.rate_resets.get(j).map(|r| r.anchor), i);
                schedule(Some(seg.anchor), end, seg.cycle, eomc, false)
            })
            .filter(|t| *t > sd)
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::parse_datetime as d;

    fn terms(extra: serde_json::Value) -> LaxTerms<18> {
        let mut v = serde_json::json!({
            "contractType": "LAX", "contractID": "x", "contractRole": "RPA", "currency": "USD",
            "statusDate": "2020-01-01T00:00:00", "initialExchangeDate": "2020-01-02T00:00:00",
            "dayCountConvention": "A360", "notionalPrincipal": "120", "nominalInterestRate": "0.05",
        });
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        LaxTerms::parse(&RawTerms::from_json(&v).unwrap()).unwrap()
    }

    #[test]
    fn maturity_from_increasing_then_decreasing_segments() {
        // +20 yearly 2021..2026 (notional 240), then -20 monthly: zero after 12 payments.
        let t = terms(serde_json::json!({
            "arrayCycleAnchorDateOfPrincipalRedemption": ["2021-01-01T00:00:00", "2027-01-01T00:00:00"],
            "arrayCycleOfPrincipalRedemption": ["P1YL1", "P1ML1"],
            "arrayNextPrincipalRedemptionPayment": [20, 20],
            "arrayIncreaseDecrease": ["INC", "DEC"],
        }));
        assert_eq!(t.base.maturity_date, d("2027-12-01").unwrap());
    }

    #[test]
    fn maturity_without_cycles_is_last_anchor() {
        let t = terms(serde_json::json!({
            "arrayCycleAnchorDateOfPrincipalRedemption": ["2021-01-01T00:00:00", "2022-01-01T00:00:00"],
            "arrayNextPrincipalRedemptionPayment": [60, 60],
            "arrayIncreaseDecrease": ["DEC", "DEC"],
        }));
        assert_eq!(t.base.maturity_date, d("2022-01-01").unwrap());
    }
}
