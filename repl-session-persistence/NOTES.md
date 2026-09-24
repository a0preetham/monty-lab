# Experiment A — REPL confirmation

**Reproduce:** `cargo run --release` (prints the full transcript and the `Facts` struct
for each question). Verdicts and analysis live in `../FINDINGS.md`; this file records the
API shape as actually found.

## Real API shape (monty 0.0.23)

```rust
MontyRepl::new(script_name: &str, ResourceTracker, CompileOptions) -> Self
feed_run(&mut self, code, inputs: Vec<(String, MontyObject)>, PrintWriter) -> Result<MontyObject, MontyException>
feed_start(self, code, inputs, PrintWriter) -> Result<ReplProgress, Box<ReplStartError>>  // CONSUMES self
call_function(&mut self, name, args, PrintWriter) -> Result<MontyObject, MontyException>
function_names() -> Vec<&str>   // callable globals only — NOT classes
has_function(name) -> bool
heap_entry_count() -> usize     // feature `ref-count-return` only
```

```rust
enum ReplProgress {
    FunctionCall(ReplFunctionCall),   // { function_name, args, kwargs, call_id, object_id }
    OsCall(ReplOsCall),
    ResolveFutures(ReplResolveFutures),
    NameLookup(ReplNameLookup),       // { name }, object_id() for attribute lookups
    Complete { repl: MontyRepl, value: MontyObject },
}
```

Every suspension variant offers `.into_repl()` (abandon snippet, keep session),
`.abort(exc, print)` (raise an *uncatchable* exception), and a `.resume(..)`.
`ReplProgress::into_complete() -> Option<(MontyRepl, MontyObject)>` and
`into_function_call() / into_name_lookup() / into_resolve_futures()` are the ergonomic
destructors. `ReplStartError { pub repl, pub error }` — a raising snippet returns the
session.

Host answers:

```rust
enum ExtFunctionResult { Return(MontyObject), Error(MontyException), Future(u32), NotFound(String) }
enum NameLookupResult  { Value(MontyObject), Undefined, Error(MontyException) }
```

`ReplFunctionCall::resume` takes `impl Into<ExtFunctionResult>`, so a bare `MontyObject`
works. `MontyObject` is **data-only** — no callable variant, which is why `inputs` cannot
carry tools.

Snapshots: `monty::dump(script_name, type_check: Option<&TypeCheckState>, SessionRef) ->
Result<Vec<u8>, postcard::Error>`, `Dump::load(&bytes)` → `Dump { state: Session, .. }`,
where `Session` is `Suspended(ReplProgress)` / the complete case. `DUMP_VERSION = 8`.

## Layout

| File | Question |
| --- | --- |
| `src/q1_session_state.rs` | What survives between `feed_run` calls |
| `src/q2_suspend_resume.rs` | `feed_start` suspend/resume, ownership, dump of a suspended session |
| `src/q3_dynamic_tools.rs` | Per-turn tool sets (issue #190) |
| `src/q4_options.rs` | `CompileOptions` / `ResourceTracker` / `type_check` (issue #872) |

## Surprises worth carrying forward

- `feed_start` consuming `self` is not a hazard: all three exit paths return the session.
- The compiler decides the suspension shape: `foo(...)` on an unknown name → `FunctionCall`;
  bare `foo` → `NameLookup`. Only the latter caches. Route tools through call syntax.
- `function_names()` does not list classes, so it can't enumerate session contents.
- Limits surface as ordinary catchable Python exceptions (`RecursionError`, `TimeoutError`).
