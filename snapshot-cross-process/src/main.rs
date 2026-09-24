//! Experiment B — snapshot round-trip across a *process* boundary.
//!
//! Each stage is a separate invocation of this binary, so the interpreter state
//! genuinely crosses an OS process boundary through bytes on disk and nothing
//! else. `./run.sh` drives the sequence.
//!
//! The two legs that matter:
//!   * stage2 continues feeding a session built by stage1, in a fresh process.
//!   * stage4 resumes a session that stage3 left *suspended mid-tool-call*, and
//!     answers with a different implementation of the same external function.

use std::path::Path;
use std::process::ExitCode;

use monty::{dump, Dump, MontyRepl, ReplProgress, Session, SessionRef};
use monty_types::{CompileOptions, MontyObject, PrintWriter, ResourceTracker};

const SCRIPT: &str = "agent-run.py";

fn quiet() -> PrintWriter<'static> {
    PrintWriter::Disabled
}

fn new_repl() -> MontyRepl {
    MontyRepl::new(SCRIPT, ResourceTracker::default(), CompileOptions::default())
}

fn exc(e: &monty_types::MontyException) -> String {
    e.to_string().lines().next_back().unwrap_or_default().trim().to_owned()
}

/// State a real agent run would accumulate: config, a tool-result cache, a
/// couple of user-defined helpers, a class, and some rows of data.
const AGENT_STATE: &[&str] = &[
    "run_id = 'run_01HQ8XYZ'",
    "config = {'model': 'claude-opus-5', 'max_steps': 12, 'temperature': 0.0}",
    "history = []",
    "def record(step, result):\n    history.append({'step': step, 'result': result})\n    return len(history)\n",
    "class Budget:\n    def __init__(self, total):\n        self.total = total\n        self.spent = 0\n\n    def charge(self, n):\n        self.spent += n\n        return self.total - self.spent\n\n    def __repr__(self):\n        return f'Budget({self.spent}/{self.total})'\n",
    "budget = Budget(1000)",
    "record('bootstrap', 'ok')",
    "budget.charge(120)",
    "rows = [{'id': i, 'name': 'item-' + str(i), 'score': i * 1.5} for i in range(50)]",
    "import math\nnorm = math.sqrt(sum(r['score'] ** 2 for r in rows))",
];

fn build_state(repl: &mut MontyRepl) {
    for snippet in AGENT_STATE {
        repl.feed_run(snippet, vec![], quiet())
            .unwrap_or_else(|e| panic!("building state failed on {snippet:?}: {}", exc(&e)));
    }
}

/// stage1 — build an agent session from scratch and dump it Idle.
fn stage1(path: &Path) -> ExitCode {
    let mut repl = new_repl();
    build_state(&mut repl);
    let probe = repl.feed_run("(run_id, len(history), repr(budget), len(rows))", vec![], quiet()).unwrap();
    println!("  state built in this process: {probe:?}");

    let bytes = dump(SCRIPT, None, SessionRef::Idle(&repl)).expect("dump");
    std::fs::write(path, &bytes).expect("write dump");
    println!("  dumped Idle session: {} bytes -> {}", bytes.len(), path.display());
    println!("  header: magic={:?} dump_version={}", &bytes[..6], u16::from_le_bytes([bytes[6], bytes[7]]));
    ExitCode::SUCCESS
}

/// stage2 — FRESH PROCESS. Load, verify every kind of state survived, keep feeding.
fn stage2(path: &Path) -> ExitCode {
    let bytes = std::fs::read(path).expect("read dump");
    println!("  loading {} bytes in a process that never built this state", bytes.len());
    let loaded = Dump::load(&bytes).expect("load");
    println!("  dump.script_name = {:?}, type_check = {:?}", loaded.script_name, loaded.type_check);

    let Session::Idle(repl) = loaded.state else {
        eprintln!("  FAIL: expected Session::Idle");
        return ExitCode::FAILURE;
    };
    let mut repl = *repl;

    let mut ok = true;
    let mut check = |label: &str, code: &str, want: &str| {
        let got = match repl.feed_run(code, vec![], quiet()) {
            Ok(v) => format!("{v:?}"),
            Err(e) => format!("Err({})", exc(&e)),
        };
        let good = got == want;
        ok &= good;
        println!("  [{}] {label}: {got}", if good { "ok" } else { "FAIL" });
        if !good {
            println!("        expected {want}");
        }
    };

    check("plain global", "run_id", "String(\"run_01HQ8XYZ\")");
    check("dict config", "config['model']", "String(\"claude-opus-5\")");
    check("list built earlier", "len(rows)", "Int(50)");
    check("float from math", "round(norm, 4)", "Float(301.5895)");
    // Functions and classes are the interesting ones: code objects, not data.
    check("user function still callable", "record('after-restore', 'ok')", "Int(2)");
    check("class instance method", "budget.charge(80)", "Int(800)");
    check("instance repr survives", "repr(budget)", "String(\"Budget(200/1000)\")");
    check("isinstance against restored class", "isinstance(budget, Budget)", "Bool(true)");
    // Defining new things on top of restored state.
    check(
        "new class over restored state",
        "class Tally:\n    def __init__(self):\n        self.n = len(history)\n\n    def bump(self):\n        self.n += 1\n        return self.n\nTally().bump()",
        "Int(3)",
    );
    check("new function closing over restored global", "def spent_ratio():\n    return budget.spent / config['max_steps']\nspent_ratio()", "Float(16.666666666666668)");

    let bytes2 = dump(SCRIPT, None, SessionRef::Idle(&repl)).expect("redump");
    println!("  re-dumped after continuing: {} bytes", bytes2.len());
    std::fs::write(path.with_extension("stage2.dump"), &bytes2).expect("write");

    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// stage3 — build state, start a snippet that calls a host tool, and dump while
/// SUSPENDED. This process never resolves the call.
fn stage3(path: &Path) -> ExitCode {
    let mut repl = new_repl();
    build_state(&mut repl);
    repl.feed_run("pending_note = 'set before the tool call'", vec![], quiet()).unwrap();

    let code = "price = fetch_price('WIDGET')\nremaining = budget.charge(price)\nrecord('purchase', price)\n(price, remaining, len(history))";
    let progress = repl.feed_start(code, vec![], quiet()).expect("start");
    let ReplProgress::FunctionCall(ref call) = progress else {
        eprintln!("  FAIL: expected to suspend on fetch_price");
        return ExitCode::FAILURE;
    };
    println!("  suspended mid-snippet at {}({:?}), call_id={}", call.function_name, call.args, call.call_id);

    let bytes = dump(SCRIPT, None, SessionRef::Suspended(&progress)).expect("dump suspended");
    std::fs::write(path, &bytes).expect("write");
    println!("  dumped SUSPENDED session: {} bytes", bytes.len());
    println!("  this process is exiting without ever resolving fetch_price");
    ExitCode::SUCCESS
}

/// stage4 — FRESH PROCESS with a *different implementation* of `fetch_price`.
/// The dump records the paused function's name, never the host code, so the
/// same name resolves to a different body here.
fn stage4(path: &Path, price: i64) -> ExitCode {
    let bytes = std::fs::read(path).expect("read dump");
    let loaded = Dump::load(&bytes).expect("load");
    let Session::Suspended(progress) = loaded.state else {
        eprintln!("  FAIL: expected Session::Suspended");
        return ExitCode::FAILURE;
    };
    let ReplProgress::FunctionCall(call) = *progress else {
        eprintln!("  FAIL: expected a suspended FunctionCall");
        return ExitCode::FAILURE;
    };
    println!("  restored, still paused at {}({:?})", call.function_name, call.args);
    println!("  this process answers {price} — a different implementation, same name");

    let (mut repl, value) = match call.resume(MontyObject::Int(price), quiet()) {
        Ok(p) => match p.into_complete() {
            Some(pair) => pair,
            None => {
                eprintln!("  FAIL: did not complete");
                return ExitCode::FAILURE;
            }
        },
        Err(e) => {
            eprintln!("  FAIL: resume errored: {}", exc(&e.error));
            return ExitCode::FAILURE;
        }
    };
    println!("  snippet completed -> {value:?}");

    let mut ok = true;
    for (label, code, want) in [
        ("pre-suspension binding", "pending_note", "String(\"set before the tool call\")"),
        ("host value landed in guest state", "price", &format!("Int({price})")[..]),
        ("class mutated by the resumed snippet", "repr(budget)", &format!("Budget({}/1000)", 120 + price)[..]),
        ("session still feedable after resume", "record('post-resume', 'ok')", "Int(3)"),
    ] {
        let got = match repl.feed_run(code, vec![], quiet()) {
            Ok(v) => match v {
                MontyObject::String(ref s) if label.contains("class mutated") => format!("{s}"),
                other => format!("{other:?}"),
            },
            Err(e) => format!("Err({})", exc(&e)),
        };
        let good = got == want;
        ok &= good;
        println!("  [{}] {label}: {got}", if good { "ok" } else { "FAIL" });
        if !good {
            println!("        expected {want}");
        }
    }

    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// stage5 — how big is a dump for realistic agent state?
fn stage5() -> ExitCode {
    let mut repl = new_repl();
    let empty = dump(SCRIPT, None, SessionRef::Idle(&repl)).unwrap().len();
    println!("  {:<44} {:>9}", "empty session", empty);

    build_state(&mut repl);
    let base = dump(SCRIPT, None, SessionRef::Idle(&repl)).unwrap().len();
    println!("  {:<44} {:>9}", "typical agent state (50 rows, class, fns)", base);

    for n in [100usize, 1_000, 10_000] {
        let mut r = new_repl();
        build_state(&mut r);
        r.feed_run(
            &format!("bulk = [{{'id': i, 'name': 'item-' + str(i), 'score': i * 1.5}} for i in range({n})]"),
            vec![],
            quiet(),
        )
        .unwrap();
        let size = dump(SCRIPT, None, SessionRef::Idle(&r)).unwrap().len();
        println!("  {:<44} {:>9}  (+{} over base)", format!("+ {n} dict rows"), size, size - base);
    }

    // Many turns, little data — the long-lived-session shape.
    let mut r = new_repl();
    build_state(&mut r);
    for i in 0..500 {
        r.feed_run(&format!("record('step-{i}', {i})"), vec![], quiet()).unwrap();
    }
    let many = dump(SCRIPT, None, SessionRef::Idle(&r)).unwrap().len();
    println!("  {:<44} {:>9}  (+{} over base)", "+ 500 feeds appending to history", many, many - base);

    println!("\n  Durable Object SQLite allows 1 GB (free) / 10 GB (paid) per object.");
    println!("  A {}-byte snapshot is {:.6}% of the free-plan budget.", base, base as f64 / 1_073_741_824.0 * 100.0);
    ExitCode::SUCCESS
}

/// stage6 — what a version-mismatched dump actually does, since that is the
/// whole premise of the multi-version plan (workstream D).
fn stage6(path: &Path) -> ExitCode {
    let mut bytes = std::fs::read(path).expect("read dump");
    println!("  monty::DUMP_VERSION in this build = {}", monty::DUMP_VERSION);
    println!("  genuine dump loads: {}", Dump::load(&bytes).is_ok());

    // Rewrite the version field to something this build cannot read.
    bytes[6..8].copy_from_slice(&7u16.to_le_bytes());
    match Dump::load(&bytes) {
        Ok(_) => {
            eprintln!("  FAIL: a v7 dump was accepted by a v{} build", monty::DUMP_VERSION);
            return ExitCode::FAILURE;
        }
        Err(e) => println!("  header says v7 -> {e:?}"),
    }

    // And a non-dump.
    match Dump::load(b"not a monty dump at all") {
        Ok(_) => {
            eprintln!("  FAIL: garbage accepted");
            return ExitCode::FAILURE;
        }
        Err(e) => println!("  garbage input   -> {e:?}"),
    }
    println!("  => the header is readable before deserialization, so a dispatcher can");
    println!("     route on version without a matching interpreter. (workstream D)");
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let stage = args.get(1).map(String::as_str).unwrap_or("");
    let path = Path::new(args.get(2).map(String::as_str).unwrap_or("/tmp/monty-b.dump"));
    match stage {
        "stage1" => stage1(path),
        "stage2" => stage2(path),
        "stage3" => stage3(path),
        "stage4" => stage4(path, args.get(3).and_then(|s| s.parse().ok()).unwrap_or(250)),
        "stage5" => stage5(),
        "stage6" => stage6(path),
        _ => {
            eprintln!("usage: {} <stage1|stage2|stage3|stage4|stage5|stage6> [dumpfile] [price]", args[0]);
            ExitCode::FAILURE
        }
    }
}
