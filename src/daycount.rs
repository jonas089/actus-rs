//! Day count (year fraction) conventions, ACTUS `DCC`.

use crate::fixed::Fixed;
use crate::time::{Calendar, DateTime, last_day_of_month};
use chrono::{Datelike, Duration, NaiveDate, Timelike};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DayCountConvention {
    /// Actual/Actual ISDA.
    AA,
    /// Actual/360.
    A360,
    /// Actual/365 fixed.
    A365,
    /// Actual/336.
    A336,
    /// 30E/360 ISDA.
    E30_360ISDA,
    /// 30E/360 (Eurobond basis).
    E30_360,
    /// 28E/336.
    E28_336,
    /// Business/252 (counts business days of the contract calendar).
    B252,
}

impl DayCountConvention {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim() {
            "AA" => Self::AA,
            "A360" => Self::A360,
            "A365" => Self::A365,
            "A336" => Self::A336,
            "30E360ISDA" => Self::E30_360ISDA,
            "30E360" => Self::E30_360,
            "28E336" | "28336" => Self::E28_336,
            "B252" => Self::B252,
            _ => return None,
        })
    }
}

/// A day count convention bound to the contract context it needs
/// (maturity date for 30E/360 ISDA, calendar for Business/252).
#[derive(Clone, Debug, PartialEq)]
pub struct DayCounter {
    pub convention: DayCountConvention,
    pub maturity: Option<DateTime>,
    pub calendar: Calendar,
}

/// Round a timestamp to the nearest full hour (minutes >= 30 round up), so that e.g. a
/// maturity at `23:59:59` counts as the following midnight.
fn truncate_to_hour(t: DateTime) -> DateTime {
    let h = t.date().and_hms_opt(t.hour(), 0, 0).expect("valid time");
    if t.minute() >= 30 { h + Duration::hours(1) } else { h }
}

/// Whole days between two timestamps, truncated toward zero.
pub fn days_between(start: DateTime, end: DateTime) -> i64 {
    (end - start).num_days()
}

fn is_leap(y: i32) -> bool {
    NaiveDate::from_ymd_opt(y, 2, 29).is_some()
}

fn jan1(y: i32) -> DateTime {
    NaiveDate::from_ymd_opt(y, 1, 1).expect("valid").and_hms_opt(0, 0, 0).expect("valid")
}

impl DayCounter {
    pub fn new(convention: DayCountConvention) -> Self {
        DayCounter { convention, maturity: None, calendar: Calendar::NoHoliday }
    }

    /// Fraction of a year between `start` and `end` (`Y(s, t)` in the spec).
    pub fn year_fraction<const S: u32>(&self, start: DateTime, end: DateTime) -> Fixed<S> {
        let s = truncate_to_hour(start);
        let e = truncate_to_hour(end);
        let r = |num: i64, den: i64| Fixed::<S>::ratio(num, den);
        match self.convention {
            DayCountConvention::A360 => r(days_between(s, e), 360),
            DayCountConvention::A365 => r(days_between(s, e), 365),
            DayCountConvention::A336 => r(days_between(s, e), 336),
            DayCountConvention::AA => {
                let (y1, y2) = (s.year(), e.year());
                let basis = |y: i32| if is_leap(y) { 366 } else { 365 };
                if y1 == y2 {
                    r(days_between(s, e), basis(y1))
                } else {
                    r(days_between(s, jan1(y1 + 1)), basis(y1))
                        + r(days_between(jan1(y2), e), basis(y2))
                        + Fixed::from_int((y2 - y1 - 1) as i64)
                }
            }
            DayCountConvention::E30_360 => {
                let d1 = if s.day() == 31 { 30 } else { s.day() as i64 };
                let d2 = if e.day() == 31 { 30 } else { e.day() as i64 };
                r(thirty_days(s, e, d1, d2, 30), 360)
            }
            DayCountConvention::E30_360ISDA => {
                let d1 = if s.date() == last_day_of_month(s).date() { 30 } else { s.day() as i64 };
                let is_feb_maturity = Some(e) == self.maturity.map(truncate_to_hour) && e.month() == 2;
                let d2 = if !is_feb_maturity && e.date() == last_day_of_month(e).date() {
                    30
                } else {
                    e.day() as i64
                };
                r(thirty_days(s, e, d1, d2, 30), 360)
            }
            DayCountConvention::E28_336 => {
                let d1 = (s.day() as i64).min(28);
                let d2 = (e.day() as i64).min(28);
                r(thirty_days(s, e, d1, d2, 28), 336)
            }
            DayCountConvention::B252 => {
                let mut n = 0i64;
                let mut t = s;
                while t < e {
                    if self.calendar.is_business_day(t) {
                        n += 1;
                    }
                    t += Duration::days(1);
                }
                r(n, 252)
            }
        }
    }
}

fn thirty_days(s: DateTime, e: DateTime, d1: i64, d2: i64, month_days: i64) -> i64 {
    let dy = (e.year() - s.year()) as i64;
    let dm = e.month() as i64 - s.month() as i64;
    month_days * 12 * dy + month_days * dm + (d2 - d1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::parse_datetime as d;

    #[test]
    fn actual_actual_across_years() {
        let dc = DayCounter::new(DayCountConvention::AA);
        let f: Fixed<18> = dc.year_fraction(d("2015-07-01").unwrap(), d("2016-07-01").unwrap());
        // 184/365 + 182/366
        let expect = Fixed::<18>::ratio(184, 365) + Fixed::<18>::ratio(182, 366);
        assert_eq!(f, expect);
    }

    #[test]
    fn thirty_e_360() {
        let dc = DayCounter::new(DayCountConvention::E30_360);
        let f: Fixed<18> = dc.year_fraction(d("2013-01-31").unwrap(), d("2013-02-28").unwrap());
        assert_eq!(f, Fixed::ratio(28, 360));
    }
}
