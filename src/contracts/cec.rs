//! CEC — Credit Enhancement Collateral.
//!
//! Collateral (covering contracts, valued at market) secures the exposure of covered
//! contracts. On a covered credit event the collateral is liquidated up to the covered
//! exposure (XD) and settles (STD); otherwise the arrangement ends at maturity (MD).

use super::ceg::{CreditEnhancementState, CreditEnhancementTerms};
use super::loan::clip_events;
use crate::contract::{settlement_fx, ContractModel};
use crate::error::{Error, Result};
use crate::events::{sort_events, Event, EventType};
use crate::fixed::Fixed;
use crate::observer::RiskFactorObserver;
use crate::terms::{CommonTerms, RawTerms};
use crate::time::DateTime;

#[derive(Clone, Debug, PartialEq)]
pub struct CecTerms<const S: u32> {
    pub common: CommonTerms,
    pub enhancement: CreditEnhancementTerms<S>,
}

impl<const S: u32> CecTerms<S> {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        Ok(CecTerms { common: CommonTerms::parse(raw)?, enhancement: CreditEnhancementTerms::parse(raw)? })
    }

    /// Covered notional at `t`: `CECV * R * exposure(t)`.
    fn notional(&self, t: DateTime, obs: &dyn RiskFactorObserver<S>) -> Result<Fixed<S>> {
        Ok(self.enhancement.coverage * self.common.role_sign() * self.enhancement.exposure(t, obs)?)
    }
}

impl<const S: u32> ContractModel<S> for CecTerms<S> {
    type State = CreditEnhancementState<S>;

    fn common(&self) -> &CommonTerms {
        &self.common
    }

    fn initial_state(&self, obs: &dyn RiskFactorObserver<S>) -> Result<CreditEnhancementState<S>> {
        let sd = self.common.status_date;
        let md = self.enhancement.maturity_date;
        Ok(CreditEnhancementState {
            status_date: sd,
            maturity_date: md,
            notional_principal: if sd >= md { Fixed::ZERO } else { self.notional(sd, obs)? },
            fee_accrued: Fixed::ZERO,
            exercise_date: self.enhancement.exercise_date,
            exercise_amount: self.enhancement.exercise_amount.unwrap_or(Fixed::ZERO),
            contract_performance: self.common.contract_performance,
        })
    }

    fn schedule(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>> {
        let mut evs = self.enhancement.schedule(&self.common, obs);
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
        let mut s = s0.clone();
        let payoff = match ev.event_type {
            EventType::AD => {
                s.notional_principal =
                    if t >= self.enhancement.maturity_date { Fixed::ZERO } else { self.notional(t, obs)? };
                Fixed::ZERO
            }
            EventType::MD => {
                s.notional_principal = Fixed::ZERO;
                Fixed::ZERO
            }
            EventType::XD => {
                // Collateral is liquidated up to the covered exposure (in absolute terms; the
                // sign follows the contract role).
                let nt = self.notional(t, obs)?;
                let collateral = self.enhancement.covering_value(t, obs)?;
                s.notional_principal = nt;
                s.exercise_amount = nt.signum() * collateral.min(nt.abs());
                s.exercise_date = Some(t);
                Fixed::ZERO
            }
            EventType::STD => {
                let p = settlement_fx(&self.common, t, obs)? * s0.exercise_amount;
                s.notional_principal = Fixed::ZERO;
                p
            }
            other => return Err(Error::Unsupported(format!("event {other} for CEC"))),
        };
        s.status_date = t;
        Ok((s, payoff))
    }
}
