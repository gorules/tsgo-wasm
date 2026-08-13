pub const TSGO_WASM_ZSTD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tsgo.wasm.zst"));
pub const TSGO_REV: &str = include_str!("../artifacts/tsgo.rev");

pub fn module_bytes() -> std::io::Result<Vec<u8>> {
    zstd::decode_all(TSGO_WASM_ZSTD)
}

#[cfg(feature = "runtime")]
pub use runtime::{Output, TypeScript, TypeScriptConfig, build_cwasm, build_cwasm_zst, precompile};

#[cfg(feature = "runtime")]
mod runtime {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use wasmtime::{
        Config, Engine, InstancePre, Linker, Module, Store, StoreLimits, StoreLimitsBuilder,
    };
    use wasmtime_wasi::p1::{self, WasiP1Ctx};
    use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
    use wasmtime_wasi::{DirPerms, FilePerms, I32Exit, WasiCtxBuilder};

    #[derive(Clone, Debug)]
    pub struct TypeScriptConfig {
        pub signals_based_traps: bool,
        pub memory_limit: Option<usize>,
        pub epoch_tick: Duration,
        pub pipe_capacity: usize,
    }

    impl Default for TypeScriptConfig {
        fn default() -> Self {
            Self {
                signals_based_traps: true,
                memory_limit: None,
                epoch_tick: Duration::from_millis(100),
                pipe_capacity: 1 << 26,
            }
        }
    }

    impl TypeScriptConfig {
        fn wasmtime_config(&self) -> Config {
            let mut config = Config::new();
            config.epoch_interruption(true);
            config.signals_based_traps(self.signals_based_traps);
            config
        }

        fn engine(&self) -> anyhow::Result<Engine> {
            Ok(Engine::new(&self.wasmtime_config())?)
        }

        pub fn load(&self) -> anyhow::Result<TypeScript> {
            let engine = self.engine()?;
            let module = Module::new(&engine, crate::module_bytes()?)?;
            TypeScript::assemble(engine, module, self.clone())
        }

        pub fn load_cached(&self, cache: impl AsRef<Path>) -> anyhow::Result<TypeScript> {
            let cache = cache.as_ref();
            let engine = self.engine()?;
            let module = match unsafe { Module::deserialize_file(&engine, cache) } {
                Ok(module) => module,
                Err(_) => {
                    let module = Module::new(&engine, crate::module_bytes()?)?;
                    std::fs::write(cache, module.serialize()?)?;
                    module
                }
            };
            TypeScript::assemble(engine, module, self.clone())
        }

        pub unsafe fn from_cwasm(&self, cwasm: &[u8]) -> anyhow::Result<TypeScript> {
            let engine = self.engine()?;
            let decompressed;
            let cwasm = if cwasm.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
                decompressed = zstd::decode_all(cwasm)?;
                decompressed.as_slice()
            } else {
                cwasm
            };
            let module = unsafe { Module::deserialize(&engine, cwasm)? };
            TypeScript::assemble(engine, module, self.clone())
        }

        pub fn precompile(&self, target: Option<&str>) -> anyhow::Result<Vec<u8>> {
            let mut config = self.wasmtime_config();
            if let Some(target) = target {
                config.target(target)?;
            }
            let engine = Engine::new(&config)?;
            Ok(engine.precompile_module(&crate::module_bytes()?)?)
        }

        pub fn build_cwasm(&self) -> anyhow::Result<PathBuf> {
            let (out_dir, target) = build_script_env()?;
            let cwasm = self.precompile(target.as_deref())?;
            let path = out_dir.join("tsgo.cwasm");
            std::fs::write(&path, cwasm)?;
            Ok(path)
        }

        pub fn build_cwasm_zst(&self, level: i32) -> anyhow::Result<PathBuf> {
            let (out_dir, target) = build_script_env()?;
            let cwasm = self.precompile(target.as_deref())?;
            let path = out_dir.join("tsgo.cwasm.zst");
            std::fs::write(&path, zstd::encode_all(cwasm.as_slice(), level)?)?;
            Ok(path)
        }
    }

    fn build_script_env() -> anyhow::Result<(PathBuf, Option<String>)> {
        let out_dir = PathBuf::from(std::env::var("OUT_DIR")?);
        let target = std::env::var("TARGET")?;
        let host = std::env::var("HOST")?;
        Ok((out_dir, (target != host).then_some(target)))
    }

    pub fn precompile(target: Option<&str>) -> anyhow::Result<Vec<u8>> {
        TypeScriptConfig::default().precompile(target)
    }

    pub fn build_cwasm() -> anyhow::Result<PathBuf> {
        TypeScriptConfig::default().build_cwasm()
    }

    pub fn build_cwasm_zst(level: i32) -> anyhow::Result<PathBuf> {
        TypeScriptConfig::default().build_cwasm_zst(level)
    }

    #[derive(Debug)]
    pub struct Output {
        pub exit_code: i32,
        pub stdout: String,
        pub stderr: String,
    }

    struct State {
        wasi: WasiP1Ctx,
        limits: StoreLimits,
    }

    pub struct TypeScript {
        engine: Engine,
        instance_pre: InstancePre<State>,
        config: TypeScriptConfig,
        stop_ticker: Arc<AtomicBool>,
    }

    impl TypeScript {
        pub fn new() -> anyhow::Result<Self> {
            TypeScriptConfig::default().load()
        }

        pub fn with_cache(cache: impl AsRef<Path>) -> anyhow::Result<Self> {
            TypeScriptConfig::default().load_cached(cache)
        }

        pub unsafe fn from_cwasm(cwasm: &[u8]) -> anyhow::Result<Self> {
            unsafe { TypeScriptConfig::default().from_cwasm(cwasm) }
        }

        fn assemble(
            engine: Engine,
            module: Module,
            config: TypeScriptConfig,
        ) -> anyhow::Result<Self> {
            let mut linker: Linker<State> = Linker::new(&engine);
            p1::add_to_linker_sync(&mut linker, |state: &mut State| &mut state.wasi)?;
            let instance_pre = linker.instantiate_pre(&module)?;

            let stop_ticker = Arc::new(AtomicBool::new(false));
            let ticker_engine = engine.clone();
            let ticker_stop = stop_ticker.clone();
            let tick = config.epoch_tick;
            std::thread::spawn(move || {
                while !ticker_stop.load(Ordering::Relaxed) {
                    std::thread::sleep(tick);
                    ticker_engine.increment_epoch();
                }
            });

            Ok(Self {
                engine,
                instance_pre,
                config,
                stop_ticker,
            })
        }

        pub fn run(
            &self,
            args: &[&str],
            mounts: &[(&Path, &str)],
            timeout: Option<Duration>,
        ) -> anyhow::Result<Output> {
            let stdout = MemoryOutputPipe::new(self.config.pipe_capacity);
            let stderr = MemoryOutputPipe::new(self.config.pipe_capacity);

            let mut builder = WasiCtxBuilder::new();
            builder.args(&[&["tsgo"], args].concat());
            builder.stdout(stdout.clone());
            builder.stderr(stderr.clone());
            for (host, guest) in mounts {
                builder.preopened_dir(host, *guest, DirPerms::all(), FilePerms::all())?;
            }

            let mut limits = StoreLimitsBuilder::new();
            if let Some(memory) = self.config.memory_limit {
                limits = limits.memory_size(memory);
            }
            let state = State {
                wasi: builder.build_p1(),
                limits: limits.build(),
            };

            let mut store = Store::new(&self.engine, state);
            store.limiter(|state| &mut state.limits);
            match timeout {
                Some(timeout) => {
                    let ticks = timeout
                        .as_millis()
                        .div_ceil(self.config.epoch_tick.as_millis())
                        + 1;
                    store.set_epoch_deadline(ticks as u64);
                }
                None => store.set_epoch_deadline(u64::MAX),
            }

            let instance = self.instance_pre.instantiate(&mut store)?;
            let start = instance.get_typed_func::<(), ()>(&mut store, "_start")?;
            let exit_code = match start.call(&mut store, ()) {
                Ok(()) => 0,
                Err(error) => match error.downcast_ref::<I32Exit>() {
                    Some(exit) => exit.0,
                    None => return Err(error.into()),
                },
            };
            drop(store);

            Ok(Output {
                exit_code,
                stdout: String::from_utf8_lossy(&stdout.contents()).into_owned(),
                stderr: String::from_utf8_lossy(&stderr.contents()).into_owned(),
            })
        }

        pub fn check(
            &self,
            dir: impl AsRef<Path>,
            entry: &str,
            timeout: Option<Duration>,
        ) -> anyhow::Result<Output> {
            let guest_entry = format!("/project/{entry}");
            self.run(
                &["--noEmit", &guest_entry],
                &[(dir.as_ref(), "/project")],
                timeout,
            )
        }

        pub fn version(&self) -> anyhow::Result<String> {
            Ok(self
                .run(&["--version"], &[], None)?
                .stdout
                .trim()
                .to_string())
        }
    }

    impl Drop for TypeScript {
        fn drop(&mut self) {
            self.stop_ticker.store(true, Ordering::Relaxed);
        }
    }
}
