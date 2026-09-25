# monty-lab

A private experiment repo validating whether a **durable agent runtime** can be
built on [Pydantic Monty](https://github.com/pydantic/monty/) (a minimal, secure
Python interpreter in Rust) hosted in **Cloudflare Durable Objects**, one object
per agent run, with the interpreter snapshotted into that object's own SQLite.

Nine workstreams, A–I. Every claim in here traces to output from code in this
repo — nothing is taken from documentation alone unless it is labelled as such.

**Read [`FINDINGS.md`](FINDINGS.md) first.** It holds the verdicts. This file is
for orientation, setup, and running things.

---

## The question

Can an agent run be parked mid-tool-call — say, on a human approval — have its
memory thrown away, and be resumed weeks later by a different process running
different host code, without a replay journal?

The mechanism under test:

1. **`MontyRepl`** — the Rust interpreter fed code snippet by snippet, with
   globals, functions and classes persisting between snippets.
2. **One Durable Object per run**, its globally unique name *being* the run ID.
   It holds the live interpreter in memory across turns and hibernates when idle.
3. **`monty::dump` / `Dump::load`** into that object's own SQLite. No external
   bucket, no replay.

Warm path: the object stays awake, no serialization per turn. Cold path: the
object wakes, loads bytes from local storage, keeps feeding.

Origin: a handoff document dated 2026-09-20 (`Monty + Durable Objects agent
runtime — handoff`). Experiments A–G come from it directly; H and I were added
to close two caveats it listed as untested.

## Verdict in one paragraph

**The design holds.** The load-bearing claim — park mid-tool-call, discard
memory, resume against a different host — works, and now works inside a real
Durable Object on real Cloudflare. Four of the handoff's stated blockers do not
apply to the versions tested, and one of its prohibitions turned out to be
avoidable. One new blocker was found and fixed (`monty-alloc` is mandatory).
Three costs it does not mention need budgeting for: snapshots grow ~48 bytes per
turn, the 1.0 interpreter costs ~6 MiB linked, and session creation costs
23–25 ms CPU. Details and numbers: [`FINDINGS.md`](FINDINGS.md).

---

## Layout

| Directory | # | Question | Result |
| --- | --- | --- | --- |
| [`repl-session-persistence`](repl-session-persistence/NOTES.md) | A | Does `MontyRepl` hold state across feeds; does suspend/resume work? | yes; tool sets are dynamic |
| [`snapshot-cross-process`](snapshot-cross-process/NOTES.md) | B | Does a dump survive a process boundary and resume against a different host? | yes, including mid-tool-call |
| [`agent-loop`](agent-loop/NOTES.md) | C | Can an agent loop vary its tool set every turn? | yes, no workaround needed |
| [`multi-version-linking`](multi-version-linking/NOTES.md) | D | Several Monty versions in one binary, routed by dump header? | yes, across a real format split |
| [`wasm-target-viability`](wasm-target-viability/NOTES.md) | E | Does Monty compile and run on wasm32 for Workers? | yes, ~3.9 MiB |
| [`durable-object-host`](durable-object-host/NOTES.md) | F | One DO per run ID, SQLite, hibernate, alarms | yes — 23/23, deployed and measured |
| [`crash-recovery-wasm`](crash-recovery-wasm/NOTES.md) | G | What does a guest crash do to the wasm instance? | fatal without `monty-alloc`; fine with it |
| [`name-id-exhaustion`](name-id-exhaustion/NOTES.md) | H | Is monty #895's ~65k-feed ceiling real? | no — but snapshots grow ~48 B/turn |
| [`host-objects-across-dump`](host-objects-across-dump/NOTES.md) | I | Do host objects in guest state survive a dump? | handle survives; the host can rebind it |

Each experiment is a **standalone crate with its own `Cargo.toml` and
`Cargo.lock`** — deliberately *not* a Cargo workspace. That is what lets D and H
pin different, mutually incompatible `monty` versions while the rest stay on one,
and it means you can break one experiment without touching the others. Each has
a `[workspace]` stanza to opt out of any parent workspace.

Every directory carries a `NOTES.md` with its reproduction commands, the API
shape as actually found, and the traps hit on the way.

```
monty-lab/
├── FINDINGS.md          ← verdicts, all nine workstreams
├── README.md            ← you are here
├── <experiment>/
│   ├── NOTES.md         ← reproduction + API shape as found
│   ├── Cargo.toml       ← own crate, own lock, opts out of workspaces
│   └── src/
└── durable-object-host/
    ├── src/worker.js    ← the Durable Object itself
    ├── wasm/            ← the step-machine wasm binding (its own crate)
    ├── test-client.mjs  ← the 23 checks
    └── deploy-measure.sh ← latency + hibernation, against a deployed worker
```

---

## Setup

### Rust — two toolchains, on purpose

```bash
rustup toolchain install 1.95.0    # monty 0.0.x MSRV is exactly 1.95
rustup toolchain install 1.96.0    # monty 1.0.0-beta.2 requires 1.96
rustup target add wasm32-unknown-unknown --toolchain 1.95.0
```

Experiments A, B, C, E, F, G and I build on **1.95.0**. D and H link the 1.0 line
alongside 0.0.x and therefore need **1.96.0** — prefix those with
`rustup run 1.96.0`.

This split is not incidental; see *the MSRV couples every linked version* in
[D's notes](multi-version-linking/NOTES.md). Any experiment that links a new
Monty forces that Monty's MSRV onto every other version in the same binary.

### Node and tooling

```bash
# Node 22, then:
cargo install wasm-pack          # 0.15.0 used here
cd durable-object-host && npm install   # wrangler 4.135 is a devDependency
```

`wrangler` is local to `durable-object-host`, so invoke it as `npx wrangler`
from inside that directory.

### Versions under test

| Crate | Versions | Dump format |
| --- | --- | --- |
| `monty` | 0.0.22, 0.0.23 | 8 |
| `monty` | 1.0.0-beta.2 | 12 |
| `monty-alloc` | 0.0.23 | — |

Note **dump format is not crate version**: 0.0.22 and 0.0.23 interoperate.
1.0.0-beta.2 refuses format 8 outright (`VersionTooOld { min_supported: 12 }`).

---

## Running the experiments

From each experiment's own directory:

| # | Directory | Command |
| --- | --- | --- |
| A | `repl-session-persistence` | `cargo run --release` |
| B | `snapshot-cross-process` | `./run.sh` (each stage is a separate process) |
| C | `agent-loop` | `cargo run --release` |
| D | `multi-version-linking` | `rustup run 1.96.0 cargo run --release --bin dispatcher` |
| E | `wasm-target-viability` | `cargo build --release --target wasm32-unknown-unknown`, then `wasm-pack build --target nodejs --release --out-dir pkg && node run_node.mjs` |
| F | `durable-object-host` | see [below](#f-the-durable-object-host) |
| G | `crash-recovery-wasm` | `wasm-pack build --target nodejs --release --out-dir pkg`, then `node harness.mjs` (the trap) and `node harness2.mjs` (the fix) |
| H | `name-id-exhaustion` | `rustup run 1.96.0 cargo run --release` |
| I | `host-objects-across-dump` | `./run.sh` (five processes) |

D also builds size baselines — `only_v23`, `only_v1`, `pair_v23_v1`, `trio` —
whose stripped sizes are the per-interpreter marginal cost. G's
`--bin native_check` is the native control for the wasm result.

### F, the Durable Object host

`GET /` serves a **demo UI**: enter a session ID (empty mints a new one) and some
Python, and it runs as a turn in that session, showing whether the interpreter
came from memory or from a cold snapshot, the turn count, the snapshot size, and
a resume box when a run parks. See
[the demo section](durable-object-host/NOTES.md#the-demo-ui).

Locally, against workerd — no Cloudflare credentials needed:

```bash
cd durable-object-host
npm install
cd wasm && wasm-pack build --target web --release --out-dir pkg && cd ..
npx wrangler dev --local --port 8799     # terminal 1
node test-client.mjs                     # terminal 2  -> 23 passed, 0 failed
```

Against real Cloudflare (this **publishes a public endpoint that executes guest
Python with no auth** — see [Safety](#safety-when-deploying)):

```bash
npx wrangler deploy
BASE=https://<worker>.workers.dev node test-client.mjs      # the same 23 checks
BASE=https://<worker>.workers.dev ./deploy-measure.sh       # latency + real hibernation
npx wrangler tail --format json                             # server-side cpuTime
npx wrangler delete                                         # tear it down
```

`deploy-measure.sh` step 5 waits out a real idle period instead of calling
`/evict`, which is what makes the hibernation result mean anything.

**Read timings from `wrangler tail`, not from inside the worker.** Every clock is
frozen within a request on real Workers — `performance.now()`, `Date.now()`, and
Monty's own — so in-worker instrumentation reports 0 ms. This is measured, not
theoretical.

---

## Traps that cost time

Collected from all nine; each is written up where it was found.

- **`monty-alloc` is not optional.** Without it installed as the global
  allocator, one over-allocating guest snippet traps the wasm instance with
  `unreachable` and *nothing in it works again*. With it, the same snippet gives
  a clean `MemoryError` and the session survives. (G)
- **`#[global_allocator]` must be in the artifact's crate root**, not in a
  linked library, or it is silently not installed. `set_limit` reports "not
  installed" if nothing has allocated yet. `monty-alloc`'s *hard* limit calls
  `process::exit`, so the soft limit must fire first. (G)
- **Use `wasm-pack --target web`, not `bundler`**, or Workers'
  `WebAssembly.Module` import path does not work. (F)
- **Use `serde_wasm_bindgen`'s `json_compatible()` serializer**, or guest dicts
  arrive in JS as `Map` and `JSON.stringify` silently yields `{}`. (F)
- **Statics on a wasm-bindgen class need `await initWasm(...)` first**, per
  isolate. A static called before any request initialised the module throws
  `Cannot read properties of undefined`. This passes locally when an earlier
  request happened to warm the same isolate. (F)
- **Route tools through call syntax, not bare names.** An undeclared *call*
  suspends as `FunctionCall` and the host is re-asked every time; a bare
  undeclared *name* suspends as `NameLookup`, which **is** cached. Only
  constants belong in the latter. (A)
- **Answer an unresolvable host object with `Error`, not `NotFound`.**
  `NotFound` produces `NameError: name 'balance' is not defined`, which points at
  the method and reads like a missing function. (I)
- **`getrandom 0.3` needs the `wasm_js` backend** configured for wasm targets. (E)
- **Monty's `max_duration` cannot fire on Workers.** Leave it unset; rely on
  `max_memory` and `max_recursion_depth`. (E, F)

## Safety when deploying

`durable-object-host` has **no authentication**: `POST /run/:runId/turn {code}`
executes guest Python, and the demo UI at `GET /` is a front-end for exactly
that. `verifyHuman()` in `worker.js` is where a Turnstile check belongs; it is a
no-op today. Monty's sandbox has no network or filesystem access
(experiment E — every import is a wasm-bindgen placeholder), so the exposure is
your Workers CPU/request quota and the objects' SQLite storage rather than data
egress. Still, a deployed worker is an open compute endpoint: gate it behind a
shared secret, or deploy only for as long as a measurement takes and then
`npx wrangler delete`. The run recorded in `FINDINGS.md` took the latter route.

Durable Objects bill **wall-clock duration at 128 MB** while an object is in
memory and unable to hibernate, so holding a long outbound fetch inside a turn
costs duration even though it costs almost no CPU. Park instead. (Doc-derived;
not yet measured here.)

---

## What is proven, and what is not

Proven, end to end: REPL semantics across feeds; snapshot round-trip across
processes including mid-tool-call against a *different* host implementation;
per-turn tool sets; multi-version routing across a genuine dump-format split;
wasm on Workers; the full Durable Object integration deployed to real Cloudflare
with real hibernation observed; guest-crash recovery; no turn ceiling; host
objects rebindable by id.

Still open — the honest list, expanded in `FINDINGS.md`:

- **No real model in the loop.** C's Python is scripted. Whether a model writes
  Monty-subset Python reliably is untested, and it decides whether this is an
  agent harness or a retry machine. **Highest-value next experiment.**
- **No proven compaction path** for the ~48 B/turn snapshot growth. A reload does
  not compact it (measured at exactly 2.01x). Re-materializing globals into a
  fresh session is the obvious candidate and is unexercised.
- **Longevity is extrapolated.** Hibernation is confirmed at 150 s idle, not at
  weeks, and parked-mid-tool-call plus *natural* hibernation was never tested in
  combination — only each half separately.
- **The MSRV coupling** is a latent structural risk to version linking, not a
  present blocker.
- **No in-request guard on CPU-bound guest code**, because the clocks are frozen.
  I/O waits *can* be bounded — timers advance across awaits.
- **Free plan untested directly**; the conclusion is measured CPU cost against
  the documented 10 ms limit.
- **JSPI on Workers** unverified, though the step-machine design avoids needing
  it.
- **1.0 is not wasm-measured**; D's ~6 MiB figure is native.

## Adding an experiment

The conventions that keep this repo readable:

1. **New top-level directory, own crate, own lock, `[workspace]` stanza** to opt
   out of any parent workspace. Pin `monty` with `=` so a result stays
   attributable to a version.
2. **A `NOTES.md`** with a `**Reproduce:**` line at the top, the API shape as
   actually found, and any trap hit. Notes record what happened; verdicts go in
   `FINDINGS.md`.
3. **Print evidence, exit non-zero on failure.** Every harness here ends in
   `ALL CHECKS PASSED` / `SOME CHECKS FAILED` or a pass count, so a run is
   self-verifying rather than needing interpretation.
4. **If a process boundary is part of the claim, use a real one** — a `run.sh`
   driving separate invocations, as in B and I. In-process "fresh" state is not
   the same thing and the distinction has mattered here.
5. **Add a row to the table above and a section to `FINDINGS.md`**, and letter it
   next in sequence (J onward).
6. **Label doc-derived claims as such.** The value of this repo is that
   measurements and documentation are distinguishable.

## Provenance

- Handoff document: 2026-09-20.
- A–G first run: 2026-09-20, `monty 0.0.23`, rustc 1.95.0, local workerd only.
- D extended, H and I added, F deployed to real Cloudflare and measured:
  2026-09-24, adding `monty 1.0.0-beta.2` and rustc 1.96.0. The deployed worker
  was deleted afterwards.
