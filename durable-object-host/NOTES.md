# Experiment F — Durable Object session host

**This is the full integration, not a stub.** Real Monty, compiled to wasm,
running inside a real Durable Object under local workerd, with SQLite persistence,
async tools, hibernation, parking and alarms. 23/23 checks pass.

## Reproduce

```bash
npm install
cd wasm && wasm-pack build --target web --release --out-dir pkg && cd ..
npx wrangler dev --local --port 8799     # terminal 1
node test-client.mjs                     # terminal 2
```

Local workerd needs no Cloudflare credentials. It has since also been
**deployed to real Cloudflare** and measured there — see
[Deployed to real Cloudflare](#deployed-to-real-cloudflare).

## Architecture

```
POST /run/:runId/:action
  └─ worker default export: router. idFromName(runId) — the name IS the address.
     └─ AgentRun (Durable Object, SQLite-backed)
        ├─ this.session : warm AgentSession (wasm) or null when cold
        └─ storage.sql  : snapshot | notes | log
```

The router only routes. It never holds an interpreter, because a session that
crossed a worker boundary every turn would undo the warm path — the point the
handoff makes about pinning the version at session creation.

### The step machine

The obvious shape — hand Rust a tool callback — forces every tool to be
synchronous, which is useless when tools are `fetch` and storage reads. So the
interpreter is exposed to JS as a step machine and **the loop lives in JS**:

```js
let step = session.start(code);
while (step.kind === 'tool') {
  const result = await callTool(step.name, step.args);   // genuinely async
  step = session.resume(result);
}
```

Rust keeps the interpreter and the suspension; JS keeps the async. This sidesteps
JSPI entirely, which matters because JSPI availability on Workers is unverified
(experiment E flagged a `__wbindgen_jspi_spawn_poll` import).

## What the tests establish

| # | Property | Result |
| --- | --- | --- |
| 1 | Warm path — consecutive turns reuse the in-memory session | `loadedFrom: "memory"` |
| 2 | Async tools, including two backed by the object's own SQLite | `recall('favourite')['lang']` → `"python"` |
| 3 | A failing tool raises a catchable Python exception | guest caught it |
| 4 | Eviction → cold wake from the object's own storage | `loadedFrom: "snapshot"`, state intact |
| 5 | **Park mid-tool-call, hibernate, resume later** | parked at `await_approval`, evicted, resumed → `"approved: yes-by-cfo"` |
| 6 | Alarm-driven resumption with no inbound request | alarm ran the pending turn |
| 7 | Run IDs are separate objects | second run ID saw none of the first's state |
| 8 | Guest OOM inside the DO | typed `MemoryError` to the host, object survived |

Test 5 is the durable-execution claim end to end: a run suspended *inside* a tool
call was snapshotted (1,598 bytes), the object's memory was discarded, and a later
request resumed the same suspension and ran it to completion. That is the "parked
on an approval for weeks" case working.

## Findings

**A guest dict reached tools as a JS `Map`, not an object.** `serde-wasm-bindgen`
encodes maps as `Map` by default, so `JSON.stringify(args)` silently produced
`{}` — the SQL round-trip test failed with no error anywhere. Fixed by using
`serde_wasm_bindgen::Serializer::json_compatible()`. Worth knowing before anyone
debugs it from the JS side.

**`MemoryError` is not catchable by guest code.** `except MemoryError` does not
run; the host receives a typed turn error instead and the object stays usable.
Guest code cannot defend against its own allocation failure.

**Wasm target matters for Workers.** `--target bundler` emits
`import * as wasm from './x_bg.wasm'`, which Workers cannot satisfy — it resolves
a `.wasm` import to a `WebAssembly.Module`. Use `--target web` and pass the
imported module to init:

```js
import wasmModule from '../wasm/pkg/monty_session_bg.wasm';
await initWasm({ module_or_path: wasmModule });
```

**The version check has a home.** `AgentSession.dumpVersion()` is stored in the
snapshot row, and `ensureSession` compares it on wake. Today a mismatch throws;
that throw is exactly where a multi-version build (experiment D) would dispatch to
the interpreter that can read those bytes.



## The demo UI

`GET /` serves a single self-contained page (`src/demo.js`) — no build step, no
dependencies. Enter a session ID and some Python; submitting runs it as a turn in
that session. An empty session ID mints a new `crypto.randomUUID()` and puts it
in the URL hash, so the session is bookmarkable and can be pasted back later.

It shows the things that make this runtime interesting rather than just the
result: a `loaded from` badge distinguishing **new** / **memory** (warm) /
**snapshot** (cold wake), the turn count, the live snapshot size in bytes, the
tool calls the turn made, and the object's event log. Calling `await_approval(x)`
parks the run and the page offers a resume box — you can close the tab and come
back to it, which is the durable-execution case as a thing you can click.

**The session ID is effectively a bearer token.** It is the Durable Object's
name, so whoever holds it can read and continue that session. That is why new
IDs are full UUIDs rather than anything short or sequential.

Two things this shook out that the test suite did not:

- **`dir()` does not exist in Monty**, so the obvious `"n" in dir()` idiom for
  "increment if defined" fails with `NameError`. `try: n = n + 1 / except
  NameError: n = 1` works and is what the example uses.
- **The turn count was lost on a cold wake.** `evict()` cleared it and
  `ensureSession()` never read it back, though the snapshot row has always
  stored it — so a resumed run reported turn 1 and looked new. The count is now
  restored from the row. No test asserted on `turns`, which is why the suite
  never caught it; the demo displays it, so it showed up immediately.

### A bot check, if the demo goes public

`verifyHuman(request, env, action)` in `worker.js` is the single insertion point,
called on the mutating actions (`turn`, `resume`, `schedule`) only. It currently
returns `null` (allow) and short-circuits when `env.TURNSTILE_SECRET` is unset,
so a Turnstile verification can be dropped in without touching the router.
Nothing is enforced today — see [Safety](../README.md#safety-when-deploying).

## Deployed to real Cloudflare

Deployed 2026-09-24 as `monty-agent-runtime` on the Workers paid plan, and
**23/23 checks pass against real Cloudflare**, not just workerd:
`BASE=https://<worker>.workers.dev node test-client.mjs`. The worker was deleted
after these measurements were taken; nothing is left running.

Bundle as uploaded: **4,494.43 KiB uncompressed, 1,687.66 KiB gzipped** — against
the 64 MiB uncompressed limit, and Cloudflare reported **2 ms Worker Startup
Time** for it. The ~4 MB module the handoff worried about is not a startup
problem.

### Real hibernation happens, and the cold path is the one that runs

The handoff's central claim is that an idle object hibernates and wakes from its
own storage. `evict` only simulated that. Left genuinely idle for **150 seconds**
with no requests, the next turn reported `loadedFrom: "snapshot"` — Cloudflare
discarded the in-memory session on its own, and the run continued from SQLite
with state intact.

Client-side latency, from a machine several hops from the edge (so these include
network RTT, not just Cloudflare time):

| Request | `loadedFrom` | Wall |
| --- | --- | --- |
| first turn on a brand-new object | `new` | 544 ms |
| warm turn | `memory` | 113 ms |
| warm turn | `memory` | 179 ms |
| turn after a forced `evict` | `snapshot` | 139 ms |
| **turn after 150 s idle** | **`snapshot`** | **192 ms** |
| first turn on a second new object, isolate warm | `new` | 353 ms |

### CPU cost per request, read from `wrangler tail`

Server-side timing had to come from `wrangler tail`, because the worker cannot
measure itself — see the frozen clocks below.

| What | Execution model | CPU |
| --- | --- | --- |
| Router (`/run/:id/:action` → `idFromName`) | `stateless` | 0–1 ms |
| First turn on a new object (wasm init + session create + feed + persist) | `durableObject` | **23–25 ms** |
| Turn that woke from a snapshot | `durableObject` | **2–5 ms** |
| Warm turn | `durableObject` | ~1 ms |

**This settles the free-plan question.** The free tier allows 10 ms CPU per
invocation, and **creating a session costs 23–25 ms** — so the free plan cannot
start a run at all, though warm turns and snapshot wakes (1–5 ms) would fit
inside it. "Free tier is prototyping only" is right, and now it has a number and
a reason: it is session creation, not steady-state work, that does not fit.

The router costing ~0–1 ms confirms the design choice of keeping it stateless:
routing is free, and the interpreter never crosses a worker boundary.

### Every clock is frozen inside a request — measured, not deduced

Experiment E deduced from the wasm import list that `performance.now()` is frozen
on Workers and that Monty's `max_duration`, backed by it, therefore cannot fire.
The `timer-probe` action measures it directly: three clocks read either side of
20,000,000 iterations of real CPU work.

| Clock | Local workerd | Real Workers |
| --- | --- | --- |
| `performance.now()` | +47 ms | **0** |
| `Date.now()` | +47 ms | **0** |
| Monty's own `web_time::Instant` | +47 ms | **0** |

All three are frozen, `Date.now()` included — the prediction was right and
understated. The clocks *do* advance between requests (successive probes read
0 ms, 117 ms, 230 ms), which is the documented Spectre mitigation: time moves on
I/O, never during compute.

The consequence, confirmed by `duration-probe`: with `max_duration` armed at
**50 ms**, a 3,000,000-iteration guest loop **ran to completion** with no timeout.
Monty's wall-clock limit is inert on Workers. Rely on `max_memory` and
`max_recursion_depth`, as this host already does.

A second consequence the handoff does not mention: **the host cannot time its own
turns either.** Every in-worker duration the instrumentation collects reads 0 ms.
Per-turn latency and CPU accounting have to come from `wrangler tail`, Workers
Analytics, or a clock outside the request — not from `Date.now()` around the
work.

### The real CPU ceiling, and what it costs

Ramping guest work to find the wall:

| Guest iterations | Outcome |
| --- | --- |
| 1M – 100M | completes (100M took 18.4 s wall) |
| 300M+ | fails |

At the wall, `wrangler tail` reports `outcome: exceededCpu` at **32,500 ms CPU**
with `Error: Durable Object exceeded its CPU time limit and was reset.` So the
enforcing limit is Cloudflare's ~30 s per-invocation CPU budget, not anything in
Monty — consistent with the frozen-clock result, since Monty's own limit can
never fire.

**The object survives it.** After the reset, the next request to the same run id
returned `loadedFrom: "snapshot"` with pre-existing guest state (`survivor`)
intact, and the event log shows a clean `woke from snapshot`. A runaway turn
costs the turn, not the run — the same shape as the guest-OOM result in G, and
direct confirmation that snapshot cadence is what bounds the blast radius.

### Reproducing these numbers

```bash
npx wrangler deploy
BASE=https://<worker>.workers.dev node test-client.mjs      # the 23 checks
BASE=https://<worker>.workers.dev ./deploy-measure.sh       # latency + hibernation
npx wrangler tail --format json                             # server-side CPU
npx wrangler delete                                         # when done
```

`deploy-measure.sh` step 5 waits out a real idle period rather than calling
`evict`, which is what makes the hibernation result meaningful.

### Probes added for these measurements

`timer-probe`, `duration-probe` and `cpu-burn` actions, plus a `max_duration_ms`
argument on the `AgentSession` constructor and an `AgentSession.clockNowMs()`
static that reads the interpreter's own clock. They exist to answer questions
local workerd cannot and are not part of the runtime design — `max_duration` in
particular should stay `undefined` in production, since it cannot work here.

One trap worth recording: `clockNowMs()` is static, so calling it before any
request has initialised the module throws
`Cannot read properties of undefined`. It passed locally only because an earlier
request had already initialised wasm **in the same isolate**. Any static export
on a wasm-bindgen class needs `await initWasm(...)` first, per isolate.

## Not covered

- **Snapshot-per-turn is unconditional here.** A production host would weigh
  snapshot cost against how much work a guest OOM may discard (experiment G).
- **Free plan untested directly.** The deploy was on a paid plan; the free-tier
  conclusion above is drawn from measured CPU cost against the documented 10 ms
  limit, not from an attempt to run there.
- **Idle hibernation was observed at 150 s, not bracketed.** The eviction
  threshold below that was not searched for, and Cloudflare does not commit to
  one.
- **Nothing was parked for days.** The longest real gap tested is 150 s; "parked
  on an approval for weeks" is still an extrapolation from it.
