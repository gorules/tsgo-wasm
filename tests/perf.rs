use std::time::{Duration, Instant};
use tsgo_wasm::TypeScript;

#[test]
#[ignore]
fn session_vs_oneshot_timings() {
    let timeout = Duration::from_secs(120);
    let sources = [
        ("lib/user.ts", "export interface User { id: number }\n"),
        (
            "main.ts",
            "import { User } from './lib/user';\nconst u: User = { id: 1 };\nexport default u;\n",
        ),
    ];

    let t = Instant::now();
    let ts = TypeScript::new().unwrap();
    println!("module compile:        {:?}", t.elapsed());

    for i in 1..=3 {
        let t = Instant::now();
        let diags = ts.check(&sources, timeout).unwrap();
        println!(
            "one-shot check #{i}:     {:?} ({} diags)",
            t.elapsed(),
            diags.len()
        );
    }

    let t = Instant::now();
    let mut session = ts.api_session(&sources, Duration::from_secs(120)).unwrap();
    println!("session boot:          {:?}", t.elapsed());

    let t = Instant::now();
    let diags = session.diagnostics().unwrap();
    println!(
        "first diagnostics:     {:?} ({} diags)",
        t.elapsed(),
        diags.len()
    );

    for i in 1..=3 {
        let t = Instant::now();
        session
            .update_file(
                "main.ts",
                &format!(
                    "import {{ User }} from './lib/user';\nconst u: User = {{ id: {} }};\nexport default u;\n",
                    i + 10
                ),
            )
            .unwrap();
        let diags = session.diagnostics().unwrap();
        println!(
            "update+rediagnose #{i}:  {:?} ({} diags)",
            t.elapsed(),
            diags.len()
        );
    }

    for i in 1..=3 {
        let t = Instant::now();
        session
            .update_file(
                "main.ts",
                &format!(
                    "import {{ User }} from './lib/user';\nconst u: User = {{ id: '{}' }};\nexport default u;\n",
                    i + 20
                ),
            )
            .unwrap();
        let diags = session.diagnostics_for("main.ts").unwrap();
        println!(
            "update+file-diag #{i}:   {:?} ({} diags)",
            t.elapsed(),
            diags.len()
        );
    }
}
