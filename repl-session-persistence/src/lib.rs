//! Experiment A — REPL confirmation for a Monty-backed agent runtime.
//!
//! Four questions, four modules, each exposing `run(&mut Probe) -> Facts`.
//! `src/main.rs` prints the probe transcript; `tests/` assert on the facts.
//! Nothing here is a library for other code to use — it is a record of what
//! `monty 0.0.23` actually does.

use monty::MontyRepl;
use monty_types::{CompileOptions, PrintWriter, ResourceTracker};

pub mod q1_session_state;
pub mod q2_suspend_resume;
pub mod q3_dynamic_tools;
pub mod q4_options;


/// Collects human-readable evidence lines as an experiment runs.
#[derive(Debug, Default)]
pub struct Probe {
    lines: Vec<String>,
}

impl Probe {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn say(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }

    #[must_use]
    pub fn lines(&self) -> &[String] {
        &self.lines
    }
}

/// `say!(probe, "x = {x}")`.
#[macro_export]
macro_rules! say {
    ($probe:expr, $($arg:tt)*) => {
        $probe.say(format!($($arg)*))
    };
}

/// The session shape an agent runtime would use: default limits, default
/// compile options. See `q4_options` for what those defaults contain.
#[must_use]
pub fn new_repl() -> MontyRepl {
    MontyRepl::new(
        "agent-session.py",
        ResourceTracker::default(),
        CompileOptions::default(),
    )
}

/// `print()` output is irrelevant to these probes; keep it off stdout so the
/// report stays readable.
#[must_use]
pub fn quiet() -> PrintWriter<'static> {
    PrintWriter::Disabled
}

/// First line of an exception — the traceback tail, which is the part that
/// identifies the error.
#[must_use]
pub fn exc_summary(e: &monty_types::MontyException) -> String {
    e.to_string()
        .lines()
        .next_back()
        .unwrap_or_default()
        .trim()
        .to_owned()
}
