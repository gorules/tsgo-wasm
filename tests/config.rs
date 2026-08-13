use std::time::Duration;
use tsgo_wasm::TypeScriptConfig;

#[test]
fn signals_free_config_checks() {
    let config = TypeScriptConfig {
        signals_based_traps: false,
        memory_limit: Some(2 * 1024 * 1024 * 1024),
        epoch_tick: Duration::from_millis(10),
        ..Default::default()
    };
    let ts = config.load().unwrap();

    let dir = std::env::temp_dir().join(format!("tsgo-wasm-config-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.ts"), "const n: number = 'x';\n").unwrap();
    let output = ts
        .check(&dir, "main.ts", Some(Duration::from_secs(120)))
        .unwrap();
    assert_ne!(output.exit_code, 0);
    assert!(
        output.stdout.contains("TS2322"),
        "stdout: {}",
        output.stdout
    );
}

#[test]
fn memory_limit_is_enforced() {
    let config = TypeScriptConfig {
        memory_limit: Some(8 * 1024 * 1024),
        ..Default::default()
    };
    let ts = config.load().unwrap();
    let dir = std::env::temp_dir().join(format!("tsgo-wasm-memlimit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.ts"), "export const x: number = 1;\n").unwrap();
    let result = ts.check(&dir, "main.ts", Some(Duration::from_secs(120)));
    assert!(
        result.is_err() || result.as_ref().unwrap().exit_code != 0,
        "expected failure under 8MB limit, got: {result:?}"
    );
}
