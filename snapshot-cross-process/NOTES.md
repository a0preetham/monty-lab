# Experiment B — snapshot round-trip across a process boundary

**Reproduce:** `./run.sh`. Every stage is a separate invocation of the binary, so
interpreter state crosses only as bytes on disk.

| Stage | Process | What it proves |
| --- | --- | --- |
| 1 | A | Build agent state, `dump` Idle to a file |
| 2 | B | A process that never built the state loads it and keeps feeding |
| 3 | C | Suspend mid-tool-call, dump, exit **without** resolving |
| 4 | D, E | Resume those same bytes against a *different* implementation, twice |
| 5 | — | Dump sizes for realistic agent state |
| 6 | — | Version-mismatch and garbage handling |

## Results

A 6,952-byte dump carried: plain globals, a dict, a 50-row list, a float from
`math`, a user function, a class, a live instance with mutated attributes, and a
custom `__repr__`. In the fresh process all of it worked, `isinstance` still held
against the restored class, and new classes and functions could be defined on top
of restored state.

The suspended leg is the one that matters for durable execution. Stage 3 parked on
`fetch_price('WIDGET')` and exited. Stage 4 — a different process — restored, saw
it still paused at the same call, answered `250`, and ran to completion. Stage 4
run again over the *same bytes* answered `999` and produced a different result
(`Budget(370/1000)` vs `Budget(1119/1000)`). The dump records the paused
function's name, never host code, so one snapshot resolves against whatever
implementation the new process supplies.

## Dump sizes

| Session | Bytes |
| --- | --- |
| empty | 66 |
| typical agent state (50 rows, a class, functions) | 6,867 |
| + 100 dict rows | 14,211 |
| + 1,000 dict rows | 80,813 |
| + 10,000 dict rows | 761,351 |
| + 500 feeds appending to a list | 62,778 |

Roughly 74 bytes per dict row. A typical session is 0.00064% of the free-plan 1 GB
per-object budget, so snapshot size is not a constraint — guest data volume is,
and only past ~10k rows.

## Header behaviour

`DUMP_VERSION` is 8. The header is `MONTY\0` plus a little-endian `u16`, readable
before any deserialization:

```
genuine dump   -> loads
header says v7 -> VersionMismatch { found: 7, expected: 8 }
garbage        -> NotADump
```

A stale snapshot is distinguishable from a corrupt one without a matching
interpreter, which is what makes the workstream D dispatcher possible.

## Note

The one failure on the first run (`float from math`) was a wrong hardcoded
expectation in the test, not interpreter behaviour: `sqrt(2.25·Σi² for i<50)` is
301.5895, which is what Monty returned. Fixed.
