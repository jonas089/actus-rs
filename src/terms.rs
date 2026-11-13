//! Contract terms: parsing helpers, shared enums, and term groups reused by several contracts.

use crate::daycount::{DayCountConvention, DayCounter};
use crate::error::{Error, Result};
use crate::fixed::Fixed;
use crate::time::{
    parse_datetime, BusinessDayAdjuster, BusinessDayConvention, Calendar, Cycle, DateTime, EndOfMonthConvention,
    Period,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};
use std::str::FromStr;

// ---------------------------------------------------------------------------------------------
// Raw terms (ACTUS JSON) access
// ---------------------------------------------------------------------------------------------

/// Contract terms as given in ACTUS JSON (`{"contractType": "PAM", "notionalPrincipal": "1000", ...}`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawTerms(pub Map<String, Json>);

impl RawTerms {
    pub fn from_json(v: &Json) -> Result<Self> {
        match v {
            Json::Object(m) => Ok(RawTerms(m.clone())),
            _ => Err(Error::invalid("terms", "expected a JSON object")),
        }
    }

    pub fn has(&self, key: &str) -> bool {
        self.text(key).is_some() || matches!(self.0.get(key), Some(Json::Array(a)) if !a.is_empty())
    }

    /// Trimmed text value; empty strings and nulls count as absent.
    pub fn text(&self, key: &str) -> Option<String> {
        match self.0.get(key)? {
            Json::String(s) => {
                let t = s.trim();
                if t.is_empty() || t.eq_ignore_ascii_case("null") { None } else { Some(t.to_string()) }
            }
            Json::Number(n) => Some(n.to_string()),
            Json::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }

    pub fn req_text(&self, key: &str) -> Result<String> {
        self.text(key).ok_or_else(|| Error::MissingTerm(key.to_string()))
    }

    pub fn date(&self, key: &str) -> Result<Option<DateTime>> {
        self.text(key).map(|s| parse_datetime(&s).map_err(|_| Error::invalid(key, s))).transpose()
    }

    pub fn req_date(&self, key: &str) -> Result<DateTime> {
        self.date(key)?.ok_or_else(|| Error::MissingTerm(key.to_string()))
    }

    pub fn fixed<const S: u32>(&self, key: &str) -> Result<Option<Fixed<S>>> {
        self.text(key).map(|s| Fixed::parse(&s).map_err(|_| Error::invalid(key, s))).transpose()
    }

    pub fn req_fixed<const S: u32>(&self, key: &str) -> Result<Fixed<S>> {
        self.fixed(key)?.ok_or_else(|| Error::MissingTerm(key.to_string()))
    }

    pub fn fixed_or<const S: u32>(&self, key: &str, default: Fixed<S>) -> Result<Fixed<S>> {
        Ok(self.fixed(key)?.unwrap_or(default))
    }

    pub fn cycle(&self, key: &str) -> Result<Option<Cycle>> {
        self.text(key).map(|s| Cycle::parse(&s)).transpose()
    }

    pub fn period(&self, key: &str) -> Result<Option<Period>> {
        self.text(key).map(|s| Period::parse(&s)).transpose()
    }

    pub fn parse_enum<T: FromStr>(&self, key: &str) -> Result<Option<T>> {
        self.text(key).map(|s| s.parse::<T>().map_err(|_| Error::invalid(key, s))).transpose()
    }

    pub fn enum_or<T: FromStr>(&self, key: &str, default: T) -> Result<T> {
        Ok(self.parse_enum(key)?.unwrap_or(default))
    }

    /// A list value: either a JSON array, or a single value / `"[a, b]"`-style string.
    pub fn list(&self, key: &str) -> Vec<String> {
        match self.0.get(key) {
            Some(Json::Array(a)) => a
                .iter()
                .filter_map(|v| match v {
                    Json::String(s) => Some(s.trim().to_string()),
                    Json::Number(n) => Some(n.to_string()),
                    _ => None,
                })
                .collect(),
            _ => match self.text(key) {
                Some(s) => s
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .split(',')
                    .map(|x| x.trim().trim_matches('\'').trim_matches('"').to_string())
                    .filter(|x| !x.is_empty())
                    .collect(),
                None => Vec::new(),
            },
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Enumerations
// ---------------------------------------------------------------------------------------------

macro_rules! simple_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $v:ident $(= $alias:literal)*),* $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name { $($(#[$vm])* $v),* }
        impl FromStr for $name {
            type Err = ();
            fn from_str(s: &str) -> std::result::Result<Self, ()> {
                match s.trim() {
                    $(stringify!($v) $(| $alias)* => Ok($name::$v),)*
                    _ => Err(()),
                }
            }
        }
    };
}

simple_enum!(
    /// Role of the contract creator (`CNTRL`).
    ContractRole {
        /// Real position asset.
        RPA,
        /// Real position liability.
        RPL,
        /// Receive first (fixed) leg.
        RFL,
        /// Pay first (fixed) leg.
        PFL,
        /// Receive fix.
        RF,
        /// Pay fix.
        PF,
        /// Protection / instrument buyer.
        BUY,
        /// Protection / instrument seller.
        SEL,
        /// Long position.
        LG,
        /// Short position.
        ST,
        /// Collateral instrument.
        COL,
        /// Close-out netting instrument.
        CNO,
        /// Guarantor.
        GUA,
        /// Obligee.
        OBL,
        /// Underlying.
        UDL,
        /// Underlying, positive sign.
        UDLP,
        /// Underlying, negative sign.
        UDLM,
    }
);

impl ContractRole {
    /// Contract role sign `R(CNTRL)`: +1 for claims, -1 for obligations.
    pub fn sign<const S: u32>(self) -> Fixed<S> {
        use ContractRole::*;
        match self {
            RPA | RFL | RF | BUY | LG | COL | CNO | OBL | UDL | UDLP => Fixed::ONE,
            RPL | PFL | PF | SEL | ST | GUA | UDLM => -Fixed::<S>::ONE,
        }
    }
}

simple_enum!(
    /// Contract performance (`PRF`).
    ContractPerformance {
        /// Performant.
        PF,
        /// Delayed.
        DL,
        /// Delinquent.
        DQ,
        /// Default.
        DF,
        /// Matured.
        MA,
        /// Terminated.
        TE,
    }
);

simple_enum!(FeeBasis { A, N });
simple_enum!(
    /// Penalty type for prepayments (`PYTP`).
    PenaltyType {
        /// No penalty.
        O,
        /// Absolute amount.
        A,
        /// Rate applied to notional.
        N,
        /// Interest rate differential.
        I,
    }
);
simple_enum!(PrepaymentEffect { N, A, M });
simple_enum!(InterestCalculationBase { NT, NTIED, NTL });
simple_enum!(CyclePoint { B, E });
simple_enum!(DeliverySettlement { S, D });
simple_enum!(OptionType { C, P, CP });
simple_enum!(OptionExerciseType { E, B, A });
simple_enum!(GuaranteedExposure { NO, NI, MV });
simple_enum!(Seniority { S, J });
simple_enum!(IncreaseDecrease { INC, DEC });
simple_enum!(FixedVariable { F = "FIX", V = "VAR" });

impl FromStr for BusinessDayConvention {
    type Err = ();
    fn from_str(s: &str) -> std::result::Result<Self, ()> {
        use BusinessDayConvention::*;
        Ok(match s.trim() {
            "NOS" | "NULL" => NOS,
            "SCF" => SCF,
            "SCMF" => SCMF,
            "CSF" => CSF,
            "CSMF" => CSMF,
            "SCP" => SCP,
            "SCMP" => SCMP,
            "CSP" => CSP,
            "CSMP" => CSMP,
            _ => return Err(()),
        })
    }
}

impl FromStr for EndOfMonthConvention {
    type Err = ();
    fn from_str(s: &str) -> std::result::Result<Self, ()> {
        match s.trim() {
            "SD" => Ok(EndOfMonthConvention::SD),
            "EOM" => Ok(EndOfMonthConvention::EOM),
            _ => Err(()),
        }
    }
}

/// Scaling effect (`SCEF`): which of interest (`I`) and notional (`N`) are index-scaled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct ScalingEffect {
    pub interest: bool,
    pub notional: bool,
}

impl FromStr for ScalingEffect {
    type Err = ();
    fn from_str(s: &str) -> std::result::Result<Self, ()> {
        let s = s.trim();
        if s.len() != 3 || !s.chars().all(|c| matches!(c, 'I' | 'N' | 'O' | '0')) {
            return Err(());
        }
        Ok(ScalingEffect { interest: s.contains('I'), notional: s.contains('N') })
    }
}

impl ScalingEffect {
    pub fn any(&self) -> bool {
        self.interest || self.notional
    }
}

// ---------------------------------------------------------------------------------------------
// Shared term groups
// ---------------------------------------------------------------------------------------------

/// Terms every contract has.
#[derive(Clone, Debug, PartialEq)]
pub struct CommonTerms {
    pub contract_id: String,
    pub status_date: DateTime,
    pub contract_role: ContractRole,
    pub currency: String,
    pub settlement_currency: Option<String>,
    pub contract_deal_date: Option<DateTime>,
    pub market_object_code: Option<String>,
    pub creator_id: Option<String>,
    pub counterparty_id: Option<String>,
    pub business_day: BusinessDayAdjuster,
    pub end_of_month: EndOfMonthConvention,
    pub contract_performance: ContractPerformance,
}

impl CommonTerms {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        let calendar = match raw.text("calendar").as_deref() {
            Some("MF") | Some("MondayToFriday") => Calendar::MondayToFriday,
            Some("NC") | Some("NoCalendar") | Some("NOCALENDAR") | Some("NoHoliday") | None => Calendar::NoHoliday,
            Some(other) => return Err(Error::invalid("calendar", other)),
        };
        Ok(CommonTerms {
            contract_id: raw.text("contractID").unwrap_or_default(),
            status_date: raw.req_date("statusDate")?,
            contract_role: raw.parse_enum("contractRole")?.ok_or_else(|| Error::MissingTerm("contractRole".into()))?,
            currency: raw.text("currency").unwrap_or_default(),
            settlement_currency: raw.text("settlementCurrency"),
            contract_deal_date: raw.date("contractDealDate")?,
            market_object_code: raw.text("marketObjectCode"),
            creator_id: raw.text("creatorID"),
            counterparty_id: raw.text("counterpartyID"),
            business_day: BusinessDayAdjuster::new(
                raw.enum_or("businessDayConvention", BusinessDayConvention::NOS)?,
                calendar,
            ),
            end_of_month: raw.enum_or("endOfMonthConvention", EndOfMonthConvention::SD)?,
            contract_performance: raw.enum_or("contractPerformance", ContractPerformance::PF)?,
        })
    }

    pub fn role_sign<const S: u32>(&self) -> Fixed<S> {
        self.contract_role.sign()
    }

    pub fn calendar(&self) -> Calendar {
        self.business_day.calendar.clone()
    }
}

/// Parse a day count convention bound to the contract's maturity and calendar.
pub fn parse_day_counter(raw: &RawTerms, maturity: Option<DateTime>, calendar: Calendar) -> Result<Option<DayCounter>> {
    match raw.text("dayCountConvention") {
        None => Ok(None),
        Some(s) => {
            let c = DayCountConvention::parse(&s).ok_or_else(|| Error::invalid("dayCountConvention", &s))?;
            Ok(Some(DayCounter { convention: c, maturity, calendar }))
        }
    }
}

/// Anchor of a cycle: the explicit anchor if given, else `default` when a cycle is given.
pub fn anchor(raw: &RawTerms, anchor_key: &str, cycle_key: &str, default: Option<DateTime>) -> Result<Option<DateTime>> {
    match raw.date(anchor_key)? {
        Some(a) => Ok(Some(a)),
        None if raw.has(cycle_key) => Ok(default),
        None => Ok(None),
    }
}

/// Rate reset terms (`RR*`).
#[derive(Clone, Debug, PartialEq)]
pub struct RateResetTerms<const S: u32> {
    pub anchor: Option<DateTime>,
    pub cycle: Option<Cycle>,
    pub market_object_code: Option<String>,
    pub multiplier: Fixed<S>,
    pub spread: Fixed<S>,
    pub life_cap: Option<Fixed<S>>,
    pub life_floor: Option<Fixed<S>>,
    pub period_cap: Option<Fixed<S>>,
    pub period_floor: Option<Fixed<S>>,
    pub next_reset_rate: Option<Fixed<S>>,
    pub fixing_period: Option<Period>,
}

impl<const S: u32> RateResetTerms<S> {
    pub fn parse(raw: &RawTerms, default_anchor: Option<DateTime>) -> Result<Self> {
        Ok(RateResetTerms {
            anchor: anchor(raw, "cycleAnchorDateOfRateReset", "cycleOfRateReset", default_anchor)?,
            cycle: raw.cycle("cycleOfRateReset")?,
            market_object_code: raw.text("marketObjectCodeOfRateReset"),
            multiplier: raw.fixed_or("rateMultiplier", Fixed::ONE)?,
            spread: raw.fixed_or("rateSpread", Fixed::ZERO)?,
            life_cap: raw.fixed("lifeCap")?,
            life_floor: raw.fixed("lifeFloor")?,
            period_cap: raw.fixed("periodCap")?,
            period_floor: raw.fixed("periodFloor")?,
            next_reset_rate: raw.fixed("nextResetRate")?,
            fixing_period: raw.period("fixingPeriod")?.or(raw.period("fixingDays")?),
        })
    }

    pub fn is_defined(&self) -> bool {
        self.anchor.is_some() || self.cycle.is_some()
    }
}

/// Fee terms (`FE*`).
#[derive(Clone, Debug, PartialEq)]
pub struct FeeTerms<const S: u32> {
    pub anchor: Option<DateTime>,
    pub cycle: Option<Cycle>,
    pub basis: Option<FeeBasis>,
    pub rate: Option<Fixed<S>>,
    pub accrued: Option<Fixed<S>>,
}

impl<const S: u32> FeeTerms<S> {
    pub fn parse(raw: &RawTerms, default_anchor: Option<DateTime>) -> Result<Self> {
        Ok(FeeTerms {
            anchor: anchor(raw, "cycleAnchorDateOfFee", "cycleOfFee", default_anchor)?,
            cycle: raw.cycle("cycleOfFee")?,
            basis: raw.parse_enum("feeBasis")?,
            rate: raw.fixed("feeRate")?,
            accrued: raw.fixed("feeAccrued")?,
        })
    }

    pub fn rate_or_zero(&self) -> Fixed<S> {
        self.rate.unwrap_or(Fixed::ZERO)
    }
}

/// Index scaling terms (`SC*`).
#[derive(Clone, Debug, PartialEq)]
pub struct ScalingTerms<const S: u32> {
    pub anchor: Option<DateTime>,
    pub cycle: Option<Cycle>,
    pub effect: ScalingEffect,
    pub market_object_code: Option<String>,
    pub index_at_contract_deal_date: Option<Fixed<S>>,
    pub notional_multiplier: Fixed<S>,
    pub interest_multiplier: Fixed<S>,
}

impl<const S: u32> ScalingTerms<S> {
    pub fn parse(raw: &RawTerms, default_anchor: Option<DateTime>) -> Result<Self> {
        Ok(ScalingTerms {
            anchor: anchor(raw, "cycleAnchorDateOfScalingIndex", "cycleOfScalingIndex", default_anchor)?,
            cycle: raw.cycle("cycleOfScalingIndex")?,
            effect: raw.enum_or("scalingEffect", ScalingEffect::default())?,
            market_object_code: raw.text("marketObjectCodeOfScalingIndex"),
            index_at_contract_deal_date: raw.fixed("scalingIndexAtContractDealDate")?,
            notional_multiplier: raw.fixed_or("notionalScalingMultiplier", Fixed::ONE)?,
            interest_multiplier: raw.fixed_or("interestScalingMultiplier", Fixed::ONE)?,
        })
    }
}

/// Prepayment and penalty terms (`OP*`, `PP*`, `PY*`).
#[derive(Clone, Debug, PartialEq)]
pub struct PrepaymentTerms<const S: u32> {
    pub effect: PrepaymentEffect,
    pub anchor: Option<DateTime>,
    pub cycle: Option<Cycle>,
    pub penalty_type: PenaltyType,
    pub penalty_rate: Fixed<S>,
}

impl<const S: u32> PrepaymentTerms<S> {
    pub fn parse(raw: &RawTerms, default_anchor: Option<DateTime>) -> Result<Self> {
        Ok(PrepaymentTerms {
            effect: raw.enum_or("prepaymentEffect", PrepaymentEffect::N)?,
            anchor: anchor(raw, "cycleAnchorDateOfOptionality", "cycleOfOptionality", default_anchor)?,
            cycle: raw.cycle("cycleOfOptionality")?,
            penalty_type: raw.enum_or("penaltyType", PenaltyType::O)?,
            penalty_rate: raw.fixed_or("penaltyRate", Fixed::ZERO)?,
        })
    }
}

/// Secondary-market purchase and early termination (`PRD`, `PPRD`, `TD`, `PTD`).
#[derive(Clone, Debug, PartialEq)]
pub struct TradeTerms<const S: u32> {
    pub purchase_date: Option<DateTime>,
    pub price_at_purchase_date: Fixed<S>,
    pub termination_date: Option<DateTime>,
    pub price_at_termination_date: Fixed<S>,
}

impl<const S: u32> TradeTerms<S> {
    pub fn parse(raw: &RawTerms) -> Result<Self> {
        Ok(TradeTerms {
            purchase_date: raw.date("purchaseDate")?,
            price_at_purchase_date: raw.fixed_or("priceAtPurchaseDate", Fixed::ZERO)?,
            termination_date: raw.date("terminationDate")?,
            price_at_termination_date: raw.fixed_or("priceAtTerminationDate", Fixed::ZERO)?,
        })
    }
}
