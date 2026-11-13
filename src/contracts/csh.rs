//! CSH — Cash (a cash position; only monitoring events).

use crate::contract::ContractModel;
use crate::error::{Error, Result};
use crate::events::{sort_events, Event, EventType};
use crate::fixed::Fixed;
use crate::impl_state_view;
use crate::observer::RiskFactorObserver;
use crate::terms::{CommonTerms, RawTerms};
use crate::time::DateTime;

#[derive(Clone, Debug, PartialEq)]
pub struct CshTerms<const S: u32> {
    pub common: CommonTerms,
    pub notional_principal: Fixed<S>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CshState<const S: u32> {
    pub status_date: DateTime,
    pub notional_principal: Fixed<S>,
}

impl_state_view!(CshState {
    status_date => StatusDate,
    notional_principal => NotionalPrincipal,
});

impl<const S: u32> CshTerms<S> {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        Ok(CshTerms { common: CommonTerms::parse(raw)?, notional_principal: raw.req_fixed("notionalPrincipal")? })
    }
}

impl<const S: u32> ContractModel<S> for CshTerms<S> {
    type State = CshState<S>;

    fn common(&self) -> &CommonTerms {
        &self.common
    }

    fn initial_state(&self, _obs: &dyn RiskFactorObserver<S>) -> Result<CshState<S>> {
        Ok(CshState {
            status_date: self.common.status_date,
            notional_principal: self.common.role_sign() * self.notional_principal,
        })
    }

    /// Cash has no scheduled events; only observed monitoring (AD) events are evaluated.
    fn schedule(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<Event<S>>> {
        let (cur, cid) = (&self.common.currency, &self.common.contract_id);
        let mut evs: Vec<Event<S>> = obs
            .events(cid)
            .into_iter()
            .filter(|o| o.event_type == EventType::AD)
            .map(|o| Event::at(EventType::AD, o.time, cur, cid))
            .collect();
        super::loan::clip_events(&mut evs, self.common.status_date, to);
        sort_events(&mut evs);
        Ok(evs)
    }

    fn step(&self, s0: &CshState<S>, ev: &Event<S>, _obs: &dyn RiskFactorObserver<S>) -> Result<(CshState<S>, Fixed<S>)> {
        match ev.event_type {
            EventType::AD => Ok((CshState { status_date: ev.schedule_time, ..s0.clone() }, Fixed::ZERO)),
            other => Err(Error::Unsupported(format!("event {other} for CSH"))),
        }
    }
}
