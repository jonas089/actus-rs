//! ANN — Annuity.
//!
//! Like NAM (a fixed total payment per period, interest first), but the payment is the
//! annuity amount that exactly repays the loan by maturity, and it is recomputed by a
//! principal-redemption-fixing (PRF) event after every rate reset.
//! The event logic is shared with LAM in [`super::lam::AmortizerTerms`].

use super::lam::AmortizerTerms;
use super::loan::{LoanCalc, LoanState};
use crate::events::{Event, EventType};
use crate::fixed::Fixed;
use crate::time::schedule;

/// ANN terms.
pub type AnnTerms<const S: u32> = AmortizerTerms<S>;

/// Annuity amount `A(s, T, n, a, r)`: the constant payment that repays notional `n` plus
/// accrued interest `a` over the remaining redemption dates `t_1..t_m` at rate `r`:
///
/// `A = (n + a + Y(s, t_1) r n) * prod_{i<m}(1 + r Y(t_i, t_{i+1})) / (1 + sum_{i<m} prod_{j>=i}(1 + r Y(t_j, t_{j+1})))`
///
/// The result carries the sign of the notional (negative for liabilities).
pub(crate) fn annuity_amount<const S: u32>(t: &AmortizerTerms<S>, s: &LoanState<S>) -> Fixed<S> {
    annuity_amount_from(t, s, false)
}

/// Annuity amount over the redemption dates after the status date, or at/after it if
/// `include_status_date` (used when the first redemption falls on the status date itself).
pub(crate) fn annuity_amount_from<const S: u32>(
    t: &AmortizerTerms<S>,
    s: &LoanState<S>,
    include_status_date: bool,
) -> Fixed<S> {
    let end = t.amortization_date.unwrap_or(t.maturity_date);
    let sd = s.status_date;
    let times: Vec<_> = schedule(
        Some(t.principal_redemption_anchor),
        end,
        t.principal_redemption_cycle,
        t.common.end_of_month,
        true,
    )
    .into_iter()
    .filter(|x| *x > sd || (include_status_date && *x == sd))
    .collect();
    if times.is_empty() {
        return Fixed::ZERO;
    }
    let r = s.nominal_interest_rate;
    let n = s.notional_principal;
    let dc = &t.day_counter;
    let growth: Vec<Fixed<S>> =
        times.windows(2).map(|w| Fixed::ONE + r * dc.year_fraction::<S>(w[0], w[1])).collect();
    // Suffix products: suffix[i] = prod_{j>=i} growth[j].
    let mut suffix = vec![Fixed::ONE; growth.len() + 1];
    for i in (0..growth.len()).rev() {
        suffix[i] = suffix[i + 1] * growth[i];
    }
    let product = suffix[0];
    let sum: Fixed<S> = suffix[..growth.len()].iter().copied().sum();
    let scale = n + s.accrued_interest + dc.year_fraction::<S>(sd, times[0]) * r * n;
    scale * (product / (Fixed::ONE + sum))
}

/// PRF events: re-fix the annuity amount after each rate reset, and once before the first
/// redemption if no amount was given.
pub(crate) fn prf_events<const S: u32>(t: &AmortizerTerms<S>, calc: &LoanCalc<'_, S>, rate_resets: &[Event<S>]) -> Vec<Event<S>> {
    let mut times: Vec<_> = rate_resets.iter().map(|e| e.time).collect();
    if t.next_principal_redemption_payment.is_none() {
        let prf = t.principal_redemption_anchor - chrono::Duration::days(1);
        // A fixing before the loan exists would see no notional; in that case the amount is
        // fixed by the IED event instead (see `fixes_amount_at_ied`).
        if prf >= t.initial_exchange_date {
            times.push(prf);
        }
    }
    times.sort();
    times.dedup();
    calc.events(&times, EventType::PRF)
}

/// Whether the annuity amount must be fixed at IED because the first redemption is due
/// immediately (no PRF event can precede it).
pub(crate) fn fixes_amount_at_ied<const S: u32>(t: &AmortizerTerms<S>) -> bool {
    t.next_principal_redemption_payment.is_none()
        && t.principal_redemption_anchor - chrono::Duration::days(1) < t.initial_exchange_date
}
