use std::time::Duration;
use tsgo_wasm::TypeScript;

#[test]
fn api_session_checks_and_updates_incrementally() {
    let ts = TypeScript::new().unwrap();
    let mut session = ts
        .api_session(
            &[
                ("lib/user.ts", "export interface User { id: number }\n"),
                (
                    "main.ts",
                    "import { User } from './lib/user';\nconst u: User = { id: 'x' };\nexport default u;\n",
                ),
            ],
            Duration::from_secs(60),
        )
        .unwrap();

    let diagnostics = session.diagnostics().unwrap();
    let error = diagnostics
        .iter()
        .find(|d| d.code == 2322)
        .unwrap_or_else(|| panic!("expected TS2322, got: {diagnostics:?}"));
    assert!(error.is_error());
    assert_eq!(error.file_name.as_deref(), Some("main.ts"));
    let range = error.range.expect("range resolved from in-memory source");
    assert_eq!(range.start.line, 2, "got: {error:?}");

    session
        .update_file(
            "main.ts",
            "import { User } from './lib/user';\nconst u: User = { id: 1 };\nexport default u;\n",
        )
        .unwrap();
    let diagnostics = session.diagnostics().unwrap();
    assert!(
        diagnostics.is_empty(),
        "expected clean check, got: {diagnostics:?}"
    );

    session
        .update_file(
            "lib/user.ts",
            "export interface User { id: number; name: string }\n",
        )
        .unwrap();
    let diagnostics = session.diagnostics().unwrap();
    let missing = diagnostics
        .iter()
        .find(|d| d.code == 2741)
        .unwrap_or_else(|| panic!("expected TS2741 missing property, got: {diagnostics:?}"));
    assert!(
        missing
            .related_information
            .iter()
            .any(|r| r.file_name.as_deref() == Some("lib/user.ts")),
        "expected related info pointing at lib/user.ts, got: {missing:?}"
    );
}
