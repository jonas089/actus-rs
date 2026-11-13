//! The contract model interface and the projection driver.

use crate::error::{Error, Result};
use crate::events::{Event, EventResult};
use crate::fixed::Fixed;
use crate::observer::{Probe, RiskFactorObserver};
use crate::state::StateView;
use crate::terms::CommonTerms;
use crate::time::DateTime;
use std::collections::VecDeque;
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

    /// Lazily evaluate the contract event by event, in order. Works for contracts without a
    /// natural end (STK, UMP, CSH, ...): their schedule is generated window by window, so the
    /// iterator is unbounded and callers stop it (e.g. with `take_while`).
    fn stream<'a>(&'a self, obs: &'a dyn RiskFactorObserver<S>) -> Projection<'a, S, Self>
    where
        Self: Sized,
    {
        Projection::new(self, obs)
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

/// Lazy, in-order evaluation of a contract (see [`ContractModel::stream`]).
pub struct Projection<'a, const S: u32, M: ContractModel<S>> {
    model: &'a M,
    obs: &'a dyn RiskFactorObserver<S>,
    state: Option<M::State>,
    buffer: VecDeque<Event<S>>,
    /// Events of the current schedule window, for `is_reported`.
    window: Vec<Event<S>>,
    last_key: Option<i64>,
    /// `None` once the full schedule is known (bounded contract or schedule exhausted).
    horizon: Option<DateTime>,
    window_years: i64,
    /// Error from generating the first schedule window, reported on the first `next`.
    pending_error: Option<Error>,
    done: bool,
}

/// Unbounded schedules are generated in windows of growing length; after this many years past
/// the status date without any new event, the stream ends.
const MAX_EMPTY_HORIZON_YEARS: i64 = 200;

impl<'a, const S: u32, M: ContractModel<S>> Projection<'a, S, M> {
    fn new(model: &'a M, obs: &'a dyn RiskFactorObserver<S>) -> Self {
        let mut p = Projection {
            model,
            obs,
            state: None,
            buffer: VecDeque::new(),
            window: Vec::new(),
            last_key: None,
            horizon: None,
            window_years: 1,
            pending_error: None,
            done: false,
        };
        match model.schedule(None, obs) {
            Ok(evs) => {
                p.buffer = evs.iter().cloned().collect();
                p.window = evs;
            }
            Err(_) => {
                // No natural end: generate the first window.
                p.horizon = Some(crate::time::Period { years: 1, months: 0, days: 0 }.add_to(model.common().status_date));
                p.pending_error = p.refill_window();
            }
        }
        p
    }

    /// Extend the schedule horizon until new events appear (or give up).
    fn refill_window(&mut self) -> Option<Error> {
        let start = self.model.common().status_date;
        let limit = crate::time::Period { years: MAX_EMPTY_HORIZON_YEARS, months: 0, days: 0 }.add_to(start);
        while let Some(h) = self.horizon {
            match self.model.schedule(Some(h), self.obs) {
                Ok(evs) => {
                    let fresh: Vec<Event<S>> =
                        evs.iter().filter(|e| self.last_key.is_none_or(|k| e.key() > k)).cloned().collect();
                    self.window = evs;
                    let next = crate::time::Period { years: self.window_years, months: 0, days: 0 }.add_to(h);
                    self.window_years = (self.window_years * 2).min(16);
                    self.horizon = if h >= limit && fresh.is_empty() { None } else { Some(next) };
                    if !fresh.is_empty() {
                        self.buffer.extend(fresh);
                        return None;
                    }
                }
                Err(e) => {
                    self.horizon = None;
                    return Some(e);
                }
            }
        }
        None
    }
}

impl<const S: u32, M: ContractModel<S>> Iterator for Projection<'_, S, M> {
    type Item = Result<EventResult<S>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if let Some(e) = self.pending_error.take() {
            self.done = true;
            return Some(Err(e));
        }
        if self.state.is_none() {
            match self.model.initial_state(self.obs).and_then(|s| s.check("initial state").map(|_| s)) {
                Ok(s) => self.state = Some(s),
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            }
        }
        loop {
            if self.buffer.is_empty() {
                if self.horizon.is_none() {
                    self.done = true;
                    return None;
                }
                if let Some(e) = self.refill_window() {
                    self.done = true;
                    return Some(Err(e));
                }
                continue;
            }
            let ev = self.buffer.pop_front().expect("non-empty");
            self.last_key = Some(ev.key());
            let state = self.state.as_ref().expect("initialised");
            let ctx = format!("{} at {}", ev.event_type, ev.time);
            let result = self.model.step(state, &ev, self.obs).and_then(|(next, payoff)| {
                if payoff.is_poisoned() {
                    return Err(Error::Arithmetic { context: format!("{ctx} (payoff)") });
                }
                next.check(&ctx)?;
                Ok((next, payoff))
            });
            match result {
                Ok((next, payoff)) => {
                    let reported = self.model.is_reported(&ev, &self.window);
                    let snapshot = next.snapshot();
                    self.state = Some(next);
                    if reported {
                        return Some(Ok(EventResult { event: ev, payoff, state: snapshot }));
                    }
                }
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            }
        }
    }
}
