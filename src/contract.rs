//! The contract model interface and the projection driver.

use crate::error::{Error, Result};
use crate::events::{Event, EventResult};
use crate::fixed::Fixed;
use crate::observer::{Probe, RiskFactorObserver};
use crate::state::StateView;
use crate::terms::CommonTerms;
use crate::time::DateTime;
use std::fmt::Debug;

/// A contract type's behaviour. Implemented by each typed terms struct (e.g. `PamTerms`).
///
/// The core is [`step`](ContractModel::step): a pure function from (terms, pre-event state,
/// event, observations) to (post-event state, payoff). Everything else is built on top.
pub trait ContractModel<const S: u32> {
    type State: StateView<S> + Clone + Debug;

    fn common(&self) -> &CommonTerms;

    /// State at the status date `t0`.
    fn initial_state(&self, obs: &dyn RiskFactorObserver<S>) -> Result<Self::State>;

    /// All events from the status date up to `to` (inclusive), sorted. Includes unscheduled
    /// events supplied by the observer. `to = None` means the contract's natural end; contracts
    /// without one (e.g. STK, UMP) require `to`.
    fn schedule(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>>;

    /// Apply one event: returns the post-event state and the payoff.
    fn step(
        &self,
        state: &Self::State,
        event: &Event<S>,
        obs: &dyn RiskFactorObserver<S>,
    ) -> Result<(Self::State, Fixed<S>)>;

    /// Whether an evaluated event is part of the reported result. Events before a purchase date
    /// are evaluated (they shape the state) but are not cash flows of the buyer.
    fn is_reported(&self, _event: &Event<S>, _all: &[Event<S>]) -> bool {
        true
    }

    /// Risk factor observations `step` will read for `event`, as `(id, time)` pairs.
    fn required_inputs(&self, state: &Self::State, event: &Event<S>) -> Vec<(String, DateTime)> {
        let probe = Probe::default();
        let _ = self.step(state, event, &probe);
        probe.requested.into_inner().into_iter().collect()
    }

    /// Run the contract: generate the schedule and apply every event in order.
    fn project(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<EventResult<S>>> {
        let events = self.schedule(to, obs)?;
        let mut state = self.initial_state(obs)?;
        state.check("initial state")?;
        let mut out = Vec::with_capacity(events.len());
        for ev in &events {
            let (next, payoff) = self.step(&state, ev, obs)?;
            let ctx = format!("{} at {}", ev.event_type, ev.time);
            if payoff.is_poisoned() {
                return Err(Error::Arithmetic { context: format!("{ctx} (payoff)") });
            }
            next.check(&ctx)?;
            if self.is_reported(ev, &events) {
                out.push(EventResult { event: ev.clone(), payoff, state: next.snapshot() });
            }
            state = next;
        }
        Ok(out)
    }
}

/// Settlement currency FX rate `X_CUR^CURS(t)`: 1 unless a different settlement currency is set,
/// in which case the risk factor `"CUR/CURS"` is observed.
pub fn settlement_fx<const S: u32>(
    common: &CommonTerms,
    t: DateTime,
    obs: &dyn RiskFactorObserver<S>,
) -> Result<Fixed<S>> {
    match &common.settlement_currency {
        Some(cs) if *cs != common.currency => obs.risk_factor(&format!("{}/{}", common.currency, cs), t),
        _ => Ok(Fixed::ONE),
    }
}
