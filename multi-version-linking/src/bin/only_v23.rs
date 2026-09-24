//! Size baseline: the same work, but referencing exactly ONE monty version.
//! Diffed against the two-version `dispatcher` binary this gives the marginal
//! cost of linking an additional interpreter.
use monty_types_v23::{CompileOptions, PrintWriter, ResourceTracker};

fn main() {
    let mut r = monty_v23::MontyRepl::new("x.py", ResourceTracker::default(), CompileOptions::default());
    r.feed_run("run_id = 'only_v23'", vec![], PrintWriter::Disabled).unwrap();
    r.feed_run("def tally(xs):\n    return sum(xs)\n", vec![], PrintWriter::Disabled).unwrap();
    let bytes = monty_v23::dump("x.py", None, monty_v23::SessionRef::Idle(&r)).unwrap();
    println!("one linked version, dump {} bytes, DUMP_VERSION {}", bytes.len(), monty_v23::DUMP_VERSION);
}
