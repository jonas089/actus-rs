//! Time: cycles, schedules, end-of-month and business-day conventions, calendars.
//!
//! ACTUS timestamps carry no time zone; we use [`chrono::NaiveDateTime`].

use crate::error::{Error, Result};
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Weekday};
use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

pub type DateTime = NaiveDateTime;

/// Parse an ACTUS timestamp (`2013-01-01T00:00:00`, `2013-01-01T00:00` or `2013-01-01`).
pub fn parse_datetime(s: &str) -> Result<DateTime> {
    let s = s.trim();
    for fmt in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%dT%H:%M:%S%.f"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Ok(dt);
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(d.and_hms_opt(0, 0, 0).expect("midnight is valid"));
    }
    Err(Error::InvalidTerm { term: "timestamp".into(), value: s.into() })
}

/// Last calendar day of the month `dt` falls in.
pub fn last_day_of_month(dt: DateTime) -> DateTime {
    let (y, m) = (dt.year(), dt.month());
    let first_next = if m == 12 {
        NaiveDate::from_ymd_opt(y + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(y, m + 1, 1)
    }
    .expect("valid date");
    (first_next - Duration::days(1)).and_time(dt.time())
}

pub fn is_last_day_of_month(dt: DateTime) -> bool {
    last_day_of_month(dt).date() == dt.date()
}

/// Add calendar months, clamping the day to the end of the target month (Feb 31 -> Feb 28/29).
pub fn add_months(dt: DateTime, months: i64) -> DateTime {
    let total = dt.year() as i64 * 12 + (dt.month0() as i64) + months;
    let y = total.div_euclid(12) as i32;
    let m = total.rem_euclid(12) as u32 + 1;
    let mut d = dt.day();
    loop {
        if let Some(date) = NaiveDate::from_ymd_opt(y, m, d) {
            return date.and_time(dt.time());
        }
        d -= 1;
    }
}

/// A calendar period (years/months/days), as in ISO 8601 durations used by ACTUS cycles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Period {
    pub years: i64,
    pub months: i64,
    pub days: i64,
}

impl Period {
    pub const ZERO: Period = Period { years: 0, months: 0, days: 0 };

    pub fn is_zero(&self) -> bool {
        *self == Self::ZERO
    }

    /// Parse an ISO 8601 period like `P1M`, `P3M`, `P1Y`, `P2W`, `P15D`, `P0D`, `P1Y6M`.
    /// ACTUS extensions `Q` (quarter) and `H` (half year) are accepted.
    pub fn parse(s: &str) -> Result<Period> {
        let s = s.trim();
        let bad = || Error::InvalidTerm { term: "period".into(), value: s.into() };
        let body = s.strip_prefix('P').ok_or_else(bad)?;
        if body.is_empty() {
            return Err(bad());
        }
        let mut p = Period::ZERO;
        let mut num = String::new();
        for c in body.chars() {
            if c.is_ascii_digit() || (c == '-' && num.is_empty()) {
                num.push(c);
                continue;
            }
            let n: i64 = num.parse().map_err(|_| bad())?;
            num.clear();
            match c {
                'Y' => p.years += n,
                'H' => p.months += 6 * n,
                'Q' => p.months += 3 * n,
                'M' => p.months += n,
                'W' => p.days += 7 * n,
                'D' => p.days += n,
                _ => return Err(bad()),
            }
        }
        if !num.is_empty() {
            return Err(bad());
        }
        Ok(p)
    }

    pub fn times(&self, k: i64) -> Period {
        Period { years: self.years * k, months: self.months * k, days: self.days * k }
    }

    /// `dt + self`: months (incl. years) first with end-of-month clamping, then days.
    pub fn add_to(&self, dt: DateTime) -> DateTime {
        let months = self.years * 12 + self.months;
        let mut t = if months != 0 { add_months(dt, months) } else { dt };
        if self.days != 0 {
            t += Duration::days(self.days);
        }
        t
    }

    pub fn subtract_from(&self, dt: DateTime) -> DateTime {
        self.times(-1).add_to(dt)
    }
}

impl fmt::Display for Period {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "P")?;
        if self.years != 0 {
            write!(f, "{}Y", self.years)?;
        }
        if self.months != 0 {
            write!(f, "{}M", self.months)?;
        }
        if self.days != 0 || self.is_zero() {
            write!(f, "{}D", self.days)?;
        }
        Ok(())
    }
}

/// How a schedule ends when the cycle does not divide the schedule period evenly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stub {
    /// `L0`: the last regular period is merged with the remainder (one long last period).
    Long,
    /// `L1`: the remainder forms its own short last period.
    Short,
}

/// An ACTUS cycle, e.g. `P1ML0` = every month, long stub.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cycle {
    pub period: Period,
    pub stub: Stub,
}

impl Cycle {
    pub fn parse(s: &str) -> Result<Cycle> {
        let s = s.trim();
        let bad = || Error::InvalidTerm { term: "cycle".into(), value: s.into() };
        let (p, stub) = match s.rsplit_once('L') {
            Some((p, st)) => {
                let stub = match st {
                    "0" => Stub::Long,
                    "1" => Stub::Short,
                    _ => return Err(bad()),
                };
                (p, stub)
            }
            None => (s, Stub::Short),
        };
        let period = Period::parse(p)?;
        if period.is_zero() {
            return Err(bad());
        }
        Ok(Cycle { period, stub })
    }
}

impl fmt::Display for Cycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}L{}", self.period, if self.stub == Stub::Long { 0 } else { 1 })
    }
}

/// End-of-month convention.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum EndOfMonthConvention {
    /// Same day: schedule dates keep the anchor's day of month (clamped in short months).
    #[default]
    SD,
    /// End of month: if the anchor is a month end, all schedule dates fall on month ends.
    EOM,
}

/// Generate a cyclic schedule from `start` to `end` (ACTUS schedule function `S(s, c, T)`).
///
/// * no cycle: `{start, end}` (or just `{start}` when `include_end` is false)
/// * otherwise `start, start+c, start+2c, ... < end`, plus `end` if `include_end`.
///
/// With a long stub (`L0`), the last regular date before `end` is dropped when the cycle does
/// not land exactly on `end`, so the final period is longer than one cycle.
///
/// End-of-month adjustment applies only when the anchor is the last day of a month and the
/// cycle has a non-zero *months* component (a pure-year cycle such as `P1Y` is not adjusted;
/// this mirrors the reference implementation the official tests were generated with).
pub fn schedule(
    start: Option<DateTime>,
    end: DateTime,
    cycle: Option<Cycle>,
    eomc: EndOfMonthConvention,
    include_end: bool,
) -> Vec<DateTime> {
    let mut set = BTreeSet::new();
    let (Some(cycle), Some(start)) = (cycle, start) else {
        if let Some(s) = start {
            set.insert(s);
        }
        if include_end {
            set.insert(end);
        } else if start == Some(end) {
            set.remove(&end);
        }
        return set.into_iter().collect();
    };
    let eom = eomc == EndOfMonthConvention::EOM && is_last_day_of_month(start) && cycle.period.months > 0;
    let shift = |t: DateTime| if eom { last_day_of_month(t) } else { t };

    let mut t = start;
    let mut counter = 1;
    while t < end {
        set.insert(t);
        t = shift(cycle.period.times(counter).add_to(start));
        counter += 1;
    }
    if include_end {
        set.insert(end);
    } else if end == start {
        set.remove(&start);
    }
    if cycle.stub == Stub::Long && set.len() > 2 && end != t {
        set.remove(&shift(cycle.period.times(counter - 2).add_to(start)));
    }
    set.into_iter().collect()
}

/// Array schedule: one sub-schedule per (anchor, cycle) pair, each running to the next anchor.
pub fn array_schedule(
    anchors: &[DateTime],
    cycles: &[Option<Cycle>],
    end: DateTime,
    eomc: EndOfMonthConvention,
) -> Vec<DateTime> {
    let mut set = BTreeSet::new();
    for i in 0..anchors.len() {
        let seg_end = if i + 1 < anchors.len() { anchors[i + 1] } else { end };
        let cycle = cycles.get(i).copied().flatten();
        set.extend(schedule(Some(anchors[i]), seg_end, cycle, eomc, true));
    }
    set.into_iter().collect()
}

/// Determines whether a day is a business day.
pub trait BusinessDayCalendar: Send + Sync + fmt::Debug {
    fn is_business_day(&self, date: NaiveDate) -> bool;
}

/// Business-day calendar referenced by contract terms.
#[derive(Clone, Debug, Default)]
pub enum Calendar {
    /// Every day is a business day (ACTUS `NC`, the default).
    #[default]
    NoHoliday,
    /// Monday to Friday are business days (ACTUS `MF`).
    MondayToFriday,
    /// A user-supplied holiday calendar.
    Custom(Arc<dyn BusinessDayCalendar>),
}

impl PartialEq for Calendar {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Calendar::NoHoliday, Calendar::NoHoliday) => true,
            (Calendar::MondayToFriday, Calendar::MondayToFriday) => true,
            (Calendar::Custom(a), Calendar::Custom(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl Calendar {
    pub fn is_business_day(&self, dt: DateTime) -> bool {
        match self {
            Calendar::NoHoliday => true,
            Calendar::MondayToFriday => !matches!(dt.weekday(), Weekday::Sat | Weekday::Sun),
            Calendar::Custom(c) => c.is_business_day(dt.date()),
        }
    }
}

/// Business-day convention (ACTUS `BDC`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum BusinessDayConvention {
    /// No shift.
    #[default]
    NOS,
    SCF,
    SCMF,
    CSF,
    CSMF,
    SCP,
    SCMP,
    CSP,
    CSMP,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ShiftRule {
    None,
    Following,
    ModifiedFollowing,
    Preceding,
    ModifiedPreceding,
}

impl BusinessDayConvention {
    fn rule(self) -> ShiftRule {
        use BusinessDayConvention::*;
        match self {
            NOS => ShiftRule::None,
            SCF | CSF => ShiftRule::Following,
            SCMF | CSMF => ShiftRule::ModifiedFollowing,
            SCP | CSP => ShiftRule::Preceding,
            SCMP | CSMP => ShiftRule::ModifiedPreceding,
        }
    }

    /// `CS*` conventions calculate on the unshifted date and only shift the payment.
    fn calculate_before_shift(self) -> bool {
        use BusinessDayConvention::*;
        matches!(self, CSF | CSMF | CSP | CSMP)
    }
}

/// Applies a business-day convention with a calendar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BusinessDayAdjuster {
    pub convention: BusinessDayConvention,
    pub calendar: Calendar,
}

impl BusinessDayAdjuster {
    pub fn new(convention: BusinessDayConvention, calendar: Calendar) -> Self {
        BusinessDayAdjuster { convention, calendar }
    }

    fn step_until_business(&self, t: DateTime, days: i64) -> DateTime {
        let mut s = t;
        let mut guard = 0;
        while !self.calendar.is_business_day(s) && guard < 3660 {
            s += Duration::days(days);
            guard += 1;
        }
        s
    }

    /// The date on which a scheduled event actually takes place (payment date).
    pub fn shift_event_time(&self, t: DateTime) -> DateTime {
        match self.convention.rule() {
            ShiftRule::None => t,
            ShiftRule::Following => self.step_until_business(t, 1),
            ShiftRule::Preceding => self.step_until_business(t, -1),
            ShiftRule::ModifiedFollowing => {
                let s = self.step_until_business(t, 1);
                if s.month() != t.month() { self.step_until_business(t, -1) } else { s }
            }
            ShiftRule::ModifiedPreceding => {
                let s = self.step_until_business(t, -1);
                if s.month() != t.month() { self.step_until_business(t, 1) } else { s }
            }
        }
    }

    /// The date used for calculations (accrual) for a scheduled time.
    pub fn shift_calc_time(&self, t: DateTime) -> DateTime {
        if self.convention.calculate_before_shift() { t } else { self.shift_event_time(t) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> DateTime {
        parse_datetime(s).unwrap()
    }

    #[test]
    fn cycle_parse() {
        let c = Cycle::parse("P1ML0").unwrap();
        assert_eq!(c.period.months, 1);
        assert_eq!(c.stub, Stub::Long);
        assert_eq!(Cycle::parse("P2WL1").unwrap().period.days, 14);
    }

    #[test]
    fn monthly_schedule_with_eom() {
        let s = schedule(
            Some(d("2013-01-31")),
            d("2013-05-31"),
            Some(Cycle::parse("P1ML1").unwrap()),
            EndOfMonthConvention::EOM,
            true,
        );
        let days: Vec<u32> = s.iter().map(|t| t.day()).collect();
        assert_eq!(days, vec![31, 28, 31, 30, 31]);
    }

    #[test]
    fn long_stub_drops_last_regular_date() {
        let c = Cycle::parse("P1ML0").unwrap();
        let s = schedule(Some(d("2013-01-01")), d("2013-03-15"), Some(c), EndOfMonthConvention::SD, true);
        assert_eq!(s, vec![d("2013-01-01"), d("2013-02-01"), d("2013-03-15")]);
        let c = Cycle::parse("P1ML1").unwrap();
        let s = schedule(Some(d("2013-01-01")), d("2013-03-15"), Some(c), EndOfMonthConvention::SD, true);
        assert_eq!(s.len(), 4);
    }

    #[test]
    fn modified_following() {
        let adj = BusinessDayAdjuster::new(BusinessDayConvention::SCMF, Calendar::MondayToFriday);
        // Saturday 2013-08-31 -> following would be September, so go back to Friday 30th.
        assert_eq!(adj.shift_event_time(d("2013-08-31")), d("2013-08-30"));
        assert_eq!(adj.shift_event_time(d("2013-06-01")), d("2013-06-03"));
    }
}
