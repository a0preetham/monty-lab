//! Q3 — does the host tool set have to be fixed when the session is created?
//!
//! The handoff treats monty issue #190 ("external fns occupy the first N
//! contiguous global slots with no setter") as a blocker: an agent varies its
//! tool set per turn, and that was said to be inexpressible.
//!
//! Two candidate mechanisms exist in 0.0.23, and this module tells them apart:
//!
//!   * `inputs: Vec<(String, MontyObject)>` — a *per-feed* argument. But
//!     `MontyObject` has no callable variant (see `inputs_cannot_carry_a_tool`),
//!     so this can seed data, not functions.
//!   * suspension — an unresolved name suspends the snippet and asks the host
//!     to resolve it *at call time*. Nothing is declared up front.
//!
//! The question that decides the agent loop's design is whether resolution is
//! re-asked on every call or cached after the first, since `ReplNameLookup`'s
//! docs say it "caches the resolved value in the namespace slot".

use monty::{MontyRepl, ReplProgress};
use monty_types::{ExtFunctionResult, MontyObject, NameLookupResult};

use crate::{new_repl, quiet, say, Probe};

#[derive(Debug)]
pub struct Facts {
    /// Which `ReplProgress` variant an undeclared `foo(...)` call produces.
    pub call_suspension_variant: String,
    /// Which variant a bare undeclared *name* (no call) produces.
    pub bare_name_suspension_variant: String,
    /// Every (feed, name) the host was asked to resolve, in order. If the host
    /// is re-asked on each feed, varying tool sets are expressible.
    pub resolution_log: Vec<String>,
    /// Feed 3 calls a tool that did not exist when the session was created.
    pub late_added_tool: MontyObject,
    /// Same name, different host implementation on a later feed.
    pub rebound_tool_first: MontyObject,
    pub rebound_tool_second: MontyObject,
    /// Is the host re-asked for a name it already resolved in an earlier feed?
    pub reasked_after_first_resolution: bool,
    /// Withdrawing a tool: host answers `NotFound`/`Undefined` for a name it
    /// previously resolved.
    pub withdrawn_tool: String,
    /// Data (not functions) seeded through the per-feed `inputs` argument.
    pub inputs_seeded_value: MontyObject,
    /// Does an `inputs` binding from feed 1 persist into feed 2?
    pub inputs_persist_next_feed: String,
}

/// Drives one snippet to completion, letting `resolve` answer every suspension.
/// Returns the value plus a log of what the host was asked for.
fn feed_with_host(
    repl: MontyRepl,
    code: &str,
    inputs: Vec<(String, MontyObject)>,
    mut resolve: impl FnMut(&str, &[MontyObject]) -> Option<MontyObject>,
    p: &mut Probe,
) -> (MontyRepl, Result<MontyObject, String>, Vec<String>) {
    let mut asked = Vec::new();
    let mut progress = match repl.feed_start(code, inputs, quiet()) {
        Ok(pr) => pr,
        Err(e) => {
            let msg = crate::exc_summary(&e.error);
            return (e.repl, Err(msg), asked);
        }
    };
    loop {
        match progress {
            ReplProgress::FunctionCall(call) => {
                asked.push(format!("FunctionCall:{}", call.function_name));
                say!(p, "    host asked for FunctionCall {}({:?})", call.function_name, call.args);
                let answer = match resolve(&call.function_name, &call.args) {
                    Some(v) => ExtFunctionResult::Return(v),
                    None => ExtFunctionResult::NotFound(call.function_name.clone()),
                };
                progress = match call.resume(answer, quiet()) {
                    Ok(pr) => pr,
                    Err(e) => {
                        let msg = crate::exc_summary(&e.error);
                        return (e.repl, Err(msg), asked);
                    }
                };
            }
            ReplProgress::NameLookup(lookup) => {
                asked.push(format!("NameLookup:{}", lookup.name));
                say!(p, "    host asked for NameLookup {:?}", lookup.name);
                let answer = match resolve(&lookup.name, &[]) {
                    Some(v) => NameLookupResult::Value(v),
                    None => NameLookupResult::Undefined,
                };
                progress = match lookup.resume(answer, quiet()) {
                    Ok(pr) => pr,
                    Err(e) => {
                        let msg = crate::exc_summary(&e.error);
                        return (e.repl, Err(msg), asked);
                    }
                };
            }
            ReplProgress::OsCall(os) => {
                asked.push("OsCall".to_owned());
                say!(p, "    host asked for an OsCall (unhandled here); abandoning");
                return (os.into_repl(), Err("unhandled OsCall".to_owned()), asked);
            }
            ReplProgress::ResolveFutures(f) => {
                asked.push("ResolveFutures".to_owned());
                return (f.into_repl(), Err("unhandled ResolveFutures".to_owned()), asked);
            }
            ReplProgress::Complete { repl, value } => return (repl, Ok(value), asked),
        }
    }
}

/// Name of the variant, without moving out of the progress.
fn variant_of(progress: &ReplProgress) -> &'static str {
    match progress {
        ReplProgress::FunctionCall(_) => "FunctionCall",
        ReplProgress::OsCall(_) => "OsCall",
        ReplProgress::ResolveFutures(_) => "ResolveFutures",
        ReplProgress::NameLookup(_) => "NameLookup",
        ReplProgress::Complete { .. } => "Complete",
    }
}

pub fn run(p: &mut Probe) -> Facts {
    let repl = new_repl();

    say!(p, "-- what does an undeclared call suspend as? --");
    let progress = repl
        .feed_start("weather('paris')", vec![], quiet())
        .expect("undeclared call should suspend, not fail");
    let call_suspension_variant = variant_of(&progress).to_owned();
    say!(p, "  weather('paris') -> ReplProgress::{call_suspension_variant}");
    let repl = progress.into_repl();

    say!(p, "-- and a bare undeclared name, with no call? --");
    let progress = repl
        .feed_start("some_undeclared_name", vec![], quiet())
        .expect("bare undeclared name should suspend");
    let bare_name_suspension_variant = variant_of(&progress).to_owned();
    say!(p, "  some_undeclared_name -> ReplProgress::{bare_name_suspension_variant}");
    let repl = progress.into_repl();

    let mut resolution_log = Vec::new();

    say!(p, "-- feed 1: tool set is {{weather}} --");
    let (repl, v1, asked) = feed_with_host(
        repl,
        "first = weather('paris')\nfirst",
        vec![],
        |name, _| (name == "weather").then_some(MontyObject::Int(20)),
        p,
    );
    resolution_log.extend(asked.iter().map(|a| format!("feed1 {a}")));
    let rebound_tool_first = v1.clone().unwrap_or(MontyObject::Repr("<err>".into()));
    say!(p, "  -> {v1:?}");

    say!(p, "-- feed 2: SAME name, different host implementation --");
    let (repl, v2, asked) = feed_with_host(
        repl,
        "second = weather('paris')\nsecond",
        vec![],
        |name, _| (name == "weather").then_some(MontyObject::Int(999)),
        p,
    );
    // If the host is not re-asked here, the first resolution was cached and the
    // tool set is effectively frozen after first use.
    let reasked_after_first_resolution = !asked.is_empty();
    resolution_log.extend(asked.iter().map(|a| format!("feed2 {a}")));
    let rebound_tool_second = v2.clone().unwrap_or(MontyObject::Repr("<err>".into()));
    say!(p, "  -> {v2:?}   (host re-asked: {reasked_after_first_resolution})");

    say!(p, "-- feed 3: a tool that did not exist when the session started --");
    let (repl, v3, asked) = feed_with_host(
        repl,
        "third = newly_added_tool(5)\nthird",
        vec![],
        |name, args| match name {
            "newly_added_tool" => Some(MontyObject::Int(match args.first() {
                Some(MontyObject::Int(n)) => n * 100,
                _ => -1,
            })),
            _ => None,
        },
        p,
    );
    resolution_log.extend(asked.iter().map(|a| format!("feed3 {a}")));
    let late_added_tool = v3.clone().unwrap_or(MontyObject::Repr("<err>".into()));
    say!(p, "  -> {v3:?}");

    say!(p, "-- feed 4: withdraw a tool the host previously answered --");
    let (repl, v4, asked) = feed_with_host(
        repl,
        "try:\n    weather('paris')\nexcept NameError as e:\n    withdrawn = 'NameError'\nwithdrawn",
        vec![],
        |_, _| None, // host now refuses everything
        p,
    );
    resolution_log.extend(asked.iter().map(|a| format!("feed4 {a}")));
    let withdrawn_tool = match &v4 {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("Err({e})"),
    };
    say!(p, "  -> {withdrawn_tool}");

    say!(p, "-- the per-feed `inputs` argument: data only --");
    let mut repl = repl;
    let inputs_seeded_value = repl
        .feed_run(
            "seeded * 2",
            vec![("seeded".to_owned(), MontyObject::Int(21))],
            quiet(),
        )
        .unwrap_or(MontyObject::Repr("<err>".into()));
    say!(p, "  feed_run(\"seeded * 2\", inputs=[seeded=21]) -> {inputs_seeded_value:?}");

    let inputs_persist_next_feed = match repl.feed_run("seeded", vec![], quiet()) {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("Err({})", crate::exc_summary(&e)),
    };
    say!(p, "  next feed, no inputs: seeded -> {inputs_persist_next_feed}");

    Facts {
        call_suspension_variant,
        bare_name_suspension_variant,
        resolution_log,
        late_added_tool,
        rebound_tool_first,
        rebound_tool_second,
        reasked_after_first_resolution,
        withdrawn_tool,
        inputs_seeded_value,
        inputs_persist_next_feed,
    }
}

/// `MontyObject` — the only thing `inputs` can carry — has no callable variant.
/// Listed here so the claim is checked by the compiler rather than by prose:
/// if a future version adds one, this stops compiling and the note is wrong.
pub fn inputs_cannot_carry_a_tool() -> &'static str {
    "MontyObject is data-only (Int/Float/String/Bytes/List/Tuple/Dict/Set/Date/\
     Exception/Repr/...); there is no Function or Callable variant, so a host \
     tool cannot be passed through `inputs`. Tools arrive via suspension."
}
