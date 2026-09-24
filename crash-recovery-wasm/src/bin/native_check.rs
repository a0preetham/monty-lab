//! Is the global allocator actually armed, and does the soft limit produce a
//! catchable MemoryError? Native control for the wasm result.
//!
//! The allocator must be declared in the *binary's* crate root — declaring it
//! in a library the binary links does nothing.
use monty::MontyRepl;
use monty_types::{CompileOptions, PrintWriter, ResourceLimits, ResourceTracker};

#[global_allocator]
static ALLOC: monty_alloc::LimitedAllocator = monty_alloc::LimitedAllocator;

fn main() {
    let live = monty_types::LIVE_MEMORY.load(std::sync::atomic::Ordering::Relaxed);
    println!("LIVE_MEMORY at start = {live} bytes");
    match monty_alloc::set_limit(Some(64 * 1024 * 1024), false) {
        Ok(()) => println!("set_limit: OK — allocator IS installed"),
        Err(e) => {
            println!("set_limit: ERR — {e}");
            return;
        }
    }

    let mut repl = MontyRepl::new(
        "oom.py",
        ResourceTracker::new(ResourceLimits::default().max_memory(64 * 1024 * 1024)),
        CompileOptions::default(),
    );
    repl.feed_run("marker = 'pre-oom'", vec![], PrintWriter::Disabled).unwrap();

    println!("\nguest allocates ~1.6 GB against a 64 MiB soft limit:");
    match repl.feed_run("big = [0] * 200_000_000", vec![], PrintWriter::Disabled) {
        Ok(v) => println!("  returned {v:?}  (limit did NOT fire)"),
        Err(e) => println!("  raised: {}", e.to_string().lines().next_back().unwrap_or("").trim()),
    }
    match repl.feed_run("marker", vec![], PrintWriter::Disabled) {
        Ok(v) => println!("  session still usable, marker = {v:?}"),
        Err(e) => println!("  session broken: {e:?}"),
    }
    println!("  LIVE_MEMORY now = {} bytes", monty_types::LIVE_MEMORY.load(std::sync::atomic::Ordering::Relaxed));
}
