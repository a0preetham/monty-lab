//! Q2 — suspend/resume through a host function, and where the `MontyRepl` lives
//! while it is suspended.
//!
//! The ownership story is the whole point: `feed_start` takes `self` by value,
//! so between the suspension and the resume the host holds *no* `MontyRepl` at
//! all — it holds a `ReplProgress` that has swallowed it.

use monty::{MontyRepl, ReplProgress, Session, SessionRef, dump};
use monty_types::{ExtFunctionResult, MontyObject};

use crate::{new_repl, quiet, say, Probe};

#[derive(Debug)]
pub struct Facts {
    /// Names of the host calls observed, in order, for the multi-call snippet.
    pub call_sequence: Vec<String>,
    pub final_value: MontyObject,
    /// The host-supplied value, read back in a *later* feed.
    pub value_visible_next_feed: MontyObject,
    /// A suspension raised from inside a function defined in an earlier feed.
    pub call_from_earlier_function: String,
    /// `ExtFunctionResult::Error` surfaces as a catchable Python exception.
    pub host_error_is_catchable: MontyObject,
    /// `ExtFunctionResult::NotFound` surfaces as `NameError`.
    pub host_not_found: MontyObject,
    /// After abandoning a suspended snippet with `into_repl()`, is the session
    /// still usable, and are pre-suspension bindings intact?
    pub state_after_abandon: MontyObject,
    /// Round-tripping a *suspended* session through `dump()`/`Dump::load()`.
    pub resumed_from_dump: MontyObject,
    /// `tracker()` is readable at the suspension point without consuming it.
    pub tracker_readable_while_suspended: bool,
}

pub fn run(p: &mut Probe) -> Facts {
    let mut repl = new_repl();
    repl.feed_run("scale = 2", vec![], quiet()).unwrap();

    say!(p, "-- one feed, two host calls, value flows back into guest code --");
    // `weather` and `tax` were never declared anywhere: see q3.
    const CODE: &str = "raw = weather('paris')\nadjusted = raw * scale + tax()\nadjusted";

    // feed_start CONSUMES the repl. From here until Complete we do not own one.
    let mut progress: ReplProgress = repl.feed_start(CODE, vec![], quiet()).unwrap();
    say!(p, "  repl.feed_start(..) consumed the MontyRepl; we now hold a ReplProgress");

    let mut call_sequence = Vec::new();
    let mut tracker_readable_while_suspended = false;

    let (repl, final_value) = loop {
        match progress {
            ReplProgress::FunctionCall(call) => {
                // `tracker()` lives on ReplProgress, so read it *before* matching.
                call_sequence.push(call.function_name.clone());
                say!(
                    p,
                    "  suspended: {}({:?})  call_id={}",
                    call.function_name,
                    call.args,
                    call.call_id
                );
                let answer = match call.function_name.as_str() {
                    "weather" => MontyObject::Int(20),
                    "tax" => MontyObject::Int(2),
                    other => panic!("unexpected host call {other}"),
                };
                say!(p, "    host answers {answer:?}, resume() consumes the call");
                // resume() consumes the ReplFunctionCall and hands back a new
                // ReplProgress — the MontyRepl moves along inside it.
                progress = call.resume(answer, quiet()).unwrap();
            }
            ReplProgress::Complete { repl, value } => {
                say!(p, "  Complete: the MontyRepl comes back out, owned, with {value:?}");
                break (repl, value);
            }
            other => {
                tracker_readable_while_suspended = other.tracker().max_suspensions() > 0;
                panic!("unexpected progress variant");
            }
        }
    };
    let mut repl = repl;

    // prove tracker() is callable on a live suspension without consuming it
    let probe_progress = repl.feed_start("weather('x')", vec![], quiet()).unwrap();
    let elapsed = probe_progress.tracker().elapsed();
    let max_susp = probe_progress.tracker().max_suspensions();
    say!(
        p,
        "  progress.tracker(): elapsed={elapsed:?} max_suspensions={max_susp} (borrow, not consume)"
    );
    tracker_readable_while_suspended = true;
    // abandon this one: into_repl() gives the session back, snippet discarded
    let mut repl2 = probe_progress.into_repl();
    say!(p, "  progress.into_repl() abandoned the in-flight snippet and returned the session");
    let state_after_abandon = repl2
        .feed_run("(scale, adjusted)", vec![], quiet())
        .unwrap();
    say!(p, "  after abandon, (scale, adjusted) -> {state_after_abandon:?}");
    repl = repl2;

    let value_visible_next_feed = repl.feed_run("adjusted", vec![], quiet()).unwrap();
    say!(p, "  next feed sees the host-derived global: adjusted -> {value_visible_next_feed:?}");

    say!(p, "-- a host call raised from inside a function defined in an earlier feed --");
    repl.feed_run("def lookup(city):\n    return weather(city) + 1\n", vec![], quiet())
        .unwrap();
    let progress = repl.feed_start("lookup('rome')", vec![], quiet()).unwrap();
    let call = progress.into_function_call().expect("suspension inside user fn");
    let call_from_earlier_function = format!("{}({:?})", call.function_name, call.args);
    say!(p, "  suspended at {call_from_earlier_function}");
    let (mut repl, v) = call
        .resume(MontyObject::Int(9), quiet())
        .unwrap()
        .into_complete()
        .expect("completes");
    say!(p, "  -> {v:?}");

    say!(p, "-- host answers that are not values --");
    let progress = repl
        .feed_start(
            "try:\n    boom()\nexcept ValueError as e:\n    caught = f'caught {e}'\ncaught",
            vec![],
            quiet(),
        )
        .unwrap();
    let call = progress.into_function_call().unwrap();
    let exc = monty_types::MontyException::new(
        monty_types::ExcType::ValueError,
        Some("host said no".to_owned()),
    );
    let (mut repl, host_error_is_catchable) = call
        .resume(ExtFunctionResult::Error(exc), quiet())
        .unwrap()
        .into_complete()
        .expect("the sandbox caught it");
    say!(p, "  ExtFunctionResult::Error -> {host_error_is_catchable:?}");

    let progress = repl
        .feed_start(
            "try:\n    no_such_tool()\nexcept NameError as e:\n    missing = f'NameError: {e}'\nmissing",
            vec![],
            quiet(),
        )
        .unwrap();
    let call = progress.into_function_call().unwrap();
    let (repl, host_not_found) = call
        .resume(
            ExtFunctionResult::NotFound(call_name_placeholder()),
            quiet(),
        )
        .unwrap()
        .into_complete()
        .expect("the sandbox caught it");
    say!(p, "  ExtFunctionResult::NotFound -> {host_not_found:?}");

    say!(p, "-- a SUSPENDED session survives dump()/Dump::load() and resumes elsewhere --");
    let progress = repl.feed_start("checkpoint = weather('oslo') * 10\ncheckpoint", vec![], quiet()).unwrap();
    let bytes = dump("agent-session.py", None, SessionRef::Suspended(&progress)).unwrap();
    say!(p, "  dumped a mid-tool-call session: {} bytes", bytes.len());
    drop(progress); // the original session is thrown away entirely
    let Session::Suspended(restored) = monty::Dump::load(&bytes).unwrap().state else {
        panic!("expected a suspended session")
    };
    let call = restored.into_function_call().unwrap();
    say!(p, "  restored, still suspended at {}({:?})", call.function_name, call.args);
    let (mut repl, resumed_from_dump) = call
        .resume(MontyObject::Int(3), quiet())
        .unwrap()
        .into_complete()
        .unwrap();
    say!(p, "  resumed to completion -> {resumed_from_dump:?}");
    let carried = repl.feed_run("(scale, checkpoint)", vec![], quiet()).unwrap();
    say!(p, "  the restored session still has the old globals: {carried:?}");

    say!(p, "-- the error path also returns the session --");
    let err = repl
        .feed_start("half = 1\nraise RuntimeError('late')", vec![], quiet())
        .expect_err("raises");
    say!(p, "  Err(Box<ReplStartError>) with .repl and .error: {}", crate::exc_summary(&err.error));
    let mut repl: MontyRepl = err.repl;
    let half = repl.feed_run("half", vec![], quiet()).unwrap();
    say!(p, "  recovered session still holds pre-raise binding half -> {half:?}");

    Facts {
        call_sequence,
        final_value,
        value_visible_next_feed,
        call_from_earlier_function,
        host_error_is_catchable,
        host_not_found,
        state_after_abandon,
        resumed_from_dump,
        tracker_readable_while_suspended,
    }
}

fn call_name_placeholder() -> String {
    "no_such_tool".to_owned()
}
