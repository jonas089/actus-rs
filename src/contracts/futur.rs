//! FUTUR — Future on a market-observed underlying, cash settled.

use super::loan::after_purchase;
use super::optns::{observed_exercise, ExerciseState, ExerciseTerms};
use crate::contract::ContractModel;
use crate::error::{Error, Result};
use crate::events::{Event, EventType};
use crate::fixed::Fixed;
use crate::observer::RiskFactorObserver;
use crate::terms::{CommonTerms, RawTerms};
use crate::time::DateTime;

#[derive(Clone, Debug, PartialEq)]
pub struct FuturTerms<const S: u32> {
    pub common: CommonTerms,
    pub exercise: ExerciseTerms<S>,
    pub futures_price: Fixed<S>,
}

impl<const S: u32> FuturTerms<S> {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        Ok(FuturTerms {
            common: CommonTerms::parse(raw)?,
            exercise: ExerciseTerms::parse(raw)?,
            futures_price: raw.req_fixed("futuresPrice")?,
        })
    }
}

impl<const S: u32> ContractModel<S> for FuturTerms<S> {
    type State = ExerciseState<S>;

    fn common(&self) -> &CommonTerms {
        &self.common
    }

    fn initial_state(&self, _obs: &dyn RiskFactorObserver<S>) -> Result<ExerciseState<S>> {
        Ok(self.exercise.initial_state(&self.common))
    }

    fn schedule(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>> {
        let observed = observed_exercise(&self.common, self.exercise.maturity_date, obs);
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
        let x = self.exercise.underlying_price(t, obs)? - self.futures_price;
        match ev.event_type {
            EventType::MD => s.exercise_date = if x.is_zero() { None } else { Some(t) },
            EventType::XD => s.exercise_amount = x,
            other => return Err(Error::Unsupported(format!("event {other} for FUTUR"))),
        }
        s.status_date = t;
        Ok((s, Fixed::ZERO))
    }
}
