use std::time::Duration;
use tsgo_wasm::TypeScript;

fn temp_project(name: &str, source: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("tsgo-wasm-test-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.ts"), source).unwrap();
    dir
}

#[test]
fn reports_version() {
    let ts = TypeScript::new().unwrap();
    assert!(ts.version().unwrap().starts_with("Version"));
}

#[test]
fn finds_type_errors() {
    let ts = TypeScript::new().unwrap();
    let dir = temp_project(
        "errors",
        "interface User { id: number }\nconst u: User = { id: 'nope' };\nexport default u;\n",
    );
    let output = ts
        .check(&dir, "main.ts", Some(Duration::from_secs(60)))
        .unwrap();
    assert_ne!(output.exit_code, 0);
    assert!(
        output.stdout.contains("TS2322"),
        "stdout: {}",
        output.stdout
    );
}

#[test]
fn passes_valid_code() {
    let ts = TypeScript::new().unwrap();
    let dir = temp_project(
        "valid",
        "export const add = (a: number, b: number): number => a + b;\n",
    );
    let output = ts
        .check(&dir, "main.ts", Some(Duration::from_secs(60)))
        .unwrap();
    assert_eq!(
        output.exit_code, 0,
        "stdout: {}\nstderr: {}",
        output.stdout, output.stderr
    );
}

#[test]
fn enforces_timeout() {
    let ts = TypeScript::new().unwrap();
    let dir = temp_project("timeout", "export const x = 1;\n");
    let result = ts.check(&dir, "main.ts", Some(Duration::from_millis(1)));
    assert!(result.is_err());
}
