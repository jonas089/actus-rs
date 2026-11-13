//! Contract events.

use crate::fixed::Fixed;
use crate::state::StateSnapshot;
use crate::terms::ContractPerformance;
use crate::time::DateTime;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// ACTUS event types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EventType {
    /// Monitoring: accrues states, no cash flow.
    AD,
    /// Initial exchange of principal.
    IED,
    /// Fee payment.
    FP,
    /// Principal redemption.
    PR,
    /// Principal drawing.
    PD,
    /// Principal payment amount fixing.
    PRF,
    /// Penalty payment.
    PY,
    /// Principal prepayment (unscheduled).
    PP,
    /// Interest payment.
    IP,
    /// Interest capitalization.
    IPCI,
    /// Credit event (unscheduled).
    CE,
    /// Rate reset fixing with a known rate.
    RRF,
    /// Rate reset with a rate observed from the market.
    RR,
    /// Dividend payment.
    DV,
    /// Purchase.
    PRD,
    /// Margin call.
    MR,
    /// Termination.
    TD,
    /// Scaling index fixing.
    SC,
    /// Interest calculation base fixing.
    IPCB,
    /// Maturity.
    MD,
    /// Exercise.
    XD,
    /// Settlement.
    STD,
    /// Principal increase.
    PI,
    /// Interest payment, fixed leg (SWPPV).
    IPFX,
    /// Interest payment, floating leg (SWPPV).
    IPFL,
}

impl EventType {
    /// Tie-break order for events at the same instant. Values are offsets in seconds added to the
    /// event time before sorting (as in the reference implementation the official tests come
    /// from), so a TD at 23:59:59 can sort after an event at 00:00 of the following day.
    pub fn sequence(self) -> i64 {
        use EventType::*;
        match self {
            IED => 20,
            PR => 30,
            IP | IPFX | IPCI => 40,
            IPFL => 45,
            FP => 60,
            DV => 70,
            MR => 80,
            RRF | RR => 100,
            PRF => 105,
            SC => 110,
            IPCB => 120,
            PRD => 130,
            TD => 140,
            MD => 150,
            XD => 160,
            STD => 170,
            AD => 950,
            PD | PY | PP | CE | PI => 0,
        }
    }
}

impl FromStr for EventType {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        use EventType::*;
        Ok(match s.trim() {
            "AD" => AD,
            "IED" => IED,
            "FP" => FP,
            "PR" => PR,
            "PD" => PD,
            "PRF" => PRF,
            "PY" => PY,
            "PP" => PP,
            "IP" => IP,
            "IPCI" => IPCI,
            "CE" => CE,
            "RRF" => RRF,
            "RR" => RR,
            "DV" => DV,
            "PRD" => PRD,
            "MR" => MR,
            "TD" => TD,
            "SC" => SC,
            "IPCB" => IPCB,
            "MD" => MD,
            "XD" => XD,
            "STD" => STD,
            "PI" => PI,
            "IPFX" => IPFX,
            "IPFL" => IPFL,
            _ => return Err(()),
        })
    }
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

/// A scheduled or unscheduled contract event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event<const S: u32> {
    pub event_type: EventType,
    /// When the event takes place (after business-day shifting).
    pub time: DateTime,
    /// The unshifted schedule time; calculations use this (shifted per the BDC rules).
    pub schedule_time: DateTime,
    pub currency: String,
    pub contract_id: String,
    /// Value carried by an unscheduled event (e.g. a prepayment amount); zero otherwise.
    #[serde(default)]
    pub value: Fixed<S>,
    /// Contract performance carried by an unscheduled credit event.
    #[serde(default)]
    pub performance: Option<ContractPerformance>,
    /// For events of combined contracts (SWAPS, CAPFL): the child events this event applies,
    /// as `(child index, child event)`. Empty for all other events.
    #[serde(default)]
    pub components: Vec<(u8, Event<S>)>,
}

impl<const S: u32> Event<S> {
    pub fn new(event_type: EventType, schedule_time: DateTime, time: DateTime, currency: &str, contract_id: &str) -> Self {
        Event {
            event_type,
            time,
            schedule_time,
            currency: currency.to_string(),
            contract_id: contract_id.to_string(),
            value: Fixed::ZERO,
            performance: None,
            components: Vec::new(),
        }
    }

    /// Event without business-day shifting.
    pub fn at(event_type: EventType, time: DateTime, currency: &str, contract_id: &str) -> Self {
        Self::new(event_type, time, time, currency, contract_id)
    }

    /// Ordering key: time in seconds plus the event type's sequence offset.
    pub fn key(&self) -> i64 {
        sort_key(self.time, self.event_type)
    }
}

pub fn sort_key(time: DateTime, event_type: EventType) -> i64 {
    time.and_utc().timestamp() + event_type.sequence()
}

/// Sort events by time and event sequence (stable for identical keys).
pub fn sort_events<const S: u32>(events: &mut [Event<S>]) {
    events.sort_by_key(|e| e.key());
}

/// The outcome of applying one event: the cash flow and the post-event state.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EventResult<const S: u32> {
    pub event: Event<S>,
    /// Cash flow from the perspective of the contract's creator (positive = received).
    pub payoff: Fixed<S>,
    pub state: StateSnapshot<S>,
}
