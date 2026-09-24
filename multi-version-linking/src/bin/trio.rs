//! Size baseline: THREE linked interpreters, same trivial work per version.
//! `trio - pair_v23_v1` is the marginal cost of adding the 0.0.22 interpreter.

fn main() {
    let mut a = monty_v22::MontyRepl::new(
        "x.py",
        monty_types_v22::ResourceTracker::default(),
        monty_types_v22::CompileOptions::default(),
    );
    a.feed_run("run_id = 'trio'", vec![], monty_types_v22::PrintWriter::Disabled).unwrap();
    let ba = monty_v22::dump("x.py", None, monty_v22::SessionRef::Idle(&a)).unwrap();

    let mut b = monty_v23::MontyRepl::new(
        "x.py",
        monty_types_v23::ResourceTracker::default(),
        monty_types_v23::CompileOptions::default(),
    );
    b.feed_run("run_id = 'trio'", vec![], monty_types_v23::PrintWriter::Disabled).unwrap();
    let bb = monty_v23::dump("x.py", None, monty_v23::SessionRef::Idle(&b)).unwrap();

    let mut c = monty_v1::MontyRepl::new(
        "x.py",
        monty_types_v1::ResourceTracker::default(),
        monty_types_v1::CompileOptions::default(),
    );
    c.feed_run("run_id = 'trio'", vec![], monty_types_v1::PrintWriter::Disabled).unwrap();
    let bc = monty_v1::dump("x.py", None, monty_v1::SessionRef::Idle(&c)).unwrap();

    println!("three linked versions, dumps {} / {} / {} bytes", ba.len(), bb.len(), bc.len());
}
