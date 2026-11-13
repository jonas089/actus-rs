# FXOUT, SWPPV, SWAPS, CAPFL: spec vs. official tests

Each bullet: what / spec says / tests (Java reference) do / what we implemented.

## FXOUT
- **Delivery events are called MD, not STD.** Spec: physical delivery creates two settlement events `STD(1)` (receive `NT` in CUR) and `STD(2)` (pay `NT2` in CUR2). Tests: two `MD` events at maturity, the second with currency `CUR2`. Implemented: two `MD` events; the second carries currency2 and pays `R·(−NT2)`.
- **Settlement date of a cash-settled FXOUT.** Spec: `STD` at `Md` (maturity). Tests: `STD` at maturity + `settlementPeriod`, business-day shifted; the FX rate `CUR2/CUR` is still observed *at maturity*. Implemented as the tests do.
- **Termination payoff has no role sign.** Spec: `POF_TD_STK` (role-signed price). Tests: payoff = `PTD` regardless of role (`fxout10`: RPL, PTD 100 → +100). Implemented as the tests do. The purchase payoff *is* role-signed (`R·(−PPRD)`), so PRD/TD are asymmetric — likely a reference-implementation bug.
- **No maturity events when terminated.** Tests: with a termination date, no MD/STD at all is generated (even if TD is after maturity). Implemented as the tests do.
- **Input formats.** Tests use `calendar: "NOCALENDAR"` and `settlementPeriod: "P1DL0"` (a period with a cycle stub suffix). Both are accepted (shared change: `NOCALENDAR` in `terms.rs`; FXOUT strips the `L…` suffix).
- **States.** The reference implementation reports notional/rate/accrued as 0 for FXOUT; our FXOUT state carries only status date, maturity and performance. The conformance harness now treats a state the contract type doesn't carry as 0 (shared change in `conformance.rs`).

## SWPPV
- **Extra role sign in the spec.** Spec IP payoffs are `R·(Ipac + Y·IPNR·Nt)` while `Nt` is already `R·NT`, so the role sign is applied twice (fixed payer would receive fixed interest). Tests: no extra sign. Implemented without the extra `R`.
- **Fixed/floating leg event names and gross accrual.** Spec: `IP(fix)`/`IP(var)` with separate `Ipac1`/`Ipac2`. Tests: `IPFX`/`IPFL`; the reported `accruedInterest` is the fixed-leg accrual in gross mode (DS=D) and the net (fixed−floating) accrual in net mode (DS=S). Implemented: `accruedInterest` = fixed (gross) / net accrual, `accruedInterest2` = floating accrual.
- **Rate resets are not capped.** Spec and tests agree: `Ipnr = RRMLT·O + RRSP`, no life/period caps or floors (unlike PAM). Implemented as specified.
- **Termination payoff has no role sign** (same as FXOUT; `swppv13`: PF, PTD 400 → +400). Purchase is role-signed. Implemented as the tests do.
- **Reference bugs not reproduced (untested):** Java's `AD` transition adds the fixed accrual twice; its initial state (status date after IED) uses `IPNR` instead of `IPNR2` for the floating rate. We use the spec (single accrual, `IPNR2`).

## SWAPS
- **Not a state machine of its own.** Spec defines SWAPS by composing the legs' events. Tests (Java): legs are evaluated independently and their *results* are merged. Implemented: `evaluate()` runs both legs (with the observer) and combines results; `step()` looks the event up in that evaluation, so the core interface still holds.
- **Leg roles come from the parent.** The legs' own `contractRole` is overridden: `RFL` → first leg RPA, second leg RPL; otherwise the reverse. Implemented as the tests do.
- **Net settlement (DS=S) drops most events.** Tests: only IED, MD, PR and IP survive; congruent pairs are netted into one parent event (payoff sum, notional sum, accrued sum, **rate reported as 0**); IED/MD that don't coincide in time are dropped entirely; RR, PRF, FP etc. of the legs disappear. Implemented as the tests do. This loses information (e.g. leg rate resets) and is non-intuitive.
- **Purchase/termination payoffs are unsigned and odd.** PRD payoff = `PPRD` (no role sign, positive). TD payoff = `PTD + accrued`, where "accrued" is taken from leg events at exactly the termination time (0 if a leg has an IP then or no event then). Implemented as the tests do; the accrued term is a rough approximation of the true accrued interest at TD.
- **Unresolved here:** 5 of 11 cases (swaps04–08) have ANN legs; they fail in this branch only because ANN is implemented in another branch.

## CAPFL
- **Only interest events are reported.** Spec: the cap's events are the merged IP events plus its own PRD/TD. Tests: only merged IP events; Java evaluates PRD/TD with the underlying's functions and then discards them. Implemented: merged IPs as in the tests; PRD/TD per the spec (not covered by tests).
- **Every IP is reported, even with zero payoff.** Payoff `R·|IP(uncapped) − IP(capped)|`; states reported as 0. Implemented as specified/tested.
- **Underlying role and caps.** The underlying is forced to RPA; the capped copy gets the cap's `lifeCap`/`lifeFloor` (absent → unbounded, overriding any caps the underlying had). Implemented as the tests do.
