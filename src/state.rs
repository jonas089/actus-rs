//! Contract state variables.
//!
//! Each contract type has its own typed state struct; [`StateView`] exposes any of them
//! uniformly by spec state-variable name.

use crate::error::{Error, Result};
use crate::fixed::Fixed;
use crate::terms::ContractPerformance;
use crate::time::DateTime;
use serde::Serialize;
use std::collections::BTreeMap;

/// The ACTUS state variables (dictionary `states`), plus exercise states used by options/futures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StateVariable {
    StatusDate,
    MaturityDate,
    TerminationDate,
    NotionalPrincipal,
    NotionalPrincipal2,
    NominalInterestRate,
    NominalInterestRate2,
    AccruedInterest,
    AccruedInterest2,
    FeeAccrued,
    InterestCalculationBaseAmount,
    InterestScalingMultiplier,
    NotionalScalingMultiplier,
    NextPrincipalRedemptionPayment,
    ContractPerformance,
    NonPerformingDate,
    ExerciseDate,
    ExerciseAmount,
    BoundaryCrossedFlag,
    BoundaryLeg1ActiveFlag,
    BoundaryLeg2ActiveFlag,
}

impl StateVariable {
    /// Name as used in the ACTUS dictionary and test files.
    pub fn name(self) -> &'static str {
        use StateVariable::*;
        match self {
            StatusDate => "statusDate",
            MaturityDate => "maturityDate",
            TerminationDate => "terminationDate",
            NotionalPrincipal => "notionalPrincipal",
            NotionalPrincipal2 => "notionalPrincipal2",
            NominalInterestRate => "nominalInterestRate",
            NominalInterestRate2 => "nominalInterestRate2",
            AccruedInterest => "accruedInterest",
            AccruedInterest2 => "accruedInterest2",
            FeeAccrued => "feeAccrued",
            InterestCalculationBaseAmount => "interestCalculationBaseAmount",
            InterestScalingMultiplier => "interestScalingMultiplier",
            NotionalScalingMultiplier => "notionalScalingMultiplier",
            NextPrincipalRedemptionPayment => "nextPrincipalRedemptionPayment",
            ContractPerformance => "contractPerformance",
            NonPerformingDate => "nonPerformingDate",
            ExerciseDate => "exerciseDate",
            ExerciseAmount => "exerciseAmount",
            BoundaryCrossedFlag => "boundaryCrossedFlag",
            BoundaryLeg1ActiveFlag => "boundaryLeg1ActiveFlag",
            BoundaryLeg2ActiveFlag => "boundaryLeg2ActiveFlag",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        use StateVariable::*;
        [
            StatusDate,
            MaturityDate,
            TerminationDate,
            NotionalPrincipal,
            NotionalPrincipal2,
            NominalInterestRate,
            NominalInterestRate2,
            AccruedInterest,
            AccruedInterest2,
            FeeAccrued,
            InterestCalculationBaseAmount,
            InterestScalingMultiplier,
            NotionalScalingMultiplier,
            NextPrincipalRedemptionPayment,
            ContractPerformance,
            NonPerformingDate,
            ExerciseDate,
            ExerciseAmount,
            BoundaryCrossedFlag,
            BoundaryLeg1ActiveFlag,
            BoundaryLeg2ActiveFlag,
        ]
        .into_iter()
        .find(|v| v.name() == s)
    }
}

/// A state variable's value.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Value<const S: u32> {
    Number(Fixed<S>),
    Date(DateTime),
    Performance(ContractPerformance),
    Flag(bool),
}

impl<const S: u32> Value<S> {
    pub fn as_number(&self) -> Option<Fixed<S>> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }
}

/// Uniform read access to a typed contract state.
pub trait StateView<const S: u32> {
    /// Value of a state variable, or `None` if this contract type has no such state
    /// (or it is currently undefined).
    fn get(&self, var: StateVariable) -> Option<Value<S>>;

    /// The state variables this contract type carries.
    fn variables(&self) -> &'static [StateVariable];

    fn snapshot(&self) -> StateSnapshot<S> {
        StateSnapshot(self.variables().iter().filter_map(|v| self.get(*v).map(|x| (*v, x))).collect())
    }

    /// Error if any numeric state is poisoned by overflow or division by zero.
    fn check(&self, context: &str) -> Result<()> {
        for v in self.variables() {
            if let Some(Value::Number(n)) = self.get(*v)
                && n.is_poisoned() {
                    return Err(Error::Arithmetic { context: format!("{context} ({})", v.name()) });
                }
        }
        Ok(())
    }
}

/// A point-in-time copy of all state variables of a contract.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StateSnapshot<const S: u32>(pub BTreeMap<StateVariable, Value<S>>);

impl<const S: u32> StateSnapshot<S> {
    pub fn get(&self, v: StateVariable) -> Option<Value<S>> {
        self.0.get(&v).copied()
    }

    pub fn number(&self, v: StateVariable) -> Option<Fixed<S>> {
        self.get(v).and_then(|x| x.as_number())
    }
}

impl<const S: u32> StateView<S> for StateSnapshot<S> {
    fn get(&self, var: StateVariable) -> Option<Value<S>> {
        self.0.get(&var).copied()
    }
    fn variables(&self) -> &'static [StateVariable] {
        ALL_VARIABLES
    }
}

const ALL_VARIABLES: &[StateVariable] = &[
    StateVariable::StatusDate,
    StateVariable::MaturityDate,
    StateVariable::TerminationDate,
    StateVariable::NotionalPrincipal,
    StateVariable::NotionalPrincipal2,
    StateVariable::NominalInterestRate,
    StateVariable::NominalInterestRate2,
    StateVariable::AccruedInterest,
    StateVariable::AccruedInterest2,
    StateVariable::FeeAccrued,
    StateVariable::InterestCalculationBaseAmount,
    StateVariable::InterestScalingMultiplier,
    StateVariable::NotionalScalingMultiplier,
    StateVariable::NextPrincipalRedemptionPayment,
    StateVariable::ContractPerformance,
    StateVariable::NonPerformingDate,
    StateVariable::ExerciseDate,
    StateVariable::ExerciseAmount,
    StateVariable::BoundaryCrossedFlag,
    StateVariable::BoundaryLeg1ActiveFlag,
    StateVariable::BoundaryLeg2ActiveFlag,
];

/// Helper macro: implement [`StateView`] for a struct by listing `field => StateVariable` pairs.
/// Fields may be `Fixed<S>`, `Option<Fixed<S>>`, `DateTime`, `Option<DateTime>`,
/// `ContractPerformance` or `bool` (anything implementing [`IntoValue`]).
#[macro_export]
macro_rules! impl_state_view {
    ($ty:ident { $($field:ident => $var:ident),* $(,)? }) => {
        impl<const S: u32> $crate::state::StateView<S> for $ty<S> {
            fn get(&self, var: $crate::state::StateVariable) -> Option<$crate::state::Value<S>> {
                use $crate::state::IntoValue;
                match var {
                    $($crate::state::StateVariable::$var => self.$field.clone().into_value(),)*
                    #[allow(unreachable_patterns)]
                    _ => None,
                }
            }
            fn variables(&self) -> &'static [$crate::state::StateVariable] {
                &[$($crate::state::StateVariable::$var),*]
            }
        }
    };
}

/// Conversion of state fields into [`Value`]s.
pub trait IntoValue<const S: u32> {
    fn into_value(self) -> Option<Value<S>>;
}

impl<const S: u32> IntoValue<S> for Fixed<S> {
    fn into_value(self) -> Option<Value<S>> {
        Some(Value::Number(self))
    }
}
impl<const S: u32> IntoValue<S> for Option<Fixed<S>> {
    fn into_value(self) -> Option<Value<S>> {
        self.map(Value::Number)
    }
}
impl<const S: u32> IntoValue<S> for DateTime {
    fn into_value(self) -> Option<Value<S>> {
        Some(Value::Date(self))
    }
}
impl<const S: u32> IntoValue<S> for Option<DateTime> {
    fn into_value(self) -> Option<Value<S>> {
        self.map(Value::Date)
    }
}
impl<const S: u32> IntoValue<S> for ContractPerformance {
    fn into_value(self) -> Option<Value<S>> {
        Some(Value::Performance(self))
    }
}
impl<const S: u32> IntoValue<S> for bool {
    fn into_value(self) -> Option<Value<S>> {
        Some(Value::Flag(self))
    }
}
