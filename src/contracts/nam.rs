//! NAM — Negative Amortizer.
//!
//! The borrower pays a fixed total amount per period (`PRNXT`); interest is taken out of it
//! first and only the remainder redeems principal. If interest exceeds the payment, the
//! principal part is negative (the loan grows) — hence "negative amortization".
//! The event logic is shared with LAM in [`super::lam::AmortizerTerms`].

use super::lam::AmortizerTerms;
use super::loan::LoanState;
use crate::error::Result;
use crate::fixed::Fixed;
use crate::time::{schedule, DateTime};

/// NAM terms.
pub type NamTerms<const S: u32> = AmortizerTerms<S>;

/// Principal part of a NAM/ANN payment: the fixed payment minus accrued interest, capped at the
/// outstanding notional. `accrued_since` is interest accrued since the last status date.
pub(crate) fn redemption<const S: u32>(t: &AmortizerTerms<S>, s: &LoanState<S>, accrued_since: Fixed<S>) -> Fixed<S> {
    let r = t.common.role_sign();
    let prnxt = s.next_principal_redemption_payment.unwrap_or(Fixed::ZERO);
    let amount = prnxt - r * (s.accrued_interest + accrued_since);
    amount - (amount - s.notional_principal.abs()).max(Fixed::ZERO)
}

/// Maturity when not given: from the last redemption date at or before the status date,
/// count the periods the fixed payment needs to repay the notional, where each payment's
/// principal part is the payment minus one period's interest on the full notional.
pub(crate) fn derived_maturity<const S: u32>(t: &AmortizerTerms<S>) -> Result<DateTime> {
    let cycle = t.pr_cycle()?;
    let sd = t.common.status_date;
    let ied = t.initial_exchange_date;
    let anchor = t.principal_redemption_anchor;
    let last = if anchor >= sd {
        anchor
    } else if cycle.period.add_to(ied) >= sd {
        cycle.period.add_to(ied)
    } else {
        schedule(Some(anchor), sd, Some(cycle), t.common.end_of_month, true)
            .into_iter()
            .filter(|x| *x < sd)
            .next_back()
            .unwrap_or(anchor)
    };
    let yf: Fixed<S> = t.day_counter.year_fraction(last, cycle.period.add_to(last));
    let rate = t.nominal_interest_rate.unwrap_or(Fixed::ZERO);
    let per_cycle = t.prnxt_term()? - yf * rate * t.notional_principal;
    let remaining = t.periods_needed(per_cycle)? - 1;
    Ok(t.common.business_day.shift_event_time(cycle.period.times(remaining).add_to(last)))
}
