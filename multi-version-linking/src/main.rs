//! Experiment D — two Monty versions linked into one binary, dispatched by
//! reading the dump header.
//!
//! The premise from the handoff: a deploy that bumps the interpreter makes every
//! existing snapshot unreadable, and for runs parked on an approval for weeks
//! there is no drain window. The fix is to keep old interpreters linked in and
//! route each snapshot to the version that can read it.
//!
//! Cargo makes this easier than expected here: every Monty release is `0.0.x`,
//! and Cargo treats each `0.0.x` patch as its own semver-incompatible range, so
//! `monty_v22` and `monty_v23` coexist as wholly separate crates — separate
//! types, separate statics, distinct symbols. The compiler refuses to mix them,
//! which is exactly the safety property wanted.

use std::process::ExitCode;

use monty_types_v22::{
    CompileOptions as CompileOptions22, PrintWriter as PrintWriter22, ResourceTracker as ResourceTracker22,
};
use monty_types_v23::{
    CompileOptions as CompileOptions23, PrintWriter as PrintWriter23, ResourceTracker as ResourceTracker23,
};

const SCRIPT: &str = "agent-run.py";
const MAGIC: &[u8] = b"MONTY\0";
const HEADER_LEN: usize = 8;

/// A live session, on whichever interpreter version owns it.
///
/// These two variants hold *different types that happen to share a name*. The
/// whole point is that they cannot be confused: `monty_v22::MontyRepl` and
/// `monty_v23::MontyRepl` are unrelated to the compiler.
enum VersionedSession {
    V22(monty_v22::MontyRepl),
    V23(monty_v23::MontyRepl),
}

impl VersionedSession {
    /// Start a new run on the newest interpreter.
    fn new_latest() -> Self {
        Self::V23(monty_v23::MontyRepl::new(
            SCRIPT,
            ResourceTracker23::default(),
            CompileOptions23::default(),
        ))
    }

    fn new_v22() -> Self {
        Self::V22(monty_v22::MontyRepl::new(
            SCRIPT,
            ResourceTracker22::default(),
            CompileOptions22::default(),
        ))
    }

    fn feed(&mut self, code: &str) -> Result<String, String> {
        match self {
            Self::V22(r) => r
                .feed_run(code, vec![], PrintWriter22::Disabled)
                .map(|v| format!("{v:?}"))
                .map_err(|e| e.to_string().lines().next_back().unwrap_or_default().trim().to_owned()),
            Self::V23(r) => r
                .feed_run(code, vec![], PrintWriter23::Disabled)
                .map(|v| format!("{v:?}"))
                .map_err(|e| e.to_string().lines().next_back().unwrap_or_default().trim().to_owned()),
        }
    }

    fn dump(&self) -> Vec<u8> {
        match self {
            Self::V22(r) => monty_v22::dump(SCRIPT, None, monty_v22::SessionRef::Idle(r)).expect("dump v22"),
            Self::V23(r) => monty_v23::dump(SCRIPT, None, monty_v23::SessionRef::Idle(r)).expect("dump v23"),
        }
    }

    fn version_name(&self) -> &'static str {
        match self {
            Self::V22(_) => "monty 0.0.22",
            Self::V23(_) => "monty 0.0.23",
        }
    }
}

/// Read the dump format version without deserializing the body.
///
/// This is the whole dispatcher: 8 bytes, no interpreter involved. A host can
/// route a snapshot it cannot itself read.
fn sniff_dump_version(bytes: &[u8]) -> Result<u16, String> {
    let header = bytes.get(..HEADER_LEN).ok_or("too short to be a dump")?;
    if &header[..MAGIC.len()] != MAGIC {
        return Err("not a monty dump (bad magic)".to_owned());
    }
    Ok(u16::from_le_bytes([header[MAGIC.len()], header[MAGIC.len() + 1]]))
}

/// Route a snapshot to whichever linked interpreter claims its dump version.
///
/// Note this is a *dump*-version table, not a crate-version table: two crate
/// versions may share a dump format. The mapping is maintained by hand, which
/// is the maintenance cost the handoff flagged.
fn load_any(bytes: &[u8]) -> Result<VersionedSession, String> {
    let found = sniff_dump_version(bytes)?;
    if found == monty_v23::DUMP_VERSION {
        let d = monty_v23::Dump::load(bytes).map_err(|e| format!("{e:?}"))?;
        match d.state {
            monty_v23::Session::Idle(r) => Ok(VersionedSession::V23(*r)),
            _ => Err("not an idle session".to_owned()),
        }
    } else if found == monty_v22::DUMP_VERSION {
        let d = monty_v22::Dump::load(bytes).map_err(|e| format!("{e:?}"))?;
        match d.state {
            monty_v22::Session::Idle(r) => Ok(VersionedSession::V22(*r)),
            _ => Err("not an idle session".to_owned()),
        }
    } else {
        Err(format!(
            "dump v{found} has no linked interpreter (this build carries v{} and v{})",
            monty_v22::DUMP_VERSION,
            monty_v23::DUMP_VERSION
        ))
    }
}

fn main() -> ExitCode {
    let mut ok = true;

    println!("== linked interpreters ==");
    println!("  monty 0.0.22 -> DUMP_VERSION {}", monty_v22::DUMP_VERSION);
    println!("  monty 0.0.23 -> DUMP_VERSION {}", monty_v23::DUMP_VERSION);
    println!(
        "  the two constants are separate statics: same path spelling, different crates{}",
        if monty_v22::DUMP_VERSION == monty_v23::DUMP_VERSION {
            " (and here they happen to carry the same value)"
        } else {
            ""
        }
    );

    println!("\n== a run that started on 0.0.22 ==");
    let mut old = VersionedSession::new_v22();
    old.feed("run_id = 'parked_on_approval'").unwrap();
    old.feed("def tally(xs):\n    return sum(xs)\n").unwrap();
    old.feed("class Budget:\n    def __init__(self, t):\n        self.t = t\n    def __repr__(self):\n        return f'Budget({self.t})'\n").unwrap();
    old.feed("budget = Budget(500)").unwrap();
    let old_bytes = old.dump();
    println!("  built on {}, dumped {} bytes, header v{}", old.version_name(), old_bytes.len(), sniff_dump_version(&old_bytes).unwrap());

    println!("\n== a run that started on 0.0.23 ==");
    let mut new = VersionedSession::new_latest();
    new.feed("run_id = 'started_today'").unwrap();
    new.feed("def tally(xs):\n    return sum(xs)\n").unwrap();
    let new_bytes = new.dump();
    println!("  built on {}, dumped {} bytes, header v{}", new.version_name(), new_bytes.len(), sniff_dump_version(&new_bytes).unwrap());

    println!("\n== dispatch: the host reads 8 bytes and picks an interpreter ==");
    for (label, bytes) in [("0.0.22 snapshot", &old_bytes), ("0.0.23 snapshot", &new_bytes)] {
        match load_any(bytes) {
            Ok(mut s) => {
                let who = s.version_name();
                let a = s.feed("run_id");
                let b = s.feed("tally([1, 2, 3, 4])");
                println!("  {label:<16} -> routed to {who:<13} run_id={a:?} tally={b:?}");
                if a.is_err() || b.is_err() {
                    ok = false;
                }
            }
            Err(e) => {
                println!("  {label:<16} -> FAILED: {e}");
                ok = false;
            }
        }
    }

    println!("\n== a snapshot from a version this build does NOT carry ==");
    let mut orphan = new_bytes.clone();
    orphan[6..8].copy_from_slice(&999u16.to_le_bytes());
    match load_any(&orphan) {
        Ok(_) => {
            println!("  FAIL: accepted a v999 dump");
            ok = false;
        }
        Err(e) => println!("  v999 snapshot -> {e}"),
    }
    println!("  (a retired-version snapshot is identifiable, not merely corrupt —");
    println!("   the host can report it rather than guess.)");

    println!("\n== cross-version confusion is a compile error, not a runtime bug ==");
    println!("  Feeding a v22 repl to a v23 function does not typecheck; see");
    println!("  `cross_version_is_rejected` in this file for the exact attempt.");

    if ok {
        println!("\nALL CHECKS PASSED");
        ExitCode::SUCCESS
    } else {
        println!("\nSOME CHECKS FAILED");
        ExitCode::FAILURE
    }
}

/// The safety property, recorded as source rather than prose.
///
/// Uncommenting the body fails to compile with a type mismatch: `monty_v22`'s
/// `MontyRepl`/`SessionRef` and `monty_v23`'s are unrelated types, so a snapshot
/// cannot be handed to the wrong interpreter by accident.
///
/// ```compile_fail
/// let r = monty_v22::MontyRepl::new("x.py", Default::default(), Default::default());
/// let _ = monty_v23::dump("x.py", None, monty_v23::SessionRef::Idle(&r));
/// ```
#[allow(dead_code)]
fn cross_version_is_rejected() {}
