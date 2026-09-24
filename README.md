# monty-lab

Experiments validating the Monty + Durable Objects agent runtime handoff
(2026-09-20). Nine workstreams: A–G from the handoff itself, plus H and I, which
close two of its untested caveats.

**Start with [`FINDINGS.md`](FINDINGS.md).** Each experiment has its own
`NOTES.md` with reproduction commands and the API shape as actually found.

Each experiment is a standalone crate with its own `Cargo.toml` and `Cargo.lock`
— deliberately not a Cargo workspace, so experiments can pin different `monty`
versions and be worked on independently.

| Directory | # | Question | Result |
| --- | --- | --- | --- |
| [`repl-session-persistence`](repl-session-persistence/NOTES.md) | A | Does `MontyRepl` hold state across feeds; does suspend/resume work? | yes; tool sets are dynamic |
| [`snapshot-cross-process`](snapshot-cross-process/NOTES.md) | B | Does a dump survive a process boundary and resume against a different host? | yes, including mid-tool-call |
| [`agent-loop`](agent-loop/NOTES.md) | C | Can an agent loop vary its tool set every turn? | yes, no workaround needed |
| [`multi-version-linking`](multi-version-linking/NOTES.md) | D | Several Monty versions in one binary, routed by dump header? | yes — and now across a real format split |
| [`wasm-target-viability`](wasm-target-viability/NOTES.md) | E | Does Monty compile and run on wasm32 for Workers? | yes, ~3.9 MiB |
| [`durable-object-host`](durable-object-host/NOTES.md) | F | One DO per run ID, SQLite, hibernate, alarms | yes — 23/23, full integration |
| [`crash-recovery-wasm`](crash-recovery-wasm/NOTES.md) | G | What does a guest crash do to the wasm instance? | fatal without `monty-alloc`; fine with it |
| [`name-id-exhaustion`](name-id-exhaustion/NOTES.md) | H | Is monty #895's ~65k-feed ceiling real? | no — but snapshots grow ~48 B/turn forever |
| [`host-objects-across-dump`](host-objects-across-dump/NOTES.md) | I | Do host objects in guest state survive a dump? | handle survives; the host can rebind it |

## The short version

The design holds. Four of the handoff's stated blockers (#190 fixed tool sets,
undefinable classes, the #872 `type_check` bug, and #895's turn ceiling) do not
apply. One prohibition it states — keep host objects out of long-lived guest
state — turns out to be unnecessary if ids are persisted.

One new blocker was found and fixed: **without `monty-alloc` installed as the
global allocator, a single over-allocating guest snippet permanently kills the
wasm instance.**

Three costs are worth budgeting for, none of them in the handoff: **snapshots
grow about 48 bytes per turn regardless of state** (H), the **1.0 interpreter
costs ~6 MiB linked rather than ~4 MiB**, cutting the bundle ceiling to roughly
10 versions (D), and **session creation costs 23–25 ms CPU against 1–5 ms for
every other kind of turn** (F), which is what rules out the free plan.

`durable-object-host/` runs the whole thing end to end — real Monty in wasm,
inside a real Durable Object under local workerd, with a run parked mid-tool-call
across a hibernation and resumed later.

## Environment

Node 22, local workerd via wrangler 4.135, and two Rust toolchains:

- **1.95.0** — Monty 0.0.x's MSRV is exactly 1.95. Experiments A, B, C, E, F, G
  and I build on it.
- **1.96.0** — required by `monty 1.0.0-beta.2`. Experiments D and H link the
  1.0 line alongside 0.0.x, so they need it: `rustup run 1.96.0 cargo run ...`.

Versions under test: `monty` 0.0.22, 0.0.23 and 1.0.0-beta.2.

Experiment F has been **deployed to real Cloudflare and measured there** (paid
plan, 2026-09-24, 23/23 passing), then deleted — see
[`durable-object-host/NOTES.md`](durable-object-host/NOTES.md#deployed-to-real-cloudflare).
Hibernation, cold-wake cost, the frozen clocks and the CPU ceiling are all
measured rather than deduced. See the "Still open" section of `FINDINGS.md` for
what remains.
