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

No Cloudflare credentials are needed or used — everything runs against local
workerd. Nothing here has been deployed.

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

## Not covered

- **No deploy.** Real hibernation, cold-start latency with a ~4 MB wasm module,
  and the paid-plan CPU limits are all unverified; `evict` simulates eviction by
  dropping in-memory state, which is faithful to the documented model but is not
  the real thing.
- **`performance.now()` is frozen on Workers** (experiment E). Monty's
  `max_duration` is backed by it, so guest wall-clock limits will not advance.
  This host therefore relies on `max_memory` and `max_recursion_depth`, not time.
  Untested against real Workers.
- **Snapshot-per-turn is unconditional here.** A production host would weigh
  snapshot cost against how much work a guest OOM may discard (experiment G).
