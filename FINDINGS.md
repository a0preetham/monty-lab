# Findings

All seven workstreams (A–G) from the handoff, run against `monty 0.0.23`
(published 2026-09-20) on Rust 1.95.0. Every claim traces to output from code in
this repo; per-experiment reproduction steps are in each `NOTES.md`.

## Verdict

**The design holds.** Every load-bearing assumption was tested and none failed.
Three of the handoff's stated blockers turned out not to apply at all, one new
blocker appeared and has a fix, and the whole thing now runs end to end inside a
Durable Object.

| Handoff claim | Reality on 0.0.23 |
| --- | --- |
| #190: external fn set fixed at session creation; per-turn tool sets "not expressible" | **Does not apply.** Tools resolve at call time, per call. Nothing is declared up front. |
| "Classes not yet definable inside Monty" | **Outdated.** Classes, methods, `isinstance`, `@dataclass` all work and survive snapshots. |
| #872: `type_check` desyncs the namespace | **Unreachable.** There is no `type_check` knob on the Rust API. The open decision does not arise. |
| Wasm viability unknown — "invalidates the whole hosting plan if it fails" | **Passes.** Compiles, runs, and hosts a Durable Object session. |
| Crash isolation without the pool — open question | **Answered, and it matters:** see [the one thing you must do](#the-one-thing-you-must-do). |

### The one thing you must do

**Install `monty-alloc` as the global allocator and arm `max_memory`.** Without
it, a single over-allocating guest snippet traps the wasm instance with
`unreachable` and *nothing in it works again* — not even resetting the session.
With it, the same snippet yields a clean `MemoryError`, the session survives with
state intact, and work continues. `monty-alloc 0.0.23` is published; the handoff
does not mention it.

---

## A. REPL confirmation — `repl-session-persistence/`

Session state persists across feeds: globals, mutated containers, functions
(including redefinition), imports, classes, instances and `@dataclass`. Error
semantics match a CPython REPL — a snippet that binds then raises keeps the
binding; a snippet that fails to compile leaves the session untouched.

`feed_start` consumes the `MontyRepl`, but all three exit paths return it
(`Complete`, `.into_repl()`, `ReplStartError.repl`). No path loses the session.

**Tool sets are dynamic.** Calling an undeclared name suspends as `FunctionCall`;
the host is re-asked on *every* call, so tools can be added, swapped and withdrawn
per turn. (A bare undeclared name suspends as `NameLookup`, which *is* cached —
route tools through call syntax.) `inputs` is real and per-feed but carries data
only: `MontyObject` has no callable variant.

Defaults: `max_recursion_depth` 1000, `max_suspensions` 1000, `max_duration` and
`max_memory` unset. Limits surface as catchable Python exceptions. No heap
introspection without the `ref-count-return` feature.

## B. Snapshot round-trip across processes — `snapshot-cross-process/`

**The gate, and it passes.** A 6,952-byte dump carried globals, functions, a
class and a live instance into a process that never built them; everything worked
and new code could be defined on top.

The suspended leg is the important one: a session parked mid-tool-call was dumped,
its process exited without resolving, and two *different* processes each resumed
the same bytes with different implementations of the same tool name, producing
different results. The dump records the paused function's name, never host code.

Sizes: ~6.9 KB typical, ~74 bytes per dict row, 761 KB at 10,000 rows. Against a
1 GB per-object budget, snapshot size is not a constraint.

## C. Agent loop — `agent-loop/`

Four turns, tool set rebuilt from scratch each turn, session dumped and reloaded
between every one. All passed; a helper defined in turn 1 was still callable after
four hibernations.

Tool failure → catchable `RuntimeError`. Tool withdrawal → catchable `NameError`.
The guest-side API is just Python function calls: no decorator or manifest.

The model is scripted — no API credentials here — so the Python is canned while
everything beneath it is real.

## D. Multi-version linking — `multi-version-linking/`

Two versions in one binary needs only renamed dependencies, because every Monty
release is `0.0.x` and Cargo treats each patch as semver-incompatible. Cross-
version confusion is a compile error.

**Marginal cost per interpreter: 4.05 MiB** (native), matching the 3.87 MiB wasm
figure — roughly 14–16 versions within Cloudflare's 64 MiB limit.

**Dump version is not crate version, confirmed:** 0.0.22 and 0.0.23 both report
`DUMP_VERSION = 8`, and a 0.0.22 snapshot loaded fine on 0.0.23. Good news
(snapshots strand far less often than per-release), with a catch (the routing
table maps dump versions, maintained by hand). Because both published versions
share format 8, a genuine cross-format route could not be exercised.

## E. Wasm target viability — `wasm-target-viability/`

Both `wasm32-unknown-unknown` and `wasm32-wasip1` compile on default features.
Monty supports wasm deliberately — it swaps in `web_time::Instant` for that
target. `getrandom 0.3` needs the `wasm_js` backend configured.

**~3.87 MiB optimized** (1.55 MB gzipped). Nine Python cases run under Node.
Imports are all wasm-bindgen placeholders: no WASI, threads, filesystem or
network.

**`performance.now()` is frozen in Workers** (Spectre mitigation), and Monty's
`max_duration` is backed by it — so guest wall-clock limits will not advance
under Workers. Rely on memory and recursion limits, and on Cloudflare's own CPU
limit. Deduced from the import list plus documented Workers behaviour; not tested
against real Workers.

## F. Durable Object session host — `durable-object-host/`

**Full integration, 23/23 passing** — real Monty in wasm, in a real Durable
Object, under local workerd. One object per run ID (`idFromName`), SQLite
persistence, async tools including two backed by the object's own SQL, eviction
and cold wake, alarm-driven resumption, per-run isolation.

The headline test: a run **parked inside a tool call**, snapshotted at 1,598
bytes, had its memory discarded, and was resumed by a later request straight back
into the suspension. That is "parked on an approval for weeks" working.

The interpreter is exposed to JS as a step machine so the suspension loop lives in
JS and tools can be genuinely async — no JSPI required.

Gotchas found: use `--target web` (not `bundler`) so Workers' `WebAssembly.Module`
import works; and use `serde_wasm_bindgen`'s `json_compatible()` serializer, or
guest dicts arrive as JS `Map` and `JSON.stringify` silently yields `{}`.

## G. Crash recovery under wasm — `crash-recovery-wasm/`

| Guest failure | Surfaces as | Instance afterwards |
| --- | --- | --- |
| Recursion past `max_recursion_depth` | `RecursionError` | alive |
| Recursion 200,000 deep, limit raised | completes normally | alive |
| Over-allocation, **no** `monty-alloc` | **wasm trap `unreachable`** | **permanently dead** |
| Over-allocation, `monty-alloc` armed | `MemoryError` | alive, state intact |

Stack depth is not the risk — Monty's VM keeps frames on the heap, so 200k-deep
recursion costs memory, not stack. The pool's stack-overflow rationale did not
reproduce. Memory exhaustion is the risk, and untreated it is total.

Implementer traps: `#[global_allocator]` must be in the artifact's crate root, not
a linked library; `set_limit` reports "not installed" if nothing has allocated
yet; and `monty-alloc`'s *hard* limit calls `process::exit`, so the soft limit
must fire first.

---

## Consequences for the design

1. **Drop the #190 workaround from scope.** Per-turn tool sets work natively.
2. **`monty-alloc` is mandatory, not optional.** It is the difference between a
   recoverable turn error and a dead isolate.
3. **Don't rely on Monty's wall-clock limit under Workers.** Budget against
   Cloudflare's CPU limit; use memory and recursion limits for the guest.
4. **Snapshot cadence sets the blast radius** of a guest OOM, since recovery means
   discarding the instance and reloading the last snapshot.
5. **Version routing is cheaper than feared.** Shared dump formats across crate
   versions mean fewer forced retirements; ~4 MiB per linked interpreter means
   14–16 fit in a bundle.
6. **`MemoryError` is not catchable by guest code** — the host must handle it.

## Still open

- **No deploy.** Real hibernation, cold-start latency with a ~4 MB module, and
  paid-plan CPU limits are unverified. This environment has no Cloudflare
  credentials, so everything is local workerd.
- **The frozen-timer caveat is deduced, not measured** against real Workers.
- **No real model in the loop** (C) — no API credentials.
- **`u16` name-id exhaustion after ~65k feeds** (monty #895) was not tested; a
  long-lived run will eventually hit it.
- **Host class-instance store is not part of a dump** — the handoff's warning
  about keeping host objects out of long-lived guest state was not exercised.
- **JSPI on Workers** remains unverified, though the step-machine design avoids
  needing it.
