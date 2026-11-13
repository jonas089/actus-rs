//! OPTNS — Option (European / American / Bermudan, call, put, collar) on a market-observed
//! underlying, cash settled.
//!
//! Also hosts the exercise/settlement machinery shared with FUTUR.

use super::loan::{after_purchase, apply_termination, clip_events};
use crate::contract::{settlement_fx, ContractModel};
use crate::error::{Error, Result};
use crate::events::{sort_events, Event, EventType};
use crate::fixed::Fixed;
use crate::impl_state_view;
use crate::observer::RiskFactorObserver;
use crate::terms::{CommonTerms, ContractPerformance, OptionExerciseType, OptionType, RawTerms, TradeTerms};
use crate::time::{DateTime, Period};
use crate::{parse_contract_structure, ReferenceObject, ReferenceRole};

/// State of an exercisable derivative (OPTNS, FUTUR).
#[derive(Clone, Debug, PartialEq)]
pub struct ExerciseState<const S: u32> {
    pub status_date: DateTime,
    pub maturity_date: DateTime,
    pub exercise_date: Option<DateTime>,
    pub exercise_amount: Fixed<S>,
    pub contract_performance: ContractPerformance,
}

impl_state_view!(ExerciseState {
    status_date => StatusDate,
    maturity_date => MaturityDate,
    exercise_date => ExerciseDate,
    exercise_amount => ExerciseAmount,
    contract_performance => ContractPerformance,
});

/// Terms shared by OPTNS and FUTUR: underlying, maturity, exercise and settlement.
#[derive(Clone, Debug, PartialEq)]
pub struct ExerciseTerms<const S: u32> {
    pub maturity_date: DateTime,
    /// Market object code of the underlying (`CTST` reference with role UDL).
    pub underlying: String,
    pub exercise_date: Option<DateTime>,
    pub exercise_amount: Option<Fixed<S>>,
    pub settlement_period: Period,
    pub trade: TradeTerms<S>,
}

impl<const S: u32> ExerciseTerms<S> {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        let structure = parse_contract_structure(raw)?;
        let underlying = structure
            .iter()
            .find(|r| r.role == ReferenceRole::UDL)
            .map(|r| match &r.object {
                ReferenceObject::MarketObject(m) => Ok(m.clone()),
                ReferenceObject::Contract(c) => c
                    .text("marketObjectCode")
                    .ok_or_else(|| Error::invalid("contractStructure", "underlying contract without marketObjectCode")),
                ReferenceObject::ContractId(id) => Ok(id.clone()),
            })
            .transpose()?
            .ok_or_else(|| Error::MissingTerm("contractStructure (underlying)".into()))?;
        Ok(ExerciseTerms {
            maturity_date: raw.req_date("maturityDate")?,
            underlying,
            exercise_date: raw.date("exerciseDate")?,
            exercise_amount: raw.fixed("exerciseAmount")?,
            settlement_period: raw.period("settlementPeriod")?.unwrap_or(Period::ZERO),
            trade: TradeTerms::parse(raw)?,
        })
    }

    pub fn initial_state(&self, common: &CommonTerms) -> ExerciseState<S> {
        ExerciseState {
            status_date: common.status_date,
            maturity_date: self.maturity_date,
            exercise_date: self.exercise_date,
            exercise_amount: self.exercise_amount.unwrap_or(Fixed::ZERO),
            contract_performance: common.contract_performance,
        }
    }

    /// PRD, MD, XD, STD and TD events. `observed_exercise` is an exercise date observed for
    /// American/Bermudan options (used only when no exercise date is fixed in the terms).
    pub fn schedule(
        &self,
        common: &CommonTerms,
        observed_exercise: Option<DateTime>,
        to: Option<DateTime>,
    ) -> Vec<Event<S>> {
        let (cur, cid) = (common.currency.as_str(), common.contract_id.as_str());
        let mut evs = Vec::new();
        if let Some(prd) = self.trade.purchase_date {
            evs.push(Event::at(EventType::PRD, prd, cur, cid));
        }
        let xd = self.exercise_date.or(observed_exercise).unwrap_or(self.maturity_date);
        evs.push(Event::at(EventType::XD, xd, cur, cid));
        let std = self.settlement_period.add_to(xd);
        evs.push(Event::new(EventType::STD, std, common.business_day.shift_event_time(std), cur, cid));
        evs.push(Event::at(EventType::MD, self.maturity_date, cur, cid));
        apply_termination(&mut evs, self.trade.termination_date, cur, cid);
        clip_events(&mut evs, common.status_date, to);
        sort_events(&mut evs);
        evs
    }

    /// Observed price of the underlying at `t`.
    pub fn underlying_price(&self, t: DateTime, obs: &dyn RiskFactorObserver<S>) -> Result<Fixed<S>> {
        obs.risk_factor(&self.underlying, t)
    }

    /// Payoff and state for the events common to OPTNS and FUTUR (PRD, TD, STD, AD, CE).
    /// Returns `None` for MD and XD, which are type specific.
    pub fn step_common(
        &self,
        common: &CommonTerms,
        s: &mut ExerciseState<S>,
        ev: &Event<S>,
        obs: &dyn RiskFactorObserver<S>,
    ) -> Result<Option<Fixed<S>>> {
        let t = ev.schedule_time;
        let fx = settlement_fx(common, t, obs)?;
        let r: Fixed<S> = common.role_sign();
        let payoff = match ev.event_type {
            EventType::PRD => fx * r * -self.trade.price_at_purchase_date,
            EventType::TD => fx * self.trade.price_at_termination_date,
            EventType::STD => {
                let p = fx * r * s.exercise_amount;
                s.exercise_amount = Fixed::ZERO;
                p
            }
            EventType::AD => Fixed::ZERO,
            EventType::CE => {
                s.contract_performance = ev.performance.unwrap_or(ContractPerformance::DF);
                Fixed::ZERO
            }
            _ => return Ok(None),
        };
        s.status_date = t;
        Ok(Some(payoff))
    }
}

/// First exercise event observed for this contract on or before maturity.
pub(crate) fn observed_exercise<const S: u32>(
    common: &CommonTerms,
    maturity: DateTime,
    obs: &dyn RiskFactorObserver<S>,
) -> Option<DateTime> {
    obs.events(&common.contract_id)
        .into_iter()
        .filter(|e| e.event_type == EventType::XD && e.time <= maturity)
        .map(|e| e.time)
        .min()
}

#[derive(Clone, Debug, PartialEq)]
pub struct OptnsTerms<const S: u32> {
    pub common: CommonTerms,
    pub exercise: ExerciseTerms<S>,
    pub option_type: OptionType,
    pub exercise_type: OptionExerciseType,
    pub strike_1: Fixed<S>,
    pub strike_2: Option<Fixed<S>>,
}

impl<const S: u32> OptnsTerms<S> {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        Ok(OptnsTerms {
            common: CommonTerms::parse(raw)?,
            exercise: ExerciseTerms::parse(raw)?,
            option_type: raw.parse_enum("optionType")?.ok_or_else(|| Error::MissingTerm("optionType".into()))?,
            exercise_type: raw.enum_or("optionExerciseType", OptionExerciseType::E)?,
            strike_1: raw.req_fixed("optionStrike1")?,
            strike_2: raw.fixed("optionStrike2")?,
        })
    }

    /// Intrinsic value at underlying price `st`.
    pub fn intrinsic(&self, st: Fixed<S>) -> Result<Fixed<S>> {
        let call = (st - self.strike_1).max(Fixed::ZERO);
        Ok(match self.option_type {
            OptionType::C => call,
            OptionType::P => (self.strike_1 - st).max(Fixed::ZERO),
            OptionType::CP => {
                let k2 = self.strike_2.ok_or_else(|| Error::MissingTerm("optionStrike2".into()))?;
                call + (k2 - st).max(Fixed::ZERO)
            }
        })
    }
}

impl<const S: u32> ContractModel<S> for OptnsTerms<S> {
    type State = ExerciseState<S>;

    fn common(&self) -> &CommonTerms {
        &self.common
    }

    fn initial_state(&self, _obs: &dyn RiskFactorObserver<S>) -> Result<ExerciseState<S>> {
        Ok(self.exercise.initial_state(&self.common))
    }

    fn schedule(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>> {
        let observed = if self.exercise_type == OptionExerciseType::E {
            None
        } else {
            observed_exercise(&self.common, self.exercise.maturity_date, obs)
        };
        Ok(self.exercise.schedule(&self.common, observed, to))
    }

    fn is_reported(&self, event: &Event<S>, _all: &[Event<S>]) -> bool {
        after_purchase(event, self.exercise.trade.purchase_date)
    }

    fn step(
        &self,
        s0: &ExerciseState<S>,
        ev: &Event<S>,
        obs: &dyn RiskFactorObserver<S>,
    ) -> Result<(ExerciseState<S>, Fixed<S>)> {
        let mut s = s0.clone();
        if let Some(p) = self.exercise.step_common(&self.common, &mut s, ev, obs)? {
            return Ok((s, p));
        }
        let t = ev.schedule_time;
        match ev.event_type {
            EventType::MD => {
                // At maturity an in-the-money option is exercised (unless already exercised).
                if s0.exercise_date.is_none() {
                    let x = self.intrinsic(self.exercise.underlying_price(t, obs)?)?;
                    s.exercise_date = if x.is_zero() { None } else { Some(t) };
                }
            }
            EventType::XD => {
                s.exercise_amount = self.intrinsic(self.exercise.underlying_price(t, obs)?)?;
            }
            other => return Err(Error::Unsupported(format!("event {other} for OPTNS"))),
        }
        s.status_date = t;
        Ok((s, Fixed::ZERO))
    }
}
