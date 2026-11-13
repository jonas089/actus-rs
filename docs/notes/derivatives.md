# Notes: OPTNS, FUTUR, CEG, CEC

Format: **what** — spec says / tests (Java) do / what we implemented.

## OPTNS / FUTUR

- **PRD payoff sign** — spec: `X * (-PPRD)` (no role sign). Tests and Java: `R * (-PPRD)`. We: `fx * R * (-PPRD)`.
- **TD payoff** — spec: `X * PTD`, no role sign. Java: same. We: `fx * PTD`.
- **STD payoff** — spec: `X * R * Xa`. Java: same. We: same, and `Xa` is reset to 0.
- **MD vs XD ordering** — XD and MD fall on the same date when no exercise date is given. Sequence offsets put MD (150) before XD (160), so MD decides `Xd` first and XD then computes `Xa`. Tests depend on this ordering.
- **FUTUR MD** — spec: `Xd = t` unless `S_t - PFUT == 0`. Java: same, and it overwrites an existing `Xd`. We: same as Java.
- **OPTNS MD** — `Xd` is set only when the option is unexercised and in the money.
- **XD date** — `exerciseDate` term, else the first observed XD event for this contract on or before maturity (American and Bermudan options only), else maturity. Spec: XD at XD term or MD. Java/tests: the same, plus observed exercise. We: as described.
- **Collar (CP)** — intrinsic value is `max(S-K1,0) + max(K2-S,0)`, the same in spec and Java.

## CEG

- **Exercise amount** — spec: `Xa = CECV * Nt`. Java: `Xa = Nt`, but its `Nt` already contains `CECV * R`. Applying the spec literally would count coverage twice. We: `Xa = Nt`.
- **Covered contract role** — Java overrides every embedded covered contract's `contractRole` to `RPA` before evaluating it, so exposure is always measured from the creditor's side. The spec does not say this. We do the same as Java.
- **Notional** — spec: `CECV * R * NT`, else the covered exposure. The exposure is notional (NO), notional plus accrued interest (NI), or market value (MV). Java recomputes it at PRD, FP and XD. We do the same. Child states are evaluated at `t` by injecting a monitoring event, so accrual reaches `t`.
- **Credit event types (CETC)** — spec: a list. Java: only `CETC[0]` is used. We match against any listed type; the default is `[DF]`.
- **Credit event trigger** — XD is scheduled at the first observed event of a covered contract (by its ID) on or before maturity whose performance is in CETC. STD follows at XD + STP. MD is dropped when XD exists.
- **PRD state** — Java sets `feeAccrued = feeRate`, which is a bug. We leave `Feac` unchanged.
- **Fee basis A accrual** — Java divides by the period ending at `sd + cycle` instead of the actual next fee date. We use the actual surrounding FP dates and fall back to `prev + cycle`.
- **FP payoff (FEB=A)** — `fx * R * FER`. The guarantor receives the fee when the guarantee is bought (`R = +1`).
- **Initial `Feac`** — Java has a TODO here. We use the `feeAccrued` term, else accrual over the current fee period.
- **Day count default** — A365 when `dayCountConvention` is absent; Java does the same.
- **Test-value truncation** — Java's `ResultSet.roundTo` floors to an integer before comparing (`setScale(10, FLOOR).toBigInteger()`). Some expected values are therefore truncated: guarantee14 STD expects 3508695, while the true value is 3508695.652. This case depends on LAM and is not exercised yet. If it fails only on the fraction once LAM exists, the harness needs an integer-floor fallback.

## CEC

- **Notional** — spec: `Nt = min(MV collateral, exposure)`. Java: `Nt` is `CECV * R * exposure`, and the `min` applies only to `Xa` at XD. We follow Java.
- **Exercise amount sign** — Java: `min(MV, Nt)`. For a seller role `Nt < 0`, so the `min` always returns `Nt`. We use `sign(Nt) * min(MV, |Nt|)`, which matches the tests for buyers and stays sensible for sellers.
- **Collateral value** — read as the market value of each covering contract's `marketObjectCode` at `t`. Covering contracts are not evaluated, so COM support is not needed.
- **Events** — CEC has no PRD or FP; it uses XD/STD on a credit event, else MD.

## Resolved after merge

- guarantee09–14 and collateral09–15 (LAM covered contracts) pass after merging LAM.
- guarantee14's expected STD payoff is stored truncated to an integer in the test data; the
  harness accepts the integer part for exactly that case (`ERRATA_TRUNCATED_PAYOFF`).
- Side finding: the Java test harness compared `setScale(10, FLOOR).toBigInteger()`, i.e. only the
  integer part of every value. Our harness compares to ~1e-9, so it is far stricter than the suite
  the reference implementation was validated with.
