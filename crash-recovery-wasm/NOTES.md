# Experiment G — crash recovery semantics under wasm

**Bottom line: in-process crash isolation is survivable, but only if you install
`monty-alloc` and arm `max_memory`. Without it, one over-allocating guest
permanently kills the wasm instance.**

## Reproduce

```bash
wasm-pack build --target nodejs --release --out-dir pkg
node harness.mjs     # no allocator limit -> the trap
node harness2.mjs    # monty-alloc armed  -> catchable MemoryError
cargo run --release --bin native_check    # native control
```

## The question

Pydantic recommend `monty-pool` over the in-process API because a stack overflow
or allocator abort kills only a worker process. On Workers there is no pool — the
interpreter shares the wasm instance with the host — so what does a guest failure
do to that instance?

## Results

The module keeps ONE long-lived `MontyRepl` in a thread-local, as a Durable Object
would, and the harness attacks it.

| Guest failure | Surfaces as | Instance afterwards |
| --- | --- | --- |
| Recursion past `max_recursion_depth` | `RecursionError`, catchable | alive |
| Recursion 200,000 deep with the limit raised | *completes normally* | alive |
| Over-allocation, **no** `monty-alloc` | **wasm trap `unreachable`** | **permanently dead** |
| Over-allocation, `monty-alloc` armed | `MemoryError`, clean host error | alive, state intact |
| Unbounded tool-calling loop | host-side call cap | alive |

### Stack depth is not the risk

Recursing 200,000 frames deep did not overflow the wasm stack — Monty's VM keeps
frames on the heap, so deep recursion costs linear memory (9.1 → 27.2 MiB) rather
than stack. The stack-overflow scenario that motivates the pool did not reproduce.

### Memory exhaustion is the risk, and it is total

Without a tracking allocator, `big = [0] * 200_000_000` traps the instance with
`unreachable`. Everything afterwards traps too — including `session_alive()` and
`reset_session()`. There is no in-process recovery: the instance must be
discarded and a fresh one created. On Workers that means the isolate is torn down
and the run resumes from its last snapshot, so **snapshot cadence sets how much
work a guest OOM costs you**.

### `monty-alloc` fixes it

`monty-alloc 0.0.23` is published (it is not mentioned in the handoff). Installed
as the global allocator and armed via `set_limit`, Monty checks a soft limit at
its own checkpoints and raises before the allocator's hard ceiling:

```
MemoryError: memory limit exceeded: 3200003143 bytes > 67108864 bytes
session_alive: true
marker       : String("pre-oom")     # pre-failure state intact
```

A second over-allocation on the same session behaved identically, and normal work
continued afterwards. Verified both natively and in wasm.

## Two traps for the implementer

1. **`#[global_allocator]` must be declared in the crate root that becomes the
   artifact.** Declaring it in a library that a binary links does nothing — the
   native control initially reported "monty-alloc is not installed" for exactly
   this reason. For a `cdylib` (the wasm case) `lib.rs` is the right place.

2. **`LIVE_MEMORY == 0` does not mean the allocator is missing.** `set_limit`
   detects installation by checking that counter, so calling it before anything
   has allocated returns "not installed as the global allocator" even when it is.
   A lazy `thread_local!` session is enough to trigger this false negative. Force
   one allocation first.

3. **`monty-alloc`'s *hard* limit calls `process::exit`.** That suits the pool's
   worker model; in a shared wasm instance it is fatal. The soft limit has to fire
   first, so leave headroom between the guest ceiling and the isolate's 128 MB.

## Caveat

`MemoryError` is **not catchable by guest code** — `except MemoryError` does not
run. It surfaces to the host as a failed turn, which is arguably the right
behaviour, but guest code cannot defend against its own allocation failure. This
was confirmed in experiment F inside a real Durable Object.
