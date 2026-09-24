//! Experiment G — crash recovery semantics under wasm.
//!
//! Pydantic recommend `monty-pool` over the in-process API because a stack
//! overflow or allocator abort kills only a worker process. On Workers there is
//! no pool: the interpreter shares the wasm instance with the host. The question
//! is what a guest failure does to that instance, and whether it stays usable.
//!
//! This module keeps ONE long-lived `MontyRepl` in a thread-local, exactly as a
//! Durable Object would hold a warm session, and exposes it so the JS harness
//! can try to break it and then check whether the session survived.

use std::cell::RefCell;

use monty::{dump, MontyRepl, ReplProgress, SessionRef};
use monty_types::{CompileOptions, MontyObject, PrintWriter, ResourceLimits, ResourceTracker};
use wasm_bindgen::prelude::*;

const SCRIPT: &str = "session.py";

/// The mitigation under test. Without a tracking allocator, `max_memory` is
/// inert and a guest allocation failure traps the whole wasm instance. With it,
/// Monty checks a *soft* limit at its own checkpoints and can raise a catchable
/// `MemoryError` before the allocator's hard limit is reached.
///
/// Note the hard limit calls `process::exit` — fine for `monty-pool`'s worker
/// model, fatal for a shared wasm instance. The soft limit is what has to do
/// the work here.
#[global_allocator]
static ALLOC: monty_alloc::LimitedAllocator = monty_alloc::LimitedAllocator;

thread_local! {
    /// The warm session. `None` only while a snippet is in flight, because
    /// `feed_start` consumes the repl.
    static SESSION: RefCell<Option<MontyRepl>> = RefCell::new(Some(fresh()));
}

fn fresh() -> MontyRepl {
    MontyRepl::new(SCRIPT, ResourceTracker::default(), CompileOptions::default())
}

fn fresh_with(limits: ResourceLimits) -> MontyRepl {
    MontyRepl::new(SCRIPT, ResourceTracker::new(limits), CompileOptions::default())
}

/// Feed a snippet to the persistent session. Python exceptions come back as
/// `Err(String)`; the session is retained either way.
#[wasm_bindgen]
pub fn feed(code: &str) -> Result<String, JsValue> {
    SESSION.with(|s| {
        let mut slot = s.borrow_mut();
        let Some(repl) = slot.as_mut() else {
            return Err(JsValue::from_str("session is poisoned: no repl in slot"));
        };
        match repl.feed_run(code, vec![], PrintWriter::Disabled) {
            Ok(v) => Ok(format!("{v:?}")),
            Err(e) => Err(JsValue::from_str(&last_line(&e.to_string()))),
        }
    })
}

/// Replace the session with a fresh one — the "discard and restart" recovery
/// path, for measuring what it costs to take.
#[wasm_bindgen]
pub fn reset_session() {
    SESSION.with(|s| *s.borrow_mut() = Some(fresh()));
}

/// Install a session with an explicit memory cap (bytes). `max_memory` is only
/// enforced when a tracking allocator is installed, so this also tells us
/// whether the limit does anything at all on this target.
#[wasm_bindgen]
pub fn reset_session_with_memory_limit(bytes: usize) -> Result<(), JsValue> {
    monty_alloc::set_limit(Some(bytes), false).map_err(JsValue::from_str)?;
    SESSION.with(|s| *s.borrow_mut() = Some(fresh_with(ResourceLimits::default().max_memory(bytes))));
    Ok(())
}

/// Clear the allocator limit again so later probes are unconstrained.
#[wasm_bindgen]
pub fn clear_memory_limit() -> Result<(), JsValue> {
    monty_alloc::set_limit(None, false).map_err(JsValue::from_str)
}

/// Live bytes the tracking allocator has charged — proves it is actually armed.
#[wasm_bindgen]
pub fn live_memory_bytes() -> f64 {
    monty_types::LIVE_MEMORY.load(std::sync::atomic::Ordering::Relaxed) as f64
}

/// Install a session with a recursion cap.
#[wasm_bindgen]
pub fn reset_session_with_recursion_limit(depth: usize) {
    SESSION.with(|s| *s.borrow_mut() = Some(fresh_with(ResourceLimits::default().max_recursion_depth(depth))));
}

/// Is the session still present and usable?
#[wasm_bindgen]
pub fn session_alive() -> bool {
    SESSION.with(|s| {
        let mut slot = s.borrow_mut();
        match slot.as_mut() {
            Some(repl) => repl.feed_run("1 + 1", vec![], PrintWriter::Disabled).is_ok(),
            None => false,
        }
    })
}

/// Snapshot size for the live session — proves the session is not just alive
/// but still serializable after whatever the harness did to it.
#[wasm_bindgen]
pub fn snapshot_len() -> Result<usize, JsValue> {
    SESSION.with(|s| {
        let slot = s.borrow();
        let Some(repl) = slot.as_ref() else {
            return Err(JsValue::from_str("no repl in slot"));
        };
        dump(SCRIPT, None, SessionRef::Idle(repl))
            .map(|b| b.len())
            .map_err(|e| JsValue::from_str(&format!("{e:?}")))
    })
}

/// Drive a snippet that suspends, answering every host call with `answer`, with
/// a hard cap on iterations so a pathological guest cannot spin forever.
#[wasm_bindgen]
pub fn feed_with_tool(code: &str, answer: i32, max_calls: u32) -> Result<String, JsValue> {
    SESSION.with(|s| {
        let mut slot = s.borrow_mut();
        let Some(repl) = slot.take() else {
            return Err(JsValue::from_str("no repl in slot"));
        };
        let mut progress = match repl.feed_start(code, vec![], PrintWriter::Disabled) {
            Ok(p) => p,
            Err(e) => {
                *slot = Some(e.repl);
                return Err(JsValue::from_str(&last_line(&e.error.to_string())));
            }
        };
        let mut calls = 0u32;
        loop {
            match progress {
                ReplProgress::FunctionCall(call) => {
                    calls += 1;
                    if calls > max_calls {
                        *slot = Some(call.into_repl());
                        return Err(JsValue::from_str("exceeded max_calls; snippet abandoned"));
                    }
                    progress = match call.resume(MontyObject::Int(answer.into()), PrintWriter::Disabled) {
                        Ok(p) => p,
                        Err(e) => {
                            *slot = Some(e.repl);
                            return Err(JsValue::from_str(&last_line(&e.error.to_string())));
                        }
                    };
                }
                ReplProgress::Complete { repl, value } => {
                    *slot = Some(repl);
                    return Ok(format!("{value:?}"));
                }
                other => {
                    *slot = Some(other.into_repl());
                    return Err(JsValue::from_str("unhandled suspension kind"));
                }
            }
        }
    })
}

/// Current wasm linear memory size in bytes, read from the JS side's
/// perspective via `core::arch`. Lets the harness watch the heap grow and see
/// whether it is released after a guest blow-up.
#[wasm_bindgen]
pub fn linear_memory_bytes() -> u32 {
    #[cfg(target_arch = "wasm32")]
    {
        (core::arch::wasm32::memory_size(0) as u32) * 65536
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0
    }
}

fn last_line(s: &str) -> String {
    s.lines().next_back().unwrap_or_default().trim().to_owned()
}
