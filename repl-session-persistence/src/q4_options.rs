//! Q4 — `CompileOptions`, `ResourceTracker`, and the `type_check` question.
//!
//! The handoff's open decision — "is `type_check` worth enabling given #872?" —
//! turns out not to arise on this hosting path at 0.0.23; see
//! `type_check_availability` below.

use std::time::Duration;

use monty::MontyRepl;
use monty_types::{
    CompileOptions, MontyObject, ResourceLimits, ResourceTracker, DEFAULT_MAX_RECURSION_DEPTH,
    DEFAULT_MAX_SUSPENSIONS,
};

use crate::{new_repl, quiet, say, Probe};

#[derive(Debug)]
pub struct Facts {
    pub default_max_recursion_depth: usize,
    pub default_max_suspensions: usize,
    pub default_max_duration: Option<Duration>,
    pub default_max_memory: Option<usize>,
    pub dump_version: u16,
    /// Does the suspension budget reset per feed, or accumulate over the life
    /// of the session? A long-lived agent run cares a great deal.
    pub suspensions_reset_per_feed: String,
    /// What a too-low recursion limit actually produces.
    pub recursion_limit_effect: String,
    /// Wall-clock limit enforcement.
    pub duration_limit_effect: String,
}

pub fn run(p: &mut Probe) -> Facts {
    let limits = ResourceLimits::default();
    say!(p, "-- ResourceLimits::default() --");
    say!(p, "  max_recursion_depth = {}", limits.max_recursion_depth);
    say!(p, "  max_suspensions     = {}", limits.max_suspensions);
    say!(p, "  max_duration        = {:?}", limits.max_duration);
    say!(p, "  max_memory          = {:?}  (needs monty-alloc as global allocator)", limits.max_memory);
    say!(p, "-- CompileOptions::default() --");
    say!(p, "  {:?}", CompileOptions::default());
    say!(p, "  (one field: assert_message_annotations — there is no type_check knob)");
    say!(p, "-- monty::DUMP_VERSION = {} --", monty::DUMP_VERSION);

    // Does the suspension budget accumulate across feeds?
    let mut repl = MontyRepl::new(
        "limits.py",
        ResourceTracker::new(ResourceLimits::default().max_suspensions(3)),
        CompileOptions::default(),
    );
    repl.feed_run("n = 0", vec![], quiet()).ok();
    let mut suspensions_seen = 0usize;
    let mut outcome = String::from("budget not exhausted");
    'feeds: for feed in 1..=4 {
        let mut progress = match repl.feed_start("tool()", vec![], quiet()) {
            Ok(pr) => pr,
            Err(e) => {
                outcome = format!(
                    "feed {feed} refused to start after {suspensions_seen} suspensions: {}",
                    crate::exc_summary(&e.error)
                );
                repl = e.repl;
                break 'feeds;
            }
        };
        loop {
            match progress {
                monty::ReplProgress::FunctionCall(call) => {
                    suspensions_seen += 1;
                    match call.resume(MontyObject::Int(1), quiet()) {
                        Ok(pr) => progress = pr,
                        Err(e) => {
                            outcome = format!(
                                "resume failed on feed {feed} after {suspensions_seen} suspensions: {}",
                                crate::exc_summary(&e.error)
                            );
                            repl = e.repl;
                            break 'feeds;
                        }
                    }
                }
                monty::ReplProgress::NameLookup(lookup) => {
                    suspensions_seen += 1;
                    match lookup.resume(MontyObject::Int(1).into(), quiet()) {
                        Ok(pr) => progress = pr,
                        Err(e) => {
                            outcome = format!(
                                "resume failed on feed {feed} after {suspensions_seen} suspensions: {}",
                                crate::exc_summary(&e.error)
                            );
                            repl = e.repl;
                            break 'feeds;
                        }
                    }
                }
                monty::ReplProgress::Complete { repl: r, .. } => {
                    repl = r;
                    break;
                }
                other => {
                    repl = other.into_repl();
                    break;
                }
            }
        }
    }
    let suspensions_reset_per_feed = format!("{suspensions_seen} suspensions over 4 feeds (cap 3): {outcome}");
    say!(p, "-- suspension budget across feeds --");
    say!(p, "  {suspensions_reset_per_feed}");

    // Recursion limit.
    let mut repl2 = MontyRepl::new(
        "recursion.py",
        ResourceTracker::new(ResourceLimits::default().max_recursion_depth(16)),
        CompileOptions::default(),
    );
    let recursion_limit_effect = match repl2.feed_run(
        "def down(n):\n    return 0 if n == 0 else down(n - 1)\ndown(200)",
        vec![],
        quiet(),
    ) {
        Ok(v) => format!("no error: {v:?}"),
        Err(e) => crate::exc_summary(&e),
    };
    say!(p, "-- max_recursion_depth(16), recurse 200 deep --");
    say!(p, "  {recursion_limit_effect}");

    // Duration limit. NOTE: on wasm32-unknown-unknown this path needs
    // web_time::Instant; see resource.rs and experiment E.
    let mut repl3 = MontyRepl::new(
        "duration.py",
        ResourceTracker::new(ResourceLimits::default().max_duration(Duration::from_millis(50))),
        CompileOptions::default(),
    );
    let duration_limit_effect = match repl3.feed_run(
        "total = 0\nfor i in range(50_000_000):\n    total += i\ntotal",
        vec![],
        quiet(),
    ) {
        Ok(v) => format!("no error (finished in time): {v:?}"),
        Err(e) => crate::exc_summary(&e),
    };
    say!(p, "-- max_duration(50ms), 50M-iteration loop --");
    say!(p, "  {duration_limit_effect}");

    // `heap_entry_count()` exists but is gated behind the `ref-count-return`
    // feature ("introspection for GC tests"), so it is not available to a
    // default-feature host. There is no heap-size introspection without it.
    let mut repl4 = new_repl();
    repl4
        .feed_run("rows = [{'i': i, 's': str(i)} for i in range(100)]", vec![], quiet())
        .ok();
    say!(p, "-- heap_entry_count() is gated behind feature `ref-count-return`; not read --");

    Facts {
        default_max_recursion_depth: DEFAULT_MAX_RECURSION_DEPTH,
        default_max_suspensions: DEFAULT_MAX_SUSPENSIONS,
        default_max_duration: limits.max_duration,
        default_max_memory: limits.max_memory,
        dump_version: monty::DUMP_VERSION,
        suspensions_reset_per_feed,
        recursion_limit_effect,
        duration_limit_effect,
    }
}

/// Where `type_check` actually lives at 0.0.23 — checked against the crate
/// graph, not assumed.
///
/// `CompileOptions` has exactly one field (`assert_message_annotations`); there
/// is no `type_check` toggle on the in-process Rust API. `monty` does not
/// depend on `monty-type-checking` at all. The only trace of type checking in
/// `monty` is `Dump::type_check: Option<TypeCheckState>` — a slot the dump
/// format carries so a *host that does* run the checker can persist its state.
///
/// So monty issue #872 (with `type_check=True`, a snippet ending in an uncaught
/// exception is never committed and its bindings are refused later) cannot be
/// reproduced through this API, because this API cannot turn type checking on.
/// The handoff's open decision does not arise for a Rust in-process host.
pub fn type_check_availability() -> &'static str {
    "not exposed on the 0.0.23 in-process Rust API; CompileOptions has no \
     type_check field and monty does not depend on monty-type-checking. #872 \
     is unreachable from here."
}
