# Experiment H — monty #895, and what was actually there instead

**Reproduce:** `cargo run --release` (needs rustc 1.96+; links both 0.0.23 and
1.0.0-beta.2 so every result is a side-by-side)

The handoff listed [#895](https://github.com/pydantic/monty/issues/895) as
untested: *"Every `feed_run` mints and permanently interns a script name.
Long-lived sessions with many turns will hit this"* — a `u16` ceiling at 65,535
feeds. For a runtime whose whole premise is runs that live for weeks, a hard
ceiling on turns would be a real constraint, so it needed a number.

## #895 is not reachable from the Rust API

**300,000 feeds, no failure, on both 0.0.23 and 1.0.0-beta.2** — 4.5x the
theoretical ceiling, in about a second. Not a slow degradation or a caught error:
nothing happens at all. Whatever mints per-feed ids in the reported path, the
`MontyRepl` API as used here either reuses them or does not consume that space.

So the handoff's open item can be closed, with a caveat: this tests the shape of
the ceiling as it would be hit by *this* host design (one long-lived `MontyRepl`,
`feed_run` per turn, fixed script name). It is not a claim that #895 is fixed.

## What is actually there: snapshots grow with turn count, not state

Pushing the feed count turned up something the handoff does not mention. Guest
state held constant at one small dict, varying only how many feeds preceded the
dump:

### monty 0.0.23

| feeds | dump bytes | bytes/feed | dump ms | load ms |
| --- | --- | --- | --- | --- |
| 0 | 151 | — | 0.0 | 0.0 |
| 100 | 4,338 | 43.4 | 0.0 | 0.1 |
| 1,000 | 43,943 | 43.9 | 0.1 | 0.3 |
| 10,000 | 457,945 | 45.8 | 0.4 | 2.1 |
| 50,000 | 2,377,949 | 47.6 | 4.1 | 11.7 |
| 100,000 | 4,777,951 | 47.8 | 10.7 | 29.8 |

### monty 1.0.0-beta.2

| feeds | dump bytes | bytes/feed | dump ms | load ms |
| --- | --- | --- | --- | --- |
| 0 | 242 | — | 0.0 | 0.0 |
| 100 | 4,429 | 44.3 | 0.0 | 0.1 |
| 1,000 | 44,034 | 44.0 | 0.0 | 0.4 |
| 10,000 | 458,036 | 45.8 | 0.4 | 3.2 |
| 50,000 | 2,378,040 | 47.6 | 3.1 | 16.9 |
| 100,000 | 4,778,042 | 47.8 | 6.9 | 36.7 |

**About 48 bytes of permanent snapshot per feed, linear and unbounded**, on both
versions, identically. Experiment B's headline of ~6.9 KB for realistic agent
state is a measure of *state*; this is the other axis, and for a long-lived run
it is the one that dominates. A 100,000-turn run carries a 4.8 MB snapshot whose
contents are, as far as the guest is concerned, nothing.

Round-trip cost tracks it: dump and load are both linear in turn count, reaching
~11 ms and ~30 ms at 100,000 feeds. That cost is paid on **every** hibernation
and **every** cold wake, not once.

## A reload does not clear it

The obvious mitigation for a snapshot-based runtime would be to periodically
start a fresh interpreter from a dump — something a Durable Object host does
anyway each time it wakes. It does not help:

```
0.0.23         dump after 40,000 feeds: 1,897,953 bytes
               after reload + 40,000 more: 3,818,006 bytes  (2.01x)
```

Exactly double, on both versions. The accumulation is *in* the dump, so loading
it brings the whole history along. There is no in-API way found here to compact
a session; the only reset is abandoning the session and replaying, which is the
replay journal the design explicitly rejected.

## Consequences

1. **Drop the #895 turn ceiling from the risk list.** 300k turns is not a limit
   this design will meet.
2. **Budget snapshot size against turn count, not state size.** ~48 bytes/turn.
   At 1 GB per Durable Object the hard wall is ~21 million turns, so storage is
   not the constraint — cold-wake latency and per-hibernation write volume are.
3. **Ordinary runs are unaffected.** At 1,000 turns a snapshot is 44 KB and
   loads in 0.3 ms. The concern starts in the tens of thousands.
4. **A turn budget per run is worth having anyway**, since it now has a cost
   curve attached rather than a cliff.
