use std::time::Duration;
use tsgo_wasm::TypeScript;

const TIMEOUT: Duration = Duration::from_secs(60);

#[test]
fn reports_version() {
    let ts = TypeScript::new().unwrap();
    assert!(ts.version().unwrap().starts_with("Version"));
}

#[test]
fn finds_type_errors() {
    let ts = TypeScript::new().unwrap();
    let diagnostics = ts
        .check(
            &[(
                "main.ts",
                "interface User { id: number }\nconst u: User = { id: 'nope' };\nexport default u;\n",
            )],
            TIMEOUT,
        )
        .unwrap();
    assert!(
        diagnostics.iter().any(|d| d.code == 2322),
        "expected TS2322, got: {diagnostics:?}"
    );
}

#[test]
fn passes_valid_code() {
    let ts = TypeScript::new().unwrap();
    let diagnostics = ts
        .check(
            &[(
                "main.ts",
                "export const add = (a: number, b: number): number => a + b;\n",
            )],
            TIMEOUT,
        )
        .unwrap();
    assert!(diagnostics.is_empty(), "got: {diagnostics:?}");
}

#[test]
fn resolves_imports_across_files() {
    let ts = TypeScript::new().unwrap();
    let diagnostics = ts
        .check(
            &[
                ("lib/user.ts", "export interface User { id: number }\n"),
                (
                    "main.ts",
                    "import { User } from './lib/user';\nconst u: User = { id: 'x' };\nexport default u;\n",
                ),
            ],
            TIMEOUT,
        )
        .unwrap();
    assert!(
        diagnostics.iter().any(|d| d.code == 2322),
        "expected TS2322, got: {diagnostics:?}"
    );
}

#[test]
fn respects_tsconfig() {
    let ts = TypeScript::new().unwrap();
    let diagnostics = ts
        .check(
            &[
                (
                    "tsconfig.json",
                    r#"{ "compilerOptions": { "strict": true }, "include": ["**/*.ts"] }"#,
                ),
                ("main.ts", "export const x: number = undefined;\n"),
            ],
            TIMEOUT,
        )
        .unwrap();
    assert!(
        diagnostics.iter().any(|d| d.code == 2322),
        "expected TS2322 under strict, got: {diagnostics:?}"
    );
}

#[test]
fn enforces_timeout() {
    let ts = TypeScript::new().unwrap();
    let result = ts.check(
        &[("main.ts", "export const x = 1;\n")],
        Duration::from_millis(1),
    );
    assert!(result.is_err());
}
