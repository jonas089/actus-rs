//! Type-erased contract state and the per-event interface of [`Contract`].
//!
//! Combined contracts (SWAPS, CAPFL) hold their children as [`Contract`]s and their children's
//! states as [`AnyState`]s, so a parent event applies its child events one `step` at a time.

use crate::contract::ContractModel;
use crate::contracts::capfl::CapflState;
use crate::contracts::ceg::CreditEnhancementState;
use crate::contracts::clm::ClmState;
use crate::contracts::csh::CshState;
use crate::contracts::fxout::FxoutState;
use crate::contracts::loan::LoanState;
use crate::contracts::optns::ExerciseState;
use crate::contracts::stk::OwnershipState;
use crate::contracts::swaps::SwapsState;
use crate::contracts::swppv::SwppvState;
use crate::error::{Error, Result};
use crate::events::Event;
use crate::fixed::Fixed;
use crate::observer::RiskFactorObserver;
use crate::state::{StateVariable, StateView, Value};
use crate::time::DateTime;
use crate::Contract;

/// The state of any contract type.
#[derive(Clone, Debug, PartialEq)]
pub enum AnyState<const S: u32> {
    /// PAM, LAM, LAX, NAM, ANN, UMP.
    Loan(LoanState<S>),
    Clm(ClmState<S>),
    Csh(CshState<S>),
    /// STK, COM.
    Ownership(OwnershipState<S>),
    Fxout(FxoutState<S>),
    Swppv(SwppvState<S>),
    Swaps(Box<SwapsState<S>>),
    Capfl(Box<CapflState<S>>),
    /// OPTNS, FUTUR.
    Exercise(ExerciseState<S>),
    /// CEG, CEC.
    CreditEnhancement(CreditEnhancementState<S>),
}

macro_rules! each_state {
    ($self:ident, $s:ident => $e:expr) => {
        match $self {
            AnyState::Loan($s) => $e,
            AnyState::Clm($s) => $e,
            AnyState::Csh($s) => $e,
            AnyState::Ownership($s) => $e,
            AnyState::Fxout($s) => $e,
            AnyState::Swppv($s) => $e,
            AnyState::Swaps($s) => $e,
            AnyState::Capfl($s) => $e,
            AnyState::Exercise($s) => $e,
            AnyState::CreditEnhancement($s) => $e,
        }
    };
}

impl<const S: u32> StateView<S> for AnyState<S> {
    fn get(&self, var: StateVariable) -> Option<Value<S>> {
        each_state!(self, s => s.get(var))
    }
    fn variables(&self) -> &'static [StateVariable] {
        each_state!(self, s => s.variables())
    }
}

impl<const S: u32> AnyState<S> {
    /// Numeric state variable, zero if the contract type does not carry it.
    pub fn number(&self, var: StateVariable) -> Fixed<S> {
        self.get(var).and_then(|v| v.as_number()).unwrap_or(Fixed::ZERO)
    }
}

impl<const S: u32> Contract<S> {
    /// State at the status date.
    pub fn initial_state(&self, obs: &dyn RiskFactorObserver<S>) -> Result<AnyState<S>> {
        Ok(match self {
            Contract::Pam(c) => AnyState::Loan(c.initial_state(obs)?),
            Contract::Ump(c) => AnyState::Loan(c.initial_state(obs)?),
            Contract::Lam(c) | Contract::Nam(c) | Contract::Ann(c) => AnyState::Loan(c.initial_state(obs)?),
            Contract::Lax(c) => AnyState::Loan(c.initial_state(obs)?),
            Contract::Clm(c) => AnyState::Clm(c.initial_state(obs)?),
            Contract::Csh(c) => AnyState::Csh(c.initial_state(obs)?),
            Contract::Stk(c) => AnyState::Ownership(c.initial_state(obs)?),
            Contract::Com(c) => AnyState::Ownership(c.initial_state(obs)?),
            Contract::Fxout(c) => AnyState::Fxout(c.initial_state(obs)?),
            Contract::Swppv(c) => AnyState::Swppv(c.initial_state(obs)?),
            Contract::Swaps(c) => AnyState::Swaps(Box::new(c.initial_state(obs)?)),
            Contract::Capfl(c) => AnyState::Capfl(Box::new(c.initial_state(obs)?)),
            Contract::Optns(c) => AnyState::Exercise(c.initial_state(obs)?),
            Contract::Futur(c) => AnyState::Exercise(c.initial_state(obs)?),
            Contract::Ceg(c) => AnyState::CreditEnhancement(c.initial_state(obs)?),
            Contract::Cec(c) => AnyState::CreditEnhancement(c.initial_state(obs)?),
        })
    }

    /// Events from the status date up to `to` (or the natural end), sorted.
    pub fn schedule(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>> {
        dispatch!(self, c => c.schedule(to, obs))
    }

    /// Whether an evaluated event is part of the reported result (see [`ContractModel::is_reported`]).
    pub fn is_reported(&self, event: &Event<S>, all: &[Event<S>]) -> bool {
        dispatch!(self, c => c.is_reported(event, all))
    }

    /// Apply one event (see [`ContractModel::step`]).
    pub fn step(
        &self,
        state: &AnyState<S>,
        event: &Event<S>,
        obs: &dyn RiskFactorObserver<S>,
    ) -> Result<(AnyState<S>, Fixed<S>)> {
        macro_rules! go {
            ($c:expr, $variant:ident) => {
                match state {
                    AnyState::$variant(st) => {
                        let (next, payoff) = $c.step(st, event, obs)?;
                        Ok((AnyState::$variant(next), payoff))
                    }
                    _ => Err(mismatch(self)),
                }
            };
        }
        macro_rules! go_boxed {
            ($c:expr, $variant:ident) => {
                match state {
                    AnyState::$variant(st) => {
                        let (next, payoff) = $c.step(st, event, obs)?;
                        Ok((AnyState::$variant(Box::new(next)), payoff))
                    }
                    _ => Err(mismatch(self)),
                }
            };
        }
        match self {
            Contract::Pam(c) => go!(c, Loan),
            Contract::Ump(c) => go!(c, Loan),
            Contract::Lam(c) | Contract::Nam(c) | Contract::Ann(c) => go!(c, Loan),
            Contract::Lax(c) => go!(c, Loan),
            Contract::Clm(c) => go!(c, Clm),
            Contract::Csh(c) => go!(c, Csh),
            Contract::Stk(c) => go!(c, Ownership),
            Contract::Com(c) => go!(c, Ownership),
            Contract::Fxout(c) => go!(c, Fxout),
            Contract::Swppv(c) => go!(c, Swppv),
            Contract::Swaps(c) => go_boxed!(c, Swaps),
            Contract::Capfl(c) => go_boxed!(c, Capfl),
            Contract::Optns(c) => go!(c, Exercise),
            Contract::Futur(c) => go!(c, Exercise),
            Contract::Ceg(c) => go!(c, CreditEnhancement),
            Contract::Cec(c) => go!(c, CreditEnhancement),
        }
    }
}

fn mismatch<const S: u32>(c: &Contract<S>) -> Error {
    Error::Unsupported(format!("state does not belong to contract {}", c.contract_id()))
}
