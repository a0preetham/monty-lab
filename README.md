# monty-lab

Experiments validating the Monty + Durable Objects agent runtime handoff
(2026-09-20). All seven workstreams A–G, against `monty 0.0.23`.

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
| [`multi-version-linking`](multi-version-linking/NOTES.md) | D | Two Monty versions in one binary, routed by dump header? | yes, ~4 MiB per version |
| [`wasm-target-viability`](wasm-target-viability/NOTES.md) | E | Does Monty compile and run on wasm32 for Workers? | yes, ~3.9 MiB |
| [`durable-object-host`](durable-object-host/NOTES.md) | F | One DO per run ID, SQLite, hibernate, alarms | yes — 23/23, full integration |
| [`crash-recovery-wasm`](crash-recovery-wasm/NOTES.md) | G | What does a guest crash do to the wasm instance? | fatal without `monty-alloc`; fine with it |

## The short version

The design holds. Three of the handoff's stated blockers (#190 fixed tool sets,
undefinable classes, the #872 `type_check` bug) do not apply to `monty 0.0.23`'s
Rust API at all. One new blocker was found and fixed: **without `monty-alloc`
installed as the global allocator, a single over-allocating guest snippet
permanently kills the wasm instance.**

`durable-object-host/` runs the whole thing end to end — real Monty in wasm,
inside a real Durable Object under local workerd, with a run parked mid-tool-call
across a hibernation and resumed later.

## Environment

Rust 1.95.0 (Monty's MSRV is exactly 1.95), Node 22, local workerd via wrangler
4.135. **No Cloudflare credentials** — nothing has been deployed; see the
"Still open" section of `FINDINGS.md` for what that leaves unverified.
