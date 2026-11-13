# Notes: CLM, UMP, CSH, STK, COM

Format: what / spec says / official tests (Java reference) do / what we implemented.

## CLM — Call Money

- **Calls (XD) are not in the spec at all.** Spec: maturity `Md` = `MD`, else the time of the last
  observed event, else `t_max`; no XD/STD events. Tests (clm07, 08, 09, 13, 14): an observed `XD`
  event is a call; repayment happens at call date + `xDayNotice` with `IP` + `STD` (settlement)
  events. The Java reference skips exactly these five tests (it never implemented calls), so the
  expected values come from a different generator. Implemented: call date = earliest observed XD;
  maturity = call + notice; IPCI/RR/FP cycles end at the call date (exclusive); events XD, then
  IP and STD at maturity; `STD` instead of `MD` when called.
- **Interest stops at the call date.** Tests: with a 31-day notice, IP at repayment pays interest
  only up to the call date, not up to repayment. Implemented: after XD no further accrual.
- **Where the interest up to the call is held differs with/without notice.** Tests: with notice
  (clm07/08) the XD row shows accrued interest 0; with zero notice (clm09/13/14) it shows the full
  accrued interest. Implemented: XD with notice moves accrued interest into the `exerciseAmount`
  state (paid at IP); XD without notice keeps it in accrued interest. Economically identical;
  only the reported split differs. CLM therefore has its own state (`ClmState` = lending state +
  `exerciseDate` + `exerciseAmount`).
- **XD sorts before IP at the same instant.** Generic event sequence puts XD (160) after IP (40);
  tests list XD, IP, STD at the same timestamp. Implemented a CLM-local order with XD first.
- **No initial accrued-interest estimate.** Spec ("same as PAM") and the dictionary: if
  `accruedInterest` is not given, estimate it from the last interest date to the status date.
  Tests (clm03, 05, 06, 07, 10, 11 with status date after IED): accrued interest at the status
  date is 0, i.e. one partial period of interest is silently lost. Implemented the test behaviour
  (`accruedInterest` term or 0) for CLM.
- **Analysis horizon is ignored once the end is known.** clm07 has `to` = 2015-09-30 but the
  expected results run to repayment on 2015-10-21. Implemented: `to` only clips when there is
  neither a maturity date nor a call; otherwise the contract runs to repayment.
- **Cycle anchors default to IED, not IED + cycle.** Spec: IPCI anchor = `IPANX` else `IED + IPCL`.
  Tests (clm12): first IPCI at IED. Implemented the test behaviour (same as PAM).
- **IED payoff** excludes `premiumDiscountAtIED` (spec POF_IED_CLM), as specified.
- **MD/STD payoff** also includes any remaining accrued interest and fees (spec MD of PAM);
  after IP at the same instant these are zero, so it matches the tests.

## UMP — Undefined Maturity Profile

- No maturity: projections require a horizon `to` (or a termination date); otherwise an error.
- Principal changes only through observed `PR` (repayment) / `PI` (increase) events, value =
  unsigned amount, sign applied by contract role. Not exercised by the official tests.
- Spec PR fee accrual for `FEB = A` uses an undefined `t^-`/`t^+`; we use the generic fee accrual.
- Initial accrued interest follows the spec (estimate if not given); all official UMP tests start
  before IED, so this is untested. Note that CLM, whose spec is identical here, had to use the
  "no estimate" behaviour to pass its tests.
- Termination (`TD`) uses the PAM formula `R * (PTD + accrued interest)`; matches ump09.

## CSH — Cash

- Spec has only AD events. Tests supply AD events via `eventsObserved` without a `contractId`;
  the conformance harness now assigns such events to the contract under test.

## STK — Stock

- **Dividend amount source differs.** Spec: DV payoff = `R * O^rf(CID + "-DV", t)`. Tests use a
  non-dictionary term `marketObjectCodeOfDividends` and multiply by `quantity`. Implemented:
  `marketObjectCodeOfDividends` if given, else `"<CID>-DV"`; payoff `R * quantity * dividend`.
- **Term names**: tests use `cycleOfDividendPayment` / `cycleAnchorDateOfDividendPayment`; the
  dictionary has `cycleOfDividend` / `cycleAnchorDateOfDividend`. Both accepted.
- **Quantity**: spec PRD/TD payoffs omit `quantity`; tests (and economics) multiply by it.
  Implemented `R * quantity * price` (quantity default 1).
- Dividend observation uses the unshifted schedule date; the event itself is shifted by the BDC
  (stk09: 2017-01-01 Sunday paid 2017-01-02, observed at 2017-01-01).
- Dividend anchor defaults to the purchase date when only a cycle is given (Java default).
- `nextDividendPaymentAmount` (spec DV(fix)) fixes the first dividend; untested.
- Dividends recur without end; we generate up to the termination date or the horizon.

## COM — Commodity

- **Spec TD uses the PRD payoff function** (`POF_PRD_STK`, i.e. pays −price at termination) —
  a spec typo. Tests: TD receives `R * quantity * PTD`. Implemented the test behaviour.
- Spec omits `quantity`; tests multiply by it (com01: 2 × 700).
- PRD/TD are only scheduled if strictly after the status date and strictly before the horizon
  `to` (Java behaviour; the spec has no such rule).
