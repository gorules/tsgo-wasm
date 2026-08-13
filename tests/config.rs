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
    let diagnostics = ts
        .check(
            &[("main.ts", "const n: number = 'x';\n")],
            Duration::from_secs(120),
        )
        .unwrap();
    assert!(
        diagnostics.iter().any(|d| d.code == 2322),
        "expected TS2322, got: {diagnostics:?}"
    );
}

#[test]
fn memory_limit_is_enforced() {
    let config = TypeScriptConfig {
        memory_limit: Some(8 * 1024 * 1024),
        ..Default::default()
    };
    let ts = config.load().unwrap();
    let result = ts.check(
        &[("main.ts", "export const x: number = 1;\n")],
        Duration::from_secs(120),
    );
    assert!(
        result.is_err(),
        "expected failure under 8MB limit, got: {result:?}"
    );
}
