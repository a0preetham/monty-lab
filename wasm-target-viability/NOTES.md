# Experiment E — Wasm target viability

**Bottom line: viable, with two caveats** (frozen `performance.now()` under Workers;
`wasm-opt` needs explicit feature flags). Full analysis in `../FINDINGS.md`.

## Reproduce

```bash
cargo build --release --target wasm32-unknown-unknown   # 4,912,612 B
cargo build --release --target wasm32-wasip1            # compiles; size meaningless (see below)
wasm-pack build --target nodejs --release --out-dir pkg # wasm-opt step fails, see caveat
node run_node.mjs                                       # 9/9 pass
```

`wasm-pack` stops on `wasm-opt`, but it has already written `pkg/` by then, so the Node
harness runs against valid output. To optimise manually:

```bash
wasm-opt pkg/monty_wasm_probe_bg.wasm -o out.wasm -O \
  --enable-bulk-memory --enable-nontrapping-float-to-int \
  --enable-sign-ext --enable-reference-types
```

## Required configuration

`getrandom 0.3` will not build for `wasm32-unknown-unknown` without a backend. Two pieces,
both required:

- `.cargo/config.toml`: `rustflags = ['--cfg', 'getrandom_backend="wasm_js"']`
- `Cargo.toml`: name `getrandom` explicitly with `features = ["wasm_js"]` so the feature is
  unified into monty's own (unfeatured) dependency on it.

## Sizes

| Artifact | Raw | Gzipped |
| --- | --- | --- |
| `cargo build --release` | 4,912,612 | 1,657,056 |
| post-`wasm-bindgen` | 4,347,171 | 1,554,746 |
| post-`wasm-opt -O` | 4,057,087 (3.87 MiB) | 1,587,273 |

16 versions of ~3.87 MiB fit Cloudflare's 64 MiB uncompressed limit.

The `wasm32-wasip1` release build is 36,655 B because nothing is exported without
wasm-bindgen and the linker strips the interpreter. It shows the target compiles; it is not
a size measurement.

## Module imports (parsed from the raw `.wasm` import section)

All 16 are wasm-bindgen placeholders — no WASI, no threads/atomics, no filesystem, no
network. The real host requirements:

| Import | Workers |
| --- | --- |
| `__wbg_getRandomValues_*` | available |
| `__wbg_performance_*`, `__wbg_now_*` | present but timer is frozen per request — `max_duration` will not advance |
| `GLOBAL_THIS` / `SELF` / `GLOBAL` / `WINDOW` accessors | standard feature detection |
| `__wbg___wbindgen_jspi_spawn_poll_*` | JSPI; async paths only, **unverified on Workers** |

Monty targets wasm deliberately: `monty-types/src/resource.rs` swaps in `web_time::Instant`
under `cfg(all(target_arch = "wasm32", target_os = "unknown"))` because `std`'s panics there.

## Not done

No deploy, no workerd run — there are no Cloudflare credentials in this environment. The
`performance.now()` caveat is deduced from the import list plus documented Workers timer
behaviour; it should be confirmed against workerd in workstream F.
