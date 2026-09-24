//! Experiment E: minimal wasm wrapper around `monty` to prove the interpreter
//! links AND runs on wasm32.
use monty::MontyRun;
use monty_types::CompileOptions;
use wasm_bindgen::prelude::*;

/// Execute a Python snippet and return its final expression value rendered as
/// a string. Errors come back as `Err(JsValue)` carrying the Python exception.
#[wasm_bindgen]
pub fn eval_py(source: &str) -> Result<String, JsValue> {
    let run = MontyRun::new(source.to_owned(), "snippet.py", vec![], CompileOptions::default())
        .map_err(|e| JsValue::from_str(&format!("compile error: {e:?}")))?;
    let out = run
        .run_no_limits(vec![])
        .map_err(|e| JsValue::from_str(&format!("{:?}: {:?}", e.exc_type(), e.message())))?;
    Ok(format!("{:?}", out.as_ref()))
}

/// Exercises the `getrandom` path (boundary uuid creation) so the harness
/// proves entropy actually works, not just that it links.
#[wasm_bindgen]
pub fn random_u128() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("entropy unavailable");
    format!("{:032x}", u128::from_le_bytes(bytes))
}
