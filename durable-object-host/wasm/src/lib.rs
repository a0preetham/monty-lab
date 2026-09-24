//! A Monty REPL session exposed to JS as a *step machine*.
//!
//! The obvious shape — pass JS a tool callback and let Rust drive the loop —
//! forces every tool to be synchronous, which is useless in a Worker where
//! tools are `fetch` and storage reads. Instead the loop lives in JS:
//!
//! ```js
//! let step = session.start(code);
//! while (step.kind === 'tool') {
//!   const result = await callTool(step.name, step.args);  // real async
//!   step = session.resume(result);
//! }
//! ```
//!
//! Rust keeps the interpreter and the suspension; JS keeps the async. No JSPI
//! needed, which matters because JSPI availability on Workers is unverified.

use monty::{dump, Dump, MontyRepl, ReplProgress, Session, SessionRef};
use monty_types::{
    CompileOptions, ExcType, ExtFunctionResult, MontyException, MontyObject, NameLookupResult,
    PrintWriter, ResourceLimits, ResourceTracker,
};
use serde::Serialize;
use serde::Serializer as _;
use wasm_bindgen::prelude::*;

/// Experiment G: without a tracking allocator a guest OOM traps the whole wasm
/// instance and nothing in it can be used again — not even to reset. With this
/// armed, an over-allocating guest gets a catchable `MemoryError` instead.
#[global_allocator]
static ALLOC: monty_alloc::LimitedAllocator = monty_alloc::LimitedAllocator;

/// Default guest memory ceiling. Workers gives the isolate 128 MB total, so the
/// guest gets a fraction and the host keeps the rest.
const DEFAULT_MAX_MEMORY: usize = 48 * 1024 * 1024;

/// What the session wants next.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Step {
    /// Guest called an undeclared function. Host should run the tool and
    /// `resume` with its result.
    Tool {
        name: String,
        args: Vec<serde_json::Value>,
        #[serde(rename = "callId")]
        call_id: u32,
    },
    /// Guest referenced an undeclared *name*. Resolution is cached by the
    /// interpreter, so this suits constants, not tools.
    Name { name: String },
    /// Snippet finished.
    Done { value: serde_json::Value },
    /// Snippet raised. The session is still usable.
    Error { error: String, #[serde(rename = "excType")] exc_type: String },
}

/// Where the interpreter is right now. `feed_start` consumes the repl, so a
/// suspended session genuinely holds no `MontyRepl`.
enum State {
    Idle(Box<MontyRepl>),
    Suspended(Box<ReplProgress>),
    /// Only after a bug on our side; kept so JS gets an error rather than a panic.
    Taken,
}

#[wasm_bindgen]
pub struct AgentSession {
    state: State,
    script: String,
}

#[wasm_bindgen]
impl AgentSession {
    /// A brand-new run.
    #[wasm_bindgen(constructor)]
    pub fn new(script_name: &str, max_memory: Option<usize>) -> Result<AgentSession, JsValue> {
        let limit = max_memory.unwrap_or(DEFAULT_MAX_MEMORY);
        // Arm the allocator. Failure here is not fatal — it means the memory
        // ceiling is unenforced, which the host may want to know about.
        let _ = monty_alloc::set_limit(Some(limit), false);
        Ok(Self {
            state: State::Idle(Box::new(MontyRepl::new(
                script_name,
                ResourceTracker::new(ResourceLimits::default().max_memory(limit)),
                CompileOptions::default(),
            ))),
            script: script_name.to_owned(),
        })
    }

    /// Rehydrate from a snapshot — the cold path after hibernation.
    pub fn restore(bytes: &[u8], max_memory: Option<usize>) -> Result<AgentSession, JsValue> {
        let limit = max_memory.unwrap_or(DEFAULT_MAX_MEMORY);
        let _ = monty_alloc::set_limit(Some(limit), false);
        let d = Dump::load(bytes).map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
        let state = match d.state {
            Session::Idle(r) => State::Idle(r),
            Session::Suspended(p) => State::Suspended(p),
            Session::Running(_) => return Err(JsValue::from_str("dump holds a one-shot run, not a repl")),
        };
        Ok(Self { state, script: d.script_name })
    }

    /// Is this session parked mid-tool-call?
    #[wasm_bindgen(getter)]
    pub fn suspended(&self) -> bool {
        matches!(self.state, State::Suspended(_))
    }

    /// The dump format version, so the host can record it alongside the bytes
    /// and route to a matching interpreter after a deploy (experiment D).
    #[wasm_bindgen(js_name = dumpVersion)]
    pub fn dump_version() -> u16 {
        monty::DUMP_VERSION
    }

    /// Serialize — works whether idle *or* suspended mid-tool-call, which is
    /// what lets a run park on an approval for weeks.
    pub fn snapshot(&self) -> Result<Vec<u8>, JsValue> {
        let r = match &self.state {
            State::Idle(repl) => dump(&self.script, None, SessionRef::Idle(repl)),
            State::Suspended(p) => dump(&self.script, None, SessionRef::Suspended(p)),
            State::Taken => return Err(JsValue::from_str("session is in an invalid state")),
        };
        r.map_err(|e| JsValue::from_str(&format!("{e:?}")))
    }

    /// Feed a snippet and run until it needs the host or finishes.
    pub fn start(&mut self, code: &str) -> Result<JsValue, JsValue> {
        let State::Idle(repl) = std::mem::replace(&mut self.state, State::Taken) else {
            return Err(JsValue::from_str("session is busy: resume the pending tool call first"));
        };
        match repl.feed_start(code, vec![], PrintWriter::Disabled) {
            Ok(p) => self.settle(p),
            Err(e) => {
                let step = Step::Error { error: summarize(&e.error), exc_type: format!("{:?}", e.error.exc_type()) };
                self.state = State::Idle(Box::new(e.repl));
                to_js(&step)
            }
        }
    }

    /// Resume a pending tool call with a JSON-ish value from the host.
    pub fn resume(&mut self, value: JsValue) -> Result<JsValue, JsValue> {
        self.resume_inner(value, false, "")
    }

    /// Resume a pending tool call by raising a *catchable* exception in the
    /// guest — the "your tool failed" path.
    #[wasm_bindgen(js_name = resumeError)]
    pub fn resume_error(&mut self, message: &str) -> Result<JsValue, JsValue> {
        self.resume_inner(JsValue::NULL, true, message)
    }

    fn resume_inner(&mut self, value: JsValue, is_error: bool, message: &str) -> Result<JsValue, JsValue> {
        let State::Suspended(progress) = std::mem::replace(&mut self.state, State::Taken) else {
            return Err(JsValue::from_str("session is not suspended"));
        };
        let obj = if is_error {
            MontyObject::None
        } else {
            let json: serde_json::Value = serde_wasm_bindgen::from_value(value)
                .map_err(|e| JsValue::from_str(&format!("tool result was not JSON-compatible: {e}")))?;
            json_to_monty(&json)
        };

        let next = match *progress {
            ReplProgress::FunctionCall(call) => {
                let answer = if is_error {
                    ExtFunctionResult::Error(MontyException::new(ExcType::RuntimeError, Some(message.to_owned())))
                } else {
                    ExtFunctionResult::Return(obj)
                };
                call.resume(answer, PrintWriter::Disabled)
            }
            ReplProgress::NameLookup(lookup) => {
                let answer = if is_error {
                    NameLookupResult::Error(MontyException::new(ExcType::RuntimeError, Some(message.to_owned())))
                } else {
                    NameLookupResult::Value(obj)
                };
                lookup.resume(answer, PrintWriter::Disabled)
            }
            other => {
                self.state = State::Idle(Box::new(other.into_repl()));
                return Err(JsValue::from_str("pending suspension is not resumable by this host"));
            }
        };

        match next {
            Ok(p) => self.settle(p),
            Err(e) => {
                let step = Step::Error { error: summarize(&e.error), exc_type: format!("{:?}", e.error.exc_type()) };
                self.state = State::Idle(Box::new(e.repl));
                to_js(&step)
            }
        }
    }

    /// Tell the guest the tool does not exist, producing a catchable `NameError`.
    #[wasm_bindgen(js_name = resumeNotFound)]
    pub fn resume_not_found(&mut self) -> Result<JsValue, JsValue> {
        let State::Suspended(progress) = std::mem::replace(&mut self.state, State::Taken) else {
            return Err(JsValue::from_str("session is not suspended"));
        };
        let next = match *progress {
            ReplProgress::FunctionCall(call) => {
                let name = call.function_name.clone();
                call.resume(ExtFunctionResult::NotFound(name), PrintWriter::Disabled)
            }
            ReplProgress::NameLookup(lookup) => lookup.resume(NameLookupResult::Undefined, PrintWriter::Disabled),
            other => {
                self.state = State::Idle(Box::new(other.into_repl()));
                return Err(JsValue::from_str("not resumable"));
            }
        };
        match next {
            Ok(p) => self.settle(p),
            Err(e) => {
                let step = Step::Error { error: summarize(&e.error), exc_type: format!("{:?}", e.error.exc_type()) };
                self.state = State::Idle(Box::new(e.repl));
                to_js(&step)
            }
        }
    }

    /// Park the progress and describe it to JS.
    fn settle(&mut self, progress: ReplProgress) -> Result<JsValue, JsValue> {
        let step = match progress {
            ReplProgress::Complete { repl, value } => {
                self.state = State::Idle(Box::new(repl));
                Step::Done { value: monty_to_json(&value) }
            }
            ReplProgress::FunctionCall(call) => {
                let step = Step::Tool {
                    name: call.function_name.clone(),
                    args: call.args.iter().map(monty_to_json).collect(),
                    call_id: call.call_id,
                };
                self.state = State::Suspended(Box::new(ReplProgress::FunctionCall(call)));
                step
            }
            ReplProgress::NameLookup(lookup) => {
                let step = Step::Name { name: lookup.name.clone() };
                self.state = State::Suspended(Box::new(ReplProgress::NameLookup(lookup)));
                step
            }
            other => {
                // OS calls and async futures are not granted by this host.
                self.state = State::Idle(Box::new(other.into_repl()));
                Step::Error {
                    error: "guest requested a capability this host does not grant".to_owned(),
                    exc_type: "Unsupported".to_owned(),
                }
            }
        };
        to_js(&step)
    }
}

/// Note the `json_compatible` serializer. The default encodes maps as JS `Map`
/// objects, so a guest dict reaches a tool as a `Map` and `JSON.stringify` of it
/// yields `{}` — data loss that is silent and easy to miss. Plain objects are
/// what a JS host actually wants at this boundary.
fn to_js<T: Serialize>(v: &T) -> Result<JsValue, JsValue> {
    let ser = serde_wasm_bindgen::Serializer::json_compatible();
    v.serialize(&ser).map_err(|e| JsValue::from_str(&e.to_string()))
}

fn summarize(e: &MontyException) -> String {
    e.to_string().lines().next_back().unwrap_or_default().trim().to_owned()
}

/// Lossy on purpose: the host boundary is JSON, so guest values that have no
/// JSON shape arrive as their repr string rather than failing the turn.
fn monty_to_json(v: &MontyObject) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        MontyObject::None => J::Null,
        MontyObject::Bool(b) => J::Bool(*b),
        MontyObject::Int(i) => J::Number((*i).into()),
        MontyObject::Float(f) => serde_json::Number::from_f64(*f).map_or(J::Null, J::Number),
        MontyObject::String(s) => J::String(s.clone()),
        MontyObject::List(xs) | MontyObject::Tuple(xs) | MontyObject::Set(xs) | MontyObject::FrozenSet(xs) => {
            J::Array(xs.iter().map(monty_to_json).collect())
        }
        MontyObject::Dict(pairs) => J::Object(
            pairs
                .iter()
                .map(|(k, val)| (key_to_string(k), monty_to_json(val)))
                .collect(),
        ),
        other => J::String(format!("{other:?}")),
    }
}

fn key_to_string(k: &MontyObject) -> String {
    match k {
        MontyObject::String(s) => s.clone(),
        MontyObject::Int(i) => i.to_string(),
        MontyObject::Bool(b) => b.to_string(),
        other => format!("{other:?}"),
    }
}

fn json_to_monty(v: &serde_json::Value) -> MontyObject {
    use serde_json::Value as J;
    match v {
        J::Null => MontyObject::None,
        J::Bool(b) => MontyObject::Bool(*b),
        J::Number(n) => n
            .as_i64()
            .map(MontyObject::Int)
            .or_else(|| n.as_f64().map(MontyObject::Float))
            .unwrap_or(MontyObject::None),
        J::String(s) => MontyObject::String(s.clone()),
        J::Array(xs) => MontyObject::List(xs.iter().map(json_to_monty).collect()),
        J::Object(map) => MontyObject::Dict(
            map.iter()
                .map(|(k, val)| (MontyObject::String(k.clone()), json_to_monty(val)))
                .collect::<Vec<_>>()
                .into(),
        ),
    }
}
