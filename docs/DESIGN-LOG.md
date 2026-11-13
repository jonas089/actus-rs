# actus-rs design log

Decisions agreed during the design walkthrough. Each entry: decision, rationale, open follow-ups.
Reference material (not part of this crate) lives in `../ref/` (official spec, dictionary, tests)
and `../actus-core/` (2020 Java reference, ACTUS Core License — read for cross-checking only, do not copy).

## D1 — Source of truth (proposed, awaiting confirmation)
- Normative: `actus-techspecs` (spec) + `actus-tests` (277 official cases for 18 types).
- The Java `actus-core` is a cross-check only, never authoritative.
- Where the spec is silent, ambiguous or wrong, we write a documented addendum (our own spec text + tests).

## D2 — Deployment target
- General-purpose Rust library. `std` is allowed; no `no_std` requirement
  (modern zkVMs/TEEs run std Rust).
- Determinism is still required (same inputs → bit-identical outputs on every platform),
  which rules out floating point in the engine.

## D3 — Numerics
- Fixed-point decimal arithmetic, configurable precision, default 18 decimal places.
- Finding: the spec's math is purely rational (+, −, ×, ÷, min/max, finite products/sums).
  No exp/ln/sqrt anywhere, so fixed point can represent it exactly up to rounding.
- Storage: `i128`; multiply/divide computed in 256-bit intermediates, then rounded back.
  Range at 18 dp: about ±1.7e20 units (official tests max out at 1.1e7).
- Precision is a compile-time type parameter, e.g. `Fixed<18>` as the default; mixing precisions is a type error.
- Rounding: half-even (banker's) wherever a result has more digits than the precision.
- Payoffs are never rounded to currency minor units by the engine; that's the caller's job.
- Overflow (default, not yet discussed): checked arithmetic returning an error, never wrap, never panic.

## D4 — Coverage and phasing
- Phase 1: the 18 spec-defined types (PAM, LAM, LAX, NAM, ANN, CLM, UMP, CSH, STK, COM,
  FXOUT, SWPPV, SWAPS, CAPFL, OPTNS, FUTUR, CEG, CEC), conformant to official tests.
- Phase 2: design + specify the rest one by one with the user
  (ANX, NAX, PBN, BNDCP, BNDWR, EXOTi, CDSWP, TRSWP, CLNTE, SCRMR, SCRCR, MAR, REP, and BCS).
- Constraint: Phase-1 core abstractions must not preclude Phase 2
  (composition / child contracts, credit events, path-dependent state).

## D5 — Core execution model: pure step + projection on top
- Core is a pure, deterministic function:
  `step(terms, state, event, inputs) -> Result<(State, Payoff)>`.
- `required_inputs(terms, state, event) -> Vec<ObservationKey>` declares what an event needs
  (e.g. reference rate for a rate reset), so every input is explicit and can be recorded,
  replayed, audited or proven.
- Schedules are lazy iterators (`schedule(terms) -> impl Iterator<Item = Event>`), so contracts
  with no natural end (STK, UMP, PBN) need no artificial cutoff.
- Unscheduled events (prepayment, default, call, exercise) are ordinary events passed in.
- Batch projection (`project(terms, until, &dyn Observer)`) is a convenience driver on top;
  the spec's risk-factor observer becomes a source of inputs for that driver, not part of the core.

## D6 — Contract terms: typed struct per contract type
- `enum Contract { Pam(PamTerms), Ann(AnnTerms), ... }`; each terms struct holds only the
  attributes that apply to that type, as typed fields.
- Dictionary applicability rules are enforced when terms are built (invalid combos rejected up front).
- JSON in/out in the official ACTUS format (as used by actus-tests) via serde.

## D7 — Composition: both, by role
- Children that exist only as part of the parent (SWAPS/SWPPV legs, the underlying loan of CAPFL)
  are embedded and stepped together as a tree.
- Independent contracts (loans covered by CEG/CEC, an OPTNS/FUTUR underlying) are referenced by ID;
  their states/events arrive as explicit inputs like market data.

## Notes
- Official test format: `{terms, to, dataObserved, eventsObserved, results}`; results list
  eventDate, eventType, payoff, currency and the state values after each event.
- The Java test suite compares results rounded (floor) to 10 decimals; our conformance
  harness will use a comparable tolerance, since fixed-point vs float differ in the last digits.

## D8 — Date/time: chrono
- Timestamps are `chrono::NaiveDateTime` (no time zone, matching ACTUS ISO 8601 timestamps).
- ACTUS-specific schedule arithmetic (cycle stepping, end-of-month, business-day shifting)
  is implemented by us on top of chrono.

## D9 — Business-day calendars: trait + 2 built-ins
- `BusinessDayCalendar` trait; built-ins: NoHoliday (default) and MondayToFriday, as in the spec.
- Real holiday calendars are user-supplied; we don't ship or maintain holiday data.

## D10 — State: typed per contract type + uniform view
- Each contract type has its own state struct holding only the variables it uses.
- A `StateView` trait exposes any spec state variable by name (`get(StateVariable)`),
  used by parent contracts, input recording, and the conformance test output.

## Status (2026-11-13)
- All 18 spec-defined contract types implemented; 276/276 official ACTUS test cases pass
  (`cargo run --example conformance`, needs `../ref/actus-tests`).
- Deviations from the decisions above, to revisit:
  - D5: schedules are generated eagerly up to a horizon (`to`), not as lazy iterators.
  - D10: the lending types (PAM, LAM, LAX, NAM, ANN, UMP) share one `LoanState`; CLM extends it.
  - SWAPS evaluates its legs and merges results; its `step` looks results up rather than being an
    independent transition function.
- All places where behaviour differs from the spec, or where we chose between spec and tests,
  are in `docs/notes/*.md`.
