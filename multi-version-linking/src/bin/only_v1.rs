//! Size baseline for the 1.0 line: identical work to `only_v23`, one linked
//! version, so the two binaries differ only in which interpreter they carry.
use monty_types_v1::{CompileOptions, PrintWriter, ResourceTracker};

fn main() {
    let mut r = monty_v1::MontyRepl::new("x.py", ResourceTracker::default(), CompileOptions::default());
    r.feed_run("run_id = 'only_v1'", vec![], PrintWriter::Disabled).unwrap();
    r.feed_run("def tally(xs):\n    return sum(xs)\n", vec![], PrintWriter::Disabled).unwrap();
    let bytes = monty_v1::dump("x.py", None, monty_v1::SessionRef::Idle(&r)).unwrap();
    println!("one linked version, dump {} bytes, DUMP_VERSION {}", bytes.len(), monty_v1::DUMP_VERSION);
}
