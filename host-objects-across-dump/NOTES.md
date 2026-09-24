# Experiment I — host objects held in long-lived guest state

**Reproduce:** `./run.sh` (five processes; the dump is the only thing that
crosses between them)

The handoff's warning, listed as untested:

> the host-side class-instance store is **not** part of a dump. Restoring into a
> fresh process starts with an empty store, so returned host objects become
> proxies and method calls on them raise. **Keep host objects out of long-lived
> guest state.**

The first two sentences are correct. The conclusion is too strong.

## What is and is not in the dump

`stage1` hands the guest a host-defined `Ledger` instance, uses it, and dumps
with the handle still live in a global. The dump is **680 bytes**, and the host
store at exit holds one live object that goes nowhere.

Split by what survives into `stage2`, a fresh process with an empty store:

| Guest expression | Survives with no host object? | Why |
| --- | --- | --- |
| `receipts` (plain guest state) | yes | ordinary heap, fully in the dump |
| `ledger.owner` (eager attr) | yes | eager attrs cross *with* the instance, so they are in the dump |
| `type(ledger).__name__` | yes | the class name travels on the wire type |
| `ledger.limit` (lazy attr) | no — `AttributeError` | routed to the host at access time |
| `ledger.balance()` (method) | no — `NameError` | routed to the host at call time |

The important half is what the trace shows: **the suspension still arrives, and
still carries the right instance id.** The guest-side handle is intact; only the
host's side of the mapping is gone. Nothing is corrupt and nothing is silently
wrong.

Both failures are **catchable in guest code** — a `try/except Exception` around
the call yields `NameError` and the turn continues. So an un-restored handle
degrades a turn, it does not kill a session.

## The host can put the object back

`stage3` is the case the handoff rules out. A fresh process builds a *completely
different* `Ledger` — different balance, different limit, constructed by code
that never saw `stage1` — registers it under the **same instance id**, and loads
the same dump:

```
ledger.owner                  String("ops")   [eager, from the dump]
ledger.limit                  Int(250)        [the NEW object's limit]
ledger.balance()              Int(999)        [the NEW object's balance]
ledger.charge(50)             Int(949)
ledger.charge(10_000)         ValueError: charge of 10000 would breach the limit
```

Run again with a different object (`stage3 ... 4200`) and the same bytes answer
`Int(4200)`. The new object's limit is enforced, so the host really is the one
answering — this is not a cached value from the dump.

This is exactly the result experiment B found for external *functions*: the
snapshot records the identity, never the implementation, and resolution happens
at call time. Host objects behave the same way, keyed by `MontyUuid` instead of
by name.

`stage4` checks the failure mode of getting it wrong: a store holding the right
*kind* of object under a different id fails identically to an empty store
(`NameError`), rather than answering from the wrong object. The id is the
contract, and a mismatch is loud.

## Consequences

1. **Host objects in long-lived guest state are workable.** The rule is not
   "keep them out", it is **"persist the id→object mapping, or be able to
   reconstruct it, beside the snapshot"** — the same discipline the design
   already accepts for tool names. For a Durable Object host the mapping is
   naturally a row in the object's own SQLite, written when the handle is
   created.
2. **Ids must be stable and persisted, not regenerated.** A host that mints
   fresh uuids on wake strands every handle in the snapshot. (This experiment
   uses deterministic ids so a later process can rebind deliberately; a real
   host wants uuid4 plus a stored mapping.)
3. **Put anything the guest should still see after a cold wake into the eager
   attrs**, where it becomes part of the dump, rather than leaving it to a lazy
   lookup.
4. **Answer an unresolvable handle with `Error`, not `NotFound`.** `NotFound`
   produces `NameError: name 'balance' is not defined`, which points at the
   method name and reads like a missing function — misleading when the real
   cause is a missing object. A host should return
   `ExtFunctionResult::Error(MontyException::new(...))` naming the stranded
   handle, so the guest (and the logs) get the actual reason.
5. **A stranded handle is recoverable, so it is worth detecting at wake** rather
   than at first use: the host can reconcile its store against the snapshot when
   it loads, and refuse or rebuild before the guest touches anything.
