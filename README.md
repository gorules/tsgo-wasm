# tsgo-wasm

[microsoft/typescript-go](https://github.com/microsoft/typescript-go) (`tsgo`) compiled to a WASI p1 module, embedded in a Rust crate with an optional wasmtime runtime.

## Usage

```toml
[dependencies]
tsgo-wasm = "0.1"
```

Each crate version pins an exact typescript-go revision (see `TSGO_REV` and the CHANGELOG); bumping the crate switches the module. The wasm binary itself is fetched from this repo's release assets at build time — see [Module distribution](#module-distribution).

```rust
use std::time::Duration;
use tsgo_wasm::TypeScript;

let ts = TypeScript::new()?;
let output = ts.check("./project-dir", "main.ts", Some(Duration::from_secs(30)))?;
println!("exit={} {}", output.exit_code, output.stdout);
```

- `TypeScript::new()` compiles the embedded module eagerly (~2s wall / ~20s CPU, parallelized). Construct it at process startup so the cost lands on boot, not on the first check; the `Module` is reused across runs at full speed. No artifacts on disk, ever.
- `TypeScript::with_cache(path)` is a dev-loop convenience: it deserializes a previously cached compilation (~10ms) and falls back to compile-and-cache, so frequent process restarts (`cargo watch`) skip the boot compile. The cache is keyed to the wasmtime version/config by wasmtime itself; a mismatch silently recompiles.
- `check(dir, entry, timeout)` mounts `dir` read-write at `/project` and runs `tsgo --noEmit /project/<entry>`.
- `run(args, mounts, timeout)` is the raw CLI passthrough.
- Timeouts use epoch interruption (100ms granularity), safe under concurrent runs.
- Each run gets a fresh `Store`/instance: no state leaks between runs. Expect a ~0.9s floor per invocation (Go runtime boot + default lib loading inside the guest) — batch work into few invocations where possible.

## Custom engine config

Embedders with non-default requirements use `TypeScriptConfig` — every path (load, cache, precompile, cwasm) flows through the same config value, so AOT artifacts and the loading engine match by construction:

```rust
use tsgo_wasm::TypeScriptConfig;

let config = TypeScriptConfig {
    signals_based_traps: false,
    memory_limit: Some(2 * 1024 * 1024 * 1024),
    epoch_tick: Duration::from_millis(10),
    ..Default::default()
};
let ts = config.load()?;
```

- `signals_based_traps: false` is required when wasmtime shares a process with a runtime that owns the signal handlers (V8 in a Node addon, JVM). It switches to explicit bounds checks: ~4x slower compilation, larger code, slower execution — leave it `true` (default) in pure Rust processes, where guest faults already surface as clean `Err` traps.
- `memory_limit` caps each run's linear memory via a store limiter.
- `TypeScript::new()` / `with_cache` / `from_cwasm` and the `build_cwasm*` helpers are shorthands for the same methods on `TypeScriptConfig::default()`.
- A cwasm only loads into an engine whose compile-relevant config matches — precompile and load with the same `TypeScriptConfig` value.

## Build-time AOT

For consumers where the ~13 CPU-seconds boot compile is unacceptable (Lambda, tightly CPU-limited pods), precompile in your own build script — version, target, and engine config stay matched by construction, including cross-compilation:

```toml
[build-dependencies]
tsgo-wasm = "0.1"
```

```rust
fn main() {
    tsgo_wasm::build_cwasm().unwrap();
}
```

```rust
static TSGO_CWASM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tsgo.cwasm"));
let ts = unsafe { TypeScript::from_cwasm(TSGO_CWASM)? };
```

`build_cwasm_zst(level)` writes `tsgo.cwasm.zst` instead (~17 MB at level 19 vs 92 MB raw) — `from_cwasm` detects zstd transparently, trading ~92 MB of binary size for a short decompression at startup.

Costs: +92 MB in the binary (or the compressed size with the zst variant) and ~13 CPU-seconds per fresh build. In dev profiles build dependencies default to opt-level 0, which makes the precompile step several times slower — add `[profile.dev.build-override] opt-level = 2` if you AOT in dev builds. `precompile(target)` returns the raw cwasm bytes if you'd rather ship it as a file. Embeddings with their own wasmtime engine (different version or config) must not use these helpers; feed `module_bytes()` to their own `Engine::precompile_module` instead.

## Bytes only

```toml
[dependencies]
tsgo-wasm = { version = "0.1", default-features = false }
```

Exposes `TSGO_WASM_ZSTD`, `TSGO_REV`, and `module_bytes()` with only a `zstd` dependency. The module is runtime-agnostic wasm + WASI p1: it runs under wasmtime, wasmer, Node's `node:wasi`, or any other wasip1 host. Works on any target for embedding; executing it requires a native host runtime (the `runtime` feature does not build on `wasm32-*`).

## Performance

Measured on an M-series Mac, 5.1k-line type-check: native tsgo 0.13s (multi-threaded); wasm 0.9–1.0s in wasmtime and V8 alike (wasip1 is single-threaded and Go's wasm codegen is ~4x slower per core). The sandboxing and portability are the point; use native tsgo where you control the input and platform.

## Updating tsgo

Every published crate version is immutably fixed to one typescript-go commit: `tsgo.rev` and `tsgo.sha256` are frozen into the crates.io package at publish time, and tsgo updates always land as a new minor (`0.1.x` → `0.2.0`), which cargo treats as an incompatible range — consumers never receive a new tsgo without an explicit version bump.

Updating is therefore a release act (requires Go, zstd, and an authenticated `gh`):

1. Edit `artifacts/tsgo.rev` to the desired microsoft/typescript-go commit sha — this file is the only pin.
2. Run `make update` — builds the module at that sha, tests, uploads the release asset, refreshes `tsgo.sha256`.
3. Open a PR with the changed `artifacts/` as `feat: update tsgo to microsoft/typescript-go@<sha>`.

Merging feeds release-please, which maintains the Release PR (version bump + CHANGELOG); merging that tags `v<version>` and publishes the new crate version to crates.io.

## Module distribution

The wasm binary is never committed to git or packaged into the crate — the repo and crates.io package carry only its rev and sha256. `build.rs` resolves the module in order:

1. `TSGO_WASM_FILE=<path>` env override (offline / vendored / air-gapped builds)
2. `artifacts/tsgo.wasm.zst` in the source tree, if its sha256 matches (present after `make tsgo`)
3. Download from this repo's `tsgo-<rev>` release asset, verified against the committed sha256

The result lands in `OUT_DIR` and is embedded via `include_bytes!`, so the consumer API is identical in all three paths. `TSGO_REV` (from the committed `artifacts/tsgo.rev`) always records the exact upstream commit.
