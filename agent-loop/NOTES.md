# Experiment C — agent loop over MontyRepl

**Reproduce:** `cargo run --release`

The loop, tool registry, suspension handling and per-turn snapshot are real. The
*model* is scripted — there are no API credentials in this environment — so the
Python each turn is canned rather than generated. This establishes the host's
shape, not model behaviour.

## What it does

Four turns on one session, with the tool set **rebuilt from scratch every turn**:

| Turn | Tools offered | Point |
| --- | --- | --- |
| 1 | `search` | Model defines a helper and calls a tool |
| 2 | `fetch_rows`, `today` | `search` is gone, two new tools appear; turn 1's helper is reused |
| 3 | `fetch_rows`, `commit_total` | Same tool name, now a *failing* implementation |
| 4 | *(none)* | Every tool withdrawn |

Between every turn the session is dumped to bytes, dropped entirely, and reloaded
— the Durable Object hibernation cycle.

## Results

All four turns passed. After four turns and four hibernations:

```
total              -> 600
stamp              -> "2026-09-20"
source             -> "cached: upstream 503"
len(findings)      -> 2
summarize([1,2,3]) -> {'n': 3, 'first': 1}      # helper defined in turn 1
```

Snapshots grew 481 → 970 → 1,267 → 1,512 bytes.

## What it settles

- **Varying tool sets need no workaround.** Nothing is registered with the
  interpreter in advance; the host answers each suspension as it arrives.
- **Tool failures are ordinary Python exceptions.** `ExtFunctionResult::Error` let
  turn 3's guest code `except RuntimeError` and fall back to cached state.
- **Withdrawing a tool is expressible.** `NotFound` gives the guest a catchable
  `NameError`, so turn 4 degraded instead of crashing.
- **The guest-side API is just Python.** Tools are bare function calls — no
  decorator, import or manifest for the model to get wrong.

## Design note

`AgentSession` holds `Option<MontyRepl>` because `feed_start` consumes the repl: a
suspended session genuinely holds none. Every exit path puts one back, so the
`Option` is never empty across a turn boundary.

## Not covered

A real model in the loop, and token-level concerns (how much of the session to
describe back to the model each turn). Both need API access this environment
does not have.
