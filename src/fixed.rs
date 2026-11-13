//! Deterministic fixed-point decimal arithmetic.
//!
//! `Fixed<S>` stores a value as an `i128` scaled by `10^S` (S = number of decimal places,
//! default 18). Multiplication and division are computed in 256-bit intermediates and rounded
//! back half-even (banker's rounding).
//!
//! Every operation rounds its result to `S` decimals, so precision is absolute, not relative:
//! in `rate * year_fraction * notional` the first product is rounded before scaling by the
//! notional, giving an error of about `notional * 10^-S` (1e-12 on 1,000,000 at 18 dp).
//! Results are bit-for-bit deterministic on every platform.
//!
//! Overflow and division by zero never panic and never wrap: they produce a *poisoned* value
//! (similar to a NaN) that propagates through all further arithmetic. The engine checks every
//! payoff and state it produces and turns poison into [`crate::Error::Arithmetic`]. This keeps
//! the financial formulas readable (`a * b + c`) while still guaranteeing that an arithmetic
//! failure is reported as an error instead of producing a wrong number.

use ethnum::I256;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cmp::Ordering;
use std::fmt;
use std::ops::{Add, AddAssign, Div, Mul, MulAssign, Neg, Sub, SubAssign};
use std::str::FromStr;

/// Default precision: 18 decimal places.
pub const DEFAULT_SCALE: u32 = 18;

/// A fixed-point decimal with `S` decimal places.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fixed<const S: u32 = DEFAULT_SCALE>(i128);

/// The default fixed-point type used throughout the engine (18 decimal places).
pub type Decimal = Fixed<DEFAULT_SCALE>;

const POISON: i128 = i128::MIN;

const fn pow10(exp: u32) -> i128 {
    let mut v: i128 = 1;
    let mut i = 0;
    while i < exp {
        v *= 10;
        i += 1;
    }
    v
}

/// Divide `n` by `d` rounding half to even. `d` must be non-zero.
fn div_half_even(n: I256, d: I256) -> I256 {
    let q = n / d;
    let r = n % d;
    if r == I256::ZERO {
        return q;
    }
    let twice = r.abs() * I256::new(2);
    let dabs = d.abs();
    let negative = (n < I256::ZERO) != (d < I256::ZERO);
    let away = match twice.cmp(&dabs) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => (q % I256::new(2)) != I256::ZERO,
    };
    if away {
        if negative { q - I256::ONE } else { q + I256::ONE }
    } else {
        q
    }
}

fn narrow(v: I256) -> i128 {
    if v > I256::from(i128::MAX) || v <= I256::from(i128::MIN) {
        POISON
    } else {
        v.as_i128()
    }
}

impl<const S: u32> Fixed<S> {
    /// `10^S`, the raw representation of one.
    pub const SCALE: i128 = pow10(S);
    pub const ZERO: Self = Fixed(0);
    pub const ONE: Self = Fixed(pow10(S));
    /// The poisoned value produced by overflow or division by zero.
    pub const POISON: Self = Fixed(POISON);

    /// Construct from the raw scaled integer.
    pub const fn from_raw(raw: i128) -> Self {
        Fixed(raw)
    }

    /// The raw scaled integer.
    pub const fn raw(self) -> i128 {
        self.0
    }

    pub fn from_int(v: i64) -> Self {
        match (v as i128).checked_mul(Self::SCALE) {
            Some(r) if r != POISON => Fixed(r),
            _ => Self::POISON,
        }
    }

    /// `num / den` rounded half-even.
    pub fn ratio(num: i64, den: i64) -> Self {
        Self::from_int(num) / Self::from_int(den)
    }

    pub fn is_poisoned(self) -> bool {
        self.0 == POISON
    }

    pub fn is_zero(self) -> bool {
        self.0 == 0
    }

    pub fn is_negative(self) -> bool {
        !self.is_poisoned() && self.0 < 0
    }

    pub fn is_positive(self) -> bool {
        !self.is_poisoned() && self.0 > 0
    }

    pub fn abs(self) -> Self {
        if self.is_poisoned() { self } else { Fixed(self.0.abs()) }
    }

    /// -1, 0 or +1 as a `Fixed`.
    pub fn signum(self) -> Self {
        if self.is_poisoned() {
            self
        } else {
            Self::from_int(self.0.signum() as i64)
        }
    }

    /// Minimum that propagates poison.
    pub fn min(self, other: Self) -> Self {
        if self.is_poisoned() || other.is_poisoned() {
            Self::POISON
        } else if self.0 <= other.0 {
            self
        } else {
            other
        }
    }

    /// Maximum that propagates poison.
    pub fn max(self, other: Self) -> Self {
        if self.is_poisoned() || other.is_poisoned() {
            Self::POISON
        } else if self.0 >= other.0 {
            self
        } else {
            other
        }
    }

    /// Clamp into `[lo, hi]`, where either bound may be absent (unbounded).
    pub fn clamp_opt(self, lo: Option<Self>, hi: Option<Self>) -> Self {
        let mut v = self;
        if let Some(lo) = lo {
            v = v.max(lo);
        }
        if let Some(hi) = hi {
            v = v.min(hi);
        }
        v
    }

    /// Lossy conversion for display, plotting and test comparison. Never used in the engine.
    pub fn to_f64(self) -> f64 {
        if self.is_poisoned() {
            return f64::NAN;
        }
        let int = self.0 / Self::SCALE;
        let frac = self.0 % Self::SCALE;
        int as f64 + frac as f64 / Self::SCALE as f64
    }

    /// Convert to another precision (rounding half-even when reducing precision).
    pub fn rescale<const T: u32>(self) -> Fixed<T> {
        if self.is_poisoned() {
            return Fixed::<T>::POISON;
        }
        if T >= S {
            match self.0.checked_mul(pow10(T - S)) {
                Some(v) if v != POISON => Fixed(v),
                _ => Fixed::<T>::POISON,
            }
        } else {
            Fixed(narrow(div_half_even(I256::from(self.0), I256::from(pow10(S - T)))))
        }
    }

    /// Parse a decimal string such as `"3000"`, `" -0.05 "`, `"1.5E-3"`.
    /// Digits beyond the precision are rounded half-even.
    pub fn parse(s: &str) -> Result<Self, ParseFixedError> {
        let s = s.trim();
        if s.is_empty() {
            return Err(ParseFixedError(s.to_string()));
        }
        let err = || ParseFixedError(s.to_string());
        let (mantissa, exp) = match s.find(['e', 'E']) {
            Some(i) => (&s[..i], s[i + 1..].parse::<i32>().map_err(|_| err())?),
            None => (s, 0),
        };
        let (neg, mantissa) = match mantissa.as_bytes().first() {
            Some(b'-') => (true, &mantissa[1..]),
            Some(b'+') => (false, &mantissa[1..]),
            _ => (false, mantissa),
        };
        let (int_part, frac_part) = match mantissa.find('.') {
            Some(i) => (&mantissa[..i], &mantissa[i + 1..]),
            None => (mantissa, ""),
        };
        if int_part.is_empty() && frac_part.is_empty() {
            return Err(err());
        }
        if !int_part.bytes().chain(frac_part.bytes()).all(|b| b.is_ascii_digit()) {
            return Err(err());
        }
        // value = digits * 10^(exp - frac_len)
        let digits: String = format!("{int_part}{frac_part}");
        let digits = digits.trim_start_matches('0');
        let digits = if digits.is_empty() { "0" } else { digits };
        if digits.len() > 70 {
            return Err(err());
        }
        let mut n = I256::ZERO;
        for b in digits.bytes() {
            n = n * I256::new(10) + I256::new((b - b'0') as i128);
        }
        let shift = exp as i64 - frac_part.len() as i64 + S as i64;
        let scaled = if shift >= 0 {
            if shift > 76 {
                return Err(err());
            }
            let mut v = n;
            for _ in 0..shift {
                v = v.checked_mul(I256::new(10)).ok_or_else(err)?;
            }
            v
        } else {
            let down = (-shift) as u32;
            if down > 76 {
                I256::ZERO
            } else {
                let mut d = I256::ONE;
                for _ in 0..down {
                    d *= I256::new(10);
                }
                div_half_even(n, d)
            }
        };
        let raw = narrow(if neg { -scaled } else { scaled });
        if raw == POISON {
            return Err(err());
        }
        Ok(Fixed(raw))
    }
}

/// Error returned when a string is not a valid decimal number.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid decimal number: {0:?}")]
pub struct ParseFixedError(pub String);

impl<const S: u32> FromStr for Fixed<S> {
    type Err = ParseFixedError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Fixed::parse(s)
    }
}

impl<const S: u32> Default for Fixed<S> {
    fn default() -> Self {
        Self::ZERO
    }
}

impl<const S: u32> PartialOrd for Fixed<S> {
    /// Poisoned values are unordered.
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self.is_poisoned() || other.is_poisoned() {
            None
        } else {
            Some(self.0.cmp(&other.0))
        }
    }
}

impl<const S: u32> Add for Fixed<S> {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        if self.is_poisoned() || rhs.is_poisoned() {
            return Self::POISON;
        }
        match self.0.checked_add(rhs.0) {
            Some(v) => Fixed(v),
            None => Self::POISON,
        }
    }
}

impl<const S: u32> Sub for Fixed<S> {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        if self.is_poisoned() || rhs.is_poisoned() {
            return Self::POISON;
        }
        match self.0.checked_sub(rhs.0) {
            Some(v) => Fixed(v),
            None => Self::POISON,
        }
    }
}

impl<const S: u32> Neg for Fixed<S> {
    type Output = Self;
    fn neg(self) -> Self {
        if self.is_poisoned() { self } else { Fixed(-self.0) }
    }
}

impl<const S: u32> Mul for Fixed<S> {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        if self.is_poisoned() || rhs.is_poisoned() {
            return Self::POISON;
        }
        let p = I256::from(self.0) * I256::from(rhs.0);
        Fixed(narrow(div_half_even(p, I256::from(Self::SCALE))))
    }
}

impl<const S: u32> Div for Fixed<S> {
    type Output = Self;
    fn div(self, rhs: Self) -> Self {
        if self.is_poisoned() || rhs.is_poisoned() || rhs.0 == 0 {
            return Self::POISON;
        }
        let n = I256::from(self.0) * I256::from(Self::SCALE);
        Fixed(narrow(div_half_even(n, I256::from(rhs.0))))
    }
}

impl<const S: u32> AddAssign for Fixed<S> {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl<const S: u32> SubAssign for Fixed<S> {
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

impl<const S: u32> MulAssign for Fixed<S> {
    fn mul_assign(&mut self, rhs: Self) {
        *self = *self * rhs;
    }
}

impl<const S: u32> std::iter::Sum for Fixed<S> {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, |a, b| a + b)
    }
}

impl<const S: u32> fmt::Display for Fixed<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_poisoned() {
            return write!(f, "NaN");
        }
        let neg = self.0 < 0;
        let abs = self.0.unsigned_abs();
        let scale = Self::SCALE as u128;
        let int = abs / scale;
        let frac = abs % scale;
        let sign = if neg { "-" } else { "" };
        if S == 0 || frac == 0 {
            return write!(f, "{sign}{int}");
        }
        let frac_s = format!("{:0width$}", frac, width = S as usize);
        write!(f, "{sign}{int}.{}", frac_s.trim_end_matches('0'))
    }
}

impl<const S: u32> fmt::Debug for Fixed<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl<const S: u32> Serialize for Fixed<S> {
    fn serialize<Se: Serializer>(&self, serializer: Se) -> Result<Se::Ok, Se::Error> {
        serializer.collect_str(self)
    }
}

impl<'de, const S: u32> Deserialize<'de> for Fixed<S> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(deserializer)?;
        let s = match &v {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            _ => return Err(serde::de::Error::custom("expected decimal string or number")),
        };
        Fixed::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    type F = Fixed<18>;

    #[test]
    fn parse_and_display() {
        assert_eq!(F::parse("3000").unwrap().to_string(), "3000");
        assert_eq!(F::parse("  -0.05 ").unwrap().to_string(), "-0.05");
        assert_eq!(F::parse("1.5E-3").unwrap().to_string(), "0.0015");
        assert_eq!(F::parse("434.866594118346").unwrap().to_string(), "434.866594118346");
        assert!(F::parse("abc").is_err());
    }

    #[test]
    fn rounding_half_even() {
        let a = Fixed::<0>::from_int(5);
        let b = Fixed::<0>::from_int(2);
        assert_eq!((a / b).raw(), 2); // 2.5 -> 2
        assert_eq!((Fixed::<0>::from_int(7) / b).raw(), 4); // 3.5 -> 4
        assert_eq!((-a / b).raw(), -2);
    }

    #[test]
    fn mul_div() {
        let a = F::parse("0.1").unwrap();
        let b = F::parse("3000").unwrap();
        assert_eq!((a * b).to_string(), "300");
        assert_eq!((F::ONE / F::from_int(3)).to_string(), "0.333333333333333333");
        assert_eq!((F::from_int(2) / F::from_int(3)).to_string(), "0.666666666666666667");
    }

    #[test]
    fn poison_propagates() {
        let z = F::ZERO;
        let p = F::ONE / z;
        assert!(p.is_poisoned());
        assert!((p + F::ONE).is_poisoned());
        assert!(p.max(F::ONE).is_poisoned());
        let big = F::from_raw(i128::MAX);
        assert!((big + F::ONE).is_poisoned());
        assert!((big * F::from_int(2)).is_poisoned());
    }
}

