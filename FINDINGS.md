# Findings

Nine workstreams: A–G from the handoff, plus H and I, added to close two of its
untested caveats. Run against `monty` 0.0.22, 0.0.23 and 1.0.0-beta.2 on Rust
1.95.0 and 1.96.0. Every claim traces to output from code in this repo;
per-experiment reproduction steps are in each `NOTES.md`.

## Verdict

**The design holds.** Every load-bearing assumption was tested and none failed.
Four of the handoff's stated blockers turned out not to apply at all, one of its
prohibitions turned out to be avoidable, one new blocker appeared and has a fix,
and the whole thing now runs end to end inside a Durable Object.

What the handoff does not account for is cost rather than correctness: snapshots
grow with turn count, and the 1.0 interpreter is half again as large as a 0.0.x
one.

| Handoff claim | Reality on 0.0.23 |
| --- | --- |
| #190: external fn set fixed at session creation; per-turn tool sets "not expressible" | **Does not apply.** Tools resolve at call time, per call. Nothing is declared up front. |
| "Classes not yet definable inside Monty" | **Outdated.** Classes, methods, `isinstance`, `@dataclass` all work and survive snapshots. |
| #872: `type_check` desyncs the namespace | **Unreachable.** There is no `type_check` knob on the Rust API. The open decision does not arise. |
| Wasm viability unknown — "invalidates the whole hosting plan if it fails" | **Passes.** Compiles, runs, and hosts a Durable Object session. |
| #895: `u16` name-id exhaustion after ~65k feeds | **Not reachable** from the Rust API — 300,000 feeds, clean, on both lines. But it has a cousin: see [H](#h-the-turn-ceiling-that-isnt--name-id-exhaustion). |
| "Keep host objects out of long-lived guest state" | **Too strong.** The handle survives the dump and the host can rebind it by id; see [I](#i-host-objects-across-a-dump--host-objects-across-dump). |
| "this experiment could not exercise a genuine cross-format route" (D, first run) | **Now exercised.** 1.0.0-beta.2 carries dump format 12 against 0.0.x's 8. |
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

Several versions in one binary needs only renamed dependencies, because Cargo
treats each `0.0.x` patch — and `1.0.0-beta.2` — as its own semver-incompatible
range. Cross-version confusion is a compile error. Three are linked here: 0.0.22,
0.0.23 and 1.0.0-beta.2.

**The cross-format route is now exercised for real.** The first run of this
experiment could only synthesise a foreign snapshot by rewriting a header, since
both published versions carried dump format 8. `monty 1.0.0-beta.2` (published
2026-09-21) carries format **12**, so each interpreter can be handed the other's
actual bytes. Both refuse, and not identically:

```
1.0.0-beta.2 reading a v8 dump  -> VersionTooOld { found: 8, min_supported: 12 }
0.0.23 reading a v12 dump       -> VersionMismatch { found: 12, expected: 8 }
```

`min_supported: 12` is the load-bearing detail: **1.0 did not merely stop writing
format 8, it stopped reading it.** There is no compatibility window to lean on —
a deploy from 0.0.23 to 1.0 without a router meets `VersionTooOld` on every
parked run. With the router, all three snapshots resume on the version that wrote
them and keep executing.

**Costs, measured with like-for-like binaries:** a 0.0.x interpreter adds
**3.82 MiB** (consistent with the 4.05 MiB measured on rustc 1.95), but the 1.0
interpreter adds **5.96 MiB — 56% more**. That revises the bundle budget from
14–16 versions down to **roughly 10** within Cloudflare's 64 MiB limit. Still
above the "ten-plus" the handoff wanted, but the margin is gone.

**The MSRV couples every linked version.** `monty 1.0.0-beta.2` requires rustc
1.96 where 0.0.23 required exactly 1.95, and because all linked versions share
one crate graph the whole bundle builds on the newest one's toolchain. It worked
here — 0.0.22 and 0.0.23 both build clean on 1.96.0 — but adopting a new Monty
forces its MSRV onto every old interpreter being kept for parked runs. A future
release whose MSRV exceeds what an old version can compile under would make them
unlinkable, and the only way out would be the drain window this design exists to
avoid.

**Dump version is not crate version, confirmed both ways:** 0.0.22 and 0.0.23
both report `DUMP_VERSION = 8` and interoperate; 1.0.0-beta.2 reports 12. So
snapshots strand far less often than per-release — but the routing table is keyed
by dump version, with duplicate keys collapsed, and maintained by hand. Where
several linked versions read a format, route to the newest of them.

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

## H. The turn ceiling that isn't — `name-id-exhaustion/`

monty [#895](https://github.com/pydantic/monty/issues/895) — every `feed_run`
interns a script name into a `u16` space, so "long-lived sessions with many turns
will hit this" — **is not reachable from the Rust API**. 300,000 feeds, 4.5x the
theoretical 65,535 ceiling, no failure, on both 0.0.23 and 1.0.0-beta.2. Not a
caught error or a slow degradation: nothing happens. The turn ceiling can come
off the risk list.

Pushing the feed count turned up the real cost, which the handoff does not
mention. With guest state held constant at one small dict:

| feeds | dump bytes | bytes/feed | dump ms | load ms |
| --- | --- | --- | --- | --- |
| 100 | 4,338 | 43.4 | 0.0 | 0.1 |
| 1,000 | 43,943 | 43.9 | 0.1 | 0.3 |
| 10,000 | 457,945 | 45.8 | 0.4 | 2.1 |
| 100,000 | 4,777,951 | 47.8 | 10.7 | 29.8 |

**About 48 bytes of permanent snapshot per turn, linear and unbounded**, on both
versions, identically. Experiment B's ~6.9 KB headline measures *state*; this is
the other axis, and for a long run it dominates. A 100,000-turn run carries a
4.8 MB snapshot containing, as far as the guest is concerned, nothing — and pays
~11 ms to dump and ~30 ms to load it on **every** hibernation and **every** cold
wake.

**A reload does not compact it.** 40,000 feeds dumped to 1,897,953 bytes;
reloading and feeding 40,000 more gave 3,818,006 — exactly 2.01x. The
accumulation is in the dump, so loading brings the whole history along. The only
reset is abandoning the session, which is the replay journal the design rejected.

Ordinary runs are unaffected: 1,000 turns is a 44 KB snapshot that loads in
0.3 ms. The concern starts in the tens of thousands.

## I. Host objects across a dump — `host-objects-across-dump/`

The handoff says the host-side class-instance store is not part of a dump, so
restored host objects become dead proxies, and concludes: *keep host objects out
of long-lived guest state.* The premise is right and **the conclusion is too
strong.**

Five processes, sharing only a 680-byte dump. With an empty store on the far
side, eager attributes (`ledger.owner`) and the class name still answer from the
dump, while a method call raises `NameError` and a lazy attribute raises
`AttributeError` — **both catchable in guest code**, so a stranded handle
degrades a turn rather than killing a session. Crucially the suspension still
arrives carrying the correct instance id: the guest-side handle is intact, and
only the host's half of the mapping is missing.

**So the host can put it back.** A fresh process built an entirely different
`Ledger` — different balance and limit, by code that never saw the original —
registered it under the same instance id, loaded the same bytes, and the guest's
`ledger` worked completely, including the *new* object's limit being enforced.
Run again with another object and the same bytes answer differently. This is
experiment B's result for external functions, restated for objects: the snapshot
records identity, never implementation, and resolution happens at call time —
keyed by `MontyUuid` rather than by name.

A store holding the right kind of object under the *wrong* id fails exactly like
an empty store rather than answering from the wrong object. The id is the
contract, and a mismatch is loud.

One implementer trap: answering an unresolvable handle with
`ExtFunctionResult::NotFound` produces `NameError: name 'balance' is not
defined`, which points at the method and reads like a missing function. Return
`Error(MontyException::new(...))` naming the stranded handle instead.

---

## Consequences for the design

1. **Drop the #190 workaround from scope.** Per-turn tool sets work natively.
2. **`monty-alloc` is mandatory, not optional.** It is the difference between a
   recoverable turn error and a dead isolate.
3. **Don't rely on Monty's wall-clock limit under Workers.** Budget against
   Cloudflare's CPU limit; use memory and recursion limits for the guest.
4. **Snapshot cadence sets the blast radius** of a guest OOM, since recovery means
   discarding the instance and reloading the last snapshot.
5. **Version routing is cheaper than feared, but not free.** Shared dump formats
   across crate versions mean fewer forced retirements. Budget ~6 MiB per linked
   1.0-era interpreter, so roughly 10 per bundle — and expect each new Monty to
   impose its MSRV on every version linked beside it.
6. **`MemoryError` is not catchable by guest code** — the host must handle it.
7. **Budget snapshot size against turn count, not state size** (~48 B/turn). At
   1 GB per object the wall is ~21 million turns, so storage is not the
   constraint; cold-wake latency and per-hibernation write volume are. A turn
   budget per run now has a cost curve attached rather than a cliff.
8. **Host objects may be held in long-lived guest state** provided the host
   persists the instance-id→object mapping beside the snapshot — for a Durable
   Object, a row in its own SQLite. Ids must be stable, never regenerated on
   wake. Put anything that must survive a cold wake unattended into the eager
   attrs, where it lands in the dump.

## Still open

- **No deploy.** Real hibernation, cold-start latency with a ~4 MB module, and
  paid-plan CPU limits are unverified. This environment has no Cloudflare
  credentials, so everything is local workerd.
- **The frozen-timer caveat is deduced, not measured** against real Workers.
- **No real model in the loop** (C) — no API credentials.
- **JSPI on Workers** remains unverified, though the step-machine design avoids
  needing it.
- **The 1.0 line is not wasm-measured.** D's ~6 MiB figure is native; E found
  native and wasm within 5% for 0.0.23, but that was not rechecked for 1.0.
- **Nothing above 1.0.0-beta.2 exists yet**, so a second genuine format bump —
  and whether `min_supported` keeps climbing — is unobserved.
