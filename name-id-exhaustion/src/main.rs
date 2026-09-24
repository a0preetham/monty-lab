//! Experiment H — monty [#895](https://github.com/pydantic/monty/issues/895):
//! every `feed_run` mints and permanently interns a script name, and name ids
//! are `u16`. The handoff listed this as untested, with the consequence that
//! "long-lived sessions with many turns will hit this".
//!
//! For a durable agent runtime the question is not academic: a run parked and
//! resumed for weeks is exactly a session with many turns. Three things need
//! answering.
//!
//! 1. Does the ceiling exist, and where exactly?
//! 2. How does it surface — a catchable error, or a panic that takes the host
//!    process (and on Workers, the isolate) with it?
//! 3. **Does a dump/reload cycle reset the interned space?** If it does, the
//!    runtime already has its escape hatch: snapshot and reload periodically,
//!    which a Durable Object host does anyway when it hibernates.
//!
//! Both 0.0.23 and 1.0.0-beta.2 are linked, so the same probe answers whether
//! the 1.0 line still has it.

use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::ExitCode;
use std::time::Instant;

const SCRIPT: &str = "agent-run.py";

/// How far past the theoretical u16 ceiling to push before calling it clear.
/// 4.5x the ceiling: if ids were minted per feed and never reused, nothing here
/// would get close to finishing.
const FEED_LIMIT: usize = 300_000;

/// Feeds before dumping, in the reload test. Deliberately past 65,535 in total
/// across the two legs but under it within either.
const LEG: usize = 40_000;

#[derive(Debug)]
enum Outcome {
    /// Reached `FEED_LIMIT` with no failure.
    Survived { feeds: usize },
    /// `feed_run` returned `Err` — recoverable, the host can react.
    Error { at: usize, message: String },
    /// `feed_run` panicked — on Workers this is a dead isolate.
    Panic { at: usize },
}

/// Run `n` trivial feeds against a fresh repl, reporting how it ends.
///
/// The snippets are trivial on purpose: this measures the per-feed interning
/// cost, not the cost of the code being fed.
macro_rules! feed_until_failure {
    ($monty:ident, $types:ident, $limit:expr, $progress:expr) => {{
        let mut repl = $monty::MontyRepl::new(
            SCRIPT,
            $types::ResourceTracker::default(),
            $types::CompileOptions::default(),
        );
        let reached = std::cell::Cell::new(0usize);
        let started = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| {
            for i in 1..=$limit {
                reached.set(i);
                let code = format!("x = {}", i % 7);
                if let Err(e) = repl.feed_run(&code, vec![], $types::PrintWriter::Disabled) {
                    return Some((i, e.to_string()));
                }
                if $progress && i % 50_000 == 0 {
                    print!("    {i} feeds ok ({:.1}s)\n", started.elapsed().as_secs_f64());
                    let _ = std::io::stdout().flush();
                }
            }
            None
        }));
        let outcome = match result {
            Ok(None) => Outcome::Survived { feeds: $limit },
            Ok(Some((at, message))) => Outcome::Error {
                at,
                message: message.lines().next_back().unwrap_or_default().trim().to_owned(),
            },
            Err(_) => Outcome::Panic { at: reached.get() },
        };
        (outcome, started.elapsed())
    }};
}

/// Feed `LEG` snippets, dump, load into a *fresh* repl, feed `LEG` more.
///
/// If the interned name space is part of the dump, the second leg fails partway;
/// if loading starts a fresh space, the total sails past the single-process
/// ceiling. That distinction decides whether this issue needs any mitigation at
/// all in a snapshot-based runtime.
macro_rules! survives_reload {
    ($monty:ident, $types:ident) => {{
        // An immediately-invoked closure so `?` and early `return` belong to the
        // probe rather than to whatever called the macro.
        (|| -> Result<(usize, usize, usize, Option<String>, String), String> {
        let mut repl = $monty::MontyRepl::new(
            SCRIPT,
            $types::ResourceTracker::default(),
            $types::CompileOptions::default(),
        );
        repl.feed_run("carried = 'before reload'", vec![], $types::PrintWriter::Disabled)
            .expect("seed");
        for i in 1..=LEG {
            repl.feed_run(&format!("x = {}", i % 7), vec![], $types::PrintWriter::Disabled)
                .map_err(|e| format!("leg 1 failed at {i}: {e}"))?;
        }
        let bytes = $monty::dump(SCRIPT, None, $monty::SessionRef::Idle(&repl))
            .map_err(|e| format!("dump failed: {e:?}"))?;
        drop(repl);

        let dump = $monty::Dump::load(&bytes).map_err(|e| format!("load failed: {e:?}"))?;
        let mut repl = match dump.state {
            $monty::Session::Idle(r) => *r,
            _ => return Err("reloaded into a non-idle session".to_owned()),
        };
        let mut second_leg = 0usize;
        let mut failure = None;
        for i in 1..=LEG {
            match repl.feed_run(&format!("y = {}", i % 7), vec![], $types::PrintWriter::Disabled) {
                Ok(_) => second_leg = i,
                Err(e) => {
                    failure = Some(format!("{e}").lines().next_back().unwrap_or_default().trim().to_owned());
                    break;
                }
            }
        }
        let carried = repl
            .feed_run("carried", vec![], $types::PrintWriter::Disabled)
            .map(|v| format!("{v:?}"))
            .unwrap_or_else(|e| format!("LOST: {e}"));
        // The size after leg 2 is the actual answer about mitigation: if the
        // per-feed accumulation were cleared by a reload, this would come back
        // about the same as leg 1 rather than twice it.
        let after = $monty::dump(SCRIPT, None, $monty::SessionRef::Idle(&repl))
            .map(|b| b.len())
            .map_err(|e| format!("second dump failed: {e:?}"))?;
        Ok((bytes.len(), after, second_leg, failure, carried))
        })()
    }};
}


/// Dump size and round-trip cost as a function of *turn count*, with the guest
/// state held constant.
///
/// This is the question the exhaustion probe turned up by accident: 40,000
/// trivial feeds produced a 1.9 MB snapshot where experiment B measured 6.9 KB
/// for realistic agent state. If snapshot size tracks the number of turns rather
/// than the size of the state, then for a durable runtime the cost of a long run
/// is paid on every hibernation and every cold wake.
macro_rules! growth_curve {
    ($monty:ident, $types:ident, $checkpoints:expr) => {{
        let mut rows = Vec::new();
        let mut repl = $monty::MontyRepl::new(
            SCRIPT,
            $types::ResourceTracker::default(),
            $types::CompileOptions::default(),
        );
        // One small piece of real state, so every row measures the same guest
        // heap and differs only in how many feeds preceded it.
        repl.feed_run("state = {'k': 'v'}", vec![], $types::PrintWriter::Disabled).expect("seed");

        let mut fed = 0usize;
        for target in $checkpoints {
            while fed < target {
                fed += 1;
                repl.feed_run(&format!("x = {}", fed % 7), vec![], $types::PrintWriter::Disabled)
                    .expect("feed");
            }
            let t0 = Instant::now();
            let bytes = $monty::dump(SCRIPT, None, $monty::SessionRef::Idle(&repl)).expect("dump");
            let dump_ms = t0.elapsed().as_secs_f64() * 1000.0;

            let t1 = Instant::now();
            let loaded = $monty::Dump::load(&bytes).expect("load");
            let load_ms = t1.elapsed().as_secs_f64() * 1000.0;
            // Prove the reload is usable, not just parseable.
            let mut r2 = match loaded.state {
                $monty::Session::Idle(r) => *r,
                _ => panic!("not idle"),
            };
            r2.feed_run("state['k']", vec![], $types::PrintWriter::Disabled).expect("state survived");

            rows.push((fed, bytes.len(), dump_ms, load_ms));
        }
        rows
    }};
}

fn report(label: &str, outcome: &Outcome, elapsed: std::time::Duration) -> bool {
    match outcome {
        Outcome::Survived { feeds } => {
            println!(
                "  {label:<18} {feeds} feeds, no failure ({:.1}s) — no u16 ceiling reached",
                elapsed.as_secs_f64()
            );
            true
        }
        Outcome::Error { at, message } => {
            println!(
                "  {label:<18} failed at feed {at} with a recoverable Err ({:.1}s)",
                elapsed.as_secs_f64()
            );
            println!("  {:<18} {message}", "");
            true
        }
        Outcome::Panic { at } => {
            println!(
                "  {label:<18} PANICKED at feed {at} ({:.1}s) — not recoverable in-process",
                elapsed.as_secs_f64()
            );
            true
        }
    }
}

fn main() -> ExitCode {
    println!("== how many feeds before the u16 name-id space runs out? ==");
    println!("  pushing to {FEED_LIMIT} feeds; the theoretical ceiling is 65,535.");

    println!("\n  monty 0.0.23:");
    let (o23, t23) = feed_until_failure!(monty_v23, monty_types_v23, FEED_LIMIT, true);
    report("0.0.23", &o23, t23);

    println!("\n  monty 1.0.0-beta.2:");
    let (o1, t1) = feed_until_failure!(monty_v1, monty_types_v1, FEED_LIMIT, true);
    report("1.0.0-beta.2", &o1, t1);

    println!("\n== snapshot size and round-trip cost vs turn count ==");
    println!("  Guest state is identical in every row: one small dict. The only");
    println!("  difference is how many feeds came before the dump.");
    const CHECKPOINTS: [usize; 6] = [0, 100, 1_000, 10_000, 50_000, 100_000];
    for (label, rows) in [
        ("0.0.23", growth_curve!(monty_v23, monty_types_v23, CHECKPOINTS)),
        ("1.0.0-beta.2", growth_curve!(monty_v1, monty_types_v1, CHECKPOINTS)),
    ] {
        println!("\n  {label}:");
        println!("    {:>8}  {:>12}  {:>11}  {:>9}  {:>9}", "feeds", "dump bytes", "bytes/feed", "dump ms", "load ms");
        for (fed, size, dump_ms, load_ms) in rows {
            let per = if fed == 0 { 0.0 } else { size as f64 / fed as f64 };
            println!("    {fed:>8}  {size:>12}  {per:>11.1}  {dump_ms:>9.1}  {load_ms:>9.1}");
        }
    }

    println!("\n== does a dump/reload reset the interned space? ==");
    println!("  {LEG} feeds, dump, load into a fresh repl, {LEG} more — {} total,", LEG * 2);
    println!("  which is past 65,535 if and only if the space carries across the dump.");

    let mut ok = true;
    for (label, res) in [
        ("0.0.23", survives_reload!(monty_v23, monty_types_v23)),
        ("1.0.0-beta.2", survives_reload!(monty_v1, monty_types_v1)),
    ] {
        match res {
            Ok((size, after, second_leg, failure, carried)) => {
                match failure {
                    None => println!("  {label:<18} second leg {second_leg}/{LEG} ok, past the 65,535 ceiling"),
                    Some(e) => println!("  {label:<18} second leg stopped at {second_leg}/{LEG}: {e}"),
                }
                println!(
                    "  {:<18} dump after leg 1: {size} bytes -> after leg 2: {after} bytes ({:.2}x)",
                    "",
                    after as f64 / size as f64
                );
                println!("  {:<18} state carried across the reload: {carried}", "");
            }
            Err(e) => {
                println!("  {label:<18} HARNESS FAILURE: {e}");
                ok = false;
            }
        }
    }

    if ok {
        println!("\nPROBE COMPLETE");
        ExitCode::SUCCESS
    } else {
        println!("\nHARNESS FAILED");
        ExitCode::FAILURE
    }
}
