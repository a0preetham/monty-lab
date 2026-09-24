# Experiment D — two Monty versions in one binary

**Reproduce:** `cargo run --release --bin dispatcher`

## Linking

Every Monty release is `0.0.x`, and Cargo treats each `0.0.x` patch as its own
semver-incompatible range. So this is all the manifest needs:

```toml
monty_v22 = { package = "monty", version = "=0.0.22" }
monty_v23 = { package = "monty", version = "=0.0.23" }
```

Both compile into one binary. They are wholly separate crates — separate types,
separate statics, distinct symbols — so handing a `monty_v22::MontyRepl` to a
`monty_v23` function is a compile error rather than a runtime corruption. That
safety property is recorded as a `compile_fail` doctest in `src/main.rs`.

## Sizes

| Binary | Stripped release |
| --- | --- |
| `only_v23` (one interpreter) | 8,600,536 |
| `dispatcher` (two interpreters) | 12,851,320 |

**Marginal cost per extra interpreter: 4,250,784 bytes (4.05 MiB)**, close to the
3.87 MiB wasm figure from experiment E. Against Cloudflare's 64 MiB uncompressed
limit that is roughly 14–16 versions in one bundle — comfortably more than the
ten-plus the handoff wanted.

## The dispatcher

Eight bytes, no interpreter involved:

```rust
fn sniff_dump_version(bytes: &[u8]) -> Result<u16, String>  // MONTY\0 + u16 LE
```

A snapshot whose version no linked interpreter claims is reported as exactly that
(`dump v999 has no linked interpreter`) rather than mistaken for corruption, so a
retired-version run can be identified and reported instead of guessed at.

## The caveat the handoff flagged, now confirmed

**Dump version is not crate version.** monty 0.0.22 and 0.0.23 both report
`DUMP_VERSION = 8`, and a snapshot written by the 0.0.22 interpreter was read
successfully by the 0.0.23 one. Two consequences:

1. Good news — not every crate bump strands snapshots. Only a dump-format bump
   does, so old interpreters retire far less often than one-version-per-release
   would imply.
2. The routing table must map *dump* versions to interpreters, and that mapping
   is maintained by hand. Because the only two published versions share format 8,
   this experiment could not exercise a genuine cross-format route — the v999
   case is synthesised by rewriting the header.
