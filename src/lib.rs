pub const TSGO_WASM_ZSTD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tsgo.wasm.zst"));
pub const TSGO_REV: &str = include_str!("../artifacts/tsgo.rev");

pub fn module_bytes() -> std::io::Result<Vec<u8>> {
    zstd::decode_all(TSGO_WASM_ZSTD)
}

#[cfg(feature = "runtime")]
pub use runtime::{Output, TypeScript, build_cwasm, precompile};

#[cfg(feature = "runtime")]
mod runtime {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use wasmtime::{Config, Engine, Linker, Module, Store};
    use wasmtime_wasi::p1::{self, WasiP1Ctx};
    use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
    use wasmtime_wasi::{DirPerms, FilePerms, I32Exit, WasiCtxBuilder};

    const EPOCH_TICK: Duration = Duration::from_millis(100);
    const PIPE_CAPACITY: usize = 1 << 26;

    fn engine_config() -> Config {
        let mut config = Config::new();
        config.epoch_interruption(true);
        config
    }

    pub fn precompile(target: Option<&str>) -> anyhow::Result<Vec<u8>> {
        let mut config = engine_config();
        if let Some(target) = target {
            config.target(target)?;
        }
        let engine = Engine::new(&config)?;
        Ok(engine.precompile_module(&crate::module_bytes()?)?)
    }

    pub fn build_cwasm() -> anyhow::Result<std::path::PathBuf> {
        let out_dir = std::env::var("OUT_DIR")?;
        let target = std::env::var("TARGET")?;
        let host = std::env::var("HOST")?;
        let cwasm = precompile((target != host).then_some(target.as_str()))?;
        let path = std::path::PathBuf::from(out_dir).join("tsgo.cwasm");
        std::fs::write(&path, cwasm)?;
        Ok(path)
    }

    #[derive(Debug)]
    pub struct Output {
        pub exit_code: i32,
        pub stdout: String,
        pub stderr: String,
    }

    pub struct TypeScript {
        engine: Engine,
        module: Module,
        linker: Linker<WasiP1Ctx>,
        stop_ticker: Arc<AtomicBool>,
    }

    impl TypeScript {
        pub fn new() -> anyhow::Result<Self> {
            let engine = Self::engine()?;
            let module = Module::new(&engine, crate::module_bytes()?)?;
            Self::assemble(engine, module)
        }

        pub fn with_cache(cache: impl AsRef<Path>) -> anyhow::Result<Self> {
            let cache = cache.as_ref();
            let engine = Self::engine()?;
            let module = match unsafe { Module::deserialize_file(&engine, cache) } {
                Ok(module) => module,
                Err(_) => {
                    let module = Module::new(&engine, crate::module_bytes()?)?;
                    std::fs::write(cache, module.serialize()?)?;
                    module
                }
            };
            Self::assemble(engine, module)
        }

        pub unsafe fn from_cwasm(cwasm: &[u8]) -> anyhow::Result<Self> {
            let engine = Self::engine()?;
            let module = unsafe { Module::deserialize(&engine, cwasm)? };
            Self::assemble(engine, module)
        }

        fn engine() -> anyhow::Result<Engine> {
            Ok(Engine::new(&engine_config())?)
        }

        fn assemble(engine: Engine, module: Module) -> anyhow::Result<Self> {
            let mut linker = Linker::new(&engine);
            p1::add_to_linker_sync(&mut linker, |ctx| ctx)?;
            let stop_ticker = Arc::new(AtomicBool::new(false));
            let ticker_engine = engine.clone();
            let ticker_stop = stop_ticker.clone();
            std::thread::spawn(move || {
                while !ticker_stop.load(Ordering::Relaxed) {
                    std::thread::sleep(EPOCH_TICK);
                    ticker_engine.increment_epoch();
                }
            });
            Ok(Self {
                engine,
                module,
                linker,
                stop_ticker,
            })
        }

        pub fn run(
            &self,
            args: &[&str],
            mounts: &[(&Path, &str)],
            timeout: Option<Duration>,
        ) -> anyhow::Result<Output> {
            let stdout = MemoryOutputPipe::new(PIPE_CAPACITY);
            let stderr = MemoryOutputPipe::new(PIPE_CAPACITY);

            let mut builder = WasiCtxBuilder::new();
            builder.args(&[&["tsgo"], args].concat());
            builder.stdout(stdout.clone());
            builder.stderr(stderr.clone());
            for (host, guest) in mounts {
                builder.preopened_dir(host, *guest, DirPerms::all(), FilePerms::all())?;
            }

            let mut store = Store::new(&self.engine, builder.build_p1());
            match timeout {
                Some(timeout) => {
                    let ticks = timeout.as_millis().div_ceil(EPOCH_TICK.as_millis()) + 1;
                    store.set_epoch_deadline(ticks as u64);
                }
                None => store.set_epoch_deadline(u64::MAX),
            }

            let instance = self.linker.instantiate(&mut store, &self.module)?;
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
