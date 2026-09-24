# Experiment D — several Monty versions in one binary

**Reproduce:** `cargo run --release --bin dispatcher` (needs rustc 1.96+, see
[MSRV](#the-msrv-couples-every-linked-version))

## Linking

Every Monty `0.0.x` release is its own semver-incompatible range as far as Cargo
is concerned, and `1.0.0-beta.2` is incompatible with those in turn. So this is
all the manifest needs:

```toml
monty_v22 = { package = "monty", version = "=0.0.22" }
monty_v23 = { package = "monty", version = "=0.0.23" }
monty_v1  = { package = "monty", version = "=1.0.0-beta.2" }
```

All three compile into one binary. They are wholly separate crates — separate
types, separate statics, distinct symbols — so handing a `monty_v22::MontyRepl`
to a `monty_v23` function is a compile error rather than a runtime corruption.
That safety property is recorded as a `compile_fail` doctest in `src/main.rs`.

## The cross-format route, exercised for real

The original run of this experiment could only *synthesise* a foreign snapshot by
rewriting a header, because the two published versions (0.0.22 and 0.0.23) both
carried dump format 8. `monty 1.0.0-beta.2`, published 2026-09-21, carries
**format 12** — so the route now runs against genuinely incompatible bytes.

Both refusals are real, and they are not the same error:

```
1.0.0-beta.2 reading a v8 dump  -> VersionTooOld { found: 8, min_supported: 12 }
0.0.23 reading a v12 dump       -> VersionMismatch { found: 12, expected: 8 }
```

`min_supported: 12` is the important half: **1.0 did not merely stop writing
format 8, it stopped reading it.** There is no read-old-write-new compatibility
window to lean on, which is precisely the situation the router exists for. A
deploy that moved 0.0.23 → 1.0 without one would meet `VersionTooOld` on every
parked run.

With the router, all three snapshots resume on the version that wrote them and
keep executing — including a format-8 run in a build that also carries 1.0.

## Sizes

Measured with parallel single-purpose binaries (identical trivial work, once per
linked version) so each marginal is a like-for-like diff. Stripped release,
rustc 1.96.0:

| Binary | Linked interpreters | Bytes |
| --- | --- | --- |
| `only_v23` | 0.0.23 | 8,574,968 |
| `only_v1` | 1.0.0-beta.2 | 9,993,336 |
| `pair_v23_v1` | 0.0.23 + 1.0 | 14,819,272 |
| `trio` | 0.0.22 + 0.0.23 + 1.0 | 18,826,832 |
| `dispatcher` | all three + the full harness | 19,301,944 |

- **Marginal cost of an extra 0.0.x interpreter: 4,007,560 bytes (3.82 MiB)**
  (`trio - pair_v23_v1`), consistent with the 4.05 MiB measured on rustc 1.95.
- **Marginal cost of the 1.0 interpreter: 6,244,304 bytes (5.96 MiB)**
  (`pair_v23_v1 - only_v23`) — **56% more than a 0.0.x one.**

That revises the handoff's bundle budget downward. Against Cloudflare's 64 MiB
uncompressed limit, at ~6 MiB per interpreter the ceiling is **roughly 10
versions**, not the 14–16 that the 0.0.x figure implied. Still comfortably above
the "ten-plus" the handoff wanted, but the margin is gone — if the 1.0 line keeps
growing, the budget needs rechecking before it is relied on. (The wasm figure for
1.0 is not measured; experiment E found native and wasm within 5% for 0.0.23.)

## The MSRV couples every linked version

`monty 1.0.0-beta.2` requires **rustc 1.96** (0.0.23's MSRV was exactly 1.95).
Because all linked versions compile in one crate graph, the whole binary builds
on the newest version's toolchain — so adopting a new Monty forces its MSRV onto
every old interpreter you are keeping around for parked runs.

That worked here: 0.0.22 and 0.0.23 both build cleanly on 1.96.0. But it is a
standing risk in this design that the handoff does not mention. A future Monty
whose MSRV exceeds what an old version can compile under would make those two
unlinkable, and the only way to drain the old runs would be the drain window the
design set out to avoid.

## The dispatcher

Eight bytes, no interpreter involved:

```rust
fn sniff_dump_version(bytes: &[u8]) -> Result<u16, String>  // MONTY\0 + u16 LE
```

A snapshot whose version no linked interpreter claims is reported as exactly that
(`dump v999 has no linked interpreter (this build reads v8, v12)`) rather than
mistaken for corruption, so a retired-version run can be identified and reported
instead of guessed at.

Note the table is keyed by **dump** version, not crate version, and two of the
three linked crates answer to the same key. Where several linked versions read a
format, route to the newest of them (0.0.23, not 0.0.22).

## The caveat the handoff flagged, now confirmed both ways

**Dump version is not crate version.** 0.0.22 and 0.0.23 both report
`DUMP_VERSION = 8`, and a 0.0.22 snapshot was read successfully by the 0.0.23
interpreter. 1.0.0-beta.2 reports 12. So:

1. Not every crate bump strands snapshots — only a format bump does, and old
   interpreters retire far less often than one-per-release would imply.
2. The routing table must map dump versions to interpreters by hand, with
   duplicate keys collapsed.
