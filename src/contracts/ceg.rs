//! CEG — Credit Enhancement Guarantee.
//!
//! A guarantee covers the exposure of one or more contracts. If a covered contract suffers a
//! credit event of a covered type, the guarantee is exercised (XD) and the guaranteed amount
//! settles (STD). Otherwise it expires at maturity (MD). The guarantor may charge fees (FP).
//!
//! Also hosts the covered/covering-contract machinery shared with CEC.

use super::loan::clip_events;
use crate::contract::{settlement_fx, ContractModel};
use crate::daycount::{DayCountConvention, DayCounter};
use crate::error::{Error, Result};
use crate::events::{sort_events, Event, EventType};
use crate::fixed::Fixed;
use crate::impl_state_view;
use crate::observer::{ObservedEvent, RiskFactorObserver};
use crate::state::{StateSnapshot, StateVariable, Value};
use crate::terms::{
    parse_day_counter, CommonTerms, ContractPerformance, FeeBasis, FeeTerms, GuaranteedExposure, RawTerms,
};
use crate::time::{schedule, DateTime, Period};
use crate::{parse_contract_structure, Contract, ReferenceObject, ReferenceRole};
use std::str::FromStr;

// ---------------------------------------------------------------------------------------------
// Shared credit-enhancement machinery (CEG, CEC)
// ---------------------------------------------------------------------------------------------

/// A contract covered by a credit enhancement.
#[derive(Clone, Debug, PartialEq)]
pub enum Covered<const S: u32> {
    /// Embedded contract, evaluated by the engine. Always evaluated from the creditor's side
    /// (contract role `RPA`), whatever role its terms state.
    Contract { id: String, contract: Box<Contract<S>>, maturity: Option<DateTime>, market_object_code: Option<String> },
    /// Contract known only by its ID; credit events for it arrive as observed events.
    Id(String),
}

impl<const S: u32> Covered<S> {
    pub fn id(&self) -> &str {
        match self {
            Covered::Contract { id, .. } | Covered::Id(id) => id,
        }
    }
}

/// Terms shared by CEG and CEC.
#[derive(Clone, Debug, PartialEq)]
pub struct CreditEnhancementTerms<const S: u32> {
    pub covered: Vec<Covered<S>>,
    /// Market object codes of the covering contracts (collateral), CEC only.
    pub covering: Vec<String>,
    pub guaranteed_exposure: GuaranteedExposure,
    pub coverage: Fixed<S>,
    pub credit_events_covered: Vec<ContractPerformance>,
    pub maturity_date: DateTime,
    pub exercise_date: Option<DateTime>,
    pub exercise_amount: Option<Fixed<S>>,
    pub settlement_period: Period,
}

impl<const S: u32> CreditEnhancementTerms<S> {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        let mut covered = Vec::new();
        let mut covering = Vec::new();
        for r in parse_contract_structure(raw)? {
            match (r.role, r.object) {
                (ReferenceRole::COVE, ReferenceObject::Contract(mut terms)) => {
                    terms.0.insert("contractRole".into(), "RPA".into());
                    let id = terms.text("contractID").unwrap_or_default();
                    let maturity = terms.date("maturityDate")?;
                    let market_object_code = terms.text("marketObjectCode");
                    let contract = Contract::from_raw(&terms).map_err(|e| Error::Child(format!("{id}: {e}")))?;
                    covered.push(Covered::Contract { id, contract: Box::new(contract), maturity, market_object_code });
                }
                (ReferenceRole::COVE, ReferenceObject::ContractId(id)) => covered.push(Covered::Id(id)),
                (ReferenceRole::COVE, ReferenceObject::MarketObject(m)) => covered.push(Covered::Id(m)),
                (ReferenceRole::COVI, ReferenceObject::Contract(terms)) => covering.push(
                    terms
                        .text("marketObjectCode")
                        .ok_or_else(|| Error::invalid("contractStructure", "covering contract without marketObjectCode"))?,
                ),
                (ReferenceRole::COVI, ReferenceObject::MarketObject(m)) => covering.push(m),
                (role, _) => return Err(Error::invalid("contractStructure", format!("unsupported reference {role:?}"))),
            }
        }
        let maturity_date = match raw.date("maturityDate")? {
            Some(md) => md,
            None => covered
                .iter()
                .map(|c| match c {
                    Covered::Contract { maturity, .. } => *maturity,
                    Covered::Id(_) => None,
                })
                .collect::<Option<Vec<_>>>()
                .and_then(|v| v.into_iter().max())
                .ok_or_else(|| Error::MissingTerm("maturityDate".into()))?,
        };
        let mut credit_events_covered = Vec::new();
        for s in raw.list("creditEventTypeCovered") {
            credit_events_covered
                .push(ContractPerformance::from_str(&s).map_err(|_| Error::invalid("creditEventTypeCovered", &s))?);
        }
        if credit_events_covered.is_empty() {
            credit_events_covered.push(ContractPerformance::DF);
        }
        Ok(CreditEnhancementTerms {
            covered,
            covering,
            guaranteed_exposure: raw.enum_or("guaranteedExposure", GuaranteedExposure::NO)?,
            coverage: raw.fixed_or("coverageOfCreditEnhancement", Fixed::ONE)?,
            credit_events_covered,
            maturity_date,
            exercise_date: raw.date("exerciseDate")?,
            exercise_amount: raw.fixed("exerciseAmount")?,
            settlement_period: raw.period("settlementPeriod")?.unwrap_or(Period::ZERO),
        })
    }

    /// Time of the first covered credit event on a covered contract, on or before maturity.
    pub fn credit_event_time(&self, obs: &dyn RiskFactorObserver<S>) -> Option<DateTime> {
        self.covered
            .iter()
            .flat_map(|c| obs.events(c.id()))
            .filter(|e| e.time <= self.maturity_date)
            .filter(|e| match e.states.get(StateVariable::ContractPerformance) {
                Some(Value::Performance(p)) => self.credit_events_covered.contains(&p),
                _ => false,
            })
            .map(|e| e.time)
            .min()
    }

    /// XD + STD on exercise (fixed exercise date or observed credit event), else MD.
    pub fn schedule(&self, common: &CommonTerms, obs: &dyn RiskFactorObserver<S>) -> Vec<Event<S>> {
        let (cur, cid) = (common.currency.as_str(), common.contract_id.as_str());
        match self.exercise_date.or_else(|| self.credit_event_time(obs)) {
            Some(xd) => {
                let std = self.settlement_period.add_to(xd);
                vec![
                    Event::at(EventType::XD, xd, cur, cid),
                    Event::new(EventType::STD, std, common.business_day.shift_event_time(std), cur, cid),
                ]
            }
            None => vec![Event::at(EventType::MD, self.maturity_date, cur, cid)],
        }
    }

    /// Covered exposure at `t`, before coverage and role sign: notional (NO), notional plus
    /// accrued interest (NI) or market value (MV) of the covered contracts.
    pub fn exposure(&self, t: DateTime, obs: &dyn RiskFactorObserver<S>) -> Result<Fixed<S>> {
        let mut total = Fixed::ZERO;
        for c in &self.covered {
            let Covered::Contract { contract, market_object_code, id, .. } = c else {
                return Err(Error::Unsupported(format!(
                    "exposure of covered contract {} referenced only by ID (give notionalPrincipal)",
                    c.id()
                )));
            };
            total += match self.guaranteed_exposure {
                GuaranteedExposure::MV => {
                    let moc = market_object_code
                        .as_deref()
                        .ok_or_else(|| Error::MissingTerm(format!("marketObjectCode of {id}")))?;
                    obs.risk_factor(moc, t)?
                }
                g => {
                    let st = child_state_at(contract, t, obs)?;
                    let n = |v| st.as_ref().and_then(|s| s.number(v)).unwrap_or(Fixed::ZERO);
                    let nt = n(StateVariable::NotionalPrincipal);
                    if g == GuaranteedExposure::NI { nt + n(StateVariable::AccruedInterest) } else { nt }
                }
            };
        }
        Ok(total)
    }

    /// Market value of the covering contracts (collateral) at `t`.
    pub fn covering_value(&self, t: DateTime, obs: &dyn RiskFactorObserver<S>) -> Result<Fixed<S>> {
        let mut total = Fixed::ZERO;
        for moc in &self.covering {
            total += obs.risk_factor(moc, t)?;
        }
        Ok(total)
    }
}

/// State of a child contract at `t`, including accruals up to `t` (a monitoring event is
/// injected at `t`, like the spec's child observer `U^sv`).
fn child_state_at<const S: u32>(
    child: &Contract<S>,
    t: DateTime,
    obs: &dyn RiskFactorObserver<S>,
) -> Result<Option<StateSnapshot<S>>> {
    let monitored = Monitored { inner: obs, contract_id: child.contract_id().to_string(), time: t };
    child.state_at(t, &monitored).map_err(|e| Error::Child(format!("{}: {e}", child.contract_id())))
}

struct Monitored<'a, const S: u32> {
    inner: &'a dyn RiskFactorObserver<S>,
    contract_id: String,
    time: DateTime,
}

impl<const S: u32> RiskFactorObserver<S> for Monitored<'_, S> {
    fn risk_factor(&self, id: &str, time: DateTime) -> Result<Fixed<S>> {
        self.inner.risk_factor(id, time)
    }
    fn events(&self, contract_id: &str) -> Vec<ObservedEvent<S>> {
        let mut evs = self.inner.events(contract_id);
        if contract_id == self.contract_id {
            evs.push(ObservedEvent {
                contract_id: contract_id.to_string(),
                event_type: EventType::AD,
                time: self.time,
                value: Fixed::ZERO,
                states: StateSnapshot::default(),
            });
        }
        evs
    }
}

/// State of a credit enhancement (CEG, CEC).
#[derive(Clone, Debug, PartialEq)]
pub struct CreditEnhancementState<const S: u32> {
    pub status_date: DateTime,
    pub maturity_date: DateTime,
    pub notional_principal: Fixed<S>,
    pub fee_accrued: Fixed<S>,
    pub exercise_date: Option<DateTime>,
    pub exercise_amount: Fixed<S>,
    pub contract_performance: ContractPerformance,
}

impl_state_view!(CreditEnhancementState {
    status_date => StatusDate,
    maturity_date => MaturityDate,
    notional_principal => NotionalPrincipal,
    fee_accrued => FeeAccrued,
    exercise_date => ExerciseDate,
    exercise_amount => ExerciseAmount,
    contract_performance => ContractPerformance,
});

// ---------------------------------------------------------------------------------------------
// CEG
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct CegTerms<const S: u32> {
    pub common: CommonTerms,
    pub enhancement: CreditEnhancementTerms<S>,
    pub day_counter: DayCounter,
    /// Fixed guaranteed amount; if absent the covered exposure is used.
    pub notional_principal: Option<Fixed<S>>,
    pub fees: FeeTerms<S>,
    pub purchase_date: Option<DateTime>,
    pub price_at_purchase_date: Fixed<S>,
}

impl<const S: u32> CegTerms<S> {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        let common = CommonTerms::parse(raw)?;
        let enhancement = CreditEnhancementTerms::parse(raw)?;
        let purchase_date = raw.date("purchaseDate")?;
        let mut fees = FeeTerms::parse(raw, None)?;
        // Fee anchor defaults to one fee cycle after the purchase date.
        if let (None, Some(c), Some(prd)) = (fees.anchor, fees.cycle, purchase_date) {
            fees.anchor = Some(c.period.add_to(prd));
        }
        Ok(CegTerms {
            day_counter: parse_day_counter(raw, Some(enhancement.maturity_date), common.calendar())?
                .unwrap_or_else(|| DayCounter::new(DayCountConvention::A365)),
            notional_principal: raw.fixed("notionalPrincipal")?,
            fees,
            purchase_date,
            price_at_purchase_date: raw.fixed_or("priceAtPurchaseDate", Fixed::ZERO)?,
            enhancement,
            common,
        })
    }

    fn yf(&self, from: DateTime, to: DateTime) -> Fixed<S> {
        let bd = &self.common.business_day;
        self.day_counter.year_fraction(bd.shift_calc_time(from), bd.shift_calc_time(to))
    }

    fn fee_schedule(&self) -> Vec<DateTime> {
        let Some(rate) = self.fees.rate else { return Vec::new() };
        if rate.is_zero() || self.fees.anchor.is_none() {
            return Vec::new();
        }
        let end = self.enhancement.exercise_date.unwrap_or(self.enhancement.maturity_date);
        schedule(self.fees.anchor, end, self.fees.cycle, self.common.end_of_month, false)
    }

    /// Guaranteed notional at `t`: `CECV * R * NT`, or `CECV * R * exposure(t)` if no NT.
    fn notional(&self, t: DateTime, obs: &dyn RiskFactorObserver<S>) -> Result<Fixed<S>> {
        let base = match self.notional_principal {
            Some(nt) => nt,
            None => self.enhancement.exposure(t, obs)?,
        };
        Ok(self.enhancement.coverage * self.common.role_sign() * base)
    }

    /// Fee accrued at `t` given the pre-event state.
    fn accrue_fee(&self, s: &CreditEnhancementState<S>, t: DateTime) -> Fixed<S> {
        let Some(rate) = self.fees.rate.filter(|r| !r.is_zero()) else { return s.fee_accrued };
        match (self.fees.basis, self.fees.cycle) {
            (Some(FeeBasis::A), Some(cycle)) => {
                let fp = self.fee_schedule();
                let Some(prev) = fp.iter().rev().find(|x| **x < t).copied() else { return s.fee_accrued };
                let next = fp.iter().find(|x| **x > t).copied().unwrap_or_else(|| cycle.period.add_to(prev));
                self.yf(prev, t) / self.yf(prev, next) * self.common.role_sign() * rate
            }
            _ => s.fee_accrued + self.yf(s.status_date, t) * s.notional_principal * rate,
        }
    }
}

impl<const S: u32> ContractModel<S> for CegTerms<S> {
    type State = CreditEnhancementState<S>;

    fn common(&self) -> &CommonTerms {
        &self.common
    }

    fn initial_state(&self, obs: &dyn RiskFactorObserver<S>) -> Result<CreditEnhancementState<S>> {
        let sd = self.common.status_date;
        let md = self.enhancement.maturity_date;
        let nt = if sd >= md { Fixed::ZERO } else { self.notional(sd, obs)? };
        // Fee accrued at t0 (the Java reference leaves this unimplemented): explicit term, else
        // accrued over the current fee period.
        let fee_accrued = match (self.fees.rate, self.fees.accrued) {
            (None, _) => Fixed::ZERO,
            (Some(_), Some(a)) => a,
            (Some(rate), None) => {
                let fp = self.fee_schedule();
                match fp.iter().rev().find(|x| **x < sd).copied() {
                    None => Fixed::ZERO,
                    Some(prev) => match (self.fees.basis, fp.iter().find(|x| **x > sd).copied()) {
                        (Some(FeeBasis::A), Some(next)) => {
                            self.yf(prev, sd) / self.yf(prev, next) * self.common.role_sign() * rate
                        }
                        (Some(FeeBasis::A), None) => Fixed::ZERO,
                        _ => self.yf(prev, sd) * nt * rate,
                    },
                }
            }
        };
        Ok(CreditEnhancementState {
            status_date: sd,
            maturity_date: md,
            notional_principal: nt,
            fee_accrued,
            exercise_date: self.enhancement.exercise_date,
            exercise_amount: self.enhancement.exercise_amount.unwrap_or(Fixed::ZERO),
            contract_performance: self.common.contract_performance,
        })
    }

    fn schedule(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>> {
        let (cur, cid) = (self.common.currency.as_str(), self.common.contract_id.as_str());
        let mut evs = Vec::new();
        if let Some(prd) = self.purchase_date {
            evs.push(Event::at(EventType::PRD, prd, cur, cid));
        }
        for t in self.fee_schedule() {
            evs.push(Event::new(EventType::FP, t, self.common.business_day.shift_event_time(t), cur, cid));
        }
        evs.extend(self.enhancement.schedule(&self.common, obs));
        clip_events(&mut evs, self.common.status_date, to);
        sort_events(&mut evs);
        Ok(evs)
    }

    fn step(
        &self,
        s0: &CreditEnhancementState<S>,
        ev: &Event<S>,
        obs: &dyn RiskFactorObserver<S>,
    ) -> Result<(CreditEnhancementState<S>, Fixed<S>)> {
        let t = ev.schedule_time;
        let fx = settlement_fx(&self.common, t, obs)?;
        let r: Fixed<S> = self.common.role_sign();
        let mut s = s0.clone();
        let payoff = match ev.event_type {
            EventType::AD | EventType::CE => {
                s.fee_accrued = self.accrue_fee(s0, t);
                if ev.event_type == EventType::CE {
                    s.contract_performance = ev.performance.unwrap_or(ContractPerformance::DF);
                }
                Fixed::ZERO
            }
            EventType::PRD => {
                s.notional_principal = self.notional(t, obs)?;
                fx * r * -self.price_at_purchase_date
            }
            EventType::FP => {
                let rate = self.fees.rate_or_zero();
                let p = match self.fees.basis {
                    Some(FeeBasis::A) => fx * r * rate,
                    _ => fx * (s0.fee_accrued + self.yf(s0.status_date, t) * s0.notional_principal * rate),
                };
                s.notional_principal = self.notional(t, obs)?;
                s.fee_accrued = Fixed::ZERO;
                p
            }
            EventType::MD => {
                s.notional_principal = Fixed::ZERO;
                Fixed::ZERO
            }
            EventType::XD => {
                s.fee_accrued = self.accrue_fee(s0, t);
                s.notional_principal = self.notional(t, obs)?;
                // The notional already includes the coverage ratio.
                s.exercise_amount = s.notional_principal;
                s.exercise_date = Some(t);
                Fixed::ZERO
            }
            EventType::STD => {
                let p = fx * (s0.exercise_amount + s0.fee_accrued);
                s.notional_principal = Fixed::ZERO;
                s.fee_accrued = Fixed::ZERO;
                p
            }
            other => return Err(Error::Unsupported(format!("event {other} for CEG"))),
        };
        s.status_date = t;
        Ok((s, payoff))
    }
}
