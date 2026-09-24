//! Experiment C — an agent loop on top of `MontyRepl`.
//!
//! The two-stack model from the handoff: the agent loop is an *outer* stack
//! living outside the interpreter. Each turn the model emits a Python snippet,
//! the host feeds it, and any suspension is serviced as a tool call. Suspension
//! matters *within* a turn; session state is what stitches turns together.
//!
//! The model is scripted here — there are no API credentials in this
//! environment — so the transcript is canned while everything below it (the
//! loop, the tool registry, suspension handling, snapshot between turns) is
//! real. What this establishes is the *shape* of the host: what the guest-side
//! API looks like, and whether a tool set that changes every turn is workable.

use std::collections::HashMap;
use std::process::ExitCode;

use monty::{dump, Dump, MontyRepl, ReplProgress, Session, SessionRef};
use monty_types::{
    CompileOptions, ExcType, ExtFunctionResult, MontyException, MontyObject, NameLookupResult,
    PrintWriter, ResourceTracker,
};

const SCRIPT: &str = "agent-run.py";

fn quiet() -> PrintWriter<'static> {
    PrintWriter::Disabled
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

/// A host tool. Returning `Err` raises a *catchable* Python exception at the
/// call site, so guest code can `try`/`except` around a failing tool.
type Tool = Box<dyn Fn(&[MontyObject]) -> Result<MontyObject, MontyException>>;

/// The tool set for a single turn. Rebuilt every turn on purpose: the question
/// this experiment had to answer was whether that is expressible at all
/// (monty #190 says external fns occupy fixed global slots at session
/// creation). It is — nothing here is declared to the interpreter in advance.
#[derive(Default)]
struct ToolRegistry {
    tools: HashMap<String, Tool>,
}

impl ToolRegistry {
    fn with(mut self, name: &str, f: impl Fn(&[MontyObject]) -> Result<MontyObject, MontyException> + 'static) -> Self {
        self.tools.insert(name.to_owned(), Box::new(f));
        self
    }

    fn names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.tools.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }

    fn call(&self, name: &str, args: &[MontyObject]) -> Option<Result<MontyObject, MontyException>> {
        self.tools.get(name).map(|f| f(args))
    }
}

fn err(kind: ExcType, msg: &str) -> MontyException {
    MontyException::new(kind, Some(msg.to_owned()))
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// What a turn did, for the loop to report back to the model.
struct TurnOutcome {
    value: Result<String, String>,
    tool_calls: Vec<String>,
    printed: String,
}

/// Owns the interpreter for the life of the run.
///
/// The `Option` is not defensive: `feed_start` takes `MontyRepl` by value, so
/// the session genuinely has no repl in hand while a snippet is in flight. Every
/// exit path puts one back.
struct AgentSession {
    repl: Option<MontyRepl>,
}

impl AgentSession {
    fn new() -> Self {
        Self {
            repl: Some(MontyRepl::new(SCRIPT, ResourceTracker::default(), CompileOptions::default())),
        }
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let d = Dump::load(bytes).map_err(|e| format!("{e:?}"))?;
        match d.state {
            Session::Idle(r) => Ok(Self { repl: Some(*r) }),
            _ => Err("expected an idle session".to_owned()),
        }
    }

    fn to_bytes(&self) -> Vec<u8> {
        dump(SCRIPT, None, SessionRef::Idle(self.repl.as_ref().expect("idle"))).expect("dump")
    }

    /// Run one turn: feed the model's snippet, service every suspension against
    /// *this turn's* tool set, and return what happened.
    fn run_turn(&mut self, code: &str, tools: &ToolRegistry) -> TurnOutcome {
        let mut tool_calls = Vec::new();
        let mut printed = String::new();

        let repl = self.repl.take().expect("session has a repl between turns");
        let mut progress = match repl.feed_start(code, vec![], PrintWriter::collect_string(&mut printed)) {
            Ok(p) => p,
            Err(e) => {
                self.repl = Some(e.repl);
                return TurnOutcome {
                    value: Err(summarize(&e.error)),
                    tool_calls,
                    printed,
                };
            }
        };

        loop {
            match progress {
                // Calling an undeclared name. This is the tool path.
                ReplProgress::FunctionCall(call) => {
                    let answer = match tools.call(&call.function_name, &call.args) {
                        Some(Ok(v)) => {
                            tool_calls.push(format!("{}({}) -> {}", call.function_name, fmt_args(&call.args), fmt(&v)));
                            ExtFunctionResult::Return(v)
                        }
                        Some(Err(e)) => {
                            tool_calls.push(format!("{}({}) -> raised {}", call.function_name, fmt_args(&call.args), summarize(&e)));
                            ExtFunctionResult::Error(e)
                        }
                        // Not in this turn's tool set: the guest gets a NameError
                        // it can catch and route around.
                        None => {
                            tool_calls.push(format!("{}(..) -> not available this turn", call.function_name));
                            ExtFunctionResult::NotFound(call.function_name.clone())
                        }
                    };
                    progress = match call.resume(answer, PrintWriter::collect_string(&mut printed)) {
                        Ok(p) => p,
                        Err(e) => {
                            self.repl = Some(e.repl);
                            return TurnOutcome { value: Err(summarize(&e.error)), tool_calls, printed };
                        }
                    };
                }
                // A bare undeclared name. Resolution here is cached in the
                // namespace slot, so constants are fine but tools should not
                // come through this path.
                ReplProgress::NameLookup(lookup) => {
                    let answer = match tools.call(&lookup.name, &[]) {
                        Some(Ok(v)) => NameLookupResult::Value(v),
                        Some(Err(e)) => NameLookupResult::Error(e),
                        None => NameLookupResult::Undefined,
                    };
                    tool_calls.push(format!("<name {}>", lookup.name));
                    progress = match lookup.resume(answer, PrintWriter::collect_string(&mut printed)) {
                        Ok(p) => p,
                        Err(e) => {
                            self.repl = Some(e.repl);
                            return TurnOutcome { value: Err(summarize(&e.error)), tool_calls, printed };
                        }
                    };
                }
                ReplProgress::OsCall(os) => {
                    // Nothing in this host grants OS access; abandon the snippet
                    // but keep the session.
                    self.repl = Some(os.into_repl());
                    return TurnOutcome { value: Err("guest attempted an OS call".to_owned()), tool_calls, printed };
                }
                ReplProgress::ResolveFutures(f) => {
                    self.repl = Some(f.into_repl());
                    return TurnOutcome { value: Err("async not wired up in this host".to_owned()), tool_calls, printed };
                }
                ReplProgress::Complete { repl, value } => {
                    self.repl = Some(repl);
                    return TurnOutcome { value: Ok(fmt(&value)), tool_calls, printed };
                }
            }
        }
    }
}

fn fmt(v: &MontyObject) -> String {
    format!("{v:?}")
}

fn fmt_args(args: &[MontyObject]) -> String {
    args.iter().map(fmt).collect::<Vec<_>>().join(", ")
}

fn summarize(e: &MontyException) -> String {
    e.to_string().lines().next_back().unwrap_or_default().trim().to_owned()
}

// ---------------------------------------------------------------------------
// The scripted "model"
// ---------------------------------------------------------------------------

/// One scripted turn: what the model "writes", and which tools the host decides
/// to expose for it. The tool set deliberately changes shape turn to turn.
struct ScriptedTurn {
    note: &'static str,
    code: &'static str,
    tools: fn() -> ToolRegistry,
}

fn turn1_tools() -> ToolRegistry {
    ToolRegistry::default().with("search", |args| {
        let q = match args.first() {
            Some(MontyObject::String(s)) => s.clone(),
            _ => return Err(err(ExcType::TypeError, "search() takes one string")),
        };
        Ok(MontyObject::List(vec![
            MontyObject::String(format!("{q} result A")),
            MontyObject::String(format!("{q} result B")),
        ]))
    })
}

/// Turn 2 adds a tool that did not exist in turn 1 and drops `search`.
fn turn2_tools() -> ToolRegistry {
    ToolRegistry::default()
        .with("fetch_rows", |_| {
            Ok(MontyObject::List(
                (1..=5)
                    .map(|i| {
                        MontyObject::Dict(monty_types::DictPairs::from(vec![
                            (MontyObject::String("id".into()), MontyObject::Int(i)),
                            (MontyObject::String("amount".into()), MontyObject::Int(i * 40)),
                        ]))
                    })
                    .collect(),
            ))
        })
        .with("today", |_| Ok(MontyObject::String("2026-09-20".into())))
}

/// Turn 3 keeps `fetch_rows` but its implementation now fails, and adds a
/// write-tool that the guest is expected to guard.
fn turn3_tools() -> ToolRegistry {
    ToolRegistry::default()
        .with("fetch_rows", |_| Err(err(ExcType::RuntimeError, "upstream 503")))
        .with("commit_total", |args| match args.first() {
            Some(MontyObject::Int(n)) if *n >= 0 => Ok(MontyObject::String(format!("committed {n}"))),
            _ => Err(err(ExcType::ValueError, "commit_total needs a non-negative int")),
        })
}

/// Turn 4 exposes nothing at all — every tool has been withdrawn.
fn turn4_tools() -> ToolRegistry {
    ToolRegistry::default()
}

const SCRIPT_TURNS: &[ScriptedTurn] = &[
    ScriptedTurn {
        note: "model writes a helper and calls a tool; both must outlive the turn",
        code: "def summarize(items):\n    return {'n': len(items), 'first': items[0]}\n\nfindings = search('durable objects')\nsummarize(findings)",
        tools: turn1_tools,
    },
    ScriptedTurn {
        note: "new tool set: `search` is gone, `fetch_rows`/`today` are new; reuses turn 1's helper",
        code: "rows = fetch_rows()\nstamp = today()\ntotal = sum(r['amount'] for r in rows)\n(summarize(rows), stamp, total)",
        tools: turn2_tools,
    },
    ScriptedTurn {
        note: "same tool name, failing implementation; guest catches it and falls back to stored state",
        code: "try:\n    rows = fetch_rows()\n    source = 'fresh'\nexcept RuntimeError as e:\n    source = 'cached: ' + str(e)\nresult = commit_total(total)\n(source, result)",
        tools: turn3_tools,
    },
    ScriptedTurn {
        note: "every tool withdrawn; guest must degrade rather than crash",
        code: "try:\n    commit_total(total)\n    status = 'wrote'\nexcept NameError:\n    status = 'no tools available, using local state'\n(status, total, stamp)",
        tools: turn4_tools,
    },
];

// ---------------------------------------------------------------------------

fn main() -> ExitCode {
    let mut ok = true;
    let mut session = AgentSession::new();

    println!("=== agent loop: {} turns, tool set rebuilt each turn ===", SCRIPT_TURNS.len());

    for (i, turn) in SCRIPT_TURNS.iter().enumerate() {
        let tools = (turn.tools)();
        println!("\n--- turn {} ---", i + 1);
        println!("  note   : {}", turn.note);
        println!("  tools  : {:?}", tools.names());
        println!("  code   : {}", turn.code.replace('\n', "\n           "));

        let outcome = session.run_turn(turn.code, &tools);
        for c in &outcome.tool_calls {
            println!("  tool   : {c}");
        }
        if !outcome.printed.is_empty() {
            println!("  stdout : {}", outcome.printed.trim_end());
        }
        match &outcome.value {
            Ok(v) => println!("  result : {v}"),
            Err(e) => {
                println!("  ERROR  : {e}");
                ok = false;
            }
        }

        // Hibernate between turns, exactly as a Durable Object would: dump to
        // bytes, drop the interpreter, reload before the next turn.
        let bytes = session.to_bytes();
        drop(session);
        println!("  snapshot: {} bytes, session dropped and reloaded", bytes.len());
        session = match AgentSession::from_bytes(&bytes) {
            Ok(s) => s,
            Err(e) => {
                println!("  ERROR  : reload failed: {e}");
                return ExitCode::FAILURE;
            }
        };
    }

    println!("\n--- final state, after 4 turns and 4 hibernations ---");
    for probe in ["total", "stamp", "status", "source", "len(findings)", "summarize([1,2,3])"] {
        let out = session.run_turn(probe, &ToolRegistry::default());
        match out.value {
            Ok(v) => println!("  {probe:<22} -> {v}"),
            Err(e) => {
                println!("  {probe:<22} -> ERROR {e}");
                ok = false;
            }
        }
    }

    if ok {
        println!("\nALL TURNS PASSED");
        ExitCode::SUCCESS
    } else {
        println!("\nSOME TURNS FAILED");
        ExitCode::FAILURE
    }
}
