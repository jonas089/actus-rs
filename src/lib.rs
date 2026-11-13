//! # actus-rs
//!
//! A deterministic state-transition engine for financial contracts following the
//! [ACTUS](https://www.actusfrf.org) standard.
//!
//! * [`fixed`]: fixed-point decimal arithmetic (default 18 decimal places, half-even rounding).
//! * [`time`] / [`daycount`]: schedules, business-day and day-count conventions.
//! * [`contract::ContractModel`]: the per-contract-type interface (`schedule`, `step`, `project`).
//! * [`Contract`]: any supported contract, parsed from ACTUS JSON terms.

pub mod conformance;
pub mod contract;
pub mod contracts;
pub mod daycount;
pub mod error;
pub mod events;
pub mod fixed;
pub mod observer;
pub mod state;
pub mod terms;
pub mod time;

pub use contract::ContractModel;
pub use error::{Error, Result};
pub use events::{Event, EventResult, EventType};
pub use fixed::{Decimal, Fixed};
pub use observer::{MarketData, ObservedEvent, RiskFactorObserver};
pub use time::DateTime;

use contracts::lam::{AmortizerKind, AmortizerTerms};
use contracts::lax::LaxTerms;
use contracts::pam::PamTerms;
use terms::RawTerms;

/// Any supported ACTUS contract.
#[derive(Clone, Debug, PartialEq)]
pub enum Contract<const S: u32 = { fixed::DEFAULT_SCALE }> {
    Pam(PamTerms<S>),
    Lam(AmortizerTerms<S>),
    Lax(LaxTerms<S>),
    Nam(AmortizerTerms<S>),
    Ann(AmortizerTerms<S>),
}

macro_rules! dispatch {
    ($self:ident, $c:ident => $e:expr) => {
        match $self {
            Contract::Pam($c) => $e,
            Contract::Lam($c) => $e,
            Contract::Lax($c) => $e,
            Contract::Nam($c) => $e,
            Contract::Ann($c) => $e,
        }
    };
}

impl<const S: u32> Contract<S> {
    /// Parse ACTUS JSON terms (`{"contractType": "PAM", ...}`).
    pub fn from_json(v: &serde_json::Value) -> Result<Self> {
        let raw = RawTerms::from_json(v)?;
        Self::from_raw(&raw)
    }

    pub fn from_raw(raw: &RawTerms) -> Result<Self> {
        let ct = raw.req_text("contractType")?;
        Ok(match ct.as_str() {
            "PAM" => Contract::Pam(PamTerms::parse(raw)?),
            "LAM" => Contract::Lam(AmortizerTerms::parse(raw, AmortizerKind::Lam)?),
            "LAX" => Contract::Lax(LaxTerms::parse(raw)?),
            "NAM" => Contract::Nam(AmortizerTerms::parse(raw, AmortizerKind::Nam)?),
            "ANN" => Contract::Ann(AmortizerTerms::parse(raw, AmortizerKind::Ann)?),
            other => return Err(Error::UnknownContractType(other.to_string())),
        })
    }

    pub fn contract_id(&self) -> &str {
        dispatch!(self, c => &c.common().contract_id)
    }

    /// Run the contract up to `to` (or its natural end) against the given observer.
    pub fn project(&self, to: Option<DateTime>, obs: &dyn RiskFactorObserver<S>) -> Result<Vec<EventResult<S>>> {
        dispatch!(self, c => c.project(to, obs))
    }

    pub fn common(&self) -> &terms::CommonTerms {
        dispatch!(self, c => c.common())
    }

    /// State of the contract at time `t` (after all events up to and including `t`),
    /// as used by parent contracts observing a child (`U^sv` in the spec).
    pub fn state_at(&self, t: DateTime, obs: &dyn RiskFactorObserver<S>) -> Result<Option<state::StateSnapshot<S>>> {
        let res = self.project(Some(t), obs)?;
        Ok(res.last().map(|r| r.state.clone()))
    }
}

/// Role of a referenced contract within a combined contract (`CTST` reference role).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ReferenceRole {
    /// Underlying.
    UDL,
    /// First leg.
    FIL,
    /// Second leg.
    SEL,
    /// Covered contract (credit enhancements).
    COVE,
    /// Covering contract (credit enhancements).
    COVI,
}

impl std::str::FromStr for ReferenceRole {
    type Err = ();
    fn from_str(s: &str) -> std::result::Result<Self, ()> {
        Ok(match s.trim() {
            "UDL" => ReferenceRole::UDL,
            "FIL" => ReferenceRole::FIL,
            "SEL" => ReferenceRole::SEL,
            "COVE" => ReferenceRole::COVE,
            "COVI" => ReferenceRole::COVI,
            _ => return Err(()),
        })
    }
}

/// What a contract reference points to.
#[derive(Clone, Debug, PartialEq)]
pub enum ReferenceObject {
    /// An embedded child contract, kept as raw terms so the parent can adjust them
    /// (e.g. set leg roles) before parsing with [`Contract::from_raw`].
    Contract(RawTerms),
    /// A market object (price series) observed through the risk factor observer.
    MarketObject(String),
    /// Another contract identified by its contract ID (observed, not embedded).
    ContractId(String),
}

/// One entry of the `contractStructure` term.
#[derive(Clone, Debug, PartialEq)]
pub struct ContractReference {
    pub role: ReferenceRole,
    pub object: ReferenceObject,
}

/// Parse the `contractStructure` term.
pub fn parse_contract_structure(raw: &RawTerms) -> Result<Vec<ContractReference>> {
    let Some(serde_json::Value::Array(items)) = raw.0.get("contractStructure") else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for item in items {
        let role_s = item.get("referenceRole").and_then(|v| v.as_str()).unwrap_or("");
        let role = role_s.parse().map_err(|_| Error::invalid("referenceRole", role_s))?;
        let ty = item.get("referenceType").and_then(|v| v.as_str()).unwrap_or("CNT");
        let obj = item.get("object").cloned().unwrap_or(serde_json::Value::Null);
        let text = |k: &str| obj.get(k).and_then(|v| v.as_str()).map(|s| s.trim().to_string());
        let object = match ty {
            "CNT" => ReferenceObject::Contract(RawTerms::from_json(&obj)?),
            "MOC" => ReferenceObject::MarketObject(
                text("marketObjectCode").ok_or_else(|| Error::invalid("contractStructure", "MOC without code"))?,
            ),
            "CID" => ReferenceObject::ContractId(
                text("contractIdentifier")
                    .or_else(|| text("contractID"))
                    .ok_or_else(|| Error::invalid("contractStructure", "CID without identifier"))?,
            ),
            other => return Err(Error::invalid("referenceType", other)),
        };
        out.push(ContractReference { role, object });
    }
    Ok(out)
}
