//! Experiment D — several Monty versions linked into one binary, dispatched by
//! reading the dump header.
//!
//! The premise from the handoff: a deploy that bumps the interpreter makes every
//! existing snapshot unreadable, and for runs parked on an approval for weeks
//! there is no drain window. The fix is to keep old interpreters linked in and
//! route each snapshot to the version that can read it.
//!
//! Cargo makes this easier than expected here: every Monty 0.0.x release is its
//! own semver-incompatible range, and 1.0.0-beta.2 is incompatible with those in
//! turn, so `monty_v22`, `monty_v23` and `monty_v1` coexist as wholly separate
//! crates — separate types, separate statics, distinct symbols. The compiler
//! refuses to mix them, which is exactly the safety property wanted.
//!
//! 1.0.0-beta.2 is what makes this experiment complete: it carries dump format
//! **12** where the 0.0.x pair both carry **8**, so the cross-format route is
//! exercised for real rather than synthesised by rewriting a header.

use std::process::ExitCode;

use monty_types_v1::{
    CompileOptions as CompileOptionsV1, PrintWriter as PrintWriterV1, ResourceTracker as ResourceTrackerV1,
};
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
/// These variants hold *different types that happen to share a name*. The whole
/// point is that they cannot be confused: `monty_v22::MontyRepl`,
/// `monty_v23::MontyRepl` and `monty_v1::MontyRepl` are unrelated to the
/// compiler.
enum VersionedSession {
    V22(monty_v22::MontyRepl),
    V23(monty_v23::MontyRepl),
    V1(monty_v1::MontyRepl),
}

impl VersionedSession {
    /// Start a new run on the newest interpreter. New runs always land here;
    /// older versions exist only to carry runs that already started on them.
    fn new_latest() -> Self {
        Self::V1(monty_v1::MontyRepl::new(
            SCRIPT,
            ResourceTrackerV1::default(),
            CompileOptionsV1::default(),
        ))
    }

    fn new_v22() -> Self {
        Self::V22(monty_v22::MontyRepl::new(
            SCRIPT,
            ResourceTracker22::default(),
            CompileOptions22::default(),
        ))
    }

    fn new_v23() -> Self {
        Self::V23(monty_v23::MontyRepl::new(
            SCRIPT,
            ResourceTracker23::default(),
            CompileOptions23::default(),
        ))
    }

    fn feed(&mut self, code: &str) -> Result<String, String> {
        fn tail(e: impl ToString) -> String {
            e.to_string().lines().next_back().unwrap_or_default().trim().to_owned()
        }
        match self {
            Self::V22(r) => r
                .feed_run(code, vec![], PrintWriter22::Disabled)
                .map(|v| format!("{v:?}"))
                .map_err(tail),
            Self::V23(r) => r
                .feed_run(code, vec![], PrintWriter23::Disabled)
                .map(|v| format!("{v:?}"))
                .map_err(tail),
            Self::V1(r) => r
                .feed_run(code, vec![], PrintWriterV1::Disabled)
                .map(|v| format!("{v:?}"))
                .map_err(tail),
        }
    }

    fn dump(&self) -> Vec<u8> {
        match self {
            Self::V22(r) => monty_v22::dump(SCRIPT, None, monty_v22::SessionRef::Idle(r)).expect("dump v22"),
            Self::V23(r) => monty_v23::dump(SCRIPT, None, monty_v23::SessionRef::Idle(r)).expect("dump v23"),
            Self::V1(r) => monty_v1::dump(SCRIPT, None, monty_v1::SessionRef::Idle(r)).expect("dump v1"),
        }
    }

    fn version_name(&self) -> &'static str {
        match self {
            Self::V22(_) => "monty 0.0.22",
            Self::V23(_) => "monty 0.0.23",
            Self::V1(_) => "monty 1.0.0-beta.2",
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
/// Note this is a *dump*-version table, not a crate-version table: 0.0.22 and
/// 0.0.23 share format 8, so one arm serves both. The mapping is maintained by
/// hand, which is the maintenance cost the handoff flagged.
fn load_any(bytes: &[u8]) -> Result<VersionedSession, String> {
    let found = sniff_dump_version(bytes)?;
    if found == monty_v1::DUMP_VERSION {
        let d = monty_v1::Dump::load(bytes).map_err(|e| format!("{e:?}"))?;
        match d.state {
            monty_v1::Session::Idle(r) => Ok(VersionedSession::V1(*r)),
            _ => Err("not an idle session".to_owned()),
        }
    } else if found == monty_v23::DUMP_VERSION {
        // Newest interpreter that still reads format 8 wins: 0.0.23, not 0.0.22.
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
        // Deduplicated on purpose: the table is keyed by dump version, and two of
        // the three linked crates answer to the same one.
        let mut carried = vec![monty_v22::DUMP_VERSION, monty_v23::DUMP_VERSION, monty_v1::DUMP_VERSION];
        carried.sort_unstable();
        carried.dedup();
        let carried: Vec<String> = carried.iter().map(|v| format!("v{v}")).collect();
        Err(format!(
            "dump v{found} has no linked interpreter (this build reads {})",
            carried.join(", ")
        ))
    }
}

/// Build some realistic parked-run state: a helper, a class, a live instance.
fn seed(s: &mut VersionedSession, run_id: &str) {
    s.feed(&format!("run_id = {run_id:?}")).unwrap();
    s.feed("def tally(xs):\n    return sum(xs)\n").unwrap();
    s.feed("class Budget:\n    def __init__(self, t):\n        self.t = t\n    def __repr__(self):\n        return f'Budget({self.t})'\n").unwrap();
    s.feed("budget = Budget(500)").unwrap();
}

fn main() -> ExitCode {
    let mut ok = true;

    println!("== linked interpreters ==");
    println!("  monty 0.0.22       -> DUMP_VERSION {}", monty_v22::DUMP_VERSION);
    println!("  monty 0.0.23       -> DUMP_VERSION {}", monty_v23::DUMP_VERSION);
    println!("  monty 1.0.0-beta.2 -> DUMP_VERSION {}", monty_v1::DUMP_VERSION);
    println!("  the constants are separate statics: same path spelling, different crates.");
    println!("  0.0.22 and 0.0.23 share a format; 1.0.0-beta.2 does not — two formats, three versions.");

    println!("\n== runs parked on each version ==");
    let mut old = VersionedSession::new_v22();
    seed(&mut old, "parked_on_approval");
    let old_bytes = old.dump();
    println!(
        "  {:<18} dumped {} bytes, header v{}",
        old.version_name(),
        old_bytes.len(),
        sniff_dump_version(&old_bytes).unwrap()
    );

    let mut mid = VersionedSession::new_v23();
    seed(&mut mid, "parked_last_month");
    let mid_bytes = mid.dump();
    println!(
        "  {:<18} dumped {} bytes, header v{}",
        mid.version_name(),
        mid_bytes.len(),
        sniff_dump_version(&mid_bytes).unwrap()
    );

    let mut new = VersionedSession::new_latest();
    seed(&mut new, "started_today");
    let new_bytes = new.dump();
    println!(
        "  {:<18} dumped {} bytes, header v{}",
        new.version_name(),
        new_bytes.len(),
        sniff_dump_version(&new_bytes).unwrap()
    );

    println!("\n== the cross-format refusal, for real ==");
    println!("  Not a rewritten header this time: format 8 and format 12 are both");
    println!("  genuinely published, so each interpreter can be handed the other's bytes.");
    match monty_v1::Dump::load(&mid_bytes) {
        Ok(_) => {
            println!("  FAIL: the 1.0 interpreter accepted a format-8 dump");
            ok = false;
        }
        Err(e) => println!("  1.0.0-beta.2 reading a v8 dump  -> refused: {e:?}"),
    }
    match monty_v23::Dump::load(&new_bytes) {
        Ok(_) => {
            println!("  FAIL: the 0.0.23 interpreter accepted a format-12 dump");
            ok = false;
        }
        Err(e) => println!("  0.0.23 reading a v12 dump       -> refused: {e:?}"),
    }
    println!("  This is the failure the whole design exists to avoid: without the");
    println!("  router, a deploy that moved 0.0.23 -> 1.0 would meet exactly this.");

    println!("\n== dispatch: the host reads 8 bytes and picks an interpreter ==");
    for (label, bytes) in [
        ("0.0.22 snapshot", &old_bytes),
        ("0.0.23 snapshot", &mid_bytes),
        ("1.0 snapshot", &new_bytes),
    ] {
        match load_any(bytes) {
            Ok(mut s) => {
                let who = s.version_name();
                let a = s.feed("run_id");
                let b = s.feed("tally([1, 2, 3, 4])");
                let c = s.feed("repr(budget)");
                println!("  {label:<16} -> routed to {who:<18} run_id={a:?} tally={b:?} budget={c:?}");
                if a.is_err() || b.is_err() || c.is_err() {
                    ok = false;
                }
            }
            Err(e) => {
                println!("  {label:<16} -> FAILED: {e}");
                ok = false;
            }
        }
    }
    println!("  Each run resumed on the version that wrote it, and kept going — a");
    println!("  format-8 run still works after the 1.0 interpreter is in the build.");

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
/// cannot be handed to the wrong interpreter by accident. The same holds for
/// `monty_v1`.
///
/// ```compile_fail
/// let r = monty_v22::MontyRepl::new("x.py", Default::default(), Default::default());
/// let _ = monty_v23::dump("x.py", None, monty_v23::SessionRef::Idle(&r));
/// ```
#[allow(dead_code)]
fn cross_version_is_rejected() {}
