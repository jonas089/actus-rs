# Notes: PERP

A perpetual future: linear, cash settled, no maturity. ACTUS does not define it, so there is no
spec text and no official test to follow. This note is the specification; `tests/perp.rs` works
every number out by hand.

## What it takes from ACTUS, and what is new

- **From the dictionary, unchanged** — `contractStructure` with a `UDL` reference (the price the
  position is marked at), `quantity`, `futuresPrice`, `terminationDate`, `priceAtTerminationDate`,
  `settlementCurrency`, and the roles `LG` / `ST` (`BUY` / `SEL` work too).
- **From the dictionary, used for the first time** — `cycleOfMargining` and
  `cycleAnchorDateOfMargining`. The dictionary has a Margining group and an `MR` event, but none
  of the 18 specified types uses them.
- **New** — the contract type `PERP`; the terms `cycleOfFunding`, `cycleAnchorDateOfFunding` and
  `marketObjectCodeOfFunding`; the event `FND` (funding payment, sequence 75); the state
  `settlementPrice`.
- **New in the core** — periods with a time part (`PT8H`, `P1DT12H`, `PT30M`). ACTUS cycles stop
  at days; funding does not. In the date part `H` is still the ACTUS half year and `M` a month;
  after the `T` they are hours and minutes.

## Events and payoffs

`R` is the role sign, `q` the quantity, `P(t)` the observed price, `Ps` the settlement price in
the state, `X` the settlement-currency rate.

- **FND** — `X * -(R * q * P(t) * f(t))`, where `f(t)` is the funding rate observed at `t` for the
  period ending at `t`. A positive rate is paid by the long side. State unchanged.
- **MR** — `X * R * q * (P(t) - Ps)`, then `Ps = P(t)`.
- **TD** — `X * R * q * (C - Ps)` with `C` the close price, then `Ps = C` and the performance
  becomes `TE`.
- **CE**, **AD** — as for STK: a credit event sets the performance, no cash.
- At the same instant the order is FND, MR, TD: whoever holds the position at a funding time
  pays or receives that funding, also when it is closed at that time.

The initial `Ps` is `futuresPrice`. Terms describing a running position give the price it was
last settled at there, with the status date at that settlement.

## Decisions

- **The funding rate is observed, not computed.** Venues differ in how they derive it (premium
  index, interest component, clamps, averaging). The contract takes the published rate as a risk
  factor, `marketObjectCodeOfFunding`, by default `"<underlying>-FR"`.
- **One price series.** Settlement and the funding notional both use the underlying's observed
  price. A venue with a separate mark and index price publishes the mark as the underlying; the
  index only enters through the funding rate.
- **No funding for a part period.** Funding is paid at funding times to whoever holds the
  position then; closing between two funding times pays nothing for the time since the last one.
- **Margining is optional.** Without `cycleOfMargining` the whole gain or loss is paid at TD.
  The total is the same either way (tested); only the timing differs.
- **The end is an event, not a term.** A close or a liquidation is an observed `TD` event whose
  value is the close price; `terminationDate` does the same from the terms. The earlier of the
  two ends the position. A value of zero (or no `priceAtTerminationDate`) means the price
  observed at that time.
- **No end, so a horizon.** As for UMP and STK, `project` needs `to` unless a termination is
  known; `stream` is unbounded.
- **Cycle anchors default to the status date**, so the first funding and the first settlement
  fall one cycle after it.
- **Termination cuts by time.** Nothing happens to a position after the second it is closed in;
  events in that same second are all applied, in sequence order.

## Not modelled

- **The margin balance.** Initial and maintenance margin, and whether a loss was covered, belong
  to the account a position is held in, which holds many positions. The dictionary's
  `initialMargin`, `maintenanceMarginLowerBound` and `variationMargin` are therefore not read.
- **Changing size.** `quantity` is fixed. Adding to or reducing a position is a new contract, or
  a position layer above this one.
- **Inverse (coin-margined) perpetuals**, whose payoff is in `1 / price`.
- **Trading fees.**

## Ticks

A ledger counts time in ticks (block heights, slots), not dates. Three things in the core make
contracts run at that resolution; all of them apply to every type, not only PERP.

- **Events order by time, then by sequence.** The reference implementation added the per-type
  sequence to the event time as seconds (up to 950), so two events of different types less than
  16 minutes apart could swap, and an event that soon after the status date was dropped. The
  sequence now only orders events of the same second (`events::sort_key`). No official test
  depended on the old behaviour: all 276 pass unchanged.
- **`time::TickClock`** maps tick `k` to `genesis + k × seconds_per_tick` and gives cycles as a
  number of ticks. Terms and observations stay in engine time; the clock converts at the
  boundary. PERP and FUTUR never measure elapsed time, so for them the axis can be purely
  logical (one second per tick). Types that accrue interest take year fractions from the engine
  time, so their clock must follow the ledger's real slot length, and a ledger with irregular
  slots needs a timestamp per tick, which this clock does not model.
- **`stream` starts with a one-day window** and doubles it, instead of starting with a year, so
  a contract with a cycle of seconds yields its first events at once.

Still open:

- **Resolution is one second.** Two ticks cannot share a second.
- **Streaming cost.** `stream` regenerates the schedule from the status date for each window, so
  a long-running position with a short cycle should be re-based (new status date, `futuresPrice`
  = last settlement price) from time to time.
