//! Size baseline: exactly TWO linked interpreters (0.0.23 and 1.0.0-beta.2),
//! doing the same trivial work as `only_v23` / `only_v1` once per version.
//! `pair - only_v23` is the marginal cost of adding the 1.0 interpreter.

fn main() {
    let mut a = monty_v23::MontyRepl::new(
        "x.py",
        monty_types_v23::ResourceTracker::default(),
        monty_types_v23::CompileOptions::default(),
    );
    a.feed_run("run_id = 'pair'", vec![], monty_types_v23::PrintWriter::Disabled).unwrap();
    let ba = monty_v23::dump("x.py", None, monty_v23::SessionRef::Idle(&a)).unwrap();

    let mut b = monty_v1::MontyRepl::new(
        "x.py",
        monty_types_v1::ResourceTracker::default(),
        monty_types_v1::CompileOptions::default(),
    );
    b.feed_run("run_id = 'pair'", vec![], monty_types_v1::PrintWriter::Disabled).unwrap();
    let bb = monty_v1::dump("x.py", None, monty_v1::SessionRef::Idle(&b)).unwrap();

    println!("two linked versions, dumps {} and {} bytes", ba.len(), bb.len());
}
