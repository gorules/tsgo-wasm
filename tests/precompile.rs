use tsgo_wasm::TypeScript;

#[test]
fn precompile_roundtrips() {
    let cwasm = tsgo_wasm::precompile(None).unwrap();
    let ts = unsafe { TypeScript::from_cwasm(&cwasm) }.unwrap();
    assert!(ts.version().unwrap().starts_with("Version"));
}
